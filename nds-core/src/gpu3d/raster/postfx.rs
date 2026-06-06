//! Post-rasterization passes.
//!
//! All four are independent — each runs as a separate pass over the
//! framebuffer + auxiliary buffers, gated by its own `DISP3DCNT` bit. We
//! apply them in the order: fog → edge mark → toon/highlight →
//! anti-aliasing. The order matters for edge-marking: edge colors replace
//! polygon colors after fog has already been applied to polygon pixels.
//!
//! Anti-aliasing is approximated as an edge-only post-pass. Scan conversion
//! records clipped pixel coverage and edge direction hints for opaque polygon
//! edges; the post-pass uses those hints to soften silhouettes and keep the
//! edge-mark interaction enabled.

use super::{
    Rasterizer, AA_EDGE_DOWN, AA_EDGE_LEFT, AA_EDGE_RIGHT, AA_EDGE_UP, DEPTH_MAX, FB_HEIGHT,
    FB_WIDTH,
};

/// Run every enabled post-effect over the framebuffer.
pub fn apply(rast: &mut Rasterizer) {
    if rast.disp3dcnt & (1 << 7) != 0 {
        apply_fog(rast);
    }

    if rast.disp3dcnt & (1 << 5) != 0 {
        apply_edge_marking(rast);
    }

    let toon_or_highlight_mode = rast.disp3dcnt & (1 << 1) != 0;
    // Toon/highlight is a per-polygon decision — see POLYGON_ATTR.mode = 2.
    // We can't tell from the post-pass which pixels came from toon-mode
    // polygons without tagging the pixel during rasterization. Phase 7
    // part 2: toon table is consulted in the rasterizer's combine step
    // instead. The toggle bit here selects mode for those pixels.
    let _ = toon_or_highlight_mode;

    if rast.disp3dcnt & (1 << 4) != 0 && !rast.debug_disable_antialiasing {
        apply_antialiasing(rast);
    }
}

/// Edge marking: for each opaque/wireframe pixel, compare its polygon ID
/// and depth to its 4 neighbors. A neighbor can produce an edge only when
/// it is a different polygon/background and the center pixel is closer.
/// The center pixel is then tinted with the corresponding entry from
/// `EDGE_COLOR` (indexed by `poly_id >> 3`).
fn apply_edge_marking(rast: &mut Rasterizer) {
    // Snapshot both buffers so we compare against the original values.
    let ids = rast.id_buffer.clone();
    let fb_snapshot = rast.framebuffer.clone();
    let depths = rast.depth_buffer.clone();
    let edge_enabled = rast.edge_enable_buffer.clone();
    let edge_color = rast.edge_color;

    for y in 0..FB_HEIGHT {
        for x in 0..FB_WIDTH {
            let idx = y * FB_WIDTH + x;
            if fb_snapshot[idx] & (1 << 15) == 0 {
                continue;
            }
            if edge_enabled[idx] == 0 {
                continue;
            }
            let center = ids[idx];
            let center_depth = depths[idx];

            // The attribute/depth buffers are initialized from the rear-plane.
            // Transparent rear-plane color still has polygon ID/depth state,
            // and GBATEK notes screen-border edge checks respect that ID.
            let neighbor_diff = |nx: isize, ny: isize| -> bool {
                if nx < 0 || nx >= FB_WIDTH as isize || ny < 0 || ny >= FB_HEIGHT as isize {
                    let clear_id = ((rast.clear_color >> 24) & 0x3F) as u8;
                    return center != clear_id && center_depth < DEPTH_MAX;
                }
                let nidx = (ny as usize) * FB_WIDTH + (nx as usize);
                let different = ids[nidx] != center;
                let neighbor_depth = depths[nidx];
                different && center_depth < neighbor_depth
            };

            let is_edge = neighbor_diff(x as isize - 1, y as isize)
                || neighbor_diff(x as isize + 1, y as isize)
                || neighbor_diff(x as isize, y as isize - 1)
                || neighbor_diff(x as isize, y as isize + 1);

            if is_edge {
                let edge_idx = (center >> 3) as usize;
                let new_color = edge_color[edge_idx] & 0x7FFF;
                rast.framebuffer[idx] = new_color | (1 << 15);
            }
        }
    }
}

/// Fog: blend each pixel toward `FOG_COLOR` by the density value derived from
/// `FOG_OFFSET`, `DISP3DCNT[11:8]`, and `FOG_TABLE`.
fn apply_fog(rast: &mut Rasterizer) {
    let shift = ((rast.disp3dcnt >> 8) & 0xF) as u32;
    let fog_color_packed = rast.fog_color & 0x7FFF;
    let fog_alpha = ((rast.fog_color >> 16) & 0x1F) as u32;
    let alpha_only = rast.disp3dcnt & (1 << 6) != 0;
    let fog_offset = rast.fog_offset;
    let fog_table = rast.fog_table;

    let fr = (fog_color_packed & 0x1F) as u32;
    let fg = ((fog_color_packed >> 5) & 0x1F) as u32;
    let fb = ((fog_color_packed >> 10) & 0x1F) as u32;

    for i in 0..rast.framebuffer.len() {
        if rast.framebuffer[i] & (1 << 15) == 0 {
            continue;
        }
        if rast.fog_enable_buffer[i] == 0 {
            continue;
        }

        let depth = rast.depth_buffer[i].max(0);
        let density = fog_density_for_depth(depth, fog_offset, shift, &fog_table);
        let effective_fog_alpha = if fog_alpha_glitch_uses_full_alpha(depth, fog_offset, shift) {
            31
        } else {
            fog_alpha
        };

        let pixel = rast.framebuffer[i];
        let pr = (pixel & 0x1F) as u32;
        let pg = ((pixel >> 5) & 0x1F) as u32;
        let pb = ((pixel >> 10) & 0x1F) as u32;

        // density is 0..127; we use 128 as the denominator.
        let blend = |p: u32, f: u32| -> u32 { (p * (128 - density) + f * density) / 128 };

        let pa = rast.alpha_buffer[i] as u32;
        let na = blend(pa, effective_fog_alpha).min(31) as u8;
        rast.alpha_buffer[i] = na;
        if na == 0 {
            rast.framebuffer[i] &= !(1 << 15);
        } else {
            rast.framebuffer[i] |= 1 << 15;
        }

        if alpha_only {
            let _ = (pr, pg, pb, fr, fg, fb);
        } else {
            let nr = blend(pr, fr).min(31);
            let ng = blend(pg, fg).min(31);
            let nb = blend(pb, fb).min(31);
            let alpha_bit = if na != 0 { 1 << 15 } else { 0 };
            rast.framebuffer[i] =
                (nr as u16) | ((ng as u16) << 5) | ((nb as u16) << 10) | alpha_bit;
        }
    }
}

