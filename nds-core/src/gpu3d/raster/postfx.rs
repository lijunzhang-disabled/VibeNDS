//! Post-rasterization passes.
//!
//! All four are independent — each runs as a separate pass over the
//! framebuffer + auxiliary buffers, gated by its own `DISP3DCNT` bit. We
//! apply them in the order: fog → edge mark → toon/highlight →
//! anti-aliasing. The order matters for edge-marking: edge colors replace
//! polygon colors after fog has already been applied to polygon pixels.
//!
//! Anti-aliasing is approximated as an edge-only post-pass. Real hardware
//! stores coverage during scan conversion; we do not, but we still soften
//! opaque polygon silhouettes and keep the edge-mark interaction enabled.

use super::{Rasterizer, DEPTH_MAX, FB_HEIGHT, FB_WIDTH};

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

    if rast.disp3dcnt & (1 << 4) != 0 {
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

            // A pixel that isn't written is treated as background at
            // infinite depth, producing an edge along the visible boundary.
            let neighbor_diff = |nx: isize, ny: isize| -> bool {
                if nx < 0 || nx >= FB_WIDTH as isize || ny < 0 || ny >= FB_HEIGHT as isize {
                    return center_depth < DEPTH_MAX; // off-screen = background
                }
                let nidx = (ny as usize) * FB_WIDTH + (nx as usize);
                let written = fb_snapshot[nidx] & (1 << 15) != 0;
                let different = !written || ids[nidx] != center;
                let neighbor_depth = if written { depths[nidx] } else { DEPTH_MAX };
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

        let pixel = rast.framebuffer[i];
        let pr = (pixel & 0x1F) as u32;
        let pg = ((pixel >> 5) & 0x1F) as u32;
        let pb = ((pixel >> 10) & 0x1F) as u32;

        // density is 0..127; we use 128 as the denominator.
        let blend = |p: u32, f: u32| -> u32 { (p * (128 - density) + f * density) / 128 };

        if alpha_only {
            // Only blend alpha (no color change). We don't model alpha bits
            // in the framebuffer beyond "written"; treat this as a no-op
            // unless a future game needs it.
            let _ = (pr, pg, pb, blend, fr, fg, fb, fog_alpha);
        } else {
            let nr = blend(pr, fr).min(31);
            let ng = blend(pg, fg).min(31);
            let nb = blend(pb, fb).min(31);
            rast.framebuffer[i] =
                (nr as u16) | ((ng as u16) << 5) | ((nb as u16) << 10) | (1 << 15);
        }
    }
}

/// Approximate DS anti-aliasing: make opaque silhouette pixels partially
/// blend toward the 3D rear plane. Real hardware uses coverage values from
/// rasterization; this detects the same class of pixels conservatively using
/// polygon ID/depth neighborhood tests.
fn apply_antialiasing(rast: &mut Rasterizer) {
    let fb_snapshot = rast.framebuffer.clone();
    let ids = rast.id_buffer.clone();
    let depths = rast.depth_buffer.clone();
    let edge_enabled = rast.edge_enable_buffer.clone();
    let rear = (rast.clear_color & 0x7FFF) as u16;

    for y in 0..FB_HEIGHT {
        for x in 0..FB_WIDTH {
            let idx = y * FB_WIDTH + x;
            if fb_snapshot[idx] & (1 << 15) == 0 || edge_enabled[idx] == 0 {
                continue;
            }
            let center = ids[idx];
            let center_depth = depths[idx];

            let neighbor_exposes_edge = |nx: isize, ny: isize| -> bool {
                if nx < 0 || nx >= FB_WIDTH as isize || ny < 0 || ny >= FB_HEIGHT as isize {
                    return true;
                }
                let nidx = (ny as usize) * FB_WIDTH + (nx as usize);
                let written = fb_snapshot[nidx] & (1 << 15) != 0;
                if !written {
                    return true;
                }
                ids[nidx] != center && center_depth < depths[nidx]
            };

            let is_edge = neighbor_exposes_edge(x as isize - 1, y as isize)
                || neighbor_exposes_edge(x as isize + 1, y as isize)
                || neighbor_exposes_edge(x as isize, y as isize - 1)
                || neighbor_exposes_edge(x as isize, y as isize + 1);

            if is_edge {
                rast.framebuffer[idx] =
                    alpha_blend_bgr555(fb_snapshot[idx] & 0x7FFF, rear, 16) | (1 << 15);
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
    let depth = depth.max(0);
    if depth <= 0x2000 {
        // Current Z-buffer path stores NDC z in 0..0x2000. Hardware fog
        // compares against the 15-bit Z/W depth domain.
        ((depth as i64 * 0x7FFF + 0x1000) / 0x2000) as i32
    } else if depth <= 0x7FFF {
        depth
    } else {
        (depth >> 9).min(0x7FFF)
    }
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
        r.edge_color[0] = 0x001F;
        r.fog_color = 0;
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
    fn test_antialias_softens_opaque_silhouette_pixel() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 << 4;
        r.clear_color = 0x7FFF;
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
        r.edge_color[0] = 0x001F;
        r.clear_color = 0x7FFF;
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

        assert_eq!(fog_density_for_depth(0x50FF, 0x5000, 2, &table), 8);
        assert_eq!(fog_density_for_depth(0x5100, 0x5000, 2, &table), 8);
        assert_eq!(fog_density_for_depth(0x5200, 0x5000, 2, &table), 64);
        assert_eq!(fog_density_for_depth(0x7000, 0x5000, 2, &table), 128);
    }

    #[test]
    fn test_fog_density_interpolates_between_boundaries() {
        let mut table = [0u8; 32];
        table[0] = 0;
        table[1] = 127;

        assert_eq!(fog_density_for_depth(0x3600, 0x3000, 0, &table), 64);
    }

    #[test]
    fn test_fog_density_handles_zero_step_shift() {
        let mut table = [0u8; 32];
        table[0] = 16;
        table[31] = 127;

        assert_eq!(fog_density_for_depth(0x3000, 0x3000, 11, &table), 16);
        assert_eq!(fog_density_for_depth(0x3001, 0x3000, 11, &table), 128);
    }

    #[test]
    fn test_fog_depth_scales_current_z_buffer_units() {
        assert_eq!(fog_depth_units(0), 0);
        assert!((fog_depth_units(0x1000) - 0x4000).abs() <= 1);
        assert_eq!(fog_depth_units(0x2000), 0x7FFF);
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
}
