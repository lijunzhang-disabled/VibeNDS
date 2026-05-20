//! State that both CPUs see through their respective buses.
//!
//! Phase 1 keeps this minimal: 4 MB Main RAM, 32 KB shared WRAM, the
//! `WRAMCNT` selector, and an `ipc` placeholder. Palette / VRAM / OAM /
//! interrupt registers etc. are added in later phases.

use serde::{Deserialize, Serialize};

use crate::interrupt::InterruptController;
use crate::vram::VramRouter;
use crate::gpu2d::{Engine2d, Which as EngineWhich};
use crate::ipc::Ipc;
use crate::timer::Timers;
use crate::dma::DmaController;
use crate::spi::SpiBus;
use crate::cart::AuxSpi;
use crate::gpu3d::Engine3d;
use crate::audio::Audio;

pub const MAIN_RAM_SIZE: usize = 4 * 1024 * 1024;
pub const SHARED_WRAM_SIZE: usize = 32 * 1024;
pub const PALETTE_SIZE: usize = 2 * 1024; // 1 KB Engine A + 1 KB Engine B
pub const OAM_SIZE: usize = 2 * 1024;     // 1 KB Engine A + 1 KB Engine B

/// Helper: store the 4 MB main RAM on the heap. We keep it as a `Vec<u8>`
/// rather than a fixed-size array so `bincode::serialize` doesn't need to
/// stack-allocate the whole 4 MB at deserialize time.
fn boxed_zeroed(len: usize) -> Vec<u8> {
    vec![0u8; len]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedState {
    /// 4 MB Main RAM, accessible to both CPUs.
    #[serde(with = "serde_bytes_vec")]
    pub main_ram: Vec<u8>,

    /// 32 KB shared WRAM. The `wramcnt` field decides who sees what half.
    #[serde(with = "serde_bytes_vec")]
    pub shared_wram: Vec<u8>,

    /// `WRAMCNT` register at 0x04000247. Only the low 2 bits matter:
    ///   0 → all 32 KB visible to ARM9, none to ARM7
    ///   1 → upper 16 KB to ARM9, lower 16 KB to ARM7
    ///   2 → lower 16 KB to ARM9, upper 16 KB to ARM7
    ///   3 → none to ARM9, all 32 KB to ARM7
    pub wramcnt: u8,

    /// ARM9 interrupt controller (`IE`/`IF`/`IME` at the ARM9 view of 0x04000208/210/214).
    pub irq9: InterruptController,
    /// ARM7 interrupt controller — same register addresses, distinct state.
    pub irq7: InterruptController,

    /// Combined VCOUNT/DISPSTAT mirror. Both CPUs see the same VCOUNT
    /// (0x04000006), but DISPSTAT (0x04000004) has per-CPU bits — we keep
    /// two separate copies and gate IRQ enables off each CPU's own.
    pub vcount: u16,
    pub dispstat9: u16,
    pub dispstat7: u16,

    /// `KEYINPUT` (0x04000130). Active-low: bit n = 0 means button n is held.
    pub keyinput: u16,
    /// Per-CPU `KEYCNT` (0x04000132). Bit 14 = enable, bit 15 = AND mode
    /// (require all selected keys), bits 0-9 = key mask. Each CPU has its
    /// own.
    pub keycnt9: u16,
    pub keycnt7: u16,
    /// `EXTKEYIN` (0x04000136, ARM7 only). Active-low: X, Y, debug, lid, pen-down.
    pub extkeyin: u16,

    /// Palette RAM (1 KB Engine A at 0x05000000, 1 KB Engine B at 0x05000400).
    #[serde(with = "serde_bytes_vec")]
    pub palette: Vec<u8>,

    /// OAM (1 KB Engine A at 0x07000000, 1 KB Engine B at 0x07000400).
    #[serde(with = "serde_bytes_vec")]
    pub oam: Vec<u8>,

    /// VRAM banks A-I and the routing tables driven by VRAMCNT_A..I.
    pub vram: VramRouter,

    /// `POWCNT1` (0x04000304, ARM9). Bit 15 = swap LCD assignment (0 =
    /// Engine A on top, B on bottom; 1 = swapped). Other bits gate engine
    /// power; we mostly ignore them in Phase 3.
    pub powcnt1: u16,

    /// 2D engines A (full feature set) and B (subset).
    pub engine_a: Engine2d,
    pub engine_b: Engine2d,

    /// Inter-processor communication (SYNC + FIFO).
    pub ipc: Ipc,

    /// Per-CPU timer banks (4 timers each).
    pub timers9: Timers,
    pub timers7: Timers,

    /// Per-CPU DMA controllers (4 channels each).
    pub dma9: DmaController,
    pub dma7: DmaController,

    /// SPI bus (ARM7-only) wrapping firmware / TSC / PMIC devices.
    pub spi: SpiBus,

    /// AUXSPI bus (cart-side backup). Routed through slot-1 control regs
    /// at `0x040001A0..0x040001A3`. ARM7 by default, but `EXMEMCNT` bit
    /// 11 can flip slot-1 over to ARM9; Phase 5 keeps it ARM7-only.
    pub auxspi: AuxSpi,

    /// 3D engine — matrix stacks + vertex pipeline + lighting + clipper +
    /// viewport transform + GXFIFO. ARM9-only.
    pub gpu3d: Engine3d,

    /// Audio — 16 channels + mixer. ARM7-only.
    pub audio: Audio,
}

impl SharedState {
    pub fn new() -> Self {
        SharedState {
            main_ram: boxed_zeroed(MAIN_RAM_SIZE),
            shared_wram: boxed_zeroed(SHARED_WRAM_SIZE),
            wramcnt: 0,
            irq9: InterruptController::new(),
            irq7: InterruptController::new(),
            vcount: 0,
            dispstat9: 0,
            dispstat7: 0,
            keyinput: 0x03FF,
            keycnt9: 0,
            keycnt7: 0,
            extkeyin: 0x007F,
            palette: boxed_zeroed(PALETTE_SIZE),
            oam: boxed_zeroed(OAM_SIZE),
            vram: VramRouter::new(),
            powcnt1: 0x0001,
            engine_a: Engine2d::new(EngineWhich::A),
            engine_b: Engine2d::new(EngineWhich::B),
            ipc: Ipc::new(),
            timers9: Timers::new(),
            timers7: Timers::new(),
            dma9: DmaController::new(true),
            dma7: DmaController::new(false),
            spi: SpiBus::new(),
            auxspi: AuxSpi::new(),
            gpu3d: Engine3d::new(),
            audio: Audio::new(),
        }
    }

    /// Compute the (slice, offset_within_slice) view of shared WRAM that
    /// the ARM9 currently sees, or `None` if WRAMCNT excludes the ARM9.
    /// The returned slice is the full backing region — callers index it
    /// modulo the slice length (the addresses repeat across the 0x03000000-
    /// 0x037FFFFF window per GBATEK).
    pub fn arm9_wram_view(&self) -> Option<(&[u8], usize)> {
        match self.wramcnt & 0x3 {
            0 => Some((&self.shared_wram[..], 0)),
            1 => Some((&self.shared_wram[0x4000..0x8000], 0)), // upper 16K
            2 => Some((&self.shared_wram[0..0x4000], 0)),      // lower 16K
            3 => None,
            _ => unreachable!(),
        }
    }

    pub fn arm9_wram_view_mut(&mut self) -> Option<&mut [u8]> {
        match self.wramcnt & 0x3 {
            0 => Some(&mut self.shared_wram[..]),
            1 => Some(&mut self.shared_wram[0x4000..0x8000]),
            2 => Some(&mut self.shared_wram[0..0x4000]),
            3 => None,
            _ => unreachable!(),
        }
    }

    pub fn arm7_wram_view(&self) -> Option<&[u8]> {
        match self.wramcnt & 0x3 {
            0 => None,
            1 => Some(&self.shared_wram[0..0x4000]),
            2 => Some(&self.shared_wram[0x4000..0x8000]),
            3 => Some(&self.shared_wram[..]),
            _ => unreachable!(),
        }
    }

    pub fn arm7_wram_view_mut(&mut self) -> Option<&mut [u8]> {
        match self.wramcnt & 0x3 {
            0 => None,
            1 => Some(&mut self.shared_wram[0..0x4000]),
            2 => Some(&mut self.shared_wram[0x4000..0x8000]),
            3 => Some(&mut self.shared_wram[..]),
            _ => unreachable!(),
        }
    }
}

impl Default for SharedState {
    fn default() -> Self { Self::new() }
}

/// `Vec<u16>` serialized as a flat byte stream (length × 2 bytes,
/// little-endian). Same fast-path idea as `serde_bytes_vec`, just for
/// 16-bit elements. Used for the 3D framebuffer.
pub(crate) mod serde_bytes_vec_u16 {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Vec<u16>, s: S) -> Result<S::Ok, S::Error> {
        let mut bytes = Vec::with_capacity(v.len() * 2);
        for &w in v {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        s.serialize_bytes(&bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u16>, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Vec<u16>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("byte buffer of u16 LE")
            }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Vec<u16>, E> {
                let mut out = Vec::with_capacity(v.len() / 2);
                for chunk in v.chunks_exact(2) {
                    out.push(u16::from_le_bytes([chunk[0], chunk[1]]));
                }
                Ok(out)
            }
            fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Vec<u16>, E> {
                self.visit_bytes(&v)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u16>, A::Error> {
                let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(w) = seq.next_element::<u16>()? { out.push(w); }
                Ok(out)
            }
        }
        d.deserialize_bytes(Visitor)
    }
}

/// `Vec<i32>` serialized as a flat byte stream (length × 4 bytes,
/// little-endian). Used for the 3D depth buffer.
pub(crate) mod serde_bytes_vec_u32_i {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Vec<i32>, s: S) -> Result<S::Ok, S::Error> {
        let mut bytes = Vec::with_capacity(v.len() * 4);
        for &w in v {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        s.serialize_bytes(&bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<i32>, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Vec<i32>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("byte buffer of i32 LE")
            }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Vec<i32>, E> {
                let mut out = Vec::with_capacity(v.len() / 4);
                for chunk in v.chunks_exact(4) {
                    out.push(i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
                Ok(out)
            }
            fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Vec<i32>, E> {
                self.visit_bytes(&v)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<i32>, A::Error> {
                let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(w) = seq.next_element::<i32>()? { out.push(w); }
                Ok(out)
            }
        }
        d.deserialize_bytes(Visitor)
    }
}

pub(crate) mod serde_bytes_vec {
    //! Serialize `Vec<u8>` as a byte string rather than a sequence of u8s.
    //! Without this, bincode encodes 4 MB as 4 M serde elements, which
    //! works but is slower than the byte-buffer fast path.
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("byte buffer")
            }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
                Ok(v.to_vec())
            }
            fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
                Ok(v)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u8>, A::Error> {
                let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(b) = seq.next_element::<u8>()? {
                    out.push(b);
                }
                Ok(out)
            }
        }
        d.deserialize_bytes(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wramcnt_mode_0_arm9_full() {
        let s = SharedState::new();
        assert!(s.arm9_wram_view().is_some());
        assert_eq!(s.arm9_wram_view().unwrap().0.len(), 32 * 1024);
        assert!(s.arm7_wram_view().is_none());
    }

    #[test]
    fn test_wramcnt_mode_1_split_upper_to_arm9() {
        let mut s = SharedState::new();
        s.wramcnt = 1;
        assert_eq!(s.arm9_wram_view().unwrap().0.len(), 0x4000);
        assert_eq!(s.arm7_wram_view().unwrap().len(), 0x4000);
        // Distinct halves
        s.shared_wram[0] = 0xAA;       // lower
        s.shared_wram[0x4000] = 0xBB;  // upper
        assert_eq!(s.arm7_wram_view().unwrap()[0], 0xAA);
        assert_eq!(s.arm9_wram_view().unwrap().0[0], 0xBB);
    }

    #[test]
    fn test_wramcnt_mode_2_split_lower_to_arm9() {
        let mut s = SharedState::new();
        s.wramcnt = 2;
        s.shared_wram[0] = 0xCC;
        s.shared_wram[0x4000] = 0xDD;
        assert_eq!(s.arm9_wram_view().unwrap().0[0], 0xCC);
        assert_eq!(s.arm7_wram_view().unwrap()[0], 0xDD);
    }

    #[test]
    fn test_wramcnt_mode_3_arm7_full() {
        let mut s = SharedState::new();
        s.wramcnt = 3;
        assert!(s.arm9_wram_view().is_none());
        assert_eq!(s.arm7_wram_view().unwrap().len(), 32 * 1024);
    }
}
