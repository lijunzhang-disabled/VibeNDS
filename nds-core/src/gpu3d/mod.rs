//! 3D graphics engine.
//!
//! Phase 6 = geometry pipeline (this module). Phase 7 = rasterizer.
//!
//! Build order in this module follows the natural data flow:
//!   matrix → matrix stacks → GX command set → GXFIFO →
//!   vertex pipeline → lighting → clipping → viewport.
//!
//! Background: `docs/concepts/3d-graphics-basics.md`.

pub mod matrix;
pub mod stacks;
pub mod command;
pub mod fifo;
pub mod vertex;
pub mod clip;
pub mod lighting;

pub use matrix::{Matrix, ONE, fmul};
pub use stacks::{MatrixStacks, MtxMode};
pub use command::GxCmd;
pub use fifo::{GxFifo, GxOp, FIFO_CAPACITY, FIFO_HALF};
pub use vertex::{Polygon, PrimitiveType, Vertex, VertexState};
pub use clip::clip_polygon;
pub use lighting::{Light, LightingState, compute_vertex_color};
