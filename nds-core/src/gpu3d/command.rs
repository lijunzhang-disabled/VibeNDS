//! GX command set — opcode constants + parameter-count table.
//!
//! ARM9 submits commands two ways:
//!
//! - **Packed format** via `GXFIFO` at `0x04000400`. One 32-bit word
//!   packs up to 4 command IDs (one byte each); successive words supply
//!   parameters in declaration order.
//! - **Direct ports** at `0x04000440..0x040005FF`. Each command has a
//!   dedicated address; writing to it is equivalent to submitting that
//!   command with one parameter (some commands take 0 params; you still
//!   write the address with a dummy word).
//!
//! This module defines the command ID enum and the parameter-count
//! lookup. Decoding & dispatch live in `fifo.rs` / engine glue.
//!
//! Reference: GBATEK §"DS 3D Geometry Commands".

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GxCmd {
    // ─── Matrix commands (0x10-0x1B) ──────────────────────────
    MtxMode      = 0x10, // 1 param
    MtxPush      = 0x11, // 0 params
    MtxPop       = 0x12, // 1
    MtxStore     = 0x13, // 1
    MtxRestore   = 0x14, // 1
    MtxIdentity  = 0x15, // 0
    MtxLoad4x4   = 0x16, // 16
    MtxLoad4x3   = 0x17, // 12
    MtxMult4x4   = 0x18, // 16
    MtxMult4x3   = 0x19, // 12
    MtxMult3x3   = 0x1A, //  9
    MtxScale     = 0x1B, //  3
    MtxTrans     = 0x1C, //  3

    // ─── Vertex attribute commands (0x20-0x2C) ────────────────
    Color        = 0x20, // 1
    Normal       = 0x21, // 1
    TexCoord     = 0x22, // 1
    Vtx16        = 0x23, // 2 (two 16-bit halves of x/y/z; second half has w-pad)
    Vtx10        = 0x24, // 1
    VtxXY        = 0x25, // 1
    VtxXZ        = 0x26, // 1
    VtxYZ        = 0x27, // 1
    VtxDiff      = 0x28, // 1
    PolygonAttr  = 0x29, // 1
    TexImageParm = 0x2A, // 1
    PltBase      = 0x2B, // 1

    // ─── Lighting / material commands (0x30-0x34) ────────────
    DifAmb       = 0x30, // 1
    SpeEmi       = 0x31, // 1
    LightVector  = 0x32, // 1
    LightColor   = 0x33, // 1
    Shininess    = 0x34, // 32 (specular LUT)

    // ─── Geometry control (0x40-0x60) ────────────────────────
    BeginVtxs    = 0x40, // 1
    EndVtxs      = 0x41, // 0 (no-op on real hw)
    SwapBuffers  = 0x50, // 1
    Viewport     = 0x60, // 1

    // ─── Test commands (0x70-0x72) ────────────────────────────
    BoxTest      = 0x70, // 3
    PosTest      = 0x71, // 2
    VecTest      = 0x72, // 1
}

impl GxCmd {
    pub fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0x10 => GxCmd::MtxMode,      0x11 => GxCmd::MtxPush,
            0x12 => GxCmd::MtxPop,       0x13 => GxCmd::MtxStore,
            0x14 => GxCmd::MtxRestore,   0x15 => GxCmd::MtxIdentity,
            0x16 => GxCmd::MtxLoad4x4,   0x17 => GxCmd::MtxLoad4x3,
            0x18 => GxCmd::MtxMult4x4,   0x19 => GxCmd::MtxMult4x3,
            0x1A => GxCmd::MtxMult3x3,   0x1B => GxCmd::MtxScale,
            0x1C => GxCmd::MtxTrans,

            0x20 => GxCmd::Color,        0x21 => GxCmd::Normal,
            0x22 => GxCmd::TexCoord,     0x23 => GxCmd::Vtx16,
            0x24 => GxCmd::Vtx10,        0x25 => GxCmd::VtxXY,
            0x26 => GxCmd::VtxXZ,        0x27 => GxCmd::VtxYZ,
            0x28 => GxCmd::VtxDiff,      0x29 => GxCmd::PolygonAttr,
            0x2A => GxCmd::TexImageParm, 0x2B => GxCmd::PltBase,

