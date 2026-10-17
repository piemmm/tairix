//! How many of a grid's rows one core fills in a unit of work.

/// About how many vertices of a grid one core fills in a unit of work: well
/// under a millisecond on a desktop core.
const UNIT_VERTICES: usize = 8192;

/// Rows of a grid `side` vertices a side one core fills in a unit of work.
pub(crate) fn unit_rows(side: usize) -> usize {
    (UNIT_VERTICES / side.max(1)).max(1)
}
