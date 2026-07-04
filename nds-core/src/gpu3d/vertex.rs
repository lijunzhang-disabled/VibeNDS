//! Vertex pipeline + polygon assembly.
//!
//! Vertices stream in via `VTX_*` commands. Each vertex's position is
//! transformed by `clip = position × projection`, optionally lit, and
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
    Triangles,     // every 3 verts = 1 triangle
    Quads,         // every 4 verts = 1 quad
    TriangleStrip, // 0,1,2 then 1,2,3 then 2,3,4 ...
    QuadStrip,     // 0,1,3,2 then 2,3,5,4 then ...
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
    /// True when a negative signed screen-space area is the polygon's front
    /// side. Triangle strips invert this for every second triangle on DS.
    #[serde(default = "default_front_area_negative")]
    pub front_area_negative: bool,
}

/// Per-engine vertex pipeline state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VertexState {
    pub primitive: Option<PrimitiveType>,
    /// True while a vertex list is open. Real hardware only terminates this
    /// state when a new BEGIN_VTXS starts or SWAP_BUFFERS force-ends it;
    /// END_VTXS itself is effectively a no-op.
    pub list_active: bool,
    /// Last submitted vertex position (the "current" pos for `VTX_DIFF`).
    pub last_pos: [i32; 3],
    /// Current per-vertex color (5+5+5 = BGR555 packed in u16).
    pub current_color: u16,
    /// Current per-vertex texture coordinates (S, T) in 1.11.4 fixed-point.
    pub current_tex: [i16; 2],
    /// Most recent raw TEXCOORD values, before optional texture matrix use.
    pub raw_tex: [i16; 2],

    /// `POLYGON_ATTR` — frozen into each polygon at assembly time.
    pub polygon_attr: u32,
    /// Deferred polygon attributes for the next vertex list. Hardware defers
    /// POLYGON_ATTR writes made during an active list until the next BEGIN.
    pub pending_polygon_attr: Option<u32>,
    /// `TEXIMAGE_PARAM` — likewise.
    pub tex_image_param: u32,
    /// `PLTT_BASE` — texture palette base.
    pub palette_base: u16,
    /// Texture state latched at BEGIN_VTXS for strip primitives. Hardware
    /// allows texture state per polygon except within strips, where connected
    /// polygons keep the list-start texture binding.
    pub strip_tex_image_param: u32,
    pub strip_palette_base: u16,

    /// Vertices accumulated for the current primitive. Cleared at
    /// `BEGIN_VTXS`; popped as polygons get emitted.
    pub vertex_buffer: Vec<Vertex>,

    /// Per-frame polygon output (drained by SWAP_BUFFERS).
    pub polygon_buffer: Vec<Polygon>,
}

/// Build a polygon from N vertices (N ∈ {3, 4}). Snapshots the engine's
/// current polygon-attr and texture state.
fn default_front_area_negative() -> bool {
    true
}

fn make_polygon(
    verts: Vec<Vertex>,
    state: &VertexState,
    primitive: PrimitiveType,
    front_area_negative: bool,
) -> Polygon {
    let (tex_image_param, palette_base) = if primitive.is_strip() {
        (state.strip_tex_image_param, state.strip_palette_base)
    } else {
        (state.tex_image_param, state.palette_base)
    };
    Polygon {
        vertices: verts,
        attr: state.polygon_attr,
        tex_image_param,
        palette_base,
        front_area_negative,
    }
}

impl VertexState {
    pub fn new() -> Self {
        Self::default()
    }

    /// `BEGIN_VTXS` — start a new primitive group.
    pub fn begin(&mut self, primitive: PrimitiveType) {
        if let Some(attr) = self.pending_polygon_attr.take() {
            self.polygon_attr = attr;
        }
        trace_gx_state(
            self.polygon_buffer.len(),
            format_args!(
                "begin prim={primitive:?} tex=0x{:08X} pal={} attr=0x{:08X}",
                self.tex_image_param, self.palette_base, self.polygon_attr
            ),
        );
        self.primitive = Some(primitive);
        self.list_active = true;
        self.vertex_buffer.clear();
        self.strip_tex_image_param = self.tex_image_param;
        self.strip_palette_base = self.palette_base;
    }

