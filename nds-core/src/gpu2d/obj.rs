//! OBJ (sprite) rendering.
//!
//! 128 OAM entries × 8 bytes per engine. 12 shape × size combos. Regular
//! sprites with H/V flip; affine sprites with PA/PB/PC/PD from the 32 OAM
//! affine groups (32 × 8 bytes spread across the OAM, indexed by the
//! affine selector in attribute 1).
//!
//! 1D mapping: tiles laid out sequentially, with the boundary selected by
//! DISPCNT bits 20:21. 2D mapping: tiles arranged in a 32-tile-wide grid
//! (256 px wide bitmap), same as GBA. In 256-color 2D mapping, hardware
//! ignores the low bit of the base tile number because each 8x8 tile consumes
//! two 32-byte character slots.

use super::{Engine2d, Which};
use crate::vram::VramRouter;

const SCREEN_WIDTH: usize = 256;

#[derive(Debug, Clone, Copy)]
pub struct ObjPixel {
    pub color: u16,
    pub priority: u8,
    pub oam_index: u8,
    /// `gfx_mode` from OAM attr0: 0 = normal, 1 = semi-transparent,
    /// 2 = OBJ window (mask only, not displayed), 3 = bitmap (NDS).
    pub gfx_mode: u8,
    /// NDS bitmap OBJ alpha from attr2 bits 12-15. Indexed OBJs use those
    /// bits as a palette bank instead.
    pub bitmap_alpha: Option<u8>,
}

#[derive(Clone, Copy)]
pub struct ObjLine {
    pub pixel: [Option<ObjPixel>; SCREEN_WIDTH],
    /// OBJ window mask — set for pixels covered by gfx_mode=2 sprites.
    pub window: [bool; SCREEN_WIDTH],
}

impl Default for ObjLine {
    fn default() -> Self {
        ObjLine {
            pixel: [None; SCREEN_WIDTH],
            window: [false; SCREEN_WIDTH],
        }
    }
}

