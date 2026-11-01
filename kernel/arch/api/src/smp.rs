//! Secondary-CPU bring-up surface of the Arch HAL (`plans/WIRING.md` §2,
//! Stage W14).
//!
//! The charter mandates SMP from day one. Bringing a machine's other
//! logical CPUs online is the one architecture primitive that was still
//! ad-hoc inside each port after Stage W13: every port owned a `smp`
//! module — x86_64 INIT-SIPI-SIPI, aarch64 PSCI `CPU_ON`, riscv64 SBI
//! HSM `hart_start`, wasm32 a Web Worker spawn — but the rest of the
//! kernel could not reach those through one neutral surface. This module
//! closes the burn-down: secondary bring-up becomes the
//! object-safe [`SecondaryBringup`] HAL trait, implemented once per port
//! on the port's [`crate::SchedulerArch`] handle (the owner of the dense
//! [`crate::CpuId`] ↔ native-id topology map).
//!
//! # What the trait does *not* cover
//!
//! The *directed inter-processor interrupt* half of SMP already lives on
//! [`crate::SchedulerArch::send_ipi`]; this slice is purely about
//! **starting** a parked CPU, so it does not duplicate the IPI surface
//! (no interface creep).
//!
//! The set-once *secondary entry* a freshly-started CPU runs is
//! deliberately **not** part of this trait. On the bare-metal ports it is
//! an `extern "C" fn(CpuId) -> !` function pointer the port installs once
//! at boot; on wasm32 a secondary is a *fresh module instance* whose
//! entry is the fixed `tairix_arch_wasm32_main` export, not a runtime
//! pointer one instance can hand another. Forcing a settable-entry method
//! onto the HAL would make wasm32 fake one it could never honour
//! (no fakes), so entry installation stays the
//! genuinely port-shaped concern it is, performed once before
//! [`SecondaryBringup::start_secondary`] is first called.
//!
//! # Per-arch shape (the modularity carve-out)
//!
//! Each port implements the *same* trait its own way; these parallel
//! implementations are the deliberate shape of the HAL, never collapsed
//! behind `cfg` (carve-out):
//!
//! * **x86_64** owns a low-memory trampoline frame, a per-AP stack pool,
//!   and the boot PML4; `start_secondary` installs the trampoline, writes
//!   the per-AP boot slot, runs the SDM INIT-SIPI-SIPI handshake, and
//!   settles the start on the AP's answer, joined or refused, or withdraws
//!   it ([`settle_start`]) before returning, so the shared trampoline frame
//!   is safe to reuse for the next CPU and an AP answering late never runs.
//! * **aarch64** issues PSCI `CPU_ON` over the firmware conduit
//!   (`hvc`/`smc`) discovered from the device tree, targeting the CPU's
//!   `MPIDR_EL1` affinity, or releases it from a Devicetree spin-table; it
//!   waits, bounded, for the core's answer, which the core gives with its
//!   caches off, and gives a [`StartVerdict`] the core waits on before it
//!   runs: a core whose start was given up on stops.
//! * **riscv64** issues the SBI HSM `hart_start` firmware call targeting
//!   the CPU's hart id.
//! * **wasm32** asks its JavaScript host to spawn a Web Worker that
//!   instantiates the same module as a new logical CPU.
//!
//! # Why the host conformance vertical proves only the observable half
//!
//! Exactly as for [`crate::xtlb`] and [`crate::mmu::AddressSpace::activate`],
//! the *effect* of a bring-up (a second CPU actually running) is not
//! observable from a single-threaded host test, and the
//! INIT-SIPI-SIPI / PSCI / SBI / Web Worker machinery only exists on the
//! freestanding/wasm target. The host [`conformance`] vertical therefore
//! proves the contract that *is* observable on the host — the call is
//! object-safe and **fails closed** rather than
//! panicking for a CPU that cannot be started (out of range / the boot
//! CPU / unmapped) — while the real cross-core round-trip is exercised
//! end-to-end by the multi-core `ipi_smp_qemu_*` /
//! `cross_cpu_tlb_shootdown_qemu_*` QEMU verticals and the wasm32 browser
//! vertical.

use core::num::NonZeroU8;

use crate::CpuId;

