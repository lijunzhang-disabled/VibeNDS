//! Background rendering: text BGs (4bpp / 8bpp) and 256-color affine BGs.
//!
//! Phase 3 implements the bread-and-butter modes. Extended affine modes
//! (3, 4, 5) and large-screen mode 6 are deferred until we hit a game
//! that needs them.

use crate::vram::VramRouter;
use super::{Engine2d, Which};

/// One opaque pixel candidate produced by a BG.
#[derive(Debug, Clone, Copy)]
pub struct BgPixel {
    pub color: u16,
    pub priority: u8,
    pub bg_index: u8,
}

const SCREEN_WIDTH: usize = 256;

/// Render BG `n` (0..3) into `line_pixels[x] = Some(BgPixel)` if non-transparent.
/// `palette` is the engine's 1 KB palette slice (engine-A or engine-B half).
pub fn render_text_bg(
    engine: &Engine2d,
    n: usize,
    line: u16,
    palette: &[u8],
    vram: &VramRouter,
    line_pixels: &mut [Option<BgPixel>; SCREEN_WIDTH],
) {
    let bgcnt = engine.bgcnt[n];
    let priority = (bgcnt & 0x3) as u8;
    let char_base_block = ((bgcnt >> 2) & 0xF) as u32;
    let mosaic_enabled = bgcnt & (1 << 6) != 0;
    let bpp_8 = bgcnt & (1 << 7) != 0;
    let screen_base_block = ((bgcnt >> 8) & 0x1F) as u32;
    let _ext_pal_slot = (bgcnt >> 13) & 1;
    let screen_size = (bgcnt >> 14) & 0x3;

    // Char base step is 16 KB on NDS; with the engine's "char base"
    // DISPCNT field providing an extra +0x10000 step.
    let char_dispcnt = ((engine.dispcnt >> 24) & 0x7) * 0x10000;
    let screen_dispcnt = ((engine.dispcnt >> 27) & 0x7) * 0x10000;
    let char_base = char_dispcnt + char_base_block * 0x4000;
    let screen_base = screen_dispcnt + screen_base_block * 0x800;

    let (map_w_tiles, map_h_tiles) = match screen_size {
        0 => (32u32, 32u32),
        1 => (64, 32),
        2 => (32, 64),
        3 => (64, 64),
        _ => unreachable!(),
    };
    let map_w_px = map_w_tiles * 8;
    let map_h_px = map_h_tiles * 8;

    let scroll_x = engine.bg_hofs[n] as u32 & (map_w_px - 1);
    let mut scroll_y = engine.bg_vofs[n] as u32 + line as u32;
    if mosaic_enabled {
        let mh = ((engine.mosaic >> 4) & 0xF) as u32 + 1;
        scroll_y -= scroll_y % mh;
    }
    scroll_y &= map_h_px - 1;

    let tile_y = scroll_y / 8;
    let pixel_y = scroll_y & 7;

    for x in 0..SCREEN_WIDTH {
        let mut sx = (scroll_x + x as u32) & (map_w_px - 1);
        if mosaic_enabled {
            let mw = (engine.mosaic & 0xF) as u32 + 1;
            sx -= sx % mw;
        }
        let tile_x = sx / 8;
        let pixel_x = sx & 7;

        // The 32×32-tile screen blocks are stitched together for larger
        // map sizes: index = (ty / 32) * (map_w_tiles / 32) + (tx / 32).
        let blocks_x = map_w_tiles / 32;
        let block_x = tile_x / 32;
        let block_y = tile_y / 32;
        let block = block_y * blocks_x + block_x;
        let in_block_x = tile_x & 31;
        let in_block_y = tile_y & 31;
        let map_addr = screen_base + block * 0x800
                     + (in_block_y * 32 + in_block_x) * 2;
        let entry = read_bg_u16(engine.which, vram, map_addr);

        let tile_num = (entry & 0x3FF) as u32;
        let h_flip = entry & (1 << 10) != 0;
        let v_flip = entry & (1 << 11) != 0;
        let palette_num = ((entry >> 12) & 0xF) as u8;

        let row = if v_flip { 7 - pixel_y } else { pixel_y };
        let col = if h_flip { 7 - pixel_x } else { pixel_x };

        let color_idx = if bpp_8 {
            // 8bpp: 64 bytes/tile, one byte per pixel
            let tile_addr = char_base + tile_num * 64 + row * 8 + col;
            read_bg_u8(engine.which, vram, tile_addr)
        } else {
            // 4bpp: 32 bytes/tile, two pixels per byte
            let tile_addr = char_base + tile_num * 32 + row * 4 + (col >> 1);
            let byte = read_bg_u8(engine.which, vram, tile_addr);
            let nibble = if col & 1 != 0 { byte >> 4 } else { byte & 0xF };
            if nibble != 0 { palette_num * 16 + nibble } else { 0 }
        };

        if color_idx == 0 {
            continue;
        }
        let color = palette_color(palette, color_idx as u32);
        line_pixels[x] = Some(BgPixel { color, priority, bg_index: n as u8 });
    }
}