/// Approximate DS anti-aliasing: soften opaque silhouette pixels toward the
/// color across the exposed edge. The rasterizer records coverage and edge
/// direction hints when available; neighborhood ID/depth tests provide the
/// fallback exposure check.
fn apply_antialiasing(rast: &mut Rasterizer) {
    let fb_snapshot = rast.framebuffer.clone();
    let ids = rast.id_buffer.clone();
    let depths = rast.depth_buffer.clone();
    let edge_enabled = rast.edge_enable_buffer.clone();
    let zero_dot = rast.zero_dot_buffer.clone();
    let rear_colors = rast.rear_color_buffer.clone();
    let aa_coverage = rast.aa_coverage_buffer.clone();
    let aa_edge_hint = rast.aa_edge_hint_buffer.clone();
    let edge_marking = rast.disp3dcnt & (1 << 5) != 0;

    for y in 0..FB_HEIGHT {
        for x in 0..FB_WIDTH {
            let idx = y * FB_WIDTH + x;
            if fb_snapshot[idx] & (1 << 15) == 0 || edge_enabled[idx] == 0 {
                continue;
            }
            if zero_dot[idx] != 0 && !edge_marking {
                rast.framebuffer[idx] &= !(1 << 15);
                rast.alpha_buffer[idx] = 0;
                continue;
            }
            let center = ids[idx];
            let center_depth = depths[idx];

            let exposed_neighbor = |nx: isize, ny: isize| -> Option<(u16, bool)> {
                if nx < 0 || nx >= FB_WIDTH as isize || ny < 0 || ny >= FB_HEIGHT as isize {
                    let clear_id = ((rast.clear_color >> 24) & 0x3F) as u8;
                    return (center != clear_id && center_depth < DEPTH_MAX).then(|| {
                        let rear = rear_colors[idx];
                        (rear & 0x7FFF, rear & (1 << 15) != 0)
                    });
                }
                let nidx = (ny as usize) * FB_WIDTH + (nx as usize);
                if ids[nidx] == center || center_depth >= depths[nidx] {
                    return None;
                }
                let (neighbor_color, preblend) = if fb_snapshot[nidx] & (1 << 15) != 0 {
                    (fb_snapshot[nidx] & 0x7FFF, true)
                } else {
                    let rear = rear_colors[nidx];
                    (rear & 0x7FFF, rear & (1 << 15) != 0)
                };
                Some((neighbor_color, preblend))
            };

            let hints = aa_edge_hint[idx];
            let hinted_neighbor = (hints & AA_EDGE_LEFT != 0)
                .then(|| exposed_neighbor(x as isize - 1, y as isize))
                .flatten()
                .or_else(|| {
                    (hints & AA_EDGE_RIGHT != 0)
                        .then(|| exposed_neighbor(x as isize + 1, y as isize))
                        .flatten()
                })
                .or_else(|| {
                    (hints & AA_EDGE_UP != 0)
                        .then(|| exposed_neighbor(x as isize, y as isize - 1))
                        .flatten()
                })
                .or_else(|| {
                    (hints & AA_EDGE_DOWN != 0)
                        .then(|| exposed_neighbor(x as isize, y as isize + 1))
                        .flatten()
                });

            let blend_target = hinted_neighbor
                .or_else(|| exposed_neighbor(x as isize - 1, y as isize))
                .or_else(|| exposed_neighbor(x as isize + 1, y as isize))
                .or_else(|| exposed_neighbor(x as isize, y as isize - 1))
                .or_else(|| exposed_neighbor(x as isize, y as isize + 1));

            if let Some((target, preblend)) = blend_target {
                let coverage = if aa_coverage[idx] != 0 {
                    aa_coverage[idx].min(31)
                } else {
                    16
                };
                if preblend {
                    rast.framebuffer[idx] =
                        alpha_blend_bgr555(fb_snapshot[idx] & 0x7FFF, target, coverage) | (1 << 15);
                } else {
                    rast.framebuffer[idx] = fb_snapshot[idx];
                }
                rast.alpha_buffer[idx] = coverage;
            } else if zero_dot[idx] != 0 {
                rast.framebuffer[idx] &= !(1 << 15);
                rast.alpha_buffer[idx] = 0;
            }
        }
    }
}

#[inline]
fn fog_density_for_blend(value: u8) -> u32 {
    if value & 0x7F == 0x7F {
        128
    } else {
        (value & 0x7F) as u32
    }
}

