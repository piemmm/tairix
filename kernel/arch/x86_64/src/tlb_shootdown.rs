//! Cross-CPU TLB shootdown on x86_64 (the `plans/WIRING.md` W6 slice of
//! the Arch HAL "TLB shootdown" surface).
//!
//! x86_64 has no broadcast TLB-invalidation instruction: `invlpg` only
//! affects the CPU that executes it. To make a page-table edit globally
//! visible the initiating CPU must therefore *interrupt* every other
//! online CPU and have each one run `invlpg` for the affected page —
//! the classic inter-processor "TLB shootdown". This module owns that
//! protocol; `crate::kernel_arch::X86_64Arch` implements
//! [`tairix_arch_api::CrossCpuTlbShootdown`] over it.
//!
//! # Protocol
//!
//! A single global descriptor (`SHOOTDOWN`) serialises shootdowns behind a
//! spin-acquired flag, so the (rare) page-table-teardown path never races
//! itself across CPUs. Each CPU is asked through a slot of its own in the
//! caller-sized per-CPU state the arch handle publishes (`install`), keyed
//! by its dense id, so a CPU's APIC id may be any width and the machine any
//! size. The initiator:
//!
//! 1. finds its first target other than itself that has an APIC id, and
//!    only then spin-acquires the descriptor lock,
//! 2. stores the target range,
//! 3. asks each distinct CPU but itself (`ask`): counts it into the
//!    outstanding-acknowledge count, then publishes its slot as owing —
//!    the slot is the "go" signal, and the count is never below the slots
//!    owing,
//! 4. raises a [`TLB_SHOOTDOWN_VECTOR`] IPI at each CPU it asked, in one
//!    batch the local APIC orders after every slot it published,
//! 5. invalidates the range on *itself* with `invlpg` — unless it asked for
//!    the remote half only (`shootdown_remote`, a user unmap that already
//!    flushed each page as it cleared it),
//! 6. spins until every target has acknowledged (the count reaches zero),
//!    then releases the lock.
//!
//! With no target at all — the single-CPU case — there is nobody to ask or
//! wait for, so the call is just the local `invlpg` sweep (or nothing, for
//! the remote half). A range past `SINGLE_PAGE_FLUSH_CEILING` pages reloads
//! `CR3` instead of issuing an `invlpg` per page, on the initiator and on
//! every target alike.
//!
//! The spin in step 6 is a genuine, bounded synchronisation, not a "retry
//! until it works" bring-up hack: under-invalidating (returning before a CPU
//! has flushed) is the only failure mode, so the initiator *must* wait for
//! the acknowledge.
//!
//! # A target acknowledges from a spin as readily as from its ISR
//!
//! The acknowledge is `serve_pending`, reached from two places: the
//! shootdown ISR, and any spin round in `lib/sync` (the boot path installs it
//! as that crate's spin service). Both matter, because a CPU whose own
//! interrupts are masked cannot take the IPI at all — and masking is exactly
//! what `tairix_sync::IrqSafeSpinLock` does for the whole of its acquire
//! spin, the kernel heap's lock included. An initiator holding that heap lock
//! and a second CPU spinning to acquire it would otherwise wait on each other
//! for ever: the initiator for an acknowledge the masked CPU cannot send, the
//! masked CPU for a lock the initiator will not release until it has one.
//!
//! Serving from a spin means a target could otherwise acknowledge twice —
//! once from the spin, once when the deferred IPI is finally delivered —
//! double-decrementing the count and returning the *next* initiator early,
//! i.e. under-invalidating. The slot forecloses it: a target claims by
//! clearing its own owing bit, so the prior value that `fetch_and` returns is
//! both the claim and the "am I a target?" test, and exactly one caller can
//! win it. A CPU not owing — a stale delivery, a CPU that was never asked,
//! the initiator itself — invalidates nothing and decrements nothing. The
//! ISR still writes its LAPIC EOI unconditionally: the in-service bit is set
//! whether or not there was work to do.
//!
//! # Host build
//!
//! The descriptor, the ISR, and the install helper are gated to
//! `target_os = "none"`: they reach LAPIC MMIO and the per-CPU IDT. The
//! slot bookkeeping is not, so the decisions the protocol rests on — who
//! is asked, who is excluded, how many acknowledges are owed, who may claim
//! one — are host-tested below. The host `X86_64Arch` shootdown impl is a vacuous
//! no-op (there is no second CPU and no TLB) and the conformance vertical
//! asserts only that the call is total and panic-free; the real cross-CPU
//! round-trip, including the masked-spin acknowledge, is proven by the
//! `cross_cpu_tlb_shootdown_qemu_x86_64` QEMU vertical.
//!

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
use core::sync::atomic::{AtomicBool, AtomicU64};
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
use core::sync::atomic::{AtomicU32, AtomicU8, AtomicUsize, Ordering};

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
use crate::interrupts::SavedRegs;

