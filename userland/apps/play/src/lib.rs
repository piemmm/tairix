//! TAIRiX `play` — the command-line sound player (`plans/SOUND.md` SND10).
//!
//! Playback is `tairix-player`'s engine: each file decoded in the parser
//! sandbox, never in this process, and written into one `audio-v1` stream
//! while the files' shape holds, so a list plays gapless. Playback is not in
//! the interface loop: the full-screen interface is a view of a playback that
//! would happen without it, which is why `play` keeps playing when it is sent
//! to the background and draws itself again when it is brought back.
//!
//! # Module map
//!
//! * [`command`] — the command line and what it asks for.
//! * [`report`] — the `stdinfo` records and the lines on standard error.
//! * [`view`] — the full-screen interface, drawn from the engine's status.
//!
//! `no_std` with `alloc`, no `unsafe`, and no I/O: the `Run` binary supplies
//! every seam.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod command;
pub mod report;
pub mod view;
