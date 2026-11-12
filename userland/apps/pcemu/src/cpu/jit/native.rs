//! Blocks run natively: the entry into code the front-end installed in this
//! process's code window, and the helper table that code calls back
//! through. The engine's only `unsafe`.
//!
//! A block is entered with a pointer to an `Entered`: the frame at its
//! base, which is all the code addresses, and beside it the environment the
//! refresh helper reaches, so the one pointer the code hands back to that
//! helper reaches both. The store the page map's addresses lie in is exposed
//! before each entry: the code reaches it only through those addresses.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr;

use tairix_abi::PAGE_SIZE;

use super::dispatch::{Env, Execute};
use super::frame::Frame;
use super::page::PageMap;
use super::{helper, Exit, MOST_CODE_BYTES};

/// The frame a block is entered with, and the environment its refresh
/// helper reaches.
#[repr(C)]
pub(crate) struct Entered<'e> {
    frame: Frame,
    env: *mut Env<'e>,
}

/// What a block's code is entered as: the frame, the page map's words and
/// the helper table, its exit answered.
pub(crate) type Entry =
    for<'e> unsafe extern "C" fn(*mut Entered<'e>, *const u64, *const Helpers) -> u64;

/// A pure helper as the code calls it: its arguments in the first
/// registers, those it does not take unread.
type Pure = extern "C" fn(u64, u64, u64, u64, u64) -> u64;

/// The arguments the code passes a helper at most: as many as a [`Pure`]
/// reads.
const MOST_ARGS: usize = 5;

const _: () = {
    let mut n = 0;
    let mut most = 0;
    while n < helper::ARITY.len() {
        if helper::ARITY[n] > most {
            most = helper::ARITY[n];
        }
        n += 1;
    }
    assert!(most == MOST_ARGS);
};

/// The helper table, in [`helper`]'s order: the code calls helper `n`
/// through the word at `8 * n`.
#[repr(C)]
pub(crate) struct Helpers {
    refresh: for<'e> unsafe extern "C" fn(*mut Entered<'e>, u64) -> u64,
    pure: [Pure; helper::COUNT as usize - 1],
}

const _: () = assert!(
    helper::REFRESH == 0 && size_of::<Helpers>() == helper::COUNT as usize * size_of::<u64>()
);

static HELPERS: Helpers = Helpers {
    refresh,
    pure: [
        pure::<1>, pure::<2>, pure::<3>, pure::<4>, pure::<5>, pure::<6>, pure::<7>, pure::<8>,
    ],
};

/// Pure helper `INDEX`, as the interpreter computes it.
extern "C" fn pure<const INDEX: u32>(arg0: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64) -> u64 {
    let passed: [u64; MOST_ARGS] = [arg0, arg1, arg2, arg3, arg4];
    helper::call(INDEX, &passed).unwrap_or(0)
}

/// [`helper::REFRESH`]: the bus's refresh at cycle `at`, through the
/// environment the block was entered with.
///
/// # Safety
///
/// `entered` is the pointer [`Native::run`] entered the running block with.
#[allow(unsafe_code)] // Reached only from the code `Native::run` entered.
unsafe extern "C" fn refresh(entered: *mut Entered<'_>, at: u64) -> u64 {
    // SAFETY: the code hands back the pointer it was entered with and holds
    // no reference into what it names while it calls; `Native::run` keeps
    // the frame and the environment live, and borrows neither, until the
    // code returns.
    let entered = unsafe { &mut *entered };
    // SAFETY: as above: `env` is the environment `run` was handed, a value
    // apart from the frame.
    let env = unsafe { &mut *entered.env };
    env.refresh(&mut entered.frame, at)
}

/// A block's code, installed.
struct Installed {
    entry: Entry,
    address: u64,
    len: u64,
}

/// Runs code installed in this process's code window.
pub struct Native {
    /// The blocks, by handle.
    blocks: Vec<Option<Installed>>,
    /// Handles given back, for the next block to take.
    free: Vec<usize>,
    /// Bytes of the window the blocks take: each its whole pages.
    held: u64,
    /// Gives back the `len` bytes of installed code at an address.
    unmap: Box<dyn FnMut(u64, u64)>,
}

impl core::fmt::Debug for Native {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Native")
            .field("held", &self.held)
            .finish_non_exhaustive()
    }
}

/// The window bytes `len` bytes of code take.
const fn pages_of(len: u64) -> u64 {
    len.div_ceil(PAGE_SIZE as u64) * PAGE_SIZE as u64
}

impl Drop for Native {
    fn drop(&mut self) {
        for block in self.blocks.drain(..).flatten() {
            (self.unmap)(block.address, block.len);
        }
    }
}

