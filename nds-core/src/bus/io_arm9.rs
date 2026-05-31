//! ARM9 I/O register read/write dispatch (0x04000000 page).
//!
//! Phase 2 covers the registers needed to wire up V/HBlank IRQs, basic
//! status reads, and WRAMCNT routing. Other registers return 0 on read
//! and log a trace on write.

use super::SharedState;
use crate::gpu2d::Engine2d;
use crate::gpu3d::{GxCmd};
use crate::interrupt::Irq;
use crate::ipc::{self, Side};

/// Returns Some((engine, local_offset)) if `offset` falls within an engine's
/// register page **and** is not a shared register (DISPSTAT, VCOUNT live at
/// 0x4/0x6 and are shared, not engine).
#[inline]
fn classify_engine(offset: u32) -> Option<(EngineSel, u32)> {
    if offset < 0x70 {
        // 0x04 = DISPSTAT, 0x06 = VCOUNT are shared between both CPUs.
        // 0x60-0x6B are 3D-engine / capture / main-mem-FIFO regs (not
        // really Engine A's, just happen to live in its address window).
        if offset == 0x04 || offset == 0x06 { return None; }
        if (0x60..0x6C).contains(&offset) { return None; }
        Some((EngineSel::A, offset))
    } else if (0x1000..0x1070).contains(&offset) {
        Some((EngineSel::B, offset - 0x1000))
    } else {
        None
    }
}

#[derive(Clone, Copy)]
enum EngineSel { A, B }

#[inline]
fn engine_mut(shared: &mut SharedState, sel: EngineSel) -> &mut Engine2d {
    match sel { EngineSel::A => &mut shared.engine_a, EngineSel::B => &mut shared.engine_b }
}

#[inline]
fn engine(shared: &SharedState, sel: EngineSel) -> &Engine2d {
    match sel { EngineSel::A => &shared.engine_a, EngineSel::B => &shared.engine_b }
}

/// Read a single 16-bit engine register identified by `local_off` (offset
/// within either Engine A or Engine B's I/O page).
fn read_engine_reg16(eng: &Engine2d, local_off: u32) -> u16 {
    match local_off {
        0x00 => eng.dispcnt as u16,
        0x02 => (eng.dispcnt >> 16) as u16,
        0x08 => eng.bgcnt[0],
        0x0A => eng.bgcnt[1],
        0x0C => eng.bgcnt[2],
        0x0E => eng.bgcnt[3],
        0x10 => eng.bg_hofs[0],
        0x12 => eng.bg_vofs[0],
        0x14 => eng.bg_hofs[1],
        0x16 => eng.bg_vofs[1],
        0x18 => eng.bg_hofs[2],
        0x1A => eng.bg_vofs[2],
        0x1C => eng.bg_hofs[3],
        0x1E => eng.bg_vofs[3],
        0x40 => eng.win0h,
        0x42 => eng.win1h,
        0x44 => eng.win0v,
        0x46 => eng.win1v,
        0x48 => eng.winin,
        0x4A => eng.winout,
        0x4C => eng.mosaic,
        0x50 => eng.bldcnt,
        0x52 => eng.bldalpha,
        0x54 => eng.bldy as u16,
        0x6C => eng.master_bright,
        _ => 0,
    }
}

