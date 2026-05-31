//! Single-triangle scanline rasterizer.
//!
//! Per the rasterization concepts doc, the algorithm is:
//!
//! 1. Triangulate the polygon (3-vertex → 1 triangle; N-vertex → fan).
//! 2. For each triangle, sort vertices by `screen_y`.
//! 3. Split into top half (V_top → V_mid) and bottom half (V_mid → V_bot).
//! 4. For each scanline `y` in each half, walk two edges to find
//!    `(x_left, x_right)` plus interpolated attribute values at each end.
//! 5. For each pixel `x` in `(x_left, x_right)`, interpolate attributes,
//!    do the depth test, write the pixel.
//!
//! Per-vertex attributes carried through the interpolators:
//! - depth (linear in screen space — NDS Z-buffer mode; W-buffer mode TODO)
//! - 1/W (for perspective-correct color/texcoord recovery)
//! - color/W as 3 channels (R/W, G/W, B/W)
//! - U/W, V/W (Phase 7 part 2 — texture)
//!
//! Color interpolation is perspective-correct here even though most
//! references say "screen-linear is fine for color." Doing it correctly
//! for color is the same cost (we already need the divide for textures),
//! and the result matches real hardware more faithfully.

use super::super::viewport::{ScreenPolygon, ScreenVertex};
use super::texture::{self, TexParams};
use super::{Rasterizer, DEPTH_MAX, FB_HEIGHT, FB_WIDTH};
use crate::vram::VramRouter;

/// Per-vertex attributes carried through interpolation.
#[derive(Debug, Clone, Copy)]
struct Vert {
    x: i32,         // 24.8 fixed-point screen x
    y: i32,         // integer screen y after sort
    depth: i32,     // 1.19.12 NDC-space depth
    inv_w: i64,     // 1/W scaled by (1<<42) so products stay in i64
    r_over_w: i64,  // (R / 31) × inv_w
    g_over_w: i64,
    b_over_w: i64,
    s_over_w: i64,  // (S / 16) × inv_w  (S is 1.11.4 → divide by 16 to get pixel units)
    t_over_w: i64,
    poly_id: u8,
    alpha: u8,
}

impl Vert {
    fn from(v: &ScreenVertex, poly_id: u8, alpha: u8) -> Self {
        let y_pixel = v.screen_y >> 8;
        let w = v.w.max(1) as i64;
        let inv_w = (1i64 << 42) / w;
        let r = (v.color & 0x1F) as i64;
        let g = ((v.color >> 5) & 0x1F) as i64;
        let b = ((v.color >> 10) & 0x1F) as i64;
        let s = v.tex[0] as i64;
        let t = v.tex[1] as i64;
        Vert {
            x: v.screen_x,
            y: y_pixel,
            depth: v.depth_z,
            inv_w,
            r_over_w: r * inv_w / 31,
            g_over_w: g * inv_w / 31,
            b_over_w: b * inv_w / 31,
            // S/T are 1.11.4 fixed-point pixel coords. Scale by inv_w
            // (we'll divide by inv_w per pixel to recover the true value).
            s_over_w: (s * inv_w) >> 4,
            t_over_w: (t * inv_w) >> 4,
            poly_id,
            alpha,
        }
    }
}

/// Rasterize one polygon: triangulate by fanning around v[0], then
/// rasterize each triangle. `vram` is None for unit tests that only
/// exercise the per-vertex-color path.
pub fn rasterize_polygon(p: &ScreenPolygon, rast: &mut Rasterizer, vram: Option<&VramRouter>) {
    if p.vertices.len() < 3 { return; }
    let poly_id = ((p.attr >> 24) & 0x3F) as u8;
    let alpha = ((p.attr >> 16) & 0x1F) as u8;
    let mode = ((p.attr >> 4) & 0x3) as u8;
    let tex_params = TexParams::from_register(p.tex_image_param);
    let palette_base = p.palette_base;

    let v0 = Vert::from(&p.vertices[0], poly_id, alpha);
    for i in 1..p.vertices.len() - 1 {
        let v1 = Vert::from(&p.vertices[i], poly_id, alpha);
        let v2 = Vert::from(&p.vertices[i + 1], poly_id, alpha);
        rasterize_triangle(v0, v1, v2, rast, vram, tex_params, palette_base, mode);
    }
}

