//! VRAM banks A-I with VRAMCNT routing.
//!
//! Each bank is a fixed byte buffer plus a `VRAMCNT_x` register (0x04000240
//! ..0x04000249, with 0x247 = WRAMCNT). The (mst, offset) fields decide
//! where the bank shows up — LCDC, Engine A/B BG, Engine A/B OBJ, ARM7
//! mirror, texture image, texture palette, BG/OBJ ext-pal.
//!
//! Reads/writes in the 0x06xxxxxx CPU window, and reads from the engines'
//! viewpoints, all funnel through the same `VramRouter` so a write touches
//! every bank that shares the address.

use serde::{Deserialize, Serialize};

pub const A_SIZE: usize = 128 * 1024;
pub const B_SIZE: usize = 128 * 1024;
pub const C_SIZE: usize = 128 * 1024;
pub const D_SIZE: usize = 128 * 1024;
pub const E_SIZE: usize = 64 * 1024;
pub const F_SIZE: usize = 16 * 1024;
pub const G_SIZE: usize = 16 * 1024;
pub const H_SIZE: usize = 32 * 1024;
pub const I_SIZE: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BankId { A, B, C, D, E, F, G, H, I }

impl BankId {
    pub const ALL: [BankId; 9] = [
        BankId::A, BankId::B, BankId::C, BankId::D,
        BankId::E, BankId::F, BankId::G, BankId::H, BankId::I,
    ];

    pub fn size(self) -> usize {
        match self {
            BankId::A => A_SIZE, BankId::B => B_SIZE,
            BankId::C => C_SIZE, BankId::D => D_SIZE,
            BankId::E => E_SIZE, BankId::F => F_SIZE,
            BankId::G => G_SIZE, BankId::H => H_SIZE,
            BankId::I => I_SIZE,
        }
    }
}

/// Logical region a bank can be exposed in, plus the (target-relative)
/// base offset it occupies. The bank's whole size is mapped contiguously
/// from this base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VramTarget {
    /// Bank not in use anywhere.
    Disabled,
    /// LCDC mirror at 0x06800000+offset (where offset is bank-specific).
    Lcdc { lcdc_offset: u32 },
    /// Engine A BG at 0x06000000+base.
    EngineABg { base: u32 },
    /// Engine A OBJ at 0x06400000+base.
    EngineAObj { base: u32 },
    /// Engine B BG at 0x06200000+base.
    EngineBBg { base: u32 },
    /// Engine B OBJ at 0x06600000+base.
    EngineBObj { base: u32 },
    /// ARM7 mirror at 0x06000000+base (ARM7 view).
    Arm7 { base: u32 },
    /// 3D texture image, slot 0..3 × 128 KB.
    TextureImage { slot: u8 },
    /// 3D texture palette, slot 0..5 × 16 KB.
    TexturePalette { slot: u8 },
    /// Engine A BG extended palette, slot 0..3 × 8 KB.
    BgExtPalA { slot: u8 },
    /// Engine A OBJ extended palette, 8 KB.
    ObjExtPalA,
    /// Engine B BG extended palette, slot 0..3 × 8 KB.
    BgExtPalB { slot: u8 },
    /// Engine B OBJ extended palette, 8 KB.
    ObjExtPalB,
}

/// One VRAM bank.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VramBank {
    pub id: BankId,
    /// `VRAMCNT_x` register byte. Bit 7 = enable, bits 4..3 reserved on
    /// most banks, bits 6..3 hold the offset (encoded variably per bank),
    /// bits 2..0 are the mst (mode select).
    pub cnt: u8,
    #[serde(with = "crate::bus::shared::serde_bytes_vec")]
    pub data: Vec<u8>,
    /// Decoded target — recomputed whenever `cnt` is written.
    pub target: VramTarget,
}

impl VramBank {
    pub fn new(id: BankId) -> Self {
        VramBank {
            id,
            cnt: 0,
            data: vec![0u8; id.size()],
            target: VramTarget::Disabled,
        }
    }

    /// Set VRAMCNT_x, recomputing the decoded target.
    pub fn write_cnt(&mut self, val: u8) {
        self.cnt = val;
        self.target = decode_target(self.id, val);
    }
}

