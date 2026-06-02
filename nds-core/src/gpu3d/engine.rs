//! 3D geometry engine — ties together matrix stacks, vertex pipeline,
//! lighting, clipping, and viewport into a single state machine that
//! consumes `GxOp`s from the GXFIFO and produces `ScreenPolygon`s in the
//! "raster-side" buffer.
//!
//! The engine owns no I/O state itself; the bus dispatcher writes into
//! the GXFIFO and calls `drain_fifo()` after pushing each command. The
//! actual `0x04000400`/`0x04000440+` I/O wiring lives in `io_arm9.rs`.

use serde::{Deserialize, Serialize};

use super::clip::clip_polygon;
use super::command::GxCmd;
use super::fifo::{GxFifo, GxOp};
use super::lighting::{compute_vertex_color, LightingState};
use super::matrix::Matrix;
use super::raster::Rasterizer;
use super::stacks::{MatrixStacks, MtxMode};
use super::vertex::{
    decode_vtx10, decode_vtx16, decode_vtx_diff, decode_vtx_pair, PrimitiveType, Vertex,
    VertexState, VtxAxisPair,
};
use super::viewport::{transform_polygon, ScreenPolygon, Viewport};

/// Maximum polygons / vertices per frame, per GBATEK.
pub const POLYGON_BUF_LIMIT: usize = 2048;
pub const VERTEX_BUF_LIMIT: usize = 6144;

/// Full 3D engine state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Engine3d {
    pub stacks: MatrixStacks,
    pub vertex: VertexState,
    pub lighting: LightingState,
    pub fifo: GxFifo,

    pub viewport: Viewport,

    /// Geometry buffer: polygons under construction this frame.
    pub geometry_polygons: Vec<ScreenPolygon>,

    /// Raster buffer: last frame's geometry, swapped in by SWAP_BUFFERS,
    /// rasterized into `rasterizer.framebuffer` at the same time.
    pub raster_polygons: Vec<ScreenPolygon>,

    /// Set whenever `SWAP_BUFFERS` is queued; consumed and cleared at the
    /// next frame boundary (VBlank-end) by the top-level scheduler.
    pub swap_pending: bool,
    /// SWAP_BUFFERS attributes for the geometry currently being built. The
    /// command's bits apply to following geometry commands, not to the frame
    /// that is being swapped out.
    pub geometry_swap_attrs: u32,
    /// Latched SWAP_BUFFERS parameter to apply to the next empty geometry
    /// buffer after the pending VBlank swap.
    pub pending_swap_attrs: u32,
    /// Sticky geometry-engine lock-up caused by SWAP_BUFFERS with an
    /// incomplete polygon list. Real hardware keeps GXSTAT.27 busy forever.
    #[serde(default)]
    pub geometry_locked: bool,

    /// Results for POS_TEST (`0x04000620..=0x0400062F`).
    pub pos_test_result: [i32; 4],
    /// Results for VEC_TEST (`0x04000630..=0x04000635`).
    pub vec_test_result: [i16; 3],
    /// Result bit for BOX_TEST, exposed as GXSTAT bit 1.
    pub box_test_visible: bool,
    pub test_busy: bool,

    /// DISP_1DOT_DEPTH: zero-size polygon W-depth cutoff. This is an
    /// immediate geometry register, not routed through GXFIFO.
    pub disp_1dot_depth: u16,

    /// 3D rasterizer (Phase 7) — produces the 256×192 BGR555 framebuffer
    /// Engine A composites as BG0 when DISPCNT bit 3 is set.
    pub rasterizer: Rasterizer,
}

impl Engine3d {
    pub fn new() -> Self {
        Engine3d {
            stacks: MatrixStacks::new(),
            vertex: VertexState::new(),
            lighting: LightingState::new(),
            fifo: GxFifo::new(),
            viewport: Viewport::full_screen(),
            geometry_polygons: Vec::new(),
            raster_polygons: Vec::new(),
            swap_pending: false,
            geometry_swap_attrs: 0,
            pending_swap_attrs: 0,
            geometry_locked: false,
            pos_test_result: [0; 4],
            vec_test_result: [0; 3],
            box_test_visible: false,
            test_busy: false,
            disp_1dot_depth: 0,
            rasterizer: Rasterizer::new(),
        }
    }

    /// Drain every command currently `ready` in the FIFO. Called by the
    /// bus dispatcher after each write that might have completed a command.
    pub fn drain_fifo(&mut self) {
        while !self.swap_pending && !self.geometry_locked {
            let Some(op) = self.fifo.pop_op() else {
                break;
            };
            self.dispatch(op);
        }
        if !self.swap_pending && !self.geometry_locked {
            self.fifo.reconcile_after_drain();
        }
    }

