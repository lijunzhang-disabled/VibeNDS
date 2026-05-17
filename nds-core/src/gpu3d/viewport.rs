//! Viewport transform + perspective divide.
//!
//! After clipping, each polygon's vertices are in clip space with `w > 0`
//! everywhere. The final step before rasterization:
//!
//! 1. **Perspective divide** — `x/w, y/w, z/w` to get NDC in `[-1, +1]`.
//! 2. **Viewport transform** — NDC `(-1..+1)` → screen pixels `(0..255, 0..191)`.
//!
//! The output `ScreenVertex` carries everything the rasterizer needs:
//! screen `(x, y)`, the original `w` (preserved for perspective-correct
//! interpolation in Phase 7), the depth `z`, plus color and texture coords.

use serde::{Deserialize, Serialize};

use super::matrix::ONE;
use super::vertex::{Polygon, Vertex};

/// VIEWPORT command parameter unpacked into rectangle bounds.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Viewport {
    /// Pixel-coord viewport rect (origin top-left). Default = full screen.
    pub x1: u8,
    pub y1: u8,
    pub x2: u8,
    pub y2: u8,
}

impl Viewport {
    pub fn full_screen() -> Self {
        Viewport { x1: 0, y1: 0, x2: 255, y2: 191 }
    }

    /// `VIEWPORT` command: `param = (y2 << 24) | (x2 << 16) | (y1 << 8) | x1`.
    /// y coordinates are inverted relative to the screen on real hardware
    /// (top of screen is y = 192 - 1). We treat the stored values as
    /// screen-pixel coords with y growing downward to match our framebuffer.
    pub fn from_param(param: u32) -> Self {
        let x1 = (param & 0xFF) as u8;
        let y1 = ((param >> 8) & 0xFF) as u8;
        let x2 = ((param >> 16) & 0xFF) as u8;
        let y2 = ((param >> 24) & 0xFF) as u8;
        Viewport { x1, y1, x2, y2 }
    }

    /// Viewport pixel count (inclusive of both x1 and x2). For the
    /// full-screen viewport (x1=0, x2=255) this is 256.
    pub fn width(&self) -> i32 { (self.x2 as i32 - self.x1 as i32 + 1).max(1) }
    pub fn height(&self) -> i32 { (self.y2 as i32 - self.y1 as i32 + 1).max(1) }
    /// NDC-to-pixel scale span: NDC +1 maps to pixel `x2` (inclusive); NDC
    /// −1 maps to `x1`. This is `width - 1` for non-degenerate viewports.
    pub fn span_x(&self) -> i32 { (self.x2 as i32 - self.x1 as i32).max(1) }
    pub fn span_y(&self) -> i32 { (self.y2 as i32 - self.y1 as i32).max(1) }
}

/// One vertex in screen space, post-perspective-divide.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ScreenVertex {
    /// Screen X / Y in 24.8 fixed-point (so sub-pixel for the rasterizer).
    pub screen_x: i32,
    pub screen_y: i32,
    /// Depth (Z/W), 1.19.12 fixed-point in NDC range `[-1, +1]`.
    pub depth_z: i32,
    /// Original W, preserved for perspective-correct interpolation.
    pub w: i32,
    /// Color (BGR555).
    pub color: u16,
    /// Texture coordinates (S, T) in 1.11.4 fixed-point.
    pub tex: [i16; 2],
}

/// One polygon in screen space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenPolygon {
    pub vertices: Vec<ScreenVertex>,
    pub attr: u32,
    pub tex_image_param: u32,
    pub palette_base: u16,
}

/// Apply perspective divide + viewport transform to one clip-space vertex.
fn transform_vertex(v: &Vertex, vp: Viewport) -> ScreenVertex {
    let w = v.clip[3];
    // Guard against w == 0 — caller already clipped near plane, but just in case.
    let w_safe = if w == 0 { 1 } else { w };

    // Perspective divide. NDC components in [-ONE, +ONE].
    let div = |v: i32| -> i32 {
        (((v as i64) * (ONE as i64)) / (w_safe as i64)) as i32
    };
    let ndc_x = div(v.clip[0]);
    let ndc_y = div(v.clip[1]);
    let ndc_z = div(v.clip[2]);

    // Viewport transform: ndc [-ONE, +ONE] → pixel [vp.x1, vp.x2] (inclusive).
    // Uses the *span* (x2 − x1) for the scale; NDC +1 lands exactly on x2,
    // NDC −1 lands exactly on x1. Computed in 24.8 fixed-point for sub-pixel
    // precision.
    let span_x = vp.span_x() as i64;
    let span_y = vp.span_y() as i64;
    let half_span_x = span_x * 128; // 0.5 × span_x in 24.8 (×256/2)
    let half_span_y = span_y * 128;
    let x1_8 = (vp.x1 as i64) * 256;
    let y1_8 = (vp.y1 as i64) * 256;

    // screen_x_8 = vp.x1 + (ndc_x / ONE + 1) × (span_x / 2)  [all in 24.8]
    let screen_x = x1_8 + half_span_x + ((ndc_x as i64) * half_span_x) / (ONE as i64);
    // Y is flipped because NDC +Y is up but screen +Y is down.
    let screen_y = y1_8 + half_span_y + ((-ndc_y as i64) * half_span_y) / (ONE as i64);

    ScreenVertex {
        screen_x: screen_x as i32,
        screen_y: screen_y as i32,
        depth_z: ndc_z,
        w,
        color: v.color,
        tex: v.tex,
    }
}