/// Decode `VRAMCNT_x` for a given bank. The disabled target is returned
/// for unsupported (mst, offset) combinations — those are mostly impossible
/// hardware configurations, so we treat the bank as disabled rather than
/// crashing.
pub fn decode_target(bank: BankId, cnt: u8) -> VramTarget {
    if cnt & 0x80 == 0 {
        return VramTarget::Disabled;
    }
    let mst = cnt & 0x07;
    let offset = (cnt >> 3) & 0x03;

    match (bank, mst) {
        // ─── Bank A (128 KB) — mst 0..3
        (BankId::A, 0) => VramTarget::Lcdc { lcdc_offset: 0x00000 },
        (BankId::A, 1) => VramTarget::EngineABg { base: (offset as u32) * 0x20000 },
        (BankId::A, 2) => VramTarget::EngineAObj { base: ((offset & 1) as u32) * 0x20000 },
        (BankId::A, 3) => VramTarget::TextureImage { slot: offset },

        // ─── Bank B (128 KB)
        (BankId::B, 0) => VramTarget::Lcdc { lcdc_offset: 0x20000 },
        (BankId::B, 1) => VramTarget::EngineABg { base: (offset as u32) * 0x20000 },
        (BankId::B, 2) => VramTarget::EngineAObj { base: ((offset & 1) as u32) * 0x20000 },
        (BankId::B, 3) => VramTarget::TextureImage { slot: offset },

        // ─── Bank C (128 KB)
        (BankId::C, 0) => VramTarget::Lcdc { lcdc_offset: 0x40000 },
        (BankId::C, 1) => VramTarget::EngineABg { base: (offset as u32) * 0x20000 },
        (BankId::C, 2) => VramTarget::Arm7 { base: ((offset & 1) as u32) * 0x20000 },
        (BankId::C, 3) => VramTarget::TextureImage { slot: offset },
        (BankId::C, 4) => VramTarget::EngineBBg { base: 0 },

        // ─── Bank D (128 KB)
        (BankId::D, 0) => VramTarget::Lcdc { lcdc_offset: 0x60000 },
        (BankId::D, 1) => VramTarget::EngineABg { base: (offset as u32) * 0x20000 },
        (BankId::D, 2) => VramTarget::Arm7 { base: ((offset & 1) as u32) * 0x20000 },
        (BankId::D, 3) => VramTarget::TextureImage { slot: offset },
        (BankId::D, 4) => VramTarget::EngineBObj { base: 0 },

        // ─── Bank E (64 KB)
        (BankId::E, 0) => VramTarget::Lcdc { lcdc_offset: 0x80000 },
        (BankId::E, 1) => VramTarget::EngineABg { base: 0 },
        (BankId::E, 2) => VramTarget::EngineAObj { base: 0 },
        (BankId::E, 3) => VramTarget::TexturePalette { slot: 0 },
        (BankId::E, 4) => VramTarget::BgExtPalA { slot: 0 },

        // ─── Bank F (16 KB) — offset encoding is bit 1 (×0x4000) | bit 0 (×0x10000)
        (BankId::F, 0) => VramTarget::Lcdc { lcdc_offset: 0x90000 },
        (BankId::F, 1) => VramTarget::EngineABg { base: f_g_bg_offset(offset) },
        (BankId::F, 2) => VramTarget::EngineAObj { base: f_g_bg_offset(offset) },
        (BankId::F, 3) => VramTarget::TexturePalette { slot: f_g_texpal_slot(offset) },
        (BankId::F, 4) => VramTarget::BgExtPalA { slot: (offset & 1) as u8 },
        (BankId::F, 5) => VramTarget::ObjExtPalA,

        // ─── Bank G (16 KB)
        (BankId::G, 0) => VramTarget::Lcdc { lcdc_offset: 0x94000 },
        (BankId::G, 1) => VramTarget::EngineABg { base: f_g_bg_offset(offset) },
        (BankId::G, 2) => VramTarget::EngineAObj { base: f_g_bg_offset(offset) },
        (BankId::G, 3) => VramTarget::TexturePalette { slot: f_g_texpal_slot(offset) },
        (BankId::G, 4) => VramTarget::BgExtPalA { slot: (offset & 1) as u8 },
        (BankId::G, 5) => VramTarget::ObjExtPalA,

        // ─── Bank H (32 KB)
        (BankId::H, 0) => VramTarget::Lcdc { lcdc_offset: 0x98000 },
        (BankId::H, 1) => VramTarget::EngineBBg { base: 0 },
        (BankId::H, 2) => VramTarget::BgExtPalB { slot: 0 },

        // ─── Bank I (16 KB)
        (BankId::I, 0) => VramTarget::Lcdc { lcdc_offset: 0xA0000 },
        (BankId::I, 1) => VramTarget::EngineBBg { base: 0x8000 },
        (BankId::I, 2) => VramTarget::EngineBObj { base: 0 },
        (BankId::I, 3) => VramTarget::ObjExtPalB,

        _ => VramTarget::Disabled,
    }
}