    fn dispatch(&mut self, op: GxOp) {
        let cmd = match GxCmd::from_u8(op.cmd) {
            Some(c) => c,
            None => {
                log::trace!("gpu3d: ignoring unknown opcode 0x{:02X}", op.cmd);
                return;
            }
        };
        let params = op.params;
        let p0 = params.first().copied().unwrap_or(0);
        match cmd {
            // ─── Matrix commands ─────────────────────────────────
            GxCmd::MtxMode => self.stacks.set_mode(MtxMode::from_bits(p0)),
            GxCmd::MtxPush => self.stacks.push(),
            GxCmd::MtxPop => self.stacks.pop(p0),
            GxCmd::MtxStore => self.stacks.store(p0),
            GxCmd::MtxRestore => self.stacks.restore(p0),
            GxCmd::MtxIdentity => self.stacks.identity(),
            GxCmd::MtxLoad4x4 => {
                let mut words = [0i32; 16];
                for i in 0..16 {
                    words[i] = params.get(i).copied().unwrap_or(0) as i32;
                }
                self.stacks.load(Matrix::load_4x4(&words));
            }
            GxCmd::MtxLoad4x3 => {
                let mut words = [0i32; 12];
                for i in 0..12 {
                    words[i] = params.get(i).copied().unwrap_or(0) as i32;
                }
                self.stacks.load(Matrix::load_4x3(&words));
            }
            GxCmd::MtxMult4x4 => {
                let mut words = [0i32; 16];
                for i in 0..16 {
                    words[i] = params.get(i).copied().unwrap_or(0) as i32;
                }
                self.stacks.mult(Matrix::load_4x4(&words));
            }
            GxCmd::MtxMult4x3 => {
                let mut words = [0i32; 12];
                for i in 0..12 {
                    words[i] = params.get(i).copied().unwrap_or(0) as i32;
                }
                self.stacks.mult(Matrix::load_4x3(&words));
            }
            GxCmd::MtxMult3x3 => {
                let mut words = [0i32; 9];
                for i in 0..9 {
                    words[i] = params.get(i).copied().unwrap_or(0) as i32;
                }
                self.stacks.mult(Matrix::load_3x3(&words));
            }
            GxCmd::MtxScale => {
                let sx = params.first().copied().unwrap_or(0) as i32;
                let sy = params.get(1).copied().unwrap_or(0) as i32;
                let sz = params.get(2).copied().unwrap_or(0) as i32;
                self.stacks.scale(sx, sy, sz);
            }
            GxCmd::MtxTrans => {
                let tx = params.first().copied().unwrap_or(0) as i32;
                let ty = params.get(1).copied().unwrap_or(0) as i32;
                let tz = params.get(2).copied().unwrap_or(0) as i32;
                self.stacks.translate(tx, ty, tz);
            }

            // ─── Vertex attributes ────────────────────────────────
            GxCmd::Color => self.vertex.set_color(p0),
            GxCmd::Normal => self.handle_normal(p0),
            GxCmd::TexCoord => self.vertex.set_texcoord(p0, &self.stacks),
            GxCmd::Vtx16 => {
                let p = [
                    params.first().copied().unwrap_or(0),
                    params.get(1).copied().unwrap_or(0),
                ];
                let pos = decode_vtx16(p);
                self.submit_vertex(pos);
            }
            GxCmd::Vtx10 => {
                let pos = decode_vtx10(p0);
                self.submit_vertex(pos);
            }
            GxCmd::VtxXY => {
                let last = self.vertex.last_pos;
                let pos = decode_vtx_pair(p0, last, VtxAxisPair::XY);
                self.submit_vertex(pos);
            }
            GxCmd::VtxXZ => {
                let last = self.vertex.last_pos;
                let pos = decode_vtx_pair(p0, last, VtxAxisPair::XZ);
                self.submit_vertex(pos);
            }
            GxCmd::VtxYZ => {
                let last = self.vertex.last_pos;
                let pos = decode_vtx_pair(p0, last, VtxAxisPair::YZ);
                self.submit_vertex(pos);
            }
            GxCmd::VtxDiff => {
                let last = self.vertex.last_pos;
                let pos = decode_vtx_diff(p0, last);
                self.submit_vertex(pos);
            }
            GxCmd::PolygonAttr => self.vertex.set_polygon_attr(p0),
            GxCmd::TexImageParm => self.vertex.set_tex_image_param(p0),
            GxCmd::PltBase => self.vertex.set_palette_base(p0),

            // ─── Lighting / materials ─────────────────────────────
            GxCmd::DifAmb => self.handle_dif_amb(p0),
            GxCmd::SpeEmi => self.lighting.set_spe_emi(p0),
            GxCmd::LightVector => {
                let vec_mat = self.stacks.vector;
                self.lighting.set_light_vector(p0, &vec_mat);
            }
            GxCmd::LightColor => self.lighting.set_light_color(p0),
            GxCmd::Shininess => self.lighting.set_shininess(&params),

            // ─── Geometry control ─────────────────────────────────
            GxCmd::BeginVtxs => self.vertex.begin(PrimitiveType::from_bits(p0)),
            GxCmd::EndVtxs => self.vertex.end(),
            GxCmd::SwapBuffers => {
                if self.vertex.has_incomplete_polygon_list() {
                    self.geometry_locked = true;
                    return;
                }
                self.swap_pending = true;
                self.pending_swap_attrs = p0;
                self.vertex.force_end();
            }
            GxCmd::Viewport => self.viewport = Viewport::from_param(p0),

            // ─── Test commands ────────────────────────────────────
            GxCmd::BoxTest => self.handle_box_test(&params),
            GxCmd::PosTest => self.handle_pos_test(&params),
            GxCmd::VecTest => self.handle_vec_test(p0),
        }

        // After every command, drain any newly-completed polygons through
        // clipping + viewport into the geometry buffer.
        self.flush_polygons();
    }

    fn handle_normal(&mut self, param: u32) {
        // NORMAL: 10-bit signed (x, y, z) in the low 30 bits. Executing this
        // command recalculates the current vertex color from light/material
        // state before the next VTX_*.
        let sign_ext = |b: u32| -> i32 { (((b & 0x3FF) << 22) as i32) >> 22 };
        let nx = sign_ext(param) << 3; // 1.0.9 -> 1.0.12
        let ny = sign_ext(param >> 10) << 3;
        let nz = sign_ext(param >> 20) << 3;
        self.vertex.apply_normal_texcoord_transform(
            [
                sign_ext(param),
                sign_ext(param >> 10),
                sign_ext(param >> 20),
            ],
            &self.stacks,
        );

        // Compute lit color now and set as the vertex color.
        let attr = self.vertex.polygon_attr;
        let light_enable = (attr & 0xF) as u8;
        let vec_mat = self.stacks.vector;
        let color = compute_vertex_color(&self.lighting, [nx, ny, nz], &vec_mat, light_enable);
        self.vertex.current_color = color;
    }

    fn handle_dif_amb(&mut self, param: u32) {
        self.lighting.set_dif_amb(param);
        if param & (1 << 15) != 0 {
            self.vertex.current_color = (param & 0x7FFF) as u16;
        }
    }

