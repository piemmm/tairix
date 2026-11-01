//! Host unit tests for the SMP bring-up surface.
//!
//! The secondary-stack pool registration, the cpu-id validity check, the
//! set-once secondary-entry slot, and the `StartCpuError` decode build and
//! run on the host. The `MPIDR_EL1` read, the PSCI `CPU_ON` call, and the
//! secondary trampoline are exercised by the multi-core QEMU vertical.

use super::*;

extern "C" fn host_entry(_cpu: CpuId) -> ! {
    // Never invoked on the host: the tests only round-trip its address
    // through the set-once slot. Parking keeps the `-> !` signature
    // honest if it ever were called.
    loop {
        core::hint::spin_loop();
    }
}

/// A core's answer and its verdict each sit alone in a writeback granule at
/// the start of its slot, and the slot ends on a granule, so nothing a cache
/// holds of any other data can be written back over either.
#[test]
fn a_slot_keeps_its_answer_and_verdict_alone_in_their_granules() {
    assert_eq!(core::mem::align_of::<SecondaryStack>(), WRITEBACK_GRANULE);
    assert_eq!(core::mem::offset_of!(SecondaryStack, answer), 0);
    assert_eq!(
        core::mem::offset_of!(SecondaryStack, verdict),
        WRITEBACK_GRANULE
    );
    assert_eq!(
        core::mem::offset_of!(SecondaryStack, bytes),
        2 * WRITEBACK_GRANULE
    );
    assert_eq!(
        core::mem::size_of::<SecondaryStack>(),
        2 * WRITEBACK_GRANULE + SECONDARY_STACK_BYTES
    );
}

/// A started core's first push, at its slot's end through the stack pointer
/// the trampoline derives from the published base, while `slot` is a shared
/// argument of the code running on that stack; what the slot answers after.
fn push_while_shared(slot: &SecondaryStack, top: *mut u8) -> u64 {
    // SAFETY: the caller passes the last byte of `slot`'s own stack.
    unsafe { top.write(0xA5) };
    slot.answer.0.load(Ordering::Acquire)
}

/// The registered pool is one global, so this one test owns it: the ids it
/// covers, the whole slot the trampoline steps per core, each core's own
/// slot of it, a core's stack written beneath its shared slot, and
/// registration set once.
#[test]
fn cpu_validity_tracks_the_registered_pool() {
    // A caller-sized pool covers exactly its `N` slots (the capacity
    // is the discovered core count, not a baked-in `MAX_CPUS`); a second
    // pool proves registration is set-once. Declared first so they precede
    // the statements that drive them.
    static POOL: SecondaryStackPool<3> = SecondaryStackPool::new();
    static POOL2: SecondaryStackPool<2> = SecondaryStackPool::new();

    reset_secondary_stacks_for_tests();
    // Fail closed before any pool is registered: every id is invalid, so
    // a `start_secondary` cannot select an unbacked stack slice.
    assert!(!is_valid_cpu(0));
    assert!(slot(1).is_none(), "no pool, no slot");

    assert_eq!(POOL.register(), Ok(3));
    assert!(is_valid_cpu(0));
    assert!(is_valid_cpu(2));
    assert!(!is_valid_cpu(3));
    assert!(!is_valid_cpu(u32::MAX));
    assert_eq!(
        SECONDARY_STACK_STRIDE.load(Ordering::Acquire),
        core::mem::size_of::<SecondaryStack>() as u64
    );
    for (cpu, stack) in (0..).zip(&POOL.stacks) {
        assert!(core::ptr::eq(slot(cpu).expect("a slot"), stack), "{cpu}");
    }
    assert!(slot(3).is_none());

    let base = SECONDARY_STACK_BASE.load(Ordering::Acquire).cast::<u8>();
    // SAFETY: slot 1 ends two slots into the registered three-slot pool.
    let top = unsafe { base.add(2 * core::mem::size_of::<SecondaryStack>() - 1) };
    assert_eq!(push_while_shared(slot(1).expect("a slot"), top), 0);

    // A second pool is refused rather than silently re-pointing the live
    // trampoline.
    assert_eq!(
        POOL2.register(),
        Err(SecondaryStackError::AlreadyRegistered)
    );

    reset_secondary_stacks_for_tests();
}

#[test]
fn secondary_entry_round_trips_and_is_set_once() {
    clear_secondary_entry_for_tests();
    assert_eq!(secondary_entry_addr(), 0);
    // Coerce once: the published address is compared against *this*
    // pointer value, because two coercions of one `fn` item are not
    // guaranteed to share an address.
    let entry: extern "C" fn(CpuId) -> ! = host_entry;
    set_secondary_entry(entry).expect("first install");
    assert_eq!(secondary_entry_addr(), entry as *const () as usize);
    assert_eq!(
        set_secondary_entry(entry),
        Err(SetEntryError::AlreadyInstalled)
    );
    clear_secondary_entry_for_tests();
}

#[test]
fn current_cpu_index_is_boot_core_on_host() {
    // The host build has no `MPIDR_EL1`; it reports the single boot core.
    assert_eq!(current_cpu_index(), 0);
}

#[test]
fn affinity_mask_excludes_the_reserved_mpidr_bits() {
    // The reserved bit 31, the U bit 30, and the MT bit 24 are all above
    // the Aff2 byte and must not leak into the masked affinity.
    assert_eq!((1u64 << 31) & MPIDR_AFFINITY_MASK, 0);
    assert_eq!((1u64 << 30) & MPIDR_AFFINITY_MASK, 0);
    assert_eq!((1u64 << 24) & MPIDR_AFFINITY_MASK, 0);
    // Aff0 (core index on the `virt` board) survives the mask.
    assert_eq!(0x1u64 & MPIDR_AFFINITY_MASK, 1);
}