/// Why a [`SecondaryBringup::start_secondary`] request was refused.
///
/// The neutral failure surface every port maps its native error onto, so
/// the architecture-neutral kernel handles one set of outcomes. A port's
/// richer detail (the raw PSCI / SBI / host status) is preserved in
/// [`SmpError::StartRejected`] for the audit log.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum SmpError {
    /// `cpu` did not name a startable secondary CPU: it was out of
    /// range, named the boot CPU (which is already running), or was
    /// absent from the port's topology map. The request is refused
    /// before any platform action is taken (fail
    /// closed, no out-of-bounds stack selection).
    InvalidCpu,
    /// The port's secondary entry has not been installed yet, so a
    /// freshly-started CPU would have nowhere to run. Refused at the
    /// call site so the failure is loud here, not silent on the new CPU.
    NotReady,
    /// The platform's start mechanism (INIT-SIPI-SIPI / PSCI `CPU_ON` /
    /// SBI HSM `hart_start` / Web Worker spawn) refused or failed. The
    /// payload carries the port's raw status (a PSCI / SBI return code,
    /// or `0` where the mechanism reports only success/failure).
    StartRejected(i64),
    /// The CPU started but cannot hold a protection the kernel holds every
    /// CPU to, one the boot CPU offered, so it did not join and the port
    /// stopped it.
    LacksProtection,
}

impl SmpError {
    /// Stable cause string for audit records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidCpu => "smp_invalid_cpu",
            Self::NotReady => "smp_secondary_entry_not_installed",
            Self::StartRejected(_) => "smp_start_rejected",
            Self::LacksProtection => "smp_cpu_lacks_protection",
        }
    }
}

/// What a started secondary CPU answers the CPU that started it, once it
/// knows whether it can hold the protections the kernel holds every CPU to.
///
/// Every port that can learn a refusal publishes it the same way, as one
/// [`StartAnswer::word`] the starting CPU waits on ([`await_start_answer`]),
/// so a CPU that cannot run the kernel is named for why rather than taken
/// for one that never started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartAnswer {
    /// It holds every protection in force, and goes on to its entry.
    Joined,
    /// It lacks these protections in force, as the port's own bit set, and
    /// stopped.
    Refused(NonZeroU8),
}

/// The answer half of a [`StartAnswer::word`]: joined, or refused with the
/// protections lacking in its low byte. Neither is zero, so a zero word is
/// no answer from any CPU.
const ANSWER_JOINED: u64 = 1;
const ANSWER_REFUSED: u64 = 1 << 8;

/// The high half of a start word before its CPU answers, and once the start
/// is withdrawn, and of a verdict that takes an answer: none is an answer or
/// another's verdict.
const START_AWAITING: u64 = 1 << 16;
const START_WITHDRAWN: u64 = 1 << 17;
const VERDICT_TAKEN: u64 = 1 << 18;

/// `cpu`'s dense id beside the high half `half`.
const fn tagged(half: u64, cpu: CpuId) -> u64 {
    (half << 32) | cpu as u64
}

/// The decoded `half` of `word` where it names `cpu`.
fn half_for(word: u64, cpu: CpuId) -> Option<u64> {
    (word & u64::from(u32::MAX) == u64::from(cpu)).then_some(word >> 32)
}

impl StartAnswer {
    /// The answer as `cpu` gives it: the answer in the high half and the
    /// answering CPU's dense id in the low, so an answer from any other CPU —
    /// a late one from a start given up on — is never taken for this one.
    #[must_use]
    pub const fn word(self, cpu: CpuId) -> u64 {
        let answer = match self {
            Self::Joined => ANSWER_JOINED,
            Self::Refused(lacking) => ANSWER_REFUSED | lacking.get() as u64,
        };
        tagged(answer, cpu)
    }

    /// `cpu`'s answer in `word`, or `None` for no answer, another CPU's, or
    /// a word no answer is.
    #[must_use]
    pub fn from_word(word: u64, cpu: CpuId) -> Option<Self> {
        match half_for(word, cpu)? {
            ANSWER_JOINED => Some(Self::Joined),
            answer if answer & !0xFF == ANSWER_REFUSED => {
                NonZeroU8::new(answer.to_le_bytes()[0]).map(Self::Refused)
            }
            _ => None,
        }
    }
}

/// What the CPU that started `cpu` made of its answer, for a port whose
/// started CPU cannot race the starting one on a compare-and-swap: it waits
/// for this before it runs, and runs only on [`StartVerdict::Taken`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartVerdict {
    /// The answer was a join, read in time.
    Taken,
    /// The start was given up on, or the CPU refused.
    Withdrawn,
}