/// Render an affine BG (BG2 or BG3 in text-affine mixed modes).
pub fn render_affine_bg(
    engine: &Engine2d,
    n: usize,
    palette: &[u8],
    vram: &VramRouter,
    line_pixels: &mut [Option<BgPixel>; SCREEN_WIDTH],
) {
    let bgcnt = engine.bgcnt[n];
    let priority = (bgcnt & 0x3) as u8;
    let char_base_block = ((bgcnt >> 2) & 0xF) as u32;
    let screen_base_block = ((bgcnt >> 8) & 0x1F) as u32;
    let wraparound = bgcnt & (1 << 13) != 0;
    let screen_size = (bgcnt >> 14) & 0x3;

    let char_dispcnt = ((engine.dispcnt >> 24) & 0x7) * 0x10000;
    let screen_dispcnt = ((engine.dispcnt >> 27) & 0x7) * 0x10000;
    let char_base = char_dispcnt + char_base_block * 0x4000;
    let screen_base = screen_dispcnt + screen_base_block * 0x800;

    let map_size_tiles = 16u32 << screen_size; // 16, 32, 64, 128
    let map_w_px = map_size_tiles * 8;

    let (mut x_int, mut y_int, pa, pc) = if n == 2 {
        (engine.bg2_x_int, engine.bg2_y_int, engine.bg2_pa as i32, engine.bg2_pc as i32)
    } else {
        (engine.bg3_x_int, engine.bg3_y_int, engine.bg3_pa as i32, engine.bg3_pc as i32)
    };

    for x in 0..SCREEN_WIDTH {
        let tex_x = x_int >> 8;
        let tex_y = y_int >> 8;

        let inside = (0..map_w_px as i32).contains(&tex_x)
                  && (0..map_w_px as i32).contains(&tex_y);

        let (px, py) = if inside {
            (tex_x as u32, tex_y as u32)
        } else if wraparound {
            ((tex_x as u32) & (map_w_px - 1), (tex_y as u32) & (map_w_px - 1))
        } else {
            x_int = x_int.wrapping_add(pa);
            y_int = y_int.wrapping_add(pc);
            continue;
        };

        let tile_x = px / 8;
        let tile_y = py / 8;
        let in_x = px & 7;
        let in_y = py & 7;

        let map_addr = screen_base + tile_y * map_size_tiles + tile_x;
        let tile_num = read_bg_u8(engine.which, vram, map_addr) as u32;
        let tile_addr = char_base + tile_num * 64 + in_y * 8 + in_x;
        let color_idx = read_bg_u8(engine.which, vram, tile_addr);

        if color_idx != 0 {
            let color = palette_color(palette, color_idx as u32);
            line_pixels[x] = Some(BgPixel { color, priority, bg_index: n as u8 });
        }

        x_int = x_int.wrapping_add(pa);
        y_int = y_int.wrapping_add(pc);
    }
}

fn read_bg_u8(which: Which, vram: &VramRouter, addr: u32) -> u8 {
    match which {
        Which::A => vram.read_engine_a_bg(addr),
        Which::B => vram.read_engine_b_bg(addr),
    }
}

fn read_bg_u16(which: Which, vram: &VramRouter, addr: u32) -> u16 {
    match which {
        Which::A => vram.read_engine_a_bg_u16(addr),
        Which::B => vram.read_engine_b_bg_u16(addr),
    }
}

fn palette_color(palette: &[u8], index: u32) -> u16 {
    let off = (index as usize * 2) & 0x3FE;
    u16::from_le_bytes([palette[off], palette[off + 1]])
}
