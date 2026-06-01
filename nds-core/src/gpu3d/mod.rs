//! 3D graphics engine.
//!
//! Phase 6 = geometry pipeline (this module). Phase 7 = rasterizer.
//!
//! Build order in this module follows the natural data flow:
//!   matrix → matrix stacks → GX command set → GXFIFO →
//!   vertex pipeline → lighting → clipping → viewport.
//!
//! Background: `docs/concepts/3d-graphics-basics.md`.

pub mod clip;
pub mod command;
pub mod engine;
pub mod fifo;
pub mod lighting;
pub mod matrix;
pub mod raster;
pub mod stacks;
pub mod vertex;
pub mod viewport;

pub use clip::clip_polygon;
pub use command::GxCmd;
pub use engine::{Engine3d, POLYGON_BUF_LIMIT, VERTEX_BUF_LIMIT};
pub use fifo::{GxFifo, GxOp, FIFO_CAPACITY, FIFO_HALF};
pub use lighting::{compute_vertex_color, Light, LightingState};
pub use matrix::{fmul, Matrix, ONE};
pub use raster::{Rasterizer, FB_HEIGHT, FB_PIXELS, FB_WIDTH};
pub use stacks::{MatrixStacks, MtxMode};
pub use vertex::{Polygon, PrimitiveType, Vertex, VertexState};
pub use viewport::{ScreenPolygon, ScreenVertex, Viewport};
