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
use super::stacks::{MatrixStacks, MtxMode};
use super::vertex::{
    decode_vtx10, decode_vtx16, decode_vtx_diff, decode_vtx_pair,
    PrimitiveType, Vertex, VertexState, VtxAxisPair,
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
    /// consumed by the Phase 7 rasterizer.
    pub raster_polygons: Vec<ScreenPolygon>,

    /// Set whenever `SWAP_BUFFERS` is queued; consumed and cleared at the
    /// next frame boundary (VBlank-end) by the top-level scheduler.
    pub swap_pending: bool,
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
        }
    }

    /// Drain every command currently `ready` in the FIFO. Called by the
    /// bus dispatcher after each write that might have completed a command.
    pub fn drain_fifo(&mut self) {
        while let Some(op) = self.fifo.pop_op() {
            self.dispatch(op);
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
                let cur = *self.stacks.current();
                self.stacks.load(cur.mul_scale(sx, sy, sz));
            }
            GxCmd::MtxTrans => {
                let tx = params.first().copied().unwrap_or(0) as i32;
                let ty = params.get(1).copied().unwrap_or(0) as i32;
                let tz = params.get(2).copied().unwrap_or(0) as i32;
                let cur = *self.stacks.current();
                self.stacks.load(cur.mul_translate(tx, ty, tz));
            }

            // ─── Vertex attributes ────────────────────────────────
            GxCmd::Color => self.vertex.set_color(p0),
            GxCmd::Normal => self.handle_normal(p0),
            GxCmd::TexCoord => self.vertex.set_texcoord(p0),
            GxCmd::Vtx16 => {
                let p = [params.first().copied().unwrap_or(0), params.get(1).copied().unwrap_or(0)];
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
            GxCmd::DifAmb => self.lighting.set_dif_amb(p0),
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
            GxCmd::SwapBuffers => self.swap_pending = true,
            GxCmd::Viewport => self.viewport = Viewport::from_param(p0),

            // ─── Test commands — stubbed ──────────────────────────
            GxCmd::BoxTest | GxCmd::PosTest | GxCmd::VecTest => {
                // These do hardware tests (box-in-frustum etc.) and return
                // results via GXSTAT. Wire in Phase 9 if we hit a game
                // that depends on them.
            }
        }

        // After every command, drain any newly-completed polygons through
        // clipping + viewport into the geometry buffer.
        self.flush_polygons();
    }

    fn handle_normal(&mut self, param: u32) {
        // NORMAL: 10-bit signed (x, y, z) in the low 30 bits. If the
        // material's "set color from diffuse" bit is set, the diffuse
        // lighting term replaces the vertex color before the next VTX_*.
        let sign_ext = |b: u32| -> i32 { (((b & 0x3FF) << 22) as i32) >> 22 };
        let nx = sign_ext(param) << 3;       // 1.0.9 -> 1.0.12
        let ny = sign_ext(param >> 10) << 3;
        let nz = sign_ext(param >> 20) << 3;

        // Compute lit color now and set as the vertex color.
        let attr = self.vertex.polygon_attr;
        let light_enable = (attr & 0xF) as u8;
        let vec_mat = self.stacks.vector;
        let color = compute_vertex_color(&self.lighting, [nx, ny, nz], &vec_mat, light_enable);
        self.vertex.current_color = color;
    }

    fn submit_vertex(&mut self, pos: [i32; 3]) {
        self.vertex.submit_vertex(pos, &self.stacks);
    }

    /// Move any newly-assembled polygons from `vertex.polygon_buffer`
    /// through clipping + viewport into `geometry_polygons`. Respects the
    /// per-frame caps.
    fn flush_polygons(&mut self) {
        while let Some(poly) = self.vertex.polygon_buffer.pop() {
            if self.geometry_polygons.len() >= POLYGON_BUF_LIMIT {
                self.fifo.overflow = true;
                continue;
            }
            let clipped: Vec<Vertex> = match clip_polygon(&poly.vertices) {
                Some(v) => v,
                None => continue, // fully outside; discard
            };
            // Build a clipped Polygon, then viewport-transform.
            let clipped_poly = super::vertex::Polygon {
                vertices: clipped,
                attr: poly.attr,
                tex_image_param: poly.tex_image_param,
                palette_base: poly.palette_base,
            };
            let screen = transform_polygon(&clipped_poly, self.viewport);
            self.geometry_polygons.push(screen);
        }
    }

    /// Frame-boundary swap. Called by the top-level scheduler at VBlank end
    /// when `swap_pending` is true. Moves geometry buffer to raster buffer.
    pub fn swap_buffers(&mut self) {
        if !self.swap_pending { return; }
        self.raster_polygons = std::mem::take(&mut self.geometry_polygons);
        self.swap_pending = false;
    }
}