fn fog_density_for_depth(depth: i32, fog_offset: u16, shift: u32, table: &[u8; 32]) -> u32 {
    let depth = fog_depth_units(depth);
    let offset = (fog_offset & 0x7FFF) as i32;
    let step = if shift <= 10 { 0x400 >> shift } else { 0 };

    if step == 0 {
        return if depth <= offset {
            fog_density_for_blend(table[0])
        } else {
            fog_density_for_blend(table[31])
        };
    }

    let first_boundary = offset + step;
    let last_boundary = offset + step * 32;
    if depth <= first_boundary {
        return fog_density_for_blend(table[0]);
    }
    if depth >= last_boundary {
        return fog_density_for_blend(table[31]);
    }

    let relative = depth - first_boundary;
    let idx = (relative / step) as usize;
    let frac = relative % step;
    let d0 = fog_density_for_blend(table[idx]) as i32;
    let d1 = fog_density_for_blend(table[idx + 1]) as i32;
    (d0 + ((d1 - d0) * frac) / step).clamp(0, 128) as u32
}

fn fog_depth_units(depth: i32) -> i32 {
    (depth.max(0) >> 9).min(0x7FFF)
}

fn fog_alpha_glitch_uses_full_alpha(depth: i32, fog_offset: u16, shift: u32) -> bool {
    let depth = fog_depth_units(depth);
    let offset = (fog_offset & 0x7FFF) as i32;
    let step = if shift <= 10 { 0x400 >> shift } else { 0 };
    let first_boundary = offset + step;
    depth <= first_boundary
}