    fn submit_vertex(&mut self, pos: [i32; 3]) {
        self.vertex.submit_vertex(pos, &self.stacks);
    }

    fn handle_pos_test(&mut self, params: &[u32]) {
        let p0 = params.first().copied().unwrap_or(0);
        let p1 = params.get(1).copied().unwrap_or(0);
        let pos = decode_vtx16([p0, p1]);
        self.vertex.last_pos = pos;
        self.pos_test_result =
            self.stacks
                .clip_matrix()
                .mul_vec4([pos[0], pos[1], pos[2], super::matrix::ONE]);
        self.test_busy = false;
    }

    fn handle_vec_test(&mut self, param: u32) {
        if self.stacks.mode != MtxMode::PosVector {
            self.test_busy = false;
            return;
        }

        let sign_ext = |b: u32| -> i32 { (((b & 0x3FF) << 22) as i32) >> 22 };
        let x = sign_ext(param) << 3;
        let y = sign_ext(param >> 10) << 3;
        let z = sign_ext(param >> 20) << 3;
        let out = self.stacks.vector.mul_vec4([x, y, z, 0]);
        self.vec_test_result = [
            format_vec_test_result(out[0]),
            format_vec_test_result(out[1]),
            format_vec_test_result(out[2]),
        ];
        self.test_busy = false;
    }

    fn handle_box_test(&mut self, params: &[u32]) {
        let p0 = params.first().copied().unwrap_or(0);
        let p1 = params.get(1).copied().unwrap_or(0);
        let p2 = params.get(2).copied().unwrap_or(0);
        let origin = [
            (p0 as i16) as i32,
            (p0 >> 16) as i16 as i32,
            (p1 as i16) as i32,
        ];
        let size = [
            (p1 >> 16) as i16 as i32,
            (p2 as i16) as i32,
            (p2 >> 16) as i16 as i32,
        ];
        let clip = self.stacks.clip_matrix();
        self.box_test_visible = box_intersects_view_volume(origin, size, &clip);
        self.test_busy = false;
    }

    /// Move any newly-assembled polygons from `vertex.polygon_buffer`
    /// through clipping + viewport into `geometry_polygons`. Respects the
    /// per-frame caps.
    fn flush_polygons(&mut self) {
        let polys: Vec<_> = self.vertex.polygon_buffer.drain(..).collect();
        for poly in polys {
            if self.geometry_polygons.len() >= POLYGON_BUF_LIMIT {
                self.fifo.overflow = true;
                continue;
            }
            if poly.attr & (1 << 12) == 0 && intersects_far_plane(&poly.vertices) {
                continue;
            }
            let clipped: Vec<Vertex> = match clip_polygon(&poly.vertices) {
                Some(v) => v,
                None => continue, // fully outside; discard
            };
            if self.geometry_vertex_count() + clipped.len() > VERTEX_BUF_LIMIT {
                self.fifo.overflow = true;
                continue;
            }
            // Build a clipped Polygon, then viewport-transform.
            let clipped_poly = super::vertex::Polygon {
                vertices: clipped,
                attr: poly.attr,
                tex_image_param: poly.tex_image_param,
                palette_base: poly.palette_base,
                front_area_negative: poly.front_area_negative,
            };
            let screen = transform_polygon(&clipped_poly, self.viewport);
            if zero_dot_polygon_is_hidden(&screen, self.disp_1dot_depth) {
                continue;
            }
            self.geometry_polygons.push(screen);
        }
    }

    /// Frame-boundary swap. Called by the top-level scheduler at VBlank end
    /// when `swap_pending` is true. Moves geometry buffer to raster buffer
    /// and re-rasterizes the frame.
    ///
    /// `vram` is `None` when the caller doesn't have VRAM handy (unit
    /// tests); textures render as transparent in that case but per-vertex
    /// color + post-effects still work.
    pub fn swap_buffers(&mut self, vram: Option<&crate::vram::VramRouter>) {
        if self.geometry_locked || !self.swap_pending {
            return;
        }
        self.raster_polygons = std::mem::take(&mut self.geometry_polygons);
        self.swap_pending = false;
        self.rasterizer.set_swap_attrs(self.geometry_swap_attrs);
        self.rasterizer.render_frame(&self.raster_polygons, vram);
        self.geometry_swap_attrs = self.pending_swap_attrs;
        self.drain_fifo();
    }

    /// Read a pixel from the 3D framebuffer. Engine A's BG0 path calls
    /// this when DISPCNT bit 3 is set.
    pub fn read_3d_pixel(&self, x: usize, y: usize) -> u16 {
        let idx = y * super::raster::FB_WIDTH + x;
        self.rasterizer.framebuffer.get(idx).copied().unwrap_or(0)
    }

    pub fn read_disp3dcnt(&self) -> u16 {
        let mut value = self.rasterizer.disp3dcnt;
        if self.fifo.overflow {
            value |= 1 << 13;
        }
        value
    }

    pub fn write_disp3dcnt(&mut self, value: u16) {
        self.rasterizer.disp3dcnt = value & 0x4FFF;
        if value & (1 << 13) != 0 {
            self.fifo.overflow = false;
        }
    }

    pub fn gxstat_low(&self) -> u16 {
        let mut v = 0u16;
        if self.test_busy {
            v |= 1;
        }
        if self.box_test_visible {
            v |= 1 << 1;
        }
        v |= ((self.stacks.position_sp as u16) & 0x1F) << 8;
        if self.stacks.projection_sp != 0 {
            v |= 1 << 13;
        }
        if self.stacks.overflow {
            v |= 1 << 15;
        }
        v
    }

    pub fn gxstat_high(&self) -> u16 {
        self.fifo.gxstat_high_bits(self.geometry_busy())
    }

    pub fn gxstat(&self) -> u32 {
        self.gxstat_low() as u32 | ((self.gxstat_high() as u32) << 16)
    }