/// Transform every vertex of a clipped polygon to screen space.
pub fn transform_polygon(p: &Polygon, vp: Viewport) -> ScreenPolygon {
    ScreenPolygon {
        vertices: p.vertices.iter().map(|v| transform_vertex(v, vp)).collect(),
        attr: p.attr,
        tex_image_param: p.tex_image_param,
        palette_base: p.palette_base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vtx(clip: [i32; 4]) -> Vertex {
        Vertex { clip, color: 0x7FFF, tex: [0, 0] }
    }

    #[test]
    fn test_perspective_divide_centers_at_screen_center() {
        // Clip (0, 0, 0, 1) → NDC (0, 0, 0) → exact center of the
        // inclusive [0, 255] × [0, 191] viewport: (127.5, 95.5).
        let v = vtx([0, 0, 0, ONE]);
        let s = transform_vertex(&v, Viewport::full_screen());
        // 127.5 px in 24.8 = 127 × 256 + 128 = 32640
        // 95.5 px in 24.8 = 95 × 256 + 128 = 24448
        assert_eq!(s.screen_x, 127 * 256 + 128);
        assert_eq!(s.screen_y, 95 * 256 + 128);
    }

    #[test]
    fn test_perspective_divide_right_edge() {
        // Clip (w, 0, 0, w) → NDC (+1, 0, 0) → screen right (x = vp_w).
        let v = vtx([ONE, 0, 0, ONE]);
        let s = transform_vertex(&v, Viewport::full_screen());
        // Width = 255, so right edge x_8 = 255 * 256 = 65280.
        assert_eq!(s.screen_x, 255 * 256);
    }

    #[test]
    fn test_perspective_divide_top_edge() {
        // Clip (0, w, 0, w) → NDC (0, +1, 0) → screen top (y = 0).
        let v = vtx([0, ONE, 0, ONE]);
        let s = transform_vertex(&v, Viewport::full_screen());
        assert_eq!(s.screen_y, 0);
    }

    #[test]
    fn test_perspective_divide_w_double_halves_screen_pos() {
        // Doubling w doubles distance — should pull screen point halfway
        // back to center.
        let near = vtx([ONE, 0, 0, ONE]);                  // x = vp_w
        let far  = vtx([ONE, 0, 0, 2 * ONE]);              // x = vp_w/2 + center
        let s_near = transform_vertex(&near, Viewport::full_screen());
        let s_far  = transform_vertex(&far,  Viewport::full_screen());
        // s_near.x ≈ 255 × 256 = 65280
        // s_far.x  ≈ 128 × 256 + (½ × 128 × 256) = 32768 + 16384 = 49152
        let center = 128 * 256;
        assert_eq!(s_near.screen_x, 255 * 256);
        // s_far should be halfway between center and s_near.
        let halfway = (center + s_near.screen_x) / 2;
        // Within 1 pixel of the predicted halfway (rounding).
        assert!((s_far.screen_x - halfway).abs() <= 256, "got {}, expected ≈ {}", s_far.screen_x, halfway);
    }

    #[test]
    fn test_viewport_param_unpacks_correctly() {
        // x1=10, y1=20, x2=100, y2=180
        let v = Viewport::from_param((180 << 24) | (100 << 16) | (20 << 8) | 10);
        assert_eq!(v.x1, 10);
        assert_eq!(v.y1, 20);
        assert_eq!(v.x2, 100);
        assert_eq!(v.y2, 180);
        // Inclusive widths: 100 - 10 + 1 = 91, 180 - 20 + 1 = 161.
        assert_eq!(v.width(), 91);
        assert_eq!(v.height(), 161);
    }
}