fn alpha_blend_bgr555(top: u16, bottom: u16, alpha: u8) -> u16 {
    let a = alpha as u32;
    let inv = 31 - a;
    let blend = |shift: u32| -> u16 {
        let t = ((top >> shift) & 0x1F) as u32;
        let b = ((bottom >> shift) & 0x1F) as u32;
        (((t * (a + 1)) + (b * inv)) / 32).min(31) as u16
    };
    blend(0) | (blend(5) << 5) | (blend(10) << 10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu3d::viewport::{ScreenPolygon, ScreenVertex};

    fn sv(x: i32, y: i32, color: u16) -> ScreenVertex {
        ScreenVertex {
            screen_x: x << 8,
            screen_y: y << 8,
            depth_z: 0,
            w: 4096,
            color,
            tex: [0, 0],
        }
    }

    fn make_poly(verts: Vec<ScreenVertex>, poly_id: u8) -> ScreenPolygon {
        ScreenPolygon {
            vertices: verts,
            attr: (0x1F << 16) | (1 << 6) | (1 << 7) | ((poly_id as u32) << 24),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        }
    }

    fn make_poly_alpha(verts: Vec<ScreenVertex>, poly_id: u8, alpha: u8) -> ScreenPolygon {
        ScreenPolygon {
            vertices: verts,
            attr: ((alpha as u32) << 16) | (1 << 6) | (1 << 7) | ((poly_id as u32) << 24),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        }
    }

    fn fog_poly(verts: Vec<ScreenVertex>, fog_enabled: bool) -> ScreenPolygon {
        ScreenPolygon {
            vertices: verts,
            attr: (0x1F << 16) | (1 << 6) | (1 << 7) | if fog_enabled { 1 << 15 } else { 0 },
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        }
    }

    #[test]
    fn test_edge_marking_outlines_a_single_polygon() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 | (1 << 5); // 3D enable + edge mark
        r.clear_color = 1 << 24; // rear-plane polygon ID differs from test polygon.
                                 // Edge color group 0 = red.
        r.edge_color[0] = 0x001F;

        // Single red-filled polygon with poly_id = 0.
        let p = make_poly(
            vec![
                sv(50, 50, 0x7C00), // blue interior
                sv(200, 50, 0x7C00),
                sv(125, 150, 0x7C00),
            ],
            0,
        );
        r.render_frame(&[p], None);

        // A pixel on the polygon's boundary should now be RED (from
        // edge color), not blue.
        // The triangle's bottom-tip pixel is roughly at (125, 149).
        // Check the leftmost pixel of scanline 100 — should be edge.
        let mut found_red_edge = false;
        for x in 0..FB_WIDTH {
            let idx = 100 * FB_WIDTH + x;
            if r.framebuffer[idx] & (1 << 15) != 0 {
                // First written pixel on this scanline = left edge.
                let c = r.framebuffer[idx];
                if (c & 0x1F) >= 30 && ((c >> 10) & 0x1F) < 5 {
                    found_red_edge = true;
                }
                break;
            }
        }
        assert!(
            found_red_edge,
            "expected left edge of triangle to be tinted red"
        );
    }

    #[test]
    fn test_edge_marking_color_is_not_fogged() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = (1 << 5) | (1 << 7);
        r.clear_color = 1 << 24; // rear-plane polygon ID differs from test polygon.
        r.edge_color[0] = 0x001F;
        r.fog_color = 0x1F << 16; // opaque black fog.
        for d in r.fog_table.iter_mut() {
            *d = 127;
        }

        let mut p = make_poly(
            vec![
                sv(50, 50, 0x7FFF),
                sv(200, 50, 0x7FFF),
                sv(125, 150, 0x7FFF),
            ],
            0,
        );
        p.attr |= 1 << 15;
        r.render_frame(&[p], None);

        for x in 0..FB_WIDTH {
            let idx = 100 * FB_WIDTH + x;
            if r.framebuffer[idx] & (1 << 15) != 0 {
                assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x001F);
                return;
            }
        }
        panic!("expected an edge-marked pixel");
    }

    #[test]
    fn test_edge_marking_keeps_fogged_alpha() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = (1 << 5) | (1 << 7);
        r.edge_color[0] = 0x7C00;
        r.fog_color = (16 << 16) | 0x001F;
        for d in r.fog_table.iter_mut() {
            *d = 127;
        }

        let center = 20 * FB_WIDTH + 20;
        let right = 20 * FB_WIDTH + 21;
        r.framebuffer[center] = 0x03E0 | (1 << 15);
        r.alpha_buffer[center] = 31;
        r.id_buffer[center] = 0;
        r.depth_buffer[center] = 0x1000 << 9;
        r.edge_enable_buffer[center] = 1;
        r.fog_enable_buffer[center] = 1;

        r.framebuffer[right] = 0x7FFF | (1 << 15);
        r.alpha_buffer[right] = 31;
        r.id_buffer[right] = 1;
        r.depth_buffer[right] = 0x2000 << 9;

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            0x7C00,
            "edge marking should replace the fogged color with EDGE_COLOR"
        );
        assert_eq!(
            r.alpha_buffer[center], 16,
            "fogged alpha should survive the later edge-mark color replacement"
        );
    }

    #[test]
    fn test_edge_marking_skips_translucent_polygons() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 5;
        r.edge_color[0] = 0x001F;

        let p = make_poly_alpha(
            vec![
                sv(50, 50, 0x7C00),
                sv(200, 50, 0x7C00),
                sv(125, 150, 0x7C00),
            ],
            0,
            16,
        );
        r.render_frame(&[p], None);

        for x in 0..FB_WIDTH {
            let idx = 100 * FB_WIDTH + x;
            if r.framebuffer[idx] & (1 << 15) != 0 {
                assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x7C00);
                return;
            }
        }
        panic!("expected translucent polygon to write a scanline pixel");
    }

    #[test]
    fn test_edge_marking_requires_center_closer_than_neighbor() {
        fn setup(right_depth: i32) -> Rasterizer {
            let mut r = Rasterizer::new();
            r.disp3dcnt = 1 << 5;
            r.edge_color[0] = 0x001F;

            let center = 20 * FB_WIDTH + 20;
            let left = 20 * FB_WIDTH + 19;
            let right = 20 * FB_WIDTH + 21;
            let up = 19 * FB_WIDTH + 20;
            let down = 21 * FB_WIDTH + 20;

            for idx in [center, left, up, down] {
                r.framebuffer[idx] = 0x7C00 | (1 << 15);
                r.id_buffer[idx] = 0;
                r.depth_buffer[idx] = 1000;
                r.edge_enable_buffer[idx] = 1;
            }

            r.framebuffer[right] = 0x03E0 | (1 << 15);
            r.id_buffer[right] = 1;
            r.depth_buffer[right] = right_depth;
            r.edge_enable_buffer[right] = 1;
            r
        }

        let center = 20 * FB_WIDTH + 20;

        let mut r = setup(500);
        apply(&mut r);
        assert_eq!(r.framebuffer[center] & 0x7FFF, 0x7C00);

        let mut r = setup(1500);
        apply(&mut r);
        assert_eq!(r.framebuffer[center] & 0x7FFF, 0x001F);
    }

    #[test]
    fn test_edge_marking_uses_current_depth_after_translucent_depth_update() {
        fn setup(center_depth: i32) -> Rasterizer {
            let mut r = Rasterizer::new();
            r.disp3dcnt = 1 << 5;
            r.edge_color[0] = 0x001F;

            let center = 20 * FB_WIDTH + 20;
            let right = 20 * FB_WIDTH + 21;
            r.framebuffer[center] = 0x7C00 | (1 << 15);
            r.id_buffer[center] = 0;
            r.depth_buffer[center] = center_depth;
            r.edge_enable_buffer[center] = 1;

            r.framebuffer[right] = 0x03E0 | (1 << 15);
            r.id_buffer[right] = 1;
            r.depth_buffer[right] = 1000;
            r.edge_enable_buffer[right] = 1;
            r
        }

        let center = 20 * FB_WIDTH + 20;

        let mut old_far_depth = setup(2000);
        apply(&mut old_far_depth);
        assert_eq!(
            old_far_depth.framebuffer[center] & 0x7FFF,
            0x7C00,
            "edge marking should reject when the flagged edge depth is behind the neighbor"
        );

        let mut updated_near_depth = setup(500);
        apply(&mut updated_near_depth);
        assert_eq!(
            updated_near_depth.framebuffer[center] & 0x7FFF,
            0x001F,
            "edge marking should use the current depth buffer after a depth-updating translucent overlay"
        );
    }

    #[test]
    fn test_edge_marking_uses_polygon_id_color_group_and_masks_bit15() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 5;
        r.edge_color[0] = 0x001F;
        r.edge_color[1] = 0xFC00; // bit 15 ignored; color is blue.

        let center = 20 * FB_WIDTH + 20;
        let right = 20 * FB_WIDTH + 21;
        r.framebuffer[center] = 0x03E0 | (1 << 15);
        r.id_buffer[center] = 8;
        r.depth_buffer[center] = 1000;
        r.edge_enable_buffer[center] = 1;
        r.framebuffer[right] = 0x7FFF | (1 << 15);
        r.id_buffer[right] = 0;
        r.depth_buffer[right] = 1500;

        apply(&mut r);

        assert_eq!(r.framebuffer[center] & 0x7FFF, 0x7C00);
        assert_eq!(r.framebuffer[center] & (1 << 15), 1 << 15);
    }

    #[test]
    fn test_edge_marking_respects_transparent_rear_plane_polygon_id() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 5;
        r.clear_color = 4 << 24; // transparent rear plane, polygon ID 4.
        r.clear_depth = 0x7FFF;
        r.edge_color[0] = 0x001F;
        r.clear();

        let center = 20 * FB_WIDTH + 20;
        r.framebuffer[center] = 0x7C00 | (1 << 15);
        r.alpha_buffer[center] = 31;
        r.id_buffer[center] = 4;
        r.depth_buffer[center] = 1000;
        r.edge_enable_buffer[center] = 1;

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            0x7C00,
            "same rear-plane polygon ID should suppress the edge"
        );
    }

    #[test]
    fn test_edge_marking_outlines_against_different_rear_plane_polygon_id() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 5;
        r.clear_color = 3 << 24; // transparent rear plane, polygon ID 3.
        r.clear_depth = 0x7FFF;
        r.edge_color[0] = 0x001F;
        r.clear();

        let center = 20 * FB_WIDTH + 20;
        r.framebuffer[center] = 0x7C00 | (1 << 15);
        r.alpha_buffer[center] = 31;
        r.id_buffer[center] = 4;
        r.depth_buffer[center] = 1000;
        r.edge_enable_buffer[center] = 1;

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            0x001F,
            "different rear-plane polygon ID should allow the edge"
        );
    }

    #[test]
    fn test_antialias_softens_opaque_silhouette_pixel() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 4;
        r.clear_color = (1 << 24) | (31 << 16) | 0x7FFF;
        r.clear();

        let center = 20 * FB_WIDTH + 20;
        let left = 20 * FB_WIDTH + 19;
        let up = 19 * FB_WIDTH + 20;
        let down = 21 * FB_WIDTH + 20;
        for idx in [center, left, up, down] {
            r.framebuffer[idx] = 0x7C00 | (1 << 15);
            r.id_buffer[idx] = 0;
            r.depth_buffer[idx] = 1000;
            r.edge_enable_buffer[idx] = 1;
        }

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            alpha_blend_bgr555(0x7C00, 0x7FFF, 16)
        );
    }

    #[test]
    fn test_antialias_skips_translucent_pixels() {
        let center = 20 * FB_WIDTH + 20;

        let mut translucent = Rasterizer::new();
        translucent.disp3dcnt = 1 << 4;
        translucent.clear_color = 0x7FFF;
        translucent.clear();
        translucent.framebuffer[center] = 0x7C00 | (1 << 15);
        translucent.depth_buffer[center] = 1000;
        translucent.id_buffer[center] = 0;
        translucent.edge_enable_buffer[center] = 0;
        apply(&mut translucent);
        assert_eq!(translucent.framebuffer[center] & 0x7FFF, 0x7C00);
    }

    #[test]
    fn test_antialias_still_runs_when_edge_marking_enabled() {
        let center = 20 * FB_WIDTH + 20;

        let mut r = Rasterizer::new();
        r.disp3dcnt = (1 << 4) | (1 << 5);
        r.clear_color = (1 << 24) | (31 << 16) | 0x7FFF;
        r.edge_color[0] = 0x001F;
        r.clear();
        r.framebuffer[center] = 0x7C00 | (1 << 15);
        r.depth_buffer[center] = 1000;
        r.id_buffer[center] = 0;
        r.edge_enable_buffer[center] = 1;

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            alpha_blend_bgr555(0x001F, 0x7FFF, 16)
        );
    }

    #[test]
    fn test_antialias_hides_zero_dot_when_edge_marking_finds_no_edge() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = (1 << 4) | (1 << 5);
        r.clear_color = 8 << 24; // same rear-plane polygon ID as the 1-dot polygon.
        let p = make_poly(
            vec![sv(10, 10, 0x001F), sv(10, 10, 0x001F), sv(10, 10, 0x001F)],
            8,
        );

        r.render_frame(&[p], None);

        let idx = 10 * FB_WIDTH + 10;
        assert_eq!(r.framebuffer[idx] & (1 << 15), 0);
        assert_eq!(r.alpha_buffer[idx], 0);
    }

    #[test]
    fn test_antialias_respects_transparent_rear_plane_polygon_id() {
        let center = 20 * FB_WIDTH + 20;

        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 4;
        r.clear_color = (4 << 24) | 0x7FFF;
        r.clear_depth = 0x7FFF;
        r.clear();
        r.framebuffer[center] = 0x7C00 | (1 << 15);
        r.depth_buffer[center] = 1000;
        r.id_buffer[center] = 4;
        r.edge_enable_buffer[center] = 1;

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            0x7C00,
            "same rear-plane polygon ID should not expose an AA silhouette"
        );
    }

    #[test]
    fn test_antialias_softens_against_different_rear_plane_polygon_id() {
        let center = 20 * FB_WIDTH + 20;

        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 4;
        r.clear_color = (3 << 24) | (31 << 16) | 0x7FFF;
        r.clear_depth = 0x7FFF;
        r.clear();
        r.framebuffer[center] = 0x7C00 | (1 << 15);
        r.depth_buffer[center] = 1000;
        r.id_buffer[center] = 4;
        r.edge_enable_buffer[center] = 1;

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            alpha_blend_bgr555(0x7C00, 0x7FFF, 16)
        );
    }

    #[test]
    fn test_antialias_blends_against_rear_plane_pixel_color() {
        let center = 20 * FB_WIDTH + 20;

        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 4;
        r.clear_color = (3 << 24) | 0x7FFF;
        r.clear();
        r.rear_color_buffer[20 * FB_WIDTH + 19] = 0x03E0 | (1 << 15);
        r.framebuffer[center] = 0x7C00 | (1 << 15);
        r.depth_buffer[center] = 1000;
        r.id_buffer[center] = 4;
        r.edge_enable_buffer[center] = 1;

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            alpha_blend_bgr555(0x7C00, 0x03E0, 16),
            "AA must blend against the per-pixel rear plane, not only CLEAR_COLOR"
        );
    }

    #[test]
    fn test_antialias_uses_rasterized_coverage_hint() {
        let center = 20 * FB_WIDTH + 20;

        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 4;
        r.clear_color = (3 << 24) | 0x7FFF;
        r.clear();
        r.rear_color_buffer[20 * FB_WIDTH + 19] = 0x03E0 | (1 << 15);
        r.framebuffer[center] = 0x7C00 | (1 << 15);
        r.depth_buffer[center] = 1000;
        r.id_buffer[center] = 4;
        r.edge_enable_buffer[center] = 1;
        r.aa_coverage_buffer[center] = 8;

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            alpha_blend_bgr555(0x7C00, 0x03E0, 8),
            "AA should prefer scan-conversion coverage over the fixed fallback"
        );
        assert_eq!(r.alpha_buffer[center], 8);
    }

    #[test]
    fn test_antialias_transparent_rear_plane_preserves_color_and_lowers_alpha() {
        let center = 20 * FB_WIDTH + 20;

        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 4;
        r.clear_color = (3 << 24) | 0x001F;
        r.clear();
        r.framebuffer[center] = 0x7C00 | (1 << 15);
        r.depth_buffer[center] = 1000;
        r.id_buffer[center] = 4;
        r.edge_enable_buffer[center] = 1;
        r.aa_coverage_buffer[center] = 8;

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            0x7C00,
            "transparent rear-plane exposure should not pre-blend 3D color against CLEAR_COLOR"
        );
        assert_eq!(r.alpha_buffer[center], 8);
    }

    #[test]
    fn test_antialias_blends_against_visible_neighbor_color() {
        let center = 20 * FB_WIDTH + 20;
        let right = 20 * FB_WIDTH + 21;

        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 4;
        r.clear_color = 0x7FFF;
        r.clear();
        r.framebuffer[center] = 0x7C00 | (1 << 15);
        r.depth_buffer[center] = 1000;
        r.id_buffer[center] = 0;
        r.edge_enable_buffer[center] = 1;

        r.framebuffer[right] = 0x03E0 | (1 << 15);
        r.depth_buffer[right] = 1500;
        r.id_buffer[right] = 1;
        r.edge_enable_buffer[right] = 1;

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            alpha_blend_bgr555(0x7C00, 0x03E0, 16),
            "AA should soften internal edges toward the visible neighbor, not the rear plane"
        );
    }

    #[test]
    fn test_antialias_prefers_rasterized_edge_direction_hint() {
        let center = 20 * FB_WIDTH + 20;
        let left = 20 * FB_WIDTH + 19;
        let up = 19 * FB_WIDTH + 20;

        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 4;
        r.clear_color = 0x7FFF;
        r.clear();
        r.framebuffer[center] = 0x001F | (1 << 15);
        r.depth_buffer[center] = 1000;
        r.id_buffer[center] = 0;
        r.edge_enable_buffer[center] = 1;
        r.aa_coverage_buffer[center] = 8;
        r.aa_edge_hint_buffer[center] = AA_EDGE_UP;

        r.framebuffer[left] = 0x03E0 | (1 << 15);
        r.depth_buffer[left] = 1500;
        r.id_buffer[left] = 1;
        r.edge_enable_buffer[left] = 1;

        r.framebuffer[up] = 0x7C00 | (1 << 15);
        r.depth_buffer[up] = 1500;
        r.id_buffer[up] = 2;
        r.edge_enable_buffer[up] = 1;

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            alpha_blend_bgr555(0x001F, 0x7C00, 8),
            "AA should use the scan-conversion edge hint before fallback scan order"
        );
    }

    #[test]
    fn test_antialias_multi_edge_hint_ignores_unhinted_neighbors() {
        let center = 20 * FB_WIDTH + 20;
        let left = 20 * FB_WIDTH + 19;
        let right = 20 * FB_WIDTH + 21;
        let up = 19 * FB_WIDTH + 20;

        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 4;
        r.clear_color = 0x7FFF;
        r.clear();
        r.framebuffer[center] = 0x001F | (1 << 15);
        r.depth_buffer[center] = 1000;
        r.id_buffer[center] = 0;
        r.edge_enable_buffer[center] = 1;
        r.aa_coverage_buffer[center] = 8;
        r.aa_edge_hint_buffer[center] = AA_EDGE_RIGHT | AA_EDGE_UP;

        r.framebuffer[left] = 0x03E0 | (1 << 15);
        r.depth_buffer[left] = 1500;
        r.id_buffer[left] = 1;
        r.edge_enable_buffer[left] = 1;

        r.framebuffer[right] = 0x7C00 | (1 << 15);
        r.depth_buffer[right] = 1500;
        r.id_buffer[right] = 2;
        r.edge_enable_buffer[right] = 1;

        r.framebuffer[up] = 0x4210 | (1 << 15);
        r.depth_buffer[up] = 1500;
        r.id_buffer[up] = 3;
        r.edge_enable_buffer[up] = 1;

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            alpha_blend_bgr555(0x001F, 0x7C00, 8),
            "AA should try hinted directions before unrelated fallback neighbors"
        );
    }

    #[test]
    fn test_antialias_keeps_same_polygon_interior_pixels_opaque() {
        let center = 20 * FB_WIDTH + 20;

        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 4;
        r.clear_color = (7 << 24) | 0x7FFF;
        r.clear();

        for (x, y) in [(20, 20), (19, 20), (21, 20), (20, 19), (20, 21)] {
            let idx = y * FB_WIDTH + x;
            r.framebuffer[idx] = 0x7C00 | (1 << 15);
            r.alpha_buffer[idx] = 31;
            r.depth_buffer[idx] = 1000;
            r.id_buffer[idx] = 3;
            r.edge_enable_buffer[idx] = 1;
        }

        apply(&mut r);

        assert_eq!(
            r.framebuffer[center] & 0x7FFF,
            0x7C00,
            "AA should not soften a pixel whose four neighbors are the same polygon"
        );
        assert_eq!(r.alpha_buffer[center], 31);
    }

    #[test]
    fn test_antialias_requires_center_closer_than_neighbor() {
        fn setup(right_depth: i32) -> Rasterizer {
            let mut r = Rasterizer::new();
            r.disp3dcnt = 1 << 4;
            r.clear_color = 0x7FFF;
            r.clear();

            let center = 20 * FB_WIDTH + 20;
            let left = 20 * FB_WIDTH + 19;
            let right = 20 * FB_WIDTH + 21;
            let up = 19 * FB_WIDTH + 20;
            let down = 21 * FB_WIDTH + 20;

            for idx in [center, left, up, down] {
                r.framebuffer[idx] = 0x7C00 | (1 << 15);
                r.alpha_buffer[idx] = 31;
                r.id_buffer[idx] = 0;
                r.depth_buffer[idx] = 1000;
                r.edge_enable_buffer[idx] = 1;
            }

            r.framebuffer[right] = 0x03E0 | (1 << 15);
            r.alpha_buffer[right] = 31;
            r.id_buffer[right] = 1;
            r.depth_buffer[right] = right_depth;
            r.edge_enable_buffer[right] = 1;
            r
        }

        let center = 20 * FB_WIDTH + 20;

        let mut closer_neighbor = setup(500);
        apply(&mut closer_neighbor);
        assert_eq!(
            closer_neighbor.framebuffer[center] & 0x7FFF,
            0x7C00,
            "AA should not soften when the different-ID neighbor is in front"
        );
        assert_eq!(closer_neighbor.alpha_buffer[center], 31);

        let mut farther_neighbor = setup(1500);
        apply(&mut farther_neighbor);
        assert_eq!(
            farther_neighbor.framebuffer[center] & 0x7FFF,
            alpha_blend_bgr555(0x7C00, 0x03E0, 16),
            "AA should soften an exposed silhouette against the farther visible neighbor"
        );
        assert_eq!(farther_neighbor.alpha_buffer[center], 16);
    }

    #[test]
    fn test_fog_darkens_distant_pixels() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 | (1 << 7); // 3D enable + fog
        r.fog_color = 0; // black fog
                         // All fog table entries = max density (127) so blending is full.
        for d in r.fog_table.iter_mut() {
            *d = 127;
        }

        // Far white triangle.
        let p = ScreenPolygon {
            vertices: vec![
                ScreenVertex {
                    screen_x: 50 << 8,
                    screen_y: 50 << 8,
                    depth_z: 4000,
                    w: 4096,
                    color: 0x7FFF,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 200 << 8,
                    screen_y: 50 << 8,
                    depth_z: 4000,
                    w: 4096,
                    color: 0x7FFF,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 125 << 8,
                    screen_y: 150 << 8,
                    depth_z: 4000,
                    w: 4096,
                    color: 0x7FFF,
                    tex: [0, 0],
                },
            ],
            attr: (0x1F << 16) | (1 << 6) | (1 << 7) | (1 << 15),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        };
        r.render_frame(&[p], None);

        // GBATEK notes density 127 is handled as full density 128.
        let center = r.framebuffer[100 * FB_WIDTH + 125];
        assert_eq!(center & 0x7FFF, 0);
    }

    #[test]
    fn test_fog_alpha_only_updates_alpha_without_color() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 | (1 << 6) | (1 << 7); // alpha-only fog + fog enable
        r.fog_color = 0 << 16;
        for d in r.fog_table.iter_mut() {
            *d = 127;
        }

        let p = fog_poly(
            vec![
                sv(50, 50, 0x7FFF),
                sv(200, 50, 0x7FFF),
                sv(125, 150, 0x7FFF),
            ],
            true,
        );
        r.render_frame(&[p], None);

        let idx = 100 * FB_WIDTH + 125;
        assert_eq!(r.framebuffer[idx] & 0x7FFF, 0x7FFF);
        assert_eq!(r.alpha_buffer[idx], 0);
        assert_eq!(r.framebuffer[idx] & (1 << 15), 0);
    }

    #[test]
    fn test_fog_color_mode_preserves_zero_alpha_transparency() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 7; // fog enable, color+alpha mode.
        r.fog_color = 0x001F; // red fog, alpha 0.
        for d in r.fog_table.iter_mut() {
            *d = 127;
        }

        let idx = 20 * FB_WIDTH + 20;
        r.framebuffer[idx] = 0x7FFF | (1 << 15);
        r.alpha_buffer[idx] = 31;
        r.depth_buffer[idx] = 0x2000 << 9; // beyond the first-boundary alpha quirk.
        r.fog_enable_buffer[idx] = 1;

        apply(&mut r);

        assert_eq!(r.alpha_buffer[idx], 0);
        assert_eq!(
            r.framebuffer[idx] & (1 << 15),
            0,
            "color+alpha fog must not restore the framebuffer alpha bit after alpha fades to zero"
        );
        assert_eq!(
            r.framebuffer[idx] & 0x7FFF,
            0x001F,
            "color channels should still fog toward FOG_COLOR"
        );
    }

    #[test]
    fn test_fog_alpha_uses_full_alpha_before_first_boundary() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = (1 << 6) | (1 << 7); // alpha-only fog + fog enable
        r.fog_color = 0 << 16; // fog alpha would normally fade to transparent
        r.fog_table[0] = 64;

        let idx = 20 * FB_WIDTH + 20;
        r.framebuffer[idx] = 0x7FFF | (1 << 15);
        r.alpha_buffer[idx] = 31;
        r.depth_buffer[idx] = 0; // before first fog boundary
        r.fog_enable_buffer[idx] = 1;

        apply(&mut r);

        assert_eq!(r.alpha_buffer[idx], 31);
        assert_eq!(r.framebuffer[idx] & (1 << 15), 1 << 15);
    }

    #[test]
    fn test_fog_density_masks_high_bit_and_maps_127_to_full() {
        assert_eq!(fog_density_for_blend(0), 0);
        assert_eq!(fog_density_for_blend(64), 64);
        assert_eq!(fog_density_for_blend(127), 128);
        assert_eq!(fog_density_for_blend(255), 128);
    }

    #[test]
    fn test_fog_density_uses_offset_step_boundaries() {
        let mut table = [0u8; 32];
        table[0] = 8;
        table[1] = 64;
        table[31] = 127;

        assert_eq!(fog_density_for_depth(0x50FF << 9, 0x5000, 2, &table), 8);
        assert_eq!(fog_density_for_depth(0x5100 << 9, 0x5000, 2, &table), 8);
        assert_eq!(fog_density_for_depth(0x5200 << 9, 0x5000, 2, &table), 64);
        assert_eq!(fog_density_for_depth(0x7000 << 9, 0x5000, 2, &table), 128);
    }

    #[test]
    fn test_fog_density_interpolates_between_boundaries() {
        let mut table = [0u8; 32];
        table[0] = 0;
        table[1] = 127;

        assert_eq!(fog_density_for_depth(0x3600 << 9, 0x3000, 0, &table), 64);
    }

    #[test]
    fn test_fog_density_handles_zero_step_shift() {
        let mut table = [0u8; 32];
        table[0] = 16;
        table[31] = 127;

        assert_eq!(fog_density_for_depth(0x3000 << 9, 0x3000, 11, &table), 16);
        assert_eq!(fog_density_for_depth(0x3001 << 9, 0x3000, 11, &table), 128);
    }

    #[test]
    fn test_fog_depth_uses_expanded_depth_buffer_units() {
        assert_eq!(fog_depth_units(0), 0);
        assert_eq!(fog_depth_units(0x3FFF << 9), 0x3FFF);
        assert_eq!(fog_depth_units(0x7FFF << 9), 0x7FFF);
        assert_eq!(fog_depth_units(0x8FFF << 9), 0x7FFF);
    }

    #[test]
    fn test_fog_disabled_leaves_color_intact() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1; // 3D enable, no fog
        for d in r.fog_table.iter_mut() {
            *d = 127;
        }

        let p = make_poly(
            vec![
                sv(50, 50, 0x7FFF),
                sv(200, 50, 0x7FFF),
                sv(125, 150, 0x7FFF),
            ],
            0,
        );
        r.render_frame(&[p], None);

        let center = r.framebuffer[100 * FB_WIDTH + 125];
        assert!(
            (center & 0x1F) >= 30,
            "fog disabled — white should remain white"
        );
    }

    #[test]
    fn test_fog_respects_polygon_attr_bit() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 | (1 << 7);
        r.fog_color = 0;
        for d in r.fog_table.iter_mut() {
            *d = 127;
        }

        let p = fog_poly(
            vec![
                ScreenVertex {
                    screen_x: 50 << 8,
                    screen_y: 50 << 8,
                    depth_z: 4000,
                    w: 4096,
                    color: 0x7FFF,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 200 << 8,
                    screen_y: 50 << 8,
                    depth_z: 4000,
                    w: 4096,
                    color: 0x7FFF,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 125 << 8,
                    screen_y: 150 << 8,
                    depth_z: 4000,
                    w: 4096,
                    color: 0x7FFF,
                    tex: [0, 0],
                },
            ],
            false,
        );
        r.render_frame(&[p], None);
        let center = r.framebuffer[100 * FB_WIDTH + 125];
        assert!(
            (center & 0x1F) >= 30,
            "polygon without fog bit should remain white"
        );

        let p = fog_poly(
            vec![
                ScreenVertex {
                    screen_x: 50 << 8,
                    screen_y: 50 << 8,
                    depth_z: 4000,
                    w: 4096,
                    color: 0x7FFF,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 200 << 8,
                    screen_y: 50 << 8,
                    depth_z: 4000,
                    w: 4096,
                    color: 0x7FFF,
                    tex: [0, 0],
                },
                ScreenVertex {
                    screen_x: 125 << 8,
                    screen_y: 150 << 8,
                    depth_z: 4000,
                    w: 4096,
                    color: 0x7FFF,
                    tex: [0, 0],
                },
            ],
            true,
        );
        r.render_frame(&[p], None);
        let center = r.framebuffer[100 * FB_WIDTH + 125];
        assert!((center & 0x1F) < 5, "polygon with fog bit should be fogged");
    }

    #[test]
    fn test_fog_uses_w_buffered_depth_when_enabled() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 | (1 << 7) | (9 << 8); // 3D enable + fog, FOG_STEP=2.
        r.w_buffering = true;
        r.fog_color = 0;
        r.fog_table[0] = 0;
        r.fog_table[3] = 127;

        let mut p = fog_poly(
            vec![
                sv(50, 50, 0x7FFF),
                sv(200, 50, 0x7FFF),
                sv(125, 150, 0x7FFF),
            ],
            true,
        );
        for v in &mut p.vertices {
            v.depth_z = -crate::gpu3d::matrix::ONE;
            v.w = 4096;
        }

        r.render_frame(&[p], None);

        let center = r.framebuffer[100 * FB_WIDTH + 125] & 0x7FFF;
        assert!(
            (center & 0x1F) < 5,
            "W-buffer fog should use W depth; using near Z would leave this pixel white"
        );
    }
}
