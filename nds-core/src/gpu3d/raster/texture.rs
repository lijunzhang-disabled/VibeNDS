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
//!   [31:30]  texcoord transform mode (ignored here)
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
//! | 5 | 4×4 block | 0.25 (avg) | Block compression — Phase 9, stubbed here |
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
    pub fn is_disabled(self) -> bool { self.format == 0 }
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
    let mut c = coord % s;
    if c < 0 { c += s; }
    // Flip: every other repetition is mirrored.
    if flip && ((coord / s) & 1 != 0) {
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
pub fn sample(
    tp: TexParams,
    u: i32,
    v: i32,
    palette_base: u16,
    vram: &VramRouter,
) -> Texel {
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

fn read_pal_entry(palette_base: u16, idx: u32, vram: &VramRouter) -> u16 {
    // Palette base is in 16-byte units (for 4/16/256-color textures).
    let addr = ((palette_base as u32) << 4) + idx * 2;
    vram.read_texture_palette_u16(addr)
}

fn read_image_byte(tp: TexParams, byte_off: u32, vram: &VramRouter) -> u8 {
    vram.read_texture_image(tp.vram_offset + byte_off)
}

fn sample_a3i5(tp: TexParams, idx: u32, palette_base: u16, vram: &VramRouter) -> Texel {
    let byte = read_image_byte(tp, idx, vram);
    let alpha3 = (byte >> 5) & 0x7;
    let palidx = (byte & 0x1F) as u32;
    let color = read_pal_entry(palette_base, palidx, vram);
    // 3-bit alpha → 5-bit: (a3 * 4 + a3 / 2). Standard expansion.
    let alpha = (alpha3 << 2) | (alpha3 >> 1);
    Texel { color: color & 0x7FFF, alpha }
}

fn sample_a5i3(tp: TexParams, idx: u32, palette_base: u16, vram: &VramRouter) -> Texel {
    let byte = read_image_byte(tp, idx, vram);
    let alpha = (byte >> 3) & 0x1F;
    let palidx = (byte & 0x7) as u32;
    let color = read_pal_entry(palette_base, palidx, vram);
    Texel { color: color & 0x7FFF, alpha }
}

fn sample_4color(tp: TexParams, idx: u32, palette_base: u16, vram: &VramRouter) -> Texel {
    let byte = read_image_byte(tp, idx >> 2, vram);
    let bit_off = (idx & 0x3) * 2;
    let palidx = ((byte >> bit_off) & 0x3) as u32;
    palette_sample(tp, palidx, palette_base, vram)
}

fn sample_16color(tp: TexParams, idx: u32, palette_base: u16, vram: &VramRouter) -> Texel {
    let byte = read_image_byte(tp, idx >> 1, vram);
    let palidx = if idx & 1 != 0 { (byte >> 4) & 0xF } else { byte & 0xF } as u32;
    palette_sample(tp, palidx, palette_base, vram)
}

fn sample_256color(tp: TexParams, idx: u32, palette_base: u16, vram: &VramRouter) -> Texel {
    let palidx = read_image_byte(tp, idx, vram) as u32;
    palette_sample(tp, palidx, palette_base, vram)
}

fn palette_sample(tp: TexParams, palidx: u32, palette_base: u16, vram: &VramRouter) -> Texel {
    let alpha = if palidx == 0 && tp.color0_transparent { 0 } else { 31 };
    let color = if alpha == 0 {
        0
    } else {
        read_pal_entry(palette_base, palidx, vram) & 0x7FFF
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
    Texel { color: v & 0x7FFF, alpha }
}

fn sample_block_compressed(
    _tp: TexParams,
    _u: u32,
    _v: u32,
    _palette_base: u16,
    _vram: &VramRouter,
) -> Texel {
    // 4×4 block-compressed (format 5) decoder is non-trivial — defer to
    // Phase 9 polish. Return transparent so games that try to use it
    // don't paint garbage; visible as "this polygon has no texture".
    Texel { color: 0, alpha: 0 }
}

/// Combine a per-vertex (interpolated) color with a fetched texel per
/// the polygon's blend mode (`POLYGON_ATTR.mode`, bits 4-5).
///
/// | Mode | Name | Combine rule |
/// |---|---|---|
/// | 0 | Modulate | result = (texel × vertex) / 31 per channel |
/// | 1 | Decal | result = α·texel + (1-α)·vertex |
/// | 2 | Toon / highlight | same as modulate (toon is applied in post-fx) |
/// | 3 | Shadow | special — Phase 9 |
pub fn combine_with_vertex(texel: Texel, vertex_color: u16, mode: u8) -> (u16, u8) {
    if texel.alpha == 0 {
        // Texel fully transparent — keep the vertex color (with vertex's alpha = polygon alpha)
        return (vertex_color, 31);
    }

    let t_r = (texel.color & 0x1F) as u32;
    let t_g = ((texel.color >> 5) & 0x1F) as u32;
    let t_b = ((texel.color >> 10) & 0x1F) as u32;
    let v_r = (vertex_color & 0x1F) as u32;
    let v_g = ((vertex_color >> 5) & 0x1F) as u32;
    let v_b = ((vertex_color >> 10) & 0x1F) as u32;

    match mode {
        0 | 2 => {
            // Modulate: per-channel (texel * vertex) / 31.
            let r = (t_r * v_r) / 31;
            let g = (t_g * v_g) / 31;
            let b = (t_b * v_b) / 31;
            (r as u16 | ((g as u16) << 5) | ((b as u16) << 10), texel.alpha)
        }
        1 => {
            // Decal: alpha-blend texel over vertex per the texel's alpha.
            let a = texel.alpha as u32;
            let ainv = 31 - a;
            let blend = |t: u32, v: u32| -> u32 { (t * a + v * ainv) / 31 };
            let r = blend(t_r, v_r);
            let g = blend(t_g, v_g);
            let b = blend(t_b, v_b);
            (r as u16 | ((g as u16) << 5) | ((b as u16) << 10), 31)
        }
        _ => (vertex_color, 31), // shadow / unknown
    }
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
            width: 4, height: 1,
            format: 4,
            color0_transparent: true,
            repeat_s: false, repeat_t: false,
            flip_s: false, flip_t: false,
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
    fn test_direct_color_format() {
        let mut v = vram_with_image(&[
            0x1F, 0x80, // texel 0: red + alpha
            0x00, 0x7C, // texel 1: blue (BGR555 = 0x7C00) + alpha 0 (top bit clear)
        ]);
        let tp = TexParams {
            vram_offset: 0,
            width: 2, height: 1,
            format: 7,
            color0_transparent: false,
            repeat_s: false, repeat_t: false,
            flip_s: false, flip_t: false,
        };
        let t = sample(tp, 0, 0, 0, &v);
        assert_eq!(t.color, 0x001F);
        assert_eq!(t.alpha, 31);
        let t = sample(tp, 1, 0, 0, &v);
        assert_eq!(t.color, 0x7C00);
        assert_eq!(t.alpha, 0);
    }

    #[test]
    fn test_combine_modulate_with_vertex() {
        let texel = Texel { color: 0x001F, alpha: 31 }; // full red
        let (out, a) = combine_with_vertex(texel, 0x7FFF, 0); // modulate with white vertex
        assert_eq!(a, 31);
        assert_eq!(out & 0x1F, 31); // r = 31 * 31 / 31 = 31
        assert_eq!((out >> 5) & 0x1F, 0);
        assert_eq!((out >> 10) & 0x1F, 0);

        // Modulate half-red texel with half-blue vertex:
        let texel = Texel { color: 0x0010, alpha: 31 }; // r=16
        let (out, _) = combine_with_vertex(texel, 0x4000, 0); // b=16
        // r = 16 * 0 / 31 = 0, g = 0, b = 0 * 16 / 31 = 0. All channels zero
        // (no overlap between texel red and vertex blue).
        assert_eq!(out, 0);
    }
}
