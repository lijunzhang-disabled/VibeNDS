//! 3D rasterizer.
//!
//! Phase 6 ends with a `Vec<ScreenPolygon>` — flat 2D shapes in screen
//! space with per-vertex `(screen_x, screen_y, depth_z, w, color, tex)`.
//! Phase 7's job is to turn that list into a 256×192 pixel framebuffer.
//!
//! Background: `docs/concepts/rasterization.md`.
//!
//! Implementation strategy: render the entire frame at `SWAP_BUFFERS`,
//! producing a 256×192 internal framebuffer. Engine A's BG0 path then
//! reads from that buffer per scanline when `DISPCNT` bit 3 is set —
//! same shape as the 2D engines' BG renderers.

pub mod postfx;
pub mod texture;
pub mod triangle;

use serde::{Deserialize, Serialize};

use super::viewport::ScreenPolygon;
use crate::vram::VramRouter;

/// 3D framebuffer dimensions.
pub const FB_WIDTH: usize = 256;
pub const FB_HEIGHT: usize = 192;
pub const FB_PIXELS: usize = FB_WIDTH * FB_HEIGHT;

/// Maximum depth value (W-buffer space). Anything ≥ this is "infinitely
/// far" — used to clear the depth buffer at frame start.
pub const DEPTH_MAX: i32 = 0x00FF_FFFF;

fn default_alpha_buffer() -> Vec<u8> {
    vec![0u8; FB_PIXELS]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rasterizer {
    /// 256×192 framebuffer in BGR555. Bit 15 = "pixel was written this
    /// frame" (used as the alpha bit by the 2D compositor).
    #[serde(with = "crate::bus::shared::serde_bytes_vec_u16")]
    pub framebuffer: Vec<u16>,

    /// 256×192 5-bit 3D alpha buffer. The framebuffer keeps bit 15 as a
    /// transparent/written marker; this preserves the actual 0..31 alpha
    /// value for 3D fog and final 2D compositing behavior.
    #[serde(
        default = "default_alpha_buffer",
        with = "crate::bus::shared::serde_bytes_vec"
    )]
    pub alpha_buffer: Vec<u8>,

    /// 256×192 depth buffer (Z or W per `DISP3DCNT.depth_buffer_mode`).
    /// Lower values = closer.
    #[serde(with = "crate::bus::shared::serde_bytes_vec_u32_i")]
    pub depth_buffer: Vec<i32>,

    /// 256×192 polygon-ID buffer. Used by edge marking (Phase 7 part 2)
    /// to identify polygon boundaries.
    #[serde(with = "crate::bus::shared::serde_bytes_vec")]
    pub id_buffer: Vec<u8>,
    /// 256×192 edge-mark eligibility flag. Only opaque polygons and
    /// wireframes participate in the edge-marking post-pass.
    #[serde(with = "crate::bus::shared::serde_bytes_vec")]
    pub edge_enable_buffer: Vec<u8>,
    /// 256×192 last translucent polygon ID. Hardware rejects a second
    /// translucent write with the same polygon ID at a pixel, while still
    /// allowing translucent pixels over opaque pixels of the same ID.
    #[serde(with = "crate::bus::shared::serde_bytes_vec")]
    pub translucent_id_buffer: Vec<u8>,
    /// 256×192 per-pixel fog enable flag, latched from POLYGON_ATTR bit 15.
    #[serde(with = "crate::bus::shared::serde_bytes_vec")]
    pub fog_enable_buffer: Vec<u8>,
    /// 256×192 shadow stencil buffer. Shadow polygon mode first writes a
    /// mask with polygon ID 0, then draws visible shadow polygons against it.
    #[serde(with = "crate::bus::shared::serde_bytes_vec")]
    pub shadow_stencil: Vec<u8>,

    /// `CLEAR_COLOR` register — used to fill the framebuffer at frame start.
    pub clear_color: u32,
    /// `CLEAR_DEPTH` register — 16-bit value, scaled to depth buffer range.
    pub clear_depth: u16,
    /// `CLRIMAGE_OFFSET` register — rear-plane bitmap scroll offsets.
    pub clear_image_offset: u16,
    /// `DISP3DCNT` register. Bit assignments per GBATEK:
    /// ```text
    /// [0]  Texture mapping enable
    /// [1]  Highlight mode (vs Toon)
    /// [2]  Alpha-test enable
    /// [3]  Alpha-blend enable
    /// [4]  Anti-alias enable
    /// [5]  Edge-mark enable
    /// [6]  Fog: alpha only (vs alpha + color)
    /// [7]  Fog enable
    /// [11:8] Fog shift (depth → fog-table index)
    /// ```
    pub disp3dcnt: u16,

    /// `EDGE_COLOR` — 8 BGR555 entries at `0x04000330..0x0400033F`. Indexed
    /// by the top 3 bits of the polygon ID (low 3 bits select within the
    /// same edge-color group).
    pub edge_color: [u16; 8],
    /// `FOG_COLOR` — BGR555 + alpha.
    pub fog_color: u32,
    /// `FOG_OFFSET` — depth offset before fog-table lookup.
    pub fog_offset: u16,
    /// 32-entry `FOG_TABLE` — density values 0..127 indexed by shifted depth.
    pub fog_table: [u8; 32],
    /// 32-entry `TOON_TABLE` of BGR555 values.
    pub toon_table: [u16; 32],
    /// `ALPHA_TEST_REF` — pixels with alpha <= this are discarded when
    /// alpha-test is enabled (DISP3DCNT bit 2).
    pub alpha_test_ref: u8,

    /// SWAP_BUFFERS bit 0. When clear, translucent polygons are sorted by Y;
    /// when set, software order is preserved.
    pub manual_translucent_sort: bool,
    /// SWAP_BUFFERS bit 1. When set, depth tests use per-vertex W instead of Z.
    pub w_buffering: bool,
}

