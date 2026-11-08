//! The atomics a shared block's holders are counted in: loom's under
//! `--cfg loom`, so the model drives every interleaving of them. `lib/sync`
//! keeps its own shim private to that crate, hence this one.

#[cfg(loom)]
pub(crate) use loom::sync::atomic::{fence, AtomicUsize, Ordering};

#[cfg(not(loom))]
pub(crate) use core::sync::atomic::{fence, AtomicUsize, Ordering};
