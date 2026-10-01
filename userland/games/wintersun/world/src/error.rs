//! What generation can refuse, and why.

/// A refusal from the generator.
///
/// Generation is pure arithmetic over an **already validated** parameter
/// document — [`RealmParams`](crate::params::RealmParams) has one
/// constructor and it checks every field — so a bad document is refused
/// before anything here runs. What remains is an exhausted machine, or a
/// caller handing a stage what it cannot use, and each is a typed refusal,
/// never a panic.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WorldError {
    /// The realm field or a chunk did not fit in memory.
    OutOfMemory,
    /// A chunk window is not strictly sorted by coordinate, so it could
    /// not be searched.
    UnsortedWindow,
    /// A chunk coordinate so far out that its cells would not fit a cell
    /// coordinate.
    OutOfRange,
    /// A buffer handed to a realm stage does not hold one entry per sample
    /// of the realm's coarse grid.
    Mismatch,
}