/// IDT vector the cross-CPU TLB-shootdown IPI is delivered on.
///
/// One past [`crate::preempt::TIMER_VECTOR`] (`0x20`); the first
/// user-defined vectors are `0x20..` (Intel SDM Vol 3A §6.3.1). The
/// constant is `pub` so the integration test can cross-check the IDT
/// slot it installs.
pub const TLB_SHOOTDOWN_VECTOR: u8 = 0x21;

/// A CPU's slot owing nothing.
#[cfg(any(test, feature = "sched-arch"))]
pub(crate) const IDLE: u8 = 0;

/// The slot's owing bit, which the CPU clears as its claim.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const OWED: u8 = 1 << 0;

/// The slot's asked bit: the initiator's own mark of the CPUs it is to
/// interrupt, which no target reads.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const ASKED: u8 = 1 << 1;

/// Ask each distinct CPU `targets` yields, but `own` and any `apic` names no
/// APIC for, to acknowledge: count it into `pending`, then publish its slot
/// in `owed` as owing. `take` runs once, before the first CPU is asked, to
/// take the descriptor, so a set of no CPU but the caller's costs no lock;
/// the answer is whether it ran, the caller then owing the release.
///
/// Counting before publishing keeps `pending` from ever falling below the
/// slots owing, and skipping a slot already asked is what stops a repeated
/// CPU inflating the count into a wait that never ends; excluding the caller
/// is what makes the wait unable to wait on itself. A slot's asked mark is
/// only read once the descriptor is held, since another initiator's marks
/// stand until it has raised its IPIs.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
fn ask(
    owed: &[AtomicU8],
    apic: &[AtomicU32],
    pending: &AtomicUsize,
    targets: impl Iterator<Item = u32>,
    own: u32,
    mut take: impl FnMut(),
) -> bool {
    let mut taken = false;
    for cpu in targets.filter(|&cpu| cpu != own) {
        let Some((slot, lapic)) = usize::try_from(cpu)
            .ok()
            .and_then(|index| Some((owed.get(index)?, apic.get(index)?)))
        else {
            continue;
        };
        if lapic.load(Ordering::Relaxed) == crate::cpumap::NO_LAPIC {
            continue;
        }
        if !taken {
            take();
            taken = true;
        }
        if slot.load(Ordering::Relaxed) & ASKED != 0 {
            continue;
        }
        pending.fetch_add(1, Ordering::Relaxed);
        // `Release`: the target that claims it reads the range and the count
        // stored before.
        slot.fetch_or(ASKED | OWED, Ordering::Release);
    }
    taken
}

/// The APIC id of each CPU [`ask`] asked, its mark cleared: the IPIs the
/// shootdown raises, once every slot is published.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
fn asked<'s>(owed: &'s [AtomicU8], apic: &'s [AtomicU32]) -> impl Iterator<Item = u32> + 's {
    owed.iter()
        .zip(apic)
        .filter(|(slot, _)| slot.load(Ordering::Relaxed) & ASKED != 0)
        .map(|(slot, lapic)| {
            // An RMW, as the target may be clearing its owing bit.
            slot.fetch_and(!ASKED, Ordering::Relaxed);
            lapic.load(Ordering::Relaxed)
        })
}

/// Claim `cpu`'s acknowledge, if its slot in `owed` owes one: exactly one
/// caller can.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
fn claim(owed: &[AtomicU8], cpu: u32) -> bool {
    usize::try_from(cpu)
        .ok()
        .and_then(|index| owed.get(index))
        // Read first: a spinning CPU that owes nothing leaves the line its
        // neighbours' slots share unwritten. `AcqRel` on the claim, so the
        // range read after cannot be hoisted above it and the claim
        // synchronises-with the slot's publication.
        .is_some_and(|slot| {
            slot.load(Ordering::Relaxed) & OWED != 0
                && slot.fetch_and(!OWED, Ordering::AcqRel) & OWED != 0
        })
}