    pub fn write_gxstat(&mut self, value: u32) {
        self.write_gxstat_low(value as u16);
        self.write_gxstat_high((value >> 16) as u16);
    }

    pub fn write_gxstat_low(&mut self, value: u16) {
        if value & (1 << 15) != 0 {
            self.stacks.clear_overflow_error();
        }
    }

    pub fn write_gxstat_high(&mut self, value: u16) {
        self.fifo.set_irq_mode((value >> 14) as u8);
    }

    pub fn ram_count(&self) -> u32 {
        let polygons = self.geometry_polygons.len().min(0x0FFF) as u32;
        let vertices = self
            .geometry_polygons
            .iter()
            .map(|p| p.vertices.len())
            .sum::<usize>()
            .min(0x1FFF) as u32;
        polygons | (vertices << 16)
    }

    pub fn read_clip_matrix_word(&self, index: usize) -> u32 {
        if self.geometry_busy() {
            return 0;
        }
        let m = self.stacks.clip_matrix();
        m.m.get(index).copied().unwrap_or(0) as u32
    }

    pub fn read_direction_matrix_word(&self, index: usize) -> u32 {
        if self.geometry_busy() {
            return 0;
        }
        let row = index / 3;
        let col = index % 3;
        if row < 3 && col < 3 {
            self.stacks.vector.at(row, col) as u32
        } else {
            0
        }
    }

    pub fn read_pos_test_word(&self, index: usize) -> u32 {
        self.pos_test_result.get(index).copied().unwrap_or(0) as u32
    }

    pub fn read_vec_test_halfword(&self, index: usize) -> u16 {
        self.vec_test_result.get(index).copied().unwrap_or(0) as u16
    }

    fn geometry_busy(&self) -> bool {
        self.geometry_locked
            || self.swap_pending
            || !self.fifo.is_empty()
            || !self.vertex.polygon_buffer.is_empty()
    }

    fn geometry_vertex_count(&self) -> usize {
        self.geometry_polygons
            .iter()
            .map(|p| p.vertices.len())
            .sum()
    }
}

fn format_vec_test_result(v: i32) -> i16 {
    let raw13 = (v as u32) & 0x1FFF;
    let sign_extended = if raw13 & 0x1000 != 0 {
        raw13 | 0xE000
    } else {
        raw13
    };
    sign_extended as u16 as i16
}

fn intersects_far_plane(vertices: &[Vertex]) -> bool {
    vertices.iter().any(|v| v.clip[2] > v.clip[3])
}

fn zero_dot_polygon_is_hidden(poly: &ScreenPolygon, disp_1dot_depth: u16) -> bool {
    if poly.attr & (1 << 13) != 0 {
        return false;
    }
    let Some(first) = poly.vertices.first() else {
        return false;
    };
    let first_x = first.screen_x >> 8;
    let first_y = first.screen_y >> 8;
    let is_zero_dot = poly
        .vertices
        .iter()
        .all(|v| (v.screen_x >> 8) == first_x && (v.screen_y >> 8) == first_y);
    if !is_zero_dot {
        return false;
    }

    let boundary_w = ((disp_1dot_depth & 0x7FFF) as i32) << 9;
    !poly.vertices.iter().any(|v| v.w <= boundary_w)
}

fn box_intersects_view_volume(origin: [i32; 3], size: [i32; 3], clip: &Matrix) -> bool {
    let x0 = origin[0];
    let x1 = origin[0] + size[0];
    let y0 = origin[1];
    let y1 = origin[1] + size[1];
    let z0 = origin[2];
    let z1 = origin[2] + size[2];

    let corner_points = [
        [x0, y0, z0, super::matrix::ONE],
        [x1, y0, z0, super::matrix::ONE],
        [x0, y1, z0, super::matrix::ONE],
        [x1, y1, z0, super::matrix::ONE],
        [x0, y0, z1, super::matrix::ONE],
        [x1, y0, z1, super::matrix::ONE],
        [x0, y1, z1, super::matrix::ONE],
        [x1, y1, z1, super::matrix::ONE],
    ];
    let corners = corner_points.map(|point| {
        let v = clip.mul_vec4(point);
        [v[0] as i64, v[1] as i64, v[2] as i64, v[3] as i64]
    });

    // Fast reject against each frustum plane. If all cuboid corners are
    // outside any one plane, no part of the cuboid can be visible. If this
    // never happens, keep testing rather than rejecting; a cuboid may fully
    // enclose the view volume without any cuboid face clipping into it.
    for plane in 0..6 {
        if corners
            .iter()
            .all(|&corner| clip_plane_value(corner, plane) < 0)
        {
            return false;
        }
    }

    let faces = [
        [0, 1, 3, 2],
        [4, 6, 7, 5],
        [0, 4, 5, 1],
        [2, 3, 7, 6],
        [0, 2, 6, 4],
        [1, 5, 7, 3],
    ];

    if faces.iter().any(|face| {
        let polygon = face.iter().map(|&idx| corners[idx]).collect::<Vec<_>>();
        clip_homogeneous_polygon_to_view_volume(polygon)
    }) {
        return true;
    }

    // No cuboid face reached the frustum, but no frustum plane rejected the
    // cuboid either. This covers boxes enclosing the view volume.
    true
}

fn clip_homogeneous_polygon_to_view_volume(mut polygon: Vec<[i64; 4]>) -> bool {
    for plane in 0..6 {
        if polygon.is_empty() {
            return false;
        }

        let mut clipped = Vec::with_capacity(polygon.len() + 1);
        let mut prev = *polygon.last().unwrap();
        let mut prev_value = clip_plane_value(prev, plane);
        let mut prev_inside = prev_value >= 0;

        for &current in &polygon {
            let current_value = clip_plane_value(current, plane);
            let current_inside = current_value >= 0;

            if current_inside != prev_inside {
                clipped.push(interpolate_clip_vertex(
                    prev,
                    current,
                    prev_value,
                    current_value,
                ));
            }
            if current_inside {
                clipped.push(current);
            }

            prev = current;
            prev_value = current_value;
            prev_inside = current_inside;
        }

        polygon = clipped;
    }

    !polygon.is_empty()
}

