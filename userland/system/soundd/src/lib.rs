//! The cue authority (`plans/SOUND.md` §Default desktop sounds): the one
//! machine service that makes the desktop's sounds.
//!
//! A program asks for a sound by its event, over `cue-v1`; this service
//! decides from the caller's kernel-attested identity whether it may, resolves
//! the event to a theme's asset or to silence, decodes the asset in the parser
//! sandbox, and plays it as a notification through the audio service. It is a
//! player, not part of the mixer: untrusted bytes never decode in the process
//! that holds every stream, and its file reads stay off the mixer's real-time
//! path.
//!
//! Pure over five seams — the store it reads, the streams it plays into, the
//! decode worker's launcher, the cache's pressure gauge, and the audit sink —
//! so the whole authority is host-tested with no machine.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod bounds;
pub mod decode;
pub mod events;
pub mod pcm;
pub mod service;
pub mod speaker;
pub mod store;

#[cfg(test)]
mod testkit;

pub use decode::Decoder;
pub use pcm::{cue_cache, CueCache};
pub use service::{CueAuthority, Tally};
pub use speaker::Speakers;
pub use store::{FileIdentity, SoundStore};