#[allow(clippy::too_many_arguments)]
fn rasterize_triangle(
    mut v0: Vert, mut v1: Vert, mut v2: Vert,
    rast: &mut Rasterizer,
    vram: Option<&VramRouter>,
    tex_params: TexParams,
    palette_base: u16,
    mode: u8,
) {
    if v0.y > v1.y { std::mem::swap(&mut v0, &mut v1); }
    if v1.y > v2.y { std::mem::swap(&mut v1, &mut v2); }
    if v0.y > v1.y { std::mem::swap(&mut v0, &mut v1); }

    if v2.y < 0 || v0.y >= FB_HEIGHT as i32 { return; }
    if v0.y == v2.y { return; }

    let total_dy = v2.y - v0.y;

    if v1.y > v0.y {
        let dy_short = v1.y - v0.y;
        for y in v0.y.max(0)..(v1.y).min(FB_HEIGHT as i32) {
            let t_long  = ((y - v0.y) as i64 * I_SCALE) / total_dy as i64;
            let t_short = ((y - v0.y) as i64 * I_SCALE) / dy_short as i64;
            let edge_long  = lerp_vert(&v0, &v2, t_long);
            let edge_short = lerp_vert(&v0, &v1, t_short);
            rasterize_scanline(y, edge_short, edge_long, rast, vram, tex_params, palette_base, mode);
        }
    }

    if v2.y > v1.y {
        let dy_short = v2.y - v1.y;
        for y in v1.y.max(0)..(v2.y).min(FB_HEIGHT as i32) {
            let t_long  = ((y - v0.y) as i64 * I_SCALE) / total_dy as i64;
            let t_short = ((y - v1.y) as i64 * I_SCALE) / dy_short as i64;
            let edge_long  = lerp_vert(&v0, &v2, t_long);
            let edge_short = lerp_vert(&v1, &v2, t_short);
            rasterize_scanline(y, edge_short, edge_long, rast, vram, tex_params, palette_base, mode);
        }
    }
}

/// Interpolation fractional bits. `t` is stored as `t_value / I_SCALE`.
const I_SCALE: i64 = 1 << 16;

#[inline]
fn lerp_i64(a: i64, b: i64, t: i64) -> i64 {
    a + ((b - a) * t) / I_SCALE
}

#[inline]
fn lerp_i32(a: i32, b: i32, t: i64) -> i32 {
    let a64 = a as i64;
    let b64 = b as i64;
    (a64 + ((b64 - a64) * t) / I_SCALE) as i32
}

/// Linearly interpolate every per-vertex attribute by `t` in `[0, I_SCALE]`.
fn lerp_vert(a: &Vert, b: &Vert, t: i64) -> Vert {
    Vert {
        x: lerp_i32(a.x, b.x, t),
        y: a.y,
        depth: lerp_i32(a.depth, b.depth, t),
        inv_w: lerp_i64(a.inv_w, b.inv_w, t),
        r_over_w: lerp_i64(a.r_over_w, b.r_over_w, t),
        g_over_w: lerp_i64(a.g_over_w, b.g_over_w, t),
        b_over_w: lerp_i64(a.b_over_w, b.b_over_w, t),
        s_over_w: lerp_i64(a.s_over_w, b.s_over_w, t),
        t_over_w: lerp_i64(a.t_over_w, b.t_over_w, t),
        poly_id: a.poly_id,
        alpha: a.alpha,
    }
}