fn write_engine_reg16(eng: &mut Engine2d, local_off: u32, val: u16) {
    match local_off {
        0x00 => eng.dispcnt = (eng.dispcnt & 0xFFFF_0000) | val as u32,
        0x02 => eng.dispcnt = (eng.dispcnt & 0x0000_FFFF) | ((val as u32) << 16),
        0x08 => eng.bgcnt[0] = val,
        0x0A => eng.bgcnt[1] = val,
        0x0C => eng.bgcnt[2] = val,
        0x0E => eng.bgcnt[3] = val,
        0x10 => eng.bg_hofs[0] = val,
        0x12 => eng.bg_vofs[0] = val,
        0x14 => eng.bg_hofs[1] = val,
        0x16 => eng.bg_vofs[1] = val,
        0x18 => eng.bg_hofs[2] = val,
        0x1A => eng.bg_vofs[2] = val,
        0x1C => eng.bg_hofs[3] = val,
        0x1E => eng.bg_vofs[3] = val,
        // BG2/3 affine params (PA, PB, PC, PD) and reference-point latches.
        0x20 => eng.bg2_pa = val as i16,
        0x22 => eng.bg2_pb = val as i16,
        0x24 => eng.bg2_pc = val as i16,
        0x26 => eng.bg2_pd = val as i16,
        0x30 => eng.bg3_pa = val as i16,
        0x32 => eng.bg3_pb = val as i16,
        0x34 => eng.bg3_pc = val as i16,
        0x36 => eng.bg3_pd = val as i16,
        0x40 => eng.win0h = val,
        0x42 => eng.win1h = val,
        0x44 => eng.win0v = val,
        0x46 => eng.win1v = val,
        0x48 => eng.winin = val,
        0x4A => eng.winout = val,
        0x4C => eng.mosaic = val,
        0x50 => eng.bldcnt = val,
        0x52 => eng.bldalpha = val,
        0x54 => eng.bldy = val,
        0x6C => eng.master_bright = val,
        _ => {}
    }
}

fn write_engine_reg32(eng: &mut Engine2d, local_off: u32, val: u32) {
    match local_off {
        0x00 => eng.dispcnt = val,
        0x28 => { eng.bg2_x_latch = sign_extend_28(val); eng.bg2_x_int = eng.bg2_x_latch; }
        0x2C => { eng.bg2_y_latch = sign_extend_28(val); eng.bg2_y_int = eng.bg2_y_latch; }
        0x38 => { eng.bg3_x_latch = sign_extend_28(val); eng.bg3_x_int = eng.bg3_x_latch; }
        0x3C => { eng.bg3_y_latch = sign_extend_28(val); eng.bg3_y_int = eng.bg3_y_latch; }
        _ => {
            write_engine_reg16(eng, local_off, val as u16);
            write_engine_reg16(eng, local_off + 2, (val >> 16) as u16);
        }
    }
}

fn sign_extend_28(val: u32) -> i32 {
    ((val as i32) << 4) >> 4
}

/// True when the address is the ARM9 I/O page.
#[inline]
pub fn in_io_page(addr: u32) -> bool {
    addr >> 24 == 0x04
}

pub fn read_io8(shared: &SharedState, addr: u32) -> u8 {
    let r = read_io16(shared, addr & !1);
    if addr & 1 != 0 { (r >> 8) as u8 } else { r as u8 }
}

pub fn read_io16(shared: &SharedState, addr: u32) -> u16 {
    use crate::vram::BankId;

    let local = addr & 0x00FF_FFFE;
    if let Some((sel, off)) = classify_engine(local) {
        return read_engine_reg16(engine(shared, sel), off);
    }
    if let Some(v) = shared.math.read16(local) {
        return v;
    }

    // Timer block: 0x100..0x110 (4 timers × 4 bytes).
    if (0x0100..0x0110).contains(&local) {
        let id = ((local - 0x0100) >> 2) as usize;
        return if local & 2 == 0 {
            shared.timers9.read_counter(id)
        } else {
            shared.timers9.read_control(id)
        };
    }

    match local {
        0x0004 => shared.dispstat9,
        0x0006 => shared.vcount,
        0x0130 => shared.keyinput,
        0x0132 => shared.keycnt9,
        0x01A0 => shared.auxspi.read_cnt(),
        0x01A2 => shared.auxspi.read_data() as u16,
        0x01A4 => shared.slot1_romctrl as u16,
        0x01A6 => (slot1_romctrl_status(shared) >> 16) as u16,
        0x0180 => shared.ipc.read_sync(Side::Arm9),
        0x0184 => shared.ipc.read_fifocnt(Side::Arm9),
        0x0204 => shared.exmemcnt,
        0x0208 => shared.irq9.read_ime() as u16,
        0x0210 => shared.irq9.read_ie() as u16,
        0x0212 => (shared.irq9.read_ie() >> 16) as u16,
        0x0214 => shared.irq9.read_if() as u16,
        0x0216 => (shared.irq9.read_if() >> 16) as u16,
        0x0240 => (shared.vram.read_cnt(BankId::A) as u16)
                | ((shared.vram.read_cnt(BankId::B) as u16) << 8),
        0x0242 => (shared.vram.read_cnt(BankId::C) as u16)
                | ((shared.vram.read_cnt(BankId::D) as u16) << 8),
        0x0244 => (shared.vram.read_cnt(BankId::E) as u16)
                | ((shared.vram.read_cnt(BankId::F) as u16) << 8),
        0x0246 => (shared.vram.read_cnt(BankId::G) as u16)
                | ((shared.wramcnt as u16) << 8),
        0x0248 => (shared.vram.read_cnt(BankId::H) as u16)
                | ((shared.vram.read_cnt(BankId::I) as u16) << 8),
        0x0304 => shared.powcnt1,
        0x0060 => shared.gpu3d.rasterizer.disp3dcnt,
        0x0600 => shared.gpu3d.fifo.stat_low(),
        0x0602 => 0, // GXSTAT high half — PE busy / polygon count
        _ => 0,
    }
}