/// F/G BG/OBJ offset: bit 0 of the offset field selects 0x0000 vs 0x4000,
/// bit 1 selects an extra +0x10000.
fn f_g_bg_offset(offset: u8) -> u32 {
    let lo = (offset & 1) as u32 * 0x4000;
    let hi = ((offset >> 1) & 1) as u32 * 0x10000;
    lo + hi
}

fn f_g_texpal_slot(offset: u8) -> u8 {
    // Same encoding as f_g_bg_offset but expressed as a 16 KB slot index.
    let lo = offset & 1;
    let hi = (offset >> 1) & 1;
    (hi << 1) | lo
}

/// VRAM router: all 9 banks + helpers to dispatch CPU/engine accesses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VramRouter {
    pub banks: [VramBank; 9],
}

impl VramRouter {
    pub fn new() -> Self {
        VramRouter {
            banks: [
                VramBank::new(BankId::A),
                VramBank::new(BankId::B),
                VramBank::new(BankId::C),
                VramBank::new(BankId::D),
                VramBank::new(BankId::E),
                VramBank::new(BankId::F),
                VramBank::new(BankId::G),
                VramBank::new(BankId::H),
                VramBank::new(BankId::I),
            ],
        }
    }

    /// Set VRAMCNT_x by bank id.
    pub fn write_cnt(&mut self, bank: BankId, val: u8) {
        self.banks[bank as usize].write_cnt(val);
    }

    pub fn read_cnt(&self, bank: BankId) -> u8 {
        self.banks[bank as usize].cnt
    }

    /// CPU read at an ARM9-side address `0x06xxxxxx`. Returns the byte from
    /// the first bank that maps the address; returns 0 if no bank does.
    pub fn cpu_read_arm9(&self, addr: u32) -> u8 {
        for bank in &self.banks {
            if let Some(b) = bank_read_arm9(bank, addr) {
                return b;
            }
        }
        0
    }

    /// CPU write at an ARM9-side address. Writes to *every* bank that maps
    /// this address (real hardware behavior — overlapping mappings are
    /// stored in all participating banks).
    pub fn cpu_write_arm9(&mut self, addr: u32, val: u8) {
        for bank in &mut self.banks {
            bank_write_arm9(bank, addr, val);
        }
    }

    /// CPU read on the ARM7-side at `0x06xxxxxx`. Only banks routed to
    /// `Arm7 { .. }` participate.
    pub fn cpu_read_arm7(&self, addr: u32) -> u8 {
        for bank in &self.banks {
            if let VramTarget::Arm7 { base } = bank.target {
                let span = bank.id.size() as u32;
                let win_addr = addr & 0x003F_FFFF; // 4 MB ARM7 window
                if win_addr >= base && win_addr < base + span {
                    let off = (win_addr - base) as usize;
                    return bank.data[off];
                }
            }
        }
        0
    }

    pub fn cpu_write_arm7(&mut self, addr: u32, val: u8) {
        let win_addr = addr & 0x003F_FFFF;
        for bank in &mut self.banks {
            if let VramTarget::Arm7 { base } = bank.target {
                let span = bank.id.size() as u32;
                if win_addr >= base && win_addr < base + span {
                    let off = (win_addr - base) as usize;
                    bank.data[off] = val;
                }
            }
        }
    }

