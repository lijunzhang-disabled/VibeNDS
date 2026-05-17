//! Vertex pipeline + polygon assembly.
//!
//! Vertices stream in via `VTX_*` commands. Each vertex's position is
//! transformed by `clip = projection × position`, optionally lit, and
//! tagged with the current texture coordinates / polygon attributes.
//! The active `BEGIN_VTXS` primitive type decides when to assemble a
//! polygon from the accumulated vertices.
//!
//! This module produces clip-space polygons. Clipping (next module) and
//! viewport (after that) finish the geometry stage.

use serde::{Deserialize, Serialize};

use super::matrix::{Matrix, ONE};
use super::stacks::MatrixStacks;

/// Primitive type from `BEGIN_VTXS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrimitiveType {
    Triangles,      // every 3 verts = 1 triangle
    Quads,          // every 4 verts = 1 quad
    TriangleStrip,  // 0,1,2 then 1,2,3 then 2,3,4 ...
    QuadStrip,      // 0,1,3,2 then 2,3,5,4 then ...
}

impl PrimitiveType {
    pub fn from_bits(b: u32) -> Self {
        match b & 0x3 {
            0 => PrimitiveType::Triangles,
            1 => PrimitiveType::Quads,
            2 => PrimitiveType::TriangleStrip,
            _ => PrimitiveType::QuadStrip,
        }
    }

    /// Number of vertices in one polygon of this primitive (after assembly).
    pub fn vertices_per_polygon(self) -> usize {
        match self {
            PrimitiveType::Triangles | PrimitiveType::TriangleStrip => 3,
            PrimitiveType::Quads | PrimitiveType::QuadStrip => 4,
        }
    }
}

/// One vertex in clip space, with the attributes carried alongside.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Vertex {
    /// Clip-space `(x, y, z, w)`. Each component is 1.19.12 fixed-point.
    pub clip: [i32; 4],
    /// Color (5 bits per RGB channel, packed BGR555). Set by lighting or `COLOR`.
    pub color: u16,
    /// Texture coordinates (S, T) in 1.11.4 fixed-point.
    pub tex: [i16; 2],
}

impl Vertex {
    pub fn new(clip: [i32; 4], color: u16, tex: [i16; 2]) -> Self {
        Vertex { clip, color, tex }
    }
}

/// One assembled polygon in clip space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Polygon {
    pub vertices: Vec<Vertex>,
    pub attr: u32,
    pub tex_image_param: u32,
    pub palette_base: u16,
}

/// Per-engine vertex pipeline state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VertexState {
    pub primitive: Option<PrimitiveType>,
    /// Last submitted vertex position (the "current" pos for `VTX_DIFF`).
    pub last_pos: [i32; 3],
    /// Current per-vertex color (5+5+5 = BGR555 packed in u16).
    pub current_color: u16,
    /// Current per-vertex texture coordinates (S, T) in 1.11.4 fixed-point.
    pub current_tex: [i16; 2],

    /// `POLYGON_ATTR` — frozen into each polygon at assembly time.
    pub polygon_attr: u32,
    /// `TEXIMAGE_PARAM` — likewise.
    pub tex_image_param: u32,
    /// `PLTT_BASE` — texture palette base.
    pub palette_base: u16,

    /// Vertices accumulated for the current primitive. Cleared at
    /// `BEGIN_VTXS`; popped as polygons get emitted.
    pub vertex_buffer: Vec<Vertex>,

    /// Per-frame polygon output (drained by SWAP_BUFFERS).
    pub polygon_buffer: Vec<Polygon>,
}

/// Build a polygon from N vertices (N ∈ {3, 4}). Snapshots the engine's
/// current polygon-attr and texture state.
fn make_polygon(verts: Vec<Vertex>, state: &VertexState) -> Polygon {
    Polygon {
        vertices: verts,
        attr: state.polygon_attr,
        tex_image_param: state.tex_image_param,
        palette_base: state.palette_base,
    }
}

impl VertexState {
    pub fn new() -> Self { Self::default() }

    /// `BEGIN_VTXS` — start a new primitive group.
    pub fn begin(&mut self, primitive: PrimitiveType) {
        self.primitive = Some(primitive);
        self.vertex_buffer.clear();
    }

    /// `END_VTXS` — no-op on real hardware. Real engines just look at the
    /// next `BEGIN_VTXS` to know when a primitive group ends.
    pub fn end(&mut self) {}

