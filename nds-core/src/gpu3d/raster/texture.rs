//! Texture fetch: decode `TEXIMAGE_PARAM` + per-texel sampling.
//!
//! ```text
//! TEXIMAGE_PARAM bits (per GBATEK):
//!   [15: 0]  VRAM offset in 8-byte units
//!   [17:16]  repeat S / T (bit 16 = S, bit 17 = T)
//!   [19:18]  flip S / T (bit 18 = S, bit 19 = T)
//!   [22:20]  width  = 8 << bits  (8 .. 1024 pixels)
//!   [25:23]  height = 8 << bits
//!   [28:26]  format (0..7)
//!   [29]     color-0 transparent (4/16/256-color formats)
//!   [31:30]  texcoord transform mode
//! ```
//!
//! Eight formats:
//!
//! | Code | Name | Bytes/texel | Notes |
//! |---|---|---|---|
//! | 0 | None | 0 | Texture disabled; use vertex color only |
//! | 1 | A3I5 | 1 | top 3 bits = alpha (0..7), low 5 bits = 5-bit palette idx |
//! | 2 | 4-color | 0.25 | 2 bits per texel; palette has 4 colors |
//! | 3 | 16-color | 0.5 | 4 bits per texel; palette has 16 colors |
//! | 4 | 256-color | 1 | 8 bits per texel; palette has 256 colors |
//! | 5 | 4×4 block | 0.25 (avg) | Block compression |
//! | 6 | A5I3 | 1 | top 5 bits = alpha, low 3 bits = 3-bit palette idx |
//! | 7 | Direct color | 2 | BGR555 + alpha bit, no palette |
//!
//! Reads the texture image from the VRAM router's "texture image" target
//! and the texture palette from its "texture palette" target.

use crate::vram::VramRouter;

/// Decoded view of `TEXIMAGE_PARAM`.
#[derive(Debug, Clone, Copy)]
pub struct TexParams {
    pub vram_offset: u32,
    pub width: u32,
    pub height: u32,
    pub format: u8,
    pub color0_transparent: bool,
    pub repeat_s: bool,
    pub repeat_t: bool,
    pub flip_s: bool,
    pub flip_t: bool,
}

impl TexParams {
    pub fn from_register(param: u32) -> Self {
        TexParams {
            vram_offset: (param & 0xFFFF) << 3, // 8-byte units
            repeat_s: param & (1 << 16) != 0,
            repeat_t: param & (1 << 17) != 0,
            flip_s: param & (1 << 18) != 0,
            flip_t: param & (1 << 19) != 0,
            width: 8u32 << ((param >> 20) & 0x7),
            height: 8u32 << ((param >> 23) & 0x7),
            format: ((param >> 26) & 0x7) as u8,
            color0_transparent: param & (1 << 29) != 0,
        }
    }

    /// Whether this polygon has *no* texture (format 0 = none).
    pub fn is_disabled(self) -> bool {
        self.format == 0
    }
}

/// Wrap or clamp a texture coordinate to the texture's pixel range.
#[inline]
fn wrap_coord(coord: i32, size: u32, repeat: bool, flip: bool) -> u32 {
    let s = size as i32;
    if !repeat {
        // Clamp.
        return coord.clamp(0, s - 1) as u32;
    }
    // Repeat with optional flip.
    let mut c = coord.rem_euclid(s);
    // Flip: every other repetition is mirrored.
    if flip && (coord.div_euclid(s) & 1 != 0) {
        c = s - 1 - c;
    }
    c as u32
}

/// One fetched texel: BGR555 color + alpha factor in 0..31. Caller blends
/// against the per-vertex color per `POLYGON_ATTR.mode`.
#[derive(Debug, Clone, Copy)]
pub struct Texel {
    pub color: u16,
    /// 0 = fully transparent, 31 = fully opaque.
    pub alpha: u8,
}

