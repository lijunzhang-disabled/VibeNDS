//! ARM7 I/O register read/write dispatch (0x04000000 page).

use super::SharedState;
use crate::interrupt::Irq;
use crate::ipc::{self, Side};

#[inline]
pub fn in_io_page(addr: u32) -> bool {
    addr >> 24 == 0x04
}

pub fn read_io8(shared: &SharedState, addr: u32) -> u8 {
    let r = read_io16(shared, addr & !1);
    if addr & 1 != 0 {
        (r >> 8) as u8
    } else {
        r as u8
    }
}

pub fn read_io16(shared: &SharedState, addr: u32) -> u16 {
    let local = addr & 0x00FF_FFFE;
    if (0x0100..0x0110).contains(&local) {
        let id = ((local - 0x0100) >> 2) as usize;
        return if local & 2 == 0 {
            shared.timers7.read_counter(id)
        } else {
            shared.timers7.read_control(id)
        };
    }

    // Audio channel block: 0x0400..0x04FF (16 channels × 16 bytes each)
    if (0x0400..0x0500).contains(&local) {
        let ch = ((local - 0x0400) / 0x10) as usize;
        let reg = (local - 0x0400) & 0xF;
        return read_audio_channel_u16(shared, ch, reg);
    }

    // Audio control:
    if local == 0x0500 {
        return shared.audio.master_cnt;
    }
    if local == 0x0504 {
        return shared.audio.bias;
    }

    match local {
        0x0004 => shared.dispstat7,
        0x0006 => shared.vcount,
        0x0130 => shared.keyinput,
        0x0132 => shared.keycnt7,
        0x0136 => shared.extkeyin,
        0x0180 => shared.ipc.read_sync(Side::Arm7),
        0x0184 => shared.ipc.read_fifocnt(Side::Arm7),
        0x01A0 => shared.auxspi.read_cnt(),
        0x01A2 => shared.auxspi.read_data() as u16,
        0x01C0 => shared.spi.read_cnt(),
        0x01C2 => shared.spi.read_data() as u16,
        0x0204 => shared.exmemcnt,
        0x0208 => shared.irq7.read_ime() as u16,
        0x0210 => shared.irq7.read_ie() as u16,
        0x0212 => (shared.irq7.read_ie() >> 16) as u16,
        0x0214 => shared.irq7.read_if() as u16,
        0x0216 => (shared.irq7.read_if() >> 16) as u16,
        0x0240 => shared.wramcnt as u16,
        _ => 0,
    }
}

pub fn read_io32(shared: &SharedState, addr: u32) -> u32 {
    let local = addr & 0x00FF_FFFC;
    if let Some((ch, kind)) = decode_dma_reg(local) {
        return match kind {
            0 => shared.dma7.read_sad(ch),
            1 => shared.dma7.read_dad(ch),
            2 => shared.dma7.read_control(ch) | shared.dma7.read_count(ch),
            _ => 0,
        };
    }
    match local {
        0x0210 => shared.irq7.read_ie(),
        0x0214 => shared.irq7.read_if(),
        0x0208 => shared.irq7.read_ime(),
        _ => {
            let lo = read_io16(shared, addr) as u32;
            let hi = read_io16(shared, addr.wrapping_add(2)) as u32;
            lo | (hi << 16)
        }
    }
}

/// Side-effecting 32-bit read (FIFORECV pops).
pub fn read_io32_mut(shared: &mut SharedState, addr: u32) -> u32 {
    let local = addr & 0x00FF_FFFC;
    match local {
        0x0010_0000 => {
            let (val, raise) = shared.ipc.read_recv(Side::Arm7);
            if raise {
                ipc::raise_send_empty(&mut shared.irq9);
            }
            val
        }
        _ => read_io32(shared, addr),
    }
}

fn decode_dma_reg(local: u32) -> Option<(usize, u32)> {
    if !(0xB0..0xE0).contains(&local) {
        return None;
    }
    let off = local - 0xB0;
    let ch = (off / 0xC) as usize;
    let kind = (off % 0xC) / 4;
    Some((ch, kind))
}

