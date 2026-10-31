//! TAIRiX `mount` — list the mounted filesystems, and mount a disk image.
//!
//! With no operands `mount` lists the mount table through the System
//! Information API's ungated `MOUNT_LIST` and `VIEW_LIST` queries, one
//! `source on target type fstype (options)` line per view binding. With an
//! `IMAGE` operand it mounts the volume that disk-image file holds,
//! privately, for the user who runs it — at `/Storage/NAME`, and at a
//! `TARGET` directory too when one is given (`plans/DISK-IMAGES.md`). The
//! kernel recognises the image, decides whether it may be mounted, and
//! audits the decision; nothing here grants anything.
//!
//! The operations that touch the outside world are injected seams, so every
//! parsing, rendering and routing decision is tested without a kernel:
//!
//! * [`Transport`](tairix_procinfo::Transport) — the list queries, shared
//!   with the other `sysinfo` clients.
//! * [`Mounter`] — the image mount.
//! * [`HelpSource`](tairix_help::HelpSource) — the bundle's own `Help/` tree,
//!   for the short help.
//!
//! A device or volume `SOURCE` is not mounted: nothing yet lets a user name
//! an unattached storage object (`plans/OPEN-DEFECTS.md` D1014).
//!
//! # Module map
//!
//! * [`command`] — [`Command`], [`MountRequest`] and the [`parse`]r.
//! * [`error`] — [`MountError`] and the exit status each failure gives.
//! * [`io`] — the [`Mounter`] seam.
//! * [`client`] — the [`run`] entry point.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

pub mod client;
pub mod command;
pub mod error;
pub mod io;

pub use client::{run, USAGE};
pub use command::{parse, Command, MountRequest};
pub use error::MountError;
pub use io::Mounter;