fn clip_plane_value(v: [i64; 4], plane: usize) -> i64 {
    match plane {
        0 => v[0] + v[3],
        1 => v[3] - v[0],
        2 => v[1] + v[3],
        3 => v[3] - v[1],
        4 => v[2] + v[3],
        5 => v[3] - v[2],
        _ => unreachable!(),
    }
}

fn interpolate_clip_vertex(a: [i64; 4], b: [i64; 4], a_value: i64, b_value: i64) -> [i64; 4] {
    let denominator = a_value - b_value;
    if denominator == 0 {
        return a;
    }

    let mut out = [0; 4];
    for i in 0..4 {
        out[i] = a[i] + (b[i] - a[i]) * a_value / denominator;
    }
    out
}

impl Default for Engine3d {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::matrix::ONE;
    use super::super::vertex::{Polygon, Vertex};
    use super::*;

    #[test]
    fn test_full_pipeline_triangle_to_screen() {
        let mut e = Engine3d::new();

        // BEGIN_VTXS triangles
        e.dispatch(GxOp {
            cmd: GxCmd::BeginVtxs as u8,
            params: vec![0],
        });
        // Three vertices forming a triangle in the canonical view volume.
        // VTX_16 packed format: y << 16 | x, _ << 16 | z.
        // Vertex 1: (0, 0, 0.5)
        e.dispatch(GxOp {
            cmd: GxCmd::Vtx16 as u8,
            params: vec![
                0,                         // y=0, x=0
                (ONE / 2) as u32 & 0xFFFF, // z=0.5
            ],
        });
        e.dispatch(GxOp {
            cmd: GxCmd::Vtx16 as u8,
            params: vec![
                (((ONE / 4) as u32) & 0xFFFF), // y=0, x=0.25
                (ONE / 2) as u32 & 0xFFFF,
            ],
        });
        e.dispatch(GxOp {
            cmd: GxCmd::Vtx16 as u8,
            params: vec![
                ((ONE / 4) as u32 & 0xFFFF) << 16, // y=0.25, x=0
                (ONE / 2) as u32 & 0xFFFF,
            ],
        });

        // Buffer should have one screen polygon.
        assert_eq!(e.geometry_polygons.len(), 1);
        let poly = &e.geometry_polygons[0];
        assert_eq!(poly.vertices.len(), 3);
        // All three vertices land within the screen rectangle.
        for v in &poly.vertices {
            assert!(v.screen_x >= 0);
            assert!(v.screen_y >= 0);
        }
    }

    #[test]
    fn test_swap_buffers_moves_geometry_to_raster() {
        let mut e = Engine3d::new();
        e.dispatch(GxOp {
            cmd: GxCmd::PolygonAttr as u8,
            params: vec![(1 << 13) | (0x1F << 16) | (1 << 6) | (1 << 7)],
        });
        // Run a zero-dot triangle and opt it out of DISP_1DOT_DEPTH culling.
        e.dispatch(GxOp {
            cmd: GxCmd::BeginVtxs as u8,
            params: vec![0],
        });
        for _ in 0..3 {
            e.dispatch(GxOp {
                cmd: GxCmd::Vtx16 as u8,
                params: vec![0, (ONE / 2) as u32 & 0xFFFF],
            });
        }
        assert_eq!(e.geometry_polygons.len(), 1);
        assert!(e.raster_polygons.is_empty());

        // Queue swap, then trigger.
        e.dispatch(GxOp {
            cmd: GxCmd::SwapBuffers as u8,
            params: vec![0],
        });
        assert!(e.swap_pending);
        e.swap_buffers(None);
        assert!(!e.swap_pending);
        assert_eq!(e.raster_polygons.len(), 1);
        assert!(e.geometry_polygons.is_empty());
    }

    #[test]
    fn test_swap_buffers_attrs_apply_to_following_geometry() {
        let mut e = Engine3d::new();

        e.dispatch(GxOp {
            cmd: GxCmd::SwapBuffers as u8,
            params: vec![2],
        });
        e.swap_buffers(None);

        assert!(!e.rasterizer.w_buffering);
        assert_eq!(e.geometry_swap_attrs, 2);

        e.dispatch(GxOp {
            cmd: GxCmd::SwapBuffers as u8,
            params: vec![0],
        });
        e.swap_buffers(None);

        assert!(e.rasterizer.w_buffering);
        assert_eq!(e.geometry_swap_attrs, 0);
    }

    #[test]
    fn test_swap_buffers_stalls_following_geometry_until_vblank_swap() {
        let mut e = Engine3d::new();
        let attr = (1 << 13) | (0x1F << 16) | (1 << 6) | (1 << 7);

        e.dispatch(GxOp {
            cmd: GxCmd::PolygonAttr as u8,
            params: vec![attr],
        });
        e.dispatch(GxOp {
            cmd: GxCmd::BeginVtxs as u8,
            params: vec![0],
        });
        for _ in 0..3 {
            e.dispatch(GxOp {
                cmd: GxCmd::Vtx16 as u8,
                params: vec![0, (ONE / 2) as u32 & 0xFFFF],
            });
        }
        assert_eq!(e.geometry_polygons.len(), 1);

        e.dispatch(GxOp {
            cmd: GxCmd::SwapBuffers as u8,
            params: vec![0],
        });
        e.fifo.ready.push_back(GxOp {
            cmd: GxCmd::PolygonAttr as u8,
            params: vec![attr],
        });
        e.fifo.ready.push_back(GxOp {
            cmd: GxCmd::BeginVtxs as u8,
            params: vec![0],
        });
        for _ in 0..3 {
            e.fifo.ready.push_back(GxOp {
                cmd: GxCmd::Vtx16 as u8,
                params: vec![0, (ONE / 2) as u32 & 0xFFFF],
            });
        }

        e.drain_fifo();
        assert_eq!(e.geometry_polygons.len(), 1);
        assert_eq!(e.fifo.ready.len(), 5);

        e.swap_buffers(None);

        assert_eq!(e.raster_polygons.len(), 1);
        assert_eq!(e.geometry_polygons.len(), 1);
        assert!(e.fifo.ready.is_empty());
    }

