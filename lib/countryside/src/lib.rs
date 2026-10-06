//! The countryside's layout, renderer-neutral: the holdings a land is
//! farmed in, the farmsteads and villages on them, the ways between them
//! routed over the ground, the fields the land between ways and water is cut
//! into, each field's boundary, gates and use, and the plots a farmstead's
//! yard and a village's streets are laid out in.
//!
//! - [`holding`] — the holdings: cells of a jittered triangular lattice whose
//!   neighbours share every edge to the bit.
//! - [`site`] — villages spaced by a priority rule, and on every holding a
//!   village does not gather in, its own farmstead on its best ground.
//! - [`network`] and [`route`] — the ways: a relative-neighbourhood graph over
//!   the settlements and gateways, ranked highway, road, lane, track and
//!   path, each routed over the ground at its rank's grade.
//! - [`field`] — each holding's land between its ways, water and plots, cut
//!   again and again into fields sized by their ground.
//! - [`boundary`] — what bounds each field: hedge, wall, fence or ditch by
//!   its ground, gated where a way meets it.
//! - [`usage`] — what each field is used for, and the one kind of bale a cut
//!   field is baled in.
//! - [`farm`] and [`village`] — a farmstead's yard and buildings, a village's
//!   plots along its streets with their gardens and fences.
//! - [`layout`] — all of it over a region, laid a bounded unit at a time.
//!
//! Every draw is keyed by what it is for and where ([`Key`]), never drawn in
//! sequence, and every feature is worked out from what lies about it alone:
//! a region laid out in pieces matches one laid out whole, and neighbouring
//! pieces meet without a seam. Arithmetic is `f64` under `lib/util::mathf`,
//! and every order breaks its ties on identity, so a layout comes out the
//! same on every Tier-1 target and however many cores share it. Which kinds
//! of settlement a world has, and what a building is, stay its consumer's:
//! this crate holds the geometry and the algorithms.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod boundary;
pub mod farm;
pub mod field;
pub mod ground;
pub mod holding;
mod key;
pub mod layout;
pub mod network;
pub mod plane;
pub mod route;
pub mod site;
#[cfg(test)]
mod testing;
pub mod usage;
pub mod village;

pub use ground::{Ground, Lie};
pub use key::Key;
pub use plane::{Point, Rect};

/// Why a layout could not be laid out.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// The heap refused room for it.
    OutOfMemory,
    /// What it was asked to lay out cannot be laid out: a region too large,
    /// a lattice too coarse.
    Shape,
    /// No way can be routed between two places the ground parts.
    Unreachable,
}
