//! The pinned shader set: the desktop's passes as GPU programs (`plans/GPU.md`
//! GD6, `plans/SHADER.md` SH3).
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
//! [`frost::blur`], [`frost::spread`] and [`frost::mix`], and the
//! compositor's [`compose::over`], [`compose::copy`], [`compose::fill`] and
//! [`compose::encode`].
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

#[cfg(test)]
extern crate alloc;

pub mod compose;
pub mod frost;
mod kit;

pub use kit::{groups, GROUP};

use tairix_spirv::{Admitted, BuildError};

/// One module of the set: the stem of its file under `spv/`, the source that
/// builds it, and the module as committed.
#[derive(Copy, Clone)]
pub struct Pinned {
    /// Its file's stem.
    pub name: &'static str,
    /// The source that builds it.
    pub build: fn() -> Result<Admitted, BuildError>,
    /// The module as committed.
    pub spirv: &'static [u8],
}

/// Every module of the set.
pub const SET: [Pinned; 8] = [
    Pinned {
        name: "frost_reduce",
        build: frost::reduce::build,
        spirv: frost::reduce::SPIRV,
    },
    Pinned {
        name: "frost_blur",
        build: frost::blur::build,
        spirv: frost::blur::SPIRV,
    },
    Pinned {
        name: "frost_spread",
        build: frost::spread::build,
        spirv: frost::spread::SPIRV,
    },
    Pinned {
        name: "frost_mix",
        build: frost::mix::build,
        spirv: frost::mix::SPIRV,
    },
    Pinned {
        name: "over",
        build: compose::over::build,
        spirv: compose::over::SPIRV,
    },
    Pinned {
        name: "copy",
        build: compose::copy::build,
        spirv: compose::copy::SPIRV,
    },
    Pinned {
        name: "fill",
        build: compose::fill::build,
        spirv: compose::fill::SPIRV,
    },
    Pinned {
        name: "encode",
        build: compose::encode::build,
        spirv: compose::encode::SPIRV,
    },
];

#[cfg(test)]
mod tests;