#[test]
fn start_cpu_error_cause_strings_are_stable() {
    assert_eq!(
        StartCpuError::CpuIdOutOfRange.as_str(),
        "cpu_id_out_of_range"
    );
    assert_eq!(
        StartCpuError::NoEntryInstalled.as_str(),
        "no_secondary_entry_installed"
    );
    assert_eq!(
        StartCpuError::NoAffinityTable.as_str(),
        "no_secondary_affinity_table"
    );
    assert_eq!(StartCpuError::Psci(-4).as_str(), "psci_cpu_on_failed");
    assert_eq!(
        StartCpuError::StartTimedOut.as_str(),
        "core_start_timed_out"
    );
    assert_eq!(StartCpuError::LacksPan.as_str(), "core_lacks_pan");
}

/// A core lacking PAN answers so, and the start is refused for exactly that
/// rather than read as one that never came up.
#[test]
fn the_boot_cpu_reads_a_cores_answer_as_joined_refused_or_silent() {
    assert_eq!(outcome(Some(StartAnswer::Joined)), Ok(()));
    assert_eq!(
        outcome(Some(StartAnswer::Refused(crate::protection::PAN))),
        Err(StartCpuError::LacksPan)
    );
    let other = core::num::NonZeroU8::new(1 << 4).unwrap();
    assert_eq!(
        outcome(Some(StartAnswer::Refused(other))),
        Err(StartCpuError::StartTimedOut),
        "a refusal naming no protection of this port's is no answer it reads"
    );
    assert_eq!(outcome(None), Err(StartCpuError::StartTimedOut));
}

#[test]
fn a_start_error_reaches_the_hal_under_its_neutral_cause() {
    assert_eq!(
        SmpError::from(StartCpuError::LacksPan),
        SmpError::LacksProtection
    );
    assert_eq!(
        SmpError::from(StartCpuError::StartTimedOut),
        SmpError::StartRejected(0)
    );
    assert_eq!(
        SmpError::from(StartCpuError::Psci(-4)),
        SmpError::StartRejected(-4)
    );
    assert_eq!(
        SmpError::from(StartCpuError::CpuIdOutOfRange),
        SmpError::InvalidCpu
    );
    for err in [
        StartCpuError::NoEntryInstalled,
        StartCpuError::NoAffinityTable,
    ] {
        assert_eq!(SmpError::from(err), SmpError::NotReady);
    }
}

#[test]
fn affinity_table_publication_is_set_once_and_observable() {
    // Dense id → affinity, the spin-table trampoline's identity source.
    static AFFINITIES: [u64; 4] = [0, 1, 2, 3];
    static SECOND: [u64; 2] = [0, 1];

    reset_secondary_affinities_for_tests();
    // Fail closed before publication: a spin-table release must be
    // refused while no table is visible to the released core.
    assert!(!secondary_affinities_registered());

    assert_eq!(register_secondary_affinities(&AFFINITIES), Ok(4));
    assert!(secondary_affinities_registered());
    assert_eq!(
        SECONDARY_AFFINITY_BASE.load(Ordering::Acquire),
        AFFINITIES.as_ptr() as u64
    );
    assert_eq!(SECONDARY_AFFINITY_COUNT.load(Ordering::Acquire), 4);

    // A second table is refused rather than silently re-pointing the
    // live trampoline's identity source.
    assert_eq!(
        register_secondary_affinities(&SECOND),
        Err(SecondaryAffinityError::AlreadyRegistered)
    );
    assert_eq!(
        SECONDARY_AFFINITY_BASE.load(Ordering::Acquire),
        AFFINITIES.as_ptr() as u64
    );

    reset_secondary_affinities_for_tests();
}

#[test]
fn kernel_release_word_starts_parked() {
    // The `_start` park loop polls this word MMU-off: it must be zero
    // (keep parking) until a spin-table release publishes an entry.
    assert_eq!(SECONDARY_KERNEL_RELEASE.load(Ordering::Acquire), 0);
}

#[test]
fn kernel_release_target_starts_gated_shut() {
    // The park loop lets a woken core proceed only when this word equals
    // its own masked affinity. The all-ones initial value is above the
    // affinity mask, so it matches no real core — the gate is shut until
    // a deliberate release names exactly one core, keeping bring-up one
    // core at a time (never a concurrent MMU-adopt / GIC-init race).
    let initial = SECONDARY_KERNEL_RELEASE_TARGET.load(Ordering::Acquire);
    assert_eq!(initial, u64::MAX);
    assert!(initial > MPIDR_AFFINITY_MASK);
    // The initial target opens the gate for no core, including affinity 0
    // (the boot core, which is never a spin-table release target anyway).
    assert!(!release_gate_open(initial, 0));
}

#[test]
fn release_gate_opens_only_for_the_named_core() {
    // The single definition of the decision the `boot.s` park loop and the
    // `smp.s` spin-table trampoline both compile to `cmp`/`b.eq`. A woken
    // core proceeds only when the published target is its own affinity;
    // every other parked core re-parks, so exactly one core is ever in
    // bring-up at a time (no concurrent MMU-adopt / GIC-init race).
    let target = 2u64;
    assert!(release_gate_open(target, 2));
    for other in [0u64, 1, 3, MPIDR_AFFINITY_MASK, u64::MAX] {
        assert!(
            !release_gate_open(target, other),
            "only the released core (affinity {target}) may proceed, not {other}"
        );
    }
    // The all-ones initial target names no real masked affinity, so the
    // gate stays shut for every core until a deliberate release opens it.
    for affinity in [0u64, 1, 2, 3, MPIDR_AFFINITY_MASK] {
        assert!(!release_gate_open(u64::MAX, affinity));
    }
}