impl Default for Engine3d {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::matrix::ONE;

    #[test]
    fn test_full_pipeline_triangle_to_screen() {
        let mut e = Engine3d::new();

        // BEGIN_VTXS triangles
        e.dispatch(GxOp { cmd: GxCmd::BeginVtxs as u8, params: vec![0] });
        // Three vertices forming a triangle in the canonical view volume.
        // VTX_16 packed format: y << 16 | x, _ << 16 | z.
        // Vertex 1: (0, 0, 0.5)
        e.dispatch(GxOp { cmd: GxCmd::Vtx16 as u8, params: vec![
            0, // y=0, x=0
            (ONE / 2) as u32 & 0xFFFF, // z=0.5
        ]});
        e.dispatch(GxOp { cmd: GxCmd::Vtx16 as u8, params: vec![
            (((ONE / 4) as u32) & 0xFFFF), // y=0, x=0.25
            (ONE / 2) as u32 & 0xFFFF,
        ]});
        e.dispatch(GxOp { cmd: GxCmd::Vtx16 as u8, params: vec![
            ((ONE / 4) as u32 & 0xFFFF) << 16, // y=0.25, x=0
            (ONE / 2) as u32 & 0xFFFF,
        ]});

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
        // Run a triangle through (same shape as previous test).
        e.dispatch(GxOp { cmd: GxCmd::BeginVtxs as u8, params: vec![0] });
        for _ in 0..3 {
            e.dispatch(GxOp { cmd: GxCmd::Vtx16 as u8, params: vec![0, (ONE / 2) as u32 & 0xFFFF] });
        }
        assert_eq!(e.geometry_polygons.len(), 1);
        assert!(e.raster_polygons.is_empty());

        // Queue swap, then trigger.
        e.dispatch(GxOp { cmd: GxCmd::SwapBuffers as u8, params: vec![0] });
        assert!(e.swap_pending);
        e.swap_buffers();
        assert!(!e.swap_pending);
        assert_eq!(e.raster_polygons.len(), 1);
        assert!(e.geometry_polygons.is_empty());
    }

    #[test]
    fn test_matrix_command_dispatches_to_stacks() {
        let mut e = Engine3d::new();
        e.dispatch(GxOp { cmd: GxCmd::MtxMode as u8, params: vec![1] });
        assert!(matches!(e.stacks.mode, MtxMode::Position));
    }

    #[test]
    fn test_viewport_command_updates_viewport() {
        let mut e = Engine3d::new();
        let param = (180u32 << 24) | (100u32 << 16) | (20u32 << 8) | 10u32;
        e.dispatch(GxOp { cmd: GxCmd::Viewport as u8, params: vec![param] });
        assert_eq!(e.viewport.x1, 10);
        assert_eq!(e.viewport.x2, 100);
    }

    #[test]
    fn test_overflow_when_polygon_buffer_full() {
        let mut e = Engine3d::new();
        // Pre-fill the geometry buffer to the limit with dummy polygons.
        for _ in 0..POLYGON_BUF_LIMIT {
            e.geometry_polygons.push(super::ScreenPolygon {
                vertices: vec![],
                attr: 0, tex_image_param: 0, palette_base: 0,
            });
        }
        // Now try to push one more through the pipeline.
        e.dispatch(GxOp { cmd: GxCmd::BeginVtxs as u8, params: vec![0] });
        for _ in 0..3 {
            e.dispatch(GxOp { cmd: GxCmd::Vtx16 as u8, params: vec![0, (ONE / 2) as u32 & 0xFFFF] });
        }
        assert!(e.fifo.overflow, "should flag overflow when geom buffer full");
    }
}
