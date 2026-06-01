//! ARM9-side bus.
//!
//! Constructed per-step from the top-level `Nds` struct so it can borrow
//! the ARM9-private memories alongside `SharedState`. See ARCHITECTURE.md
//! "Ownership Model" for the full pattern.

use super::shared::SharedState;
use crate::cpu::bus::CpuBus;
use crate::cpu::cp15::TcmRegion;
use crate::dma::{AddrControl, DmaTiming};
use serde::{Deserialize, Serialize};

pub const ITCM_SIZE: usize = 32 * 1024;
pub const DTCM_SIZE: usize = 16 * 1024;
pub const ARM9_BIOS_SIZE: usize = 4 * 1024;

/// ARM9-private memories owned by the top-level `Nds` struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Arm9Memory {
    #[serde(with = "super::shared::serde_bytes_vec")]
    pub itcm: Vec<u8>,
    #[serde(with = "super::shared::serde_bytes_vec")]
    pub dtcm: Vec<u8>,
    #[serde(with = "super::shared::serde_bytes_vec")]
    pub bios: Vec<u8>,
}

impl Arm9Memory {
    pub fn new(bios: Option<Vec<u8>>) -> Self {
        Arm9Memory {
            itcm: vec![0u8; ITCM_SIZE],
            dtcm: vec![0u8; DTCM_SIZE],
            bios: bios.unwrap_or_else(|| vec![0xFFu8; ARM9_BIOS_SIZE]),
        }
    }

    fn read_itcm32(&self, off: usize) -> u32 {
        let off = off & !3;
        if off == 0x18 {
            if self.has_calico_branch_to_compact_vectors() {
                return 0xEA00_0005; // B ITCM[0x34], the compact IRQ vector stub.
            }
            // Modern libnds/calico copies a compact low-vector table to ITCM:
            // reset, undef, swi, pabt, dabt, irq, then the six literals. ARM
            // hardware still vectors IRQ at 0x18. Depending on the crt0 copy,
            // the compact table may begin at ITCM[0] or ITCM[0x18]; in the
            // latter case IRQ lands on the reset stub. Point it at the compact
            // table's IRQ literal.
            if self.has_compact_calico_vectors_at(0) {
                return 0xE59F_F00C; // LDR PC, [PC, #0x0C] -> ITCM[0x2C]
            }
            if self.has_compact_calico_vectors_at(0x18) {
                return 0xE59F_F024; // LDR PC, [PC, #0x24] -> ITCM[0x44]
            }
        }
        if self.has_synthetic_bios() && self.low_vectors_are_blank() {
            if let Some(word) = synthetic_irq_vector_word(off) {
                return word;
            }
        }
        u32::from_le_bytes([
            self.itcm[off],
            self.itcm[off + 1],
            self.itcm[off + 2],
            self.itcm[off + 3],
        ])
    }

    fn has_compact_calico_vectors_at(&self, base: usize) -> bool {
        let ldr_pc_pc_16 = 0xE59F_F010u32.to_le_bytes();
        for off in (base..=base + 0x14).step_by(4) {
            if self.itcm[off..off + 4] != ldr_pc_pc_16 {
                return false;
            }
        }
        true
    }

    pub(crate) fn has_nintendo_sdk_irq_dispatcher_at_zero(&self) -> bool {
        const PREFIX: [u32; 4] = [
            0xE92D_4000, // STMDB SP!, {LR}
            0xE3A0_C301, // MOV R12, #0x04000000
            0xE28C_CE21, // ADD R12, R12, #0x210
            0xE51C_1008, // LDR R1, [R12, #-8]
        ];

        PREFIX.iter().enumerate().all(|(i, &word)| {
            let off = i * 4;
            u32::from_le_bytes([
                self.itcm[off],
                self.itcm[off + 1],
                self.itcm[off + 2],
                self.itcm[off + 3],
            ]) == word
        })
    }

    fn has_calico_branch_to_compact_vectors(&self) -> bool {
        let irq_branch = u32::from_le_bytes([
            self.itcm[0x18],
            self.itcm[0x19],
            self.itcm[0x1A],
            self.itcm[0x1B],
        ]);
        irq_branch == 0xEA7F_E005 && self.has_compact_calico_vectors_at(0x20)
    }

    fn has_synthetic_bios(&self) -> bool {
        self.bios.first().copied() == Some(0xFF)
    }

