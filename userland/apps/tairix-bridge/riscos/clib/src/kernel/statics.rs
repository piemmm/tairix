//! The library kernel's statics in a client's memory: where they are, found
//! through the stack chunk's words below SL as RISC OS 5.30's are, and the
//! place of each thing the kernel keeps there.
//!
//! What a client can see is where RISC OS 5.30 has it — the exception
//! handlers' trampolines, the callback registers, the error buffer
//! `_kernel_last_oserror` answers with — and the rest lies in what those
//! leave free.

use tairix_riscos_kernel::os::{Fail, Os};

use super::environment::{self, EXCEPTION_HANDLERS};
use super::handlers::{CALLBACK_WORDS, EVENT_WORDS};
use super::init::TRAMPOLINE;
use super::os::ERROR_BLOCK;
use super::{system, trap};
use crate::stack_chunk;

/// The kernel statics' size.
pub(crate) const SIZE: u32 = 0x31C;

/// Where each thing lies in the kernel statics.
pub(crate) mod at {
    /// The image base and the language descriptions `_kernel_init` was given.
    pub(crate) const IMAGE_BASE: u32 = 0x000;
    pub(crate) const LANGUAGES: u32 = 0x004;
    pub(crate) const LANGUAGES_END: u32 = 0x008;
    /// The undefined instruction, prefetch abort, data abort and address
    /// exception handlers: a trampoline each, four words apiece.
    pub(crate) const TRAMPOLINES: u32 = 0x00C;
    /// A trap's search for its handler: the words `trap::state` names.
    pub(crate) const TRAP_SEARCH: u32 = 0x04C;
    /// The heap's next free byte, which stack chunks the library claims
    /// itself and `_kernel_alloc` take from.
    pub(crate) const HEAP: u32 = 0x074;
    /// The callback handler's register buffer.
    pub(crate) const CALLBACK_REGISTERS: u32 = 0x078;
    /// R0 to R9 as the last event came, which the languages' fast event
    /// procedures are given.
    pub(crate) const EVENT_REGISTERS: u32 = 0x0BC;
    /// What the next callback dispatches — an escape, an event — and the
    /// event's code.
    pub(crate) const PENDING: u32 = 0x0E4;
    pub(crate) const PENDING_EVENT: u32 = 0x0E8;
    /// An escape's or event's search for its procedure, as a trap's.
    pub(crate) const EVENT_SEARCH: u32 = 0x0EC;
    /// The error handler's buffer: the PC, then the error block, which
    /// `_kernel_last_oserror` answers.
    pub(crate) const ERROR_BUFFER: u32 = 0x11C;
    pub(crate) const LAST_ERROR: u32 = 0x120;
    /// The handlers in force before `_kernel_init`, three words each.
    pub(crate) const SAVED_HANDLERS: u32 = 0x220;
    /// A sub-program `_kernel_system` runs: what the caller resumes with,
    /// and where it lies while the sub-program runs — the words
    /// `system::kept` names.
    pub(crate) const SYSTEM: u32 = 0x298;
    /// The return code `_kernel_exit` gives.
    pub(crate) const RETURN_CODE: u32 = 0x2D8;
    /// Whether an OS error has come since `_kernel_last_oserror` last
    /// answered.
    pub(crate) const ERROR_PENDING: u32 = 0x2DC;
    /// The heap's end — the workspace's, as `_kernel_init` was given it —
    /// and the root stack chunk.
    pub(crate) const HEAP_LIMIT: u32 = 0x2E0;
    pub(crate) const ROOT_CHUNK: u32 = 0x2E4;
    /// The copy of the command line.
    pub(crate) const COMMAND: u32 = 0x2E8;
    /// The allocator stack chunks are claimed through, and its freer.
    pub(crate) const ALLOC: u32 = 0x2EC;
    pub(crate) const FREE: u32 = 0x2F0;
    /// The procedure that extends the application's slot.
    pub(crate) const SLOT_EXTEND: u32 = 0x2F4;
    /// Whether the floating-point unit answered.
    pub(crate) const FP_AVAILABLE: u32 = 0x2F8;
    /// Whether an Escape has come since `_kernel_escape_seen` last answered.
    pub(crate) const ESCAPE_SEEN: u32 = 0x2FC;
    /// The base of the small stack an allocator runs on during stack
    /// extension: the root chunk's top.
    pub(crate) const EXTENSION_STACK: u32 = 0x300;
    /// Whether this is a module's library, not an application's.
    pub(crate) const MODULE: u32 = 0x304;
    /// `_kernel_init`'s walk of the initialisation procedures: the next
    /// description, and the run procedure the first to offer one gave.
    pub(crate) const INIT_NEXT: u32 = 0x308;
    pub(crate) const INIT_RUN: u32 = 0x30C;
    /// A finalisation's walk: the next description, and what follows it —
    /// how the program ends, and the error or exit code it ends with.
    pub(crate) const FINAL_NEXT: u32 = 0x310;
    pub(crate) const FINAL_THEN: u32 = 0x314;
    pub(crate) const FINAL_WITH: u32 = 0x318;
}

