//! The front end's host-tested model: the library's record of a machine, the
//! new-machine assistant's offers and each machine's settings, how the host's
//! keys reach a machine's, how it is paced, what is kept of its media, and
//! how its picture is shown.

/// The picture a window opens to, in logical pixels: 4:3, as a television
/// showed it. The status strip is beneath it.
pub const OPEN_PICTURE: (u32, u32) = (768, 576);
/// The least picture a window may be dragged to.
pub const LEAST_PICTURE: (u32, u32) = (320, 240);

pub mod assistant;
pub mod events;
pub mod folder;
pub mod keys;
pub mod library;
pub mod media;
pub mod menu;
pub mod pacing;
pub mod picture;
pub mod settings;
