//! Bus structures for the ARM9 and ARM7 sides + the shared-state struct
//! both buses peek into.

pub mod arm7;
pub mod arm9;
pub mod dldi;
pub mod io_arm7;
pub mod io_arm9;
pub mod math;
pub mod shared;

pub use arm7::{Arm7Memory, Bus7};
pub use arm9::{Arm9Memory, Bus9};
pub use shared::SharedState;
