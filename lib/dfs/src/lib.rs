//! The Acorn DFS on-disc format, as DFS 1.20 writes it: the one definition
//! the volume probe, the archive driver serving a DFS disc image and the Open
//! Micro emulator read and write discs through.
//!
//! - [`catalogue`] — the two sectors opening each side, and their entries.
//! - [`name`] — the bytes a name DFS makes may hold.
//! - [`image`] — where a side's bytes lie in an SSD or DSD image.
//!
//! Everything is computation over bytes the caller supplies, total over any
//! input: a catalogue that breaks the format is refused, never trusted.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod catalogue;
pub mod image;
pub mod name;