/// Note: `read_io32` is `&SharedState` so it can't pop the FIFO (popping
/// is destructive). FIFORECV is handled separately via `read_fiforecv_mut`.
pub fn read_io32(shared: &SharedState, addr: u32) -> u32 {
    let local = addr & 0x00FF_FFFC;
    if let Some((sel, off)) = classify_engine(local) {
        let eng = engine(shared, sel);
        if off == 0x00 { return eng.dispcnt; }
    }
    if let Some(v) = shared.math.read32(local) {
        return v;
    }
    if let Some((ch, kind)) = decode_dma_reg(local) {
        return match kind {
            // Real hardware: SAD/DAD readback is undefined / zero. We
            // return what was written for testability.
            0 => shared.dma9.read_sad(ch),
            1 => shared.dma9.read_dad(ch),
            2 => shared.dma9.read_control(ch) | shared.dma9.read_count(ch),
            _ => 0,
        };
    }
    match local {
        0x01A4 => slot1_romctrl_status(shared),
        0x0210 => shared.irq9.read_ie(),
        0x0214 => shared.irq9.read_if(),
        0x0208 => shared.irq9.read_ime(),
        _ => {
            let lo = read_io16(shared, addr) as u32;
            let hi = read_io16(shared, addr.wrapping_add(2)) as u32;
            lo | (hi << 16)
        }
    }
}

/// Mutable 32-bit I/O read. Distinct from `read_io32` because some reads
/// have side effects (FIFORECV pops the queue).
pub fn read_io32_mut(shared: &mut SharedState, addr: u32) -> u32 {
    let local = addr & 0x00FF_FFFC;
    match local {
        0x0010_0000 => read_fiforecv(shared),
        0x0010_0010 => read_slot1_data(shared),
        _ => read_io32(shared, addr),
    }
}

/// Decode a DMA register address (relative to 0x040000B0). Returns
/// (channel, kind) where kind is 0 = SAD, 1 = DAD, 2 = CNT.
fn decode_dma_reg(local: u32) -> Option<(usize, u32)> {
    if !(0xB0..0xE0).contains(&local) { return None; }
    let off = local - 0xB0;
    let ch = (off / 0xC) as usize;
    let kind = (off % 0xC) / 4;
    Some((ch, kind))
}

/// FIFOSEND (32-bit write at 0x04000188).
fn write_fifosend(shared: &mut SharedState, val: u32) {
    if shared.ipc.write_send(Side::Arm9, val) {
        ipc::raise_recv_not_empty(&mut shared.irq7);
    }
}

/// FIFORECV (32-bit read at 0x04100000).
fn read_fiforecv(shared: &mut SharedState) -> u32 {
    let (val, raise_send_empty_on_other) = shared.ipc.read_recv(Side::Arm9);
    if raise_send_empty_on_other {
        ipc::raise_send_empty(&mut shared.irq7);
    }
    val
}

fn slot1_romctrl_status(shared: &SharedState) -> u32 {
    let mut v = shared.slot1_romctrl & !((1 << 31) | (1 << 23));
    if !shared.slot1_data.is_empty() {
        v |= (1 << 31) | (1 << 23);
    }
    v
}