/// Render OBJs into an OBJ scanline buffer.
pub fn render_objs(
    engine: &Engine2d,
    line: u16,
    palette: &[u8], // engine's 1 KB palette half — OBJ pal lives at +0x200
    oam: &[u8],     // engine's 1 KB OAM half
    vram: &VramRouter,
    out: &mut ObjLine,
) {
    if engine.dispcnt & (1 << 12) == 0 {
        return; // OBJs disabled
    }

    let one_d_mapping = engine.dispcnt & (1 << 4) != 0;
    let one_d_boundary_shift = (engine.dispcnt >> 20) & 0x3;
    let boundary = 32u32 << one_d_boundary_shift;
    let bitmap_mapping = BitmapObjMapping::from_dispcnt(engine.dispcnt);
    let obj_ext_palette = engine.dispcnt & (1 << 31) != 0;
    let line_i32 = line as i32;

    for sprite_idx in 0..128 {
        let off = sprite_idx * 8;
        let attr0 = u16::from_le_bytes([oam[off], oam[off + 1]]);
        let attr1 = u16::from_le_bytes([oam[off + 2], oam[off + 3]]);
        let attr2 = u16::from_le_bytes([oam[off + 4], oam[off + 5]]);

        let affine = attr0 & (1 << 8) != 0;
        let disable_or_double = attr0 & (1 << 9) != 0;
        if !affine && disable_or_double {
            continue; // disabled
        }
        let gfx_mode = ((attr0 >> 10) & 0x3) as u8;
        let mosaic = attr0 & (1 << 12) != 0;
        let bpp_8 = attr0 & (1 << 13) != 0;
        let shape = (attr0 >> 14) & 0x3;
        let size = (attr1 >> 14) & 0x3;
        let (w, h) = obj_size(shape, size);

        let priority = ((attr2 >> 10) & 0x3) as u8;
        let palette_num = ((attr2 >> 12) & 0xF) as u8;
        let bitmap_alpha = palette_num;
        let tile_num = (attr2 & 0x3FF) as u32;

        let sprite_y = (attr0 & 0xFF) as i32;
        let mut sprite_x = (attr1 & 0x1FF) as i32;
        if sprite_x >= 256 {
            sprite_x -= 512;
        }

        let (box_w, box_h) = if affine && disable_or_double {
            (w as i32 * 2, h as i32 * 2)
        } else {
            (w as i32, h as i32)
        };

        let Some(row_in_box) = obj_row_in_box(line_i32, sprite_y, box_h) else {
            continue;
        };

        let obj_mosaic_w = ((engine.mosaic >> 8) & 0xF) as i32 + 1;
        let obj_mosaic_h = ((engine.mosaic >> 12) & 0xF) as i32 + 1;

        if affine {
            let affine_index = ((attr1 >> 9) & 0x1F) as usize;
            // Affine params live at OAM offsets 6,14,22,30 within each
            // 32-byte affine group: PA, PB, PC, PD.
            let group_base = affine_index * 32;
            let read_i16 = |off: usize| -> i16 { i16::from_le_bytes([oam[off], oam[off + 1]]) };
            let pa = read_i16(group_base + 0x06) as i32;
            let pb = read_i16(group_base + 0x0E) as i32;
            let pc = read_i16(group_base + 0x16) as i32;
            let pd = read_i16(group_base + 0x1E) as i32;

            // Center of the bounding box.
            let cx_box = box_w / 2;
            let cy_box = box_h / 2;
            let cx_tex = w as i32 / 2;
            let cy_tex = h as i32 / 2;

            for col_in_box in 0..box_w {
                let screen_x = sprite_x + col_in_box;
                if screen_x < 0 || screen_x >= SCREEN_WIDTH as i32 {
                    continue;
                }
                let sample_col = if mosaic {
                    (col_in_box / obj_mosaic_w) * obj_mosaic_w
                } else {
                    col_in_box
                };
                let sample_row = if mosaic {
                    (row_in_box / obj_mosaic_h) * obj_mosaic_h
                } else {
                    row_in_box
                };
                let dx_box = sample_col - cx_box;
                let dy_box = sample_row - cy_box;
                let tex_x = ((pa * dx_box + pb * dy_box) >> 8) + cx_tex;
                let tex_y = ((pc * dx_box + pd * dy_box) >> 8) + cy_tex;
                if tex_x < 0 || tex_x >= w as i32 || tex_y < 0 || tex_y >= h as i32 {
                    continue;
                }
                emit_obj_pixel(
                    engine.which,
                    vram,
                    palette,
                    tile_num,
                    tex_x as u32,
                    tex_y as u32,
                    w,
                    h,
                    bpp_8,
                    palette_num,
                    priority,
                    sprite_idx as u8,
                    gfx_mode,
                    bitmap_alpha,
                    bitmap_mapping,
                    one_d_mapping,
                    boundary,
                    obj_ext_palette,
                    screen_x as usize,
                    out,
                );
            }
        } else {
            let h_flip = attr1 & (1 << 12) != 0;
            let v_flip = attr1 & (1 << 13) != 0;
            let sample_row_in_box = if mosaic {
                (row_in_box / obj_mosaic_h) * obj_mosaic_h
            } else {
                row_in_box
            };
            let row = if v_flip {
                (h as i32 - 1 - sample_row_in_box) as u32
            } else {
                sample_row_in_box as u32
            };
            for col_in_box in 0..box_w {
                let screen_x = sprite_x + col_in_box;
                if screen_x < 0 || screen_x >= SCREEN_WIDTH as i32 {
                    continue;
                }
                let sample_col_in_box = if mosaic {
                    (col_in_box / obj_mosaic_w) * obj_mosaic_w
                } else {
                    col_in_box
                };
                let col = if h_flip {
                    (w as i32 - 1 - sample_col_in_box) as u32
                } else {
                    sample_col_in_box as u32
                };
                emit_obj_pixel(
                    engine.which,
                    vram,
                    palette,
                    tile_num,
                    col,
                    row,
                    w,
                    h,
                    bpp_8,
                    palette_num,
                    priority,
                    sprite_idx as u8,
                    gfx_mode,
                    bitmap_alpha,
                    bitmap_mapping,
                    one_d_mapping,
                    boundary,
                    obj_ext_palette,
                    screen_x as usize,
                    out,
                );
            }
        }
    }
}

