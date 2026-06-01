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
//! - depth (linear in screen space for Z-buffer mode, clip W for W-buffer mode)
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
    x: i32,        // 24.8 fixed-point screen x
    y: i32,        // integer screen y after sort
    depth: i32,    // 1.19.12 NDC-space depth
    w: i32,        // original clip W, for W-buffer mode
    inv_w: i64,    // 1/W scaled by (1<<42) so products stay in i64
    r_over_w: i64, // R × inv_w
    g_over_w: i64,
    b_over_w: i64,
    s_over_w: i64, // (S / 16) × inv_w  (S is 1.11.4 → divide by 16 to get pixel units)
    t_over_w: i64,
    attr: u32,
    poly_id: u8,
    alpha: u8,
}

impl Vert {
    fn from(v: &ScreenVertex, attr: u32, poly_id: u8, alpha: u8) -> Self {
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
            w: v.w,
            inv_w,
            r_over_w: r * inv_w,
            g_over_w: g * inv_w,
            b_over_w: b * inv_w,
            // S/T are 1.11.4 fixed-point pixel coords. Scale by inv_w
            // (we'll divide by inv_w per pixel to recover the true value).
            s_over_w: (s * inv_w) >> 4,
            t_over_w: (t * inv_w) >> 4,
            attr,
            poly_id,
            alpha,
        }
    }
}

/// Rasterize one polygon: triangulate by fanning around v[0], then
/// rasterize each triangle. `vram` is None for unit tests that only
/// exercise the per-vertex-color path.
pub fn rasterize_polygon(p: &ScreenPolygon, rast: &mut Rasterizer, vram: Option<&VramRouter>) {
    if p.vertices.len() < 3 {
        return;
    }
    if !polygon_surface_enabled(p) {
        return;
    }
    let poly_id = ((p.attr >> 24) & 0x3F) as u8;
    let alpha = ((p.attr >> 16) & 0x1F) as u8;
    if signed_area_2x(p).is_none() {
        if let Some((a, b)) = degenerate_line_segment(p) {
            draw_wire_line(a, b, p, poly_id, rast, vram);
        } else if let Some(a) = p.vertices.first() {
            draw_point(a, p.attr, poly_id, rast);
        }
        return;
    }
    if alpha == 0 {
        rasterize_wireframe(p, rast, poly_id, vram);
        return;
    }
    let mode = ((p.attr >> 4) & 0x3) as u8;
    let tex_params = TexParams::from_register(p.tex_image_param);
    let palette_base = p.palette_base;
    let exclude_lower_right_edges = uses_small_polygon_fill_rule(p.attr, tex_params, mode, rast);

    let v0 = Vert::from(&p.vertices[0], p.attr, poly_id, alpha);
    for i in 1..p.vertices.len() - 1 {
        let v1 = Vert::from(&p.vertices[i], p.attr, poly_id, alpha);
        let v2 = Vert::from(&p.vertices[i + 1], p.attr, poly_id, alpha);
        rasterize_triangle(
            v0,
            v1,
            v2,
            rast,
            vram,
            tex_params,
            palette_base,
            mode,
            exclude_lower_right_edges,
        );
    }
}

fn rasterize_wireframe(
    p: &ScreenPolygon,
    rast: &mut Rasterizer,
    poly_id: u8,
    vram: Option<&VramRouter>,
) {
    for i in 0..p.vertices.len() {
        let a = &p.vertices[i];
        let b = &p.vertices[(i + 1) % p.vertices.len()];
        draw_wire_line(a, b, p, poly_id, rast, vram);
    }
}