fn read_slot1_data(shared: &mut SharedState) -> u32 {
    let v = shared.slot1_data.pop_front().unwrap_or(0xFFFF_FFFF);
    if shared.slot1_data.is_empty() {
        shared.slot1_romctrl &= !((1 << 31) | (1 << 23));
    }
    v
}

fn start_slot1_transfer(shared: &mut SharedState, val: u32) {
    shared.slot1_romctrl = val;
    shared.slot1_data.clear();

    if val & (1 << 31) == 0 {
        return;
    }

    let cmd = shared.slot1_command[0];
    let param = ((shared.slot1_command[1] as u32) << 24)
        | ((shared.slot1_command[2] as u32) << 16)
        | ((shared.slot1_command[3] as u32) << 8)
        | shared.slot1_command[4] as u32;
    let bytes = slot1_transfer_len(val);

    match cmd {
        0x00 => queue_rom_bytes(shared, 0, bytes),      // header read
        0x9F => queue_repeat(shared, 0xFFFF_FFFF, bytes), // dummy/reset stream
        0x90 | 0xB8 => queue_repeat(shared, 0x0000_7FC2, bytes.max(4)),
        0xA0 => {}
        0xB7 => queue_rom_bytes(shared, param, bytes), // normal data read
        _ => queue_repeat(shared, 0xFFFF_FFFF, bytes),
    }

    if !shared.slot1_data.is_empty() {
        shared.slot1_romctrl |= (1 << 31) | (1 << 23);
    } else {
        shared.slot1_romctrl &= !((1 << 31) | (1 << 23));
    }
}

fn slot1_transfer_len(romctrl: u32) -> usize {
    match (romctrl >> 24) & 0x7 {
        0 => 0,
        1 => 0x200,
        2 => 0x400,
        3 => 0x800,
        4 => 0x1000,
        5 => 0x2000,
        6 => 0x4000,
        _ => 4,
    }
}

fn queue_rom_bytes(shared: &mut SharedState, addr: u32, bytes: usize) {
    let words = (bytes + 3) / 4;
    for word_idx in 0..words {
        let base = addr as usize + word_idx * 4;
        let mut b = [0xFFu8; 4];
        for (i, slot) in b.iter_mut().enumerate() {
            if let Some(&rom_byte) = shared.slot1_rom.get(base + i) {
                *slot = rom_byte;
            }
        }
        shared.slot1_data.push_back(u32::from_le_bytes(b));
    }
}

fn queue_repeat(shared: &mut SharedState, word: u32, bytes: usize) {
    for _ in 0..((bytes + 3) / 4) {
        shared.slot1_data.push_back(word);
    }
}

/// IPCSYNC write helper.
fn write_sync(shared: &mut SharedState, val: u16) {
    if shared.ipc.write_sync(Side::Arm9, val) {
        ipc::raise_ipc_sync(&mut shared.irq7);
    }
}

/// IPCFIFOCNT write helper.
fn write_fifocnt(shared: &mut SharedState, val: u16) {
    let effects = shared.ipc.write_fifocnt(Side::Arm9, val);
    if effects.raise_send_empty_on_self {
        ipc::raise_send_empty(&mut shared.irq9);
    }
    if effects.raise_recv_not_empty_on_self {
        ipc::raise_recv_not_empty(&mut shared.irq9);
    }
}

pub fn write_io8(shared: &mut SharedState, addr: u32, val: u8) {
    use crate::vram::BankId;
    match addr & 0x00FF_FFFF {
        0x0240 => shared.vram.write_cnt(BankId::A, val),
        0x0241 => shared.vram.write_cnt(BankId::B, val),
        0x0242 => shared.vram.write_cnt(BankId::C, val),
        0x0243 => shared.vram.write_cnt(BankId::D, val),
        0x0244 => shared.vram.write_cnt(BankId::E, val),
        0x0245 => shared.vram.write_cnt(BankId::F, val),
        0x0246 => shared.vram.write_cnt(BankId::G, val),
        0x0247 => shared.wramcnt = val,
        0x0248 => shared.vram.write_cnt(BankId::H, val),
        0x0249 => shared.vram.write_cnt(BankId::I, val),
        0x01A8..=0x01AF => {
            shared.slot1_command[(addr as usize) & 7] = val;
        }
        _ => {
            // 8-bit write of an unrecognized register: read-modify-write the
            // 16-bit register so we don't drop the half we don't address.
            let aligned = addr & !1;
            let mut cur = read_io16(shared, aligned);
            if addr & 1 != 0 {
                cur = (cur & 0x00FF) | ((val as u16) << 8);
            } else {
                cur = (cur & 0xFF00) | (val as u16);
            }
            write_io16(shared, aligned, cur);
        }
    }
}

