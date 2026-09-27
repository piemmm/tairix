//! The three callbacks a port's trap path hands a fault it cannot finish
//! itself.
//!
//! Each port decodes its own exception syndrome and decides which callback a
//! fault goes to; the callbacks, and the slots they are published through, are
//! the same on every port and so live here once:
//!
//! * a **user data fault** is offered to the [`UserFaultResolveFn`] first —
//!   the demand-paged file mapping — and resolved or charged to the task;
//! * any other **user fault** is charged to the running task through the
//!   [`UserFaultTerminateFn`], so one task's bad instruction never parks a CPU;
//! * everything left is the **kernel's own** and goes to the
//!   [`FaultHandlerFn`], or with none installed to the port's own report.
//!
//! Every slot is claimed once per boot before the fault class it serves can be
//! raised, and read on every trap without a lock. A second claim fails closed,
//! and an empty slot sends its faults down the fatal path, so a missed install
//! can only ever halt, never let a task survive a fault it should not.

use tairix_sync::FnCell;

use crate::backtrace::UserRegisterFrame;
use crate::fatal::KernelFault;

/// The handler a kernel-mode fault no port path can resolve is handed to.
///
/// It **must not return**: the port has no fix-up for the faulting
/// instruction, so resuming would re-take the exception forever. The kernel's
/// post-mortem installs one at boot entry; a QEMU vertical that observes its
/// own faults installs its own ahead of boot.
pub type FaultHandlerFn = fn(KernelFault) -> !;

/// The resolver a user-mode data fault is offered to before anything else.
///
/// `address` is the faulting address and `write` whether the access was a
/// store. A `true` return means the page is now resident and the port retries
/// the instruction; only a read is ever resolved that way, since a file
/// mapping is read-only and resolving a store would retry it forever. A fault
/// fatal to the task alone never returns: the resolver suspends the task with
/// an exit action, as a rescheduling syscall does, and the CPU goes on. A
/// `false` return means the fault could not be attributed to a running task
/// and sends the port down its fatal path.
///
/// `regs` is the faulting user register frame, or null, for the task's
/// post-mortem crash record; the callee narrows it and never dereferences
/// null. Like every trap-path callback it captures no environment.
pub type UserFaultResolveFn =
    extern "C" fn(address: u64, write: bool, regs: *const UserRegisterFrame) -> bool;

/// The terminator a user-mode exception that can be neither a syscall nor
/// resolved is charged to — an illegal instruction, an alignment fault, a wild
/// jump.
///
/// Nothing is retried: the callback records the task's crash exit and suspends
/// it with an exit action, never returning for it. A `false` return means the
/// exception could not be attributed to a running task, which sends the port
/// down its fatal path. `fault_pc` is the offending instruction and `regs` the
/// faulting user register frame, or null.
pub type UserFaultTerminateFn =
    extern "C" fn(fault_pc: u64, regs: *const UserRegisterFrame) -> bool;

/// Why a slot refused an install.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SetFaultHandlerError {
    /// The slot was already claimed; each is set once per boot.
    AlreadyInstalled,
}

static FAULT_HANDLER: FnCell<FaultHandlerFn> = FnCell::empty();
static USER_FAULT_RESOLVER: FnCell<UserFaultResolveFn> = FnCell::empty();
static USER_FAULT_TERMINATOR: FnCell<UserFaultTerminateFn> = FnCell::empty();

/// Install the kernel-mode fault handler, before any exception can be taken.
///
/// # Errors
///
/// [`SetFaultHandlerError::AlreadyInstalled`] on a second install: the handler
/// already published keeps the machine's fatal policy.
pub fn set_fault_handler(handler: FaultHandlerFn) -> Result<(), SetFaultHandlerError> {
    installed(FAULT_HANDLER.claim(handler))
}

/// The installed kernel-mode fault handler, if any.
#[must_use]
pub fn fault_handler() -> Option<FaultHandlerFn> {
    FAULT_HANDLER.load()
}

/// Install the user-fault resolver, before user space is entered.
///
/// # Errors
///
/// [`SetFaultHandlerError::AlreadyInstalled`] on a second install.
pub fn set_user_fault_resolver(resolver: UserFaultResolveFn) -> Result<(), SetFaultHandlerError> {
    installed(USER_FAULT_RESOLVER.claim(resolver))
}

/// The installed user-fault resolver, if any.
#[must_use]
pub fn user_fault_resolver() -> Option<UserFaultResolveFn> {
    USER_FAULT_RESOLVER.load()
}

/// Install the user-fault terminator, before user space is entered.
///
/// # Errors
///
/// [`SetFaultHandlerError::AlreadyInstalled`] on a second install.
pub fn set_user_fault_terminator(
    terminator: UserFaultTerminateFn,
) -> Result<(), SetFaultHandlerError> {
    installed(USER_FAULT_TERMINATOR.claim(terminator))
}

/// The installed user-fault terminator, if any.
#[must_use]
pub fn user_fault_terminator() -> Option<UserFaultTerminateFn> {
    USER_FAULT_TERMINATOR.load()
}

fn installed(claimed: bool) -> Result<(), SetFaultHandlerError> {
    claimed
        .then_some(())
        .ok_or(SetFaultHandlerError::AlreadyInstalled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_fault_handler(_fault: KernelFault) -> ! {
        unreachable!("a host test never takes a fault");
    }

    extern "C" fn host_user_fault_resolver(
        _address: u64,
        _write: bool,
        _regs: *const UserRegisterFrame,
    ) -> bool {
        false
    }

    extern "C" fn host_user_fault_terminator(
        _fault_pc: u64,
        _regs: *const UserRegisterFrame,
    ) -> bool {
        false
    }

    /// Every slot is exercised by one test, because each is process-wide and
    /// parallel tests clearing and reclaiming the same one would race. Each
    /// comparison is against the one coercion installed, since two coercions of
    /// one `fn` item need not share an address.
    #[test]
    fn each_slot_is_claimed_once_and_reads_back_what_was_installed() {
        FAULT_HANDLER.clear();
        USER_FAULT_RESOLVER.clear();
        USER_FAULT_TERMINATOR.clear();
        assert!(fault_handler().is_none());
        assert!(user_fault_resolver().is_none());
        assert!(user_fault_terminator().is_none());

        let handler: FaultHandlerFn = host_fault_handler;
        let resolver: UserFaultResolveFn = host_user_fault_resolver;
        let terminator: UserFaultTerminateFn = host_user_fault_terminator;
        assert_eq!(set_fault_handler(handler), Ok(()));
        assert_eq!(set_user_fault_resolver(resolver), Ok(()));
        assert_eq!(set_user_fault_terminator(terminator), Ok(()));

        assert_eq!(
            fault_handler().map(|f| f as *const ()),
            Some(handler as *const ())
        );
        assert_eq!(
            user_fault_resolver().map(|f| f as *const ()),
            Some(resolver as *const ())
        );
        assert_eq!(
            user_fault_terminator().map(|f| f as *const ()),
            Some(terminator as *const ())
        );

        assert_eq!(
            set_fault_handler(handler),
            Err(SetFaultHandlerError::AlreadyInstalled)
        );
        assert_eq!(
            set_user_fault_resolver(resolver),
            Err(SetFaultHandlerError::AlreadyInstalled)
        );
        assert_eq!(
            set_user_fault_terminator(terminator),
            Err(SetFaultHandlerError::AlreadyInstalled)
        );

        FAULT_HANDLER.clear();
        USER_FAULT_RESOLVER.clear();
        USER_FAULT_TERMINATOR.clear();
    }
}