/// Sample one texel at integer texture-space `(u, v)`. Caller is
/// responsible for perspective-correcting `u` and `v` before calling.
pub fn sample(tp: TexParams, u: i32, v: i32, palette_base: u16, vram: &VramRouter) -> Texel {
    if tp.is_disabled() {
        // Should never be called for format=0; return transparent for safety.
        return Texel { color: 0, alpha: 0 };
    }

    let su = wrap_coord(u, tp.width, tp.repeat_s, tp.flip_s);
    let tv = wrap_coord(v, tp.height, tp.repeat_t, tp.flip_t);
    let pixel_idx = tv * tp.width + su;

    match tp.format {
        1 => sample_a3i5(tp, pixel_idx, palette_base, vram),
        2 => sample_4color(tp, pixel_idx, palette_base, vram),
        3 => sample_16color(tp, pixel_idx, palette_base, vram),
        4 => sample_256color(tp, pixel_idx, palette_base, vram),
        5 => sample_block_compressed(tp, su, tv, palette_base, vram),
        6 => sample_a5i3(tp, pixel_idx, palette_base, vram),
        7 => sample_direct(tp, pixel_idx, vram),
        _ => Texel { color: 0, alpha: 0 },
    }
}

fn read_pal_entry(format: u8, palette_base: u16, idx: u32, vram: &VramRouter) -> u16 {
    let base = palette_base as u32;
    let addr = match format {
        // ndsdoc: 4-color palettes use PLTT_BASE in 8-byte units.
        2 => (base << 3) + idx * 2,
        // Other palette formats use 16-byte units.
        _ => (base << 4) + idx * 2,
    };
    vram.read_texture_palette_u16(addr)
}

fn read_image_byte(tp: TexParams, byte_off: u32, vram: &VramRouter) -> u8 {
    vram.read_texture_image(tp.vram_offset + byte_off)
}

fn sample_a3i5(tp: TexParams, idx: u32, palette_base: u16, vram: &VramRouter) -> Texel {
    let byte = read_image_byte(tp, idx, vram);
    let alpha3 = (byte >> 5) & 0x7;
    let palidx = (byte & 0x1F) as u32;
    let color = read_pal_entry(tp.format, palette_base, palidx, vram);
    // 3-bit alpha → 5-bit: (a3 * 4 + a3 / 2). Standard expansion.
    let alpha = (alpha3 << 2) | (alpha3 >> 1);
    Texel {
        color: color & 0x7FFF,
        alpha,
    }
}

fn sample_a5i3(tp: TexParams, idx: u32, palette_base: u16, vram: &VramRouter) -> Texel {
    let byte = read_image_byte(tp, idx, vram);
    let alpha = (byte >> 3) & 0x1F;
    let palidx = (byte & 0x7) as u32;
    let color = read_pal_entry(tp.format, palette_base, palidx, vram);
    Texel {
        color: color & 0x7FFF,
        alpha,
    }
}

fn sample_4color(tp: TexParams, idx: u32, palette_base: u16, vram: &VramRouter) -> Texel {
    let byte = read_image_byte(tp, idx >> 2, vram);
    let bit_off = (idx & 0x3) * 2;
    let palidx = ((byte >> bit_off) & 0x3) as u32;
    palette_sample(tp, palidx, palette_base, vram)
}

fn sample_16color(tp: TexParams, idx: u32, palette_base: u16, vram: &VramRouter) -> Texel {
    let byte = read_image_byte(tp, idx >> 1, vram);
    let palidx = if idx & 1 != 0 {
        (byte >> 4) & 0xF
    } else {
        byte & 0xF
    } as u32;
    palette_sample(tp, palidx, palette_base, vram)
}

fn sample_256color(tp: TexParams, idx: u32, palette_base: u16, vram: &VramRouter) -> Texel {
    let palidx = read_image_byte(tp, idx, vram) as u32;
    palette_sample(tp, palidx, palette_base, vram)
}

fn palette_sample(tp: TexParams, palidx: u32, palette_base: u16, vram: &VramRouter) -> Texel {
    let alpha = if palidx == 0 && tp.color0_transparent {
        0
    } else {
        31
    };
    let color = if alpha == 0 {
        0
    } else {
        read_pal_entry(tp.format, palette_base, palidx, vram) & 0x7FFF
    };
    Texel { color, alpha }
}

fn sample_direct(tp: TexParams, idx: u32, vram: &VramRouter) -> Texel {
    // 16-bit per texel: BGR555 in low 15 bits, alpha bit at 15.
    let addr = tp.vram_offset + idx * 2;
    let lo = vram.read_texture_image(addr) as u16;
    let hi = vram.read_texture_image(addr + 1) as u16;
    let v = lo | (hi << 8);
    let alpha = if v & 0x8000 != 0 { 31 } else { 0 };
    Texel {
        color: v & 0x7FFF,
        alpha,
    }
}