            0x30 => GxCmd::DifAmb,       0x31 => GxCmd::SpeEmi,
            0x32 => GxCmd::LightVector,  0x33 => GxCmd::LightColor,
            0x34 => GxCmd::Shininess,

            0x40 => GxCmd::BeginVtxs,    0x41 => GxCmd::EndVtxs,
            0x50 => GxCmd::SwapBuffers,  0x60 => GxCmd::Viewport,

            0x70 => GxCmd::BoxTest,      0x71 => GxCmd::PosTest,
            0x72 => GxCmd::VecTest,

            _ => return None,
        })
    }

    /// How many 32-bit parameter words follow this opcode.
    pub fn param_count(self) -> u8 {
        match self {
            GxCmd::MtxPush | GxCmd::MtxIdentity | GxCmd::EndVtxs => 0,
            GxCmd::MtxMode | GxCmd::MtxPop | GxCmd::MtxStore | GxCmd::MtxRestore
                | GxCmd::Color | GxCmd::Normal | GxCmd::TexCoord
                | GxCmd::Vtx10 | GxCmd::VtxXY | GxCmd::VtxXZ | GxCmd::VtxYZ | GxCmd::VtxDiff
                | GxCmd::PolygonAttr | GxCmd::TexImageParm | GxCmd::PltBase
                | GxCmd::DifAmb | GxCmd::SpeEmi | GxCmd::LightVector | GxCmd::LightColor
                | GxCmd::BeginVtxs | GxCmd::SwapBuffers | GxCmd::Viewport
                | GxCmd::VecTest => 1,
            GxCmd::Vtx16 | GxCmd::PosTest => 2,
            GxCmd::MtxScale | GxCmd::MtxTrans | GxCmd::BoxTest => 3,
            GxCmd::MtxMult3x3 => 9,
            GxCmd::MtxLoad4x3 | GxCmd::MtxMult4x3 => 12,
            GxCmd::MtxLoad4x4 | GxCmd::MtxMult4x4 => 16,
            GxCmd::Shininess => 32,
        }
    }

    /// Direct-port address offset (relative to `0x04000400`). The direct
    /// ports start at `0x04000440` and step by 4 per command; the offset
    /// from `0x04000400` is `0x40 + (cmd - 0x10) * 4`. We just return the
    /// absolute address for clarity.
    pub fn direct_port_addr(self) -> u32 {
        0x0400_0400 + 0x40 + (self as u32 - 0x10) * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_param_counts() {
        assert_eq!(GxCmd::MtxIdentity.param_count(), 0);
        assert_eq!(GxCmd::MtxMode.param_count(), 1);
        assert_eq!(GxCmd::Vtx16.param_count(), 2);
        assert_eq!(GxCmd::MtxScale.param_count(), 3);
        assert_eq!(GxCmd::MtxMult3x3.param_count(), 9);
        assert_eq!(GxCmd::MtxLoad4x3.param_count(), 12);
        assert_eq!(GxCmd::MtxLoad4x4.param_count(), 16);
        assert_eq!(GxCmd::Shininess.param_count(), 32);
    }

    #[test]
    fn test_from_u8_round_trip() {
        for b in [0x10u8, 0x16, 0x20, 0x23, 0x29, 0x32, 0x40, 0x50, 0x60, 0x70] {
            assert!(GxCmd::from_u8(b).is_some(), "0x{:02X} should decode", b);
        }
        assert!(GxCmd::from_u8(0xFF).is_none());
    }

    #[test]
    fn test_direct_port_addr_for_mtx_mode() {
        // MTX_MODE (0x10) is at offset 0x40 from 0x04000400 = 0x04000440.
        assert_eq!(GxCmd::MtxMode.direct_port_addr(), 0x0400_0440);
    }

    #[test]
    fn test_direct_port_addr_for_vtx16() {
        // VTX_16 = 0x23 → offset = 0x40 + (0x23 - 0x10) * 4 = 0x40 + 0x4C = 0x8C.
        // Absolute: 0x04000400 + 0x8C = 0x0400048C.
        assert_eq!(GxCmd::Vtx16.direct_port_addr(), 0x0400_048C);
    }
}
