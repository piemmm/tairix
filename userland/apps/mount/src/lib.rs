//! TAIRiX `mount` — list mounted filesystems and attach new ones (Stage 6 `userland/apps/`).
//!
//! With no operands `mount` lists the current mount table; with a
//! `SOURCE TARGET` pair it attaches a filesystem. The two halves use
//! different paths, both kernel-plumbing-free behind injected seams:
//!
//! * **Listing** is a *read* of live system state, so — like `ps` — it goes
//!   through the System Information API: TAIRiX has no
//!   `/proc` and no mount-table file. `mount` issues the typed, versioned
//!   `sysinfo-v1` `MOUNT_LIST` and `VIEW_LIST` queries served by
//!   `/System/Services/sysinfod.app/Run` and renders one
//!   `source on target type fstype (options)` line per view binding, a
//!   carved-out subtree read as `source[/subtree]`. The queries are ungated;
//!   the shared paging, join, and rendering live in `lib/procinfo` so no
//!   client duplicates them.
//! * **Attaching** is privileged — it needs `CAP_FS_MOUNT` — and the **kernel** makes that decision, not
//!   this tool. `mount` parses and presents a request to the injected
//!   [`Mounter`] seam; an unauthorised or invalid attempt is refused there
//!   and surfaced as [`MountError::Mount`].
//!
//! # What this crate is
//!
//! A command-line parser and presenter, not a policy point. The operations
//! that touch the outside world are the injected seams:
//!
//! * [`Transport`](tairix_procinfo::Transport) / [`Output`](tairix_procinfo::Output)
//!   — issue the list queries and write each rendered line (shared with the
//!   other `sysinfo` clients).
//! * [`Mounter`] — perform the privileged attach.
//!
//! No `mount` bundle ships and nothing implements [`Mounter`] outside the
//! tests, which wire in-memory fixtures (`plans/OPEN-DEFECTS.md`).
//!
//! # Fail closed
//!
//! An unknown option, a missing option value, or a number of operands other
//! than zero or two is a [`MountError::Usage`]; an unknown `-o`/`-t` value is
//! a [`MountError::BadOption`]. A listing transport failure surfaces the
//! underlying [`Errno`](tairix_abi::Errno) as [`MountError::Service`], a
//! refused or failed attach as [`MountError::Mount`], and a terminal write
//! failure as [`MountError::Output`]. There is no panic.
//!
//! # Module map
//!
//! * [`error`] — [`MountError`], the outcomes of [`run`].
//! * [`command`] — the [`Command`]/[`MountRequest`] shapes and the [`parse`]r.
//! * [`io`] — the [`Mounter`] seam and the [`MountSpec`] it carries.
//! * [`client`] — the [`run`] entry point.
//!
//! # Layering & safety
//!
//! `no_std` (with `alloc`); the only dependencies are the
//! audited `lib/abi` and `lib/procinfo` crates, so this userland tool never
//! links a kernel or driver crate. No `unsafe`, and no
//! `unwrap`/`expect`/`panic!` in production paths.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

pub mod client;
pub mod command;
pub mod error;
pub mod io;

pub use client::{run, USAGE};
pub use command::{parse, Command, MountRequest};
pub use error::MountError;
pub use io::{MountSpec, Mounter};
