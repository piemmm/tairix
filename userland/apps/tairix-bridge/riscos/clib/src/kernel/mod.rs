//! The library kernel, chunk 1 (`plans/RISCOS-EMULATOR.md` §10): its 48
//! entries in the PRM's order, and the library's own routines a client's
//! handlers, trap procedures and signal handlers return to.

pub(crate) mod arith;
pub(crate) mod environment;
pub(crate) mod exit;
pub(crate) mod frame;
pub(crate) mod handlers;
pub(crate) mod heap;
pub(crate) mod init;
pub(crate) mod language;
pub(crate) mod misc;
pub(crate) mod os;
pub(crate) mod stack;
pub(crate) mod statics;
pub(crate) mod system;
pub(crate) mod trap;
pub(crate) mod unwind;

use tairix_riscos_kernel::module::Code;
use tairix_riscos_kernel::os::{Fail, Os};

use crate::module::{error, raise, SHARED_C_LIBRARY};
use statics::Statics;

/// The library's own routines, after the chunks' entries in its image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Routine {
    /// Where a trap procedure returns, and an escape's or event's.
    TrapReturned,
    EventReturned,
    /// Where an initialisation procedure returns.
    InitReturned,
    /// Where the run procedure returns.
    RunReturned,
    /// Where a finalisation procedure returns.
    FinaliseReturned,
    /// The exception handlers' targets, in their trampolines' order.
    Exception(u32),
    /// The error, callback, escape, event, exit and upcall handlers.
    Error,
    CallBack,
    Escape,
    Event,
    Exit,
    UpCall,
    /// Where a frame that overflowed its stack chunk returns.
    StackReturned,
    /// The freer a stack chunk claimed from the heap remembers.
    DefaultFree,
    /// A sub-program's exit, error and upcall handlers.
    SystemExit,
    SystemError,
    SystemUpCall,
    /// Where a signal handler returns: one `raise` entered, `abort`'s, a
    /// trap's, and an escape's.
    RaiseReturned,
    AbortReturned,
    TrapSignalReturned,
    EventSignalReturned,
    /// The C library's heap, as the allocator and freer stack chunks are
    /// claimed through.
    HeapAlloc,
    HeapFree,
    /// Where a C program's `main` returns, to `exit`.
    MainReturned,
}

/// The kernel chunk's entries, in the PRM's order.
pub(crate) const ENTRIES: [Code; 48] = [
    init::init,
    exit::exit,
    exit::setreturncode,
    exit::exittraphandler,
    unwind::unwind,
    unwind::procname,
    misc::language,
    os::command_string,
    os::hostos,
    os::swi,
    os::osbyte,
    os::osrdch,
    os::oswrch,
    os::osbget,
    os::osbput,
    os::osgbpb,
    os::osword,
    os::osfind,
    os::osfile,
    os::osargs,
    os::oscli,
    os::last_oserror,
    system::system,
    os::getenv,
    os::setenv,
    heap::register_allocs,
    heap::alloc,
    stack::split_0frame,
    stack::split_frame,
    stack::copyargs,
    stack::copy0args,
    arith::udiv,
    arith::urem,
    arith::udiv10,
    arith::sdiv,
    arith::srem,
    arith::sdiv10,
    os::fpavailable,
    init::moduleinit,
    misc::irqs_on,
    misc::irqs_off,
    misc::irqs_disabled,
    init::entermodule,
    os::escape_seen,
    misc::current_stack_chunk,
    os::swi_c,
    heap::register_slotextend,
    exit::raise_error,
];

/// Each [`Routine`]'s code, in [`own_index`]'s order.
pub(crate) const OWN: [Code; 27] = [
    trap::trap_returned,
    trap::event_returned,
    init::init_returned,
    exit::run_returned,
    exit::final_returned,
    handlers::undefined,
    handlers::prefetch_abort,
    handlers::data_abort,
    handlers::address_exception,
    handlers::error,
    handlers::callback,
    handlers::escape,
    handlers::event,
    handlers::exit,
    handlers::up_call,
    stack::stack_returned,
    stack::default_free,
    system::system_exit,
    system::system_error,
    system::system_up_call,
    crate::c::signal::raise_returned,
    crate::c::signal::abort_returned,
    crate::c::signal::trap_signal_returned,
    crate::c::signal::event_signal_returned,
    crate::c::alloc::malloc,
    crate::c::alloc::free,
    crate::c::init::exit,
];

/// The routines' place after the chunks' entries.
const fn own_index(routine: Routine) -> usize {
    match routine {
        Routine::TrapReturned => 0,
        Routine::EventReturned => 1,
        Routine::InitReturned => 2,
        Routine::RunReturned => 3,
        Routine::FinaliseReturned => 4,
        Routine::Exception(n) => 5 + (n as usize % 4),
        Routine::Error => 9,
        Routine::CallBack => 10,
        Routine::Escape => 11,
        Routine::Event => 12,
        Routine::Exit => 13,
        Routine::UpCall => 14,
        Routine::StackReturned => 15,
        Routine::DefaultFree => 16,
        Routine::SystemExit => 17,
        Routine::SystemError => 18,
        Routine::SystemUpCall => 19,
        Routine::RaiseReturned => 20,
        Routine::AbortReturned => 21,
        Routine::TrapSignalReturned => 22,
        Routine::EventSignalReturned => 23,
        Routine::HeapAlloc => 24,
        Routine::HeapFree => 25,
        Routine::MainReturned => 26,
    }
}

/// Where the library's statics template lies: the module's guest code.
pub(crate) fn template(os: &mut Os<'_>) -> Result<u32, Fail> {
    os.guest_code_of(&SHARED_C_LIBRARY)
        .ok_or_else(|| raise(os, error::NO_MEMORY, &[]))
}

/// What the library's routines share.
pub(crate) mod context {
    use super::{own_index, template, Fail, Os, Routine, Statics};
    use crate::chunk::{c_routine, OWN_FIRST};
    use crate::module::{error, raise, SHARED_C_LIBRARY};

    /// The kernel statics of the client whose stack limit is `sl`.
    pub(crate) fn statics_at(os: &mut Os<'_>, sl: u32) -> Result<Statics, Fail> {
        let template = template(os)?;
        Statics::of(os, sl, template)
    }

    /// The address of the library's routine `routine`.
    pub(crate) fn routine(os: &mut Os<'_>, routine: Routine) -> Result<u32, Fail> {
        os.routine_of(&SHARED_C_LIBRARY, OWN_FIRST + own_index(routine))
            .ok_or_else(|| raise(os, error::NO_MEMORY, &[]))
    }

    /// The address of the C library's entry `n`.
    pub(crate) fn c_entry(os: &mut Os<'_>, n: usize) -> Result<u32, Fail> {
        os.routine_of(&SHARED_C_LIBRARY, c_routine(n))
            .ok_or_else(|| raise(os, error::NO_MEMORY, &[]))
    }
}