fn sample_block_compressed(
    tp: TexParams,
    u: u32,
    v: u32,
    palette_base: u16,
    vram: &VramRouter,
) -> Texel {
    let blocks_per_row = (tp.width / 4).max(1);
    let block_x = u / 4;
    let block_y = v / 4;
    let block_idx = block_y * blocks_per_row + block_x;
    let block_addr = tp.vram_offset + block_idx * 4;

    let texel_bits = read_image_byte(tp, block_idx * 4, vram) as u32
        | ((read_image_byte(tp, block_idx * 4 + 1, vram) as u32) << 8)
        | ((read_image_byte(tp, block_idx * 4 + 2, vram) as u32) << 16)
        | ((read_image_byte(tp, block_idx * 4 + 3, vram) as u32) << 24);
    let local = (v & 3) * 4 + (u & 3);
    let idx = (texel_bits >> (local * 2)) & 0x3;

    let Some(param_addr) = compressed_palette_param_addr(block_addr) else {
        return Texel { color: 0, alpha: 0 };
    };
    let param = vram.read_texture_image(param_addr) as u16
        | ((vram.read_texture_image(param_addr + 1) as u16) << 8);
    let pal_off = (param & 0x3FFF) as u32;
    let mode = (param >> 14) as u8;
    let pal_addr = ((palette_base as u32) << 4) + pal_off * 4;

    let c0 = vram.read_texture_palette_u16(pal_addr) & 0x7FFF;
    let c1 = vram.read_texture_palette_u16(pal_addr + 2) & 0x7FFF;
    let c2 = vram.read_texture_palette_u16(pal_addr + 4) & 0x7FFF;
    let c3 = vram.read_texture_palette_u16(pal_addr + 6) & 0x7FFF;

    match (mode, idx) {
        (0 | 1, 3) => Texel { color: 0, alpha: 0 },
        (0 | 2, 0) | (1 | 3, 0) => Texel {
            color: c0,
            alpha: 31,
        },
        (0 | 2, 1) | (1 | 3, 1) => Texel {
            color: c1,
            alpha: 31,
        },
        (0 | 2, 2) => Texel {
            color: c2,
            alpha: 31,
        },
        (2, 3) => Texel {
            color: c3,
            alpha: 31,
        },
        (1, 2) => Texel {
            color: average_color(c0, c1, 1, 1),
            alpha: 31,
        },
        (3, 2) => Texel {
            color: average_color(c0, c1, 5, 3),
            alpha: 31,
        },
        (3, 3) => Texel {
            color: average_color(c0, c1, 3, 5),
            alpha: 31,
        },
        _ => Texel { color: 0, alpha: 0 },
    }
}

fn compressed_palette_param_addr(block_addr: u32) -> Option<u32> {
    let rel = block_addr & 0x1_FFFF;
    match block_addr >> 17 {
        0 => Some(0x2_0000 + rel / 2),
        2 => Some(0x2_0000 + 0x1_0000 + rel / 2),
        _ => None,
    }
}

fn average_color(a: u16, b: u16, wa: u32, wb: u32) -> u16 {
    let denom = wa + wb;
    let blend = |shift: u32| -> u16 {
        let ca = ((a >> shift) & 0x1F) as u32;
        let cb = ((b >> shift) & 0x1F) as u32;
        ((ca * wa + cb * wb) / denom).min(31) as u16
    };
    blend(0) | (blend(5) << 5) | (blend(10) << 10)
}

