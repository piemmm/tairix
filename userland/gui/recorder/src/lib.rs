//! TAIRiX screen recorder, `recorder.app`: the component the desktop session
//! starts to record the screen, and that nothing else can reach
//! (`plans/CAPTURE.md`).
//!
//! The session starts it over two pipes only the session holds, delegates it
//! an empty file of the session's own making, and grants it two slots of the
//! session's own memory, which resolve for the recorder's process alone and
//! which it maps only as grants of the session its one operand names. The
//! session composes each frame into a free slot and names it.
//! Every frame is coded as VP8, the sound the session asked for as Opus, and
//! both are written as one `WebM` file through positional writes, the seek
//! index into space reserved at its start.
//!
//! # Threads
//!
//! * **The service thread** ([`Recorder`]) parks in one wait-set on the
//!   session's pipes, the sound streams' mailboxes and the encoding thread's
//!   wake. It bounds every rectangle a frame names to its slot before the
//!   frame is handed on, reads the capture rings as they fill, and mixes and
//!   codes the sound, a packet coding in a fraction of the 20 ms it spans. It
//!   never writes the file.
//! * **The encoding thread** ([`carry_out`]) writes the file's header with its
//!   first job, so a recording that never begins leaves the file empty, then
//!   codes each frame as VP8 and writes it, writes the sound's packets, and
//!   finishes the file. A slot is
//!   released once its frame is coded, so a picture that takes seconds to code
//!   holds up the pictures after it and never the sound: the packets wait in a
//!   batch on the service thread, up to [`sound::BATCH_PACKETS`] of them.
//!
//! # Memory
//!
//! A recording commits before `Ready` what it needs at once: the encoder's
//! planes, the room for a coded frame, the sound's queues, its mixer and its
//! batches, its slots being the session's memory. A recording whose total
//! passes what the process may map is refused `Unstarted(OutOfMemory)` before
//! any of it is made, and every allocation after is fallible, so a machine
//! short of memory drops a frame rather than aborting a recording.
//!
//! The core is host-tested over fakes of the pipes, the slots, the sound, the
//! clock and the file; `src/run.rs` binds it to the runtime.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod encode;
pub mod recorder;
pub mod sound;
#[cfg(test)]
mod testing;

pub use encode::{
    carry_out, Answer, Closed, Coded, Encoding, Failure, Halt, Job, Output, SlotMemory,
};
pub use recorder::{client_of, Desk, Exit, Host, Recorder, Said, EXIT_ENDED, EXIT_FINISHED};
pub use sound::{News, PacketBatch, Sound, SoundSource};

use tairix_log::EventId;

/// Stable event id: a recording could not begin, and why.
pub const RECORDER_UNSTARTED: EventId = EventId(36_000);

/// Stable event id: the recorder refused what the session sent.
pub const RECORDER_REFUSED: EventId = EventId(36_001);

/// Stable event id: a recording ended other than as the session asked.
pub const RECORDER_ENDED: EventId = EventId(36_002);

/// Stable event id: a frame was dropped for want of memory, and the next coded
/// whole.
pub const FRAME_DROPPED: EventId = EventId(36_003);

/// Stable event id: some or all of a recording's sound was lost.
pub const SOUND_LOST: EventId = EventId(36_004);

/// Stable event id: no encoding thread could be had, so its work runs on the
/// service thread.
pub const NO_ENCODING_THREAD: EventId = EventId(36_005);

/// Stable event id: the kernel granted the encoding pool fewer threads than
/// the machine has cores, so frames are coded on fewer.
pub const FEW_ENCODING_CORES: EventId = EventId(36_006);