    /// `END_VTXS` — real NDS hardware treats this as a no-op. Vertex lists
    /// are implicitly ended by the next `BEGIN_VTXS` or by `SWAP_BUFFERS`.
    pub fn end(&mut self) {}

    /// Internal list termination used for events that really do close the
    /// active list, such as buffer swaps.
    pub fn force_end(&mut self) {
        self.primitive = None;
        self.list_active = false;
        self.vertex_buffer.clear();
    }

    /// True when the active list contains vertices that have not completed a
    /// polygon. SWAP_BUFFERS during that state locks real NDS 3D hardware.
    pub fn has_incomplete_polygon_list(&self) -> bool {
        if !self.list_active {
            return false;
        }
        match self.primitive {
            Some(PrimitiveType::Triangles) => self.vertex_buffer.len() % 3 != 0,
            Some(PrimitiveType::Quads) => self.vertex_buffer.len() % 4 != 0,
            Some(PrimitiveType::TriangleStrip) => {
                !self.vertex_buffer.is_empty() && self.vertex_buffer.len() < 3
            }
            Some(PrimitiveType::QuadStrip) => {
                let len = self.vertex_buffer.len();
                (len > 0 && len < 4) || (len >= 4 && len % 2 != 0)
            }
            None => false,
        }
    }

    /// `COLOR` — pack BGR555 from the parameter word's low 15 bits.
    pub fn set_color(&mut self, param: u32) {
        self.current_color = (param & 0x7FFF) as u16;
    }

    /// `TEXCOORD` — two signed 16-bit values, packed in one word.
    pub fn set_texcoord(&mut self, param: u32, stacks: &MatrixStacks) {
        let s = param as i16;
        let t = (param >> 16) as i16;
        self.raw_tex = [s, t];
        self.current_tex = match self.texcoord_transform_mode() {
            1 => transform_texcoord_source([s, t], &stacks.texture),
            // Modes 2 and 3 are evaluated by NORMAL and VTX commands.
            _ => [s, t],
        };
    }

    /// `POLYGON_ATTR` — applies to all polygons after the next `BEGIN_VTXS`.
    pub fn set_polygon_attr(&mut self, param: u32) {
        self.pending_polygon_attr = Some(param);
    }

    /// `TEXIMAGE_PARAM`.
    pub fn set_tex_image_param(&mut self, param: u32) {
        trace_gx_state(
            self.polygon_buffer.len(),
            format_args!(
                "tex_image_param 0x{:08X} -> 0x{param:08X}",
                self.tex_image_param
            ),
        );
        self.tex_image_param = param;
    }

    /// `PLTT_BASE` — texture palette base address (low 13 bits).
    pub fn set_palette_base(&mut self, param: u32) {
        trace_gx_state(
            self.polygon_buffer.len(),
            format_args!("palette_base {} -> {}", self.palette_base, param & 0x1FFF),
        );
        self.palette_base = (param & 0x1FFF) as u16;
    }

    /// Process a fully-decoded vertex position (in object space). Transforms
    /// by the clip matrix and appends to the vertex buffer; emits a polygon
    /// when enough vertices accumulate for the current primitive type.
    pub fn submit_vertex(&mut self, pos_obj: [i32; 3], stacks: &MatrixStacks) {
        let primitive = match self.primitive {
            Some(p) => p,
            None => return, // BEGIN_VTXS wasn't called; ignore the vertex
        };

        self.last_pos = pos_obj;

        // Transform to clip space.
        let clip_mat = stacks.clip_matrix();
        let clip = clip_mat.mul_vec4([pos_obj[0], pos_obj[1], pos_obj[2], ONE]);

        let tex = if self.texcoord_transform_mode() == 3 {
            transform_vertex_source(pos_obj, self.raw_tex, &stacks.texture)
        } else {
            self.current_tex
        };
        let v = Vertex::new(clip, self.current_color, tex);
        self.vertex_buffer.push(v);

        let n = primitive.vertices_per_polygon();
        match primitive {
            PrimitiveType::Triangles => {
                if self.vertex_buffer.len() == n {
                    let verts = self.vertex_buffer.drain(..).collect::<Vec<_>>();
                    self.push_polygon(verts, primitive, true);
                }
            }
            PrimitiveType::Quads => {
                if self.vertex_buffer.len() == n {
                    let verts = self.vertex_buffer.drain(..).collect::<Vec<_>>();
                    self.push_polygon(verts, primitive, true);
                }
            }
            PrimitiveType::TriangleStrip => {
                if self.vertex_buffer.len() >= 3 {
                    let len = self.vertex_buffer.len();
                    // DS triangle strips keep natural connected order:
                    // 0,1,2 then 1,2,3 then 2,3,4. Every second triangle has
                    // the opposite front-facing winding rule.
                    let i0 = len - 3;
                    let i1 = len - 2;
                    let i2 = len - 1;
                    let front_area_negative = (len - 3) % 2 == 0;
                    let verts = vec![
                        self.vertex_buffer[i0],
                        self.vertex_buffer[i1],
                        self.vertex_buffer[i2],
                    ];
                    self.push_polygon(verts, primitive, front_area_negative);
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
                    self.push_polygon(verts, primitive, true);
                }
            }
        }
    }

