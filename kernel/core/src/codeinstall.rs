//! Installing compiled code: the mechanism behind `code_install`
//! (`plans/BROWSER.md` BR26, `plans/PCEMU.md` PC17).
//!
//! A just-in-time compiler runs in a capability-empty sandbox and writes code
//! into its own anonymous memory; the process that started it moves that
//! memory into a second sandbox, read-execute. No process ever holds a mapping
//! that is writable and executable, and code reaches only the caller's own
//! sandboxed children, a compiler serving one target for its life.
//!
//! Everything the target can refuse is settled before a page leaves the
//! compiler, and the two spaces are never locked at once.

use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_kernel_mem::{
    Frame, LiveUserSpace, Page, RamzipFaultOutcome, Unpublished, VirtAddr, PAGE_SIZE,
};
use tairix_kernel_sec::{ProcessId, TaskCapabilities};
use tairix_kernel_syscall::{CallerContext, SyscallResult};
use tairix_log::Sink;

use crate::aspace::{fold_region_pages, AddressSpaceRegistry, SnapshotRetire};
use crate::bootinfo::KernelArch;
use crate::live_producer::live_errno;
use crate::procspace::ProcessSpace;
use crate::syscalls::{ramzip_unbacked, KernelSyscallHandlers};

/// One end of an install: a live child and its address space.
struct Child {
    process: ProcessId,
    space: alloc::sync::Arc<ProcessSpace>,
}

/// Move the `len`-byte region at `base` of the caller's child `compiler`
/// into its child `target`, read-execute at an address the target's code
/// window chooses, and answer that address.
///
/// # Errors
///
/// As [`tairix_abi::SyscallNumber::CODE_INSTALL`] documents.
pub(crate) fn code_install<A>(
    handlers: &KernelSyscallHandlers<'_, A>,
    sink: &dyn Sink,
    caller: &CallerContext<'_>,
    compiler: i64,
    target: i64,
    base: u64,
    len: usize,
) -> SyscallResult
where
    A: KernelArch + 'static,
{
    let pages = region_pages(base, len)?;
    let (compiler, target) = children(handlers, caller, compiler, target)?;
    if !compiler.space.pair_code_target(&target.space) {
        return Err(Errno::PermissionDenied);
    }
    let bytes = pages * PAGE_SIZE as u64;
    let mut frames = Vec::new();
    frames
        .try_reserve_exact(usize::try_from(pages).map_err(|_| Errno::OutOfRange)?)
        .map_err(|_| Errno::OutOfMemory)?;

    {
        // Checked under the charge's own lock: a target withdrawn meanwhile
        // must not be left a total its successor to the number would inherit.
        let mut aspaces = handlers.aspaces.write();
        if !aspaces.contains(target.process) {
            return Err(Errno::NotFound);
        }
        aspaces.try_charge_aspace_bytes(target.process, bytes)?;
    }
    let refund = || {
        handlers
            .aspaces
            .write()
            .credit_aspace_bytes(target.process, bytes);
    };
    let at = match target.space.with(|live| live.reserve_code(pages)) {
        Ok(at) => at,
        Err(err) => {
            refund();
            return Err(live_errno(err));
        }
    };
    if let Err(errno) = take(handlers, sink, &compiler, base, pages, &mut frames) {
        let _ = target
            .space
            .with(|live| live.release_code(at, pages, &mut Unpublished));
        refund();
        return Err(errno);
    }
    if let Err(errno) = install(handlers, &target, at, &frames) {
        refund();
        return Err(errno);
    }
    Ok(at)
}

/// The whole pages `len` bytes from `base` span, refusing an empty length,
/// an unaligned base, and a region past the address space.
fn region_pages(base: u64, len: usize) -> Result<u64, Errno> {
    if len == 0 {
        return Err(Errno::LengthOutOfRange);
    }
    if !base.is_multiple_of(PAGE_SIZE as u64) {
        return Err(Errno::BadAlignment);
    }
    let pages = (len as u64).div_ceil(PAGE_SIZE as u64);
    pages
        .checked_mul(PAGE_SIZE as u64)
        .and_then(|bytes| base.checked_add(bytes))
        .ok_or(Errno::OutOfRange)?;
    Ok(pages)
}

