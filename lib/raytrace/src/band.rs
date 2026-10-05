//! A grid's rows, or a volume's layers, filled a band at a time across a
//! runner.

use tairix_parallel::JobRunner;

/// About how many vertices of a grid one core fills in a unit of work: well
/// under a millisecond on a desktop core.
const UNIT_VERTICES: usize = 8192;

/// Rows of a grid `side` vertices a side one core fills in a unit of work.
pub(crate) fn unit_rows(side: usize) -> usize {
    (UNIT_VERTICES / side.max(1)).max(1)
}

/// `values` cut into `per`-long bands numbered from `first`; a `per` of `0`
/// reads as `1`.
fn numbered<T>(
    values: &mut [T],
    (first, per): (usize, usize),
) -> impl Iterator<Item = (usize, &mut [T])> {
    (first..).zip(values.chunks_mut(per.max(1)))
}

/// Visit each `per`-long band of `values` with its number, counted from
/// `first`, spread across `runner`.
pub(crate) fn for_each<T: Send>(
    runner: &dyn JobRunner,
    values: &mut [T],
    bands: (usize, usize),
    visit: &(dyn Fn(usize, &mut [T]) + Sync),
) {
    tairix_parallel::for_each_drawn(runner, numbered(values, bands), &|(number, band)| {
        visit(number, band);
    });
}

/// Visit each band of `values` as [`for_each`] does, and join what each
/// answers onto `start` in the order of the bands, whichever core visited
/// which.
pub(crate) fn fold<T: Send, R: Copy + Send>(
    runner: &dyn JobRunner,
    values: &mut [T],
    bands: (usize, usize),
    start: R,
    visit: &(dyn Fn(usize, &mut [T]) -> R + Sync),
    join: &(dyn Fn(R, R) -> R + Sync),
) -> R {
    tairix_parallel::fold_drawn(
        runner,
        numbered(values, bands),
        start,
        &|(number, band)| visit(number, band),
        join,
    )
}

#[cfg(test)]
#[path = "band_tests.rs"]
mod tests;
