//! The device-neutral GPU seam: the memory and submission model every backend
//! implements, and the software device that implements it on every target
//! (`plans/GPU.md`).
//!
//! # The model
//!
//! * A [`Context`] is the unit of isolation. Its allocations, buffers,
//!   timelines and queues are its own, and a handle of another context or
//!   device is refused wherever it is named ([`Error::Foreign`]): sharing
//!   memory is an explicit [`Export`] another context imports.
//! * Memory is typed. A [`DeviceInfo`] enumerates its heaps and the memory
//!   types over them; an [`Allocation`] is made in one type and charged to its
//!   context's limit and its heap's size, refused past either, and then
//!   refused if the memory cannot be had.
//! * A [`Buffer`] is a span of an allocation made for stated uses, and is used
//!   for nothing else.
//! * Work is recorded into a [`CommandBuffer`] and handed to a [`Queue`] in a
//!   [`Submission`], which is checked whole before anything is queued: every
//!   buffer it names is its context's and live, every range is within what it
//!   names, every use is one the buffer was made for, every signal advances
//!   its timeline. A refused submission changes nothing, and nothing is
//!   clamped into range.
//! * A queue runs its work in the order submitted. A [`Timeline`] orders work
//!   across queues and with the CPU: a value is reached only once every signal
//!   up to it has landed. A [`Fence`] is one submission's completion.
//! * Every submission carries a deadline. One that runs past it loses its
//!   context ([`Loss::Overran`]); every other context's work goes on. A lost
//!   context refuses all work and only releases what it holds.
//!
//! # The software device
//!
//! [`Device::software`] runs every queue's work on the CPU, on the thread
//! whose act made it ready — the submission, the signal, or the completion
//! before it. It is the reference the hardware backends are held to, and the
//! device a program has when the machine offers no other.
//!
//! # Threads
//!
//! Every handle may be used from any thread. A wait parks on the timeline it
//! waits for and is woken by what advances it; nothing spins.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod command;
mod device;
mod error;
mod info;
mod memory;
mod model;
mod software;

pub use command::CopyRegion;
pub use device::{
    Allocation, AllocationDesc, Buffer, BufferDesc, CommandBuffer, Context, ContextDesc, Device,
    Export, Fence, Queue, Submission, Timeline,
};
pub use error::{Error, Loss, SubmitError};
pub use info::{
    BufferUsage, DeviceInfo, Limits, MemoryHeap, MemoryProperties, MemoryType, QueueCapabilities,
    QueueFamily, MAX_MEMORY_HEAPS, MAX_MEMORY_TYPES, MAX_QUEUES_PER_FAMILY, MAX_QUEUE_FAMILIES,
};

/// The most command buffers one submission carries.
pub const MAX_SUBMISSION_BUFFERS: usize = 64;

/// The most commands one submission carries, across its command buffers.
pub const MAX_SUBMISSION_COMMANDS: usize = 65_536;

/// The most timeline values one submission waits for, and the most it sets.
pub const MAX_SUBMISSION_POINTS: usize = 64;

/// The most regions one copy moves.
pub const MAX_COPY_REGIONS: usize = 1024;

/// The most bytes one update writes.
pub const MAX_UPDATE_BYTES: usize = 65_536;

#[cfg(test)]
#[path = "device_tests.rs"]
mod device_tests;