    fn low_vectors_are_blank(&self) -> bool {
        self.itcm[..0x18].iter().all(|&b| b == 0) && self.itcm[0x18..0x44].iter().all(|&b| b == 0)
    }

    pub(crate) fn has_installed_irq_vector(&self) -> bool {
        !self.low_vectors_are_blank()
    }
}

fn synthetic_irq_vector_word(off: usize) -> Option<u32> {
    match off {
        0x18 => Some(0xE92D_500F), // STMDB SP!, {R0-R3, R12, LR}
        0x1C => Some(0xE59F_0028), // LDR R0, [PC, #0x28] -> handler slot literal
        0x20 => Some(0xE590_0000), // LDR R0, [R0]
        0x24 => Some(0xE350_0000), // CMP R0, #0
        0x28 => Some(0x0AFF_FFFF), // BEQ null-handler IRQ ack
        0x2C => Some(0xE28F_E014), // ADD LR, PC, #0x14 -> wrapper epilogue
        0x30 => Some(0xE12F_FF10), // BX R0
        0x34 => Some(0xE59F_0018), // LDR R0, [PC, #0x18] -> ARM9 IE
        0x38 => Some(0xE590_1000), // LDR R1, [R0]     ; IE
        0x3C => Some(0xE590_2004), // LDR R2, [R0, #4] ; IF
        0x40 => Some(0xE001_1002), // AND R1, R1, R2   ; enabled pending
        0x44 => Some(0xE580_1004), // STR R1, [R0, #4] ; acknowledge
        0x48 => Some(0xE8BD_500F), // LDMIA SP!, {R0-R3, R12, LR}
        0x4C => Some(0xE25E_F004), // SUBS PC, LR, #4
        0x50 => Some(0x02FF_3FFC),
        0x54 => Some(0x0400_0210),
        _ => None,
    }
}

fn synthetic_sdk_irq_wrapper_word(off: usize) -> Option<u32> {
    match off {
        // The Nintendo SDK installs a callable IRQ dispatcher at ITCM[0].
        // It expects LR to point back to the BIOS IRQ wrapper; entering it
        // directly as the exception vector returns to game code in IRQ mode.
        0x18 => Some(0xE92D_500F), // STMDB SP!, {R0-R3, R12, LR}
        0x1C => Some(0xE28F_E008), // ADD LR, PC, #0x08 -> wrapper epilogue.
        0x20 => Some(0xE59F_F010), // LDR PC, [PC, #0x10] -> ITCM[0].
        0x2C => Some(0xE8BD_500F), // LDMIA SP!, {R0-R3, R12, LR}
        0x30 => Some(0xE25E_F004), // SUBS PC, LR, #4
        0x38 => Some(0x0000_0000),
        _ => None,
    }
}

/// A borrow of ARM9 state suitable to step the CPU against.
pub struct Bus9<'a> {
    pub mem: &'a mut Arm9Memory,
    pub shared: &'a mut SharedState,
    pub itcm_region: TcmRegion,
    pub dtcm_region: TcmRegion,
}

impl<'a> Bus9<'a> {
    /// Construct a fresh view. The caller must pass current TCM regions
    /// (read off the CPU's CP15 just before stepping).
    pub fn new(
        mem: &'a mut Arm9Memory,
        shared: &'a mut SharedState,
        itcm_region: TcmRegion,
        dtcm_region: TcmRegion,
    ) -> Self {
        Bus9 {
            mem,
            shared,
            itcm_region,
            dtcm_region,
        }
    }

    /// Execute the next chunk of channel `id`'s DMA transfer to completion
    /// (one trigger). Returns `irq_request` = whether to raise the
    /// channel's IRQ on the ARM9 controller.
    pub fn run_dma(&mut self, id: usize) -> bool {
        let (count, word_size, src_ctrl, dst_ctrl, irq_on_complete, timing) = {
            let d = &self.shared.dma9;
            (
                d.channels[id].internal_count,
                d.word_size(id),
                d.src_control(id),
                d.dst_control(id),
                d.irq_on_complete(id),
                d.timing(id),
            )
        };
        let transfer_count = if timing == DmaTiming::GxFifo {
            // Hardware GXFIFO DMA feeds at most 112 words per half-empty
            // trigger, then waits for the next trigger if data remains.
            count.min(112)
        } else {
            count
        };

        for _ in 0..transfer_count {
            let (sad, dad) = {
                let c = &self.shared.dma9.channels[id];
                (c.internal_sad, c.internal_dad)
            };
            if word_size == 4 {
                let v = self.read32(sad);
                if timing == DmaTiming::GxFifo && is_gxfifo_packed_addr(dad) {
                    let _ = super::io_arm9::write_io32(self.shared, dad, v);
                } else {
                    self.write32(dad, v);
                }
            } else {
                let v = self.read16(sad);
                self.write16(dad, v);
            }
            advance(
                &mut self.shared.dma9.channels[id].internal_sad,
                src_ctrl,
                word_size,
            );
            advance(
                &mut self.shared.dma9.channels[id].internal_dad,
                dst_ctrl,
                word_size,
            );
        }

        let completed = self.shared.dma9.finish_transfer_chunk(id, transfer_count);
        completed && irq_on_complete
    }
}