/// The per-CPU state the arch handle publishes: each CPU's slot, and its
/// APIC id, both by dense id.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[derive(Clone, Copy)]
struct PerCpu {
    owed: &'static [AtomicU8],
    apic: &'static [AtomicU32],
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
static PER_CPU: tairix_sync::once::OnceCell<PerCpu> = tairix_sync::once::OnceCell::new();

/// Publish each CPU's slot `owed` and APIC id `apic`, by dense id: once per
/// boot, by the arch handle, before any other CPU starts.
#[cfg(all(target_arch = "x86_64", target_os = "none", feature = "sched-arch"))]
pub(crate) fn install(owed: &'static [AtomicU8], apic: &'static [AtomicU32]) {
    let _ = PER_CPU.set(PerCpu { owed, apic });
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
fn per_cpu() -> Option<PerCpu> {
    PER_CPU.get().ok().flatten().copied()
}

/// The calling CPU's dense id, [`u32::MAX`] where it has none, which no
/// target is.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
fn own_cpu() -> u32 {
    crate::preempt::cpu_id_for_lapic(crate::apic::local_apic_id())
}

/// Global, lock-serialised shootdown descriptor.
///
/// There is at most one in-flight cross-CPU shootdown system-wide; the
/// `lock` flag enforces that.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
struct ShootdownMailbox {
    /// Spin flag: `true` while an initiator owns the descriptor.
    lock: AtomicBool,
    /// First page of the range every target must `invlpg`.
    vaddr: AtomicU64,
    /// How many consecutive 4 KiB pages from `vaddr` to invalidate.
    pages: AtomicUsize,
    /// Outstanding acknowledges; the initiator waits for this to reach 0.
    ///
    /// Never below the number of slots still owing, because a CPU is
    /// counted before its slot is published and clears its slot before it
    /// decrements. So `pending == 0` proves no slot owes, which is what lets
    /// [`serve_pending`] gate on one load.
    pending: AtomicUsize,
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
static SHOOTDOWN: ShootdownMailbox = ShootdownMailbox {
    lock: AtomicBool::new(false),
    vaddr: AtomicU64::new(0),
    pages: AtomicUsize::new(0),
    pending: AtomicUsize::new(0),
};

/// Invalidate `pages` consecutive 4 KiB pages from the page containing
/// `vaddr` on the calling CPU and on every CPU whose dense id `targets`
/// yields, returning once all of them have acknowledged.
///
/// `targets` may yield the calling CPU's own id and may repeat one: asking
/// excludes the caller and collapses duplicates (`ask`). A zero page count
/// is a no-op, and an empty target set degrades to a purely local `invlpg`
/// sweep.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn shootdown<I>(vaddr: u64, pages: usize, targets: I)
where
    I: Iterator<Item = u32>,
{
    run(vaddr, pages, targets, true);
}

/// [`shootdown`] for a caller that has already invalidated the range on
/// itself: only the CPUs `targets` yields are reached, and an empty or
/// self-only target set costs nothing at all.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn shootdown_remote<I>(vaddr: u64, pages: usize, targets: I)
where
    I: Iterator<Item = u32>,
{
    run(vaddr, pages, targets, false);
}

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
fn run<I>(vaddr: u64, pages: usize, targets: I, flush_self: bool)
where
    I: Iterator<Item = u32>,
{
    if pages == 0 {
        return;
    }
    // Before the arch handle publishes its CPUs there is only the boot CPU.
    let Some(PerCpu { owed, apic }) = per_cpu() else {
        if flush_self {
            invlpg_range(vaddr, pages);
        }
        return;
    };
    let own = own_cpu();
    let take = || {
        // Acquire the descriptor, serving any request already in flight: this
        // spin is reached with interrupts masked (the kernel-heap teardown is
        // such a caller), so waiting without serving would leave the CPU
        // holding the descriptor waiting on an acknowledge this CPU cannot
        // send.
        while SHOOTDOWN
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            serve_pending();
            core::hint::spin_loop();
        }
        // The range goes out before any slot is published as owing: a target
        // that claims its slot reads it at once.
        SHOOTDOWN.vaddr.store(vaddr, Ordering::Relaxed);
        SHOOTDOWN.pages.store(pages, Ordering::Relaxed);
    };
    if !ask(owed, apic, &SHOOTDOWN.pending, targets, own, take) {
        if flush_self {
            invlpg_range(vaddr, pages);
        }
        return;
    }
    // Raised only now, after every slot is published, so no target can take
    // its IPI before its slot owes and miss the request.
    let mut lapic = crate::apic::Lapic::new(crate::apic::LocalApic);
    lapic.send_ipis(
        asked(owed, apic),
        crate::apic::DeliveryMode::Fixed,
        TLB_SHOOTDOWN_VECTOR,
    );

    // Invalidate locally while the targets are flushing in parallel.
    if flush_self {
        invlpg_range(vaddr, pages);
    }

    // Wait for every interrupted CPU to acknowledge. `Acquire` pairs with the
    // acknowledge's `Release` decrement so the remote `invlpg`s are ordered
    // before this call returns. Serving here would be dead work: this CPU
    // never asks itself, so it can never be a target of its own request.
    while SHOOTDOWN.pending.load(Ordering::Acquire) != 0 {
        core::hint::spin_loop();
    }

    SHOOTDOWN.lock.store(false, Ordering::Release);
}

/// Claim and discharge this CPU's outstanding acknowledge, if it has one.
///
/// Total and idempotent: a CPU that was never asked, or that has already
/// acknowledged, invalidates nothing and decrements nothing. The boot path
/// installs it as `lib/sync`'s spin service so a CPU spinning with its
/// interrupts masked still acknowledges; the shootdown ISR calls it for the
/// ordinary interrupt-delivered case.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub fn serve_pending() {
    // Nothing in flight is the overwhelmingly common case and must cost one
    // load rather than a LAPIC read. Sound because `pending` is never below
    // the number of slots owing.
    if SHOOTDOWN.pending.load(Ordering::Acquire) == 0 {
        return;
    }
    let Some(PerCpu { owed, .. }) = per_cpu() else {
        return;
    };
    if !claim(owed, own_cpu()) {
        return;
    }

    // Winning the claim proves this CPU is a target that has not yet
    // acknowledged, so the initiator is still inside `shootdown` holding the
    // descriptor and the range read here is still its range.
    let pages = SHOOTDOWN.pages.load(Ordering::Relaxed);
    let vaddr = SHOOTDOWN.vaddr.load(Ordering::Relaxed);
    invlpg_range(vaddr, pages);

    // Acknowledge last, with `Release`, so the `invlpg`s above are ordered
    // before the initiator's `Acquire` load observes the decrement.
    SHOOTDOWN.pending.fetch_sub(1, Ordering::Release);
}

/// Past this many pages one `CR3` reload is cheaper than an `invlpg` each:
/// Linux's measured `tlb_single_page_flush_ceiling`
/// (`Documentation/arch/x86/tlb.rst`). It also bounds a sparse unmap's span,
/// which would otherwise cost an `invlpg` per page of its gaps.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const SINGLE_PAGE_FLUSH_CEILING: usize = 33;

/// Whether invalidating `pages` pages should reload `CR3` instead.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
const fn flushes_whole_tlb(pages: usize) -> bool {
    pages > SINGLE_PAGE_FLUSH_CEILING
}

/// Invalidate the calling CPU's TLB entries for `pages` consecutive 4 KiB
/// pages from the page containing `vaddr`.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
fn invlpg_range(vaddr: u64, pages: usize) {
    const PAGE_BYTES: u64 = 4096;
    if flushes_whole_tlb(pages) {
        crate::paging::invalidate_all_local();
        return;
    }
    let mut page = vaddr & !(PAGE_BYTES - 1);
    for _ in 0..pages {
        invlpg(page);
        page = page.wrapping_add(PAGE_BYTES);
    }
}

/// Invalidate the calling CPU's TLB entry for the page containing
/// `vaddr`.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
fn invlpg(vaddr: u64) {
    // SAFETY: `invlpg` invalidates the calling CPU's TLB entry for the
    // page containing the operand address; it touches no memory and only
    // discards a cached translation. No Rust spelling exists. This is the
    // same instruction `crate::paging`'s local `TlbShootdown::flush_page`
    // issues (the local-invalidation primitive); the cross-CPU initiator
    // and the per-target ISR below both reuse it.
    unsafe {
        core::arch::asm!(
            "invlpg [{addr}]",
            addr = in(reg) vaddr,
            options(nostack, preserves_flags),
        );
    }
}

/// Rust trampoline called by the shootdown ISR stub.
///
/// # Safety
///
/// Only callable from the ISR stub. Invoking it from arbitrary Rust is
/// undefined behaviour because the EOI write assumes the LAPIC's
/// in-service bit is set for [`TLB_SHOOTDOWN_VECTOR`].
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[no_mangle]
unsafe extern "C" fn tairix_arch_x86_64_tlb_shootdown_dispatch(_regs: *mut SavedRegs) {
    serve_pending();

    // Unconditional, unlike the acknowledge above: the in-service bit is set
    // for this vector whether or not this CPU still owed one.
    crate::apic::local_eoi();
}

// Emit the ISR stub the IDT vector points at (gated to the freestanding
// target by the macro, exactly like the timer ISR).
crate::define_isr!(tairix_arch_x86_64_isr_tlb_shootdown => tairix_arch_x86_64_tlb_shootdown_dispatch);

/// Linear address of the shootdown ISR stub, for IDT installation.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[must_use]
pub fn tlb_shootdown_isr_addr() -> u64 {
    tairix_arch_x86_64_isr_tlb_shootdown as *const () as usize as u64
}

/// Install the cross-CPU TLB-shootdown ISR in the calling CPU's per-CPU
/// IDT at [`TLB_SHOOTDOWN_VECTOR`].
///
/// Called once on every CPU as it comes online, alongside
/// [`crate::preempt::init_local_preempt`], so the CPU can service a
/// shootdown IPI. Unlike the timer it programs no device — the vector is
/// raised on demand by `shootdown`.
///
/// # Errors
///
/// * [`crate::percpu::InitError::CpuIndexOutOfRange`] if `cpu_index` is
///   outside the registered [`crate::percpu::PerCpuStorage`].
/// * [`crate::percpu::InitError::NotInitialised`] if
///   [`crate::percpu::init`] has not yet run for `cpu_index`.
///
/// # Safety
///
/// * `cpu_index` must be the index passed to [`crate::percpu::init`] on
///   *this* CPU.
/// * Interrupts on the calling CPU must be disabled during install.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub unsafe fn init_local_tlb_shootdown(cpu_index: usize) -> Result<(), crate::percpu::InitError> {
    // SAFETY: caller's contract guarantees this is the CPU whose index
    // was passed to `percpu::init`, and interrupts are disabled.
    unsafe {
        crate::percpu::install_vector(cpu_index, TLB_SHOOTDOWN_VECTOR, tlb_shootdown_isr_addr())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::sync::atomic::{AtomicU32, AtomicU8, AtomicUsize, Ordering};
    use std::vec::Vec;

    use super::{
        ask, asked, claim, flushes_whole_tlb, ASKED, IDLE, OWED, SINGLE_PAGE_FLUSH_CEILING,
        TLB_SHOOTDOWN_VECTOR,
    };
    use crate::cpumap::NO_LAPIC;

    /// A machine of five CPUs, the fourth with no APIC id, the first past
    /// the eight bits xAPIC names.
    fn machine() -> ([AtomicU8; 5], [AtomicU32; 5]) {
        (
            [const { AtomicU8::new(IDLE) }; 5],
            [0x100, 1, 2, NO_LAPIC, 0xFFFF_FFFE].map(AtomicU32::new),
        )
    }

    fn owing(owed: &[AtomicU8]) -> [bool; 5] {
        core::array::from_fn(|cpu| owed[cpu].load(Ordering::Relaxed) & OWED != 0)
    }

    #[test]
    fn the_caller_is_never_asked_so_it_cannot_wait_on_itself() {
        let ((owed, apic), pending) = (machine(), AtomicUsize::new(0));
        // The caller is listed anyway, exactly as a careless caller would.
        assert!(ask(&owed, &apic, &pending, [0, 1, 2].into_iter(), 1, || {}));
        assert_eq!(pending.load(Ordering::Relaxed), 2);
        assert_eq!(owing(&owed), [true, false, true, false, false]);
    }

    #[test]
    fn a_repeated_cpu_owes_one_acknowledge_not_two() {
        // A duplicate that inflated the count would leave the initiator
        // waiting for an acknowledge no CPU owes it.
        let ((owed, apic), pending) = (machine(), AtomicUsize::new(0));
        assert!(ask(
            &owed,
            &apic,
            &pending,
            [4, 4, 4, 2, 2].into_iter(),
            0,
            || {}
        ));
        assert_eq!(pending.load(Ordering::Relaxed), 2);
        let ipis: Vec<u32> = asked(&owed, &apic).collect();
        assert_eq!(ipis, [2, 0xFFFF_FFFE], "one IPI per distinct target");
    }

    #[test]
    fn a_cpu_with_no_apic_id_or_past_the_machine_is_never_asked() {
        let ((owed, apic), pending) = (machine(), AtomicUsize::new(0));
        assert!(!ask(
            &owed,
            &apic,
            &pending,
            [3, 5, u32::MAX].into_iter(),
            0,
            || {}
        ));
        assert_eq!(pending.load(Ordering::Relaxed), 0);
        assert_eq!(asked(&owed, &apic).count(), 0);
    }

    #[test]
    fn an_empty_or_self_only_target_set_asks_nothing() {
        let ((owed, apic), pending) = (machine(), AtomicUsize::new(0));
        let taken = core::cell::Cell::new(0);
        let take = || taken.set(taken.get() + 1);
        assert!(!ask(&owed, &apic, &pending, core::iter::empty(), 2, take));
        assert!(!ask(&owed, &apic, &pending, [2, 3].into_iter(), 2, take));
        assert_eq!(pending.load(Ordering::Relaxed), 0);
        assert_eq!(taken.get(), 0, "no descriptor taken for nobody to ask");
        assert!(ask(&owed, &apic, &pending, [2, 0, 1].into_iter(), 2, take));
        assert_eq!(taken.get(), 1, "taken once, before the first is asked");
    }

    /// Each CPU asked is raised by its APIC id whatever its width, once, and
    /// its mark is gone after, while it still owes until it claims.
    #[test]
    fn every_asked_cpu_is_raised_once_by_its_apic_id() {
        let ((owed, apic), pending) = (machine(), AtomicUsize::new(0));
        assert!(ask(&owed, &apic, &pending, [0, 2, 4].into_iter(), 1, || {}));
        let ipis: Vec<u32> = asked(&owed, &apic).collect();
        assert_eq!(ipis, [0x100, 2, 0xFFFF_FFFE]);
        assert_eq!(asked(&owed, &apic).count(), 0, "raised once");
        assert!(owed
            .iter()
            .all(|slot| slot.load(Ordering::Relaxed) & ASKED == 0));
        assert_eq!(owing(&owed), [true, false, true, false, true]);
    }

    /// A CPU claims its acknowledge exactly once, a CPU not asked never, and
    /// a round leaves every slot idle once each target has claimed.
    #[test]
    fn an_acknowledge_is_claimed_once_and_a_round_ends_idle() {
        let ((owed, apic), pending) = (machine(), AtomicUsize::new(0));
        assert!(!claim(&owed, 0), "nothing owed before the round");
        assert!(ask(&owed, &apic, &pending, [0, 4].into_iter(), 2, || {}));
        assert!(claim(&owed, 0));
        assert!(!claim(&owed, 0), "a second delivery claims nothing");
        assert!(!claim(&owed, 1), "never asked");
        assert!(!claim(&owed, 99), "past the machine");
        // A target may claim before the initiator raises it.
        assert_eq!(asked(&owed, &apic).count(), 2);
        assert!(claim(&owed, 4));
        assert!(owed.iter().all(|slot| slot.load(Ordering::Relaxed) == IDLE));
    }

    #[test]
    fn a_range_past_the_ceiling_reloads_cr3_and_one_at_it_does_not() {
        assert!(!flushes_whole_tlb(0));
        assert!(!flushes_whole_tlb(SINGLE_PAGE_FLUSH_CEILING));
        assert!(flushes_whole_tlb(SINGLE_PAGE_FLUSH_CEILING + 1));
        assert!(flushes_whole_tlb(usize::MAX));
    }

    #[test]
    fn shootdown_vector_is_one_past_the_timer_vector() {
        // The timer owns 0x20 (the first user vector); the shootdown IPI
        // takes the next slot. If this changes, the QEMU vertical that
        // installs the vector must change in lock-step.
        assert_eq!(TLB_SHOOTDOWN_VECTOR, 0x21);
        assert_eq!(TLB_SHOOTDOWN_VECTOR, crate::preempt::TIMER_VECTOR + 1);
    }
}