/// Combine a per-vertex (interpolated) color with a fetched texel per
/// the polygon's blend mode (`POLYGON_ATTR.mode`, bits 4-5).
///
/// | Mode | Name | Combine rule |
/// |---|---|---|
/// | 0 | Modulate | result = expanded 6-bit texel × vertex |
/// | 1 | Decal | result = expanded 6-bit α·texel + (1-α)·vertex |
/// | 2 | Toon / highlight | rasterizer handles toon table lookup |
/// | 3 | Shadow | special — Phase 9 |
pub fn combine_with_vertex(texel: Texel, vertex_color: u16, mode: u8) -> (u16, u8) {
    let t_r = (texel.color & 0x1F) as u32;
    let t_g = ((texel.color >> 5) & 0x1F) as u32;
    let t_b = ((texel.color >> 10) & 0x1F) as u32;
    let v_r = (vertex_color & 0x1F) as u32;
    let v_g = ((vertex_color >> 5) & 0x1F) as u32;
    let v_b = ((vertex_color >> 10) & 0x1F) as u32;

    match mode {
        0 | 2 => {
            if texel.alpha == 0 {
                return (0, 0);
            }
            let r = modulate_channel(t_r, v_r);
            let g = modulate_channel(t_g, v_g);
            let b = modulate_channel(t_b, v_b);
            (r | (g << 5) | (b << 10), texel.alpha)
        }
        1 => {
            // Decal keeps polygon alpha; texel alpha only controls color mix.
            if texel.alpha == 0 {
                return (vertex_color & 0x7FFF, 31);
            }
            if texel.alpha == 31 {
                return (texel.color & 0x7FFF, 31);
            }
            let r = decal_channel(t_r, v_r, texel.alpha);
            let g = decal_channel(t_g, v_g, texel.alpha);
            let b = decal_channel(t_b, v_b, texel.alpha);
            (r | (g << 5) | (b << 10), 31)
        }
        _ => (vertex_color, 31), // shadow / unknown
    }
}

#[inline]
fn expand_5_to_6(v: u32) -> u32 {
    if v == 0 {
        0
    } else {
        v * 2 + 1
    }
}

#[inline]
fn shrink_6_to_5(v: u32) -> u16 {
    (v >> 1).min(31) as u16
}

#[inline]
fn modulate_channel(texel: u32, vertex: u32) -> u16 {
    let t = expand_5_to_6(texel);
    let v = expand_5_to_6(vertex);
    shrink_6_to_5((((t + 1) * (v + 1)).saturating_sub(1)) / 64)
}