fn is_gxfifo_packed_addr(addr: u32) -> bool {
    let local = addr & 0x00FF_FFFC;
    (0x0400..0x0440).contains(&local)
}

fn advance(addr: &mut u32, ctrl: AddrControl, word: u32) {
    match ctrl {
        AddrControl::Increment | AddrControl::IncrementReload => *addr = addr.wrapping_add(word),
        AddrControl::Decrement => *addr = addr.wrapping_sub(word),
        AddrControl::Fixed => {}
    }
}

#[inline]
fn wram_addr_in_view(view_len: usize, addr: u32) -> usize {
    // The 0x03000000 window (8 MB span) mirrors the WRAM view. Mask the
    // address to the window then take it modulo the view size.
    let off = (addr & 0x007F_FFFF) as usize;
    off % view_len.max(1)
}

impl<'a> CpuBus for Bus9<'a> {
    fn irq_pending(&self) -> bool {
        self.shared.irq9.has_pending()
    }

    fn read8(&mut self, addr: u32) -> u8 {
        // DTCM and ITCM are checked first — if enabled they shadow the
        // physical memory map.
        if self.dtcm_region.contains(addr) {
            let off = addr.wrapping_sub(self.dtcm_region.base) as usize % DTCM_SIZE;
            return self.mem.dtcm[off];
        }
        if self.itcm_region.contains(addr) {
            let off = (addr as usize) % ITCM_SIZE;
            return self.mem.itcm[off];
        }

        match addr >> 24 {
            0x02 => self.shared.main_ram[(addr as usize) & 0x3F_FFFF],
            0x03 => {
                if let Some((view, _)) = self.shared.arm9_wram_view() {
                    let off = wram_addr_in_view(view.len(), addr);
                    view[off]
                } else {
                    0
                }
            }
            0x04 => super::io_arm9::read_io8(self.shared, addr),
            0x05 => self.shared.palette[(addr as usize) & 0x7FF],
            0x06 => self.shared.vram.cpu_read_arm9(addr),
            0x07 => self.shared.oam[(addr as usize) & 0x7FF],
            0xFF => {
                if addr & 0xFFFF_F000 == 0xFFFF_0000 {
                    self.mem.bios[(addr as usize) & 0xFFF]
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    fn read16(&mut self, addr: u32) -> u16 {
        if addr >> 24 == 0x04 {
            return super::io_arm9::read_io16(self.shared, addr);
        }
        let lo = self.read8(addr) as u16;
        let hi = self.read8(addr.wrapping_add(1)) as u16;
        lo | (hi << 8)
    }

    fn read32(&mut self, addr: u32) -> u32 {
        if self.dtcm_region.contains(addr) {
            let off = addr.wrapping_sub(self.dtcm_region.base) as usize & !3;
            let off = off % DTCM_SIZE;
            return u32::from_le_bytes([
                self.mem.dtcm[off],
                self.mem.dtcm[off + 1],
                self.mem.dtcm[off + 2],
                self.mem.dtcm[off + 3],
            ]);
        }
        if self.itcm_region.contains(addr) {
            let off = (addr as usize) & !3;
            let off = off % ITCM_SIZE;
            return self.mem.read_itcm32(off);
        }

        match addr >> 24 {
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
                let view = match self.shared.arm9_wram_view() {
                    Some((v, _)) => v,
                    None => return 0,
                };
                let off = wram_addr_in_view(view.len(), addr) & !3;
                u32::from_le_bytes([view[off], view[off + 1], view[off + 2], view[off + 3]])
            }
            0x04 => super::io_arm9::read_io32_mut(self.shared, addr),
            0x05 => {
                let off = (addr as usize) & 0x7FC;
                u32::from_le_bytes([
                    self.shared.palette[off],
                    self.shared.palette[off + 1],
                    self.shared.palette[off + 2],
                    self.shared.palette[off + 3],
                ])
            }
            0x06 => {
                let a = addr & !3;
                let b0 = self.shared.vram.cpu_read_arm9(a) as u32;
                let b1 = self.shared.vram.cpu_read_arm9(a + 1) as u32;
                let b2 = self.shared.vram.cpu_read_arm9(a + 2) as u32;
                let b3 = self.shared.vram.cpu_read_arm9(a + 3) as u32;
                b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
            }
            0x07 => {
                let off = (addr as usize) & 0x7FC;
                u32::from_le_bytes([
                    self.shared.oam[off],
                    self.shared.oam[off + 1],
                    self.shared.oam[off + 2],
                    self.shared.oam[off + 3],
                ])
            }
            0xFF => {
                if addr & 0xFFFF_F000 == 0xFFFF_0000 {
                    let off = (addr as usize) & 0xFFC;
                    if self.mem.has_synthetic_bios() {
                        if self.mem.has_nintendo_sdk_irq_dispatcher_at_zero() {
                            if let Some(word) = synthetic_sdk_irq_wrapper_word(off) {
                                return word;
                            }
                        }
                        if !self.mem.low_vectors_are_blank() && off < ITCM_SIZE {
                            return self.mem.read_itcm32(off);
                        }
                        if let Some(word) = synthetic_irq_vector_word(off) {
                            return word;
                        }
                    }
                    u32::from_le_bytes([
                        self.mem.bios[off],
                        self.mem.bios[off + 1],
                        self.mem.bios[off + 2],
                        self.mem.bios[off + 3],
                    ])
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    fn write8(&mut self, addr: u32, val: u8) {
        if self.dtcm_region.contains(addr) {
            let off = addr.wrapping_sub(self.dtcm_region.base) as usize % DTCM_SIZE;
            self.mem.dtcm[off] = val;
            return;
        }
        if self.itcm_region.contains(addr) {
            let off = (addr as usize) % ITCM_SIZE;
            self.mem.itcm[off] = val;
            return;
        }

        match addr >> 24 {
            0x02 => self.shared.main_ram[(addr as usize) & 0x3F_FFFF] = val,
            0x03 => {
                if let Some(view) = self.shared.arm9_wram_view_mut() {
                    let off = wram_addr_in_view(view.len(), addr);
                    view[off] = val;
                }
            }
            0x04 => super::io_arm9::write_io8(self.shared, addr, val),
            // Palette / OAM byte writes are dropped. VRAM byte writes are
            // valid and are used by small homebrew framebuffer renderers.
            0x05 | 0x07 => {}
            0x06 => self.shared.vram.cpu_write_arm9(addr, val),
            _ => {}
        }
    }

    fn write16(&mut self, addr: u32, val: u16) {
        match addr >> 24 {
            0x04 => super::io_arm9::write_io16(self.shared, addr, val),
            0x05 => {
                let off = (addr as usize) & 0x7FE;
                let b = val.to_le_bytes();
                self.shared.palette[off] = b[0];
                self.shared.palette[off + 1] = b[1];
            }
            0x06 => {
                let a = addr & !1;
                let b = val.to_le_bytes();
                self.shared.vram.cpu_write_arm9(a, b[0]);
                self.shared.vram.cpu_write_arm9(a + 1, b[1]);
            }
            0x07 => {
                let off = (addr as usize) & 0x7FE;
                let b = val.to_le_bytes();
                self.shared.oam[off] = b[0];
                self.shared.oam[off + 1] = b[1];
            }
            _ => {
                let bytes = val.to_le_bytes();
                self.write8(addr, bytes[0]);
                self.write8(addr.wrapping_add(1), bytes[1]);
            }
        }
    }

    fn write32(&mut self, addr: u32, val: u32) {
        if self.dtcm_region.contains(addr) {
            let off = addr.wrapping_sub(self.dtcm_region.base) as usize & !3;
            let off = off % DTCM_SIZE;
            let b = val.to_le_bytes();
            self.mem.dtcm[off] = b[0];
            self.mem.dtcm[off + 1] = b[1];
            self.mem.dtcm[off + 2] = b[2];
            self.mem.dtcm[off + 3] = b[3];
            return;
        }
        if self.itcm_region.contains(addr) {
            let off = (addr as usize) & !3;
            let off = off % ITCM_SIZE;
            let b = val.to_le_bytes();
            self.mem.itcm[off] = b[0];
            self.mem.itcm[off + 1] = b[1];
            self.mem.itcm[off + 2] = b[2];
            self.mem.itcm[off + 3] = b[3];
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
                if let Some(view) = self.shared.arm9_wram_view_mut() {
                    let off = wram_addr_in_view(view.len(), addr) & !3;
                    let b = val.to_le_bytes();
                    view[off] = b[0];
                    view[off + 1] = b[1];
                    view[off + 2] = b[2];
                    view[off + 3] = b[3];
                }
            }
            0x04 => {
                let effect = super::io_arm9::write_io32(self.shared, addr, val);
                match effect {
                    super::io_arm9::Write32Effect::RunDma9(ch) => {
                        let irq = self.run_dma(ch);
                        if irq {
                            use crate::interrupt::Irq;
                            let irq_bit = match ch {
                                0 => Irq::Dma0,
                                1 => Irq::Dma1,
                                2 => Irq::Dma2,
                                _ => Irq::Dma3,
                            };
                            self.shared.irq9.request(irq_bit);
                        }
                    }
                    super::io_arm9::Write32Effect::FireSlot1Dma => {
                        let channels = self
                            .shared
                            .dma9
                            .channels_for_timing(crate::dma::DmaTiming::Slot1);
                        for ch in channels {
                            while self.shared.dma9.channels[ch].active
                                && self.shared.dma9.timing(ch) == crate::dma::DmaTiming::Slot1
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
                                    self.shared.irq9.request(irq_bit);
                                }
                                if self.shared.slot1_data.len() >= before {
                                    break;
                                }
                            }
                            if self.shared.slot1_data.is_empty()
                                && self.shared.dma9.timing(ch) == crate::dma::DmaTiming::Slot1
                            {
                                self.shared.dma9.channels[ch].active = false;
                                self.shared.dma9.channels[ch].control &= !(1 << 31);
                            }
                        }
                    }
                    super::io_arm9::Write32Effect::FireGxFifoDma => {
                        // Fire any ARM9 DMA channel armed for the GxFifo
                        // start mode. The carry-over from Phase 4.
                        let channels = self
                            .shared
                            .dma9
                            .channels_for_timing(crate::dma::DmaTiming::GxFifo);
                        for ch in channels {
                            let irq = self.run_dma(ch);
                            if irq {
                                use crate::interrupt::Irq;
                                let irq_bit = match ch {
                                    0 => Irq::Dma0,
                                    1 => Irq::Dma1,
                                    2 => Irq::Dma2,
                                    _ => Irq::Dma3,
                                };
                                self.shared.irq9.request(irq_bit);
                            }
                        }
                    }
                    super::io_arm9::Write32Effect::None => {}
                }
            }
            0x05 => {
                let off = (addr as usize) & 0x7FC;
                let b = val.to_le_bytes();
                for i in 0..4 {
                    self.shared.palette[off + i] = b[i];
                }
            }
            0x06 => {
                let a = addr & !3;
                let b = val.to_le_bytes();
                for i in 0..4 {
                    self.shared.vram.cpu_write_arm9(a + i as u32, b[i]);
                }
            }
            0x07 => {
                let off = (addr as usize) & 0x7FC;
                let b = val.to_le_bytes();
                for i in 0..4 {
                    self.shared.oam[off + i] = b[i];
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::cp15::TcmRegion;

    fn fresh() -> (Arm9Memory, SharedState) {
        (Arm9Memory::new(None), SharedState::new())
    }

    #[test]
    fn test_main_ram_round_trip() {
        let (mut mem, mut shared) = fresh();
        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion::disabled(),
            TcmRegion::disabled(),
        );
        bus.write32(0x0200_1000, 0xDEAD_BEEF);
        assert_eq!(bus.read32(0x0200_1000), 0xDEAD_BEEF);
    }

    #[test]
    fn test_main_ram_mirrors() {
        let (mut mem, mut shared) = fresh();
        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion::disabled(),
            TcmRegion::disabled(),
        );
        bus.write32(0x0200_0000, 0xAABB_CCDD);
        // 0x02400000 is +4MB and should mirror the same 4MB.
        assert_eq!(bus.read32(0x0240_0000), 0xAABB_CCDD);
    }

    #[test]
    fn test_itcm_shadows_main_decode() {
        let (mut mem, mut shared) = fresh();
        // 64 KB ITCM window — physical ITCM is still 32 KB, so the second
        // 32 KB of the window mirrors the first.
        let itcm = TcmRegion {
            base: 0,
            size_bytes: 64 * 1024,
        };
        let mut bus = Bus9::new(&mut mem, &mut shared, itcm, TcmRegion::disabled());
        bus.write32(0x0000_0010, 0x1234_5678);
        assert_eq!(bus.read32(0x0000_0010), 0x1234_5678);
        // Mirror at +32K within the 64K window
        assert_eq!(bus.read32(0x0000_8010), 0x1234_5678);
        // Outside the window — falls through to other decoders (open bus)
        assert_eq!(bus.read32(0x0001_0010), 0);
    }

    #[test]
    fn test_compact_calico_irq_vector_fetch_points_to_irq_literal() {
        let (mut mem, mut shared) = fresh();
        let ldr = 0xE59F_F010u32.to_le_bytes();
        for off in (0..=0x14).step_by(4) {
            mem.itcm[off..off + 4].copy_from_slice(&ldr);
        }
        mem.itcm[0x18..0x1C].copy_from_slice(&0x01FF_84F0u32.to_le_bytes());
        mem.itcm[0x2C..0x30].copy_from_slice(&0x01FF_8580u32.to_le_bytes());

        let itcm = TcmRegion {
            base: 0,
            size_bytes: 32 * 1024,
        };
        let mut bus = Bus9::new(&mut mem, &mut shared, itcm, TcmRegion::disabled());

        assert_eq!(bus.read32(0x0000_0018), 0xE59F_F00C);
        assert_eq!(bus.read32(0x0000_002C), 0x01FF_8580);
    }

    #[test]
    fn test_shifted_compact_calico_irq_vector_fetch_points_to_irq_literal() {
        let (mut mem, mut shared) = fresh();
        let ldr = 0xE59F_F010u32.to_le_bytes();
        for off in (0x18..=0x2C).step_by(4) {
            mem.itcm[off..off + 4].copy_from_slice(&ldr);
        }
        mem.itcm[0x30..0x34].copy_from_slice(&0x01FF_84F0u32.to_le_bytes());
        mem.itcm[0x44..0x48].copy_from_slice(&0x01FF_8580u32.to_le_bytes());

        let itcm = TcmRegion {
            base: 0,
            size_bytes: 32 * 1024,
        };
        let mut bus = Bus9::new(&mut mem, &mut shared, itcm, TcmRegion::disabled());

        assert_eq!(bus.read32(0x0000_0018), 0xE59F_F024);
        assert_eq!(bus.read32(0x0000_0044), 0x01FF_8580);
    }

    #[test]
    fn test_calico_branch_table_irq_vector_fetch_points_to_irq_stub() {
        let (mut mem, mut shared) = fresh();
        mem.itcm[0x18..0x1C].copy_from_slice(&0xEA7F_E005u32.to_le_bytes());
        let ldr = 0xE59F_F010u32.to_le_bytes();
        for off in (0x20..=0x34).step_by(4) {
            mem.itcm[off..off + 4].copy_from_slice(&ldr);
        }
        mem.itcm[0x4C..0x50].copy_from_slice(&0x01FF_8580u32.to_le_bytes());

        let itcm = TcmRegion {
            base: 0,
            size_bytes: 32 * 1024,
        };
        let mut bus = Bus9::new(&mut mem, &mut shared, itcm, TcmRegion::disabled());

        assert_eq!(bus.read32(0x0000_0018), 0xEA00_0005);
        assert_eq!(bus.read32(0x0000_0034), 0xE59F_F010);
        assert_eq!(bus.read32(0x0000_004C), 0x01FF_8580);
    }

    #[test]
    fn test_vram_byte_write_updates_lcdc_bank() {
        let (mut mem, mut shared) = fresh();
        shared.vram.write_cnt(crate::vram::BankId::A, 0x80);
        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion::disabled(),
            TcmRegion::disabled(),
        );

        bus.write8(0x0680_0080, 0x7B);

        assert_eq!(bus.shared.vram.read_lcdc(0x80), 0x7B);
    }

    #[test]
    fn test_dtcm_at_high_base() {
        let (mut mem, mut shared) = fresh();
        let dtcm = TcmRegion {
            base: 0x027C_0000,
            size_bytes: 16 * 1024,
        };
        let mut bus = Bus9::new(&mut mem, &mut shared, TcmRegion::disabled(), dtcm);
        bus.write32(0x027C_0000, 0xCAFEBABE);
        assert_eq!(bus.read32(0x027C_0000), 0xCAFEBABE);
        // Adjacent main-RAM byte should still be 0
        assert_eq!(bus.read32(0x027C_4000), 0);
    }

    #[test]
    fn test_wram_visible_in_mode_0() {
        let (mut mem, mut shared) = fresh();
        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion::disabled(),
            TcmRegion::disabled(),
        );
        bus.write32(0x0300_0010, 0xABCD_EF01);
        assert_eq!(bus.read32(0x0300_0010), 0xABCD_EF01);
    }

    #[test]
    fn test_wram_invisible_in_mode_3() {
        let (mut mem, mut shared) = fresh();
        shared.wramcnt = 3;
        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion::disabled(),
            TcmRegion::disabled(),
        );
        bus.write32(0x0300_0000, 0xDEAD);
        // Reads return 0 (open bus stub) when ARM9 has no WRAM mapping
        assert_eq!(bus.read32(0x0300_0000), 0);
    }

    #[test]
    fn test_high_vector_bios_read() {
        let mut mem = Arm9Memory::new(Some(vec![0xAB; 4096]));
        let mut shared = SharedState::new();
        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion::disabled(),
            TcmRegion::disabled(),
        );
        assert_eq!(bus.read32(0xFFFF_0000), 0xABAB_ABAB);
    }

    #[test]
    fn test_synthetic_high_vectors_use_installed_itcm_vectors() {
        let (mut mem, mut shared) = fresh();
        mem.itcm[0x18..0x1C].copy_from_slice(&0xE59F_F00Cu32.to_le_bytes());

        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion::disabled(),
            TcmRegion::disabled(),
        );

        assert_eq!(bus.read32(0xFFFF_0018), 0xE59F_F00C);
    }

    #[test]
    fn test_high_vector_irq_enters_nintendo_sdk_dispatcher_prologue() {
        let (mut mem, mut shared) = fresh();
        for (off, word) in [
            (0x00, 0xE92D_4000u32),
            (0x04, 0xE3A0_C301),
            (0x08, 0xE28C_CE21),
            (0x0C, 0xE51C_1008),
            (0x18, 0xE89C_0006),
        ] {
            mem.itcm[off..off + 4].copy_from_slice(&word.to_le_bytes());
        }

        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion {
                base: 0,
                size_bytes: 32 * 1024,
            },
            TcmRegion::disabled(),
        );

        assert_eq!(bus.read32(0x0000_0018), 0xE89C_0006);
        assert_eq!(bus.read32(0xFFFF_0018), 0xE92D_500F);
        assert_eq!(bus.read32(0xFFFF_001C), 0xE28F_E008);
        assert_eq!(bus.read32(0xFFFF_0020), 0xE59F_F010);
        assert_eq!(bus.read32(0xFFFF_002C), 0xE8BD_500F);
        assert_eq!(bus.read32(0xFFFF_0030), 0xE25E_F004);
        assert_eq!(bus.read32(0xFFFF_0038), 0x0000_0000);
    }

    #[test]
    fn test_synthetic_high_vector_null_handler_path_acks_irq() {
        let (mut mem, mut shared) = fresh();
        let mut bus = Bus9::new(
            &mut mem,
            &mut shared,
            TcmRegion::disabled(),
            TcmRegion::disabled(),
        );

        assert_eq!(bus.read32(0xFFFF_001C), 0xE59F_0028);
        assert_eq!(bus.read32(0xFFFF_0028), 0x0AFF_FFFF);
        assert_eq!(bus.read32(0xFFFF_0034), 0xE59F_0018);
        assert_eq!(bus.read32(0xFFFF_0044), 0xE580_1004);
        assert_eq!(bus.read32(0xFFFF_0050), 0x02FF_3FFC);
        assert_eq!(bus.read32(0xFFFF_0054), 0x0400_0210);
    }
}