impl Native {
    /// An executor of the code [`Execute::adopt`] is handed, given back
    /// through `unmap`.
    ///
    /// # Safety
    ///
    /// Every address `adopt` is handed must be the first byte of the code a
    /// block was compiled to for this processor, by this crate's translator
    /// and `lib/codegen`, installed read-execute in this process, `len`
    /// bytes long and entered as an `Entry`; it stays installed until
    /// `unmap` gives it back, which nothing but this executor does.
    #[allow(unsafe_code)] // The promise is the caller's, as stated.
    #[must_use]
    pub unsafe fn new(unmap: Box<dyn FnMut(u64, u64)>) -> Self {
        Self {
            blocks: Vec::new(),
            free: Vec::new(),
            held: 0,
            unmap,
        }
    }

    /// Takes `entry`, the code at `address`: its handle.
    fn install(&mut self, entry: Entry, address: u64, len: u64) -> Option<u64> {
        let block = Installed {
            entry,
            address,
            len,
        };
        let at = if let Some(at) = self.free.pop() {
            self.blocks[at] = Some(block);
            at
        } else {
            self.blocks.try_reserve(1).ok()?;
            self.blocks.push(Some(block));
            self.blocks.len() - 1
        };
        self.held += pages_of(len);
        u64::try_from(at).ok()
    }
}

/// Whether `len` bytes at `address` could be a block's code: whole pages
/// from a page's start, no more than any block compiles to.
fn could_be_code(address: u64, len: u64) -> bool {
    address != 0
        && address.is_multiple_of(PAGE_SIZE as u64)
        && (1..=MOST_CODE_BYTES as u64).contains(&len)
        && address.checked_add(pages_of(len)).is_some()
}

/// The code at `address`, as it is entered.
#[allow(unsafe_code)] // The one conversion of an address to code.
fn entry_at(address: u64) -> Option<Entry> {
    let address = usize::try_from(address).ok()?;
    // The kernel mapped the code outside the abstract machine, so its
    // provenance is exposed by definition.
    let code = ptr::with_exposed_provenance::<u8>(address);
    // SAFETY: a data pointer and a function pointer are the same size on
    // every target this runs on, and `code` is not null (`could_be_code`).
    // What it names is `Native::new`'s promise; making the pointer runs
    // nothing.
    Some(unsafe { core::mem::transmute::<*const u8, Entry>(code) })
}

impl Execute for Native {
    #[allow(unsafe_code)] // The entry into installed code, as proven below.
    fn run(
        &mut self,
        code: u64,
        frame: &mut Frame,
        pages: &PageMap,
        env: &mut Env<'_>,
    ) -> Option<Exit> {
        let entry = self
            .blocks
            .get(usize::try_from(code).ok()?)?
            .as_ref()?
            .entry;
        // The code reaches the store only through the page map's addresses.
        let _ = env.store.as_mut_ptr().expose_provenance();
        let mut entered = Entered {
            frame: frame.clone(),
            env: ptr::from_mut(env),
        };
        // SAFETY: `entry` is a block's code, as `Native::new` was promised,
        // entered as `Entry` says: `entered`'s frame, which the code reads
        // and writes whole, beside the environment it hands back to
        // `refresh`, both live and unborrowed for the call; the page map's
        // words, which it only reads and which name only bytes of
        // `env.store`, exposed above, storing only where they say it may;
        // and the helper table, whose every entry it calls with the
        // arguments that entry takes.
        let answer = unsafe { entry(&raw mut entered, pages.words().as_ptr(), &raw const HELPERS) };
        *frame = entered.frame;
        Exit::of(answer)
    }

    fn adopt(&mut self, address: u64, len: u64) -> Option<u64> {
        if !could_be_code(address, len) {
            return None;
        }
        let handle = self.install(entry_at(address)?, address, len);
        if handle.is_none() {
            // Code there was no room to keep is given back, not leaked.
            (self.unmap)(address, len);
        }
        handle
    }

    fn release(&mut self, code: u64) {
        let Some(at) = usize::try_from(code).ok() else {
            return;
        };
        let Some(block) = self.blocks.get_mut(at).and_then(Option::take) else {
            return;
        };
        self.held -= pages_of(block.len);
        (self.unmap)(block.address, block.len);
        if self.free.try_reserve(1).is_ok() {
            self.free.push(at);
        }
    }

    fn held(&self) -> u64 {
        self.held
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;

// Apart from `tests`, which the UB oracle interprets: these make code of an
// address, which it refuses under strict provenance.
#[cfg(test)]
#[path = "native_address_tests.rs"]
mod address_tests;