pub fn write_io16(shared: &mut SharedState, addr: u32, val: u16) {
    let local = addr & 0x00FF_FFFE;
    if let Some((sel, off)) = classify_engine(local) {
        write_engine_reg16(engine_mut(shared, sel), off, val);
        return;
    }
    if shared.math.write16(local, val) {
        return;
    }
    if (0x0100..0x0110).contains(&local) {
        let id = ((local - 0x0100) >> 2) as usize;
        if local & 2 == 0 {
            shared.timers9.write_reload(id, val);
        } else {
            shared.timers9.write_control(id, val);
        }
        return;
    }
    match local {
        0x0004 => {
            // DISPSTAT — bits 0-2 are status (read-only), bits 3-15 are control.
            shared.dispstat9 = (shared.dispstat9 & 0x0007) | (val & !0x0007);
        }
        0x0208 => shared.irq9.write_ime(val as u32),
        0x0210 => {
            let prev = shared.irq9.read_ie();
            shared.irq9.write_ie((prev & 0xFFFF_0000) | (val as u32));
        }
        0x0212 => {
            let prev = shared.irq9.read_ie();
            shared.irq9.write_ie((prev & 0x0000_FFFF) | ((val as u32) << 16));
        }
        0x0214 => shared.irq9.write_if(val as u32),
        0x0216 => shared.irq9.write_if((val as u32) << 16),
        0x0132 => shared.keycnt9 = val,
        0x01A0 => shared.auxspi.write_cnt(val),
        0x01A2 => {
            if shared.auxspi.write_data(val as u8) {
                shared.irq9.request(Irq::Slot1Data);
            }
        }
        0x01A4 => {
            shared.slot1_romctrl = (shared.slot1_romctrl & 0xFFFF_0000) | val as u32;
        }
        0x01A6 => {
            let new = (shared.slot1_romctrl & 0x0000_FFFF) | ((val as u32) << 16);
            start_slot1_transfer(shared, new);
        }
        0x0180 => write_sync(shared, val),
        0x0184 => write_fifocnt(shared, val),
        0x0204 => shared.exmemcnt = val,
        0x0246 => shared.wramcnt = val as u8,
        0x0060 => shared.gpu3d.rasterizer.disp3dcnt = val,
        0x0304 => shared.powcnt1 = val,
        0x0330..=0x033F => {
            // EDGE_COLOR table — 8 × u16.
            let idx = ((local - 0x0330) / 2) as usize;
            shared.gpu3d.rasterizer.edge_color[idx] = val & 0x7FFF;
        }
        0x0340 => shared.gpu3d.rasterizer.alpha_test_ref = val as u8,
        0x0350 => shared.gpu3d.rasterizer.clear_color =
                  (shared.gpu3d.rasterizer.clear_color & 0xFFFF_0000) | val as u32,
        0x0352 => shared.gpu3d.rasterizer.clear_color =
                  (shared.gpu3d.rasterizer.clear_color & 0x0000_FFFF) | ((val as u32) << 16),
        0x0354 => shared.gpu3d.rasterizer.clear_depth = val,
        0x0356 => { /* CLEAR_IMAGE_OFFSET — unused */ }
        0x0358 => shared.gpu3d.rasterizer.fog_color =
                  (shared.gpu3d.rasterizer.fog_color & 0xFFFF_0000) | (val as u32),
        0x035A => shared.gpu3d.rasterizer.fog_color =
                  (shared.gpu3d.rasterizer.fog_color & 0x0000_FFFF) | ((val as u32) << 16),
        0x035C => shared.gpu3d.rasterizer.fog_offset = val,
        0x0360..=0x037F => {
            // 32-byte FOG_TABLE — one byte per entry, packed in u16 writes.
            let base = ((local - 0x0360) * 2) as usize;
            if base + 1 < 32 {
                shared.gpu3d.rasterizer.fog_table[base] = val as u8;
                shared.gpu3d.rasterizer.fog_table[base + 1] = (val >> 8) as u8;
            }
        }
        0x0380..=0x03BF => {
            // 32-entry TOON_TABLE u16 each.
            let idx = ((local - 0x0380) / 2) as usize;
            if idx < 32 {
                shared.gpu3d.rasterizer.toon_table[idx] = val & 0x7FFF;
            }
        }
        _ => {
            log::trace!("ARM9 I/O write16 to unhandled 0x{:08X} = 0x{:04X}", addr, val);
        }
    }
}