#[must_use]
#[derive(Debug, Clone, Copy)]
pub enum Write32Effect {
    None,
    RunDma7(usize),
}

fn write_dma_reg(shared: &mut SharedState, ch: usize, kind: u32, val: u32) -> Write32Effect {
    use crate::dma::WriteControlEffect;
    match kind {
        0 => {
            shared.dma7.write_sad(ch, val);
            Write32Effect::None
        }
        1 => {
            shared.dma7.write_dad(ch, val);
            Write32Effect::None
        }
        2 => {
            // ARM7 count is in CNT_L (low 16 bits). DMA3 uses 16 bits;
            // DMA0-2 use 14 bits but our generic mask & max_count handle it.
            let count = val & 0xFFFF;
            shared.dma7.write_count(ch, count);
            let effect = shared.dma7.write_control(ch, val);
            match effect {
                WriteControlEffect::RunNow => Write32Effect::RunDma7(ch),
                _ => Write32Effect::None,
            }
        }
        _ => Write32Effect::None,
    }
}

fn write_sync(shared: &mut SharedState, val: u16) {
    if shared.ipc.write_sync(Side::Arm7, val) {
        ipc::raise_ipc_sync(&mut shared.irq9);
    }
}

fn write_fifocnt(shared: &mut SharedState, val: u16) {
    let effects = shared.ipc.write_fifocnt(Side::Arm7, val);
    if effects.raise_send_empty_on_self {
        ipc::raise_send_empty(&mut shared.irq7);
    }
    if effects.raise_recv_not_empty_on_self {
        ipc::raise_recv_not_empty(&mut shared.irq7);
    }
}

fn write_fifosend(shared: &mut SharedState, val: u32) {
    if shared.ipc.write_send(Side::Arm7, val) {
        ipc::raise_recv_not_empty(&mut shared.irq9);
    }
}

/// Read a 16-bit halfword from one of the 16 audio channels' register
/// blocks. `reg` is the offset within the 16-byte block (0..15).
fn read_audio_channel_u16(shared: &SharedState, ch: usize, reg: u32) -> u16 {
    if ch >= 16 {
        return 0;
    }
    let c = &shared.audio.channels[ch];
    match reg {
        0x0 => c.cnt as u16,
        0x2 => (shared.audio.read_cnt(ch) >> 16) as u16,
        0x4 => c.sad as u16,
        0x6 => (c.sad >> 16) as u16,
        0x8 => c.tmr,
        0xA => c.pnt,
        0xC => c.len as u16,
        0xE => (c.len >> 16) as u16,
        _ => 0,
    }
}

fn write_audio_channel_u16(shared: &mut SharedState, ch: usize, reg: u32, val: u16) {
    if ch >= 16 {
        return;
    }
    let c = &mut shared.audio.channels[ch];
    match reg {
        0x0 => {
            // CNT low half — merge into the 32-bit register.
            let new_cnt = (c.cnt & 0xFFFF_0000) | val as u32;
            // The start bit is in the high half; this write doesn't restart.
            c.cnt = new_cnt;
        }
        0x2 => {
            // CNT high half — contains the start bit (bit 31 of CNT = bit 15 of this half).
            let new_cnt = (c.cnt & 0x0000_FFFF) | ((val as u32) << 16);
            shared.audio.write_cnt(ch, new_cnt);
        }
        0x4 => c.sad = (c.sad & 0xFFFF_0000) | val as u32,
        0x6 => c.sad = (c.sad & 0x0000_FFFF) | ((val as u32) << 16),
        0x8 => c.tmr = val,
        0xA => c.pnt = val,
        0xC => c.len = (c.len & 0xFFFF_0000) | val as u32,
        0xE => c.len = (c.len & 0x0000_FFFF) | ((val as u32) << 16),
        _ => {}
    }
}