impl Rasterizer {
    pub fn new() -> Self {
        Rasterizer {
            framebuffer: vec![0u16; FB_PIXELS],
            alpha_buffer: vec![0u8; FB_PIXELS],
            depth_buffer: vec![DEPTH_MAX; FB_PIXELS],
            id_buffer: vec![0u8; FB_PIXELS],
            edge_enable_buffer: vec![0u8; FB_PIXELS],
            translucent_id_buffer: vec![0xFFu8; FB_PIXELS],
            fog_enable_buffer: vec![0u8; FB_PIXELS],
            shadow_stencil: vec![0u8; FB_PIXELS],
            clear_color: 0,
            clear_depth: 0x7FFF,
            clear_image_offset: 0,
            disp3dcnt: 0,
            edge_color: [0; 8],
            fog_color: 0,
            fog_offset: 0,
            fog_table: [0; 32],
            toon_table: [0; 32],
            alpha_test_ref: 0,
            manual_translucent_sort: false,
            w_buffering: false,
        }
    }

    pub fn set_swap_attrs(&mut self, attrs: u32) {
        self.manual_translucent_sort = attrs & 1 != 0;
        self.w_buffering = attrs & 2 != 0;
    }

    /// Clear framebuffer + depth + id buffers from the clear registers.
    pub fn clear(&mut self) {
        self.clear_with_vram(None);
    }