fn draw_wire_line(
    a: &ScreenVertex,
    b: &ScreenVertex,
    p: &ScreenPolygon,
    poly_id: u8,
    rast: &mut Rasterizer,
    vram: Option<&VramRouter>,
) {
    let attr = p.attr;
    let mode = ((attr >> 4) & 0x3) as u8;
    let tex_params = TexParams::from_register(p.tex_image_param);
    let texture_mapping_enabled = rast.disp3dcnt & 1 != 0;
    let textured = texture_mapping_enabled && !tex_params.is_disabled() && vram.is_some();
    // POLYGON_ATTR alpha=0 selects wireframe; actual wire pixels use Av=31.
    let attr_alpha = ((attr >> 16) & 0x1F) as u8;
    let poly_alpha = if attr_alpha == 0 { 31 } else { attr_alpha };

    let mut x0 = a.screen_x >> 8;
    let mut y0 = a.screen_y >> 8;
    let x1 = b.screen_x >> 8;
    let y1 = b.screen_y >> 8;
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let steps = dx.max(-dy).max(1);
    let mut step = 0;

    loop {
        if x0 >= 0 && x0 < FB_WIDTH as i32 && y0 >= 0 && y0 < FB_HEIGHT as i32 {
            let t = ((step as i64) << 16) / steps as i64;
            let depth = lerp_i32(a.depth_z, b.depth_z, t);
            let w = lerp_i32(a.w, b.w, t);
            let vertex_color = lerp_screen_color(a, b, t);
            let (color_no_alpha_bit, frag_alpha) = if mode == 2 {
                let texel = if textured {
                    sample_line_texel(a, b, t, tex_params, p.palette_base, vram.unwrap())
                } else {
                    texture::Texel {
                        color: 0x7FFF,
                        alpha: 31,
                    }
                };
                combine_toon_highlight(texel, vertex_color, rast)
            } else if textured {
                let texel = sample_line_texel(a, b, t, tex_params, p.palette_base, vram.unwrap());
                texture::combine_with_vertex(texel, vertex_color, mode)
            } else {
                (vertex_color, 31)
            };
            let effective_alpha = final_alpha(mode, frag_alpha, poly_alpha);
            if effective_alpha == 0 {
                if advance_line_cursor(&mut x0, &mut y0, x1, y1, sx, sy, dx, dy, &mut err) {
                    break;
                }
                step += 1;
                continue;
            }
            if rast.disp3dcnt & (1 << 2) != 0 && effective_alpha <= rast.alpha_test_ref {
                if advance_line_cursor(&mut x0, &mut y0, x1, y1, sx, sy, dx, dy, &mut err) {
                    break;
                }
                step += 1;
                continue;
            }
            let depth_24 = depth_to_buffer(depth, w, rast.w_buffering);
            let idx = y0 as usize * FB_WIDTH + x0 as usize;
            if depth_test_passes(attr, depth_24, rast.depth_buffer[idx]) {
                let preserve_edge = should_preserve_existing_edge_mark(
                    rast,
                    x0 as usize,
                    y0 as usize,
                    effective_alpha,
                );
                if is_shadow_mode(attr) {
                    // Shadow mode handles polygon-ID/stencil rejection below.
                } else if translucent_same_id_rejects(rast, idx, poly_id, effective_alpha) {
                    if advance_line_cursor(&mut x0, &mut y0, x1, y1, sx, sy, dx, dy, &mut err) {
                        break;
                    }
                    step += 1;
                    continue;
                }
                let color = color_no_alpha_bit | (1 << 15);
                if effective_alpha < 31 {
                    let prev = rast.framebuffer[idx];
                    if rast.disp3dcnt & (1 << 3) != 0 && prev & (1 << 15) != 0 {
                        rast.framebuffer[idx] =
                            alpha_blend(color, prev, effective_alpha) | (1 << 15);
                    } else {
                        rast.framebuffer[idx] = color;
                    }
                    if translucent_updates_depth(attr) {
                        rast.depth_buffer[idx] = depth_24;
                    }
                } else {
                    rast.framebuffer[idx] = color;
                    rast.depth_buffer[idx] = depth_24;
                }
                rast.id_buffer[idx] = poly_id;
                update_translucent_id(rast, idx, poly_id, effective_alpha);
                update_edge_flag(rast, idx, effective_alpha, preserve_edge);
                update_fog_flag(rast, idx, attr, effective_alpha);
            }
        }
        if advance_line_cursor(&mut x0, &mut y0, x1, y1, sx, sy, dx, dy, &mut err) {
            break;
        }
        step += 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn advance_line_cursor(
    x: &mut i32,
    y: &mut i32,
    x_end: i32,
    y_end: i32,
    sx: i32,
    sy: i32,
    dx: i32,
    dy: i32,
    err: &mut i32,
) -> bool {
    if *x == x_end && *y == y_end {
        return true;
    }
    let e2 = 2 * *err;
    if e2 >= dy {
        *err += dy;
        *x += sx;
    }
    if e2 <= dx {
        *err += dx;
        *y += sy;
    }
    false
}

fn lerp_screen_color(a: &ScreenVertex, b: &ScreenVertex, t: i64) -> u16 {
    let blend = |shift: u32| -> u16 {
        let ca = ((a.color >> shift) & 0x1F) as i32;
        let cb = ((b.color >> shift) & 0x1F) as i32;
        lerp_i32(ca, cb, t).clamp(0, 31) as u16
    };
    let r = blend(0);
    let g = blend(5);
    let b = blend(10);
    r | (g << 5) | (b << 10)
}

fn sample_line_texel(
    a: &ScreenVertex,
    b: &ScreenVertex,
    t: i64,
    tex_params: TexParams,
    palette_base: u16,
    vram: &VramRouter,
) -> texture::Texel {
    let w_a = (a.w.max(1)) as i64;
    let w_b = (b.w.max(1)) as i64;
    let inv_w_a = (1i64 << 42) / w_a;
    let inv_w_b = (1i64 << 42) / w_b;
    let inv_w = lerp_i64(inv_w_a, inv_w_b, t).max(1);
    let s_over_w_a = ((a.tex[0] as i64) * inv_w_a) >> 4;
    let s_over_w_b = ((b.tex[0] as i64) * inv_w_b) >> 4;
    let t_over_w_a = ((a.tex[1] as i64) * inv_w_a) >> 4;
    let t_over_w_b = ((b.tex[1] as i64) * inv_w_b) >> 4;
    let s_w = lerp_i64(s_over_w_a, s_over_w_b, t);
    let t_w = lerp_i64(t_over_w_a, t_over_w_b, t);
    let s_pixel = ((s_w * 16) / inv_w) as i32 >> 4;
    let t_pixel = ((t_w * 16) / inv_w) as i32 >> 4;
    texture::sample(tex_params, s_pixel, t_pixel, palette_base, vram)
}

fn draw_point(v: &ScreenVertex, attr: u32, poly_id: u8, rast: &mut Rasterizer) {
    let x = v.screen_x >> 8;
    let y = v.screen_y >> 8;
    if x < 0 || x >= FB_WIDTH as i32 || y < 0 || y >= FB_HEIGHT as i32 {
        return;
    }

    let mode = ((attr >> 4) & 0x3) as u8;
    let poly_alpha = ((attr >> 16) & 0x1F) as u8;
    let effective_alpha = final_alpha(mode, 31, poly_alpha);
    if effective_alpha == 0 {
        return;
    }
    if rast.disp3dcnt & (1 << 2) != 0 && effective_alpha <= rast.alpha_test_ref {
        return;
    }

    let depth_24 = depth_to_buffer(v.depth_z, v.w, rast.w_buffering);
    let idx = y as usize * FB_WIDTH + x as usize;
    if depth_test_passes(attr, depth_24, rast.depth_buffer[idx]) {
        let preserve_edge =
            should_preserve_existing_edge_mark(rast, x as usize, y as usize, effective_alpha);
        if !is_shadow_mode(attr) && translucent_same_id_rejects(rast, idx, poly_id, effective_alpha)
        {
            return;
        }

        let color = (v.color & 0x7FFF) | (1 << 15);
        if effective_alpha < 31 {
            let prev = rast.framebuffer[idx];
            if rast.disp3dcnt & (1 << 3) != 0 && prev & (1 << 15) != 0 {
                rast.framebuffer[idx] = alpha_blend(color, prev, effective_alpha) | (1 << 15);
            } else {
                rast.framebuffer[idx] = color;
            }
            if translucent_updates_depth(attr) {
                rast.depth_buffer[idx] = depth_24;
            }
        } else {
            rast.framebuffer[idx] = color;
            rast.depth_buffer[idx] = depth_24;
        }
        rast.id_buffer[idx] = poly_id;
        update_translucent_id(rast, idx, poly_id, effective_alpha);
        update_edge_flag(rast, idx, effective_alpha, preserve_edge);
        update_fog_flag(rast, idx, attr, effective_alpha);
    }
}

fn polygon_surface_enabled(p: &ScreenPolygon) -> bool {
    if signed_area_2x(p).is_none() {
        // Degenerate line-like polygons have no reliable facing; GBATEK notes
        // line segments are rendered regardless of front/back side.
        return true;
    }

    let render_back = p.attr & (1 << 6) != 0;
    let render_front = p.attr & (1 << 7) != 0;
    if render_back && render_front {
        return true;
    }
    if !render_back && !render_front {
        return false;
    }

    let area = signed_area_2x(p).expect("non-degenerate polygon has signed area");
    let front = if p.front_area_negative {
        area < 0
    } else {
        area > 0
    };
    (front && render_front) || (!front && render_back)
}

fn signed_area_2x(p: &ScreenPolygon) -> Option<i64> {
    if p.vertices.len() < 3 {
        return None;
    }
    let mut area = 0i64;
    for i in 0..p.vertices.len() {
        let a = &p.vertices[i];
        let b = &p.vertices[(i + 1) % p.vertices.len()];
        area +=
            (a.screen_x as i64) * (b.screen_y as i64) - (b.screen_x as i64) * (a.screen_y as i64);
    }
    if area == 0 {
        None
    } else {
        Some(area)
    }
}

fn degenerate_line_segment(p: &ScreenPolygon) -> Option<(&ScreenVertex, &ScreenVertex)> {
    let first = p.vertices.first()?;
    p.vertices
        .iter()
        .find(|v| v.screen_x != first.screen_x || v.screen_y != first.screen_y)
        .map(|second| (first, second))
}

fn uses_small_polygon_fill_rule(
    attr: u32,
    tex_params: TexParams,
    mode: u8,
    rast: &Rasterizer,
) -> bool {
    let alpha = ((attr >> 16) & 0x1F) as u8;
    if alpha == 0 {
        return false;
    }

    let edge_marking = rast.disp3dcnt & (1 << 5) != 0;
    let antialiasing = rast.disp3dcnt & (1 << 4) != 0;
    if alpha == 31 && !edge_marking && !antialiasing {
        return true;
    }

    let translucent_texture = matches!(mode, 0 | 2) && matches!(tex_params.format, 1 | 6);
    let translucent = (alpha > 0 && alpha < 31) || (alpha == 31 && translucent_texture);
    translucent && rast.disp3dcnt & (1 << 3) == 0
}

#[allow(clippy::too_many_arguments)]
fn rasterize_triangle(
    mut v0: Vert,
    mut v1: Vert,
    mut v2: Vert,
    rast: &mut Rasterizer,
    vram: Option<&VramRouter>,
    tex_params: TexParams,
    palette_base: u16,
    mode: u8,
    exclude_lower_right_edges: bool,
) {
    if v0.y > v1.y {
        std::mem::swap(&mut v0, &mut v1);
    }
    if v1.y > v2.y {
        std::mem::swap(&mut v1, &mut v2);
    }
    if v0.y > v1.y {
        std::mem::swap(&mut v0, &mut v1);
    }

    if v2.y < 0 || v0.y >= FB_HEIGHT as i32 {
        return;
    }
    if v0.y == v2.y {
        return;
    }

    let total_dy = v2.y - v0.y;

    if v1.y > v0.y {
        let dy_short = v1.y - v0.y;
        for y in v0.y.max(0)..(v1.y).min(FB_HEIGHT as i32) {
            let t_long = ((y - v0.y) as i64 * I_SCALE) / total_dy as i64;
            let t_short = ((y - v0.y) as i64 * I_SCALE) / dy_short as i64;
            let edge_long = lerp_vert(&v0, &v2, t_long);
            let edge_short = lerp_vert(&v0, &v1, t_short);
            rasterize_scanline(
                y,
                edge_short,
                edge_long,
                rast,
                vram,
                tex_params,
                palette_base,
                mode,
                exclude_lower_right_edges,
            );
        }
    }

    if v2.y > v1.y {
        let dy_short = v2.y - v1.y;
        for y in v1.y.max(0)..(v2.y).min(FB_HEIGHT as i32) {
            let t_long = ((y - v0.y) as i64 * I_SCALE) / total_dy as i64;
            let t_short = ((y - v1.y) as i64 * I_SCALE) / dy_short as i64;
            let edge_long = lerp_vert(&v0, &v2, t_long);
            let edge_short = lerp_vert(&v1, &v2, t_short);
            rasterize_scanline(
                y,
                edge_short,
                edge_long,
                rast,
                vram,
                tex_params,
                palette_base,
                mode,
                exclude_lower_right_edges,
            );
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
        w: lerp_i32(a.w, b.w, t),
        inv_w: lerp_i64(a.inv_w, b.inv_w, t),
        r_over_w: lerp_i64(a.r_over_w, b.r_over_w, t),
        g_over_w: lerp_i64(a.g_over_w, b.g_over_w, t),
        b_over_w: lerp_i64(a.b_over_w, b.b_over_w, t),
        s_over_w: lerp_i64(a.s_over_w, b.s_over_w, t),
        t_over_w: lerp_i64(a.t_over_w, b.t_over_w, t),
        attr: a.attr,
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
    exclude_lower_right_edges: bool,
) {
    if y < 0 || y >= FB_HEIGHT as i32 {
        return;
    }
    if a.x > b.x {
        std::mem::swap(&mut a, &mut b);
    }

    let x_left = ((a.x + 128) >> 8).max(0);
    let mut x_right = ((b.x + 128) >> 8).min(FB_WIDTH as i32 - 1);
    if exclude_lower_right_edges {
        x_right -= 1;
    }
    if x_left > x_right {
        return;
    }

    let dx_total = (b.x - a.x).max(1) as i64;
    let row_base = (y as usize) * FB_WIDTH;

    let texture_mapping_enabled = rast.disp3dcnt & 1 != 0;
    let textured = texture_mapping_enabled && !tex_params.is_disabled() && vram.is_some();

    for x in x_left..=x_right {
        let x_24_8 = (x as i64) << 8;
        let t = ((x_24_8 - a.x as i64).max(0) * I_SCALE) / dx_total;
        let t = t.clamp(0, I_SCALE);

        let depth = if rast.w_buffering {
            lerp_i32(a.w, b.w, t)
        } else {
            lerp_i32(a.depth, b.depth, t)
        };
        let inv_w = lerp_i64(a.inv_w, b.inv_w, t).max(1);
        let r_w = lerp_i64(a.r_over_w, b.r_over_w, t);
        let g_w = lerp_i64(a.g_over_w, b.g_over_w, t);
        let b_w = lerp_i64(a.b_over_w, b.b_over_w, t);

        // Perspective-correct vertex color.
        let r = (r_w / inv_w).clamp(0, 31) as u16;
        let g = (g_w / inv_w).clamp(0, 31) as u16;
        let bch = (b_w / inv_w).clamp(0, 31) as u16;
        let vertex_color = r | (g << 5) | (bch << 10);

        // Combine with texel if textured.
        let (color_no_alpha_bit, frag_alpha) = if mode == 2 {
            let texel = if textured {
                let s_w = lerp_i64(a.s_over_w, b.s_over_w, t);
                let t_w = lerp_i64(a.t_over_w, b.t_over_w, t);
                let s_pixel = ((s_w * 16) / inv_w) as i32 >> 4;
                let t_pixel = ((t_w * 16) / inv_w) as i32 >> 4;
                texture::sample(tex_params, s_pixel, t_pixel, palette_base, vram.unwrap())
            } else {
                texture::Texel {
                    color: 0x7FFF,
                    alpha: 31,
                }
            };
            combine_toon_highlight(texel, vertex_color, rast)
        } else if textured {
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
        if poly_alpha == 0 {
            continue;
        } // fully transparent polygon
        let effective_alpha = final_alpha(mode, frag_alpha, poly_alpha);
        if effective_alpha == 0 {
            continue;
        }
        if rast.disp3dcnt & (1 << 2) != 0 && effective_alpha <= rast.alpha_test_ref {
            continue;
        }

        let color = color_no_alpha_bit | (1 << 15);

        let depth_24 = if rast.w_buffering {
            depth_to_buffer(0, depth, true)
        } else {
            depth_to_buffer(depth, 0, false)
        };
        let idx = row_base + x as usize;
        if depth_test_passes(a.attr, depth_24, rast.depth_buffer[idx]) {
            let preserve_edge =
                should_preserve_existing_edge_mark(rast, x as usize, y as usize, effective_alpha);
            if is_shadow_mode(a.attr) {
                if a.poly_id == 0 {
                    // Shadow mask pass: no color-buffer write. The follow-up
                    // visible shadow pass uses a non-zero polygon ID.
                    rast.shadow_stencil[idx] = 1;
                    continue;
                }
                if rast.shadow_stencil[idx] != 0 {
                    rast.shadow_stencil[idx] = 0;
                    continue;
                }
                if rast.id_buffer[idx] == a.poly_id {
                    continue;
                }
            } else if translucent_same_id_rejects(rast, idx, a.poly_id, effective_alpha) {
                continue;
            }

            if effective_alpha < 31 {
                let prev = rast.framebuffer[idx];
                if rast.disp3dcnt & (1 << 3) != 0 && prev & (1 << 15) != 0 {
                    let blended = alpha_blend(color, prev, effective_alpha);
                    rast.framebuffer[idx] = blended | (1 << 15);
                } else {
                    rast.framebuffer[idx] = color;
                }
                if translucent_updates_depth(a.attr) {
                    rast.depth_buffer[idx] = depth_24;
                }
            } else {
                rast.framebuffer[idx] = color;
                rast.depth_buffer[idx] = depth_24;
            }
            rast.id_buffer[idx] = a.poly_id;
            update_translucent_id(rast, idx, a.poly_id, effective_alpha);
            update_edge_flag(rast, idx, effective_alpha, preserve_edge);
            update_fog_flag(rast, idx, a.attr, effective_alpha);
        }
    }
}

fn is_shadow_mode(attr: u32) -> bool {
    ((attr >> 4) & 0x3) == 3
}

fn translucent_same_id_rejects(
    rast: &Rasterizer,
    idx: usize,
    incoming_poly_id: u8,
    incoming_alpha: u8,
) -> bool {
    incoming_alpha < 31
        && rast.framebuffer[idx] & (1 << 15) != 0
        && rast.translucent_id_buffer[idx] == incoming_poly_id
}

fn combine_toon_highlight(
    texel: texture::Texel,
    vertex_color: u16,
    rast: &Rasterizer,
) -> (u16, u8) {
    if texel.alpha == 0 {
        return (0, 0);
    }
    let toon_idx = (vertex_color & 0x1F) as usize;
    let shade = rast.toon_table[toon_idx] & 0x7FFF;
    let highlight = rast.disp3dcnt & (1 << 1) != 0;

    let combine = |tex_shift: u32| -> u16 {
        let tex = expand_5_to_6((texel.color >> tex_shift) & 0x1F);
        let sh = expand_5_to_6((shade >> tex_shift) & 0x1F);
        let mut out = (((tex + 1) * (sh + 1)).saturating_sub(1)) / 64;
        if highlight {
            out = (out + sh).min(63);
        }
        shrink_6_to_5(out)
    };

    let r = combine(0);
    let g = combine(5);
    let b = combine(10);
    (r | (g << 5) | (b << 10), texel.alpha)
}

fn final_alpha(mode: u8, tex_alpha: u8, poly_alpha: u8) -> u8 {
    match mode {
        // Modulation and toon/highlight use the same 6-bit blend formula for
        // alpha as for RGB, with At coming from the texture and Av from
        // POLYGON_ATTR.
        0 | 2 => modulate_alpha(tex_alpha, poly_alpha),
        // Decal uses texture alpha only as a color mix ratio; output alpha is
        // the polygon alpha. Shadow mode likewise follows polygon alpha here.
        _ => poly_alpha,
    }
}

fn modulate_alpha(tex_alpha: u8, poly_alpha: u8) -> u8 {
    let at = expand_5_to_6(tex_alpha as u16) as u32;
    let av = expand_5_to_6(poly_alpha as u16) as u32;
    let blended = (((at + 1) * (av + 1)).saturating_sub(1)) / 64;
    shrink_6_to_5(blended as u16) as u8
}

#[inline]
fn expand_5_to_6(v: u16) -> u16 {
    if v == 0 {
        0
    } else {
        v * 2 + 1
    }
}

#[inline]
fn shrink_6_to_5(v: u16) -> u16 {
    (v >> 1).min(31)
}

/// Alpha blend in BGR555 channel space. `alpha` ∈ 0..31 = how much of
/// `top` to mix in.
fn alpha_blend(top: u16, bot: u16, alpha: u8) -> u16 {
    let a = alpha as u32;
    let ainv = 31 - a;
    let chan = |c: u16, shift: u32| ((c >> shift) & 0x1F) as u32;
    let blend = |t: u32, b: u32| -> u16 { (((t * (a + 1)) + (b * ainv)) / 32).min(31) as u16 };
    let r = blend(chan(top, 0), chan(bot, 0));
    let g = blend(chan(top, 5), chan(bot, 5));
    let b = blend(chan(top, 10), chan(bot, 10));
    r | (g << 5) | (b << 10)
}

fn depth_test_passes(attr: u32, incoming: i32, current: i32) -> bool {
    if attr & (1 << 14) != 0 {
        (incoming - current).abs() <= 0x200
    } else {
        incoming < current
    }
}

fn depth_to_buffer(z: i32, w: i32, w_buffering: bool) -> i32 {
    if w_buffering {
        let w15 = ((w.max(0) as i64) >> 9).clamp(0, 0x7FFF) as u16;
        return super::expand_clear_depth(w15);
    }

    let z = z.clamp(-super::super::matrix::ONE, super::super::matrix::ONE) as i64;
    let z15 = ((z * 0x4000) / super::super::matrix::ONE as i64) + 0x3FFF;
    ((z15.clamp(0, 0x7FFF) as i32) << 9).min(DEPTH_MAX)
}

fn translucent_updates_depth(attr: u32) -> bool {
    attr & (1 << 11) != 0
}

fn update_fog_flag(rast: &mut Rasterizer, idx: usize, attr: u32, effective_alpha: u8) {
    let polygon_fog = if attr & (1 << 15) != 0 { 1 } else { 0 };
    rast.fog_enable_buffer[idx] = if effective_alpha < 31 {
        rast.fog_enable_buffer[idx] & polygon_fog
    } else {
        polygon_fog
    };
}

fn update_edge_flag(
    rast: &mut Rasterizer,
    idx: usize,
    effective_alpha: u8,
    preserve_existing_edge: bool,
) {
    if effective_alpha == 31 {
        rast.edge_enable_buffer[idx] = 1;
    } else if !preserve_existing_edge {
        rast.edge_enable_buffer[idx] = 0;
    }
}

fn update_translucent_id(rast: &mut Rasterizer, idx: usize, poly_id: u8, effective_alpha: u8) {
    if effective_alpha < 31 {
        rast.translucent_id_buffer[idx] = poly_id;
    }
}

fn should_preserve_existing_edge_mark(
    rast: &Rasterizer,
    x: usize,
    y: usize,
    effective_alpha: u8,
) -> bool {
    effective_alpha < 31 && rast.edge_enable_buffer[y * FB_WIDTH + x] != 0 && {
        let id = rast.id_buffer[y * FB_WIDTH + x];
        x == 0
            || y == 0
            || x + 1 == FB_WIDTH
            || y + 1 == FB_HEIGHT
            || rast.id_buffer[y * FB_WIDTH + (x - 1)] != id
            || rast.id_buffer[y * FB_WIDTH + (x + 1)] != id
            || rast.id_buffer[(y - 1) * FB_WIDTH + x] != id
            || rast.id_buffer[(y + 1) * FB_WIDTH + x] != id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu3d::viewport::ScreenVertex;
    use crate::vram::{BankId, VramRouter};

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
            attr: (0x1F << 16) | (1 << 6) | (1 << 7), // opaque, render front/back
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        }
    }

    fn shadow_poly(verts: Vec<ScreenVertex>, poly_id: u8) -> ScreenPolygon {
        ScreenPolygon {
            vertices: verts,
            attr: (3 << 4) | (1 << 6) | (1 << 7) | (0x10 << 16) | ((poly_id as u32) << 24),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        }
    }

    fn toon_poly(verts: Vec<ScreenVertex>) -> ScreenPolygon {
        ScreenPolygon {
            vertices: verts,
            attr: (2 << 4) | (1 << 6) | (1 << 7) | (0x1F << 16),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        }
    }

    fn textured_poly(verts: Vec<ScreenVertex>) -> ScreenPolygon {
        ScreenPolygon {
            vertices: verts,
            attr: (0x1F << 16) | (1 << 6) | (1 << 7),
            tex_image_param: (7 << 26), // direct-color, 8x8, image offset 0
            palette_base: 0,
            front_area_negative: true,
        }
    }

    fn colored_poly(verts: Vec<ScreenVertex>, alpha: u8) -> ScreenPolygon {
        colored_poly_with_id(verts, alpha, 0)
    }

    fn colored_poly_with_id(verts: Vec<ScreenVertex>, alpha: u8, poly_id: u8) -> ScreenPolygon {
        ScreenPolygon {
            vertices: verts,
            attr: ((alpha as u32) << 16) | (1 << 6) | (1 << 7) | ((poly_id as u32) << 24),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        }
    }

    fn vram_with_direct_red_texture() -> VramRouter {
        let mut v = VramRouter::new();
        v.write_cnt(BankId::A, 0x80 | 3);
        let b = &mut v.banks[BankId::A as usize].data;
        for i in 0..64 {
            let off = i * 2;
            b[off] = 0x1F;
            b[off + 1] = 0x80;
        }
        v
    }

    fn vram_with_a5i3_translucent_red_texture() -> VramRouter {
        vram_with_a5i3_red_texture_alpha(15)
    }

    fn vram_with_a5i3_red_texture_alpha(alpha: u8) -> VramRouter {
        let mut v = VramRouter::new();
        v.write_cnt(BankId::A, 0x80 | 3);
        v.write_cnt(BankId::E, 0x80 | 3);

        let image = &mut v.banks[BankId::A as usize].data;
        for texel in image.iter_mut().take(64) {
            *texel = ((alpha & 0x1F) << 3) | 1; // A5I3 alpha, palette index=1.
        }

        let palette = &mut v.banks[BankId::E as usize].data;
        palette[2] = 0x1F;
        palette[3] = 0x00;
        v
    }

    #[test]
    fn test_solid_red_triangle_writes_red_pixels() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 0;
        let p = poly(vec![
            sv(10, 10, 0x001F), // red
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
                ScreenVertex {
                    screen_x: 10 << 8,
                    screen_y: 10 << 8,
                    depth_z: -4096,
                    w: 4096,
                    color: 0x7C00,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 50 << 8,
                    screen_y: 10 << 8,
                    depth_z: -4096,
                    w: 4096,
                    color: 0x7C00,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 30 << 8,
                    screen_y: 30 << 8,
                    depth_z: -4096,
                    w: 4096,
                    color: 0x7C00,
                    tex: [0, 0],
                },
            ],
            attr: (0x1F << 16) | (1 << 6) | (1 << 7),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        };
        // Second polygon: red, same shape, far (depth = +ONE).
        let far = ScreenPolygon {
            vertices: vec![
                ScreenVertex {
                    screen_x: 10 << 8,
                    screen_y: 10 << 8,
                    depth_z: 4096,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 50 << 8,
                    screen_y: 10 << 8,
                    depth_z: 4096,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 30 << 8,
                    screen_y: 30 << 8,
                    depth_z: 4096,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
            ],
            attr: (0x1F << 16) | (1 << 6) | (1 << 7),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        };
        r.render_frame(&[near, far], None);

        // Center pixel: should still be blue (near won).
        let idx = (15 * FB_WIDTH) + 30;
        let c = r.framebuffer[idx];
        let r_chan = c & 0x1F;
        let b_chan = (c >> 10) & 0x1F;
        assert!(
            b_chan >= 30 && r_chan < 5,
            "expected blue (near), got 0x{:04X} (r={} b={})",
            c,
            r_chan,
            b_chan
        );
    }

    #[test]
    fn test_z_depth_expands_to_depth_buffer_range() {
        assert_eq!(depth_to_buffer(-crate::gpu3d::matrix::ONE, 0, false), 0);
        assert_eq!(depth_to_buffer(0, 0, false), 0x3FFF << 9);
        assert_eq!(
            depth_to_buffer(crate::gpu3d::matrix::ONE / 2, 0, false),
            0x5FFF << 9
        );
        assert_eq!(
            depth_to_buffer(crate::gpu3d::matrix::ONE, 0, false),
            0x7FFF << 9
        );
        assert_eq!(
            depth_to_buffer(crate::gpu3d::matrix::ONE * 2, 0, false),
            0x7FFF << 9
        );
        assert_eq!(depth_to_buffer(0, 0, true), 0);
        assert_eq!(depth_to_buffer(0, 4096, true), 8 << 9);
        assert_eq!(depth_to_buffer(0, DEPTH_MAX + 1, true), DEPTH_MAX);
    }

    #[test]
    fn test_opaque_polygon_without_edge_or_aa_excludes_right_edge() {
        let mut r = Rasterizer::new();
        let p = poly(vec![
            sv(10, 10, 0x001F),
            sv(20, 10, 0x001F),
            sv(20, 20, 0x001F),
        ]);

        r.render_frame(&[p], None);

        let inside = 15 * FB_WIDTH + 18;
        let right_edge = 15 * FB_WIDTH + 20;
        assert_eq!(r.framebuffer[inside] & 0x7FFF, 0x001F);
        assert_eq!(r.framebuffer[right_edge] & (1 << 15), 0);
    }

    #[test]
    fn test_edge_marking_disables_small_polygon_right_edge_exclusion() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 5;
        let p = poly(vec![
            sv(10, 10, 0x001F),
            sv(20, 10, 0x001F),
            sv(20, 20, 0x001F),
        ]);

        r.render_frame(&[p], None);

        let right_edge = 15 * FB_WIDTH + 20;
        assert_eq!(r.framebuffer[right_edge] & (1 << 15), 1 << 15);
    }

    #[test]
    fn test_translucent_polygon_excludes_right_edge_when_alpha_blend_disabled() {
        let mut r = Rasterizer::new();
        let p = colored_poly(
            vec![sv(10, 10, 0x001F), sv(20, 10, 0x001F), sv(20, 20, 0x001F)],
            16,
        );

        r.render_frame(&[p], None);

        let right_edge = 15 * FB_WIDTH + 20;
        assert_eq!(r.framebuffer[right_edge] & (1 << 15), 0);
    }

    #[test]
    fn test_translucent_polygon_keeps_right_edge_when_alpha_blend_enabled() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 3;
        let p = colored_poly(
            vec![sv(10, 10, 0x001F), sv(20, 10, 0x001F), sv(20, 20, 0x001F)],
            16,
        );

        r.render_frame(&[p], None);

        let right_edge = 15 * FB_WIDTH + 20;
        assert_eq!(r.framebuffer[right_edge] & (1 << 15), 1 << 15);
    }

    #[test]
    fn test_translucent_polygon_overwrites_when_alpha_blend_disabled() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 0;
        let blue = colored_poly(
            vec![
                ScreenVertex {
                    screen_x: 10 << 8,
                    screen_y: 10 << 8,
                    depth_z: 2048,
                    w: 4096,
                    color: 0x7C00,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 50 << 8,
                    screen_y: 10 << 8,
                    depth_z: 2048,
                    w: 4096,
                    color: 0x7C00,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 30 << 8,
                    screen_y: 30 << 8,
                    depth_z: 2048,
                    w: 4096,
                    color: 0x7C00,
                    tex: [0, 0],
                },
            ],
            31,
        );
        let red = colored_poly(
            vec![
                ScreenVertex {
                    screen_x: 10 << 8,
                    screen_y: 10 << 8,
                    depth_z: 0,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 50 << 8,
                    screen_y: 10 << 8,
                    depth_z: 0,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 30 << 8,
                    screen_y: 30 << 8,
                    depth_z: 0,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
            ],
            16,
        );

        r.render_frame(&[blue, red], None);

        let idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x001F);
    }

    #[test]
    fn test_translucent_polygon_blends_when_alpha_blend_enabled() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 3;
        let blue = colored_poly(
            vec![
                ScreenVertex {
                    screen_x: 10 << 8,
                    screen_y: 10 << 8,
                    depth_z: 2048,
                    w: 4096,
                    color: 0x7C00,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 50 << 8,
                    screen_y: 10 << 8,
                    depth_z: 2048,
                    w: 4096,
                    color: 0x7C00,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 30 << 8,
                    screen_y: 30 << 8,
                    depth_z: 2048,
                    w: 4096,
                    color: 0x7C00,
                    tex: [0, 0],
                },
            ],
            31,
        );
        let red = colored_poly(
            vec![
                ScreenVertex {
                    screen_x: 10 << 8,
                    screen_y: 10 << 8,
                    depth_z: 0,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 50 << 8,
                    screen_y: 10 << 8,
                    depth_z: 0,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 30 << 8,
                    screen_y: 30 << 8,
                    depth_z: 0,
                    w: 4096,
                    color: 0x001F,
                    tex: [0, 0],
                },
            ],
            16,
        );

        r.render_frame(&[blue, red], None);

        let idx = (15 * FB_WIDTH) + 30;
        let color = r.framebuffer[idx] & 0x7FFF;
        assert_eq!(color & 0x1F, 16);
        assert_eq!((color >> 10) & 0x1F, 14);
    }

    #[test]
    fn test_same_id_translucent_overlap_does_not_blend_twice() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 3;
        r.set_swap_attrs(1);

        let mut base = colored_poly_with_id(
            vec![sv(10, 10, 0x7C00), sv(50, 10, 0x7C00), sv(30, 30, 0x7C00)],
            31,
            1,
        );
        for v in &mut base.vertices {
            v.depth_z = 2048;
        }
        let red = colored_poly_with_id(
            vec![sv(10, 10, 0x001F), sv(50, 10, 0x001F), sv(30, 30, 0x001F)],
            16,
            7,
        );

        r.render_frame(&[base, red.clone(), red], None);

        let idx = (15 * FB_WIDTH) + 30;
        let expected_once = alpha_blend(0x001F | (1 << 15), 0x7C00 | (1 << 15), 16);
        assert_eq!(r.framebuffer[idx] & 0x7FFF, expected_once);
    }

    #[test]
    fn test_different_id_translucent_overlap_can_blend_twice() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 3;
        r.set_swap_attrs(1);

        let mut base = colored_poly_with_id(
            vec![sv(10, 10, 0x7C00), sv(50, 10, 0x7C00), sv(30, 30, 0x7C00)],
            31,
            1,
        );
        for v in &mut base.vertices {
            v.depth_z = 2048;
        }
        let red_7 = colored_poly_with_id(
            vec![sv(10, 10, 0x001F), sv(50, 10, 0x001F), sv(30, 30, 0x001F)],
            16,
            7,
        );
        let red_8 = colored_poly_with_id(
            vec![sv(10, 10, 0x001F), sv(50, 10, 0x001F), sv(30, 30, 0x001F)],
            16,
            8,
        );

        r.render_frame(&[base, red_7, red_8], None);

        let idx = (15 * FB_WIDTH) + 30;
        let once = alpha_blend(0x001F | (1 << 15), 0x7C00 | (1 << 15), 16) | (1 << 15);
        let expected_twice = alpha_blend(0x001F | (1 << 15), once, 16);
        assert_eq!(r.framebuffer[idx] & 0x7FFF, expected_twice);
    }

    #[test]
    fn test_alpha_test_requires_alpha_greater_than_ref() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 2;
        r.alpha_test_ref = 16;
        let rejected = colored_poly(
            vec![sv(10, 10, 0x001F), sv(50, 10, 0x001F), sv(30, 30, 0x001F)],
            16,
        );

        r.render_frame(&[rejected], None);

        let idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[idx] & (1 << 15), 0);

        let accepted = colored_poly(
            vec![sv(10, 10, 0x001F), sv(50, 10, 0x001F), sv(30, 30, 0x001F)],
            17,
        );
        r.render_frame(&[accepted], None);

        assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x001F);
        assert_eq!(r.framebuffer[idx] & (1 << 15), 1 << 15);
    }

    #[test]
    fn test_texture_blend_alpha_modes_match_hardware_formula() {
        // Modulation and toon/highlight expand 5-bit alpha to 6-bit, multiply,
        // then shrink back. A naive 5-bit product would produce 8 here.
        assert_eq!(final_alpha(0, 16, 16), 9);
        assert_eq!(final_alpha(2, 16, 16), 9);
        assert_eq!(final_alpha(0, 31, 31), 31);
        assert_eq!(final_alpha(0, 1, 1), 0);

        // Decal and shadow output alpha come from POLYGON_ATTR.
        assert_eq!(final_alpha(1, 0, 16), 16);
        assert_eq!(final_alpha(3, 0, 16), 16);
    }

    #[test]
    fn test_translucent_texture_pixel_is_not_edge_mark_eligible() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1; // texture mapping enabled.
        let vram = vram_with_a5i3_translucent_red_texture();
        let mut p = textured_poly(vec![
            sv(10, 10, 0x7FFF),
            sv(50, 10, 0x7FFF),
            sv(30, 30, 0x7FFF),
        ]);
        p.tex_image_param = 6 << 26; // A5I3, 8x8, image offset 0.

        r.render_frame(&[p], Some(&vram));

        let idx = (15 * FB_WIDTH) + 30;
        assert_ne!(r.framebuffer[idx] & (1 << 15), 0);
        assert_eq!(r.edge_enable_buffer[idx], 0);
    }

    #[test]
    fn test_translucent_overlay_preserves_opaque_edge_mark_flag() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = (1 << 3) | (1 << 5); // alpha blend + edge marking.
        r.edge_color[0] = 0x03E0;

        let opaque = colored_poly_with_id(
            vec![
                sv(50, 50, 0x7C00),
                sv(200, 50, 0x7C00),
                sv(125, 150, 0x7C00),
            ],
            31,
            0,
        );
        let translucent = colored_poly_with_id(
            vec![
                sv(50, 50, 0x001F),
                sv(200, 50, 0x001F),
                sv(125, 150, 0x001F),
            ],
            16,
            0,
        );

        r.render_frame(&[opaque, translucent], None);

        for x in 0..FB_WIDTH {
            let idx = 100 * FB_WIDTH + x;
            if r.framebuffer[idx] & (1 << 15) != 0 {
                assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x03E0);
                return;
            }
        }
        panic!("expected an edge-marked scanline pixel");
    }

    #[test]
    fn test_translucent_pixels_and_fog_flag_with_framebuffer() {
        let mut r = Rasterizer::new();
        let mut base = colored_poly(
            vec![sv(10, 10, 0x7C00), sv(50, 10, 0x7C00), sv(30, 30, 0x7C00)],
            31,
        );
        base.attr &= !(1 << 15);
        for v in &mut base.vertices {
            v.depth_z = 1024;
        }
        let mut overlay = colored_poly(
            vec![sv(10, 10, 0x001F), sv(50, 10, 0x001F), sv(30, 30, 0x001F)],
            16,
        );
        overlay.attr |= 1 << 15;
        for v in &mut overlay.vertices {
            v.depth_z = -1024;
        }

        r.render_frame(&[base, overlay], None);

        let idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.fog_enable_buffer[idx], 0);

        let mut r = Rasterizer::new();
        let mut base = colored_poly(
            vec![sv(10, 10, 0x7C00), sv(50, 10, 0x7C00), sv(30, 30, 0x7C00)],
            31,
        );
        base.attr |= 1 << 15;
        for v in &mut base.vertices {
            v.depth_z = 1024;
        }
        let mut overlay = colored_poly(
            vec![sv(10, 10, 0x001F), sv(50, 10, 0x001F), sv(30, 30, 0x001F)],
            16,
        );
        overlay.attr &= !(1 << 15);
        for v in &mut overlay.vertices {
            v.depth_z = -1024;
        }

        r.render_frame(&[base, overlay], None);

        assert_eq!(r.fog_enable_buffer[idx], 0);
    }

    #[test]
    fn test_translucent_zero_dot_ands_existing_fog_flag() {
        let mut r = Rasterizer::new();
        let idx = (20 * FB_WIDTH) + 20;
        r.clear_color = (1 << 15) | (0x1F << 16);
        r.clear();
        assert_eq!(r.fog_enable_buffer[idx], 1);

        let mut dot = colored_poly_with_id(
            vec![sv(20, 20, 0x001F), sv(20, 20, 0x001F), sv(20, 20, 0x001F)],
            16,
            1,
        );
        dot.attr &= !(1 << 15);

        rasterize_polygon(&dot, &mut r, None);

        assert_eq!(r.fog_enable_buffer[idx], 0);
    }

    #[test]
    fn test_translucent_line_ands_existing_fog_flag() {
        let mut r = Rasterizer::new();
        let idx = (20 * FB_WIDTH) + 20;
        r.clear_color = (1 << 15) | (0x1F << 16);
        r.clear();
        assert_eq!(r.fog_enable_buffer[idx], 1);

        let mut line = colored_poly_with_id(
            vec![sv(20, 20, 0x001F), sv(30, 20, 0x001F), sv(20, 20, 0x001F)],
            16,
            1,
        );
        line.attr &= !(1 << 15);

        rasterize_polygon(&line, &mut r, None);

        assert_eq!(r.fog_enable_buffer[idx], 0);
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
    fn test_disp3dcnt_bit0_disables_texture_sampling() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 0;
        let vram = vram_with_direct_red_texture();
        let p = textured_poly(vec![
            sv(10, 10, 0x7FFF),
            sv(50, 10, 0x7FFF),
            sv(30, 30, 0x7FFF),
        ]);

        r.render_frame(&[p], Some(&vram));

        let idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x7FFF);
    }

    #[test]
    fn test_disp3dcnt_bit0_enables_texture_sampling() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1;
        let vram = vram_with_direct_red_texture();
        let p = textured_poly(vec![
            sv(10, 10, 0x7FFF),
            sv(50, 10, 0x7FFF),
            sv(30, 30, 0x7FFF),
        ]);

        r.render_frame(&[p], Some(&vram));

        let idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x001F);
    }

    #[test]
    fn test_polygon_attr_with_no_surfaces_hides_polygon() {
        let mut r = Rasterizer::new();
        let mut p = poly(vec![
            sv(10, 10, 0x001F),
            sv(50, 10, 0x001F),
            sv(30, 30, 0x001F),
        ]);
        p.attr &= !((1 << 6) | (1 << 7));

        r.render_frame(&[p], None);

        let idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[idx] & (1 << 15), 0);
    }

    #[test]
    fn test_triangle_strip_inverted_facing_rule_renders_front_surface() {
        let mut r = Rasterizer::new();
        let mut p = poly(vec![
            sv(10, 10, 0x001F),
            sv(20, 10, 0x001F),
            sv(10, 20, 0x001F),
        ]);
        p.attr &= !(1 << 6); // front surface only
        p.front_area_negative = false;

        r.render_frame(&[p], None);

        let idx = 12 * FB_WIDTH + 12;
        assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x001F);
    }

    #[test]
    fn test_polygon_attr_alpha_zero_draws_wireframe() {
        let mut r = Rasterizer::new();
        let mut p = poly(vec![
            sv(10, 10, 0x001F),
            sv(50, 10, 0x001F),
            sv(30, 30, 0x001F),
        ]);
        p.attr &= !(0x1F << 16);

        r.render_frame(&[p], None);

        let edge_idx = (10 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[edge_idx] & 0x7FFF, 0x001F);
        assert_eq!(r.framebuffer[edge_idx] & (1 << 15), 1 << 15);

        let inner_idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[inner_idx] & (1 << 15), 0);
    }

    #[test]
    fn test_wireframe_a5i3_alpha_zero_texels_skip_pixels() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1; // texture mapping enabled.
        let vram = vram_with_a5i3_red_texture_alpha(0);
        let mut p = poly(vec![
            sv(10, 10, 0x7FFF),
            sv(50, 10, 0x7FFF),
            sv(30, 30, 0x7FFF),
        ]);
        p.attr &= !(0x1F << 16); // alpha=0 selects wireframe.
        p.tex_image_param = 6 << 26; // A5I3, 8x8, image offset 0.

        r.render_frame(&[p], Some(&vram));

        let edge_idx = (10 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[edge_idx] & (1 << 15), 0);
    }

    #[test]
    fn test_wireframe_a5i3_translucent_texels_are_not_edge_marked() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1; // texture mapping enabled.
        let vram = vram_with_a5i3_translucent_red_texture();
        let mut p = poly(vec![
            sv(10, 10, 0x7FFF),
            sv(50, 10, 0x7FFF),
            sv(30, 30, 0x7FFF),
        ]);
        p.attr &= !(0x1F << 16); // alpha=0 selects wireframe.
        p.tex_image_param = 6 << 26; // A5I3, 8x8, image offset 0.

        r.render_frame(&[p], Some(&vram));

        let edge_idx = (10 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[edge_idx] & 0x7FFF, 0x001F);
        assert_eq!(r.framebuffer[edge_idx] & (1 << 15), 1 << 15);
        assert_eq!(r.edge_enable_buffer[edge_idx], 0);
    }

    #[test]
    fn test_degenerate_triangle_draws_line_segment() {
        let mut r = Rasterizer::new();
        let mut p = poly(vec![
            sv(10, 10, 0x001F),
            sv(50, 10, 0x001F),
            sv(10, 10, 0x001F),
        ]);
        p.attr &= !(1 << 6); // Hide back side; line segments ignore facing.

        r.render_frame(&[p], None);

        let line_idx = (10 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[line_idx] & 0x7FFF, 0x001F);
        assert_eq!(r.framebuffer[line_idx] & (1 << 15), 1 << 15);

        let off_line_idx = (11 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[off_line_idx] & (1 << 15), 0);
    }

    #[test]
    fn test_degenerate_line_ignores_front_back_surface_bits() {
        let mut r = Rasterizer::new();
        let mut p = poly(vec![
            sv(10, 10, 0x001F),
            sv(50, 10, 0x001F),
            sv(10, 10, 0x001F),
        ]);
        p.attr &= !((1 << 6) | (1 << 7));

        r.render_frame(&[p], None);

        let line_idx = 10 * FB_WIDTH + 30;
        assert_eq!(r.framebuffer[line_idx] & 0x7FFF, 0x001F);
        assert_eq!(r.framebuffer[line_idx] & (1 << 15), 1 << 15);
    }

    #[test]
    fn test_zero_dot_polygon_draws_first_vertex_pixel() {
        let mut r = Rasterizer::new();
        let p = poly(vec![
            sv(10, 10, 0x001F),
            sv(10, 10, 0x03E0),
            sv(10, 10, 0x7C00),
        ]);

        r.render_frame(&[p], None);

        let idx = (10 * FB_WIDTH) + 10;
        assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x001F);
        assert_eq!(r.framebuffer[idx] & (1 << 15), 1 << 15);
    }

    #[test]
    fn test_zero_dot_polygon_uses_translucent_alpha() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 3;
        let mut base = colored_poly_with_id(
            vec![sv(10, 10, 0x7C00), sv(50, 10, 0x7C00), sv(30, 30, 0x7C00)],
            31,
            1,
        );
        for v in &mut base.vertices {
            v.depth_z = 2048;
        }
        let point = colored_poly_with_id(
            vec![sv(30, 15, 0x001F), sv(30, 15, 0x03E0), sv(30, 15, 0x7C00)],
            16,
            2,
        );

        r.render_frame(&[base, point], None);

        let idx = (15 * FB_WIDTH) + 30;
        let expected = alpha_blend(0x001F | (1 << 15), 0x7C00 | (1 << 15), 16);
        assert_eq!(r.framebuffer[idx] & 0x7FFF, expected);
        assert_eq!(r.edge_enable_buffer[idx], 0);
    }

    #[test]
    fn test_zero_dot_polygon_respects_alpha_test() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 2;
        r.alpha_test_ref = 16;
        let point = colored_poly_with_id(
            vec![sv(30, 15, 0x001F), sv(30, 15, 0x03E0), sv(30, 15, 0x7C00)],
            16,
            2,
        );

        r.render_frame(&[point], None);

        let idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[idx] & (1 << 15), 0);
    }

    #[test]
    fn test_shadow_mask_polygon_does_not_write_color() {
        let mut r = Rasterizer::new();
        let p = shadow_poly(
            vec![sv(10, 10, 0x0000), sv(50, 10, 0x0000), sv(30, 30, 0x0000)],
            0,
        );

        r.render_frame(&[p], None);

        let idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[idx] & (1 << 15), 0);
        assert_eq!(r.shadow_stencil[idx], 1);
    }

    #[test]
    fn test_visible_shadow_draws_only_where_mask_is_clear() {
        let mut r = Rasterizer::new();
        r.set_swap_attrs(1);
        let mut base = colored_poly(
            vec![sv(10, 10, 0x7C00), sv(50, 10, 0x7C00), sv(30, 30, 0x7C00)],
            31,
        );
        for v in &mut base.vertices {
            v.depth_z = 1024;
        }
        let mut mask = shadow_poly(
            vec![sv(10, 10, 0x0000), sv(50, 10, 0x0000), sv(30, 30, 0x0000)],
            0,
        );
        let mut visible = shadow_poly(
            vec![sv(10, 10, 0x0000), sv(50, 10, 0x0000), sv(30, 30, 0x0000)],
            1,
        );
        for v in &mut visible.vertices {
            v.depth_z = 0;
        }
        for v in &mut mask.vertices {
            v.depth_z = 0;
        }

        r.render_frame(&[base.clone(), visible.clone()], None);

        let idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x0000);

        r.render_frame(&[base, mask, visible], None);

        assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x7C00);
        assert_eq!(r.shadow_stencil[idx], 0);
    }

    #[test]
    fn test_toon_mode_uses_vertex_red_as_table_index() {
        let mut r = Rasterizer::new();
        r.toon_table[4] = 0x001F;
        let p = toon_poly(vec![
            sv(10, 10, 0x0004),
            sv(50, 10, 0x0004),
            sv(30, 30, 0x0004),
        ]);

        r.render_frame(&[p], None);

        let idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x001F);
    }

    #[test]
    fn test_highlight_mode_adds_toon_color_offset() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 1;
        r.toon_table[4] = 0x0010;
        let p = toon_poly(vec![
            sv(10, 10, 0x0004),
            sv(50, 10, 0x0004),
            sv(30, 30, 0x0004),
        ]);

        r.render_frame(&[p], None);

        let idx = (15 * FB_WIDTH) + 30;
        assert!((r.framebuffer[idx] & 0x1F) > 0x10);
    }

    #[test]
    fn test_low_vertex_color_interpolation_preserves_constant_channel() {
        let mut r = Rasterizer::new();
        let p = poly(vec![
            sv(10, 10, 0x0004),
            sv(50, 10, 0x0004),
            sv(30, 30, 0x0004),
        ]);

        r.render_frame(&[p], None);

        let idx = (15 * FB_WIDTH) + 30;
        assert_eq!(r.framebuffer[idx] & 0x1F, 4);
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