pub fn write_io8(shared: &mut SharedState, addr: u32, val: u8) {
    if (addr & 0x00FF_FFFF) == 0x0301 {
        // HALTCNT: bit 7 enters low-power halt until an enabled IRQ wakes ARM7.
        // Sleep/POSTFLG details are outside the current direct-boot scope.
        if val & 0x80 != 0 {
            shared.halt7_requested = true;
        }
        return;
    }

    let aligned = addr & !1;
    let mut cur = read_io16(shared, aligned);
    if addr & 1 != 0 {
        cur = (cur & 0x00FF) | ((val as u16) << 8);
    } else {
        cur = (cur & 0xFF00) | (val as u16);
    }
    write_io16(shared, aligned, cur);
}

pub fn write_io16(shared: &mut SharedState, addr: u32, val: u16) {
    let local = addr & 0x00FF_FFFE;
    if (0x0100..0x0110).contains(&local) {
        let id = ((local - 0x0100) >> 2) as usize;
        if local & 2 == 0 {
            shared.timers7.write_reload(id, val);
        } else {
            shared.timers7.write_control(id, val);
        }
        return;
    }

    if (0x0400..0x0500).contains(&local) {
        let ch = ((local - 0x0400) / 0x10) as usize;
        let reg = (local - 0x0400) & 0xF;
        write_audio_channel_u16(shared, ch, reg, val);
        return;
    }
    if local == 0x0500 {
        shared.audio.master_cnt = val;
        return;
    }
    if local == 0x0504 {
        shared.audio.bias = val & 0x3FF;
        return;
    }

    match local {
        0x0004 => {
            shared.dispstat7 = (shared.dispstat7 & 0x0007) | (val & !0x0007);
        }
        0x0132 => shared.keycnt7 = val,
        0x0180 => write_sync(shared, val),
        0x0184 => write_fifocnt(shared, val),
        0x01A0 => shared.auxspi.write_cnt(val),
        0x01A2 => {
            // AUXSPIDATA: byte-level transfer to the cart backup chip.
            // Returns true on transfer-complete-IRQ-enable; we raise the
            // Slot-1 IRQ on ARM7 since that's where the slot lives by
            // default.
            if shared.auxspi.write_data(val as u8) {
                shared.irq7.request(Irq::Slot1Data);
            }
        }
        0x01C0 => shared.spi.write_cnt(val),
        0x01C2 => {
            if shared.spi.write_data(val as u8) {
                shared.irq7.request(Irq::Spi);
            }
        }
        0x0204 => shared.exmemcnt = val,
        0x0208 => shared.irq7.write_ime(val as u32),
        0x0210 => {
            let prev = shared.irq7.read_ie();
            shared.irq7.write_ie((prev & 0xFFFF_0000) | (val as u32));
        }
        0x0212 => {
            let prev = shared.irq7.read_ie();
            shared
                .irq7
                .write_ie((prev & 0x0000_FFFF) | ((val as u32) << 16));
        }
        0x0214 => shared.irq7.write_if(val as u32),
        0x0216 => shared.irq7.write_if((val as u32) << 16),
        _ => {
            log::trace!(
                "ARM7 I/O write16 to unhandled 0x{:08X} = 0x{:04X}",
                addr,
                val
            );
        }
    }
}

pub fn write_io32(shared: &mut SharedState, addr: u32, val: u32) -> Write32Effect {
    let local = addr & 0x00FF_FFFC;
    if let Some((ch, kind)) = decode_dma_reg(local) {
        return write_dma_reg(shared, ch, kind, val);
    }
    match local {
        0x0188 => {
            write_fifosend(shared, val);
            Write32Effect::None
        }
        0x0208 => {
            shared.irq7.write_ime(val);
            Write32Effect::None
        }
        0x0210 => {
            shared.irq7.write_ie(val);
            Write32Effect::None
        }
        0x0214 => {
            shared.irq7.write_if(val);
            Write32Effect::None
        }
        _ => {
            write_io16(shared, addr, val as u16);
            write_io16(shared, addr.wrapping_add(2), (val >> 16) as u16);
            Write32Effect::None
        }
    }
}