    /// `COLOR` — pack BGR555 from the parameter word's low 15 bits.
    pub fn set_color(&mut self, param: u32) {
        self.current_color = (param & 0x7FFF) as u16;
    }

    /// `TEXCOORD` — two signed 16-bit values, packed in one word.
    pub fn set_texcoord(&mut self, param: u32) {
        let s = param as i16;
        let t = (param >> 16) as i16;
        self.current_tex = [s, t];
    }

    /// `POLYGON_ATTR` — applies to all polygons *after* the next BEGIN_VTXS.
    pub fn set_polygon_attr(&mut self, param: u32) {
        self.polygon_attr = param;
    }

    /// `TEXIMAGE_PARAM`.
    pub fn set_tex_image_param(&mut self, param: u32) {
        self.tex_image_param = param;
    }

    /// `PLTT_BASE` — texture palette base address (low 16 bits).
    pub fn set_palette_base(&mut self, param: u32) {
        self.palette_base = (param & 0xFFFF) as u16;
    }

    /// Process a fully-decoded vertex position (in object space). Transforms
    /// by the clip matrix and appends to the vertex buffer; emits a polygon
    /// when enough vertices accumulate for the current primitive type.
    pub fn submit_vertex(&mut self, pos_obj: [i32; 3], stacks: &MatrixStacks) {
        self.last_pos = pos_obj;

        let primitive = match self.primitive {
            Some(p) => p,
            None => return, // BEGIN_VTXS wasn't called; ignore the vertex
        };

        // Transform to clip space.
        let clip_mat = stacks.clip_matrix();
        let clip = clip_mat.mul_vec4([pos_obj[0], pos_obj[1], pos_obj[2], ONE]);

        let v = Vertex::new(clip, self.current_color, self.current_tex);
        self.vertex_buffer.push(v);

        let n = primitive.vertices_per_polygon();
        match primitive {
            PrimitiveType::Triangles => {
                if self.vertex_buffer.len() == n {
                    let verts = self.vertex_buffer.drain(..).collect::<Vec<_>>();
                    self.polygon_buffer.push(make_polygon(verts, self));
                }
            }
            PrimitiveType::Quads => {
                if self.vertex_buffer.len() == n {
                    let verts = self.vertex_buffer.drain(..).collect::<Vec<_>>();
                    self.polygon_buffer.push(make_polygon(verts, self));
                }
            }
            PrimitiveType::TriangleStrip => {
                if self.vertex_buffer.len() >= 3 {
                    let len = self.vertex_buffer.len();
                    // Take the last 3 vertices in the right winding order.
                    // Even N: 0,1,2; Odd N: 0,2,1 (to keep CCW winding consistent).
                    let i0 = len - 3;
                    let i1 = len - 2;
                    let i2 = len - 1;
                    let verts = if (len - 3) % 2 == 0 {
                        vec![self.vertex_buffer[i0], self.vertex_buffer[i1], self.vertex_buffer[i2]]
                    } else {
                        vec![self.vertex_buffer[i0], self.vertex_buffer[i2], self.vertex_buffer[i1]]
                    };
                    self.polygon_buffer.push(make_polygon(verts, self));
                }
            }
            PrimitiveType::QuadStrip => {
                // Quads in a strip share 2 vertices with the previous quad.
                // Vertex order: 0, 1, 3, 2 (then 2, 3, 5, 4) — i.e. the most
                // recent quad uses indices (n-4, n-3, n-1, n-2) of the buffer.
                if self.vertex_buffer.len() >= 4 && self.vertex_buffer.len() % 2 == 0 {
                    let len = self.vertex_buffer.len();
                    let i0 = len - 4;
                    let i1 = len - 3;
                    let i2 = len - 1;
                    let i3 = len - 2;
                    let verts = vec![
                        self.vertex_buffer[i0],
                        self.vertex_buffer[i1],
                        self.vertex_buffer[i2],
                        self.vertex_buffer[i3],
                    ];
                    self.polygon_buffer.push(make_polygon(verts, self));
                }
            }
        }
    }
}

/// Decode `VTX_16` — two 32-bit parameter words supply the three signed
/// 16-bit object-space coords (x, y, z) plus a padding half.
pub fn decode_vtx16(params: [u32; 2]) -> [i32; 3] {
    let x = (params[0] as i16) as i32;
    let y = ((params[0] >> 16) as i16) as i32;
    let z = (params[1] as i16) as i32;
    // Convert from 1.3.12 (16-bit) to 1.19.12 (32-bit) — same fractional
    // bits, just sign-extended to 32.
    [x, y, z]
}