/// Result of a 32-bit I/O write. The immediate-mode DMA path returns
/// `RunDma9(channel)` so the bus can run the transfer after releasing
/// the SharedState borrow.
#[must_use]
#[derive(Debug, Clone, Copy)]
pub enum Write32Effect {
    None,
    RunDma9(usize),
    /// GXFIFO crossed below half-full; caller should fire GxFifo DMA.
    FireGxFifoDma,
}

pub fn write_io32(shared: &mut SharedState, addr: u32, val: u32) -> Write32Effect {
    let local = addr & 0x00FF_FFFC;
    if let Some((sel, off)) = classify_engine(local) {
        write_engine_reg32(engine_mut(shared, sel), off, val);
        return Write32Effect::None;
    }
    if let Some((ch, kind)) = decode_dma_reg(local) {
        return write_dma_reg(shared, ch, kind, val);
    }
    if shared.math.write32(local, val) {
        return Write32Effect::None;
    }

    // GXFIFO (packed format) at 0x04000400 + the direct-port range
    // 0x04000440..0x040005FF.
    if local == 0x0400 {
        shared.gpu3d.fifo.write_packed(val);
        shared.gpu3d.drain_fifo();
        if shared.gpu3d.fifo.take_below_half_edge() {
            return Write32Effect::FireGxFifoDma;
        }
        return Write32Effect::None;
    }
    if (0x0440..0x0600).contains(&local) {
        // Direct ports — each command lives at its own 4-byte slot.
        let cmd_byte = ((local - 0x0440) / 4 + 0x10) as u8;
        if let Some(cmd) = GxCmd::from_u8(cmd_byte) {
            shared.gpu3d.fifo.write_direct(cmd, val);
            shared.gpu3d.drain_fifo();
            if shared.gpu3d.fifo.take_below_half_edge() {
                return Write32Effect::FireGxFifoDma;
            }
        }
        return Write32Effect::None;
    }

    match local {
        0x0188 => { write_fifosend(shared, val); Write32Effect::None }
        0x01A4 => { start_slot1_transfer(shared, val); Write32Effect::None }
        0x01A8 | 0x01AC => {
            for i in 0..4 {
                shared.slot1_command[(local - 0x01A8) as usize + i] = (val >> (i * 8)) as u8;
            }
            Write32Effect::None
        }
        0x0208 => { shared.irq9.write_ime(val); Write32Effect::None }
        0x0210 => { shared.irq9.write_ie(val); Write32Effect::None }
        0x0214 => { shared.irq9.write_if(val); Write32Effect::None }
        _ => {
            write_io16(shared, addr, val as u16);
            write_io16(shared, addr.wrapping_add(2), (val >> 16) as u16);
            Write32Effect::None
        }
    }
}

fn write_dma_reg(shared: &mut SharedState, ch: usize, kind: u32, val: u32) -> Write32Effect {
    use crate::dma::WriteControlEffect;
    match kind {
        0 => { shared.dma9.write_sad(ch, val); Write32Effect::None }
        1 => { shared.dma9.write_dad(ch, val); Write32Effect::None }
        2 => {
            // Combined CNT register: low 21 bits = count, high 11 = control flags.
            let count = val & 0x001F_FFFF;
            shared.dma9.write_count(ch, count);
            let effect = shared.dma9.write_control(ch, val);
            match effect {
                WriteControlEffect::RunNow => Write32Effect::RunDma9(ch),
                _ => Write32Effect::None,
            }
        }
        _ => Write32Effect::None,
    }
}