    /// Read a byte from the Engine A BG window at `bg_addr` (offset within
    /// the 512 KB BG window).
    pub fn read_engine_a_bg(&self, bg_addr: u32) -> u8 {
        for bank in &self.banks {
            if let VramTarget::EngineABg { base } = bank.target {
                let span = bank.id.size() as u32;
                if bg_addr >= base && bg_addr < base + span {
                    return bank.data[(bg_addr - base) as usize];
                }
            }
        }
        0
    }

    pub fn read_engine_a_obj(&self, addr: u32) -> u8 {
        for bank in &self.banks {
            if let VramTarget::EngineAObj { base } = bank.target {
                let span = bank.id.size() as u32;
                if addr >= base && addr < base + span {
                    return bank.data[(addr - base) as usize];
                }
            }
        }
        0
    }

    pub fn read_engine_b_bg(&self, addr: u32) -> u8 {
        for bank in &self.banks {
            if let VramTarget::EngineBBg { base } = bank.target {
                let span = bank.id.size() as u32;
                if addr >= base && addr < base + span {
                    return bank.data[(addr - base) as usize];
                }
            }
        }
        0
    }

    pub fn read_engine_b_obj(&self, addr: u32) -> u8 {
        for bank in &self.banks {
            if let VramTarget::EngineBObj { base } = bank.target {
                let span = bank.id.size() as u32;
                if addr >= base && addr < base + span {
                    return bank.data[(addr - base) as usize];
                }
            }
        }
        0
    }

    /// Read 16-bit from Engine A BG. Implementations expecting halfword
    /// alignment read from `addr & !1`.
    pub fn read_engine_a_bg_u16(&self, addr: u32) -> u16 {
        let a = addr & !1;
        let lo = self.read_engine_a_bg(a) as u16;
        let hi = self.read_engine_a_bg(a + 1) as u16;
        lo | (hi << 8)
    }

    pub fn read_engine_b_bg_u16(&self, addr: u32) -> u16 {
        let a = addr & !1;
        let lo = self.read_engine_b_bg(a) as u16;
        let hi = self.read_engine_b_bg(a + 1) as u16;
        lo | (hi << 8)
    }

    pub fn read_engine_a_obj_u16(&self, addr: u32) -> u16 {
        let a = addr & !1;
        let lo = self.read_engine_a_obj(a) as u16;
        let hi = self.read_engine_a_obj(a + 1) as u16;
        lo | (hi << 8)
    }

    pub fn read_engine_b_obj_u16(&self, addr: u32) -> u16 {
        let a = addr & !1;
        let lo = self.read_engine_b_obj(a) as u16;
        let hi = self.read_engine_b_obj(a + 1) as u16;
        lo | (hi << 8)
    }

    /// Read one byte from the 512 KB texture image target. The 3D engine's
    /// texture unit calls this per-texel during rasterization. Banks A-D
    /// can each back a 128 KB slot (selected by their VRAMCNT offset).
    pub fn read_texture_image(&self, addr: u32) -> u8 {
        let addr = addr & 0x7_FFFF; // 19-bit address within texture-image space
        for bank in &self.banks {
            if let VramTarget::TextureImage { slot } = bank.target {
                let base = (slot as u32) * 0x2_0000; // 128 KB per slot
                let span = bank.id.size() as u32;
                if addr >= base && addr < base + span {
                    return bank.data[(addr - base) as usize];
                }
            }
        }
        0
    }

    /// Read one byte from the 128 KB texture palette target. Slots are
    /// 16 KB each; banks E (64 KB) covers slots 0..3, F/G (16 KB each)
    /// cover any single slot per their VRAMCNT offset.
    pub fn read_texture_palette(&self, addr: u32) -> u8 {
        let addr = addr & 0x1_FFFF; // 17-bit address within texture-palette space
        for bank in &self.banks {
            if let VramTarget::TexturePalette { slot } = bank.target {
                let base = (slot as u32) * 0x4000; // 16 KB per slot
                let span = bank.id.size() as u32;
                if addr >= base && addr < base + span {
                    return bank.data[(addr - base) as usize];
                }
            }
        }
        0
    }