#[inline]
fn decal_channel(texel: u32, vertex: u32, alpha: u8) -> u16 {
    let t = expand_5_to_6(texel);
    let v = expand_5_to_6(vertex);
    let a = expand_5_to_6(alpha as u32);
    shrink_6_to_5((t * a + v * (63 - a)) / 64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vram::{BankId, VramRouter};

    fn vram_with_palette_at_0(entries: &[u16]) -> VramRouter {
        let mut v = VramRouter::new();
        // Bank E → texture palette slot 0..3
        v.write_cnt(BankId::E, 0x80 | 3);
        // Bank E sits at texture-palette addr 0..0x10000.
        // Each palette entry is 2 bytes (BGR555).
        for (i, &c) in entries.iter().enumerate() {
            let b = &mut v.banks[BankId::E as usize].data;
            b[i * 2] = c as u8;
            b[i * 2 + 1] = (c >> 8) as u8;
        }
        v
    }

    fn vram_with_image(bytes: &[u8]) -> VramRouter {
        let mut v = VramRouter::new();
        v.write_cnt(BankId::A, 0x80 | 3); // texture image slot 0
        let dst = &mut v.banks[BankId::A as usize].data;
        for (i, &b) in bytes.iter().enumerate() {
            dst[i] = b;
        }
        v
    }

    #[test]
    fn test_decode_teximage_param() {
        // width=64 (3<<20=0x300000), height=32 (2<<23=0x1000000),
        // format=256-color (4<<26=0x10000000), color0 transparent (1<<29),
        // VRAM offset 0x100 (in 8-byte units = byte offset 0x800).
        let p = (0x100u32) | (3 << 20) | (2 << 23) | (4 << 26) | (1 << 29);
        let tp = TexParams::from_register(p);
        assert_eq!(tp.vram_offset, 0x800);
        assert_eq!(tp.width, 64);
        assert_eq!(tp.height, 32);
        assert_eq!(tp.format, 4);
        assert!(tp.color0_transparent);
    }

    #[test]
    fn test_wrap_coord_clamp_mode() {
        assert_eq!(wrap_coord(-5, 16, false, false), 0);
        assert_eq!(wrap_coord(20, 16, false, false), 15);
        assert_eq!(wrap_coord(8, 16, false, false), 8);
    }

    #[test]
    fn test_wrap_coord_repeat_mode() {
        assert_eq!(wrap_coord(20, 16, true, false), 4);
        assert_eq!(wrap_coord(-1, 16, true, false), 15);
    }

    #[test]
    fn test_wrap_coord_repeat_flip() {
        // Range 0..15 then mirror 15..0 → at coord 16..31, gets flipped.
        assert_eq!(wrap_coord(16, 16, true, true), 15);
        assert_eq!(wrap_coord(31, 16, true, true), 0);
        assert_eq!(wrap_coord(32, 16, true, true), 0); // back to normal direction
                                                       // Negative repeat regions mirror the same way; Rust's truncating
                                                       // division would otherwise treat -1 as repetition 0 instead of -1.
        assert_eq!(wrap_coord(-1, 16, true, true), 0);
        assert_eq!(wrap_coord(-16, 16, true, true), 15);
        assert_eq!(wrap_coord(-17, 16, true, true), 15);
    }

    #[test]
    fn test_256color_sample_with_color0_transparent() {
        let palette = vec![0x0000, 0x001F, 0x03E0, 0x7C00]; // black, red, green, blue
        let mut v = vram_with_image(&[0, 1, 2, 3]);
        // Bank E for palette
        v.write_cnt(BankId::E, 0x80 | 3);
        for (i, &c) in palette.iter().enumerate() {
            let b = &mut v.banks[BankId::E as usize].data;
            b[i * 2] = c as u8;
            b[i * 2 + 1] = (c >> 8) as u8;
        }
        let tp = TexParams {
            vram_offset: 0,
            width: 4,
            height: 1,
            format: 4,
            color0_transparent: true,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };
        // Sample texel (0, 0): palette idx 0 → transparent
        let t = sample(tp, 0, 0, 0, &v);
        assert_eq!(t.alpha, 0);
        // Sample texel (1, 0): palette idx 1 → red
        let t = sample(tp, 1, 0, 0, &v);
        assert_eq!(t.color, 0x001F);
        assert_eq!(t.alpha, 31);
    }

    #[test]
    fn test_4color_color0_transparent() {
        let mut v = vram_with_palette_at_0(&[0x0000, 0x001F]);
        v.write_cnt(BankId::A, 0x80 | 3); // texture image slot 0
                                          // Texel indices 0 then 1 in the low two 2bpp slots.
        v.banks[BankId::A as usize].data[0] = 0b0000_0100;

        let tp = TexParams {
            vram_offset: 0,
            width: 4,
            height: 1,
            format: 2,
            color0_transparent: true,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };

        assert_eq!(sample(tp, 0, 0, 0, &v).alpha, 0);
        let opaque = sample(tp, 1, 0, 0, &v);
        assert_eq!(opaque.color, 0x001F);
        assert_eq!(opaque.alpha, 31);
    }

    #[test]
    fn test_16color_color0_transparent() {
        let mut v = vram_with_palette_at_0(&[0x0000, 0x001F]);
        v.write_cnt(BankId::A, 0x80 | 3); // texture image slot 0
                                          // Texel indices 0 then 1 in the low/high 4bpp nibbles.
        v.banks[BankId::A as usize].data[0] = 0x10;

        let tp = TexParams {
            vram_offset: 0,
            width: 2,
            height: 1,
            format: 3,
            color0_transparent: true,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };

        assert_eq!(sample(tp, 0, 0, 0, &v).alpha, 0);
        let opaque = sample(tp, 1, 0, 0, &v);
        assert_eq!(opaque.color, 0x001F);
        assert_eq!(opaque.alpha, 31);
    }

    #[test]
    fn test_a3i5_alpha_expands_to_five_bits() {
        let mut v = vram_with_palette_at_0(&[0x0000, 0x001F]);
        v.write_cnt(BankId::A, 0x80 | 3); // texture image slot 0
        let image = &mut v.banks[BankId::A as usize].data;
        image[0] = 1; // alpha 0, palette index 1.
        image[1] = (1 << 5) | 1;
        image[2] = (4 << 5) | 1;
        image[3] = (7 << 5) | 1;

        let tp = TexParams {
            vram_offset: 0,
            width: 8,
            height: 8,
            format: 1,
            color0_transparent: false,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };

        assert_eq!(sample(tp, 0, 0, 0, &v).alpha, 0);
        assert_eq!(sample(tp, 1, 0, 0, &v).alpha, 4);
        assert_eq!(sample(tp, 2, 0, 0, &v).alpha, 18);
        assert_eq!(sample(tp, 3, 0, 0, &v).alpha, 31);
    }

    #[test]
    fn test_4color_palette_base_uses_8_byte_units() {
        let mut v = vram_with_image(&[0b01]);
        v.write_cnt(BankId::E, 0x80 | 3);
        // Put palette entry 1 for PLTT_BASE=1 at byte offset 8 + 2.
        let b = &mut v.banks[BankId::E as usize].data;
        b[10] = 0x1F;
        b[11] = 0x00;
        // Put a different color at the 16-byte interpretation to catch the
        // old bug.
        b[18] = 0x00;
        b[19] = 0x7C;

        let tp = TexParams {
            vram_offset: 0,
            width: 4,
            height: 1,
            format: 2,
            color0_transparent: false,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };

        assert_eq!(sample(tp, 0, 0, 1, &v).color, 0x001F);
    }

    #[test]
    fn test_16color_palette_base_uses_16_byte_units() {
        let mut v = vram_with_image(&[0x01]);
        v.write_cnt(BankId::E, 0x80 | 3);
        let b = &mut v.banks[BankId::E as usize].data;
        b[18] = 0x00;
        b[19] = 0x7C;

        let tp = TexParams {
            vram_offset: 0,
            width: 2,
            height: 1,
            format: 3,
            color0_transparent: false,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };

        assert_eq!(sample(tp, 0, 0, 1, &v).color, 0x7C00);
    }

    #[test]
    fn test_direct_color_format() {
        let v = vram_with_image(&[
            0x1F, 0x80, // texel 0: red + alpha
            0x00, 0x7C, // texel 1: blue (BGR555 = 0x7C00) + alpha 0 (top bit clear)
        ]);
        let tp = TexParams {
            vram_offset: 0,
            width: 2,
            height: 1,
            format: 7,
            color0_transparent: false,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };
        let t = sample(tp, 0, 0, 0, &v);
        assert_eq!(t.color, 0x001F);
        assert_eq!(t.alpha, 31);
        let t = sample(tp, 1, 0, 0, &v);
        assert_eq!(t.color, 0x7C00);
        assert_eq!(t.alpha, 0);
    }

    #[test]
    fn test_4x4_compressed_mode_2_samples_four_palette_colors() {
        let mut v = VramRouter::new();
        v.write_cnt(BankId::A, 0x80 | 3); // texture image slot 0
        v.write_cnt(BankId::B, 0x80 | (1 << 3) | 3); // compressed params in slot 1
        v.write_cnt(BankId::E, 0x80 | 3); // texture palette slot 0

        // One 4x4 block. Top row texel indices are 0, 1, 2, 3.
        v.banks[BankId::A as usize].data[0] = 0b11_10_01_00;
        // Palette offset 0, mode 2 = four explicit colors.
        v.banks[BankId::B as usize].data[0] = 0;
        v.banks[BankId::B as usize].data[1] = 2 << 6;
        let colors = [0x001F, 0x03E0, 0x7C00, 0x7FFF];
        for (i, &c) in colors.iter().enumerate() {
            let b = &mut v.banks[BankId::E as usize].data;
            b[i * 2] = c as u8;
            b[i * 2 + 1] = (c >> 8) as u8;
        }

        let tp = TexParams {
            vram_offset: 0,
            width: 4,
            height: 4,
            format: 5,
            color0_transparent: false,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };

        assert_eq!(sample(tp, 0, 0, 0, &v).color, colors[0]);
        assert_eq!(sample(tp, 1, 0, 0, &v).color, colors[1]);
        assert_eq!(sample(tp, 2, 0, 0, &v).color, colors[2]);
        assert_eq!(sample(tp, 3, 0, 0, &v).color, colors[3]);
    }

    #[test]
    fn test_4x4_compressed_mode_1_makes_index_3_transparent() {
        let mut v = VramRouter::new();
        v.write_cnt(BankId::A, 0x80 | 3);
        v.write_cnt(BankId::B, 0x80 | (1 << 3) | 3);
        v.write_cnt(BankId::E, 0x80 | 3);

        v.banks[BankId::A as usize].data[0] = 0b11_10_01_00;
        v.banks[BankId::B as usize].data[1] = 1 << 6;
        let b = &mut v.banks[BankId::E as usize].data;
        b[0] = 0x1F;
        b[1] = 0x00;
        b[2] = 0x00;
        b[3] = 0x7C;

        let tp = TexParams {
            vram_offset: 0,
            width: 4,
            height: 4,
            format: 5,
            color0_transparent: false,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };

        assert_eq!(sample(tp, 2, 0, 0, &v).alpha, 31);
        assert_eq!(sample(tp, 3, 0, 0, &v).alpha, 0);
    }

    #[test]
    fn test_4x4_compressed_mode_1_interpolates_index_2_evenly() {
        let mut v = VramRouter::new();
        v.write_cnt(BankId::A, 0x80 | 3);
        v.write_cnt(BankId::B, 0x80 | (1 << 3) | 3);
        v.write_cnt(BankId::E, 0x80 | 3);

        v.banks[BankId::A as usize].data[0] = 0b10;
        v.banks[BankId::B as usize].data[1] = 1 << 6;
        let b = &mut v.banks[BankId::E as usize].data;
        b[0] = 0x1F; // c0 red = 31.
        b[1] = 0x00;
        b[2] = 0xE0; // c1 green = 31.
        b[3] = 0x03;

        let tp = TexParams {
            vram_offset: 0,
            width: 4,
            height: 4,
            format: 5,
            color0_transparent: false,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };

        let texel = sample(tp, 0, 0, 0, &v);
        assert_eq!(texel.alpha, 31);
        assert_eq!(texel.color, 15 | (15 << 5));
    }

    #[test]
    fn test_4x4_compressed_mode_3_uses_five_three_weighted_colors() {
        let mut v = VramRouter::new();
        v.write_cnt(BankId::A, 0x80 | 3);
        v.write_cnt(BankId::B, 0x80 | (1 << 3) | 3);
        v.write_cnt(BankId::E, 0x80 | 3);

        // Top row indices 0, 1, 2, 3; mode 3 derives indices 2 and 3 from
        // c0/c1 with 5:3 and 3:5 weights.
        v.banks[BankId::A as usize].data[0] = 0b11_10_01_00;
        v.banks[BankId::B as usize].data[1] = 3 << 6;
        let b = &mut v.banks[BankId::E as usize].data;
        b[0] = 0x1F; // c0 red = 31.
        b[1] = 0x00;
        b[2] = 0xE0; // c1 green = 31.
        b[3] = 0x03;

        let tp = TexParams {
            vram_offset: 0,
            width: 4,
            height: 4,
            format: 5,
            color0_transparent: false,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };

        let idx2 = sample(tp, 2, 0, 0, &v);
        let idx3 = sample(tp, 3, 0, 0, &v);

        assert_eq!(idx2.alpha, 31);
        assert_eq!(idx2.color, 19 | (11 << 5));
        assert_eq!(idx3.alpha, 31);
        assert_eq!(idx3.color, 11 | (19 << 5));
    }

    #[test]
    fn test_4x4_compressed_slot2_uses_upper_slot1_params() {
        let mut v = VramRouter::new();
        v.write_cnt(BankId::A, 0x80 | (2 << 3) | 3); // texture image slot 2
        v.write_cnt(BankId::B, 0x80 | (1 << 3) | 3); // compressed params in slot 1
        v.write_cnt(BankId::E, 0x80 | 3); // texture palette slot 0

        // One 4x4 block in slot 2. Top row texel indices are 0, 1, 2, 3.
        v.banks[BankId::A as usize].data[0] = 0b11_10_01_00;
        // Slot 2's compressed palette params are in the upper 64 KiB of slot 1.
        // Mode 2 keeps index 3 opaque, so this catches reads from the lower
        // half as transparent mode 0.
        v.banks[BankId::B as usize].data[0x10000 + 1] = 2 << 6;
        let colors = [0x001F, 0x03E0, 0x7C00, 0x7FFF];
        for (i, &c) in colors.iter().enumerate() {
            let b = &mut v.banks[BankId::E as usize].data;
            b[i * 2] = c as u8;
            b[i * 2 + 1] = (c >> 8) as u8;
        }

        let tp = TexParams {
            vram_offset: 0x4_0000,
            width: 4,
            height: 4,
            format: 5,
            color0_transparent: false,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };

        assert_eq!(sample(tp, 3, 0, 0, &v).color, colors[3]);
        assert_eq!(sample(tp, 3, 0, 0, &v).alpha, 31);
    }

    #[test]
    fn test_4x4_compressed_palette_base_offsets_palette_lookup() {
        let mut v = VramRouter::new();
        v.write_cnt(BankId::A, 0x80 | 3);
        v.write_cnt(BankId::B, 0x80 | (1 << 3) | 3);
        v.write_cnt(BankId::E, 0x80 | 3);

        // One block, first texel index 0. Mode 2 samples explicit palette
        // color 0 from the palette page selected by PLTT_BASE.
        v.banks[BankId::B as usize].data[1] = 2 << 6;
        let b = &mut v.banks[BankId::E as usize].data;
        // Base 0 color 0: red. This catches accidental base-zero sampling.
        b[0] = 0x1F;
        b[1] = 0x00;
        // PLTT_BASE=1 is a 16-byte offset for 4x4-compressed textures.
        b[16] = 0xE0;
        b[17] = 0x03;

        let tp = TexParams {
            vram_offset: 0,
            width: 4,
            height: 4,
            format: 5,
            color0_transparent: false,
            repeat_s: false,
            repeat_t: false,
            flip_s: false,
            flip_t: false,
        };

        let texel = sample(tp, 0, 0, 1, &v);
        assert_eq!(texel.color, 0x03E0);
        assert_eq!(texel.alpha, 31);
    }

    #[test]
    fn test_combine_modulate_with_vertex() {
        let texel = Texel {
            color: 0x001F,
            alpha: 31,
        }; // full red
        let (out, a) = combine_with_vertex(texel, 0x7FFF, 0); // modulate with white vertex
        assert_eq!(a, 31);
        assert_eq!(out & 0x1F, 31); // r = 31 * 31 / 31 = 31
        assert_eq!((out >> 5) & 0x1F, 0);
        assert_eq!((out >> 10) & 0x1F, 0);

        // Modulate half-red texel with half-blue vertex:
        let texel = Texel {
            color: 0x0010,
            alpha: 31,
        }; // r=16
        let (out, _) = combine_with_vertex(texel, 0x4000, 0); // b=16
                                                              // r = 16 * 0 / 31 = 0, g = 0, b = 0 * 16 / 31 = 0. All channels zero
                                                              // (no overlap between texel red and vertex blue).
        assert_eq!(out, 0);
    }

    #[test]
    fn test_combine_transparent_texel_discards_fragment() {
        let texel = Texel {
            color: 0x001F,
            alpha: 0,
        };
        let (out, a) = combine_with_vertex(texel, 0x7FFF, 0);

        assert_eq!(out, 0);
        assert_eq!(a, 0);
    }

    #[test]
    fn test_modulate_uses_six_bit_expanded_formula() {
        let texel = Texel {
            color: 0x0010,
            alpha: 31,
        };
        let (out, a) = combine_with_vertex(texel, 0x0010, 0);

        assert_eq!(a, 31);
        assert_eq!(out & 0x1F, 9);
    }

    #[test]
    fn test_decal_alpha_zero_keeps_vertex_color() {
        let texel = Texel {
            color: 0x001F,
            alpha: 0,
        };
        let (out, a) = combine_with_vertex(texel, 0x7C00, 1);

        assert_eq!(a, 31);
        assert_eq!(out, 0x7C00);
    }

    #[test]
    fn test_decal_alpha_full_uses_texel_color() {
        let texel = Texel {
            color: 0x001F,
            alpha: 31,
        };
        let (out, a) = combine_with_vertex(texel, 0x7C00, 1);

        assert_eq!(a, 31);
        assert_eq!(out, 0x001F);
    }

    #[test]
    fn test_decal_mid_alpha_uses_six_bit_ratio_formula() {
        let texel = Texel {
            color: 0x001F,
            alpha: 16,
        };
        let (out, a) = combine_with_vertex(texel, 0x0000, 1);

        assert_eq!(a, 31);
        assert_eq!(out & 0x1F, 16);
    }
}
