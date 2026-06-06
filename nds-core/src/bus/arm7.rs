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

    fn has_synthetic_bios(&self) -> bool {
        self.bios.first().copied() == Some(0xFF)
    }

    fn synthetic_undefined_handler(&self) -> u32 {
        let off = 0xFFDCusize;
        u32::from_le_bytes([
            self.wram[off],
            self.wram[off + 1],
            self.wram[off + 2],
            self.wram[off + 3],
        ])
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
            advance7(
                &mut self.shared.dma7.channels[id].internal_sad,
                src_ctrl,
                word_size,
            );
            advance7(
                &mut self.shared.dma7.channels[id].internal_dad,
                dst_ctrl,
                word_size,
            );
        }
        self.shared.dma7.finish_transfer(id);
        irq_on_complete
    }
}

#[inline]
fn in_arm7_private_wram(addr: u32) -> bool {
    (0x0380_0000..=0x0380_FFFF).contains(&addr)
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
    if len == 0 {
        return 0;
    }
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
                if in_arm7_private_wram(addr) {
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
                if self.mem.has_synthetic_bios() {
                    let has_undefined_handler = self.mem.synthetic_undefined_handler() != 0;
                    if off == 0x04 {
                        if has_undefined_handler {
                            return 0xEA00_0012; // B synthetic undefined wrapper at 0x54
                        }
                        return 0xE1B0_F00E; // MOVS PC, LR
                    }
                    if off == 0x18 {
                        return 0xE92D_500F; // STMDB SP!, {R0-R3, R12, LR}
                    }
                    if off == 0x1C {
                        return 0xE59F_0028; // LDR R0, [PC, #0x28] -> 0x0380FFFC
                    }
                    if off == 0x20 {
                        return 0xE590_0000; // LDR R0, [R0]
                    }
                    if off == 0x24 {
                        return 0xE350_0000; // CMP R0, #0
                    }
                    if off == 0x28 {
                        return 0x059F_0020; // LDREQ R0, [PC, #0x20] -> 0x03FFFFFC
                    }
                    if off == 0x2C {
                        return 0x0590_0000; // LDREQ R0, [R0]
                    }
                    if off == 0x30 {
                        return 0xE350_0000; // CMP R0, #0
                    }
                    if off == 0x34 {
                        return 0x0A00_0002; // BEQ wrapper epilogue
                    }
                    if off == 0x38 {
                        return 0xE28F_E004; // ADD LR, PC, #4 -> wrapper epilogue
                    }
                    if off == 0x3C {
                        return 0xE12F_FF10; // BX R0
                    }
                    if off == 0x40 {
                        return 0xE1A0_0000; // NOP
                    }
                    if off == 0x44 {
                        return 0xE8BD_500F; // LDMIA SP!, {R0-R3, R12, LR}
                    }
                    if off == 0x48 {
                        return 0xE25E_F004; // SUBS PC, LR, #4
                    }
                    if off == 0x4C {
                        return 0x0380_FFFC;
                    }
                    if off == 0x50 {
                        return 0x03FF_FFFC;
                    }
                    if has_undefined_handler && off == 0x54 {
                        return 0xE92D_4000; // STMDB SP!, {LR}
                    }
                    if has_undefined_handler && off == 0x58 {
                        return 0xE59F_0020; // LDR R0, [PC, #0x20] -> 0x0380FFDC
                    }
                    if has_undefined_handler && off == 0x5C {
                        return 0xE590_0000; // LDR R0, [R0]
                    }
                    if has_undefined_handler && off == 0x60 {
                        return 0xE350_0000; // CMP R0, #0
                    }
                    if has_undefined_handler && off == 0x64 {
                        return 0x0A00_0002; // BEQ wrapper epilogue
                    }
                    if has_undefined_handler && off == 0x68 {
                        return 0xE28F_E004; // ADD LR, PC, #4 -> wrapper epilogue
                    }
                    if has_undefined_handler && off == 0x6C {
                        return 0xE12F_FF10; // BX R0
                    }
                    if has_undefined_handler && off == 0x70 {
                        return 0xE1A0_0000; // NOP
                    }
                    if has_undefined_handler && off == 0x74 {
                        return 0xE8BD_4000; // LDMIA SP!, {LR}
                    }
                    if has_undefined_handler && off == 0x78 {
                        return 0xE1B0_F00E; // MOVS PC, LR
                    }
                    if has_undefined_handler && off == 0x7C {
                        return 0xE1A0_0000; // NOP
                    }
                    if has_undefined_handler && off == 0x80 {
                        return 0x0380_FFDC;
                    }
                }
                u32::from_le_bytes([
                    self.mem.bios[off],
                    self.mem.bios[off + 1],
                    self.mem.bios[off + 2],
                    self.mem.bios[off + 3],
                ])
            }
            0x02 => {
                let off = (addr as usize) & 0x3F_FFFC;
                u32::from_le_bytes([
                    self.shared.main_ram[off],
                    self.shared.main_ram[off + 1],
                    self.shared.main_ram[off + 2],
                    self.shared.main_ram[off + 3],
                ])
            }
            0x03 => {
                if in_arm7_private_wram(addr) {
                    let off = (addr as usize) & 0xFFFC;
                    u32::from_le_bytes([
                        self.mem.wram[off],
                        self.mem.wram[off + 1],
                        self.mem.wram[off + 2],
                        self.mem.wram[off + 3],
                    ])
                } else if let Some(view) = self.shared.arm7_wram_view() {
                    let off = wrap(addr, view.len()) & !3;
                    u32::from_le_bytes([view[off], view[off + 1], view[off + 2], view[off + 3]])
                } else {
                    let off = (addr as usize) & 0xFFFC;
                    u32::from_le_bytes([
                        self.mem.wram[off],
                        self.mem.wram[off + 1],
                        self.mem.wram[off + 2],
                        self.mem.wram[off + 3],
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
                if in_arm7_private_wram(addr) {
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
            match effect {
                super::io_arm7::Write32Effect::RunDma7(ch) => {
                    let irq = self.run_dma(ch);
                    if irq {
                        use crate::interrupt::Irq;
                        let irq_bit = match ch {
                            0 => Irq::Dma0,
                            1 => Irq::Dma1,
                            2 => Irq::Dma2,
                            _ => Irq::Dma3,
                        };
                        self.shared.irq7.request(irq_bit);
                    }
                }
                super::io_arm7::Write32Effect::FireSlot1Dma => {
                    let channels = self
                        .shared
                        .dma7
                        .channels_for_timing(crate::dma::DmaTiming::Slot1);
                    for ch in channels {
                        while self.shared.dma7.channels[ch].active
                            && self.shared.dma7.timing(ch) == crate::dma::DmaTiming::Slot1
                            && !self.shared.slot1_data.is_empty()
                        {
                            let before = self.shared.slot1_data.len();
                            let irq = self.run_dma(ch);
                            if irq {
                                use crate::interrupt::Irq;
                                let irq_bit = match ch {
                                    0 => Irq::Dma0,
                                    1 => Irq::Dma1,
                                    2 => Irq::Dma2,
                                    _ => Irq::Dma3,
                                };
                                self.shared.irq7.request(irq_bit);
                            }
                            if self.shared.slot1_data.len() >= before {
                                break;
                            }
                        }
                        if self.shared.slot1_data.is_empty()
                            && self.shared.dma7.timing(ch) == crate::dma::DmaTiming::Slot1
                        {
                            self.shared.dma7.channels[ch].active = false;
                            self.shared.dma7.channels[ch].control &= !(1 << 31);
                        }
                    }
                }
                super::io_arm7::Write32Effect::None => {}
            }
            return;
        }
        match addr >> 24 {
            0x02 => {
                let off = (addr as usize) & 0x3F_FFFC;
                let b = val.to_le_bytes();
                self.shared.main_ram[off] = b[0];
                self.shared.main_ram[off + 1] = b[1];
                self.shared.main_ram[off + 2] = b[2];
                self.shared.main_ram[off + 3] = b[3];
            }
            0x03 => {
                let b = val.to_le_bytes();
                if in_arm7_private_wram(addr) {
                    let off = (addr as usize) & 0xFFFC;
                    self.mem.wram[off] = b[0];
                    self.mem.wram[off + 1] = b[1];
                    self.mem.wram[off + 2] = b[2];
                    self.mem.wram[off + 3] = b[3];
                } else if let Some(view) = self.shared.arm7_wram_view_mut() {
                    let off = wrap(addr, view.len()) & !3;
                    view[off] = b[0];
                    view[off + 1] = b[1];
                    view[off + 2] = b[2];
                    view[off + 3] = b[3];
                } else {
                    let off = (addr as usize) & 0xFFFC;
                    self.mem.wram[off] = b[0];
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
    use crate::cpu::{Cpu, CpuMode, Psr};
    use crate::interrupt::Irq;

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
    fn test_shared_wram_mirror_does_not_alias_arm7_private_wram() {
        let (mut mem, mut shared) = fresh();
        shared.wramcnt = 3;
        let mut bus = Bus7::new(&mut mem, &mut shared);

        bus.write32(0x0380_0C50, 0x1234_5678);
        bus.write32(0x0381_0C50, 0xAABB_CCDD);

        assert_eq!(bus.read32(0x0380_0C50), 0x1234_5678);
        assert_eq!(bus.read32(0x0381_0C50), 0xAABB_CCDD);
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

    #[test]
    fn test_synthetic_bios_irq_vector_uses_arm7_wram_slot() {
        let (mut mem, mut shared) = fresh();
        let mut bus = Bus7::new(&mut mem, &mut shared);

        bus.write32(0x0380_FFFC, 0x0238_1234);

        assert_eq!(bus.read32(0x0000_0018), 0xE92D_500F);
        assert_eq!(bus.read32(0x0000_001C), 0xE59F_0028);
        assert_eq!(bus.read32(0x0000_0020), 0xE590_0000);
        assert_eq!(bus.read32(0x0000_0024), 0xE350_0000);
        assert_eq!(bus.read32(0x0000_0028), 0x059F_0020);
        assert_eq!(bus.read32(0x0000_002C), 0x0590_0000);
        assert_eq!(bus.read32(0x0000_0030), 0xE350_0000);
        assert_eq!(bus.read32(0x0000_0034), 0x0A00_0002);
        assert_eq!(bus.read32(0x0000_0038), 0xE28F_E004);
        assert_eq!(bus.read32(0x0000_003C), 0xE12F_FF10);
        assert_eq!(bus.read32(0x0000_0040), 0xE1A0_0000);
        assert_eq!(bus.read32(0x0000_0044), 0xE8BD_500F);
        assert_eq!(bus.read32(0x0000_0048), 0xE25E_F004);
        assert_eq!(bus.read32(0x0000_004C), 0x0380_FFFC);
        assert_eq!(bus.read32(0x0000_0050), 0x03FF_FFFC);
    }

    #[test]
    fn test_synthetic_bios_undefined_vector_uses_arm7_handler_slot() {
        let (mut mem, mut shared) = fresh();
        mem.wram[0xFFDC..0xFFE0].copy_from_slice(&0x0238_1234u32.to_le_bytes());
        let mut bus = Bus7::new(&mut mem, &mut shared);

        assert_eq!(bus.read32(0x0000_0004), 0xEA00_0012);
        assert_eq!(bus.read32(0x0000_0054), 0xE92D_4000);
        assert_eq!(bus.read32(0x0000_0058), 0xE59F_0020);
        assert_eq!(bus.read32(0x0000_006C), 0xE12F_FF10);
        assert_eq!(bus.read32(0x0000_0074), 0xE8BD_4000);
        assert_eq!(bus.read32(0x0000_0078), 0xE1B0_F00E);
        assert_eq!(bus.read32(0x0000_0080), 0x0380_FFDC);
    }

    #[test]
    fn test_synthetic_bios_undefined_vector_is_absent_without_handler() {
        let (mut mem, mut shared) = fresh();
        let mut bus = Bus7::new(&mut mem, &mut shared);

        assert_eq!(bus.read32(0x0000_0004), 0xE1B0_F00E);
        assert_eq!(bus.read32(0x0000_0054), 0xFFFF_FFFF);
    }

    #[test]
    fn test_synthetic_bios_irq_vector_preserves_r0_without_handler() {
        let (mut mem, mut shared) = fresh();
        let mut cpu = Cpu::new_arm7();
        cpu.cpsr = Psr::new(CpuMode::System);
        cpu.cpsr.bits &= !(1 << 7);
        cpu.regs[0] = 0x1234_5678;
        cpu.regs[13] = 0x0380_FF00;
        cpu.banked.sp[CpuMode::Irq.bank_index()] = 0x0380_FF80;
        cpu.regs[15] = 0x0380_1000;
        cpu.pipeline_flushed = true;

        shared.irq7.ime = true;
        shared.irq7.ie = Irq::VBlank.bit();
        shared.irq7.request(Irq::VBlank);

        let mut bus = Bus7::new(&mut mem, &mut shared);
        bus.write32(0x0380_1000, 0xE1A0_0000); // NOP after returning.
        bus.write32(0x0380_FFFC, 0);

        for _ in 0..10 {
            cpu.step(&mut bus);
            if cpu.irq_entries == 1 {
                bus.shared.irq7.iflag = 0;
            }
            if cpu.irq_entries == 1 && cpu.cpsr.mode() == CpuMode::System {
                break;
            }
        }

        assert_eq!(cpu.regs[0], 0x1234_5678);
        assert_eq!(cpu.cpsr.mode(), CpuMode::System);
    }

    #[test]
    fn test_real_bios_bytes_are_not_replaced() {
        let mut mem = Arm7Memory::new(Some(vec![0; ARM7_BIOS_SIZE]));
        let mut shared = SharedState::new();
        let mut bus = Bus7::new(&mut mem, &mut shared);

        assert_eq!(bus.read32(0x0000_0018), 0);
    }
}