impl StartVerdict {
    /// The verdict on `answer`, the one the starting CPU read or `None` for
    /// none in time: only a join is taken.
    #[must_use]
    pub fn on(answer: Option<StartAnswer>) -> Self {
        if answer == Some(StartAnswer::Joined) {
            Self::Taken
        } else {
            Self::Withdrawn
        }
    }

    /// The verdict as given `cpu`, tagged as its answer is.
    #[must_use]
    pub const fn word(self, cpu: CpuId) -> u64 {
        match self {
            Self::Taken => tagged(VERDICT_TAKEN, cpu),
            Self::Withdrawn => tagged(START_WITHDRAWN, cpu),
        }
    }

    /// `cpu`'s verdict in `word`, or `None` for none, another CPU's, or a
    /// word no verdict is.
    #[must_use]
    pub fn from_word(word: u64, cpu: CpuId) -> Option<Self> {
        match half_for(word, cpu)? {
            VERDICT_TAKEN => Some(Self::Taken),
            START_WITHDRAWN => Some(Self::Withdrawn),
            _ => None,
        }
    }
}

/// The word a start of `cpu` waits on before the CPU answers: no answer, and
/// what [`answer_start`] and [`settle_start`] each swap away.
#[must_use]
pub const fn awaiting_answer(cpu: CpuId) -> u64 {
    tagged(START_AWAITING, cpu)
}

/// The word a start is settled on, where the CPU starting another and the
/// CPU started race on one compare-and-swap.
pub trait StartWord {
    /// The word as it stands, read with acquire ordering.
    fn read(&self) -> u64;

    /// Replace `current` with `new`, acquiring and releasing, or answer what
    /// it held instead.
    ///
    /// # Errors
    ///
    /// What the word held, when it was not `current`.
    fn exchange(&self, current: u64, new: u64) -> Result<u64, u64>;
}

impl StartWord for core::sync::atomic::AtomicU64 {
    fn read(&self) -> u64 {
        self.load(core::sync::atomic::Ordering::Acquire)
    }

    fn exchange(&self, current: u64, new: u64) -> Result<u64, u64> {
        self.compare_exchange(
            current,
            new,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Acquire,
        )
    }
}

/// `cpu` answers its start, readied on `word` with [`awaiting_answer`],
/// answering whether the answer stands: not where the start was withdrawn
/// first, nor on a word readied for another CPU, where one answering late
/// must not run.
pub fn answer_start(word: &impl StartWord, cpu: CpuId, answer: StartAnswer) -> bool {
    word.exchange(awaiting_answer(cpu), answer.word(cpu))
        .is_ok()
}

/// Settle the start of `cpu`, readied on `word` with [`awaiting_answer`]:
/// its answer, once it gives one, or `None` once `out_of_time` says the wait
/// is over and the start is withdrawn — by the compare-and-swap the answer
/// races on, so the two never both win and an answer landing at the deadline
/// is taken. `out_of_time` is asked after each read that answers nothing.
pub fn settle_start(
    word: &impl StartWord,
    cpu: CpuId,
    mut out_of_time: impl FnMut() -> bool,
) -> Option<StartAnswer> {
    let answer = await_start_answer(|| word.read(), cpu, &mut out_of_time);
    if answer.is_some() {
        return answer;
    }
    match word.exchange(awaiting_answer(cpu), tagged(START_WITHDRAWN, cpu)) {
        Ok(_) => None,
        Err(raced) => StartAnswer::from_word(raced, cpu),
    }
}

/// `cpu`'s answer, re-`load`ed until it is one or `out_of_time` says the
/// wait is over: the bounded spin of a hardware handshake, `None` once it
/// is. `out_of_time` is asked after each load that answers nothing.
pub fn await_start_answer(
    mut load: impl FnMut() -> u64,
    cpu: CpuId,
    mut out_of_time: impl FnMut() -> bool,
) -> Option<StartAnswer> {
    loop {
        if let Some(answer) = StartAnswer::from_word(load(), cpu) {
            return Some(answer);
        }
        if out_of_time() {
            return None;
        }
        core::hint::spin_loop();
    }
}

/// `cpu`'s verdict, re-`load`ed until there is one, with `between` run after
/// each load that finds none.
///
/// The wait has no deadline of its own: the starting CPU gives a verdict on
/// every start it makes once its own bounded wait for the answer is over, so
/// this wait ends with that one, timed by the starting CPU's clock alone.
pub fn await_start_verdict(
    mut load: impl FnMut() -> u64,
    cpu: CpuId,
    mut between: impl FnMut(),
) -> StartVerdict {
    loop {
        if let Some(verdict) = StartVerdict::from_word(load(), cpu) {
            return verdict;
        }
        between();
    }
}