    /// Clear framebuffer + depth + id buffers from the rear plane. When
    /// DISP3DCNT bit 14 is set, the rear plane comes from texture slots 2/3.
    pub fn clear_with_vram(&mut self, vram: Option<&VramRouter>) {
        if self.disp3dcnt & (1 << 14) != 0 {
            if let Some(vram) = vram {
                self.clear_from_rear_bitmap(vram);
                return;
            }
        }

        // BGR555 from CLEAR_COLOR low 15 bits; bit 15 = alpha (0 means
        // "no 3D pixel here", lets the 2D compositor see through).
        let alpha = ((self.clear_color >> 16) & 0x1F) != 0;
        let color = (self.clear_color & 0x7FFF) as u16 | if alpha { 1 << 15 } else { 0 };
        for p in self.framebuffer.iter_mut() {
            *p = color;
        }
        let alpha_value = ((self.clear_color >> 16) & 0x1F) as u8;
        for a in self.alpha_buffer.iter_mut() {
            *a = alpha_value;
        }

        // CLEAR_DEPTH is a 15-bit value expanded to the 24-bit hardware
        // depth range: X * 0x200 + ((X + 1) / 0x8000) * 0x1FF.
        let depth = expand_clear_depth(self.clear_depth);
        for d in self.depth_buffer.iter_mut() {
            *d = depth;
        }

        let clear_poly_id = ((self.clear_color >> 24) & 0x3F) as u8;
        for i in self.id_buffer.iter_mut() {
            *i = clear_poly_id;
        }
        for e in self.edge_enable_buffer.iter_mut() {
            *e = 0;
        }
        for t in self.translucent_id_buffer.iter_mut() {
            *t = 0xFF;
        }
        let clear_fog = if self.clear_color & (1 << 15) != 0 {
            1
        } else {
            0
        };
        for f in self.fog_enable_buffer.iter_mut() {
            *f = clear_fog;
        }
        for s in self.shadow_stencil.iter_mut() {
            *s = 0;
        }
    }

    fn clear_from_rear_bitmap(&mut self, vram: &VramRouter) {
        let clear_poly_id = ((self.clear_color >> 24) & 0x3F) as u8;
        let x_off = (self.clear_image_offset & 0x00FF) as usize;
        let y_off = ((self.clear_image_offset >> 8) & 0x00FF) as usize;

        for y in 0..FB_HEIGHT {
            for x in 0..FB_WIDTH {
                let src_x = (x + x_off) & 0xFF;
                let src_y = (y + y_off) & 0xFF;
                let src = ((src_y * 256 + src_x) * 2) as u32;
                let idx = y * FB_WIDTH + x;

                let color = read_texture_image_u16(vram, 0x4_0000 + src);
                let depth = read_texture_image_u16(vram, 0x6_0000 + src);

                self.framebuffer[idx] = color;
                self.alpha_buffer[idx] = if color & (1 << 15) != 0 { 31 } else { 0 };
                self.depth_buffer[idx] = expand_clear_depth(depth);
                self.id_buffer[idx] = clear_poly_id;
                self.edge_enable_buffer[idx] = 0;
                self.translucent_id_buffer[idx] = 0xFF;
                self.fog_enable_buffer[idx] = if depth & (1 << 15) != 0 { 1 } else { 0 };
                self.shadow_stencil[idx] = 0;
            }
        }
    }

    /// Rasterize every polygon into the framebuffer, then apply post-effects.
    ///
    /// - Opaque polygons drawn first, translucent after (per NDS convention).
    /// - `vram` is `None` for unit tests that only care about per-vertex
    ///   color paths; `Some(...)` for the real pipeline so textures work.
    pub fn render_frame(&mut self, polygons: &[ScreenPolygon], vram: Option<&VramRouter>) {
        self.clear_with_vram(vram);

        let (opaque, mut translucent): (Vec<_>, Vec<_>) =
            polygons.iter().partition(|p| !is_translucent(p));

        if !self.manual_translucent_sort {
            translucent.sort_by_key(|p| polygon_y_sort_key(p));
        }

        for p in &opaque {
            triangle::rasterize_polygon(p, self, vram);
        }
        for p in &translucent {
            triangle::rasterize_polygon(p, self, vram);
        }

        // Post-effects (each gated by its own DISP3DCNT bit).
        postfx::apply(self);
    }
}

fn expand_clear_depth(value: u16) -> i32 {
    let x = (value & 0x7FFF) as i32;
    (x << 9) + (((x + 1) >> 15) * 0x1FF)
}

fn read_texture_image_u16(vram: &VramRouter, addr: u32) -> u16 {
    let lo = vram.read_texture_image(addr) as u16;
    let hi = vram.read_texture_image(addr + 1) as u16;
    lo | (hi << 8)
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self::new()
    }
}