    pub fn read_texture_palette_u16(&self, addr: u32) -> u16 {
        let a = addr & !1;
        let lo = self.read_texture_palette(a) as u16;
        let hi = self.read_texture_palette(a + 1) as u16;
        lo | (hi << 8)
    }
}

impl Default for VramRouter {
    fn default() -> Self { Self::new() }
}

/// Read from a bank if the ARM9 address `addr` (in 0x06xxxxxx) maps to it.
fn bank_read_arm9(bank: &VramBank, addr: u32) -> Option<u8> {
    let win = addr & 0x00FF_FFFF; // 16 MB window
    let span = bank.id.size() as u32;
    match bank.target {
        VramTarget::EngineABg { base } => {
            // 0x06000000-0x061FFFFF (2 MB region; 512 KB unique)
            let bg = win.wrapping_sub(0x000000) & 0x07_FFFF;
            if win < 0x20_0000 && bg >= base && bg < base + span {
                Some(bank.data[(bg - base) as usize])
            } else { None }
        }
        VramTarget::EngineAObj { base } => {
            // 0x06400000-0x0641FFFF (and mirrors up to 0x06600000)
            if (0x40_0000..0x60_0000).contains(&win) {
                let off = (win - 0x40_0000) & 0x03_FFFF;
                if off >= base && off < base + span {
                    Some(bank.data[(off - base) as usize])
                } else { None }
            } else { None }
        }
        VramTarget::EngineBBg { base } => {
            if (0x20_0000..0x40_0000).contains(&win) {
                let off = (win - 0x20_0000) & 0x01_FFFF;
                if off >= base && off < base + span {
                    Some(bank.data[(off - base) as usize])
                } else { None }
            } else { None }
        }
        VramTarget::EngineBObj { base } => {
            if (0x60_0000..0x80_0000).contains(&win) {
                let off = (win - 0x60_0000) & 0x01_FFFF;
                if off >= base && off < base + span {
                    Some(bank.data[(off - base) as usize])
                } else { None }
            } else { None }
        }
        VramTarget::Lcdc { lcdc_offset } => {
            // 0x06800000-0x068A3FFF: flat LCDC view (ARM9 only).
            if (0x80_0000..0x8A_4000).contains(&win) {
                let off = win - 0x80_0000;
                if off >= lcdc_offset && off < lcdc_offset + span {
                    Some(bank.data[(off - lcdc_offset) as usize])
                } else { None }
            } else { None }
        }
        _ => None,
    }
}