    fn push_polygon(
        &mut self,
        verts: Vec<Vertex>,
        primitive: PrimitiveType,
        front_area_negative: bool,
    ) {
        let poly = make_polygon(verts, self, primitive, front_area_negative);
        let index = self.polygon_buffer.len();
        trace_gx_state(
            index,
            format_args!(
                "poly prim={primitive:?} attr=0x{:08X} tex=0x{:08X} pal={} current_tex=0x{:08X} current_pal={} strip_tex=0x{:08X} strip_pal={} verts={} front_neg={front_area_negative}",
                poly.attr,
                poly.tex_image_param,
                poly.palette_base,
                self.tex_image_param,
                self.palette_base,
                self.strip_tex_image_param,
                self.strip_palette_base,
                poly.vertices.len()
            ),
        );
        self.polygon_buffer.push(poly);
    }

    /// Apply texture-coordinate transform mode 2. The normal components are
    /// raw 1.0.9 signed values from the NORMAL command.
    pub fn apply_normal_texcoord_transform(&mut self, normal: [i32; 3], stacks: &MatrixStacks) {
        if self.texcoord_transform_mode() == 2 {
            self.current_tex = transform_normal_source(normal, self.raw_tex, &stacks.texture);
        }
    }

    fn texcoord_transform_mode(&self) -> u32 {
        (self.effective_tex_image_param() >> 30) & 0x3
    }

    fn effective_tex_image_param(&self) -> u32 {
        if self.list_active && self.primitive.is_some_and(PrimitiveType::is_strip) {
            self.strip_tex_image_param
        } else {
            self.tex_image_param
        }
    }
}

impl PrimitiveType {
    fn is_strip(self) -> bool {
        matches!(
            self,
            PrimitiveType::TriangleStrip | PrimitiveType::QuadStrip
        )
    }
}

#[inline]
fn narrow_texcoord(value: i64) -> i16 {
    value as i16
}

fn transform_texcoord_source(tex: [i16; 2], m: &Matrix) -> [i16; 2] {
    let s = tex[0] as i64;
    let t = tex[1] as i64;
    let c = 1i64; // 1/16 in texture-coordinate units.
    let out_s =
        ((s * m.m[0] as i64) + (t * m.m[4] as i64) + (c * m.m[8] as i64) + (c * m.m[12] as i64))
            >> 12;
    let out_t =
        ((s * m.m[1] as i64) + (t * m.m[5] as i64) + (c * m.m[9] as i64) + (c * m.m[13] as i64))
            >> 12;
    [narrow_texcoord(out_s), narrow_texcoord(out_t)]
}

fn transform_normal_source(normal: [i32; 3], tex: [i16; 2], m: &Matrix) -> [i16; 2] {
    let nx = normal[0] as i64;
    let ny = normal[1] as i64;
    let nz = normal[2] as i64;
    let base_s = tex[0] as i64;
    let base_t = tex[1] as i64;
    let out_s = ((nx * m.m[0] as i64) + (ny * m.m[4] as i64) + (nz * m.m[8] as i64)) >> 17;
    let out_t = ((nx * m.m[1] as i64) + (ny * m.m[5] as i64) + (nz * m.m[9] as i64)) >> 17;
    [
        narrow_texcoord(out_s + base_s),
        narrow_texcoord(out_t + base_t),
    ]
}