/// Each field's start and size, in order: none overlaps the next, and the
/// last ends within the statics, which the build checks.
const LAYOUT: [(u32, u32); 33] = [
    (at::IMAGE_BASE, 4),
    (at::LANGUAGES, 4),
    (at::LANGUAGES_END, 4),
    (at::TRAMPOLINES, TRAMPOLINE * EXCEPTION_HANDLERS),
    (at::TRAP_SEARCH, trap::state::BYTES),
    (at::HEAP, 4),
    (at::CALLBACK_REGISTERS, 4 * CALLBACK_WORDS),
    (at::EVENT_REGISTERS, 4 * EVENT_WORDS),
    (at::PENDING, 4),
    (at::PENDING_EVENT, 4),
    (at::EVENT_SEARCH, trap::state::BYTES),
    (at::ERROR_BUFFER, 4),
    (at::LAST_ERROR, ERROR_BLOCK),
    (at::SAVED_HANDLERS, environment::SAVED_BYTES),
    (at::SYSTEM, system::kept::BYTES),
    (at::RETURN_CODE, 4),
    (at::ERROR_PENDING, 4),
    (at::HEAP_LIMIT, 4),
    (at::ROOT_CHUNK, 4),
    (at::COMMAND, 4),
    (at::ALLOC, 4),
    (at::FREE, 4),
    (at::SLOT_EXTEND, 4),
    (at::FP_AVAILABLE, 4),
    (at::ESCAPE_SEEN, 4),
    (at::EXTENSION_STACK, 4),
    (at::MODULE, 4),
    (at::INIT_NEXT, 4),
    (at::INIT_RUN, 4),
    (at::FINAL_NEXT, 4),
    (at::FINAL_THEN, 4),
    (at::FINAL_WITH, 4),
    (SIZE, 0),
];

const _: () = {
    let mut n = 1;
    while n < LAYOUT.len() {
        let (start, size) = LAYOUT[n - 1];
        assert!(
            start + size <= LAYOUT[n].0,
            "the kernel statics' fields overlap"
        );
        n += 1;
    }
};

/// A client's kernel statics, found from the SL it calls with.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Statics(u32);

impl Statics {
    /// The statics of the client whose stack limit is `sl`, relocated from
    /// the library's `template`.
    pub(crate) fn of(os: &mut Os<'_>, sl: u32, template: u32) -> Result<Self, Fail> {
        let chunk = stack_chunk::of_limit(sl);
        let offset = os
            .as_caller()
            .read_word(chunk.wrapping_add(stack_chunk::word::LIBRARY_OFFSET))?;
        Ok(Self(template.wrapping_add(offset)))
    }

    /// The statics at `base`, as a handler is given them.
    pub(crate) const fn of_base(base: u32) -> Self {
        Self(base)
    }

    /// Where they start.
    pub(crate) const fn base(self) -> u32 {
        self.0
    }

    /// The address of the field `at` lies at.
    pub(crate) const fn field(self, at: u32) -> u32 {
        self.0.wrapping_add(at)
    }

    pub(crate) fn read(self, os: &mut Os<'_>, at: u32) -> Result<u32, Fail> {
        os.as_caller().read_word(self.field(at))
    }

    pub(crate) fn write(self, os: &mut Os<'_>, at: u32, value: u32) -> Result<(), Fail> {
        os.as_caller().write_word(self.field(at), value)
    }
}