/// `compiler` and `target`, which must be distinct, sandboxed, live children
/// of the caller.
fn children<A>(
    handlers: &KernelSyscallHandlers<'_, A>,
    caller: &CallerContext<'_>,
    compiler: i64,
    target: i64,
) -> Result<(Child, Child), Errno>
where
    A: KernelArch + 'static,
{
    let compiler = handlers
        .process_wait
        .authorise_child(caller.process(), compiler)?;
    let target = handlers
        .process_wait
        .authorise_child(caller.process(), target)?;
    if compiler == target {
        return Err(Errno::PermissionDenied);
    }
    {
        let caps = handlers.caps.read();
        let sandboxed = |process| {
            caps.caps_of_process(process)
                .is_some_and(TaskCapabilities::is_sandboxed)
        };
        if !sandboxed(compiler) || !sandboxed(target) {
            return Err(Errno::PermissionDenied);
        }
    }
    let aspaces = handlers.aspaces.read();
    let space_of = |process| {
        aspaces
            .live_space(process)
            .map(|space| Child { process, space })
            .ok_or(Errno::NotFound)
    };
    Ok((space_of(compiler)?, space_of(target)?))
}

/// Move the `pages` pages from `base` out of `compiler`'s space into
/// `frames`, the compiler holding them no longer.
///
/// The range is taken out of the compiler's holding first, as an unmap's is,
/// so none of its faults backs a page of it meanwhile; and the pages the
/// compressed tier parked are restored in the same hold of the space that
/// moves them, so no reclaim can park one between.
fn take<A>(
    handlers: &KernelSyscallHandlers<'_, A>,
    sink: &dyn Sink,
    compiler: &Child,
    base: u64,
    pages: u64,
    frames: &mut Vec<Frame>,
) -> Result<(), Errno>
where
    A: KernelArch + 'static,
{
    handlers
        .aspaces
        .write()
        .begin_anon_release(compiler.process, base, pages)?;
    let mut retire = SnapshotRetire::new(handlers.aspaces, compiler.process);
    let taken = compiler.space.with(|live| {
        restore_parked(live, sink, base, pages)?;
        live.detach_anonymous(base, pages, &mut retire, frames)
            .map_err(live_errno)
    });
    if retire.suspended() {
        let replaced = compiler.space.with(|live| {
            AddressSpaceRegistry::refreeze_held(handlers.aspaces, compiler.process, live)
        });
        drop(replaced);
    }
    {
        let mut aspaces = handlers.aspaces.write();
        aspaces.end_anon_release(compiler.process, base, pages, taken.is_ok());
        if taken.is_ok() {
            aspaces.credit_aspace_bytes(compiler.process, pages * PAGE_SIZE as u64);
        }
    }
    crate::aspace::anon_release_wake(compiler.process);
    taken
}

/// Bring back each page of the `pages` from `base` the compressed tier
/// parked; a page that never held anything stays absent, for the move to
/// refuse.
fn restore_parked(
    live: &mut dyn LiveUserSpace,
    sink: &dyn Sink,
    base: u64,
    pages: u64,
) -> Result<(), Errno> {
    let Some(tier) = tairix_kernel_mem::ramzip::global() else {
        return Ok(());
    };
    for index in 0..pages {
        let va = base + index * PAGE_SIZE as u64;
        let page = Page::from_addr(VirtAddr::new(va)).map_err(|_| Errno::OutOfRange)?;
        if live.translate_page(page).is_some() {
            continue;
        }
        if let RamzipFaultOutcome::Fatal(failure) = live.ramzip_fault_in(tier, va, sink) {
            return Err(ramzip_unbacked(failure).errno());
        }
    }
    Ok(())
}

/// Map `frames` at `at`, the placement taken in `target`'s code window, and
/// publish them to its snapshot so the copy path reads what it executes.
fn install<A>(
    handlers: &KernelSyscallHandlers<'_, A>,
    target: &Child,
    at: u64,
    frames: &[Frame],
) -> Result<(), Errno>
where
    A: KernelArch + 'static,
{
    let pages = frames.len() as u64;
    let replaced = target.space.with(|live| -> Result<_, Errno> {
        live.map_code(at, frames).map_err(live_errno)?;
        let absorbed = {
            let mut aspaces = handlers.aspaces.write();
            fold_region_pages(at, pages, |page| {
                aspaces.note_faulted_page(target.process, page, live.translate_page(page))
            })
        };
        Ok(if absorbed {
            None
        } else {
            AddressSpaceRegistry::refreeze_held(handlers.aspaces, target.process, live)
        })
    })?;
    drop(replaced);
    Ok(())
}
