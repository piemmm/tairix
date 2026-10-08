//! TAIRiX kernel IRQ table and per-handle wait queue.
//!
//! The kernel side of the `CAP_IRQ_BIND`-gated `irq_bind` / `irq_wait`
//! syscalls. The contract — the wake-up ordering invariant and the
//! failure-mode table — is
//! [`docs/src/security/irq.md`](../../../docs/src/security/irq.md).
//!
//! * **Mask-before-wake.** [`IrqTable::fire`] masks the line at the
//!   controller before it advances the fire count a waiter is woken by.
//! * **Shared lines.** Several owners may bind one line — a wired PCI INTx
//!   pin. Every fire wakes each of them, and the line is re-armed only once
//!   every sharer has taken the fire and come back to wait.
//! * **Forgery defence in the table.** Every handle-keyed operation checks
//!   the handle was minted for the calling process before it acts.
//! * **Lock-free interrupt path.** Bindings sit behind one
//!   `lib/sync::RwLock`; `fire` touches only per-line atomics and the
//!   set-once [`IrqDispatchObserver`] and [`MonotonicClock`] hooks.
//! * **One wait loop.** [`block_until_ready`] is the poll-and-park loop
//!   every waiter runs, parameterised over the caller's clock and park.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

mod error;
mod table;
mod wait;

pub use error::{ActivationError, IrqError, MaskError};
pub use table::{
    BindOutcome, FireOutcome, IrqController, IrqDispatchObserver, IrqEntry, IrqTable,
    MonotonicClock, ObserverAlreadyInstalled, ReleaseOutcome, Trigger, UnsupportedController,
    WaitStep, UNSUPPORTED_CONTROLLER,
};
pub use wait::{block_until_ready, IrqWaitAbort, IrqWaiter, WaitOutcome};