/// Decode `VTX_10` — one 32-bit param has three 10-bit signed components.
pub fn decode_vtx10(param: u32) -> [i32; 3] {
    let sign_ext = |b: u32| -> i32 { (((b & 0x3FF) << 22) as i32) >> 22 };
    let x = sign_ext(param);
    let y = sign_ext(param >> 10);
    let z = sign_ext(param >> 20);
    // 10-bit values are 1.0.9 — shift left by 6 to land in 1.19.12.
    [x << 6, y << 6, z << 6]
}

/// Decode `VTX_XY` / `VTX_XZ` / `VTX_YZ` — two of the three coords are
/// in the param word; the third is the *current* value from `last_pos`.
pub fn decode_vtx_pair(param: u32, last: [i32; 3], axis: VtxAxisPair) -> [i32; 3] {
    let a = (param as i16) as i32;
    let b = ((param >> 16) as i16) as i32;
    match axis {
        VtxAxisPair::XY => [a, b, last[2]],
        VtxAxisPair::XZ => [a, last[1], b],
        VtxAxisPair::YZ => [last[0], a, b],
    }
}

/// Decode `VTX_DIFF` — three 10-bit signed *deltas* applied to `last_pos`.
pub fn decode_vtx_diff(param: u32, last: [i32; 3]) -> [i32; 3] {
    let sign_ext = |b: u32| -> i32 { (((b & 0x3FF) << 22) as i32) >> 22 };
    let dx = sign_ext(param) << 3;       // delta is 1.0.6 -> shift by 6 to 1.0.12
    let dy = sign_ext(param >> 10) << 3;
    let dz = sign_ext(param >> 20) << 3;
    [last[0] + dx, last[1] + dy, last[2] + dz]
}

