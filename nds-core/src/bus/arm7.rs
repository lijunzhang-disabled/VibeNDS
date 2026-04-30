//! ARM7-side bus.
//!
//! ARM7 owns 64 KB of private WRAM at `0x03800000` and a 16 KB BIOS at
//! `0x00000000`. Access to the 0x03000000 window depends on `WRAMCNT`: if
//! the ARM7 is mapped, that takes precedence; otherwise the access falls
//! through to ARM7 WRAM (mirrored into the same window per GBATEK).

use super::shared::SharedState;
use crate::cpu::bus::CpuBus;
use crate::dma::AddrControl;
use serde::{Deserialize, Serialize};

pub const ARM7_WRAM_SIZE: usize = 64 * 1024;
pub const ARM7_BIOS_SIZE: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Arm7Memory {
    #[serde(with = "super::shared::serde_bytes_vec")]
    pub wram: Vec<u8>,
    #[serde(with = "super::shared::serde_bytes_vec")]
    pub bios: Vec<u8>,
}

impl Arm7Memory {
    pub fn new(bios: Option<Vec<u8>>) -> Self {
        Arm7Memory {
            wram: vec![0u8; ARM7_WRAM_SIZE],
            bios: bios.unwrap_or_else(|| vec![0xFFu8; ARM7_BIOS_SIZE]),
        }
    }
}

pub struct Bus7<'a> {
    pub mem: &'a mut Arm7Memory,
    pub shared: &'a mut SharedState,
}

impl<'a> Bus7<'a> {
    pub fn new(mem: &'a mut Arm7Memory, shared: &'a mut SharedState) -> Self {
        Bus7 { mem, shared }
    }

    pub fn run_dma(&mut self, id: usize) -> bool {
        let (count, word_size, src_ctrl, dst_ctrl, irq_on_complete) = {
            let d = &self.shared.dma7;
            (
                d.channels[id].internal_count,
                d.word_size(id),
                d.src_control(id),
                d.dst_control(id),
                d.irq_on_complete(id),
            )
        };

        for _ in 0..count {
            let (sad, dad) = {
                let c = &self.shared.dma7.channels[id];
                (c.internal_sad, c.internal_dad)
            };
            if word_size == 4 {
                let v = self.read32(sad);
                self.write32(dad, v);
            } else {
                let v = self.read16(sad);
                self.write16(dad, v);
            }
            advance7(&mut self.shared.dma7.channels[id].internal_sad, src_ctrl, word_size);
            advance7(&mut self.shared.dma7.channels[id].internal_dad, dst_ctrl, word_size);
        }
        self.shared.dma7.finish_transfer(id);
        irq_on_complete
    }
}

fn advance7(addr: &mut u32, ctrl: AddrControl, word: u32) {
    match ctrl {
        AddrControl::Increment | AddrControl::IncrementReload => *addr = addr.wrapping_add(word),
        AddrControl::Decrement => *addr = addr.wrapping_sub(word),
        AddrControl::Fixed => {}
    }
}

#[inline]
fn wrap(addr: u32, len: usize) -> usize {
    if len == 0 { return 0; }
    (addr as usize) % len
}

impl<'a> CpuBus for Bus7<'a> {
    fn irq_pending(&self) -> bool {
        self.shared.irq7.has_pending()
    }

    fn read8(&mut self, addr: u32) -> u8 {
        match addr >> 24 {
            0x00 => self.mem.bios[(addr as usize) & 0x3FFF],
            0x02 => self.shared.main_ram[(addr as usize) & 0x3F_FFFF],
            0x03 => {
                if (addr & 0x00FF_0000) == 0x0080_0000 || addr >= 0x0380_0000 {
                    self.mem.wram[(addr as usize) & 0xFFFF]
                } else if let Some(view) = self.shared.arm7_wram_view() {
                    let off = wrap(addr, view.len());
                    view[off]
                } else {
                    self.mem.wram[(addr as usize) & 0xFFFF]
                }
            }
            0x04 => super::io_arm7::read_io8(self.shared, addr),
            0x06 => self.shared.vram.cpu_read_arm7(addr),
            _ => 0,
        }
    }

    fn read16(&mut self, addr: u32) -> u16 {
        if addr >> 24 == 0x04 {
            return super::io_arm7::read_io16(self.shared, addr);
        }
        let lo = self.read8(addr) as u16;
        let hi = self.read8(addr.wrapping_add(1)) as u16;
        lo | (hi << 8)
    }

    fn read32(&mut self, addr: u32) -> u32 {
        if addr >> 24 == 0x04 {
            return super::io_arm7::read_io32_mut(self.shared, addr);
        }
        match addr >> 24 {
            0x00 => {
                let off = (addr as usize) & 0x3FFC;
                u32::from_le_bytes([
                    self.mem.bios[off], self.mem.bios[off + 1],
                    self.mem.bios[off + 2], self.mem.bios[off + 3],
                ])
            }
            0x02 => {
                let off = (addr as usize) & 0x3F_FFFC;
                u32::from_le_bytes([
                    self.shared.main_ram[off], self.shared.main_ram[off + 1],
                    self.shared.main_ram[off + 2], self.shared.main_ram[off + 3],
                ])
            }
            0x03 => {
                if (addr & 0x00FF_0000) == 0x0080_0000 || addr >= 0x0380_0000 {
                    let off = (addr as usize) & 0xFFFC;
                    u32::from_le_bytes([
                        self.mem.wram[off], self.mem.wram[off + 1],
                        self.mem.wram[off + 2], self.mem.wram[off + 3],
                    ])
                } else if let Some(view) = self.shared.arm7_wram_view() {
                    let off = wrap(addr, view.len()) & !3;
                    u32::from_le_bytes([view[off], view[off + 1], view[off + 2], view[off + 3]])
                } else {
                    let off = (addr as usize) & 0xFFFC;
                    u32::from_le_bytes([
                        self.mem.wram[off], self.mem.wram[off + 1],
                        self.mem.wram[off + 2], self.mem.wram[off + 3],
                    ])
                }
            }
            _ => 0,
        }
    }

