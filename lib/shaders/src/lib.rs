//! The pinned shader set: the desktop's passes as GPU programs (`plans/GPU.md`
//! GD6 and GP4, `plans/SHADER.md` SH3).
//!
//! Each module is written in Rust through `lib/spirv`'s builder, which hands
//! it back only once the validator admits it, and committed under `spv/` as
//! the SPIR-V the desktop loads. `cargo xtask shaders` builds every module
//! from its source and fails closed where a committed one has drifted;
//! `--write` commits what the sources build. The desktop therefore carries no
//! runtime compiler, and a change to a pass is a change to its committed
//! bytes, visible in review.
//!
//! The passes are `lib/raster`'s own arithmetic, held to it bit for bit by
//! this crate's conformance cases: the frost's [`frost::reduce`],
//! [`frost::blur`], [`frost::spread`] and [`frost::mix`], the compositor's
//! [`compose::over`], [`compose::copy`], [`compose::fill`],
//! [`compose::encode`] and [`compose::resample`], its chrome,
//! [`chrome::body`] and [`chrome::shadow`], and the furniture painter's
//! [`paint::plate`], [`paint::wash`], [`paint::ring`], [`paint::coverage`],
//! [`paint::mask`] and [`paint::picture`]. Every one computes in 32-bit
//! integers, within the bounds its description is refused past
//! ([`Refused`]).
//!
//! # What a pass takes
//!
//! Every pass is a compute entry point named `main` over workgroups
//! [`GROUP`] invocations square, one a pixel or cell, dispatched as
//! [`groups`] says. Its storage buffers are words — a pixel is one, red in its
//! low byte, as `tairix_raster::Pixel` lies in memory — at set 0 and the
//! bindings its module names; its push constants are its `Push`'s words.
//! Nothing outside the extent its `Push` names is read or written.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod chrome;
pub mod compose;
pub mod frost;
pub mod indirect;
mod kit;
pub mod paint;
mod samples;

pub use kit::{groups, Refused, GROUP, MAX_PASS_COORDINATE, MAX_PASS_RADIUS};

use alloc::vec::Vec;

use tairix_spirv::{Admitted, BuildError};

/// One module of the set: the stem of its file under `spv/`, the source that
/// builds it, the module as committed, and a known input to run it on.
#[derive(Copy, Clone)]
pub struct Pinned {
    /// Its file's stem.
    pub name: &'static str,
    /// The source that builds it.
    pub build: fn() -> Result<Admitted, BuildError>,
    /// The module as committed.
    pub spirv: &'static [u8],
    /// The pass run on a known input, which a device and the software device
    /// must leave alike to the bit.
    pub sample: fn() -> Result<Sample, Refused>,
}

/// A pass run on a known input: what each binding holds before it runs, the
/// binding numbered by its place, its push constants, and the workgroups it
/// dispatches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sample {
    /// Each binding's words.
    pub buffers: Vec<Vec<u32>>,
    /// Its push constants' words.
    pub push: Vec<u32>,
    /// Its workgroups along each axis.
    pub groups: [u32; 3],
}

/// Every module of the set.
pub const SET: [Pinned; 19] = [
    Pinned {
        name: "frost_reduce",
        build: frost::reduce::build,
        spirv: frost::reduce::SPIRV,
        sample: samples::frost_reduce,
    },
    Pinned {
        name: "frost_blur",
        build: frost::blur::build,
        spirv: frost::blur::SPIRV,
        sample: samples::frost_blur,
    },
    Pinned {
        name: "frost_spread",
        build: frost::spread::build,
        spirv: frost::spread::SPIRV,
        sample: samples::frost_spread,
    },
    Pinned {
        name: "frost_mix",
        build: frost::mix::build,
        spirv: frost::mix::SPIRV,
        sample: samples::frost_mix,
    },
    Pinned {
        name: "over",
        build: compose::over::build,
        spirv: compose::over::SPIRV,
        sample: samples::over,
    },
    Pinned {
        name: "copy",
        build: compose::copy::build,
        spirv: compose::copy::SPIRV,
        sample: samples::copy,
    },
    Pinned {
        name: "fill",
        build: compose::fill::build,
        spirv: compose::fill::SPIRV,
        sample: samples::fill,
    },
    Pinned {
        name: "encode",
        build: compose::encode::build,
        spirv: compose::encode::SPIRV,
        sample: samples::encode,
    },
    Pinned {
        name: "resample",
        build: compose::resample::build,
        spirv: compose::resample::SPIRV,
        sample: samples::resample,
    },
    Pinned {
        name: "body",
        build: chrome::body::build,
        spirv: chrome::body::SPIRV,
        sample: samples::body,
    },
    Pinned {
        name: "shadow",
        build: chrome::shadow::build,
        spirv: chrome::shadow::SPIRV,
        sample: samples::shadow,
    },
    Pinned {
        name: "plate",
        build: paint::plate::build,
        spirv: paint::plate::SPIRV,
        sample: samples::plate,
    },
    Pinned {
        name: "wash",
        build: paint::wash::build,
        spirv: paint::wash::SPIRV,
        sample: samples::wash,
    },
    Pinned {
        name: "ring",
        build: paint::ring::build,
        spirv: paint::ring::SPIRV,
        sample: samples::ring,
    },
    Pinned {
        name: "coverage",
        build: paint::coverage::build,
        spirv: paint::coverage::SPIRV,
        sample: samples::coverage,
    },
    Pinned {
        name: "mask",
        build: paint::mask::build,
        spirv: paint::mask::SPIRV,
        sample: samples::mask,
    },
    Pinned {
        name: "picture",
        build: paint::picture::build,
        spirv: paint::picture::SPIRV,
        sample: samples::picture,
    },
    Pinned {
        name: "indirect_counts",
        build: indirect::counts::build,
        spirv: indirect::counts::SPIRV,
        sample: samples::indirect_counts,
    },
    Pinned {
        name: "indirect_records",
        build: indirect::records::build,
        spirv: indirect::records::SPIRV,
        sample: samples::indirect_records,
    },
];

#[cfg(test)]
mod tests;