#[derive(Debug, Clone, Copy)]
pub enum VtxAxisPair { XY, XZ, YZ }

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::stacks::MtxMode;

    fn ident_stacks() -> MatrixStacks {
        // Projection and position both identity → clip == object space.
        MatrixStacks::new()
    }

    #[test]
    fn test_triangle_assembly() {
        let mut v = VertexState::new();
        let s = ident_stacks();
        v.begin(PrimitiveType::Triangles);
        v.submit_vertex([ONE, 0, 0], &s);
        v.submit_vertex([0, ONE, 0], &s);
        assert!(v.polygon_buffer.is_empty(), "2 verts shouldn't emit yet");
        v.submit_vertex([0, 0, ONE], &s);
        assert_eq!(v.polygon_buffer.len(), 1);
        assert_eq!(v.polygon_buffer[0].vertices.len(), 3);
    }

    #[test]
    fn test_quad_assembly() {
        let mut v = VertexState::new();
        let s = ident_stacks();
        v.begin(PrimitiveType::Quads);
        for i in 0..4 {
            v.submit_vertex([i * ONE, 0, 0], &s);
        }
        assert_eq!(v.polygon_buffer.len(), 1);
        assert_eq!(v.polygon_buffer[0].vertices.len(), 4);
    }

    #[test]
    fn test_triangle_strip_winding() {
        let mut v = VertexState::new();
        let s = ident_stacks();
        v.begin(PrimitiveType::TriangleStrip);
        // 4 vertices → 2 triangles
        v.submit_vertex([0, 0, 0], &s);
        v.submit_vertex([ONE, 0, 0], &s);
        v.submit_vertex([0, ONE, 0], &s);
        assert_eq!(v.polygon_buffer.len(), 1);
        let t0 = &v.polygon_buffer[0];
        assert_eq!(t0.vertices[0].clip[0], 0);
        assert_eq!(t0.vertices[1].clip[0], ONE);
        assert_eq!(t0.vertices[2].clip[0], 0);

        v.submit_vertex([ONE, ONE, 0], &s);
        assert_eq!(v.polygon_buffer.len(), 2);
        // Second triangle: odd N (=1), so winding is swapped (i0, i2, i1).
        let t1 = &v.polygon_buffer[1];
        assert_eq!(t1.vertices[0].clip[0], ONE); // v1
        assert_eq!(t1.vertices[1].clip[0], ONE); // v3
        assert_eq!(t1.vertices[2].clip[0], 0);   // v2
    }

    #[test]
    fn test_quad_strip_assembly() {
        let mut v = VertexState::new();
        let s = ident_stacks();
        v.begin(PrimitiveType::QuadStrip);
        v.submit_vertex([0, 0, 0], &s);
        v.submit_vertex([0, ONE, 0], &s);
        v.submit_vertex([ONE, 0, 0], &s);
        v.submit_vertex([ONE, ONE, 0], &s);
        assert_eq!(v.polygon_buffer.len(), 1);
        // The strip orders as 0,1,3,2 — so vertices[2] is index 3 = (1,1,0),
        // and vertices[3] is index 2 = (1,0,0).
        let q = &v.polygon_buffer[0];
        assert_eq!(q.vertices[2].clip[0], ONE);
        assert_eq!(q.vertices[2].clip[1], ONE);
        assert_eq!(q.vertices[3].clip[0], ONE);
        assert_eq!(q.vertices[3].clip[1], 0);
    }

    #[test]
    fn test_vtx16_decode() {
        let p0 = 0x0010_FFF0u32; // y = 0x0010, x = 0xFFF0 = -16
        let p1 = 0x0000_0020u32; // z = 0x0020
        let r = decode_vtx16([p0, p1]);
        assert_eq!(r, [-16, 0x10, 0x20]);
    }

    #[test]
    fn test_vtx10_decode_negative() {
        // 10-bit value 0x3FF = -1 (sign bit set).
        let param = 0x3FF | (0x3FF << 10) | (0x3FF << 20);
        let r = decode_vtx10(param);
        let neg_one_in_19_12 = -(1 << 6); // shifted left by 6
        assert_eq!(r, [neg_one_in_19_12, neg_one_in_19_12, neg_one_in_19_12]);
    }

    #[test]
    fn test_color_packing() {
        let mut v = VertexState::new();
        v.set_color(0x7FFF);
        assert_eq!(v.current_color, 0x7FFF);
        v.set_color(0x1_8000); // upper bits ignored
        assert_eq!(v.current_color, 0);
    }

    #[test]
    fn test_vertex_carries_color_and_texcoord() {
        let mut v = VertexState::new();
        v.set_color(0x1F);
        v.set_texcoord(0x0010_FFF0u32);
        v.begin(PrimitiveType::Triangles);
        let s = ident_stacks();
        for _ in 0..3 {
            v.submit_vertex([0, 0, 0], &s);
        }
        let p = &v.polygon_buffer[0];
        assert_eq!(p.vertices[0].color, 0x1F);
        assert_eq!(p.vertices[0].tex, [-16, 0x10]);
    }

    #[test]
    fn test_polygon_attr_snapshot_per_polygon() {
        let mut v = VertexState::new();
        let s = ident_stacks();
        v.set_polygon_attr(0xAAAA_AAAA);
        v.begin(PrimitiveType::Triangles);
        for _ in 0..3 { v.submit_vertex([0, 0, 0], &s); }
        assert_eq!(v.polygon_buffer[0].attr, 0xAAAA_AAAA);

        v.set_polygon_attr(0xBBBB_BBBB);
        v.begin(PrimitiveType::Triangles);
        for _ in 0..3 { v.submit_vertex([0, 0, 0], &s); }
        assert_eq!(v.polygon_buffer[1].attr, 0xBBBB_BBBB);
    }

    #[test]
    fn test_submit_without_begin_is_ignored() {
        let mut v = VertexState::new();
        let s = ident_stacks();
        v.submit_vertex([ONE, 0, 0], &s);
        assert!(v.polygon_buffer.is_empty());
        assert!(v.vertex_buffer.is_empty());
    }

    #[test]
    fn test_clip_transform_applies() {
        let mut v = VertexState::new();
        let mut s = MatrixStacks::new();
        s.set_mode(MtxMode::Position);
        s.load(Matrix::identity().mul_translate(5 * ONE, 0, 0));
        v.begin(PrimitiveType::Triangles);
        v.submit_vertex([ONE, 0, 0], &s);
        // Object (1, 0, 0) translated by (5, 0, 0) → clip (6, 0, 0, 1).
        let r = &v.vertex_buffer[0];
        assert_eq!(r.clip[0], 6 * ONE);
        assert_eq!(r.clip[3], ONE);
    }

    #[test]
    fn test_vtx_diff_adds_to_last() {
        let last = [ONE, 2 * ONE, 3 * ONE];
        // VTX_DIFF deltas of (1, -1, 0) in 10-bit → shifted to 1.0.12
        let param = 0x0001u32 | (0x3FFu32 << 10);
        let r = decode_vtx_diff(param, last);
        assert!(r[0] > last[0]);
        assert!(r[1] < last[1]);
        assert_eq!(r[2], last[2]);
    }
}