    fn write8(&mut self, addr: u32, val: u8) {
        match addr >> 24 {
            0x02 => self.shared.main_ram[(addr as usize) & 0x3F_FFFF] = val,
            0x03 => {
                if (addr & 0x00FF_0000) == 0x0080_0000 || addr >= 0x0380_0000 {
                    self.mem.wram[(addr as usize) & 0xFFFF] = val;
                } else if let Some(view) = self.shared.arm7_wram_view_mut() {
                    let off = wrap(addr, view.len());
                    view[off] = val;
                } else {
                    self.mem.wram[(addr as usize) & 0xFFFF] = val;
                }
            }
            0x04 => super::io_arm7::write_io8(self.shared, addr, val),
            0x06 => self.shared.vram.cpu_write_arm7(addr, val),
            _ => {}
        }
    }

    fn write16(&mut self, addr: u32, val: u16) {
        if addr >> 24 == 0x04 {
            super::io_arm7::write_io16(self.shared, addr, val);
            return;
        }
        let bytes = val.to_le_bytes();
        self.write8(addr, bytes[0]);
        self.write8(addr.wrapping_add(1), bytes[1]);
    }

    fn write32(&mut self, addr: u32, val: u32) {
        if addr >> 24 == 0x04 {
            let effect = super::io_arm7::write_io32(self.shared, addr, val);
            if let super::io_arm7::Write32Effect::RunDma7(ch) = effect {
                let irq = self.run_dma(ch);
                if irq {
                    use crate::interrupt::Irq;
                    let irq_bit = match ch {
                        0 => Irq::Dma0, 1 => Irq::Dma1, 2 => Irq::Dma2, _ => Irq::Dma3,
                    };
                    self.shared.irq7.request(irq_bit);
                }
            }
            return;
        }
        match addr >> 24 {
            0x02 => {
                let off = (addr as usize) & 0x3F_FFFC;
                let b = val.to_le_bytes();
                self.shared.main_ram[off]     = b[0];
                self.shared.main_ram[off + 1] = b[1];
                self.shared.main_ram[off + 2] = b[2];
                self.shared.main_ram[off + 3] = b[3];
            }
            0x03 => {
                let b = val.to_le_bytes();
                if (addr & 0x00FF_0000) == 0x0080_0000 || addr >= 0x0380_0000 {
                    let off = (addr as usize) & 0xFFFC;
                    self.mem.wram[off]     = b[0];
                    self.mem.wram[off + 1] = b[1];
                    self.mem.wram[off + 2] = b[2];
                    self.mem.wram[off + 3] = b[3];
                } else if let Some(view) = self.shared.arm7_wram_view_mut() {
                    let off = wrap(addr, view.len()) & !3;
                    view[off]     = b[0];
                    view[off + 1] = b[1];
                    view[off + 2] = b[2];
                    view[off + 3] = b[3];
                } else {
                    let off = (addr as usize) & 0xFFFC;
                    self.mem.wram[off]     = b[0];
                    self.mem.wram[off + 1] = b[1];
                    self.mem.wram[off + 2] = b[2];
                    self.mem.wram[off + 3] = b[3];
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (Arm7Memory, SharedState) {
        (Arm7Memory::new(None), SharedState::new())
    }

    #[test]
    fn test_arm7_main_ram_round_trip() {
        let (mut mem, mut shared) = fresh();
        let mut bus = Bus7::new(&mut mem, &mut shared);
        bus.write32(0x0200_1000, 0x1234_5678);
        assert_eq!(bus.read32(0x0200_1000), 0x1234_5678);
    }

    #[test]
    fn test_arm7_wram_at_03800000() {
        let (mut mem, mut shared) = fresh();
        let mut bus = Bus7::new(&mut mem, &mut shared);
        bus.write32(0x0380_0010, 0xCAFE_F00D);
        assert_eq!(bus.read32(0x0380_0010), 0xCAFE_F00D);
    }

    #[test]
    fn test_shared_wram_visible_in_mode_3() {
        let (mut mem, mut shared) = fresh();
        shared.wramcnt = 3;
        let mut bus = Bus7::new(&mut mem, &mut shared);
        bus.write32(0x0300_0010, 0xAA55_AA55);
        assert_eq!(bus.read32(0x0300_0010), 0xAA55_AA55);
    }

    #[test]
    fn test_shared_wram_falls_through_to_arm7_wram_in_mode_0() {
        let (mut mem, mut shared) = fresh();
        // Mode 0 = ARM7 has no shared WRAM. Writes at 0x03000000 go to ARM7
        // WRAM (mirror of 0x03800000).
        shared.wramcnt = 0;
        let mut bus = Bus7::new(&mut mem, &mut shared);
        bus.write32(0x0300_0010, 0xBEEF_CAFE);
        assert_eq!(bus.read32(0x0380_0010), 0xBEEF_CAFE);
    }
}
