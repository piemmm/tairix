//! TAIRiX network-share service (`smbfs`) — SMB 3 shares listed, mounted and
//! served (`plans/SMB.md`).
//!
//! Every byte a server sends is a stranger's, so the service is several
//! processes and the ones that parse hold nothing:
//!
//! * The front holds the `netshare-v1` endpoint, the sockets, the served
//!   filesystems and every authority the service exercises. It parses no
//!   SMB: it carries each connection's bytes to and from that connection's
//!   worker, judges what the worker reports, and answers its callers.
//! * [`worker::Worker`] runs one connection's `tairix_smb` client as a
//!   capability-empty sandbox session: it holds the session's keys and every
//!   piece of parsed server state, and reaches nothing but its pipe to the
//!   front. A worker a server compromises can lie about its own connection
//!   and nothing else.
//!
//! The channel between them is [`wire`]: fixed-shape frames, each side's
//! reader a bounds-checked field read.

#![no_std]
// `Arc::try_new`, so a mount the heap refuses room for is refused rather than
// aborting the service.
#![feature(allocator_api)]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod events;
pub mod files;
pub mod front;
pub mod host;
pub mod judge;
pub mod link;
pub mod mounts;
pub mod nodes;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod wire;
pub mod worker;