fn bank_write_arm9(bank: &mut VramBank, addr: u32, val: u8) {
    let win = addr & 0x00FF_FFFF;
    let span = bank.id.size() as u32;
    match bank.target {
        VramTarget::EngineABg { base } => {
            let bg = win & 0x07_FFFF;
            if win < 0x20_0000 && bg >= base && bg < base + span {
                bank.data[(bg - base) as usize] = val;
            }
        }
        VramTarget::EngineAObj { base } => {
            if (0x40_0000..0x60_0000).contains(&win) {
                let off = (win - 0x40_0000) & 0x03_FFFF;
                if off >= base && off < base + span {
                    bank.data[(off - base) as usize] = val;
                }
            }
        }
        VramTarget::EngineBBg { base } => {
            if (0x20_0000..0x40_0000).contains(&win) {
                let off = (win - 0x20_0000) & 0x01_FFFF;
                if off >= base && off < base + span {
                    bank.data[(off - base) as usize] = val;
                }
            }
        }
        VramTarget::EngineBObj { base } => {
            if (0x60_0000..0x80_0000).contains(&win) {
                let off = (win - 0x60_0000) & 0x01_FFFF;
                if off >= base && off < base + span {
                    bank.data[(off - base) as usize] = val;
                }
            }
        }
        VramTarget::Lcdc { lcdc_offset } => {
            if (0x80_0000..0x8A_4000).contains(&win) {
                let off = win - 0x80_0000;
                if off >= lcdc_offset && off < lcdc_offset + span {
                    bank.data[(off - lcdc_offset) as usize] = val;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_bank_a_lcdc() {
        let t = decode_target(BankId::A, 0x80); // enable + mst=0
        assert!(matches!(t, VramTarget::Lcdc { lcdc_offset: 0 }));
    }

    #[test]
    fn test_decode_bank_a_engine_bg_offset_2() {
        // enable + mst=1 + offset=2
        let t = decode_target(BankId::A, 0x80 | (2 << 3) | 1);
        assert!(matches!(t, VramTarget::EngineABg { base: 0x40000 }));
    }

    #[test]
    fn test_decode_bank_c_engine_b_bg() {
        let t = decode_target(BankId::C, 0x80 | 4);
        assert!(matches!(t, VramTarget::EngineBBg { base: 0 }));
    }

    #[test]
    fn test_decode_bank_d_engine_b_obj() {
        let t = decode_target(BankId::D, 0x80 | 4);
        assert!(matches!(t, VramTarget::EngineBObj { base: 0 }));
    }

    #[test]
    fn test_decode_bank_disabled_when_enable_off() {
        let t = decode_target(BankId::A, 0x01); // enable=0
        assert!(matches!(t, VramTarget::Disabled));
    }

    #[test]
    fn test_arm9_write_routes_to_engine_a_bg() {
        let mut r = VramRouter::new();
        // Bank A → Engine A BG at offset 0
        r.write_cnt(BankId::A, 0x80 | 1);
        // Write to 0x06000010 — should hit bank A at byte 0x10
        r.cpu_write_arm9(0x06000010, 0xAB);
        assert_eq!(r.cpu_read_arm9(0x06000010), 0xAB);
        // Read engine-side too
        assert_eq!(r.read_engine_a_bg(0x10), 0xAB);
    }

    #[test]
    fn test_engine_b_bg_via_bank_c() {
        let mut r = VramRouter::new();
        r.write_cnt(BankId::C, 0x80 | 4); // mst=4 → Engine B BG
        r.cpu_write_arm9(0x06200020, 0x55);
        assert_eq!(r.read_engine_b_bg(0x20), 0x55);
    }

    #[test]
    fn test_engine_b_obj_via_bank_d() {
        let mut r = VramRouter::new();
        r.write_cnt(BankId::D, 0x80 | 4);
        r.cpu_write_arm9(0x06600040, 0xF0);
        assert_eq!(r.read_engine_b_obj(0x40), 0xF0);
    }

    #[test]
    fn test_arm7_routing_on_bank_c() {
        let mut r = VramRouter::new();
        // Bank C → ARM7 mst=2, offset=0
        r.write_cnt(BankId::C, 0x80 | 2);
        r.cpu_write_arm7(0x06000010, 0x77);
        assert_eq!(r.cpu_read_arm7(0x06000010), 0x77);
    }

    #[test]
    fn test_overlapping_writes_go_to_all_matching_banks() {
        let mut r = VramRouter::new();
        // Bank A and B both route to Engine A BG offset 0 (mst=1, offset=0)
        r.write_cnt(BankId::A, 0x80 | 1);
        r.write_cnt(BankId::B, 0x80 | 1);
        r.cpu_write_arm9(0x06000000, 0x42);
        // Both banks should hold the byte
        assert_eq!(r.banks[BankId::A as usize].data[0], 0x42);
        assert_eq!(r.banks[BankId::B as usize].data[0], 0x42);
    }

    #[test]
    fn test_disabled_bank_does_not_respond() {
        let mut r = VramRouter::new();
        r.write_cnt(BankId::A, 0x00); // disable
        r.cpu_write_arm9(0x06000000, 0x99);
        assert_eq!(r.cpu_read_arm9(0x06000000), 0);
    }

    #[test]
    fn test_lcdc_window_access() {
        let mut r = VramRouter::new();
        r.write_cnt(BankId::A, 0x80); // mst=0 → LCDC
        r.cpu_write_arm9(0x06800010, 0x33);
        assert_eq!(r.cpu_read_arm9(0x06800010), 0x33);
    }
}
