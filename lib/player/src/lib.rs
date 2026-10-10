//! TAIRiX playback engine: a programme of sound files, decoded in the parser
//! sandbox and written gapless into one `audio-v1` stream.
//!
//! The engine of both players. `play` drives it from a command line and a
//! terminal; `music.app` from a window. Neither decodes a byte of a file
//! itself, and neither keeps playback on the loop that owes its user a frame:
//! the engine is a state machine its host feeds what it is woken by.
//!
//! | Module | What it is |
//! |---|---|
//! | [`programme`] | What plays and in what order, named by entries that survive edits. |
//! | [`engine`] | The engine, with the files, the stream and the decoder's worker as seams. |
//! | [`loudness`] | A track's gain from its own tags, held below clipping. |
//! | [`span`] | A span of time, the frames it holds, and its clock. |
//! | [`describe`](mod@describe) | How a stream reads to people. |
//! | `rt` | The live seams and a host's wait-set members (feature `rt`). |
//!
//! `no_std` with `alloc`, no `unsafe`, and no I/O outside `rt`. The page is
//! `docs/src/lib/player.md`.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod describe;
pub mod engine;
pub mod loudness;
pub mod programme;
#[cfg(feature = "rt")]
pub mod rt;
pub mod span;

pub use describe::describe;
pub use engine::{
    Control, Engine, Failure, FileRefusal, Files, Note, Outcome, Settings, Skip, Speaker, Status,
    Transport,
};
pub use programme::{Advance, EntryId, Extent, List, Passes, Programme};
pub use span::Span;
