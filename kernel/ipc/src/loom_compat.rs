//! The atomics the port's state machine uses: loom's under `--cfg loom`, so
//! the models drive every interleaving of them. `lib/sync` keeps its own shim
//! private to that crate, hence this one.

#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[cfg(not(loom))]
pub(crate) use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