fn is_translucent(p: &ScreenPolygon) -> bool {
    let alpha = (p.attr >> 16) & 0x1F;
    if alpha == 0 {
        return false;
    }
    if alpha != 0 && alpha != 31 {
        return true;
    }

    // A3I5 and A5I3 carry per-texel alpha. In modulation and toon/highlight
    // modes, that alpha contributes to the final pixel alpha even when
    // POLYGON_ATTR alpha=31, so these polygons need the late translucent pass.
    // Decal mode uses texture alpha only as a color-mix ratio; final alpha is
    // Av, so an alpha=31 decal polygon remains opaque.
    let mode = (p.attr >> 4) & 0x3;
    let tex_format = (p.tex_image_param >> 26) & 0x7;
    matches!(mode, 0 | 2) && matches!(tex_format, 1 | 6)
}

fn polygon_y_sort_key(p: &&ScreenPolygon) -> i32 {
    p.vertices.iter().map(|v| v.screen_y).min().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vram::{BankId, VramRouter};

    #[test]
    fn test_clear_color_bit15_initializes_rear_plane_fog_flag() {
        let mut r = Rasterizer::new();
        r.clear_color = (1 << 15) | (0x1F << 16);

        r.clear();

        assert!(r.fog_enable_buffer.iter().all(|&f| f == 1));
    }

    #[test]
    fn test_clear_color_initializes_alpha_buffer() {
        let mut r = Rasterizer::new();
        r.clear_color = 0x12 << 16;

        r.clear();

        assert!(r.alpha_buffer.iter().all(|&a| a == 0x12));
        assert!(r.framebuffer.iter().all(|&p| p & (1 << 15) != 0));
    }

    #[test]
    fn test_clear_color_initializes_rear_plane_polygon_id() {
        let mut r = Rasterizer::new();
        r.clear_color = 0x2A << 24;

        r.clear();

        assert!(r.id_buffer.iter().all(|&id| id == 0x2A));
    }

    #[test]
    fn test_clear_depth_expands_to_hardware_depth_range() {
        assert_eq!(expand_clear_depth(0), 0);
        assert_eq!(expand_clear_depth(1), 0x200);
        assert_eq!(expand_clear_depth(0x7FFE), 0x00FF_FC00);
        assert_eq!(expand_clear_depth(0x7FFF), DEPTH_MAX);
        assert_eq!(expand_clear_depth(0xFFFF), DEPTH_MAX);
    }

    #[test]
    fn test_rear_bitmap_clear_uses_texture_slots_and_scroll() {
        let mut vram = VramRouter::new();
        vram.write_cnt(BankId::C, 0x80 | (2 << 3) | 3);
        vram.write_cnt(BankId::D, 0x80 | (3 << 3) | 3);

        let src = (3 * 256 + 2) * 2;
        {
            let color = &mut vram.banks[BankId::C as usize].data;
            color[src] = 0x1F;
            color[src + 1] = 0x80;
        }
        {
            let depth = &mut vram.banks[BankId::D as usize].data;
            depth[src] = 0x34;
            depth[src + 1] = 0x92;
        }

        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 14;
        r.clear_color = 0x2A << 24;
        r.clear_image_offset = (3 << 8) | 2;

        r.clear_with_vram(Some(&vram));

        assert_eq!(r.framebuffer[0], 0x801F);
        assert_eq!(r.depth_buffer[0], expand_clear_depth(0x1234));
        assert_eq!(r.id_buffer[0], 0x2A);
        assert_eq!(r.fog_enable_buffer[0], 1);
        assert_eq!(r.edge_enable_buffer[0], 0);
    }

    #[test]
    fn test_translucent_texture_formats_are_sorted_with_translucent_polygons_in_modulate_mode() {
        let p = ScreenPolygon {
            vertices: Vec::new(),
            attr: (31 << 16) | (1 << 6) | (1 << 7),
            tex_image_param: 6 << 26,
            palette_base: 0,
            front_area_negative: true,
        };

        assert!(is_translucent(&p));
    }

    #[test]
    fn test_opaque_decal_translucent_texture_stays_in_opaque_pass() {
        let p = ScreenPolygon {
            vertices: Vec::new(),
            attr: (1 << 4) | (31 << 16) | (1 << 6) | (1 << 7),
            tex_image_param: 6 << 26,
            palette_base: 0,
            front_area_negative: true,
        };

        assert!(!is_translucent(&p));
    }
}