fn obj_size(shape: u16, size: u16) -> (u32, u32) {
    match (shape, size) {
        (0, 0) => (8, 8),
        (0, 1) => (16, 16),
        (0, 2) => (32, 32),
        (0, 3) => (64, 64),
        (1, 0) => (16, 8),
        (1, 1) => (32, 8),
        (1, 2) => (32, 16),
        (1, 3) => (64, 32),
        (2, 0) => (8, 16),
        (2, 1) => (8, 32),
        (2, 2) => (16, 32),
        (2, 3) => (32, 64),
        _ => (8, 8),
    }
}

fn obj_row_in_box(line: i32, sprite_y: i32, box_h: i32) -> Option<i32> {
    let row = (line - sprite_y).rem_euclid(256);
    (row < box_h).then_some(row)
}

#[derive(Debug, Clone, Copy)]
enum BitmapObjMapping {
    TwoD { source_width: u32, mask_x: u32 },
    OneD { boundary: u32 },
    Reserved,
}

impl BitmapObjMapping {
    fn from_dispcnt(dispcnt: u32) -> Self {
        let bit6 = dispcnt & (1 << 6) != 0;
        let bit5 = dispcnt & (1 << 5) != 0;
        let bit22 = dispcnt & (1 << 22) != 0;
        match (bit6, bit5, bit22) {
            (false, false, _) => BitmapObjMapping::TwoD {
                source_width: 128,
                mask_x: 0x0F,
            },
            (false, true, _) => BitmapObjMapping::TwoD {
                source_width: 256,
                mask_x: 0x1F,
            },
            (true, false, false) => BitmapObjMapping::OneD { boundary: 128 },
            (true, false, true) => BitmapObjMapping::OneD { boundary: 256 },
            (true, true, _) => BitmapObjMapping::Reserved,
        }
    }