fn transform_vertex_source(pos: [i32; 3], tex: [i16; 2], m: &Matrix) -> [i16; 2] {
    let vx = pos[0] as i64;
    let vy = pos[1] as i64;
    let vz = pos[2] as i64;
    let base_s = tex[0] as i64;
    let base_t = tex[1] as i64;
    let out_s = ((vx * m.m[0] as i64) + (vy * m.m[4] as i64) + (vz * m.m[8] as i64)) >> 20;
    let out_t = ((vx * m.m[1] as i64) + (vy * m.m[5] as i64) + (vz * m.m[9] as i64)) >> 20;
    [
        narrow_texcoord(out_s + base_s),
        narrow_texcoord(out_t + base_t),
    ]
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
    // VTX_DIFF stores a signed 10-bit delta in the same 12-fractional-bit
    // coordinate units as VTX_16. The raw range is therefore about +/-0.125.
    let dx = sign_ext(param);
    let dy = sign_ext(param >> 10);
    let dz = sign_ext(param >> 20);
    [last[0] + dx, last[1] + dy, last[2] + dz]
}

#[derive(Debug, Clone, Copy)]
pub enum VtxAxisPair {
    XY,
    XZ,
    YZ,
}

fn trace_gx_state_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("NDS_TRACE_GX_STATE").is_some())
}

fn trace_gx_state(index: usize, args: std::fmt::Arguments<'_>) {
    if !trace_gx_state_enabled() || !trace_gx_poly_index_matches(index) {
        return;
    }
    eprintln!("gx state poly_index={index} {args}");
}

fn trace_gx_poly_index_matches(index: usize) -> bool {
    let Some(spec) = std::env::var_os("NDS_TRACE_GX_POLY_RANGE") else {
        return true;
    };
    let Some(spec) = spec.to_str() else {
        return true;
    };
    let Some((start, end)) = spec.split_once("..") else {
        return true;
    };
    let Ok(start) = start.parse::<usize>() else {
        return true;
    };
    let Ok(end) = end.parse::<usize>() else {
        return true;
    };
    index >= start && index < end
}

#[cfg(test)]
mod tests {
    use super::super::stacks::MtxMode;
    use super::*;

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
        assert!(t0.front_area_negative);

