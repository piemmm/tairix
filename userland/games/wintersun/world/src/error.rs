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
    /// A chunk or region off the realm's land, or so far out that its cells
    /// would not fit a cell coordinate.
    OutOfRange,
    /// A buffer handed to a realm stage does not hold one entry per sample
    /// of the realm's coarse grid.
    Mismatch,
}

impl From<tairix_terrain::TerrainError> for WorldError {
    fn from(error: tairix_terrain::TerrainError) -> Self {
        match error {
            tairix_terrain::TerrainError::OutOfMemory => Self::OutOfMemory,
            tairix_terrain::TerrainError::Shape => Self::Mismatch,
        }
    }
}

impl From<tairix_land::Error> for WorldError {
    fn from(error: tairix_land::Error) -> Self {
        match error {
            tairix_land::Error::OutOfMemory => Self::OutOfMemory,
            tairix_land::Error::Shape | tairix_land::Error::Unreachable => Self::Mismatch,
        }
    }
}

impl From<tairix_scatter::Error> for WorldError {
    fn from(error: tairix_scatter::Error) -> Self {
        match error {
            tairix_scatter::Error::OutOfMemory => Self::OutOfMemory,
            tairix_scatter::Error::Shape => Self::OutOfRange,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WorldError;
    use tairix_terrain::TerrainError;

    #[test]
    fn a_terrain_refusal_keeps_its_cause() {
        assert_eq!(
            WorldError::from(TerrainError::OutOfMemory),
            WorldError::OutOfMemory
        );
        assert_eq!(WorldError::from(TerrainError::Shape), WorldError::Mismatch);
    }

    #[test]
    fn a_scatter_refusal_keeps_its_cause() {
        assert_eq!(
            WorldError::from(tairix_scatter::Error::OutOfMemory),
            WorldError::OutOfMemory
        );
        assert_eq!(
            WorldError::from(tairix_scatter::Error::Shape),
            WorldError::OutOfRange
        );
    }
}
