//! The atomics the run queue and the scheduler's per-CPU counters use: loom's
//! under `--cfg loom`, so a model drives every interleaving of those it
//! reaches, today the run queue's alone. `lib/sync` keeps its own shim private
//! to that crate, hence this one.

// Re-exports under `cfg(loom)`.
#[cfg(loom)]
pub(crate) use loom::sync::atomic::{fence, AtomicI64, AtomicPtr, AtomicU64, Ordering};

// Re-exports under normal (non-`loom`) builds.
#[cfg(not(loom))]
pub(crate) use core::sync::atomic::{fence, AtomicI64, AtomicPtr, AtomicU64, Ordering};