    fn addr(self, tile_num: u32, tex_x: u32, tex_y: u32, target_width: u32) -> Option<u32> {
        match self {
            BitmapObjMapping::TwoD {
                source_width,
                mask_x,
            } => {
                let tile_base = (tile_num & mask_x) * 0x10 + (tile_num & !mask_x) * 0x80;
                Some(tile_base + (tex_y * source_width + tex_x) * 2)
            }
            BitmapObjMapping::OneD { boundary } => {
                Some(tile_num * boundary + (tex_y * target_width + tex_x) * 2)
            }
            BitmapObjMapping::Reserved => None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_obj_pixel(
    which: Which,
    vram: &VramRouter,
    palette: &[u8],
    tile_num: u32,
    tex_x: u32,
    tex_y: u32,
    w: u32,
    h: u32,
    bpp_8: bool,
    palette_num: u8,
    priority: u8,
    oam_index: u8,
    gfx_mode: u8,
    bitmap_alpha: u8,
    bitmap_mapping: BitmapObjMapping,
    one_d_mapping: bool,
    boundary: u32,
    obj_ext_palette: bool,
    screen_x: usize,
    out: &mut ObjLine,
) {
    let tile_x = tex_x / 8;
    let tile_y = tex_y / 8;
    let in_x = tex_x & 7;
    let in_y = tex_y & 7;
    let tiles_per_row_2d = if bpp_8 { 16 } else { 32 }; // 256-px-wide / (16 or 8)
    let tiles_per_row_1d = if bpp_8 { w / 8 } else { w / 8 };

    // Resolve which tile holds this texel.
    let tile_offset_in_chars = if one_d_mapping {
        tile_y * tiles_per_row_1d + tile_x
    } else {
        tile_y * tiles_per_row_2d + tile_x
    };

    let _ = h;

    let bytes_per_tile: u32 = if bpp_8 { 64 } else { 32 };
    // 1D mapping uses (boundary)-byte stride per "tile_num" unit; 2D mapping
    // uses 32-byte stride. The "tile_num" in OAM is in units of the engine's
    // boundary.
    let base_tile_num = if !one_d_mapping && bpp_8 {
        tile_num & !1
    } else {
        tile_num
    };
    let tile_byte_offset = if one_d_mapping {
        base_tile_num * boundary + tile_offset_in_chars * bytes_per_tile
    } else {
        base_tile_num * 32 + tile_offset_in_chars * bytes_per_tile
    };

    if gfx_mode == 3 {
        let Some(addr) = bitmap_mapping.addr(tile_num, tex_x, tex_y, w) else {
            return;
        };
        let color = read_obj_u16(which, vram, addr);
        if color & (1 << 15) == 0 {
            return;
        }
        if obj_pixel_wins(out.pixel[screen_x], priority, oam_index) {
            out.pixel[screen_x] = Some(ObjPixel {
                color: color & 0x7FFF,
                priority,
                oam_index,
                gfx_mode,
                bitmap_alpha: Some(bitmap_alpha.min(16)),
            });
        }
        return;
    }

    // The OBJ VRAM target view starts at the engine's OBJ region base.
    let base = tile_byte_offset;
    let index = if bpp_8 {
        let addr = base + in_y * 8 + in_x;
        read_obj_u8(which, vram, addr)
    } else {
        let addr = base + in_y * 4 + (in_x >> 1);
        let byte = read_obj_u8(which, vram, addr);
        let nibble = if in_x & 1 != 0 { byte >> 4 } else { byte & 0xF };
        if nibble != 0 {
            palette_num * 16 + nibble
        } else {
            0
        }
    };

    if index == 0 {
        return;
    }

    if gfx_mode == 2 {
        // OBJ window — mark only, don't paint a pixel.
        out.window[screen_x] = true;
        return;
    }

    let color = obj_palette_color(
        which,
        vram,
        palette,
        index,
        palette_num,
        bpp_8,
        obj_ext_palette,
    );

    if obj_pixel_wins(out.pixel[screen_x], priority, oam_index) {
        out.pixel[screen_x] = Some(ObjPixel {
            color,
            priority,
            oam_index,
            gfx_mode,
            bitmap_alpha: None,
        });
    }
}

fn obj_pixel_wins(current: Option<ObjPixel>, priority: u8, oam_index: u8) -> bool {
    match current {
        None => true,
        Some(cur) => {
            priority < cur.priority || (priority == cur.priority && oam_index < cur.oam_index)
        }
    }
}

fn read_obj_u8(which: Which, vram: &VramRouter, addr: u32) -> u8 {
    match which {
        Which::A => vram.read_engine_a_obj(addr),
        Which::B => vram.read_engine_b_obj(addr),
    }
}

fn read_obj_u16(which: Which, vram: &VramRouter, addr: u32) -> u16 {
    match which {
        Which::A => vram.read_engine_a_obj_u16(addr),
        Which::B => vram.read_engine_b_obj_u16(addr),
    }
}

fn obj_palette_color(
    which: Which,
    vram: &VramRouter,
    palette: &[u8],
    index: u8,
    palette_num: u8,
    bpp_8: bool,
    obj_ext_palette: bool,
) -> u16 {
    if obj_ext_palette && bpp_8 {
        let addr = palette_num as u32 * 0x200 + index as u32 * 2;
        let color = vram.read_obj_ext_palette_u16(which == Which::B, addr);
        if color != 0 {
            return color;
        }
    }

    // OBJ palette lives at +0x200 inside the engine's 1 KB palette half.
    let off = 0x200 + (index as usize * 2);
    u16::from_le_bytes([palette[off], palette[off + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vram::{BankId, VramRouter};

    #[test]
    fn test_obj_uses_engine_b_extended_palette() {
        let mut engine = Engine2d::new(Which::B);
        engine.dispcnt = (1 << 31) | (1 << 12) | (1 << 4);
        let mut vram = VramRouter::new();
        vram.write_cnt(BankId::D, 0x80 | 4);
        vram.write_cnt(BankId::I, 0x80 | 3);

        vram.banks[BankId::D as usize].data[0] = 3;
        let pal_off = 2 * 0x200 + 3 * 2;
        vram.banks[BankId::I as usize].data[pal_off] = 0xe0;
        vram.banks[BankId::I as usize].data[pal_off + 1] = 0x03;

        let mut oam = [0u8; 0x400];
        let attr0 = (1 << 13) as u16;
        let attr1 = 0u16;
        let attr2 = 2u16 << 12;
        oam[0..2].copy_from_slice(&attr0.to_le_bytes());
        oam[2..4].copy_from_slice(&attr1.to_le_bytes());
        oam[4..6].copy_from_slice(&attr2.to_le_bytes());

        let mut out = ObjLine::default();
        render_objs(&engine, 0, &[0; 0x400], &oam, &vram, &mut out);
        assert_eq!(out.pixel[0].expect("obj pixel").color, 0x03e0);
    }

    #[test]
    fn test_4bpp_obj_ignores_extended_palette_enable() {
        let mut engine = Engine2d::new(Which::B);
        engine.dispcnt = (1 << 31) | (1 << 12) | (1 << 4);
        let mut vram = VramRouter::new();
        vram.write_cnt(BankId::D, 0x80 | 4);
        vram.write_cnt(BankId::I, 0x80 | 3);

        vram.banks[BankId::D as usize].data[0] = 3;
        vram.banks[BankId::I as usize].data[3 * 2] = 0x00;
        vram.banks[BankId::I as usize].data[3 * 2 + 1] = 0x7c;

        let red = 0x001F_u16;
        let mut palette = [0u8; 0x400];
        palette[0x200 + 3 * 2..0x200 + 3 * 2 + 2].copy_from_slice(&red.to_le_bytes());

        let mut oam = [0u8; 0x400];
        for sprite in 1..128 {
            let off = sprite * 8;
            oam[off..off + 2].copy_from_slice(&(1u16 << 9).to_le_bytes());
        }

        let mut out = ObjLine::default();
        render_objs(&engine, 0, &palette, &oam, &vram, &mut out);

        assert_eq!(out.pixel[0].expect("4bpp obj pixel").color, red);
    }

    #[test]
    fn test_8bpp_2d_mapping_ignores_base_tile_low_bit() {
        let mut engine = Engine2d::new(Which::B);
        engine.dispcnt = 1 << 12; // OBJ enabled, 2D OBJ mapping.
        let mut vram = VramRouter::new();
        vram.write_cnt(BankId::D, 0x80 | 4);

        // Odd tile number 1 must resolve to the same 64-byte 8bpp tile as
        // tile number 0. If the low bit is used, the renderer reads byte 32
        // instead and returns the green palette entry below.
        vram.banks[BankId::D as usize].data[0] = 1;
        vram.banks[BankId::D as usize].data[32] = 2;

        let red = 0x001F_u16;
        let green = 0x03E0_u16;
        let mut palette = [0u8; 0x400];
        palette[0x200 + 2..0x200 + 4].copy_from_slice(&red.to_le_bytes());
        palette[0x200 + 4..0x200 + 6].copy_from_slice(&green.to_le_bytes());

        let mut oam = [0u8; 0x400];
        let attr0 = 1u16 << 13; // 256-color OBJ.
        let attr2 = 1u16; // odd base tile; low bit ignored in 2D mapping.
        oam[0..2].copy_from_slice(&attr0.to_le_bytes());
        oam[4..6].copy_from_slice(&attr2.to_le_bytes());
        for sprite in 1..128 {
            let off = sprite * 8;
            oam[off..off + 2].copy_from_slice(&(1u16 << 9).to_le_bytes());
        }

        let mut out = ObjLine::default();
        render_objs(&engine, 0, &palette, &oam, &vram, &mut out);

        assert_eq!(out.pixel[0].expect("8bpp obj pixel").color, red);
    }

    #[test]
    fn test_bitmap_obj_reads_direct_color_and_alpha() {
        let mut engine = Engine2d::new(Which::A);
        engine.dispcnt = (1 << 12) | (1 << 6); // OBJ enabled, bitmap 1D/128B mapping.
        let mut vram = VramRouter::new();
        vram.write_cnt(BankId::B, 0x80 | 2);

        vram.banks[BankId::B as usize].data[0..2].copy_from_slice(&0x801fu16.to_le_bytes());
        vram.banks[BankId::B as usize].data[2..4].copy_from_slice(&0x03e0u16.to_le_bytes());

        let mut oam = [0u8; 0x400];
        let attr0 = (3 << 10) as u16; // bitmap OBJ, 8x8 square
        let attr1 = 0u16;
        let attr2 = 7u16 << 12; // bitmap OBJ alpha, not palette bank
        oam[0..2].copy_from_slice(&attr0.to_le_bytes());
        oam[2..4].copy_from_slice(&attr1.to_le_bytes());
        oam[4..6].copy_from_slice(&attr2.to_le_bytes());
        for sprite in 1..128 {
            let off = sprite * 8;
            oam[off..off + 2].copy_from_slice(&(1u16 << 9).to_le_bytes());
        }

        let mut out = ObjLine::default();
        render_objs(&engine, 0, &[0; 0x400], &oam, &vram, &mut out);

        let px = out.pixel[0].expect("opaque bitmap obj pixel");
        assert_eq!(px.color, 0x001f);
        assert_eq!(px.bitmap_alpha, Some(7));
        assert!(
            out.pixel[1].is_none(),
            "bitmap OBJ bit15=0 pixels are transparent, not black"
        );
    }

    #[test]
    fn test_bitmap_obj_2d_256_mapping_uses_dispcnt5_source_width() {
        let mut engine = Engine2d::new(Which::A);
        engine.dispcnt = (1 << 12) | (1 << 5); // OBJ enabled, bitmap 2D/256-dot mapping.
        let mut vram = VramRouter::new();
        vram.write_cnt(BankId::B, 0x80 | 2);

        let addr = 256 * 2;
        vram.banks[BankId::B as usize].data[addr..addr + 2]
            .copy_from_slice(&0x83e0u16.to_le_bytes());

        let mut oam = [0u8; 0x400];
        let attr0 = (3 << 10) as u16;
        oam[0..2].copy_from_slice(&attr0.to_le_bytes());
        for sprite in 1..128 {
            let off = sprite * 8;
            oam[off..off + 2].copy_from_slice(&(1u16 << 9).to_le_bytes());
        }

        let mut out = ObjLine::default();
        render_objs(&engine, 1, &[0; 0x400], &oam, &vram, &mut out);

        assert_eq!(out.pixel[0].expect("bitmap obj pixel").color, 0x03e0);
    }

    #[test]
    fn test_bitmap_obj_1d_256_mapping_uses_dispcnt22_boundary() {
        let mut engine = Engine2d::new(Which::A);
        engine.dispcnt = (1 << 12) | (1 << 6) | (1 << 22);
        let mut vram = VramRouter::new();
        vram.write_cnt(BankId::B, 0x80 | 2);

        vram.banks[BankId::B as usize].data[0x100..0x102].copy_from_slice(&0xfc00u16.to_le_bytes());

        let mut oam = [0u8; 0x400];
        let attr0 = (3 << 10) as u16;
        let attr2 = 1u16;
        oam[0..2].copy_from_slice(&attr0.to_le_bytes());
        oam[4..6].copy_from_slice(&attr2.to_le_bytes());
        for sprite in 1..128 {
            let off = sprite * 8;
            oam[off..off + 2].copy_from_slice(&(1u16 << 9).to_le_bytes());
        }

        let mut out = ObjLine::default();
        render_objs(&engine, 0, &[0; 0x400], &oam, &vram, &mut out);

        assert_eq!(out.pixel[0].expect("bitmap obj pixel").color, 0x7c00);
    }

    #[test]
    fn test_later_obj_with_higher_priority_replaces_earlier_obj_pixel() {
        let mut engine = Engine2d::new(Which::A);
        engine.dispcnt = (1 << 12) | (1 << 4);
        let mut vram = VramRouter::new();
        vram.write_cnt(BankId::B, 0x80 | 2);
        vram.banks[BankId::B as usize].data[0] = 0x11;
        vram.banks[BankId::B as usize].data[32] = 0x22;

        let red = 0x001F_u16;
        let green = 0x03E0_u16;
        let mut palette = [0u8; 0x400];
        palette[0x200 + 2..0x200 + 4].copy_from_slice(&red.to_le_bytes());
        palette[0x200 + 4..0x200 + 6].copy_from_slice(&green.to_le_bytes());

        let mut oam = [0u8; 0x400];
        oam[4..6].copy_from_slice(&((3u16 << 10) | 0).to_le_bytes()); // priority 3, tile 0
        oam[8 + 4..8 + 6].copy_from_slice(&((0u16 << 10) | 1).to_le_bytes()); // priority 0, tile 1
        for sprite in 2..128 {
            let off = sprite * 8;
            oam[off..off + 2].copy_from_slice(&(1u16 << 9).to_le_bytes());
        }

        let mut out = ObjLine::default();
        render_objs(&engine, 0, &palette, &oam, &vram, &mut out);

        let px = out.pixel[0].expect("obj pixel");
        assert_eq!(px.color, green);
        assert_eq!(px.oam_index, 1);
        assert_eq!(px.priority, 0);
    }

    #[test]
    fn test_equal_obj_priority_keeps_lower_oam_index() {
        let mut engine = Engine2d::new(Which::A);
        engine.dispcnt = (1 << 12) | (1 << 4);
        let mut vram = VramRouter::new();
        vram.write_cnt(BankId::B, 0x80 | 2);
        vram.banks[BankId::B as usize].data[0] = 0x11;
        vram.banks[BankId::B as usize].data[32] = 0x22;

        let red = 0x001F_u16;
        let green = 0x03E0_u16;
        let mut palette = [0u8; 0x400];
        palette[0x200 + 2..0x200 + 4].copy_from_slice(&red.to_le_bytes());
        palette[0x200 + 4..0x200 + 6].copy_from_slice(&green.to_le_bytes());

        let mut oam = [0u8; 0x400];
        oam[4..6].copy_from_slice(&0u16.to_le_bytes()); // priority 0, tile 0
        oam[8 + 4..8 + 6].copy_from_slice(&1u16.to_le_bytes()); // priority 0, tile 1
        for sprite in 2..128 {
            let off = sprite * 8;
            oam[off..off + 2].copy_from_slice(&(1u16 << 9).to_le_bytes());
        }

        let mut out = ObjLine::default();
        render_objs(&engine, 0, &palette, &oam, &vram, &mut out);

        let px = out.pixel[0].expect("obj pixel");
        assert_eq!(px.color, red);
        assert_eq!(px.oam_index, 0);
        assert_eq!(px.priority, 0);
    }

    #[test]
    fn test_obj_row_in_box_wraps_across_256_line_boundary() {
        assert_eq!(obj_row_in_box(180, 180, 128), Some(0));
        assert_eq!(obj_row_in_box(0, 180, 128), Some(76));
        assert_eq!(
            obj_row_in_box(0, 180, 64),
            None,
            "only OBJs crossing the 256-line boundary wrap to the top"
        );
    }

    #[test]
    fn test_obj_y_wrap_draws_top_screen_portion() {
        let mut engine = Engine2d::new(Which::A);
        engine.dispcnt = (1 << 12) | (1 << 4);
        let mut vram = VramRouter::new();
        vram.write_cnt(BankId::B, 0x80 | 2);
        for row in 0..8 {
            let addr = row * 4;
            vram.banks[BankId::B as usize].data[addr] = (row as u8 + 1) | ((row as u8 + 1) << 4);
        }

        let mut palette = [0u8; 0x400];
        for idx in 1..=8usize {
            let color = idx as u16;
            let off = 0x200 + idx * 2;
            palette[off..off + 2].copy_from_slice(&color.to_le_bytes());
        }

        let mut oam = [0u8; 0x400];
        oam[0..2].copy_from_slice(&252u16.to_le_bytes());
        for sprite in 1..128 {
            let off = sprite * 8;
            oam[off..off + 2].copy_from_slice(&(1u16 << 9).to_le_bytes());
        }

        let mut top = ObjLine::default();
        render_objs(&engine, 0, &palette, &oam, &vram, &mut top);
        assert_eq!(
            top.pixel[0].expect("wrapped top obj pixel").color,
            5,
            "visible line 0 should sample source row 4 for a sprite at Y=252"
        );
    }

    #[test]
    fn test_obj_mosaic_reuses_left_cell_pixel() {
        let mut engine = Engine2d::new(Which::A);
        engine.dispcnt = (1 << 12) | (1 << 4);
        engine.mosaic = 1 << 8; // OBJ horizontal mosaic size = 2.
        let mut vram = VramRouter::new();
        vram.write_cnt(BankId::B, 0x80 | 2);
        vram.banks[BankId::B as usize].data[0] = 0x21; // x0=index1, x1=index2.

        let red = 0x001F_u16;
        let green = 0x03E0_u16;
        let mut palette = [0u8; 0x400];
        palette[0x200 + 2..0x200 + 4].copy_from_slice(&red.to_le_bytes());
        palette[0x200 + 4..0x200 + 6].copy_from_slice(&green.to_le_bytes());

        let mut oam = [0u8; 0x400];
        oam[0..2].copy_from_slice(&(1u16 << 12).to_le_bytes());
        for sprite in 1..128 {
            let off = sprite * 8;
            oam[off..off + 2].copy_from_slice(&(1u16 << 9).to_le_bytes());
        }

        let mut out = ObjLine::default();
        render_objs(&engine, 0, &palette, &oam, &vram, &mut out);

        assert_eq!(out.pixel[0].expect("obj pixel x0").color, red);
        assert_eq!(
            out.pixel[1].expect("obj pixel x1").color,
            red,
            "mosaic x1 should reuse the cell's left source pixel"
        );
    }

    #[test]
    fn test_obj_mosaic_reuses_top_cell_row() {
        let mut engine = Engine2d::new(Which::A);
        engine.dispcnt = (1 << 12) | (1 << 4);
        engine.mosaic = 1 << 12; // OBJ vertical mosaic size = 2.
        let mut vram = VramRouter::new();
        vram.write_cnt(BankId::B, 0x80 | 2);
        vram.banks[BankId::B as usize].data[0] = 0x11; // row 0 = index 1.
        vram.banks[BankId::B as usize].data[4] = 0x22; // row 1 = index 2.

        let red = 0x001F_u16;
        let green = 0x03E0_u16;
        let mut palette = [0u8; 0x400];
        palette[0x200 + 2..0x200 + 4].copy_from_slice(&red.to_le_bytes());
        palette[0x200 + 4..0x200 + 6].copy_from_slice(&green.to_le_bytes());

        let mut oam = [0u8; 0x400];
        oam[0..2].copy_from_slice(&(1u16 << 12).to_le_bytes());
        for sprite in 1..128 {
            let off = sprite * 8;
            oam[off..off + 2].copy_from_slice(&(1u16 << 9).to_le_bytes());
        }

        let mut out = ObjLine::default();
        render_objs(&engine, 1, &palette, &oam, &vram, &mut out);

        assert_eq!(
            out.pixel[0].expect("obj pixel row1").color,
            red,
            "mosaic row 1 should reuse the cell's top source row"
        );
    }
}