/// Start a parked secondary logical CPU.
///
/// The kernel calls [`Self::start_secondary`] once per detected secondary
/// CPU during bring-up. The trait is implemented on the port's
/// [`crate::SchedulerArch`] handle — the owner of the dense
/// [`CpuId`] ↔ native-id (APIC id / `MPIDR_EL1` / hart id / worker
/// index) topology map and the platform start path — and is object-safe
/// so the architecture-neutral kernel can hold it behind a
/// `&dyn SecondaryBringup`.
///
/// # Required semantics
///
/// * A `cpu` the port cannot start — out of range, the boot CPU, or
///   unmapped — must return [`SmpError::InvalidCpu`] **before** touching
///   any platform state (fail closed).
/// * If no secondary entry has been installed, the call must return
///   [`SmpError::NotReady`] rather than starting a CPU that would park.
/// * On success the call must have issued the platform start request and
///   completed any port-internal handshake required before the *next*
///   secondary can be started safely (on x86_64 that means waiting for
///   the AP's answer, because the trampoline frame is reused).
/// * A port that learns at start that the CPU cannot hold the protections
///   in force returns [`SmpError::LacksProtection`], having stopped it.
/// * Implementations must never panic for any `cpu`.
pub trait SecondaryBringup {
    /// Start the secondary logical CPU `cpu`.
    ///
    /// # Errors
    ///
    /// See [`SmpError`]. The call fails closed rather than assuming the
    /// CPU came up.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that the port's secondary entry is
    /// installed, that early boot has zeroed the secondary-stack pool,
    /// and that `cpu` names a real, parked logical CPU distinct from the
    /// caller. The implementation still validates `cpu` against its
    /// topology and refuses an unstartable id ([`SmpError::InvalidCpu`]);
    /// the `unsafe` marker reflects the genuine bring-up preconditions a
    /// safe wrapper cannot check (a real, parked, distinct CPU).
    unsafe fn start_secondary(&self, cpu: CpuId) -> Result<(), SmpError>;
}

/// The secondary-bring-up conformance vertical.
///
/// Like [`crate::xtlb::conformance`] it names only the trait and runs on
/// the host: there is no privileged INIT-SIPI-SIPI / PSCI / SBI / Web
/// Worker machinery on the host build, and a bring-up's cross-core
/// *effect* is not observable from a single-threaded test. It proves the
/// observable half of the contract — the call is object-safe and **fails
/// closed** for a CPU that cannot be started, never panicking. The real
/// multi-core round-trip is proven by the `ipi_smp_qemu_*` /
/// `cross_cpu_tlb_shootdown_qemu_*` verticals and the wasm32 browser
/// vertical.
pub mod conformance {
    use super::{SecondaryBringup, SmpError};
    use crate::CpuId;

    /// Run the [`SecondaryBringup`] conformance suite against `bringup`.
    ///
    /// `unstartable` must be a [`CpuId`] the port cannot start (the
    /// canonical choice is [`CpuId::MAX`], which no real topology maps).
    /// The suite asserts that starting it fails closed with
    /// [`SmpError::InvalidCpu`] and that the call is total (no panic),
    /// both directly and behind the object-safe erasure the kernel holds
    /// the handle through.
    ///
    /// # Panics
    ///
    /// Panics (failing the conformance test) if the port does not refuse
    /// `unstartable` with [`SmpError::InvalidCpu`].
    pub fn run_all<T: SecondaryBringup + ?Sized>(bringup: &T, unstartable: CpuId) {
        // SAFETY: the conformance vertical only ever starts an
        // `unstartable` CPU id, which the contract requires the port to
        // refuse before taking any platform action — so the bring-up
        // preconditions of `start_secondary` are vacuously satisfied.
        let refused = unsafe { bringup.start_secondary(unstartable) };
        assert_eq!(
            refused,
            Err(SmpError::InvalidCpu),
            "start_secondary must fail closed for an unstartable CPU id",
        );
    }

    #[cfg(test)]
    mod tests {
        use super::super::{SecondaryBringup, SmpError};
        use super::run_all;
        use crate::CpuId;
        use core::sync::atomic::{AtomicUsize, Ordering};

