//! The board's chips, each a register-level model with no knowledge of the
//! board around it: the board wires their pins together and supplies the
//! time.

pub mod dma;
pub mod keyboard;
pub mod pcb186;
pub mod pic;
pub mod pit;
pub mod ppi;