#[allow(clippy::too_many_arguments)]
fn rasterize_scanline(
    y: i32,
    mut a: Vert,
    mut b: Vert,
    rast: &mut Rasterizer,
    vram: Option<&VramRouter>,
    tex_params: TexParams,
    palette_base: u16,
    mode: u8,
) {
    if y < 0 || y >= FB_HEIGHT as i32 { return; }
    if a.x > b.x { std::mem::swap(&mut a, &mut b); }

    let x_left  = ((a.x + 128) >> 8).max(0);
    let x_right = ((b.x + 128) >> 8).min(FB_WIDTH as i32 - 1);
    if x_left > x_right { return; }

    let dx_total = (b.x - a.x).max(1) as i64;
    let row_base = (y as usize) * FB_WIDTH;

    let textured = !tex_params.is_disabled() && vram.is_some();

    for x in x_left..=x_right {
        let x_24_8 = (x as i64) << 8;
        let t = ((x_24_8 - a.x as i64).max(0) * I_SCALE) / dx_total;
        let t = t.clamp(0, I_SCALE);

        let depth = lerp_i32(a.depth, b.depth, t);
        let inv_w = lerp_i64(a.inv_w, b.inv_w, t).max(1);
        let r_w = lerp_i64(a.r_over_w, b.r_over_w, t);
        let g_w = lerp_i64(a.g_over_w, b.g_over_w, t);
        let b_w = lerp_i64(a.b_over_w, b.b_over_w, t);

        // Perspective-correct vertex color.
        let r = ((r_w * 31) / inv_w).clamp(0, 31) as u16;
        let g = ((g_w * 31) / inv_w).clamp(0, 31) as u16;
        let bch = ((b_w * 31) / inv_w).clamp(0, 31) as u16;
        let vertex_color = r | (g << 5) | (bch << 10);

        // Combine with texel if textured.
        let (color_no_alpha_bit, frag_alpha) = if textured {
            let s_w = lerp_i64(a.s_over_w, b.s_over_w, t);
            let t_w = lerp_i64(a.t_over_w, b.t_over_w, t);
            // S = (S/W) / (1/W). S was scaled into pixel units when packed.
            let s_pixel = ((s_w * 16) / inv_w) as i32 >> 4;
            let t_pixel = ((t_w * 16) / inv_w) as i32 >> 4;
            let texel = texture::sample(tex_params, s_pixel, t_pixel, palette_base, vram.unwrap());
            texture::combine_with_vertex(texel, vertex_color, mode)
        } else {
            (vertex_color, 31)
        };

        // Alpha-test against the polygon's own alpha threshold.
        let poly_alpha = a.alpha;
        if poly_alpha == 0 { continue; } // fully transparent polygon
        let effective_alpha = ((frag_alpha as u32) * (poly_alpha as u32) / 31) as u8;
        if effective_alpha == 0 { continue; }

        let color = color_no_alpha_bit | (1 << 15);

        let depth_24 = (depth + (1 << 12)).max(0).min(DEPTH_MAX);
        let idx = row_base + x as usize;
        if depth_24 < rast.depth_buffer[idx] {
            if effective_alpha < 31 {
                let prev = rast.framebuffer[idx];
                if prev & (1 << 15) != 0 {
                    let blended = alpha_blend(color, prev, effective_alpha);
                    rast.framebuffer[idx] = blended | (1 << 15);
                } else {
                    rast.framebuffer[idx] = color;
                }
                // Translucent: don't update depth (POLYGON_ATTR bit 11 = 0 default).
            } else {
                rast.framebuffer[idx] = color;
                rast.depth_buffer[idx] = depth_24;
            }
            rast.id_buffer[idx] = a.poly_id;
        }
    }
}

