//! The software device's shader interpreter: an admitted module's entry
//! point lowered into a program, and the tile of lanes that runs it.

pub(crate) mod exec;
pub(crate) mod ir;
pub(crate) mod lower;
pub(crate) mod ops;

#[cfg(test)]
mod tests;
