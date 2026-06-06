//! Sutherland-Hodgman 6-plane clipping in homogeneous clip space.
//!
//! Each polygon's vertices come in with clip-space `(x, y, z, w)`. We clip
//! against the canonical view volume:
//!
//! ```text
//!     -w ≤ x ≤ +w     (left, right planes)
//!     -w ≤ y ≤ +w     (bottom, top)
//!     -w ≤ z ≤ +w     (near, far)
//! ```
//!
//! The algorithm is the textbook one: iterate over the 6 planes in turn;
//! for each plane, walk the polygon's edges. Inside vertices are kept;
//! outside vertices are dropped; edges that straddle a plane produce a
//! new interpolated vertex at the intersection point. Color and texture
//! coordinates are linearly interpolated.
//!
//! NDS polygons start as triangles (3 verts) or quads (4 verts). After
//! clipping a polygon can have up to 10 vertices — one extra vertex per
//! plane crossing (and we have 6 planes). Real hardware stores up to 10
//! verts per polygon in polygon-RAM; we keep the output as a Vec.

use super::vertex::Vertex;

/// Which side of which plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plane {
    NegX, // x < -w
    PosX, // x >  w
    NegY,
    PosY,
    NegZ, // z < -w (near plane)
    PosZ, // z >  w (far plane)
}

const PLANES: [Plane; 6] = [
    Plane::NegX,
    Plane::PosX,
    Plane::NegY,
    Plane::PosY,
    Plane::NegZ,
    Plane::PosZ,
];

/// Signed distance-style classifier: positive = inside, negative = outside.
/// We don't return the actual distance — only the sign matters for the
/// "inside / outside / straddling" decisions, and the *parameterized*
/// interpolation factor for intersection uses both vertices' values.
#[inline]
fn classify(v: &Vertex, plane: Plane) -> i64 {
    let x = v.clip[0] as i64;
    let y = v.clip[1] as i64;
    let z = v.clip[2] as i64;
    let w = v.clip[3] as i64;
    match plane {
        Plane::NegX => x + w, // inside iff x >= -w → x + w >= 0
        Plane::PosX => w - x, // inside iff x <=  w → w - x >= 0
        Plane::NegY => y + w,
        Plane::PosY => w - y,
        Plane::NegZ => z + w, // inside iff z >= -w
        Plane::PosZ => w - z,
    }
}

/// Interpolate a vertex partway between `a` and `b` at parameter
/// `t = d_a / (d_a - d_b)` where `d_*` are the classifier values. This is
/// the standard parametric line-plane intersection in homogeneous coords.
fn interpolate(a: &Vertex, b: &Vertex, t_num: i64, t_den: i64) -> Vertex {
    // t ∈ [0, 1] expressed as t_num/t_den.
    // Each component: result = a + (b - a) * t = a * (1 - t) + b * t.
    let lerp_i32 = |a: i32, b: i32| -> i32 {
        // (a * (den - num) + b * num) / den
        let a64 = a as i64;
        let b64 = b as i64;
        let r = a64 * (t_den - t_num) + b64 * t_num;
        // Avoid division-by-zero just in case (caller guards).
        if t_den == 0 {
            a
        } else {
            (r / t_den) as i32
        }
    };
    let lerp_i16 = |a: i16, b: i16| -> i16 {
        let r = (a as i64) * (t_den - t_num) + (b as i64) * t_num;
        if t_den == 0 {
            a
        } else {
            (r / t_den) as i16
        }
    };
    let lerp_color = |a: u16, b: u16| -> u16 {
        // BGR555: 5+5+5. Interpolate each channel separately.
        let chan = |c: u16, shift: u32| ((c >> shift) & 0x1F) as i64;
        let ar = chan(a, 0);
        let br = chan(b, 0);
        let ag = chan(a, 5);
        let bg = chan(b, 5);
        let ab = chan(a, 10);
        let bb = chan(b, 10);
        let lerp = |x: i64, y: i64| -> u16 {
            let r = x * (t_den - t_num) + y * t_num;
            let v = if t_den == 0 { x } else { r / t_den };
            (v.clamp(0, 31)) as u16
        };
        lerp(ar, br) | (lerp(ag, bg) << 5) | (lerp(ab, bb) << 10)
    };
    Vertex {
        clip: [
            lerp_i32(a.clip[0], b.clip[0]),
            lerp_i32(a.clip[1], b.clip[1]),
            lerp_i32(a.clip[2], b.clip[2]),
            lerp_i32(a.clip[3], b.clip[3]),
        ],
        color: lerp_color(a.color, b.color),
        tex: [lerp_i16(a.tex[0], b.tex[0]), lerp_i16(a.tex[1], b.tex[1])],
    }
}

