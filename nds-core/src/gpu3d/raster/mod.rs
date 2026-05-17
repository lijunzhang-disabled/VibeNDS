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

pub mod triangle;
pub mod texture;
pub mod postfx;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rasterizer {
    /// 256×192 framebuffer in BGR555. Bit 15 = "pixel was written this
    /// frame" (used as the alpha bit by the 2D compositor).
    #[serde(with = "crate::bus::shared::serde_bytes_vec_u16")]
    pub framebuffer: Vec<u16>,

    /// 256×192 depth buffer (Z or W per `DISP3DCNT.depth_buffer_mode`).
    /// Lower values = closer.
    #[serde(with = "crate::bus::shared::serde_bytes_vec_u32_i")]
    pub depth_buffer: Vec<i32>,

    /// 256×192 polygon-ID buffer. Used by edge marking (Phase 7 part 2)
    /// to identify polygon boundaries.
    #[serde(with = "crate::bus::shared::serde_bytes_vec")]
    pub id_buffer: Vec<u8>,

    /// `CLEAR_COLOR` register — used to fill the framebuffer at frame start.
    pub clear_color: u32,
    /// `CLEAR_DEPTH` register — 16-bit value, scaled to depth buffer range.
    pub clear_depth: u16,
    /// `DISP3DCNT` register. Bit assignments per GBATEK:
    /// ```text
    /// [0]  3D enable
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
    /// `ALPHA_TEST_REF` — pixels with alpha < this are discarded when
    /// alpha-test is enabled (DISP3DCNT bit 2).
    pub alpha_test_ref: u8,
}

impl Rasterizer {
    pub fn new() -> Self {
        Rasterizer {
            framebuffer: vec![0u16; FB_PIXELS],
            depth_buffer: vec![DEPTH_MAX; FB_PIXELS],
            id_buffer: vec![0u8; FB_PIXELS],
            clear_color: 0,
            clear_depth: 0x7FFF,
            disp3dcnt: 0,
            edge_color: [0; 8],
            fog_color: 0,
            fog_offset: 0,
            fog_table: [0; 32],
            toon_table: [0; 32],
            alpha_test_ref: 0,
        }
    }

    /// 3D-enable bit. When 0, the rasterizer should produce a blank
    /// (clear-color) framebuffer regardless of polygons.
    pub fn enabled(&self) -> bool { self.disp3dcnt & 0x1 != 0 }

    /// Clear framebuffer + depth + id buffers from the clear registers.
    pub fn clear(&mut self) {
        // BGR555 from CLEAR_COLOR low 15 bits; bit 15 = alpha (0 means
        // "no 3D pixel here", lets the 2D compositor see through).
        let alpha = ((self.clear_color >> 16) & 0x1F) != 0;
        let color = (self.clear_color & 0x7FFF) as u16
                  | if alpha { 1 << 15 } else { 0 };
        for p in self.framebuffer.iter_mut() { *p = color; }

        // CLEAR_DEPTH is a 16-bit value; expand to the 24-bit depth range
        // we use internally so it can be compared against per-pixel
        // depths computed in Phase 6's NDC space.
        let depth = (self.clear_depth as i32) << 9 | 0x1FF;
        for d in self.depth_buffer.iter_mut() { *d = depth; }

        for i in self.id_buffer.iter_mut() { *i = 0; }
    }

    /// Rasterize every polygon into the framebuffer, then apply post-effects.
    ///
    /// - Opaque polygons drawn first, translucent after (per NDS convention).
    /// - `vram` is `None` for unit tests that only care about per-vertex
    ///   color paths; `Some(...)` for the real pipeline so textures work.
    pub fn render_frame(&mut self, polygons: &[ScreenPolygon], vram: Option<&VramRouter>) {
        self.clear();
        if !self.enabled() { return; }

        let (opaque, translucent): (Vec<_>, Vec<_>) =
            polygons.iter().partition(|p| !is_translucent(p));

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

impl Default for Rasterizer {
    fn default() -> Self { Self::new() }
}

fn is_translucent(p: &ScreenPolygon) -> bool {
    let alpha = (p.attr >> 16) & 0x1F;
    alpha != 0 && alpha != 31
}
