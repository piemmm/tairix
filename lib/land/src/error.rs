//! Why a land could not be built.

use tairix_terrain::TerrainError;

/// Why a land, or a piece of one, could not be built.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// The heap refused room for a stage's working grids or what it made.
    OutOfMemory,
    /// A plan, a piece or the grid a piece is refined from does not fit:
    /// a level with no ratio, a piece off the land, or a parent that does not
    /// hold everything the piece reads.
    Shape,
    /// The countryside could route no way between two places the ground
    /// parts.
    Unreachable,
}

impl From<TerrainError> for Error {
    fn from(error: TerrainError) -> Self {
        match error {
            TerrainError::OutOfMemory => Self::OutOfMemory,
            TerrainError::Shape => Self::Shape,
        }
    }
}

impl From<tairix_countryside::Error> for Error {
    fn from(error: tairix_countryside::Error) -> Self {
        match error {
            tairix_countryside::Error::OutOfMemory => Self::OutOfMemory,
            tairix_countryside::Error::Shape => Self::Shape,
            tairix_countryside::Error::Unreachable => Self::Unreachable,
        }
    }
}
