//! TAIRiX `traceroute` — print the route echo probes take to a network host,
//! in the familiar Linux `traceroute` shape (`plans/NETMAP.md` NM-S3, a
//! `plans/APPS.md` command app).
//!
//! The probes are ICMP/`ICMPv6` echo requests whose hop limit climbs one hop
//! at a time; each router whose hop limit runs out reports back, and the trace
//! ends where the destination answers or a router says the path goes no
//! further. The hop-by-hop method itself is `lib/net`'s shared `trace` engine,
//! which the network map drives too; this crate parses the command line, runs
//! the engine against an injected [`TraceIo`], and prints each hop once its
//! probes have settled.
//!
//! * [`command`] — the option grammar and its parser.
//! * [`net`] — the [`TraceIo`] seam and the [`Answer`] it reports.
//! * [`client`] — the [`run`] entry point and the hop printer.
//! * [`error`] — [`TraceError`], what ends a run early.
//!
//! The `Run` binary (`src/run.rs`) implements [`TraceIo`] over the shared
//! resolver and the `tairix_rt` echo socket. No `unsafe`, no `unwrap`, no
//! `panic!` in a production path.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod client;
pub mod command;
pub mod error;
pub mod net;

pub use client::{run, USAGE};
pub use command::{parse, Command, Config, ParseError};
pub use error::TraceError;
pub use net::{Answer, TraceIo};