/// Clip one polygon against all 6 planes. Returns `None` if the polygon
/// is fully outside any plane (and thus rejected); otherwise returns the
/// (possibly modified) list of vertices.
pub fn clip_polygon(input: &[Vertex]) -> Option<Vec<Vertex>> {
    if input.len() < 3 {
        return None;
    }
    let mut current: Vec<Vertex> = input.to_vec();

    for plane in PLANES {
        if current.is_empty() {
            return None;
        }
        let next = clip_against_plane(&current, plane);
        current = next;
    }

    if current.len() < 3 {
        None
    } else {
        Some(current)
    }
}

fn clip_against_plane(input: &[Vertex], plane: Plane) -> Vec<Vertex> {
    let mut output: Vec<Vertex> = Vec::with_capacity(input.len() + 2);
    let n = input.len();
    if n == 0 {
        return output;
    }

    let mut prev = &input[n - 1];
    let mut prev_d = classify(prev, plane);

    for cur in input {
        let cur_d = classify(cur, plane);

        match (prev_d >= 0, cur_d >= 0) {
            (true, true) => {
                // both inside — emit current
                output.push(*cur);
            }
            (true, false) => {
                // prev inside, cur outside — emit the intersection
                let t_num = prev_d;
                let t_den = prev_d - cur_d;
                output.push(interpolate(prev, cur, t_num, t_den));
            }
            (false, true) => {
                // prev outside, cur inside — emit intersection then current
                let t_num = -prev_d;
                let t_den = cur_d - prev_d;
                output.push(interpolate(prev, cur, t_num, t_den));
                output.push(*cur);
            }
            (false, false) => {
                // both outside — emit nothing
            }
        }

        prev = cur;
        prev_d = cur_d;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::super::matrix::ONE;
    use super::*;

    const MAX_CLIPPED_VERTICES_PER_POLYGON: usize = 10;

    fn vtx(x: i32, y: i32, z: i32, w: i32, color: u16) -> Vertex {
        Vertex::new([x, y, z, w], color, [0, 0])
    }

    #[test]
    fn test_triangle_fully_inside_passes_through() {
        let tri = vec![
            vtx(0, 0, ONE / 2, ONE, 0x001F),
            vtx(ONE / 2, 0, ONE / 2, ONE, 0x001F),
            vtx(0, ONE / 2, ONE / 2, ONE, 0x001F),
        ];
        let out = clip_polygon(&tri).expect("inside");
        assert_eq!(out.len(), 3);
        for (i, v) in out.iter().enumerate() {
            assert_eq!(v.clip, tri[i].clip);
        }
    }

    #[test]
    fn test_triangle_fully_outside_left_plane_is_rejected() {
        // All vertices have x < -w → outside left plane.
        let tri = vec![
            vtx(-2 * ONE, 0, ONE / 2, ONE, 0),
            vtx(-3 * ONE, ONE, ONE / 2, ONE, 0),
            vtx(-4 * ONE, -ONE, ONE / 2, ONE, 0),
        ];
        assert!(clip_polygon(&tri).is_none());
    }

    #[test]
    fn test_triangle_straddling_near_plane_clips_to_polygon() {
        // Two verts inside (z >= -w), one behind the near plane (z < -w).
        let tri = vec![
            vtx(0, 0, ONE / 2, ONE, 0x001F),   // inside
            vtx(0, ONE, ONE / 2, ONE, 0x001F), // inside
            vtx(0, 0, -2 * ONE, ONE, 0x001F),  // outside (z < -w)
        ];
        let out = clip_polygon(&tri).expect("clipped");
        // Clipping a triangle that straddles one plane produces 4 vertices.
        assert_eq!(out.len(), 4);
        // None of the output vertices should have z < -w.
        for v in &out {
            assert!(
                v.clip[2] >= -v.clip[3],
                "z should be inside near plane after clip, got z={} w={}",
                v.clip[2],
                v.clip[3]
            );
        }
    }

    #[test]
    fn test_color_interpolation_along_clipped_edge() {
        // Triangle straddling the near plane; one vertex is red, the
        // behind-near-plane vertex is blue. The new interpolated vertex on the
        // straddling edge should have a color between them.
        let tri = vec![
            vtx(0, 0, ONE, ONE, 0x001F),      // red, inside
            vtx(0, ONE, ONE, ONE, 0x001F),    // red, inside
            vtx(0, 0, -2 * ONE, ONE, 0x7C00), // blue, outside
        ];
        let out = clip_polygon(&tri).expect("clipped");
        // The two new vertices should have a mix of red and blue.
        let has_mix = out.iter().any(|v| {
            let r = v.color & 0x1F;
            let b = (v.color >> 10) & 0x1F;
            r > 0 && b > 0
        });
        assert!(
            has_mix,
            "expected at least one interpolated red+blue vertex"
        );
    }

    #[test]
    fn test_texcoord_interpolation_along_clipped_edge() {
        let mut inside_a = vtx(0, 0, ONE, ONE, 0x7FFF);
        inside_a.tex = [0, 0];
        let mut inside_b = vtx(0, ONE, ONE, ONE, 0x7FFF);
        inside_b.tex = [0, 0];
        let mut outside = vtx(0, 0, -3 * ONE, ONE, 0x7FFF);
        outside.tex = [64, 128];

        let out = clip_polygon(&[inside_a, inside_b, outside]).expect("clipped");

        assert!(
            out.iter().any(|v| v.tex == [32, 64]),
            "near-plane intersections should interpolate S/T halfway between inside and outside vertices"
        );
    }

    #[test]
    fn test_quad_fully_inside_passes_through_four_planes() {
        // A quad fully inside the frustum should survive with 4 vertices.
        let q = vec![
            vtx(0, 0, ONE / 2, ONE, 0),
            vtx(ONE / 2, 0, ONE / 2, ONE, 0),
            vtx(ONE / 2, ONE / 2, ONE / 2, ONE, 0),
            vtx(0, ONE / 2, ONE / 2, ONE, 0),
        ];
        let out = clip_polygon(&q).expect("quad");
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn test_triangle_crossing_two_planes() {
        // Triangle that straddles both the right plane and the far plane.
        // Output polygon should have ≥ 4 vertices (one new per crossing).
        let tri = vec![
            vtx(0, 0, ONE / 4, ONE, 0),
            vtx(2 * ONE, 0, 4 * ONE, ONE, 0), // x > w AND z > w
            vtx(0, ONE / 2, ONE / 4, ONE, 0),
        ];
        let out = clip_polygon(&tri).expect("clipped");
        assert!(
            out.len() >= 4,
            "expected ≥ 4 verts after 2-plane clip, got {}",
            out.len()
        );
        // All survivors must be inside all planes now.
        for v in &out {
            assert!(v.clip[0] >= -v.clip[3]);
            assert!(v.clip[0] <= v.clip[3]);
            assert!(v.clip[2] >= -v.clip[3]);
            assert!(v.clip[2] <= v.clip[3]);
        }
    }

    #[test]
    fn test_clipped_quad_stays_within_hardware_vertex_limit() {
        // A hardware input polygon is either a triangle or a quad. Even when
        // a quad crosses multiple clip planes, the clipped polygon must fit
        // the DS per-polygon storage contract.
        let q = vec![
            vtx(-2 * ONE, -ONE / 2, ONE / 4, ONE, 0x001F),
            vtx(ONE / 2, -2 * ONE, ONE / 4, ONE, 0x03E0),
            vtx(2 * ONE, ONE / 2, ONE / 4, ONE, 0x7C00),
            vtx(-ONE / 2, 2 * ONE, ONE / 4, ONE, 0x7FFF),
        ];

        let out = clip_polygon(&q).expect("clipped quad");

        assert!(
            out.len() <= MAX_CLIPPED_VERTICES_PER_POLYGON,
            "clipped quad produced {} vertices",
            out.len()
        );
        for v in &out {
            assert!(v.clip[0] >= -v.clip[3]);
            assert!(v.clip[0] <= v.clip[3]);
            assert!(v.clip[1] >= -v.clip[3]);
            assert!(v.clip[1] <= v.clip[3]);
            assert!(v.clip[2] >= -v.clip[3]);
            assert!(v.clip[2] <= v.clip[3]);
        }
    }
}