        /// A faithful host double: it refuses any CPU at or above a small
        /// bound (mimicking a port topology) and records how many starts
        /// it accepted so the suite has something observable to assert.
        /// The counter is interior-mutable because [`SecondaryBringup`]
        /// takes `&self` — the real handle is shared (`&dyn`) between
        /// CPUs exactly like [`crate::SchedulerArch`].
        #[derive(Default)]
        struct CountingBringup {
            started: AtomicUsize,
        }

        const BOUND: CpuId = 4;

        impl SecondaryBringup for CountingBringup {
            unsafe fn start_secondary(&self, cpu: CpuId) -> Result<(), SmpError> {
                if cpu == 0 || cpu >= BOUND {
                    return Err(SmpError::InvalidCpu);
                }
                self.started.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }

        #[test]
        fn suite_requires_fail_closed_on_an_unstartable_id() {
            let bringup = CountingBringup::default();
            run_all(&bringup, CpuId::MAX);
            assert_eq!(
                bringup.started.load(Ordering::Relaxed),
                0,
                "the suite must not have started any real CPU",
            );

            // And over the object-safe erasure the kernel holds it behind.
            let dynamic = CountingBringup::default();
            let erased: &dyn SecondaryBringup = &dynamic;
            run_all(erased, CpuId::MAX);
            assert_eq!(dynamic.started.load(Ordering::Relaxed), 0);
        }

        #[test]
        fn faithful_double_accepts_a_real_secondary() {
            // Positive coverage the generic suite cannot give (it does
            // not know a startable id): a valid secondary is accepted.
            let bringup = CountingBringup::default();
            // SAFETY: the double takes no platform action; CPU 1 is a
            // valid secondary in its synthetic topology.
            let ok = unsafe { bringup.start_secondary(1) };
            assert_eq!(ok, Ok(()));
            assert_eq!(bringup.started.load(Ordering::Relaxed), 1);
            // The boot CPU (0) is refused, never started.
            // SAFETY: as above; CPU 0 is refused before any action.
            assert_eq!(
                unsafe { bringup.start_secondary(0) },
                Err(SmpError::InvalidCpu)
            );
        }

