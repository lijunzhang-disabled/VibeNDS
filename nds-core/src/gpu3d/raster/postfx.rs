//! Post-rasterization passes.
//!
//! All four are independent — each runs as a separate pass over the
//! framebuffer + auxiliary buffers, gated by its own `DISP3DCNT` bit. We
//! apply them in the order: edge mark → toon/highlight → fog →
//! anti-aliasing. The order matters for fog (which should affect post-
//! toon colors) and for edge-marking (which should not be modified by
//! fog).
//!
//! Anti-aliasing is the most subtle of the four — it requires per-pixel
//! coverage values written during rasterization. Phase 7 part 2 leaves
//! it stubbed (`DISP3DCNT.antialias` bit reads/writes but has no visible
//! effect). The other three are fully functional.

use super::{Rasterizer, FB_HEIGHT, FB_WIDTH};

/// Run every enabled post-effect over the framebuffer.
pub fn apply(rast: &mut Rasterizer) {
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

    if rast.disp3dcnt & (1 << 7) != 0 {
        apply_fog(rast);
    }
}

/// Edge marking: for each pixel, compare its polygon ID to its 4
/// neighbors. A neighbor is "different" if either its polygon ID
/// differs OR it's unwritten (= background). The center pixel is then
/// tinted with the corresponding entry from `EDGE_COLOR` (indexed by
/// `poly_id >> 3`).
fn apply_edge_marking(rast: &mut Rasterizer) {
    // Snapshot both buffers so we compare against the original values.
    let ids = rast.id_buffer.clone();
    let fb_snapshot = rast.framebuffer.clone();
    let edge_color = rast.edge_color;

    for y in 0..FB_HEIGHT {
        for x in 0..FB_WIDTH {
            let idx = y * FB_WIDTH + x;
            if fb_snapshot[idx] & (1 << 15) == 0 { continue; }
            let center = ids[idx];

            // Each neighbor: (is_written, id). A pixel that isn't written
            // is treated as "different from anything," producing an edge
            // along the polygon's outer boundary.
            let neighbor_diff = |nx: isize, ny: isize| -> bool {
                if nx < 0 || nx >= FB_WIDTH as isize || ny < 0 || ny >= FB_HEIGHT as isize {
                    return true; // off-screen = different
                }
                let nidx = (ny as usize) * FB_WIDTH + (nx as usize);
                let written = fb_snapshot[nidx] & (1 << 15) != 0;
                !written || ids[nidx] != center
            };

            let is_edge =
                neighbor_diff(x as isize - 1, y as isize)
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

/// Fog: blend each pixel toward `FOG_COLOR` by the density value from
/// `FOG_TABLE[depth >> shift]`. Depth-to-table-index mapping uses
/// `FOG_OFFSET` and the per-frame shift from `DISP3DCNT[11:8]`.
fn apply_fog(rast: &mut Rasterizer) {
    let shift = ((rast.disp3dcnt >> 8) & 0xF) as u32;
    let fog_color_packed = rast.fog_color & 0x7FFF;
    let fog_alpha = ((rast.fog_color >> 16) & 0x1F) as u32;
    let alpha_only = rast.disp3dcnt & (1 << 6) != 0;
    let fog_offset = rast.fog_offset as i32;
    let fog_table = rast.fog_table;

    let fr = (fog_color_packed & 0x1F) as u32;
    let fg = ((fog_color_packed >> 5) & 0x1F) as u32;
    let fb = ((fog_color_packed >> 10) & 0x1F) as u32;

    for i in 0..rast.framebuffer.len() {
        if rast.framebuffer[i] & (1 << 15) == 0 { continue; }

        // Map depth → fog table index. Real hw uses depth bits [18:0];
        // we approximate with our DEPTH_MAX-scale depth.
        let depth = rast.depth_buffer[i].max(0);
        let depth_idx_raw = ((depth - fog_offset).max(0) >> (15 + shift)) as usize;
        let density = fog_table[depth_idx_raw.min(31)] as u32; // 0..127

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
            rast.framebuffer[i] = (nr as u16) | ((ng as u16) << 5) | ((nb as u16) << 10) | (1 << 15);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu3d::viewport::{ScreenPolygon, ScreenVertex};

    fn sv(x: i32, y: i32, color: u16) -> ScreenVertex {
        ScreenVertex {
            screen_x: x << 8, screen_y: y << 8,
            depth_z: 0, w: 4096, color, tex: [0, 0],
        }
    }

    fn make_poly(verts: Vec<ScreenVertex>, poly_id: u8) -> ScreenPolygon {
        ScreenPolygon {
            vertices: verts,
            attr: (0x1F << 16) | ((poly_id as u32) << 24),
            tex_image_param: 0,
            palette_base: 0,
        }
    }

    #[test]
    fn test_edge_marking_outlines_a_single_polygon() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 | (1 << 5); // 3D enable + edge mark
        // Edge color group 0 = red.
        r.edge_color[0] = 0x001F;

        // Single red-filled polygon with poly_id = 0.
        let p = make_poly(vec![
            sv(50, 50, 0x7C00),   // blue interior
            sv(200, 50, 0x7C00),
            sv(125, 150, 0x7C00),
        ], 0);
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
        assert!(found_red_edge, "expected left edge of triangle to be tinted red");
    }

    #[test]
    fn test_fog_darkens_distant_pixels() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1 | (1 << 7); // 3D enable + fog
        r.fog_color = 0; // black fog
        // All fog table entries = max density (127) so blending is full.
        for d in r.fog_table.iter_mut() { *d = 127; }

        // Far white triangle.
        let p = ScreenPolygon {
            vertices: vec![
                ScreenVertex { screen_x: 50 << 8, screen_y: 50 << 8, depth_z: 4000, w: 4096, color: 0x7FFF, tex:[0,0] },
                ScreenVertex { screen_x: 200 << 8, screen_y: 50 << 8, depth_z: 4000, w: 4096, color: 0x7FFF, tex:[0,0] },
                ScreenVertex { screen_x: 125 << 8, screen_y: 150 << 8, depth_z: 4000, w: 4096, color: 0x7FFF, tex:[0,0] },
            ],
            attr: 0x1F << 16, tex_image_param: 0, palette_base: 0,
        };
        r.render_frame(&[p], None);

        // Center pixel should be near-black (fog density 127 / 128 ~= 99%).
        let center = r.framebuffer[100 * FB_WIDTH + 125];
        let r_ch = center & 0x1F;
        assert!(r_ch < 5, "fog should have darkened center to near-black; got 0x{:04X}", center);
    }

    #[test]
    fn test_fog_disabled_leaves_color_intact() {
        let mut r = Rasterizer::new();
        r.disp3dcnt = 1; // 3D enable, no fog
        for d in r.fog_table.iter_mut() { *d = 127; }

        let p = make_poly(vec![
            sv(50, 50, 0x7FFF),
            sv(200, 50, 0x7FFF),
            sv(125, 150, 0x7FFF),
        ], 0);
        r.render_frame(&[p], None);

        let center = r.framebuffer[100 * FB_WIDTH + 125];
        assert!((center & 0x1F) >= 30, "fog disabled — white should remain white");
    }
}