/// Alpha blend in BGR555 channel space. `alpha` ∈ 0..31 = how much of
/// `top` to mix in.
fn alpha_blend(top: u16, bot: u16, alpha: u8) -> u16 {
    let a = alpha as u32;
    let ainv = 31 - a;
    let chan = |c: u16, shift: u32| ((c >> shift) & 0x1F) as u32;
    let blend = |t: u32, b: u32| -> u16 { (((t * a) + (b * ainv)) / 31).min(31) as u16 };
    let r = blend(chan(top, 0), chan(bot, 0));
    let g = blend(chan(top, 5), chan(bot, 5));
    let b = blend(chan(top, 10), chan(bot, 10));
    r | (g << 5) | (b << 10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu3d::viewport::ScreenVertex;

    fn sv(x_pixel: i32, y_pixel: i32, color: u16) -> ScreenVertex {
        ScreenVertex {
            screen_x: x_pixel << 8,
            screen_y: y_pixel << 8,
            depth_z: 0,
            w: 4096, // ONE
            color,
            tex: [0, 0],
        }
    }

    fn poly(verts: Vec<ScreenVertex>) -> ScreenPolygon {
        ScreenPolygon {
            vertices: verts,
            attr: 0x1F << 16, // alpha = 31 (opaque)
            tex_image_param: 0,
            palette_base: 0,
        }
    }

    #[test]
    fn test_solid_red_triangle_writes_red_pixels() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 0;
        let p = poly(vec![
            sv(10, 10, 0x001F),   // red
            sv(50, 10, 0x001F),
            sv(30, 30, 0x001F),
        ]);
        r.render_frame(&[p], None);

        // A pixel inside the triangle should be red, alpha-bit set.
        let center_idx = (15 * FB_WIDTH) + 30;
        let c = r.framebuffer[center_idx];
        assert!(c & (1 << 15) != 0, "alpha bit should be set");
        let r_chan = c & 0x1F;
        assert!(r_chan >= 30, "red channel should be ~31, got {}", r_chan);

        // A pixel outside the triangle should be unchanged (alpha 0).
        let outside = (0 * FB_WIDTH) + 0;
        assert_eq!(r.framebuffer[outside] & (1 << 15), 0);
    }

    #[test]
    fn test_depth_test_rejects_far_pixel() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1;
        // First polygon: blue, near (depth = -ONE).
        let near = ScreenPolygon {
            vertices: vec![
                ScreenVertex { screen_x: 10 << 8, screen_y: 10 << 8, depth_z: -4096, w: 4096, color: 0x7C00, tex: [0,0] },
                ScreenVertex { screen_x: 50 << 8, screen_y: 10 << 8, depth_z: -4096, w: 4096, color: 0x7C00, tex: [0,0] },
                ScreenVertex { screen_x: 30 << 8, screen_y: 30 << 8, depth_z: -4096, w: 4096, color: 0x7C00, tex: [0,0] },
            ],
            attr: 0x1F << 16, tex_image_param: 0, palette_base: 0,
        };
        // Second polygon: red, same shape, far (depth = +ONE).
        let far = ScreenPolygon {
            vertices: vec![
                ScreenVertex { screen_x: 10 << 8, screen_y: 10 << 8, depth_z: 4096, w: 4096, color: 0x001F, tex: [0,0] },
                ScreenVertex { screen_x: 50 << 8, screen_y: 10 << 8, depth_z: 4096, w: 4096, color: 0x001F, tex: [0,0] },
                ScreenVertex { screen_x: 30 << 8, screen_y: 30 << 8, depth_z: 4096, w: 4096, color: 0x001F, tex: [0,0] },
            ],
            attr: 0x1F << 16, tex_image_param: 0, palette_base: 0,
        };
        r.render_frame(&[near, far], None);

        // Center pixel: should still be blue (near won).
        let idx = (15 * FB_WIDTH) + 30;
        let c = r.framebuffer[idx];
        let r_chan = c & 0x1F;
        let b_chan = (c >> 10) & 0x1F;
        assert!(b_chan >= 30 && r_chan < 5,
            "expected blue (near), got 0x{:04X} (r={} b={})", c, r_chan, b_chan);
    }

    #[test]
    fn test_clear_color_fills_framebuffer() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1;
        // Set clear color to red with alpha = 1.
        r.clear_color = 0x001F | (0x1F << 16);
        r.render_frame(&[], None);
        // Every pixel should be red with alpha set.
        for &p in &r.framebuffer {
            assert_eq!(p & 0x7FFF, 0x001F);
            assert_eq!(p & (1 << 15), 1 << 15);
        }
    }

    #[test]
    fn test_disp3dcnt_bit0_does_not_gate_rasterization() {
        let mut r = Rasterizer::new();
        // DISP3DCNT bit 0 is texture mapping enable, not a 3D enable bit.
        r.disp3dcnt = 0;
        let p = poly(vec![
            sv(10, 10, 0x001F),
            sv(50, 10, 0x001F),
            sv(30, 30, 0x001F),
        ]);
        r.render_frame(&[p], None);
        let idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x001F);
    }

    #[test]
    fn test_off_screen_triangle_doesnt_crash() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1;
        // Triangle entirely above the screen.
        let p = poly(vec![
            sv(10, -50, 0x001F),
            sv(50, -50, 0x001F),
            sv(30, -30, 0x001F),
        ]);
        r.render_frame(&[p], None);
        // No writes; framebuffer stays clear.
        for &px in &r.framebuffer {
            assert_eq!(px & (1 << 15), 0);
        }
    }
}
