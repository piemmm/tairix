//! The `music.app` engine: the playlist, the transport the window shows, the
//! one layout every painter and hit-test reads, and the requests a paint never
//! makes itself.
//!
//! Host-tested and free of both windows and I/O. Playback is not here: it is
//! `tairix-player`'s engine, on a thread of its own in `Run`, told what the
//! listener asked through [`Command`]s and read back through the status it
//! publishes. Nothing here waits — a track's tags, its album art and the list
//! of output devices are *requested* ([`Request`]) and *collected* later, so
//! a paint draws only what has already arrived.
//!
//! | Module | What it is |
//! |---|---|
//! | [`playlist`] | The arrangement, the play order and what follows what. |
//! | [`layout`] | The window's geometry. |
//! | [`view`] | The player's state: input in, state and damage out. |
//! | [`paint`] | The renderers, which read state alone. |

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod layout;
pub mod paint;
pub mod playlist;
pub mod view;

pub use layout::Layout;
pub use playlist::{Edit, Playlist, Repeat};
pub use view::{Command, Outcome, Player, Request};

/// The window extent the player asks the desktop for, in logical pixels:
/// room for the album art beside three lines of what is playing, the
/// transport under them, and a dozen rows of the playlist.
pub const WIN_WIDTH: u32 = 720;
/// The window height the player asks for, in logical pixels.
pub const WIN_HEIGHT: u32 = 520;
