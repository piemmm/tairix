//! The client the C library's unit tests call its entries for: a session
//! holding the library, and a root stack chunk whose library offset puts the
//! client's statics in its application space.

extern crate std;

use std::sync::LazyLock;
use std::vec::Vec;

use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::session::{modules_with, session, Session, Setup};
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::module::{Builtin, Code};
use tairix_riscos_kernel::os::Regs;

use crate::module::SHARED_C_LIBRARY;
use crate::stack_chunk;

pub(crate) mod replay;

static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[&crate::MODULES]));

const SLOT: u32 = 1 << 20;
pub(crate) const CHUNK: u32 = APPLICATION + 0x1000;
/// Where the client's library statics lie.
pub(crate) const STATICS: u32 = APPLICATION + 0x2000;
/// Where a test's own guest code goes.
pub(crate) const CODE: u32 = APPLICATION + 0x3000;
/// Memory a test's strings and blocks may go in.
pub(crate) const FREE: u32 = APPLICATION + 0x4000;
/// The top of the stack a client's code runs on.
const STACK_TOP: u32 = APPLICATION + 0xF000;
/// Where the client's heap may lie, to the slot's end.
pub(crate) const HEAP: u32 = APPLICATION + 0x1_0000;
pub(crate) const SLOT_END: u32 = APPLICATION + SLOT;
/// The C heap's first arena's words.
pub(crate) const HEAP_ARENA_WORDS: u32 = 2048;
/// `B .`: what the client's own code is left at.
const SPIN: u32 = 0xEAFF_FFFE;
/// Where a routine run as a subroutine returns to.
pub(crate) const RETURN: u32 = FREE - 4;

pub(crate) struct Client {
    pub(crate) session: Session,
}

impl Client {
    pub(crate) fn new() -> Self {
        Self::hosted(None, &[])
    }

    /// A client whose session serves HostFS over `disc` where it is given
    /// one, and types `keys`.
    pub(crate) fn hosted(disc: Option<&Tree>, keys: &[u8]) -> Self {
        let mut session = session(&Setup {
            vdu: None,
            modules: MODULES.as_slice(),
            bytes: SLOT,
            keys,
            disc,
        });
        let mut os = session.kernel.os();
        let template = os
            .guest_code_of(&SHARED_C_LIBRARY)
            .expect("the library's guest code");
        let offset = STATICS.wrapping_sub(template);
        os.write_word(CHUNK + stack_chunk::word::MARK, stack_chunk::MARK)
            .expect("marked");
        os.write_word(CHUNK + stack_chunk::word::LIBRARY_OFFSET, offset)
            .expect("offset");
        Self { session }
    }

    /// A client whose C heap's first arena is laid at [`HEAP`], as
    /// `_clib_initialise` claims it, the kernel's heap after it to the
    /// slot's end.
    pub(crate) fn with_heap() -> Self {
        Self::new().heaped()
    }

    /// A client whose session serves HostFS over `disc` and types `keys`,
    /// with its C heap, the C locale, and its streams laid as
    /// `_clib_initialise` lays them.
    pub(crate) fn with_streams(disc: &Tree, keys: &[u8]) -> Self {
        let mut client = Self::hosted(Some(disc), keys).heaped();
        let statics = crate::kernel::statics::Statics::of_base(STATICS);
        let mut os = client.session.kernel.os();
        crate::c::locale::start_in_c(&mut os, statics).expect("the C locale");
        crate::c::stdio::stream::start(&mut os, statics).expect("the streams");
        client
    }

    /// The client with its C heap laid as [`Client::with_heap`] lays it.
    pub(crate) fn heaped(mut self) -> Self {
        let statics = crate::kernel::statics::Statics::of_base(STATICS);
        let mut os = self.session.kernel.os();
        crate::c::alloc::start(&mut os, statics, HEAP, HEAP_ARENA_WORDS).expect("started");
        statics
            .write(
                &mut os,
                crate::kernel::statics::at::HEAP,
                HEAP + 4 * HEAP_ARENA_WORDS,
            )
            .expect("heap");
        statics
            .write(&mut os, crate::kernel::statics::at::HEAP_LIMIT, SLOT_END)
            .expect("limit");
        self
    }

    /// The client's code made the code running, as a library entry's caller
    /// is: User mode, on a stack of its own, SL its root chunk's.
    pub(crate) fn running(&mut self) {
        self.running_with(&[]);
    }

    /// The client's code made the code running with `low` from R0 on; SL
    /// and SP as [`Client::running`] makes them and R14 [`RETURN`], a
    /// spinning instruction a routine's return lands on, where `low` does
    /// not reach them.
    pub(crate) fn running_with(&mut self, low: &[u32]) {
        self.put(RETURN, &SPIN.to_le_bytes());
        let mut os = self.session.kernel.os();
        let _ = os.enter_application(Bits::Bit32, RETURN, |user| {
            user[10] = stack_chunk::limit_of(CHUNK);
            user[13] = STACK_TOP;
            user[14] = RETURN;
            user[..low.len()].copy_from_slice(low);
        });
        let _ = self.session.kernel.run(0);
    }

    /// `entry` run as the client's code calling it, from its registers as
    /// they stand, through the kernel's own subroutine call: what came of
    /// it, R0 to R14 after, and where it went on.
    pub(crate) fn subroutine(
        &mut self,
        entry: Code,
    ) -> (Result<(), tairix_riscos_kernel::os::Unwind>, [u32; 15], u32) {
        let mut os = self.session.kernel.os();
        let done = os.as_subroutine(entry);
        let registers = os.current_registers();
        let pc = self.session.kernel.state().pc;
        (done, registers, pc)
    }

    /// `entry` called with `arguments` from R0 on and the client's SL.
    pub(crate) fn call(&mut self, entry: Code, arguments: &[u32]) -> Regs {
        let mut regs = Regs::default();
        regs.r[..arguments.len()].copy_from_slice(arguments);
        regs.r[10] = stack_chunk::limit_of(CHUNK);
        entry(&mut self.session.kernel.os(), &mut regs).expect("the entry answered");
        regs
    }

    /// `entry` called as [`Client::call`] calls it, whatever comes of it.
    pub(crate) fn outcome(
        &mut self,
        entry: Code,
        arguments: &[u32],
    ) -> Result<Regs, tairix_riscos_kernel::os::Fail> {
        let mut regs = Regs::default();
        regs.r[..arguments.len()].copy_from_slice(arguments);
        regs.r[10] = stack_chunk::limit_of(CHUNK);
        entry(&mut self.session.kernel.os(), &mut regs).map(|()| regs)
    }

    /// `entry`'s answer, called as [`Client::call`] calls it.
    pub(crate) fn answer(&mut self, entry: Code, arguments: &[u32]) -> u32 {
        self.call(entry, arguments).r[0]
    }

    pub(crate) fn put(&mut self, at: u32, bytes: &[u8]) {
        self.session
            .kernel
            .os()
            .write_bytes(at, bytes)
            .expect("written");
    }

    pub(crate) fn bytes(&mut self, at: u32, n: usize) -> Vec<u8> {
        self.session.kernel.os().read_vec(at, n).expect("read")
    }

    pub(crate) fn word(&mut self, at: u32) -> u32 {
        self.session.kernel.os().read_word(at).expect("read")
    }

    pub(crate) fn string(&mut self, at: u32) -> Vec<u8> {
        self.session
            .kernel
            .os()
            .read_zstring(at, 4096)
            .expect("a string")
    }
}