    #[test]
    fn test_swap_buffers_with_incomplete_polygon_list_locks_geometry() {
        let mut e = Engine3d::new();
        e.dispatch(GxOp {
            cmd: GxCmd::BeginVtxs as u8,
            params: vec![0],
        });
        for _ in 0..2 {
            e.dispatch(GxOp {
                cmd: GxCmd::Vtx16 as u8,
                params: vec![0, (ONE / 2) as u32 & 0xFFFF],
            });
        }

        e.dispatch(GxOp {
            cmd: GxCmd::SwapBuffers as u8,
            params: vec![0],
        });

        assert!(e.geometry_locked);
        assert!(!e.swap_pending);
        assert_eq!(e.gxstat() & (1 << 27), 1 << 27);
    }

    #[test]
    fn test_locked_geometry_engine_stops_draining_fifo() {
        let mut e = Engine3d::new();
        e.geometry_locked = true;
        e.fifo.ready.push_back(GxOp {
            cmd: GxCmd::MtxMode as u8,
            params: vec![1],
        });

        e.drain_fifo();

        assert_eq!(e.fifo.ready.len(), 1);
        assert!(matches!(e.stacks.mode, MtxMode::Projection));
    }

    #[test]
    fn test_end_vtxs_command_is_noop_for_vertex_submission() {
        let mut e = Engine3d::new();
        e.dispatch(GxOp {
            cmd: GxCmd::PolygonAttr as u8,
            params: vec![(1 << 13) | (0x1F << 16) | (1 << 6) | (1 << 7)],
        });
        e.dispatch(GxOp {
            cmd: GxCmd::BeginVtxs as u8,
            params: vec![0],
        });
        e.dispatch(GxOp {
            cmd: GxCmd::Vtx16 as u8,
            params: vec![0, (ONE / 2) as u32 & 0xFFFF],
        });
        e.dispatch(GxOp {
            cmd: GxCmd::EndVtxs as u8,
            params: vec![],
        });
        e.dispatch(GxOp {
            cmd: GxCmd::Vtx16 as u8,
            params: vec![0, (ONE / 2) as u32 & 0xFFFF],
        });
        e.dispatch(GxOp {
            cmd: GxCmd::Vtx16 as u8,
            params: vec![0, (ONE / 2) as u32 & 0xFFFF],
        });

        assert_eq!(e.geometry_polygons.len(), 1);
        assert!(e.vertex.list_active);
        assert_eq!(e.vertex.primitive, Some(PrimitiveType::Triangles));
    }

    #[test]
    fn test_end_vtxs_does_not_hide_incomplete_list_from_swap_lock() {
        let mut e = Engine3d::new();
        e.dispatch(GxOp {
            cmd: GxCmd::BeginVtxs as u8,
            params: vec![0],
        });
        for _ in 0..2 {
            e.dispatch(GxOp {
                cmd: GxCmd::Vtx16 as u8,
                params: vec![0, (ONE / 2) as u32 & 0xFFFF],
            });
        }
        e.dispatch(GxOp {
            cmd: GxCmd::EndVtxs as u8,
            params: vec![],
        });

        e.dispatch(GxOp {
            cmd: GxCmd::SwapBuffers as u8,
            params: vec![0],
        });

        assert!(e.geometry_locked);
        assert!(!e.swap_pending);
    }

    #[test]
    fn test_gxstat_busy_ignores_stored_geometry_but_reports_pending_swap() {
        let mut e = Engine3d::new();
        e.dispatch(GxOp {
            cmd: GxCmd::PolygonAttr as u8,
            params: vec![(1 << 13) | (0x1F << 16) | (1 << 6) | (1 << 7)],
        });
        e.dispatch(GxOp {
            cmd: GxCmd::BeginVtxs as u8,
            params: vec![0],
        });
        for _ in 0..3 {
            e.dispatch(GxOp {
                cmd: GxCmd::Vtx16 as u8,
                params: vec![0, (ONE / 2) as u32 & 0xFFFF],
            });
        }

        assert_eq!(e.geometry_polygons.len(), 1);
        assert_eq!(e.gxstat() & (1 << 27), 0);

        e.dispatch(GxOp {
            cmd: GxCmd::SwapBuffers as u8,
            params: vec![0],
        });
        assert!(e.swap_pending);
        assert_eq!(e.gxstat() & (1 << 27), 1 << 27);
    }

    #[test]
    fn test_matrix_command_dispatches_to_stacks() {
        let mut e = Engine3d::new();
        e.dispatch(GxOp {
            cmd: GxCmd::MtxMode as u8,
            params: vec![1],
        });
        assert!(matches!(e.stacks.mode, MtxMode::Position));
    }

    #[test]
    fn test_viewport_command_updates_viewport() {
        let mut e = Engine3d::new();
        let param = (180u32 << 24) | (100u32 << 16) | (20u32 << 8) | 10u32;
        e.dispatch(GxOp {
            cmd: GxCmd::Viewport as u8,
            params: vec![param],
        });
        assert_eq!(e.viewport.x1, 10);
        assert_eq!(e.viewport.x2, 100);
    }

    #[test]
    fn test_dif_amb_bit15_sets_current_vertex_color() {
        let mut e = Engine3d::new();

        e.dispatch(GxOp {
            cmd: GxCmd::DifAmb as u8,
            params: vec![(1 << 15) | 0x1234],
        });

        assert_eq!(e.lighting.mat_diffuse, 0x1234);
        assert_eq!(e.vertex.current_color, 0x1234);
    }