        /// Each refusal reaches the audit log under a cause of its own, so a
        /// CPU stopped for a protection it lacks is never read as one whose
        /// start the platform refused.
        #[test]
        fn smp_error_cause_strings_are_stable_and_distinct() {
            let causes = [
                (SmpError::InvalidCpu, "smp_invalid_cpu"),
                (SmpError::NotReady, "smp_secondary_entry_not_installed"),
                (SmpError::StartRejected(-4), "smp_start_rejected"),
                (SmpError::LacksProtection, "smp_cpu_lacks_protection"),
            ];
            for (at, (err, cause)) in causes.iter().enumerate() {
                assert_eq!(err.as_str(), *cause);
                assert!(causes[..at].iter().all(|(_, earlier)| earlier != cause));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        answer_start, await_start_answer, await_start_verdict, awaiting_answer, settle_start,
        StartAnswer, StartVerdict, StartWord,
    };
    use core::cell::Cell;
    use core::num::NonZeroU8;
    use core::sync::atomic::AtomicU64;

    fn refused(bits: u8) -> StartAnswer {
        StartAnswer::Refused(NonZeroU8::new(bits).unwrap())
    }

    #[test]
    fn a_start_answer_survives_its_word_and_no_other_word_is_one() {
        assert_eq!(StartAnswer::Joined.word(3), (1 << 32) | 3);
        assert_eq!(refused(0x05).word(7), (0x105 << 32) | 7);
        let answers = core::iter::once(StartAnswer::Joined).chain((1..=u8::MAX).map(refused));
        for answer in answers {
            assert_eq!(StartAnswer::from_word(answer.word(3), 3), Some(answer));
            assert_eq!(
                StartAnswer::from_word(answer.word(3), 4),
                None,
                "another CPU's answer is no answer from this one"
            );
        }
        let at = |answer: u64| (answer << 32) | 3;
        for word in [0, 3, at(0x100), at(2), at(0x200), at(0x1_0001), u64::MAX] {
            assert_eq!(StartAnswer::from_word(word, 3), None, "{word:#x}");
        }
    }

    /// A wait over `words`, each load answering the next and the last for
    /// ever after, out of time after `budget` loads that answer nothing:
    /// what it answers, and how many loads it took.
    fn wait(words: &[u64], budget: usize) -> (Option<StartAnswer>, usize) {
        let taken = Cell::new(0);
        let load = || {
            let at = taken.get();
            taken.set(at + 1);
            words[at.min(words.len() - 1)]
        };
        let answer = await_start_answer(load, 1, || taken.get() > budget);
        (answer, taken.get())
    }

    #[test]
    fn the_starting_cpu_takes_the_first_answer_and_waits_past_any_other_word() {
        let lacking = refused(0x02);
        assert_eq!(wait(&[0, 0, lacking.word(1)], 10), (Some(lacking), 3));
        assert_eq!(wait(&[(2 << 32) | 1], 5), (None, 6));
        assert_eq!(
            wait(&[StartAnswer::Joined.word(2)], 5),
            (None, 6),
            "a late answer from another CPU is waited past"
        );
        assert_eq!(wait(&[0], 0), (None, 1));
        assert_eq!(
            await_start_answer(|| lacking.word(1), 1, || true),
            Some(lacking),
            "an answer already there is taken, however late the wait began"
        );
    }

    /// An answer before the deadline stands and is what the start settles
    /// on; a start settled first is withdrawn, and the answer that comes
    /// after it does not stand.
    #[test]
    fn a_start_and_its_answer_never_both_win() {
        let word = AtomicU64::new(awaiting_answer(2));
        assert!(answer_start(&word, 2, StartAnswer::Joined));
        assert_eq!(settle_start(&word, 2, || true), Some(StartAnswer::Joined));

        let word = AtomicU64::new(awaiting_answer(2));
        assert_eq!(settle_start(&word, 2, || true), None, "withdrawn");
        assert!(
            !answer_start(&word, 2, StartAnswer::Joined),
            "too late to run"
        );
        assert!(!answer_start(&word, 2, refused(0x01)));
    }

    /// An answer landing as the wait runs out is taken: the withdrawal's
    /// compare-and-swap finds it there.
    #[test]
    fn an_answer_landing_at_the_deadline_is_taken() {
        let word = AtomicU64::new(awaiting_answer(3));
        let settled = settle_start(&word, 3, || answer_start(&word, 3, StartAnswer::Joined));
        assert_eq!(settled, Some(StartAnswer::Joined));
    }

    /// A CPU answering a start given up on while another's is under way
    /// changes nothing: its answer does not stand, and the other's start
    /// still waits on its own.
    #[test]
    fn a_late_answer_never_lands_on_another_cpus_start() {
        let word = AtomicU64::new(awaiting_answer(5));
        assert!(!answer_start(&word, 4, StartAnswer::Joined));
        assert_eq!(word.read(), awaiting_answer(5));
        assert!(answer_start(&word, 5, StartAnswer::Joined));
    }

    /// Only a join read in time is taken, and a verdict is for its own CPU.
    #[test]
    fn a_verdict_takes_only_a_join_and_names_its_cpu() {
        assert_eq!(
            StartVerdict::on(Some(StartAnswer::Joined)),
            StartVerdict::Taken
        );
        assert_eq!(
            StartVerdict::on(Some(refused(0x02))),
            StartVerdict::Withdrawn
        );
        assert_eq!(StartVerdict::on(None), StartVerdict::Withdrawn);
        for verdict in [StartVerdict::Taken, StartVerdict::Withdrawn] {
            assert_eq!(StartVerdict::from_word(verdict.word(6), 6), Some(verdict));
            assert_eq!(StartVerdict::from_word(verdict.word(6), 7), None);
            assert_eq!(StartAnswer::from_word(verdict.word(6), 6), None);
        }
        for word in [0, awaiting_answer(6), StartAnswer::Joined.word(6)] {
            assert_eq!(StartVerdict::from_word(word, 6), None, "{word:#x}");
        }
    }

    /// A started CPU waits as long as it takes for its own verdict, past no
    /// verdict and past another CPU's, and runs `between` after each miss.
    #[test]
    fn a_started_cpu_waits_for_its_own_verdict_however_long_it_takes() {
        let loads = Cell::new(0u32);
        let between = Cell::new(0u32);
        let load = || {
            loads.set(loads.get() + 1);
            match loads.get() {
                ..=10_000 => 0,
                10_001 => StartVerdict::Taken.word(2),
                _ => StartVerdict::Withdrawn.word(1),
            }
        };
        let verdict = await_start_verdict(load, 1, || between.set(between.get() + 1));
        assert_eq!(verdict, StartVerdict::Withdrawn);
        assert_eq!((loads.get(), between.get()), (10_002, 10_001));
        assert_eq!(
            await_start_verdict(|| StartVerdict::Taken.word(1), 1, || unreachable!()),
            StartVerdict::Taken,
            "a verdict already there is taken without waiting"
        );
    }
}
