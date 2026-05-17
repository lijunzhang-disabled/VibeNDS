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
pub mod viewport;
pub mod engine;
pub mod raster;

pub use matrix::{Matrix, ONE, fmul};
pub use stacks::{MatrixStacks, MtxMode};
pub use command::GxCmd;
pub use fifo::{GxFifo, GxOp, FIFO_CAPACITY, FIFO_HALF};
pub use vertex::{Polygon, PrimitiveType, Vertex, VertexState};
pub use clip::clip_polygon;
pub use lighting::{Light, LightingState, compute_vertex_color};
pub use viewport::{ScreenPolygon, ScreenVertex, Viewport};
pub use engine::{Engine3d, POLYGON_BUF_LIMIT, VERTEX_BUF_LIMIT};
pub use raster::{Rasterizer, FB_HEIGHT, FB_PIXELS, FB_WIDTH};