    #[test]
    fn test_dif_amb_without_bit15_preserves_current_vertex_color() {
        let mut e = Engine3d::new();
        e.vertex.current_color = 0x7FFF;

        e.dispatch(GxOp {
            cmd: GxCmd::DifAmb as u8,
            params: vec![0x1234],
        });

        assert_eq!(e.lighting.mat_diffuse, 0x1234);
        assert_eq!(e.vertex.current_color, 0x7FFF);
    }

    #[test]
    fn test_far_plane_intersecting_polygon_requires_attr_bit() {
        let mut e = Engine3d::new();
        let poly = Polygon {
            vertices: vec![
                Vertex::new([0, 0, ONE / 2, ONE], 0x001F, [0, 0]),
                Vertex::new([ONE / 2, 0, ONE / 2, ONE], 0x001F, [0, 0]),
                Vertex::new([0, ONE / 2, 2 * ONE, ONE], 0x001F, [0, 0]),
            ],
            attr: (0x1F << 16) | (1 << 6) | (1 << 7),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        };

        e.vertex.polygon_buffer.push(poly.clone());
        e.flush_polygons();
        assert!(e.geometry_polygons.is_empty());

        let mut with_far_bit = poly;
        with_far_bit.attr |= 1 << 12;
        e.vertex.polygon_buffer.push(with_far_bit);
        e.flush_polygons();
        assert_eq!(e.geometry_polygons.len(), 1);
    }

    #[test]
    fn test_box_clip_near_plane_uses_negative_w_boundary() {
        let inside_near = [0, 0, -ONE as i64, ONE as i64];
        let behind_near = [0, 0, -(ONE as i64) - 1, ONE as i64];

        assert_eq!(clip_plane_value(inside_near, 4), 0);
        assert!(clip_plane_value(behind_near, 4) < 0);
    }

    #[test]
    fn test_disp_1dot_depth_hides_distant_zero_dot_polygon() {
        let mut e = Engine3d::new();
        e.disp_1dot_depth = 0;
        let poly = Polygon {
            vertices: vec![
                Vertex::new([0, 0, ONE / 2, ONE], 0x001F, [0, 0]),
                Vertex::new([0, 0, ONE / 2, ONE], 0x001F, [0, 0]),
                Vertex::new([0, 0, ONE / 2, ONE], 0x001F, [0, 0]),
            ],
            attr: (0x1F << 16) | (1 << 6) | (1 << 7),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        };

        e.vertex.polygon_buffer.push(poly);
        e.flush_polygons();

        assert!(e.geometry_polygons.is_empty());
    }

    #[test]
    fn test_polygon_attr_bit13_keeps_zero_dot_polygon() {
        let mut e = Engine3d::new();
        e.disp_1dot_depth = 0;
        let poly = Polygon {
            vertices: vec![
                Vertex::new([0, 0, ONE / 2, ONE], 0x001F, [0, 0]),
                Vertex::new([0, 0, ONE / 2, ONE], 0x001F, [0, 0]),
                Vertex::new([0, 0, ONE / 2, ONE], 0x001F, [0, 0]),
            ],
            attr: (0x1F << 16) | (1 << 6) | (1 << 7) | (1 << 13),
            tex_image_param: 0,
            palette_base: 0,
            front_area_negative: true,
        };

        e.vertex.polygon_buffer.push(poly);
        e.flush_polygons();

        assert_eq!(e.geometry_polygons.len(), 1);
    }

    #[test]
    fn test_box_test_reports_box_intersecting_frustum_edge() {
        let clip = Matrix::identity();
        let origin = [-2 * ONE, -ONE / 2, ONE / 2];
        let size = [3 * ONE, ONE, ONE / 4];

        assert!(box_intersects_view_volume(origin, size, &clip));
    }

    #[test]
    fn test_box_test_rejects_box_outside_single_clip_plane() {
        let clip = Matrix::identity();
        let origin = [2 * ONE, -ONE / 2, ONE / 2];
        let size = [ONE / 2, ONE, ONE / 4];

        assert!(!box_intersects_view_volume(origin, size, &clip));
    }

    #[test]
    fn test_box_test_reports_box_enclosing_view_volume() {
        let clip = Matrix::identity();
        let origin = [-2 * ONE, -2 * ONE, -2 * ONE];
        let size = [4 * ONE, 4 * ONE, 4 * ONE];

        assert!(box_intersects_view_volume(origin, size, &clip));
    }

    #[test]
    fn test_vec_test_result_uses_4_sign_bits_and_12_fraction_bits() {
        assert_eq!(format_vec_test_result(0x07FF) as u16, 0x07FF);
        assert_eq!(format_vec_test_result(0x0800) as u16, 0x0800);
        assert_eq!(format_vec_test_result(-0x0800) as u16, 0xF800);
        assert_eq!(format_vec_test_result(0x1000) as u16, 0xF000);
    }

    #[test]
    fn test_vec_test_readback_wraps_overflowed_unit_vector() {
        let mut e = Engine3d::new();
        e.stacks.set_mode(MtxMode::PosVector);
        e.stacks.vector = Matrix::identity().mul_scale(2 * ONE, ONE, ONE);

        e.handle_vec_test(0x100);

        assert_eq!(e.read_vec_test_halfword(0), 0xF000);
        assert_eq!(e.read_vec_test_halfword(1), 0);
        assert_eq!(e.read_vec_test_halfword(2), 0);
    }

    #[test]
    fn test_vec_test_requires_pos_vector_matrix_mode() {
        let mut e = Engine3d::new();
        e.vec_test_result = [1, 2, 3];

        e.handle_vec_test(1);

        assert_eq!(e.vec_test_result, [1, 2, 3]);

        e.stacks.set_mode(MtxMode::PosVector);
        e.handle_vec_test(1);

        assert_eq!(e.read_vec_test_halfword(0), 8);
        assert_eq!(e.read_vec_test_halfword(1), 0);
        assert_eq!(e.read_vec_test_halfword(2), 0);
    }