        v.submit_vertex([ONE, ONE, 0], &s);
        assert_eq!(v.polygon_buffer.len(), 2);
        // Second triangle keeps connected order (v1, v2, v3), but its
        // front-facing area rule is inverted.
        let t1 = &v.polygon_buffer[1];
        assert_eq!(t1.vertices[0].clip[0], ONE); // v1
        assert_eq!(t1.vertices[1].clip[0], 0); // v2
        assert_eq!(t1.vertices[2].clip[0], ONE); // v3
        assert!(!t1.front_area_negative);
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
        let s = ident_stacks();
        v.set_color(0x1F);
        v.set_texcoord(0x0010_FFF0u32, &s);
        v.begin(PrimitiveType::Triangles);
        for _ in 0..3 {
            v.submit_vertex([0, 0, 0], &s);
        }
        let p = &v.polygon_buffer[0];
        assert_eq!(p.vertices[0].color, 0x1F);
        assert_eq!(p.vertices[0].tex, [-16, 0x10]);
    }

    #[test]
    fn test_texcoord_transform_mode_1_uses_texture_matrix() {
        let mut v = VertexState::new();
        let mut s = ident_stacks();
        s.set_mode(MtxMode::Texture);
        s.load(Matrix::identity().mul_translate(4 * ONE, -2 * ONE, 0));

        v.set_tex_image_param(1 << 30);
        v.set_texcoord(0x0020_0010u32, &s);

        assert_eq!(v.current_tex, [0x14, 0x1E]);
    }

    #[test]
    fn test_texcoord_transform_mode_0_ignores_texture_matrix() {
        let mut v = VertexState::new();
        let mut s = ident_stacks();
        s.set_mode(MtxMode::Texture);
        s.load(Matrix::identity().mul_translate(4 * ONE, -2 * ONE, 0));

        v.set_tex_image_param(0);
        v.set_texcoord(0x0020_0010u32, &s);

        assert_eq!(v.current_tex, [0x10, 0x20]);
    }

    #[test]
    fn test_texcoord_transform_mode_1_uses_one_sixteenth_matrix_terms() {
        let mut v = VertexState::new();
        let mut s = ident_stacks();
        let mut m = Matrix::identity();
        // GBATEK mode 1 uses (S, T, 1/16, 1/16) times the left two matrix
        // columns, so m[8]/m[9] contribute one texcoord unit when set to 1.0.
        m.m[8] = ONE;
        m.m[9] = -2 * ONE;
        s.set_mode(MtxMode::Texture);
        s.load(m);

        v.set_tex_image_param(1 << 30);
        v.set_texcoord(0x0020_0010u32, &s);

        assert_eq!(v.current_tex, [0x11, 0x1E]);
    }

    #[test]
    fn test_texcoord_transform_mode_2_uses_normal_source() {
        let mut v = VertexState::new();
        let mut s = ident_stacks();
        s.set_mode(MtxMode::Texture);
        s.load(Matrix::identity().mul_scale(8 * ONE, 4 * ONE, ONE));

        v.set_tex_image_param(2 << 30);
        v.set_texcoord((20u32 << 16) | 10, &s);
        v.apply_normal_texcoord_transform([0x100, 0x100, 0], &s);

        assert_eq!(v.current_tex, [10 + (4 << 4), 20 + (2 << 4)]);
    }

    #[test]
    fn test_texcoord_transform_mode_2_replaces_matrix_bottom_row_with_texcoord() {
        let mut v = VertexState::new();
        let mut s = ident_stacks();
        let mut m = Matrix::identity().mul_scale(8 * ONE, 4 * ONE, ONE);
        // GBATEK: in mode 2 the bottom row is replaced by the most recent
        // TEXCOORD S/T values, so m[12]/m[13] must not contribute.
        m.m[12] = 100 * ONE;
        m.m[13] = -100 * ONE;
        s.set_mode(MtxMode::Texture);
        s.load(m);

        v.set_tex_image_param(2 << 30);
        v.set_texcoord((20u32 << 16) | 10, &s);
        v.apply_normal_texcoord_transform([0x100, 0x100, 0], &s);

        assert_eq!(v.current_tex, [10 + (4 << 4), 20 + (2 << 4)]);
    }

    #[test]
    fn test_texcoord_transform_mode_3_uses_vertex_source() {
        let mut v = VertexState::new();
        let mut s = ident_stacks();
        s.set_mode(MtxMode::Texture);
        s.load(Matrix::identity().mul_scale(2 * ONE, 3 * ONE, ONE));

        v.set_tex_image_param(3 << 30);
        v.set_texcoord(0, &s);
        v.begin(PrimitiveType::Triangles);
        for _ in 0..3 {
            v.submit_vertex([2 * ONE, 4 * ONE, 0], &s);
        }

        assert_eq!(v.polygon_buffer[0].vertices[0].tex, [4 << 4, 12 << 4]);
    }

    #[test]
    fn test_texcoord_transform_mode_3_replaces_matrix_bottom_row_with_texcoord() {
        let mut v = VertexState::new();
        let mut s = ident_stacks();
        let mut m = Matrix::identity().mul_scale(2 * ONE, 3 * ONE, ONE);
        // GBATEK: in mode 3 the bottom row is replaced by the most recent
        // TEXCOORD S/T values when each VTX command executes.
        m.m[12] = 100 * ONE;
        m.m[13] = -100 * ONE;
        s.set_mode(MtxMode::Texture);
        s.load(m);

        v.set_tex_image_param(3 << 30);
        v.set_texcoord((8u32 << 16) | 4, &s);
        v.begin(PrimitiveType::Triangles);
        for _ in 0..3 {
            v.submit_vertex([2 * ONE, 4 * ONE, 0], &s);
        }

        assert_eq!(
            v.polygon_buffer[0].vertices[0].tex,
            [4 + (4 << 4), 8 + (12 << 4)]
        );
    }

    #[test]
    fn test_polygon_attr_snapshot_per_polygon() {
        let mut v = VertexState::new();
        let s = ident_stacks();
        v.set_polygon_attr(0xAAAA_AAAA);
        assert_eq!(v.polygon_attr, 0);
        v.begin(PrimitiveType::Triangles);
        for _ in 0..3 {
            v.submit_vertex([0, 0, 0], &s);
        }
        assert_eq!(v.polygon_buffer[0].attr, 0xAAAA_AAAA);

        v.set_polygon_attr(0xBBBB_BBBB);
        v.begin(PrimitiveType::Triangles);
        for _ in 0..3 {
            v.submit_vertex([0, 0, 0], &s);
        }
        assert_eq!(v.polygon_buffer[1].attr, 0xBBBB_BBBB);
    }

    #[test]
    fn test_polygon_attr_write_during_list_defers_until_next_begin() {
        let mut v = VertexState::new();
        let s = ident_stacks();

        v.set_polygon_attr(0x1111_1111);
        v.begin(PrimitiveType::Triangles);
        v.submit_vertex([0, 0, 0], &s);
        v.set_polygon_attr(0x2222_2222);
        v.submit_vertex([0, 0, 0], &s);
        v.submit_vertex([0, 0, 0], &s);

        assert_eq!(v.polygon_buffer[0].attr, 0x1111_1111);

        v.begin(PrimitiveType::Triangles);
        for _ in 0..3 {
            v.submit_vertex([0, 0, 0], &s);
        }

        assert_eq!(v.polygon_buffer[1].attr, 0x2222_2222);
    }

    #[test]
    fn test_repeated_polygon_attr_writes_during_list_keep_only_last_pending_value() {
        let mut v = VertexState::new();
        let s = ident_stacks();

        v.set_polygon_attr(0x1111_1111);
        v.begin(PrimitiveType::Triangles);
        for _ in 0..3 {
            v.submit_vertex([0, 0, 0], &s);
        }
        assert_eq!(v.polygon_buffer[0].attr, 0x1111_1111);

        v.set_polygon_attr(0x2222_2222);
        v.set_polygon_attr(0x3333_3333);
        v.set_polygon_attr(0x4444_4444);
        for _ in 0..3 {
            v.submit_vertex([0, 0, 0], &s);
        }
        assert_eq!(v.polygon_buffer[1].attr, 0x1111_1111);

        v.begin(PrimitiveType::Triangles);
        for _ in 0..3 {
            v.submit_vertex([0, 0, 0], &s);
        }

        assert_eq!(v.polygon_buffer[2].attr, 0x4444_4444);
    }

    #[test]
    fn test_begin_vtxs_restarts_list_and_discards_partial_vertices() {
        let mut v = VertexState::new();
        let s = ident_stacks();

        v.begin(PrimitiveType::Triangles);
        v.submit_vertex([0, 0, 0], &s);
        v.submit_vertex([ONE, 0, 0], &s);

        v.begin(PrimitiveType::Triangles);
        v.submit_vertex([0, 0, 0], &s);
        v.submit_vertex([ONE, 0, 0], &s);
        v.submit_vertex([0, ONE, 0], &s);

        assert_eq!(v.polygon_buffer.len(), 1);
        assert_eq!(v.polygon_buffer[0].vertices[0].clip[0], 0);
        assert_eq!(v.polygon_buffer[0].vertices[1].clip[0], ONE);
        assert_eq!(v.polygon_buffer[0].vertices[2].clip[1], ONE);
    }

    #[test]
    fn test_strip_incomplete_list_detection_matches_primitive_vertex_counts() {
        let mut v = VertexState::new();
        let s = ident_stacks();

        v.begin(PrimitiveType::TriangleStrip);
        assert!(!v.has_incomplete_polygon_list());
        v.submit_vertex([0, 0, 0], &s);
        assert!(v.has_incomplete_polygon_list());
        v.submit_vertex([ONE, 0, 0], &s);
        assert!(v.has_incomplete_polygon_list());
        v.submit_vertex([0, ONE, 0], &s);
        assert!(!v.has_incomplete_polygon_list());
        v.submit_vertex([ONE, ONE, 0], &s);
        assert!(!v.has_incomplete_polygon_list());

        v.begin(PrimitiveType::QuadStrip);
        assert!(!v.has_incomplete_polygon_list());
        for i in 0..3 {
            v.submit_vertex([i * ONE, 0, 0], &s);
            assert!(v.has_incomplete_polygon_list());
        }
        v.submit_vertex([3 * ONE, 0, 0], &s);
        assert!(!v.has_incomplete_polygon_list());
        v.submit_vertex([4 * ONE, 0, 0], &s);
        assert!(v.has_incomplete_polygon_list());
        v.submit_vertex([5 * ONE, 0, 0], &s);
        assert!(!v.has_incomplete_polygon_list());
    }

    #[test]
    fn test_separate_triangles_snapshot_texture_per_polygon() {
        let mut v = VertexState::new();
        let s = ident_stacks();

        v.set_tex_image_param(0x1111);
        v.set_palette_base(0x0020);
        v.begin(PrimitiveType::Triangles);
        for _ in 0..3 {
            v.submit_vertex([0, 0, 0], &s);
        }

        v.set_tex_image_param(0x2222);
        v.set_palette_base(0x0040);
        for _ in 0..3 {
            v.submit_vertex([0, 0, 0], &s);
        }

        assert_eq!(v.polygon_buffer[0].tex_image_param, 0x1111);
        assert_eq!(v.polygon_buffer[0].palette_base, 0x0020);
        assert_eq!(v.polygon_buffer[1].tex_image_param, 0x2222);
        assert_eq!(v.polygon_buffer[1].palette_base, 0x0040);
    }

    #[test]
    fn test_triangle_strip_keeps_texture_state_from_begin() {
        let mut v = VertexState::new();
        let s = ident_stacks();

        v.set_tex_image_param(0x1111);
        v.set_palette_base(0x0020);
        v.begin(PrimitiveType::TriangleStrip);
        v.submit_vertex([0, 0, 0], &s);
        v.submit_vertex([ONE, 0, 0], &s);
        v.submit_vertex([0, ONE, 0], &s);

        v.set_tex_image_param(0x2222);
        v.set_palette_base(0x0040);
        v.submit_vertex([ONE, ONE, 0], &s);

        assert_eq!(v.polygon_buffer.len(), 2);
        assert_eq!(v.polygon_buffer[0].tex_image_param, 0x1111);
        assert_eq!(v.polygon_buffer[0].palette_base, 0x0020);
        assert_eq!(v.polygon_buffer[1].tex_image_param, 0x1111);
        assert_eq!(v.polygon_buffer[1].palette_base, 0x0020);

        v.begin(PrimitiveType::Triangles);
        for _ in 0..3 {
            v.submit_vertex([0, 0, 0], &s);
        }
        assert_eq!(v.polygon_buffer[2].tex_image_param, 0x2222);
        assert_eq!(v.polygon_buffer[2].palette_base, 0x0040);
    }

    #[test]
    fn test_palette_base_masks_to_thirteen_bits() {
        let mut v = VertexState::new();

        v.set_palette_base(0xFFFF);

        assert_eq!(v.palette_base, 0x1FFF);
    }

    #[test]
    fn test_submit_without_begin_is_ignored() {
        let mut v = VertexState::new();
        let s = ident_stacks();
        v.submit_vertex([ONE, 0, 0], &s);
        assert!(v.polygon_buffer.is_empty());
        assert!(v.vertex_buffer.is_empty());
        assert_eq!(v.last_pos, [0, 0, 0]);
    }

    #[test]
    fn test_end_vtxs_is_noop_inside_active_list() {
        let mut v = VertexState::new();
        let s = ident_stacks();

        v.begin(PrimitiveType::Triangles);
        v.submit_vertex([0, 0, 0], &s);
        v.end();
        v.submit_vertex([ONE, 0, 0], &s);
        v.submit_vertex([0, ONE, 0], &s);

        assert_eq!(v.polygon_buffer.len(), 1);
        assert!(v.list_active);
        assert_eq!(v.primitive, Some(PrimitiveType::Triangles));
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
        // VTX_DIFF deltas are already in 12-fractional-bit coordinate units.
        let param = 0x0001u32 | (0x3FFu32 << 10);
        let r = decode_vtx_diff(param, last);
        assert_eq!(r[0], last[0] + 1);
        assert_eq!(r[1], last[1] - 1);
        assert_eq!(r[2], last[2]);
    }

    #[test]
    fn test_vtx_diff_max_range_is_one_eighth_unit() {
        let last = [0, 0, 0];
        let param = 0x1FFu32 | (0x200u32 << 10) | (0x001u32 << 20);

        let r = decode_vtx_diff(param, last);

        assert_eq!(r, [0x1FF, -0x200, 1]);
        assert!(r[0] < ONE / 8, "positive max remains below +0.125");
        assert_eq!(r[1], -ONE / 8, "negative max is exactly -0.125");
    }
}
