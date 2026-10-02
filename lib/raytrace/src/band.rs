//! A grid's rows, or a volume's layers, filled a band at a time across a
//! runner.

use alloc::vec::Vec;

use tairix_parallel::JobRunner;
use tairix_util::fallible;

/// Visit each `per`-long band of `values` with its number, counted from
/// `first`, spread across `runner`: on the calling thread alone when the heap
/// will not hold the list of bands, so no band is ever left unfilled.
pub(crate) fn for_each<T: Send>(
    runner: &dyn JobRunner,
    values: &mut [T],
    (first, per): (usize, usize),
    visit: &(dyn Fn(usize, &mut [T]) + Sync),
) {
    let per = per.max(1);
    let mut bands: Vec<(usize, &mut [T])> = Vec::new();
    if fallible::reserve(&mut bands, values.len().div_ceil(per)) {
        bands.extend((first..).zip(values.chunks_mut(per)));
        tairix_parallel::for_each(runner, &mut bands, &|(number, band)| visit(*number, band));
    } else {
        for (number, band) in (first..).zip(values.chunks_mut(per)) {
            visit(number, band);
        }
    }
}

/// Visit each band of `values` as [`for_each`] does, and join what each
/// answers onto `start` in the order of the bands, whichever core visited
/// which.
pub(crate) fn fold<T: Send, R: Copy + Send>(
    runner: &dyn JobRunner,
    values: &mut [T],
    (first, per): (usize, usize),
    start: R,
    visit: &(dyn Fn(usize, &mut [T]) -> R + Sync),
    join: impl Fn(R, R) -> R,
) -> R {
    let per = per.max(1);
    let mut bands: Vec<(usize, &mut [T], R)> = Vec::new();
    if !fallible::reserve(&mut bands, values.len().div_ceil(per)) {
        return (first..)
            .zip(values.chunks_mut(per))
            .fold(start, |joined, (number, band)| {
                join(joined, visit(number, band))
            });
    }
    bands.extend(
        (first..)
            .zip(values.chunks_mut(per))
            .map(|(number, band)| (number, band, start)),
    );
    tairix_parallel::for_each(runner, &mut bands, &|(number, band, answer)| {
        *answer = visit(*number, band);
    });
    bands
        .iter()
        .fold(start, |joined, &(_, _, answer)| join(joined, answer))
}

#[cfg(test)]
#[path = "band_tests.rs"]
mod tests;
