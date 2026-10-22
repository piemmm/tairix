//! elsh (Element Shell) — the default TAIRiX command interpreter, a POSIX-ish
//! shell (Stage 6, `PLAN.md`).
//!
//! `tairix-elsh` reads a line of text and runs it: it lexes the line with
//! full quoting and escaping, parses pipelines and `;`/`&&`/`||`/`&`
//! connectors, expands braces, `$`-variables and filename patterns, runs a
//! small set of builtins in-process, and launches everything else through an
//! injected process host with job control over background and stopped jobs.
//!
//! # What this crate is
//!
//! A **pure interpreter**. It decides *what* to run and *with what arguments*,
//! but it never itself touches the kernel or a terminal. What reaches the
//! outside world — launching/waiting/signalling a job, writing output, and
//! reading the directories a filename pattern names — goes through the
//! injected [`ProcessHost`], [`Console`] and [`GlobFs`] seams.
//! On a running kernel they are syscall-backed; in tests they are in-memory
//! fixtures. This mirrors `init`'s `Spawner`/`Reaper` design and keeps every
//! parsing, expansion, and control-flow decision exhaustively testable
//! without a kernel.
//!
//! # Pipeline
//!
//! 1. [`lexer::tokenize`] — text to a quoting-aware [`lexer::Token`] stream.
//! 2. [`parser::parse`] — tokens to a [`parser::CommandList`] tree. A
//!    here-document (`<<`, `<<-`) parses *pending*: its body is collected from
//!    the following input lines ([`parser::CommandList::feed_here_doc_line`],
//!    bounded and fail-closed) before the list runs.
//! 3. Expansion, in bash's order: brace expansion, `$`-expansion
//!    ([`env::Environment::expand_word`]), then filename generation through
//!    the shared `lib/glob` matcher, an unquoted word left empty dropped.
//! 4. [`Shell::run_line`] — run each pipeline, honouring connectors and the
//!    background flag, dispatching builtins ([`builtin`]) or launching through
//!    the [`ProcessHost`], and tracking jobs in the [`job::JobTable`].
//!    ([`Shell::parse_line`] and [`Shell::run_list`] are the two halves the
//!    REPL drives separately to collect here-document bodies in between.)
//!
//! # Deliberate simplifications
//!
//! * Expansion does not field-split, as zsh does not, and a variable's value
//!   is never read as a filename pattern — zsh's rule rather than bash's.
//! * The supported parameter expansions are `$NAME`, `${NAME}`, and `$?`.
//!
//! # Layering & safety
//!
//! `no_std` (with `alloc`); the dependencies are audited `lib/*` crates —
//! among them `tairix-abi` (for [`Errno`](tairix_abi::Errno) on the host
//! seams), `tairix-glob` and `tairix-path` (the one pattern matcher and path
//! grammar), `tairix-resref` (the one resource-reference spelling parser),
//! and `tairix-vt` (the shared read line discipline the REPL's line reader
//! runs) — so a userland program never links a kernel or driver crate.
//! No `unsafe`, and no `unwrap`/`expect`/`panic!` in production paths.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

mod brace;
pub mod builtin;
pub mod complete;
mod editor;
mod elevate;
pub mod env;
pub mod error;
mod expand;
pub mod host;
pub mod invocation;
pub mod job;
pub mod lexer;
mod options;
pub mod parser;
mod pathexp;
pub mod repl;
pub mod shell;
mod ulimit;
pub mod wireplan;

#[cfg(test)]
pub(crate) mod test_support;

pub use complete::{complete, Candidate, Completion, DirEntryInfo, DirLister, ResourceLister};
pub use env::Environment;
pub use error::ParseError;
pub use host::{
    Console, Denied, Elevator, LaunchError, LaunchSpec, LimitStore, ProcessHost, RedirAction,
    RedirTarget, ResolvedCommand, ResolvedRedirection,
};
pub use invocation::{parse as parse_invocation, Invocation, UsageError, USAGE};
pub use job::{ExitStatus, Job, JobId, JobState, JobTable, Pid, Signal, WaitOutcome};
pub use pathexp::GlobFs;
pub use repl::{run as run_repl, ReplInput};
pub use shell::Shell;
pub use wireplan::{
    lower as lower_wire_plan, MemberPlan, OpenId, PlannedOpen, PlannedWire, PumpTask, WirePlan,
};