    #[test]
    fn test_readable_matrices_return_zero_while_geometry_busy() {
        let mut e = Engine3d::new();
        e.stacks.position = Matrix::identity().mul_translate(5 * ONE, 0, 0);
        e.stacks.vector = Matrix::identity().mul_scale(2 * ONE, ONE, ONE);

        assert_eq!(e.read_clip_matrix_word(12), (5 * ONE) as u32);
        assert_eq!(e.read_direction_matrix_word(0), (2 * ONE) as u32);

        e.fifo.words.push_back(0);

        assert_eq!(e.gxstat_high() & (1 << 11), 1 << 11);
        assert_eq!(e.read_clip_matrix_word(12), 0);
        assert_eq!(e.read_direction_matrix_word(0), 0);
    }

    #[test]
    fn test_direction_matrix_readback_exposes_directional_3x3_only() {
        let mut e = Engine3d::new();
        e.stacks.vector = Matrix::identity()
            .mul_scale(2 * ONE, 3 * ONE, 4 * ONE)
            .mul_translate(7 * ONE, 8 * ONE, 9 * ONE);

        assert_eq!(e.read_direction_matrix_word(0), (2 * ONE) as u32);
        assert_eq!(e.read_direction_matrix_word(4), (3 * ONE) as u32);
        assert_eq!(e.read_direction_matrix_word(8), (4 * ONE) as u32);
        assert_eq!(e.read_direction_matrix_word(9), 0);
    }

    #[test]
    fn test_decoded_ready_fifo_ops_keep_geometry_busy() {
        let mut e = Engine3d::new();
        e.fifo.write_packed(0x1515_1515);

        let _ = e.fifo.pop_op().expect("first op");

        assert_eq!(e.fifo.len(), 3);
        assert_eq!(e.gxstat_high() & (1 << 11), 1 << 11);
    }

    #[test]
    fn test_gxstat_write_clears_stack_error_and_sets_irq_mode() {
        let mut e = Engine3d::new();
        e.stacks.overflow = true;
        e.stacks.projection_sp = 1;
        e.fifo.overflow = true;

        assert_eq!(e.gxstat_low() & (1 << 15), 1 << 15);
        e.write_gxstat((2 << 30) | (1 << 15));

        assert!(!e.stacks.overflow);
        assert_eq!(e.stacks.projection_sp, 0);
        assert!(
            e.fifo.overflow,
            "GXSTAT bit 15 clears matrix stack overflow, not FIFO overflow"
        );
        assert_eq!(e.fifo.irq_mode, 2);
        assert_eq!(e.gxstat_low() & (1 << 15), 0);
    }

    #[test]
    fn test_fifo_overflow_does_not_set_gxstat_matrix_stack_error() {
        let mut e = Engine3d::new();
        e.fifo.overflow = true;

        assert_eq!(e.gxstat_low() & (1 << 15), 0);
        assert_eq!(e.gxstat() & (1 << 15), 0);
    }

    #[test]
    fn test_overflow_when_polygon_buffer_full() {
        let mut e = Engine3d::new();
        // Pre-fill the geometry buffer to the limit with dummy polygons.
        for _ in 0..POLYGON_BUF_LIMIT {
            e.geometry_polygons.push(super::ScreenPolygon {
                vertices: vec![],
                attr: 0,
                tex_image_param: 0,
                palette_base: 0,
                front_area_negative: true,
            });
        }
        // Now try to push one more through the pipeline.
        e.dispatch(GxOp {
            cmd: GxCmd::BeginVtxs as u8,
            params: vec![0],
        });
        for _ in 0..3 {
            e.dispatch(GxOp {
                cmd: GxCmd::Vtx16 as u8,
                params: vec![0, (ONE / 2) as u32 & 0xFFFF],
            });
        }
        assert!(
            e.fifo.overflow,
            "should flag overflow when geom buffer full"
        );
    }

    #[test]
    fn test_overflow_when_vertex_buffer_full() {
        let mut e = Engine3d::new();
        for _ in 0..(VERTEX_BUF_LIMIT / 3) {
            e.geometry_polygons.push(super::ScreenPolygon {
                vertices: vec![
                    super::super::viewport::ScreenVertex {
                        screen_x: 0,
                        screen_y: 0,
                        depth_z: 0,
                        w: ONE,
                        color: 0,
                        tex: [0, 0],
                    },
                    super::super::viewport::ScreenVertex {
                        screen_x: 1,
                        screen_y: 0,
                        depth_z: 0,
                        w: ONE,
                        color: 0,
                        tex: [0, 0],
                    },
                    super::super::viewport::ScreenVertex {
                        screen_x: 0,
                        screen_y: 1,
                        depth_z: 0,
                        w: ONE,
                        color: 0,
                        tex: [0, 0],
                    },
                ],
                attr: (0x1F << 16) | (1 << 6) | (1 << 7),
                tex_image_param: 0,
                palette_base: 0,
                front_area_negative: true,
            });
        }
        assert_eq!(e.geometry_vertex_count(), VERTEX_BUF_LIMIT);

        e.dispatch(GxOp {
            cmd: GxCmd::BeginVtxs as u8,
            params: vec![0],
        });
        e.dispatch(GxOp {
            cmd: GxCmd::Vtx16 as u8,
            params: vec![0, (ONE / 2) as u32 & 0xFFFF],
        });
        e.dispatch(GxOp {
            cmd: GxCmd::Vtx16 as u8,
            params: vec![ONE as u32, (ONE / 2) as u32 & 0xFFFF],
        });
        e.dispatch(GxOp {
            cmd: GxCmd::Vtx16 as u8,
            params: vec![(ONE as u32) << 16, (ONE / 2) as u32 & 0xFFFF],
        });

        assert!(
            e.fifo.overflow,
            "should flag overflow when vertex RAM is full"
        );
        assert_eq!(e.geometry_vertex_count(), VERTEX_BUF_LIMIT);
    }
}
