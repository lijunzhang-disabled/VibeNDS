//! OBJ (sprite) rendering.
//!
//! 128 OAM entries × 8 bytes per engine. 12 shape × size combos. Regular
//! sprites with H/V flip; affine sprites with PA/PB/PC/PD from the 32 OAM
//! affine groups (32 × 8 bytes spread across the OAM, indexed by the
//! affine selector in attribute 1).
//!
//! 1D mapping: tiles laid out sequentially, with a per-engine boundary
//! (DISPCNT bits 20:21 for Engine A, fixed at 32 B for Engine B). 2D
//! mapping: tiles arranged in a 32-tile-wide grid (256 px wide bitmap),
//! same as GBA.

use crate::vram::VramRouter;
use super::{Engine2d, Which};

const SCREEN_WIDTH: usize = 256;

#[derive(Debug, Clone, Copy)]
pub struct ObjPixel {
    pub color: u16,
    pub priority: u8,
    /// `gfx_mode` from OAM attr0: 0 = normal, 1 = semi-transparent,
    /// 2 = OBJ window (mask only, not displayed), 3 = bitmap (NDS).
    pub gfx_mode: u8,
}

#[derive(Clone, Copy)]
pub struct ObjLine {
    pub pixel: [Option<ObjPixel>; SCREEN_WIDTH],
    /// OBJ window mask — set for pixels covered by gfx_mode=2 sprites.
    pub window: [bool; SCREEN_WIDTH],
}

impl Default for ObjLine {
    fn default() -> Self {
        ObjLine { pixel: [None; SCREEN_WIDTH], window: [false; SCREEN_WIDTH] }
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
        let tile_num = (attr2 & 0x3FF) as u32;

        let mut sprite_y = (attr0 & 0xFF) as i32;
        if sprite_y >= 192 { sprite_y -= 256; }
        let mut sprite_x = (attr1 & 0x1FF) as i32;
        if sprite_x >= 256 { sprite_x -= 512; }

        let (box_w, box_h) = if affine && disable_or_double {
            (w as i32 * 2, h as i32 * 2)
        } else {
            (w as i32, h as i32)
        };

        let row_in_box = line_i32 - sprite_y;
        if row_in_box < 0 || row_in_box >= box_h {
            continue;
        }

        let _ = mosaic; // mosaic not yet applied for OBJ in Phase 3

        if affine {
            let affine_index = ((attr1 >> 9) & 0x1F) as usize;
            // Affine params live at OAM offsets 6,14,22,30 within each
            // 32-byte affine group: PA, PB, PC, PD.
            let group_base = affine_index * 32;
            let read_i16 = |off: usize| -> i16 {
                i16::from_le_bytes([oam[off], oam[off + 1]])
            };
            let pa = read_i16(group_base + 0x06) as i32;
            let pb = read_i16(group_base + 0x0E) as i32;
            let pc = read_i16(group_base + 0x16) as i32;
            let pd = read_i16(group_base + 0x1E) as i32;

            // Center of the bounding box.
            let cx_box = box_w / 2;
            let cy_box = box_h / 2;
            let cx_tex = w as i32 / 2;
            let cy_tex = h as i32 / 2;
            let dy_box = row_in_box - cy_box;

            for col_in_box in 0..box_w {
                let screen_x = sprite_x + col_in_box;
                if screen_x < 0 || screen_x >= SCREEN_WIDTH as i32 { continue; }
                let dx_box = col_in_box - cx_box;
                let tex_x = ((pa * dx_box + pb * dy_box) >> 8) + cx_tex;
                let tex_y = ((pc * dx_box + pd * dy_box) >> 8) + cy_tex;
                if tex_x < 0 || tex_x >= w as i32 || tex_y < 0 || tex_y >= h as i32 {
                    continue;
                }
                emit_obj_pixel(
                    engine.which, vram, palette,
                    tile_num, tex_x as u32, tex_y as u32,
                    w, h, bpp_8, palette_num, priority, gfx_mode,
                    one_d_mapping, boundary,
                    screen_x as usize, out,
                );
            }
        } else {
            let h_flip = attr1 & (1 << 12) != 0;
            let v_flip = attr1 & (1 << 13) != 0;
            let row = if v_flip { (h as i32 - 1 - row_in_box) as u32 } else { row_in_box as u32 };
            for col_in_box in 0..box_w {
                let screen_x = sprite_x + col_in_box;
                if screen_x < 0 || screen_x >= SCREEN_WIDTH as i32 { continue; }
                let col = if h_flip { (w as i32 - 1 - col_in_box) as u32 } else { col_in_box as u32 };
                emit_obj_pixel(
                    engine.which, vram, palette,
                    tile_num, col, row, w, h, bpp_8,
                    palette_num, priority, gfx_mode,
                    one_d_mapping, boundary,
                    screen_x as usize, out,
                );
            }
        }
    }
}

fn obj_size(shape: u16, size: u16) -> (u32, u32) {
    match (shape, size) {
        (0, 0) => (8, 8),     (0, 1) => (16, 16),  (0, 2) => (32, 32),  (0, 3) => (64, 64),
        (1, 0) => (16, 8),    (1, 1) => (32, 8),   (1, 2) => (32, 16),  (1, 3) => (64, 32),
        (2, 0) => (8, 16),    (2, 1) => (8, 32),   (2, 2) => (16, 32),  (2, 3) => (32, 64),
        _ => (8, 8),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_obj_pixel(
    which: Which,
    vram: &VramRouter,
    palette: &[u8],
    tile_num: u32,
    tex_x: u32, tex_y: u32,
    w: u32, h: u32,
    bpp_8: bool,
    palette_num: u8,
    priority: u8,
    gfx_mode: u8,
    one_d_mapping: bool,
    boundary: u32,
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
    let tile_byte_offset = if one_d_mapping {
        tile_num * boundary + tile_offset_in_chars * bytes_per_tile
    } else {
        tile_num * 32 + tile_offset_in_chars * bytes_per_tile
    };

    // The OBJ VRAM target view starts at the engine's OBJ region base.
    let base = tile_byte_offset;
    let index = if bpp_8 {
        let addr = base + in_y * 8 + in_x;
        read_obj_u8(which, vram, addr)
    } else {
        let addr = base + in_y * 4 + (in_x >> 1);
        let byte = read_obj_u8(which, vram, addr);
        let nibble = if in_x & 1 != 0 { byte >> 4 } else { byte & 0xF };
        if nibble != 0 { palette_num * 16 + nibble } else { 0 }
    };

    if index == 0 {
        return;
    }

    if gfx_mode == 2 {
        // OBJ window — mark only, don't paint a pixel.
        out.window[screen_x] = true;
        return;
    }

    // OBJ palette lives at +0x200 inside the engine's 1 KB palette half.
    let off = 0x200 + (index as usize * 2);
    let color = u16::from_le_bytes([palette[off], palette[off + 1]]);

    // Lower OAM index wins on ties — only overwrite if currently unset.
    if out.pixel[screen_x].is_none() {
        out.pixel[screen_x] = Some(ObjPixel { color, priority, gfx_mode });
    }
}

fn read_obj_u8(which: Which, vram: &VramRouter, addr: u32) -> u8 {
    match which {
        Which::A => vram.read_engine_a_obj(addr),
        Which::B => vram.read_engine_b_obj(addr),
    }
}
