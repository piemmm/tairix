//! The picture tube: what makes a picture read as a cathode-ray tube's.
//!
//! A [`Tube`] holds a strength for each effect — scan lines, the phosphor's
//! afterglow, the faceplate's glow, an analogue noise floor and a travelling
//! wobble — and resolves them into the ordered [`Pass`] list a frame goes
//! through. A [`Pipeline`] draws those passes over a picture, keeping their
//! stages between frames and redrawing only what changed, so a picture whose
//! effects are off costs nothing and an unchanging one costs nothing more.
//! The moving passes run on a [`Phase`], advanced on a one-shot deadline only
//! while something moves ([`frame_due`]).
//!
//! A machine's monitor in a window is a [`screen::Picture`]: its frame, the
//! largest 4:3 picture the window holds, enlarged sharp or smooth, redrawn
//! only where the frame changed, with the tube over it.
//!
//! The terminal draws its text through it and the emulators their machines'
//! monitors, so the tubes look alike and are written once.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

// The unit tests refuse the heap at chosen requests.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

pub mod pipeline;
pub mod screen;
pub mod tube;

pub use pipeline::{Frame, Pipeline};
pub use tube::{
    frame_due, frame_interval_ns, scaled, Pass, Phase, Tube, FASTEST_MHZ, FULL, MAX_PASSES,
};
