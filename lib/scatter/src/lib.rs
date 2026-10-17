//! Keyed scatter: one place offered to each cell of a lattice fixed to the
//! land, thinned so that no two kept crowd each other, the higher ranked
//! standing.
//!
//! The rule runs in rounds. In each, an offer no still-undecided outranking
//! offer crowds is kept, and every undecided offer a kept one crowds yields;
//! whatever is still undecided after the last round yields too. One round is
//! Matérn's second process, where an offer yields even to an outranking one
//! that yields in turn; enough rounds keep exactly what thinning the offers
//! one by one from the highest ranked keeps. Over `rounds` rounds a cell's
//! fate hangs on the offers within `2 × rounds − 1` reaches of it alone, and
//! each offer is a pure function of its own cell, so a region thinned in
//! pieces keeps exactly what it keeps thinned whole, and two pieces agree
//! about their seam without either thinning the other.
//!
//! [`Thinning`] keeps or drops the cells of a region given as its rows,
//! reading every offer once, a bounded batch of work at a time shared across
//! a runner in pieces, and keeps the same places however the batches fall and
//! whichever runner shares them.

#![no_std]
#![forbid(unsafe_code)]
// Bit for bit on every target: no float max or min, which may pick either zero.
#![deny(clippy::disallowed_methods)]

extern crate alloc;

mod thin;

pub use thin::{Thinning, FARTHEST, MOST_ROUNDS};

/// A cell of a lattice: its column and row.
pub type Cell = (i64, i64);

/// What the cells of a lattice offer, how their offers rank, and how close two
/// may stand.
pub trait Offers: Sync {
    /// One cell's offer.
    type Offer: Clone + Send + Sync;
    /// What offers are ranked by. Of two ranked alike, the one in the later
    /// row, or in the same row the later column, outranks.
    type Rank: Ord + Copy + Send + Sync;
    /// What bounds how far offers can crowd others: one for each offer,
    /// joined over a strip of a row, so a search passes over the strips too
    /// far for any of theirs to crowd its own.
    type Bound: Copy + Send + Sync;

    /// The most cells apart, on either axis, two offers can lie with one
    /// crowding the other.
    fn reach(&self) -> u32;

    /// What `cell` offers, if anything.
    fn offer(&self, cell: Cell) -> Option<Self::Offer>;

    /// How `offer` ranks: of two that crowd each other, the higher stands.
    fn rank(&self, offer: &Self::Offer) -> Self::Rank;

    /// Whether `other` stands within `offer`'s room.
    fn crowds(&self, offer: &Self::Offer, other: &Self::Offer) -> bool;

    /// `offer`'s bound.
    fn bound(&self, offer: &Self::Offer) -> Self::Bound;

    /// The bound of `one` and `other` together.
    fn join(&self, one: Self::Bound, other: Self::Bound) -> Self::Bound;

    /// Whether `offer` and an offer `bound` holds, `cells` apart on the axis
    /// they lie further apart along, may crowd one another.
    fn within(&self, offer: &Self::Offer, bound: Self::Bound, cells: u32) -> bool;
}

/// A row of cells a thinning keeps or drops: the row, and its columns from the
/// first to past the last.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Span {
    /// The row.
    pub row: i64,
    /// Its columns, from the first to past the last.
    pub columns: (i64, i64),
}

/// Why a thinning could not go on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// The heap refused room for a row of offers or what a batch keeps.
    OutOfMemory,
    /// Its rows were not given in increasing order, or a row with the reach
    /// about it runs past the lattice.
    Shape,
}
