//! Strictly justified shared utilities.
//!
//! Every item is used by at least two independent callers outside this crate,
//! and `PLAN.md`'s `lib/util` table names them: an item moves here when its
//! second caller appears, never ahead of demand. Members hold text and buffers
//! on the heap, so a crate that must link no allocator takes nothing from here.
//!
//! * [`argv`] — a word argument (`--total=auto`) resolved to its one choice
//!   or refused with the valid ones listed.
//! * [`banner`] — the `==> name <==` banner before each of several sources.
//! * [`cfloat`] — C-locale `printf(3)` floating-point rendering.
//! * [`cnum`] — C-locale `strtod(3)` scanning: decimal and hexadecimal floats,
//!   `inf`/`nan`, and longest-prefix `endptr` semantics.
//! * [`conf`] — the `#`-comment line grammar every line-oriented configuration
//!   store shares, and the shape of the values a key accepts.
//! * [`count`] — GNU's count grammar for `-c`/`-n` values, multiplier suffixes
//!   included.
//! * [`defer`] — handing slow work off an interactive loop: `JobDesk`,
//!   latest-wins with at most one job in flight, and `JobQueue`, every request
//!   answered in turn.
//! * [`fallible`] — reserving a data-sized buffer before filling it, so
//!   exhaustion is a refusal rather than an allocation abort.
//! * [`fmt`] — numeric formatters that allocate nothing, for structured-log
//!   fields.
//! * [`getopt`] — a command line read as GNU's `getopt_long` reads it, from a
//!   table of the tool's options, and refused in its words.
//! * [`hex`] — two hex digits a byte, for digests, keys and seeds.
//! * [`hexdump`] — the canonical offset/hex/ASCII dump row.
//! * [`join`] — one bounded line from a list: whole items, then how many were
//!   left out.
//! * [`lanes`] — finding a byte eight lanes of a word at a time.
//! * [`mathf`] — bounded, total `f64` maths for `no_std` geometry, the same
//!   bits on every target.
//! * [`mode`] — a file mode as `chmod`, `mkdir -m` and `umask` spell it, the
//!   bits it gives a node, and the modes a new file and directory start from.
//! * [`output`] — the byte stream a command engine writes through, and the run
//!   that reports each failure it carries past.
//! * [`quote`] — GNU coreutils' quoting styles, and the three ways a command
//!   names text from elsewhere: inside a sentence, leading a `name: reason`
//!   line, and on its standard output.
//! * [`report`] — a file's structure as a person reads it, bounded against the
//!   untrusted bytes it is built from.
//! * [`retry`] — a bounded doubling schedule for waiting on something with no
//!   readiness event, and a capped doubling delay between restarts.
//! * [`seconds`] — the decimal-seconds operand a network tool's timing option
//!   takes, as nanoseconds.
//! * [`secret`] — erasing secret bytes so the optimiser cannot delete the
//!   erasure.
//! * [`size`] — GNU coreutils' block-size grammar and size renderings, and the
//!   desktop's prose rendering of the same quantity.
//! * [`space`] — three-dimensional vectors, orthonormal frames and rigid
//!   poses.
//! * [`tailwindow`] — bounded rolling "keep the last N bytes/lines" windows.
//! * [`utf8`] — which bytes begin a UTF-8 sequence, a name as it is shown,
//!   and the letters and order a reader looks names up by.

#![no_std]
// `mathf` takes the correctly rounded square root and rounding `core` exposes
// only behind this gate until they are stabilised as inherent `f64` methods.
#![feature(core_float_math)]
#![forbid(unsafe_op_in_unsafe_fn)]
// Bit for bit on every target: no float max or min, which may pick either zero.
#![deny(clippy::disallowed_methods)]
#![deny(missing_docs)]

extern crate alloc;

pub mod argv;
pub mod banner;
pub mod cfloat;
pub mod cnum;
pub mod conf;
pub mod count;
pub mod defer;
pub mod fallible;
pub mod fmt;
pub mod getopt;
pub mod hex;
pub mod hexdump;
pub mod join;
pub mod lanes;
pub mod mathf;
pub mod mode;
pub mod output;
pub mod quote;
pub mod report;
pub mod retry;
pub mod seconds;
pub mod secret;
pub mod size;
pub mod space;
pub mod tailwindow;
pub mod utf8;
