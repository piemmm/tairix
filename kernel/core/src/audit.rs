//! Stable audit-log event IDs for `kernel/core`.
//!
//! The identifiers are part of the audit contract with external log
//! consumers and may not be re-used or re-numbered; they sit in the range
//! [`tairix_log::registry::KERNEL`] gives the kernel. Each variant's rustdoc
//! says what its record carries, and `docs/src/architecture/kernel.md`
//! catalogues every event's level and sink, held to [`AuditEvent`] by a test.
//!
//! "audit" events route through the `audit_sink` channel (security-relevant
//! decisions), "log" events through the diagnostic `log_sink` channel.

use tairix_arch_api::fatal;
use tairix_log::{log, Event, EventId, Field, Level, Sink};
use tairix_sync::OnceCell;

/// Audit event identifiers emitted by `kernel/core`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum AuditEvent {
    /// `kernel_main` entered, before any subsystem init.
    BootStarted,
    /// An init phase began; `phase` names it.
    PhaseStarted,
    /// An init phase completed successfully; `phase` names it.
    PhaseReady,
    /// An init phase failed, `phase` naming it and `cause` why; the kernel
    /// halts next.
    PhaseFailed,
    /// Every init phase finished; control passes to the scheduler.
    BootCompleted,
    /// The early-boot RAM self-test finished, with the bytes it actually
    /// verified and the usable bytes the firmware map described.
    ///
    /// The test can only reach RAM the port's direct physical map covers, so
    /// a `verified` short of `usable` means the kernel cannot address some of
    /// its own RAM by pointer — it is recorded rather than left as a counter
    /// that silently understates what was proven, and it emits at `Warn`.
    /// The fields are byte totals; neither is a secret.
    RamSelfTest,
    /// The boot path decided whether the kernel heap grows past its bootstrap
    /// arena: `growth` is `installed`, `none` (the port has no direct physical
    /// map, by design) or `failed` (it has one, and emits at `Warn`), beside
    /// the arena's `bootstrap_bytes` and the most of it boot used,
    /// `peak_bytes`.
    KernelHeapGrowth,
    /// The kernel panicked; the handler logged context and is halting.
    ///
    /// `file`, `line` and `column` name the source position. A fatal entry
    /// taken while a report is already under way writes a bare record instead
    /// ([`tairix_arch_api::fatal::nested_fields`]), naming its own cause and,
    /// under `first`, the one the first report was entered for; so do
    /// [`Self::KernelFault`] and [`Self::KernelStackCorrupt`].
    Panic,
    /// A fatal CPU exception was taken in kernel mode — a same-privilege
    /// abort, page fault, or illegal instruction the kernel has no fix-up
    /// for.
    ///
    /// Distinct from [`Self::Panic`] because the two carry different
    /// evidence and call for different triage: a panic names a source
    /// location, a fault names the hardware syndrome, the faulting address,
    /// and the faulting instruction. Neither is fabricated for the other.
    /// `syndrome`, `fault_addr`, `fault_pc` and `fault_sp` carry them and the
    /// kernel stack the fault ran on, each `null` where the CPU gave none; the
    /// register and `frame_N` blocks are the panic dump's.
    KernelFault,
    /// A task's kernel stack was found corrupt — its saved stack pointer off
    /// the stack before the switch in, or its guard canary disturbed once it
    /// switched back — so the kernel halts rather than run on memory that may
    /// be damaged.
    ///
    /// `check` is `suspension` or `guard`; `task`, `task_sp`, `stack_base` and
    /// `stack_top` name the task, where it was suspended, and the stack's
    /// bounds; the register and `frame_N` blocks are the dispatcher's.
    KernelStackCorrupt,
    /// A syscall fired on a CPU with no identifiable caller.
    ///
    /// Emitted by `KernelDispatchHook` (Stage 2.7 follow-up (f4))
    /// when either `Scheduler::current_task` returns `None` for the
    /// issuing CPU or the per-task capability registry has no record
    /// for the running task. Both conditions are "should be impossible
    /// once the scheduler is live", but the charter mandates
    /// fail-closed behaviour anyway: the audit record names the
    /// failing case (`cause` field) and the bin-crate dispatch
    /// callback halts the CPU exactly as the pre-(f5)
    /// `fail_closed_dispatch` did.
    SyscallNoCallerContext,
    /// A process was spawned: its image was built and the calling CPU is
    /// about to enter it in user mode.
    ///
    /// Emitted by the capability-checked spawn caller
    /// ([`crate::spawn::spawn_and_enter`]) after the user address space
    /// has been materialised and immediately before the Arch HAL
    /// `enter_user` transition (which never returns). `entry` carries the
    /// relocated entry-point virtual address.
    ProcessSpawned,
    /// A spawn was refused; `cause` names the refusal.
    ///
    /// `cause=session_ending` is a child refused at admission because its
    /// session began ending while it was built: it never runs, its parent
    /// reaps it as killed, and `task`, `proc` and `comm` name it.
    /// `cause=deferred_load_failed` is a child whose own load refused its
    /// bundle: `task` names it, `status` the reserved load-failure status it
    /// exits with, and `errno` the refusal that status classifies. Every other
    /// refusal precedes the child. The boot path's
    /// [`crate::spawn::spawn_and_enter`] records its capability refusal with
    /// no fields.
    ProcessSpawnDenied,
    /// A spawn was authorised but building the process image failed.
    ///
    /// Emitted by [`crate::spawn::spawn_and_enter`] when
    /// [`tairix_kernel_mem::build_process_image`] returns an error (a
    /// malformed image, an out-of-range segment, or frame exhaustion).
    /// The partially built address space is discarded by the caller
    /// (fail closed). `cause` names the refusal, beside `free_frames` where
    /// the image ran out of frames.
    ProcessSpawnFailed,
    /// A loaded driver process was torn down: its scheduler task was
    /// reaped and its grants, served endpoints, IRQ bindings, capability
    /// record, and address space reclaimed.
    ///
    /// Emitted by [`crate::spawn::InitSpawnCtx::terminate_driver_process`]
    /// when the device manager unloads a driver whose hardware-tree node
    /// vanished. The record carries the torn-down driver `handle`. Tearing
    /// an already-gone handle down is a benign
    /// [`tairix_abi::Errno::NotFound`] and emits no record (idempotent).
    DriverUnloaded,
    /// A task was killed by an unresolvable user-mode fault: the fault
    /// resolver could not back the access, so the task's crash exit
    /// (`128 + SIGSEGV`) was recorded and its resources reclaimed — the
    /// task dies, never the machine.
    ///
    /// Emitted by the fault-kill path
    /// (`KernelSyscallHandlers::record_fault_exit`, a crate-private
    /// method) so a crashing program is visible on the system log, not
    /// only via its `wait` status. The record carries a debuggable
    /// post-mortem while never leaking address-space layout:
    ///
    /// * `task` — the reusable scheduler id.
    /// * `name` / `proc_id` — the faulting task's kernel-attested
    ///   executable basename and CSPRNG-minted process-instance identity
    ///   (never caller-supplied), so a crash names *which* program and
    ///   stays correlatable after the task id is recycled.
    /// * `write` — whether the fatal access was a store (`true`) or a load
    ///   (`false`).
    /// * `fault_class` — a coarse class of *why* the resolver refused the
    ///   access (`stack_limit` — stack growth refused because the task's
    ///   `StackBytes` soft bound is exhausted; `stack` — growth room the
    ///   resolver could not back, e.g. frame exhaustion; `file_region` — a
    ///   miss inside a live file mapping the resolver refused, e.g. past
    ///   end-of-file; `anon` — a miss inside a reserved anonymous region;
    ///   `wild` — an address outside every mapping).
    /// * `fault_offset` — a coarse, non-leaking locality bucket
    ///   (`null_page`, `below_stack_guard`, `region`, or `wild`); when it
    ///   carries a distance, `region_offset` holds that value — a
    ///   *distance* from a fixed anchor (virtual address 0, the stack
    ///   guard, a region end), **never** an absolute virtual address.
    ///
    /// The raw faulting address is deliberately **never** on the record
    /// (diagnostics policy: no address-space layout leakage onto the
    /// shared, hash-chained log).
    TaskFaultKilled,
    /// A task ended itself with a **nonzero** exit status.
    ///
    /// Emitted by the `exit` syscall handler so an abnormal termination is
    /// visible on the system log even when no parent reaps the task — a
    /// failing autoloaded service would otherwise vanish silently (fail
    /// loud). The record carries the task id and the exit code; both are
    /// program state, never secrets. A clean (`0`) exit stays quiet.
    TaskExitedNonzero,
    /// The `signal` syscall's cross-principal authority decision.
    ///
    /// `signal` first tries the caller's own live children
    /// (`crate::procsignal::ProcessSignal::resolve_child`), which needs no
    /// capability. Only once that lookup reports the target is not a child
    /// does the handler decide whether the target belongs to the caller's
    /// own principal (allowed) or requires `CAP_PROC_CONTROL` (allowed if
    /// held, refused otherwise). Emitted exactly once per such decision,
    /// `Info` on either allow and `Warn` on a refusal, carrying the
    /// caller's task id (`caller`), the target's pid and task id (`pid`,
    /// `target`), the requested `signal`, and the `rule` that decided it —
    /// never a capability token.
    ProcessSignalCrossPrincipal,
    /// A `sched_set_priority` authority decision that reached beyond the
    /// caller's own child: a cross-principal target or a raise toward
    /// `High` (`plans/NEW-TASKBAR.md` T12).
    ///
    /// Emitted once per such decision, allowed (`Info`) or denied
    /// (`Warn`). The record carries the kernel-attested caller's task id
    /// (`caller`), the target's pid and task id (`pid`, `target`), the
    /// requested level's wire discriminant (`priority`), the `rule` that
    /// decided the target, and whether the change was a `raise` — never a
    /// capability token. An own-child
    /// lowering is the parent's standing grant and stays unrecorded,
    /// exactly as own-child signal delivery does.
    ProcessPriorityChange,
    /// A process was killed because the session it belongs to ended: the
    /// process that session is anchored at died
    /// (`docs/src/architecture/sessions.md`).
    ///
    /// Emitted once per process the end reached, carrying its task id
    /// (`task`), its instance (`proc`), its name (`comm`), and the instance
    /// the session is anchored at (`session`) — the record that says why a
    /// process nobody signalled is gone.
    SessionMemberEnded,
    /// The session reaper could not be started, so each session is ended on
    /// the path its anchor's death lands on (`crate::session_reaper`).
    SessionReaperUnavailable,
    /// The `/System/Security/Users` database was read off the mounted
    /// root volume and parsed (`crate::users`, `plans/PI.md` P11); `path`
    /// names it and `records` counts its accounts.
    UsersDbLoaded,
    /// The `/System/Security/Users` database could not be read, or
    /// failed its bounded fail-closed validation; no database is held
    /// and every login refuses. `path` names what was refused and `cause`
    /// why.
    UsersDbRejected,
    /// The `/System/Security/Groups` group registry was read off the
    /// mounted root volume and parsed (`crate::groups`).
    GroupsDbLoaded,
    /// The `/System/Security/Groups` registry could not be read, failed its
    /// bounded fail-closed validation, or did not resolve every group a
    /// user references; no identity table is installed and the filesystem
    /// path resolves no caller groups (fail closed).
    GroupsDbRejected,
    /// The `/System/Drivers/` signed-driver store was enumerated for
    /// autoload candidates (`crate::driver_store`).
    ///
    /// Emitted by [`crate::driver_store::enumerate_driver_store`] once
    /// per scan with the count of bundle image paths found (`drivers`)
    /// and the count of entries refused fail-closed during the bounded
    /// walk (`skipped`). A missing store is not an error — it simply
    /// yields zero drivers.
    DriverStoreScanned,
    /// A `CAP_USER_ADMIN` account-administration operation was applied:
    /// validated, persisted to the root volume, and made live for the
    /// next spawn/login (`crate::useradmin`, `plans/CAPABILITY_USE.md`
    /// CU4). Carries the operation (`op`), the affected account or group
    /// (`target`), and the attested `caller_uid` — never any password
    /// material.
    UserAdminApplied,
    /// A `CAP_USER_ADMIN` account-administration operation was refused
    /// and nothing changed (fail closed). Carries
    /// [`Self::UserAdminApplied`]'s fields and the refusing `errno`.
    UserAdminRejected,
    /// An input driver delivered the **first** record of its kind to the
    /// seat registry (`crate::seat`, `plans/PI.md` P11 —
    /// the autoload-by-discovery witness).
    ///
    /// Emitted by the `key_inject` / `pointer_inject` / `touch_inject`
    /// syscall handlers the first time [`crate::seat::SeatRegistry::inject`]
    /// / [`crate::seat::SeatRegistry::inject_pointer`] /
    /// [`crate::seat::SeatRegistry::inject_touch`] succeeds for that input
    /// kind, gated by a per-kind one-shot latch
    /// ([`crate::seat::SeatRegistry::note_first_delivery`]), so it fires at
    /// most once per kind with a `kind` field (`key` / `pointer` / `touch`)
    /// attributing which input class proved itself live. It carries **no** event content, count, or
    /// timing — a per-event record would leak typed secrets and is
    /// forbidden: no input content or timing reaches the log.
    InputDelivered,
    /// A `CAP_SEAT_ADMIN` `seat_switch` retargeted a seat's foreground
    /// text console (`plans/DISPLAY.md` D3).
    ///
    /// Emitted by the `seat_switch` syscall handler after the validated
    /// retarget takes effect. Carries the `seat` id and the new foreground
    /// `console` index — the redirect of every subsequent keystroke of an
    /// unowned seat is a security-relevant ownership change.
    SeatSwitched,
    /// A `CAP_SEAT_ADMIN` `seat_revoke` forcibly evicted a seat's lease
    /// holder (`plans/DISPLAY.md` D3).
    ///
    /// Emitted by the `seat_revoke` syscall handler after the eviction.
    /// Carries the `seat` id and the `evicted` owner's task id, so every
    /// eviction is attributable; the evicted owner's next owner-gated call
    /// fails closed with the distinct `SeatRevoked` refusal.
    SeatLeaseRevoked,
    /// A seat's lease was reclaimed because its holder is gone
    /// (`plans/DISPLAY.md` D8).
    ///
    /// Emitted per reclaimed seat by the task-exit reclaim
    /// ([`crate::seat::SeatRegistry::release_owned_by`]) for a graphical
    /// session that ended without releasing — a clean exit that never
    /// reached its `display_release`, a fault kill, a signal kill, a
    /// force-quit. Carries the seat id and the dead owner's task id: the
    /// display surface and the keyboard returning to the text console is an
    /// ownership change, and it is attributable.
    SeatLeaseReclaimed,
    /// A display-class hardware-tree node was published into the live tree
    /// and the kernel minted an independent seat for it
    /// (`plans/DISPLAY.md` D6 — multi-seat / hotplug).
    ///
    /// Emitted by the `hw_emit_node` syscall handler after the validated
    /// publish. Carries the new seat id and the display node id — a new
    /// input-routing destination coming into existence is a
    /// security-relevant topology change.
    SeatCreated,
    /// A display-class hardware-tree node left the live tree and the
    /// kernel destroyed its seat (`plans/DISPLAY.md` D6).
    ///
    /// Emitted by the `hw_remove_node` syscall handler after the validated
    /// removal. Carries the destroyed seat id and the vanished node id;
    /// every lease and handle on the dead seat fails closed from this
    /// point on.
    SeatDestroyed,
    /// The kernel CSPRNG output reserve was seeded from the platform's
    /// entropy sources.
    ///
    /// Emitted once at boot by [`crate::init`] when the mix of the hardware
    /// RNG, timing jitter, interrupt timing, and firmware boot seed produced
    /// enough bytes to seed the reserve; its `sources` field names the ones
    /// able to contribute. After this, `random_get` serves cryptographic
    /// output. The record carries no entropy — only that the decision was
    /// taken (a security-relevant state change).
    EntropyReserveSeeded,
    /// The kernel CSPRNG output reserve could **not** be seeded at boot
    /// and stays unseeded (`random_get` keeps failing closed with
    /// `EntropyNotReady`).
    ///
    /// Emitted once at boot by [`crate::init`] when no source could
    /// contribute (`cause` = `no_source`) or every bounded draw was
    /// exhausted (`draw_failed`). The kernel never weakens to predictable
    /// output; it fails closed.
    EntropyReserveUnseeded,
    /// The kernel minted the per-boot identifier ([`tairix_abi::BootId`])
    /// from the seeded CSPRNG output reserve.
    ///
    /// Emitted once at boot by [`crate::init`] after the reserve is seeded.
    /// The record carries **neither** the entropy nor the id itself — only
    /// that the per-boot identity became available (a security-relevant state
    /// change; the id is a public nonce read through `boot_id_get`).
    BootIdMinted,
    /// The kernel could **not** mint the per-boot identifier: the CSPRNG
    /// output reserve was not seeded in time, so the boot id stays
    /// [`tairix_abi::BootId::UNSET`] and `boot_id_get` fails closed with
    /// `EntropyNotReady`.
    ///
    /// Emitted once at boot by [`crate::init`] on a port whose entropy source
    /// could not seed the reserve. The kernel never substitutes a predictable
    /// id; it fails closed.
    BootIdUnavailable,
    /// The kernel published the per-boot hash key from the seeded CSPRNG
    /// output reserve, so every in-kernel hash over an attacker-influenced
    /// key (a futex address, a name off a foreign volume) is keyed against
    /// collision flooding.
    ///
    /// Emitted once at boot by [`crate::init`] after the reserve is seeded.
    /// The record carries the state change only, never the key.
    HashKeyPublished,
    /// The kernel could **not** publish the per-boot hash key: the CSPRNG
    /// output reserve was not seeded in time.
    ///
    /// Emitted once at boot on a port whose entropy source could not seed the
    /// reserve. In-kernel hashes then run unkeyed, so a caller that can choose
    /// keys can choose colliding ones; the record is what makes that state
    /// visible rather than silent.
    HashKeyUnavailable,
    /// A secondary CPU was asked to start (the arch port accepted the
    /// bring-up request for it).
    ///
    /// Emitted by `kernel_main`'s SMP bring-up once per secondary after
    /// the arch port's `SecondaryBringup::start_secondary` returns `Ok`.
    /// Acceptance is not liveness: the started core attests its own
    /// arrival with [`Self::SecondaryCpuOnline`]. The record carries the
    /// dense `cpu` id.
    SecondaryCpuStarted,
    /// A secondary CPU could **not** be started; the system continues on
    /// the cores that are online (the scheduler's work stealing drains
    /// the missing core's queue), degraded but correct.
    ///
    /// Emitted by `kernel_main`'s SMP bring-up with the dense `cpu` id
    /// and a `cause` field naming the arch port's refusal. A refused
    /// core is never retried blindly (no retry-until-it-works); the
    /// failure is loud and attributable here.
    SecondaryCpuStartFailed,
    /// A started secondary CPU reached the kernel dispatch loop and is
    /// scheduling tasks — the liveness witness for the bring-up.
    ///
    /// Emitted once by the secondary CPU itself from
    /// [`crate::smp::run_secondary`], after its per-CPU hardware init and
    /// before its first dispatch, with its dense `cpu` id.
    SecondaryCpuOnline,
    /// A CPU stopped making scheduler progress for longer than the stall
    /// threshold — a soft lockup (`crate::watchdog`).
    ///
    /// Emitted from the port's timer-tick path the first time
    /// [`crate::watchdog::check_stall`] observes a CPU whose last dispatch
    /// heartbeat is older than
    /// [`crate::watchdog::DEFAULT_SOFT_LOCKUP_THRESHOLD_NS`], reported once
    /// per stall episode (never once per tick). The `cpu`
    /// field names the stalled CPU and `stalled_ms` how long it has gone
    /// without progress. Both are diagnostic state, never secrets.
    CpuStallDetected,
    /// A CPU that was reported stalled resumed making scheduler progress
    /// (`crate::watchdog`).
    ///
    /// Emitted from the dispatch loop by [`crate::watchdog::note_progress`]
    /// the first time a previously-reported stalled CPU dispatches again,
    /// so a cleared stall closes its own record rather than leaving a
    /// dangling "stuck" line. The `cpu` field names the recovered CPU and
    /// `stalled_ms` how long the episode lasted.
    CpuStallCleared,
    /// A CPU stopped taking even the non-maskable watchdog sample while it
    /// was running work — a **hard** lockup (`crate::watchdog`): wedged
    /// with interrupts masked, an interrupt storm, or a dead core.
    ///
    /// Emitted by another CPU's cross-CPU watchdog scan, once per episode.
    /// Carries the full diagnosis a post-mortem needs: the locked `cpu`,
    /// the `observer` CPU that caught it, how long it has been silent
    /// (`stalled_ms`), the last-known interrupted program counter (`pc`)
    /// and processor state (`pstate`), and the `task` that was running —
    /// the "what" and "why", never secrets.
    CpuHardLockupDetected,
    /// A CPU that was reported hard-locked resumed taking its non-maskable
    /// watchdog sample (`crate::watchdog`) — it recovered.
    ///
    /// Emitted by the recovered CPU's own watchdog sample the first time it
    /// runs again after a reported hard lockup, so the episode closes its
    /// own record. Carries `cpu` and the `stalled_ms` the episode lasted.
    CpuHardLockupCleared,
    /// The watchdog asked the architecture port to break a locked-up CPU
    /// out of its lockup (`crate::watchdog`), best-effort.
    ///
    /// Emitted alongside a detection, carrying the target `cpu`, the
    /// lockup `kind` (`soft`/`hard`), and the `outcome` the port reported
    /// (`rescheduled`/`attention`/`unrecoverable`/`unsupported`), so the
    /// recovery attempt and its result are on the audit trail.
    CpuLockupRecovery,
    /// The address-bearing developer detail of a lockup report — the
    /// sampled program counter (`pc`) and processor state (`pstate`), the
    /// self-published kernel-activity breadcrumb (`k_site`/`k_detail`/
    /// `k_seq`), and the pre-silence call-stack backtrace (`k_bt`).
    ///
    /// Emitted **only** by a `watchdog-diagnostics` (non-shippable `debug`
    /// image) build, alongside the always-on lockup summary
    /// ([`CpuHardLockupDetected`](Self::CpuHardLockupDetected) /
    /// [`CpuStallDetected`](Self::CpuStallDetected)) but through the
    /// **diagnostic** (log/UART) sink, never the persistent hash-chained
    /// audit trail — so the audit log carries no kernel address. Every
    /// kernel-address field is rendered image-base-relative (`pc=+0x…`),
    /// never the absolute runtime address, so it resolves against the
    /// debug kernel ELF without disclosing the runtime load base. A
    /// shippable image never emits it and compiles the whole facility out;
    /// the id is reserved so the catalogue stays stable
    /// (`plans/WATCHDOG.md`).
    CpuLockupDiagnostic,
    /// The boot probe's verdict on whether the lockup watchdog's
    /// **non-maskable** self-sample is live on this hardware — the sample
    /// that lets a lone CPU-bound task be observed, so a false hard-lockup
    /// report against a healthy core can be told from a real wedge
    /// (`crate::watchdog`).
    ///
    /// Emitted once on the boot CPU by a `watchdog-diagnostics` build,
    /// through the diagnostic (log/UART) stream, never the persistent
    /// hash-chained audit trail. `self_sample` states plainly whether the
    /// sample is `live` or `inactive`; `verdict` carries the honesty term
    /// (`supported`/`unsupported`/`pending`) and `reason` the note a
    /// non-live verdict carries, so a reader of a later
    /// `sampled=pre_silence` lockup record can immediately tell whether it
    /// is credible. Carries no kernel address and no secret. A shippable
    /// image compiles the whole facility out; the id is reserved so the
    /// catalogue stays stable (`plans/WATCHDOG.md`).
    CpuWatchdogSelfSample,
    /// A device interrupt reached the task waiting on it late: its `line`
    /// fired during the wait, yet the waiter resumed `late_ms` later.
    /// `drained_ms` is how long after the fire the interrupt's deferred wake
    /// was performed, absent when none was since; it tells a wake left
    /// pending apart from a woken task left unrun.
    ///
    /// Emitted by the in-kernel device wait, in task context.
    IrqWakeLate,
    /// A request outstanding on a device's `line` has gone `overdue_ms`
    /// without its completion interrupt, and the wait goes on to its budget.
    /// `pending`, `active` and `enabled` are the line at its controller and
    /// `target` the CPU it is routed to, with that CPU's `target_activity`:
    /// a line pending and enabled while its target idles was raised but never
    /// delivered, where one not pending was never raised. Each is `null`
    /// where the controller cannot say.
    ///
    /// Emitted by the in-kernel device wait, in task context.
    IrqCompletionOverdue,
    /// An operation on a device the kernel drives itself, `device` (`dev`),
    /// took `elapsed_ms` to answer: an `op` at block `lba` of `bytes`, which
    /// succeeded when `ok`. Timed on the device side of every cache, so it is
    /// the device's own time.
    ///
    /// Emitted by the in-kernel device meter, in task context.
    DeviceOperationSlow,
    /// A contended acquire of the sleeping lock taken at `site` waited
    /// `waited_ms` for it; `handoff_ms` is how late the task ran once the
    /// prior holder had handed it the lock, and `from` the site that holder
    /// took it at (`null` when it was found free rather than handed over).
    /// A long wait with a prompt handoff is a lock held long; a late handoff
    /// is a wake or the scheduler. `task` is the task the waiter waited as
    /// (`null` when no scheduler could park it), `parks` how often it
    /// parked, `unhanded` how many of those wakes brought no handoff, and
    /// `unparkable` how often it could not park.
    ///
    /// Emitted by the sleeping lock, in task context.
    LockAcquiredLate,
    /// The holder of the sleeping lock taken at `site`, `task`, took
    /// `release_ms` to hand it over to a waiter or free it.
    ///
    /// Emitted by the sleeping lock as the holder releases it, in task
    /// context.
    LockReleasedLate,
    /// A kernel body yielded its CPU at `site` to let owed work run and was
    /// resumed `late_ms` later — runnable, and not run.
    ///
    /// Emitted at the in-kernel preemption boundary, in task context.
    KernelYieldLate,
    /// `cpu` ran kernel code for `stalled_ms` without returning to its
    /// dispatcher, holding every wake and housekeeping step that waits for
    /// it; long before a soft lockup, and once per episode. A debug image
    /// adds the sampled code's detail record.
    ///
    /// Emitted by the watchdog's cadence sample, in interrupt context.
    CpuHeldInKernel,
    /// A reschedule IPI raised on `cpu` went `pending_ms` without `cpu`
    /// taking it, as `observer`'s scan found it; `activity` is what `cpu` was
    /// doing. A task woken onto `cpu` waits for its next interrupt.
    ///
    /// Emitted by the watchdog's cross-CPU scan, in interrupt context.
    IpiUndelivered,
    /// A bound IRQ line fired past its rate budget and was **quarantined**
    /// by the runaway-interrupt safety net (`kernel/irq`): the kernel kept
    /// it masked and stopped delivering it, so a never-quiesced or hostile
    /// source can no longer peg a CPU through the mask/wake/re-arm cycle.
    ///
    /// Emitted once, at the syscall boundary (task context) the instant a
    /// waiter observes the quarantine — `irq_wait` directly or
    /// `waitset_wait` for an IRQ member — rather than from the
    /// interrupt-context `fire` path. Carries the `line` (never a secret)
    /// and the owning `task`; the waiter's syscall then fails closed with
    /// `Errno::Io`. A fresh `irq_bind` clears the quarantine.
    IrqLineQuarantined,
    /// A driver ended holding DMA memory its device may still master, so the
    /// memory passed to its node's quarantine instead of the allocator
    /// (`plans/OPEN-DEFECTS.md` D167).
    ///
    /// Emitted from the process teardown, in task context. Carries the
    /// `node`, the dead driver's admission `generation`, and the `bytes` it
    /// held.
    DmaQuarantined,
    /// DMA memory left the quarantine for the allocator, scrubbed.
    ///
    /// `cause` is `reset` when a later driver for the `node` declared its
    /// device reset (`dma_quiesced`, recorded even when it freed
    /// nothing) and `removed` when a surprise removal retired the node because
    /// its device is gone (recorded only when it freed something); `bytes` is
    /// what was freed.
    DmaQuarantineReleased,
    /// A removed hardware-tree node's authority was revoked from every task
    /// holding it, and their standing access torn down.
    ///
    /// `node` is the node named in the removal; `grants`, `holders` and
    /// `killed` count the grants revoked, the tasks that held them, and the
    /// holders killed because their access could not be torn down. The torn
    /// down access is the holders' bindings of the node's interrupt lines,
    /// windows onto its registers and mappings of its shared regions. A
    /// non-zero `killed` records at `Warn`.
    HwNodeGrantsRevoked,
    /// A function behind a translation unit is denied DMA because it would
    /// not, or could not, be confined by the unit.
    ///
    /// Discovery leaves the function unpublished, `address` naming its
    /// requester id and `reason` why: `untrusted` or `unconfinable` (no
    /// isolation group confines it), or `bypasses_unit` (a virtio function
    /// declining `VIRTIO_F_ACCESS_PLATFORM`). A driver loaded for `node` is
    /// refused with `reason=stranded` where its unit translates nothing or
    /// the node's record could not be read. `unit` is the unit's node, or
    /// `unknown`.
    DmaTranslationBypass,
    /// A translation unit refused a device's access.
    ///
    /// `unit`, `stream`, `iova`, `access` and `reason` name it; `node` is the
    /// device's node where an owner holds the stream, absent where none does;
    /// `suppressed` counts the faults the unit's budget left unrecorded since
    /// its last record.
    DmaTranslationFault,
    /// A stream raised faults past its budget's storm threshold in one
    /// window: it was silenced and its `node`, where one holds it, marked
    /// `Offline`.
    ///
    /// Carries the fault's fields and `outcome`: `silenced`, or why the unit
    /// could not silence it.
    DmaTranslationStorm,
    /// Boot brought a DMA translation unit discovery reported up, or left it
    /// translating nothing.
    ///
    /// `node` is the unit's node and `outcome` is `translating`, or why it
    /// translates nothing — `unmatched`, `no_registers`, or the family's
    /// refusal — which strands its devices: each function behind it is told
    /// to stop, and none masters DMA; `faults_unrouted`, with a `reason`,
    /// when its fault interrupt could not be served. Every record but
    /// `faults_unrouted` carries `stopped` and `refused`: the functions behind
    /// the unit found mastering DMA that were stopped and that would not stop
    /// — as it took over, those firmware keeps no window for; stranded, all.
    DmaTranslationUnit,
    /// A translation unit could not confirm that a translation ended — a
    /// driver's domain, a removed node's, or one carve's — so the device may
    /// still reach what it mapped: that memory is kept for good, and after a
    /// domain's end the node takes no further driver's carves.
    ///
    /// Carries the `node` and the driver's admission `generation`.
    DmaTranslationUnconfirmed,
    /// The kernel turned a function's bus mastering on as its DMA owner
    /// began, or off as it ended.
    ///
    /// Carries the owner's `node` and admission `generation`, `master` (`on`
    /// or `off`), and `outcome`: `applied`, or `refused` where a function
    /// still reports otherwise.
    DmaBusMaster,
    /// A driver's first carve was refused because another node's live owner
    /// holds the isolation group its device belongs to: the fabric cannot
    /// keep the two apart, so they may not have two owners.
    ///
    /// Carries the refused owner's `node` and `generation`, the `group`, and
    /// the `holder` node.
    DmaGroupRefused,
    /// No task confirms freed carves in batches, so every free waits for an
    /// invalidation of its own.
    ///
    /// Carries the `cause`: `no_room` for the batch, `not_admitted`, or
    /// `cannot_park`.
    DmaFreesUnbatched,
    /// The firmware table describing the DMA translation units is
    /// malformed, so no unit translates: the PCI functions it would have
    /// described were withheld, or, by the administrator's choice, published
    /// with their DMA unconfined.
    ///
    /// Carries the `family` the table describes units of, and the `outcome`:
    /// `withheld` or `unconfined`.
    DmaUnitsMalformed,
    /// The port routed the interrupt sources it set up at boot.
    ///
    /// `outcome` is `remapped`, where every source raises only the entry the
    /// kernel wrote for it; `unremapped`, where no unit translates or one
    /// cannot remap, so a device can forge any interrupt; `refused` or
    /// `stranded`, where remapping could not be turned on, `stranded` naming
    /// a unit that also could not turn it back off and so refuses its
    /// devices' interrupts; or `unrouted`, where remapping is on but the
    /// `unrouted` sources could be given no entry and were left unrouted.
    InterruptRemapping,
    /// A driver's port access was refused because it reaches ports the kernel
    /// keeps for itself (PCI configuration mechanism #1), whatever its grant.
    ///
    /// Carries the `port`, the access `width` in bytes, and the caller's
    /// `task`.
    PortIoRefused,
    /// A capability- and permission-checked filesystem mutation succeeded.
    ///
    /// Emitted by the `fs_mkdir` / `fs_unlink` / `fs_rename` / `fs_set_mode`
    /// / `fs_set_owner` syscall handlers after the secured VFS applied the
    /// change under the caller's kernel-attested identity. The record names
    /// the operation (`op`), the caller's uid (`uid`), and the target path
    /// (`path`, plus `to` for a rename, `mode` for a chmod's new mode in
    /// octal, `owner` and `group` for a chown's new ids) — never a
    /// capability token or secret. Every mutation of on-disk state is a security-relevant
    /// decision, so it is auditable; the paths are bounded to the log
    /// field limit on a character boundary so the record can never fail to
    /// encode and drop, which would let a mutation escape the trail.
    FsNodeMutated,
    /// A filesystem mutation was refused; nothing changed (fail closed).
    ///
    /// The refusing counterpart of [`FsNodeMutated`](Self::FsNodeMutated),
    /// emitted by the same handlers when the secured VFS returns an error
    /// (a permission, capability, mount-flag, or validation refusal). The
    /// record carries the same fields plus the refusal's `errno`, so an attempt to mutate state the caller may not
    /// touch is as visible as a successful one.
    FsMutationDenied,
    /// The boot-time system configuration store
    /// (`/System/Settings/Configuration/system.conf`) was read off the
    /// unlocked root and applied (`crate::syscfg`).
    ///
    /// Emitted once at unlock after the operator's cache switches are
    /// applied to the live cache-admission control. The record names
    /// whether the store was present (`source`: `store` / `default`) and
    /// the effective mode of each cache class (`cache.filesystem`,
    /// `cache.block`, `cache.transform`, `cache.semantic`) — a
    /// security-relevant boot policy, never any secret.
    SystemConfigApplied,
    /// The boot-time system configuration store could not be read or
    /// parsed; the kernel fell back to the all-caches-enabled defaults
    /// (fail-safe: the caches are accelerators, never the source of
    /// truth) and applied them (`crate::syscfg`).
    ///
    /// Emitted once at unlock when the store is present but malformed
    /// (unreadable, not UTF-8, or outside the closed grammar). A simply
    /// *absent* store is not a rejection — it is the normal default case
    /// and emits [`SystemConfigApplied`](Self::SystemConfigApplied) with
    /// `source: default`. Carries a `cause` naming the refusal.
    SystemConfigRejected,
    /// A self-optimising accelerated-routine family selected its
    /// implementation for this boot (`crate::cpuops` / `lib/cpuops`).
    ///
    /// Emitted once per family after the common CPU-feature set is
    /// finalised: it records which implementation won (`chosen`) and why
    /// (`reason` — `priority`/`benchmark`/`baseline`/`pinned`), so the
    /// routine selection is observable and pinnable for reproducible-build
    /// validation. Operational, not a security decision, but recorded with a
    /// stable event id like every other boot-time choice.
    CpuOpsRoutineSelected,
    /// The boot-time cryptographic power-on self-test failed: the live
    /// SHA-256 path did not reproduce its published known-answer vectors
    /// (`tairix_crypto::backend`), so even the audited software baseline is
    /// computing wrong answers.
    ///
    /// Emitted once, immediately before the kernel halts. Running with a
    /// crypto core that fails its known-answer test is not a recoverable
    /// condition — every capability signature, manifest hash, and
    /// encrypted-swap tag would rest on broken cryptography — so this is a
    /// fatal, security-critical boot fault (the FIPS power-on-self-test
    /// discipline: a failed POST renders the module inoperable). Carries the
    /// `family` whose self-test failed.
    CryptoSelfTestFailed,
    /// A served volume's backing block device began reporting itself
    /// unhealthy while still serving I/O (a recovered-error threshold, a
    /// pending sector reallocation). The `dev` field names the block-service
    /// endpoint. Emitted once on the healthy/recovering -> degraded edge, not
    /// per request.
    VolumeDegraded,
    /// A served volume's backing block device stalled or reset and entered
    /// its bounded recovery grace window; its I/O is being ridden out
    /// reissuably while it is given a bounded chance to come back. The `dev`
    /// field names the block-service endpoint. Emitted once on entry to the
    /// recovering edge.
    VolumeRecovering,
    /// A degraded or recovering volume returned to healthy service — the
    /// disk came back. The `dev` field names the block-service endpoint.
    /// Logged as a recovery, not a fault, and emitted once on the return
    /// edge so a disk that comes back to life is noted in the health trail.
    VolumeRecovered,
    /// An admitted `system_power` transition: the caller's capability was
    /// accepted, every mounted volume flushed, and the platform is about to
    /// be asked to power off or restart.
    ///
    /// Emitted immediately before the platform primitive is called — the
    /// machine stops inside that call, so a record written afterwards would
    /// never reach the trail. The `caller` and `action` fields name the
    /// kernel-attested requester and the requested transition
    /// (`power-off`/`restart`), never a capability token. A refused call is
    /// already recorded by the dispatcher's own audited refusal, so this id
    /// marks only transitions that were allowed to proceed.
    SystemPower,
    /// An owned hardware-tree node was retired by the `hw_remove_node`
    /// syscall.
    ///
    /// Emitted by the handler after the validated removal, for every
    /// posture. The `node` field names the removed node and `mode` the
    /// removal posture (`surprise` for a device that physically vanished,
    /// `orderly` for an administrator's idle stop-if-idle retirement) — a
    /// topology change an operator must be able to attribute. It carries no
    /// secret or capability token.
    HwNodeRemoved,
    /// An orderly `hw_remove_node` was refused because the node is still in
    /// use; nothing was removed (fail closed).
    ///
    /// Emitted by the handler when the stop-if-idle posture finds a volume
    /// still attached on a block-service endpoint the node declares, so the
    /// removal is refused with [`tairix_abi::Errno::Busy`] to keep a live
    /// mounted volume from becoming a surprise-removal event. The `node`
    /// field names the node left in place — a security-relevant refusal an
    /// operator must be able to see. It carries no secret or capability
    /// token.
    HwNodeRemoveRefused,
    /// The write-back flusher could not publish a volume whose batched
    /// filesystem transaction had aged out (`plans/ARXFS-WRITEBACK.md` §10).
    ///
    /// The `volume` field names the mount handle and `error` the driver's
    /// refusal. No caller is waiting on a background publish, so this record
    /// is the only place the failure can be seen: the driver has abandoned
    /// the transaction back to its last published root and, having reported
    /// operations into it, serves no further writes — so the volume is now
    /// read-only and behind. It carries no secret or capability token.
    VolumeWritebackFailed,
    /// A mounted volume is down to its reserve
    /// (`docs/src/filesystem/arxfs-spec.md` §6): ordinary writes may be
    /// refused, while the system's own writes and deletes go on into it.
    ///
    /// The `volume` field names the mount handle and `source` the volume.
    /// It is the operator's warning while the system still has runway.
    VolumeReserveEntered,
    /// A volume that was down to its reserve has room outside it again;
    /// `volume` names the mount handle and `source` the volume.
    VolumeReserveLeft,
    /// A user-space filesystem driver answered a call with a reply the kernel
    /// refused: the call failed with a device fault, and the volume reads as
    /// degraded until a call is answered cleanly. The `endpoint` and `op`
    /// fields name the driver's endpoint and the operation.
    ServedReplyMalformed,
    /// A user-space filesystem driver went a whole idle budget without
    /// answering a call or touching its volume, so the call was abandoned
    /// and the volume reads as degraded. The `endpoint` and `op` fields name
    /// the driver's endpoint and the operation.
    ServedCallStalled,
    /// A wall-clock write lost to its provenance ladder: the stored reading
    /// came from a better source, so a weaker one cannot roll it back and the
    /// clock is untouched. `caller` names the writing process, and
    /// `attempted` and `stored` the two provenance states.
    WallClockSetRefused,
    /// A principal bound a volume's subtree at a view path
    /// (`plans/NEW-NAMESPACE.md` NS-4a). The `uid` and `target` fields name
    /// the binder and the path.
    ViewBound,
    /// A view bind was refused; `uid`, `target` and `error` say whose, where
    /// and why.
    ViewBindRefused,
    /// A principal withdrew a view binding; `uid` and `target` name it.
    ViewUnbound,
    /// A view unbind was refused; `uid`, `target` and `error` say whose,
    /// where and why.
    ViewUnbindRefused,
    /// An interactive surface overran the frame budget it declared
    /// (`plans/FIX-STALLTRACE.md`).
    ///
    /// Emitted by the syscall dispatcher of a `watchdog-diagnostics` build
    /// through the diagnostic (log/UART) stream, never the persistent
    /// hash-chained audit trail — like the lockup detail, an
    /// address-bearing developer aid is kept off the tamper-evident log.
    /// The fields say what spent the budget: `budget_ms`/`elapsed_ms`, how
    /// much of it went to blocking calls (`blocked_ms`, `calls`) versus
    /// user-mode work, the syscall that carried the span over
    /// (`blocked_in`), the syscalls it made most often (`top_calls`), and a
    /// `pc` plus `bt` chain of the stalling code. `top_calls` is what names a
    /// span spent in a storm of short calls, where the call in flight at the
    /// boundary is an arbitrary member of it.
    /// Every code address is expressed relative to the program's own load
    /// base and is omitted when that base is unknown, so the record is an
    /// offline `addr2line` input and never an ASLR oracle. It carries no
    /// secret and no capability token. A shippable image compiles the whole
    /// facility out; the id is reserved so the catalogue stays stable.
    TaskLatencyOverrun,
}

impl AuditEvent {
    /// Stable numeric identifier carried by the emitted log record.
    #[must_use]
    pub const fn id(self) -> EventId {
        EventId(match self {
            Self::BootStarted => 4000,
            Self::PhaseStarted => 4001,
            Self::PhaseReady => 4002,
            Self::PhaseFailed => 4003,
            Self::BootCompleted => 4004,
            Self::RamSelfTest => 4005,
            Self::KernelHeapGrowth => 4006,
            Self::Panic => fatal::KERNEL_PANIC.id.0,
            Self::KernelFault => fatal::KERNEL_FAULT.id.0,
            Self::KernelStackCorrupt => fatal::KERNEL_STACK_CORRUPT.id.0,
            Self::SyscallNoCallerContext => 4021,
            Self::ProcessSpawned => 4030,
            Self::ProcessSpawnDenied => 4031,
            Self::ProcessSpawnFailed => 4032,
            Self::DriverUnloaded => 4033,
            Self::TaskFaultKilled => 4034,
            Self::TaskExitedNonzero => 4035,
            Self::ProcessSignalCrossPrincipal => 4036,
            Self::ProcessPriorityChange => 4037,
            Self::SessionMemberEnded => 4038,
            Self::SessionReaperUnavailable => 4039,
            Self::UsersDbLoaded => 4040,
            Self::UsersDbRejected => 4041,
            Self::GroupsDbLoaded => 4043,
            Self::GroupsDbRejected => 4044,
            Self::DriverStoreScanned => 4042,
            Self::UserAdminApplied => 4045,
            Self::UserAdminRejected => 4046,
            Self::InputDelivered => 4050,
            Self::SeatSwitched => 4051,
            Self::SeatLeaseRevoked => 4052,
            Self::SeatCreated => 4053,
            Self::SeatDestroyed => 4054,
            Self::SeatLeaseReclaimed => 4055,
            Self::EntropyReserveSeeded => 4060,
            Self::EntropyReserveUnseeded => 4061,
            Self::BootIdMinted => 4062,
            Self::BootIdUnavailable => 4063,
            Self::HashKeyPublished => 4064,
            Self::HashKeyUnavailable => 4065,
            Self::SecondaryCpuStarted => 4070,
            Self::SecondaryCpuStartFailed => 4071,
            Self::SecondaryCpuOnline => 4072,
            Self::IrqWakeLate => 4073,
            Self::IrqCompletionOverdue => 4074,
            Self::DeviceOperationSlow => 4075,
            Self::LockAcquiredLate => 4076,
            Self::LockReleasedLate => 4069,
            Self::KernelYieldLate => 4077,
            Self::CpuHeldInKernel => 4078,
            Self::IpiUndelivered => 4079,
            Self::CpuStallDetected => 4080,
            Self::CpuStallCleared => 4081,
            Self::CpuHardLockupDetected => 4082,
            Self::CpuHardLockupCleared => 4083,
            Self::CpuLockupRecovery => 4084,
            Self::CpuLockupDiagnostic => 4085,
            Self::CpuWatchdogSelfSample => 4086,
            Self::IrqLineQuarantined => 4090,
            Self::DmaQuarantined => 4091,
            Self::DmaQuarantineReleased => 4092,
            Self::HwNodeGrantsRevoked => 4093,
            Self::DmaTranslationBypass => 4087,
            Self::DmaTranslationFault => 4088,
            Self::DmaTranslationStorm => 4089,
            Self::DmaTranslationUnit => 4094,
            Self::DmaTranslationUnconfirmed => 4095,
            Self::DmaBusMaster => 4147,
            Self::DmaGroupRefused => 4148,
            Self::DmaFreesUnbatched => 4250,
            Self::DmaUnitsMalformed => 4104,
            Self::PortIoRefused => 4158,
            Self::InterruptRemapping => 4159,
            Self::FsNodeMutated => 4260,
            Self::FsMutationDenied => 4261,
            Self::SystemConfigApplied => 4110,
            Self::SystemConfigRejected => 4111,
            Self::CpuOpsRoutineSelected => 4120,
            Self::CryptoSelfTestFailed => 4121,
            Self::VolumeDegraded => 4130,
            Self::VolumeRecovering => 4131,
            Self::VolumeRecovered => 4132,
            Self::SystemPower => 4262,
            Self::HwNodeRemoved => 4264,
            Self::HwNodeRemoveRefused => 4265,
            Self::VolumeWritebackFailed => 4263,
            Self::TaskLatencyOverrun => 4266,
            Self::VolumeReserveEntered => 4267,
            Self::VolumeReserveLeft => 4268,
            Self::ServedReplyMalformed => 4190,
            Self::ServedCallStalled => 4191,
            Self::WallClockSetRefused => 4269,
            Self::ViewBound => 4310,
            Self::ViewBindRefused => 4311,
            Self::ViewUnbound => 4312,
            Self::ViewUnbindRefused => 4313,
        })
    }

    /// Short, fixed name used as the `message` field of the emitted
    /// [`tairix_log::Event`]. Kept under the 120-character convention
    /// described in `lib/log` so a single record fits one terminal line.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::BootStarted => "kernel boot started",
            Self::PhaseStarted => "kernel init phase started",
            Self::PhaseReady => "kernel init phase ready",
            Self::PhaseFailed => "kernel init phase failed",
            Self::BootCompleted => "kernel boot completed",
            Self::RamSelfTest => "ram self-test completed",
            Self::KernelHeapGrowth => "kernel heap growth decided",
            Self::Panic => fatal::KERNEL_PANIC.message,
            Self::KernelFault => fatal::KERNEL_FAULT.message,
            Self::KernelStackCorrupt => fatal::KERNEL_STACK_CORRUPT.message,
            Self::SyscallNoCallerContext => "syscall has no caller context",
            Self::ProcessSpawned => "process spawned",
            Self::ProcessSpawnDenied => "process spawn denied",
            Self::ProcessSpawnFailed => "process spawn failed",
            Self::DriverUnloaded => "driver unloaded",
            Self::TaskFaultKilled => "task killed by unresolvable user fault",
            Self::TaskExitedNonzero => "task exited with nonzero status",
            Self::ProcessSignalCrossPrincipal => "process signal cross-principal decision",
            Self::ProcessPriorityChange => "process scheduling-priority change decision",
            Self::SessionMemberEnded => "process ended with its session",
            Self::SessionReaperUnavailable => "session reaper unavailable",
            Self::UsersDbLoaded => "users database loaded",
            Self::UsersDbRejected => "users database rejected",
            Self::GroupsDbLoaded => "groups database loaded",
            Self::GroupsDbRejected => "groups database rejected",
            Self::DriverStoreScanned => "driver store scanned",
            Self::UserAdminApplied => "user administration applied",
            Self::UserAdminRejected => "user administration rejected",
            Self::InputDelivered => "first input delivered to focus arbiter",
            Self::SeatSwitched => "seat foreground switched",
            Self::SeatLeaseRevoked => "seat lease revoked",
            Self::SeatCreated => "seat created for display node",
            Self::SeatDestroyed => "seat destroyed with display node",
            Self::SeatLeaseReclaimed => "seat lease reclaimed from dead owner",
            Self::EntropyReserveSeeded => "entropy reserve seeded",
            Self::EntropyReserveUnseeded => "entropy reserve unseeded",
            Self::BootIdMinted => "per-boot id minted",
            Self::BootIdUnavailable => "per-boot id unavailable",
            Self::HashKeyPublished => "per-boot hash key published",
            Self::HashKeyUnavailable => "per-boot hash key unavailable",
            Self::SecondaryCpuStarted => "secondary cpu start requested",
            Self::SecondaryCpuStartFailed => "secondary cpu start failed",
            Self::SecondaryCpuOnline => "secondary cpu online",
            Self::IrqWakeLate => "device interrupt reached its waiter late",
            Self::IrqCompletionOverdue => "device completion interrupt overdue",
            Self::DeviceOperationSlow => "kernel-driven device operation slow",
            Self::LockAcquiredLate => "sleeping lock acquired late",
            Self::LockReleasedLate => "sleeping lock released late",
            Self::KernelYieldLate => "in-kernel yield resumed late",
            Self::CpuHeldInKernel => "cpu held by kernel code without dispatching",
            Self::IpiUndelivered => "reschedule ipi not taken",
            Self::CpuStallDetected => "cpu stall detected",
            Self::CpuStallCleared => "cpu stall cleared",
            Self::CpuHardLockupDetected => "cpu hard lockup detected",
            Self::CpuHardLockupCleared => "cpu hard lockup cleared",
            Self::CpuLockupRecovery => "cpu lockup recovery requested",
            Self::CpuLockupDiagnostic => "cpu lockup diagnostic detail",
            Self::CpuWatchdogSelfSample => "cpu watchdog non-maskable self-sample capability",
            Self::IrqLineQuarantined => "irq line quarantined (runaway interrupt)",
            Self::DmaQuarantined => "dead driver's dma memory quarantined",
            Self::DmaQuarantineReleased => "quarantined dma memory released",
            Self::HwNodeGrantsRevoked => "removed node's grants revoked",
            Self::DmaTranslationBypass => "function bypassing its translation unit refused",
            Self::DmaTranslationFault => "dma translation fault",
            Self::DmaTranslationStorm => "dma translation fault storm silenced",
            Self::DmaTranslationUnit => "dma translation unit brought up",
            Self::DmaTranslationUnconfirmed => "dma translation end unconfirmed",
            Self::DmaBusMaster => "bus mastering changed",
            Self::DmaGroupRefused => "dma carve refused: isolation group held",
            Self::DmaFreesUnbatched => "dma frees confirmed one at a time",
            Self::DmaUnitsMalformed => "dma translation units' firmware table malformed",
            Self::PortIoRefused => "port access refused: kernel-owned port",
            Self::InterruptRemapping => "interrupt sources routed",
            Self::FsNodeMutated => "filesystem node mutated",
            Self::FsMutationDenied => "filesystem mutation denied",
            Self::SystemConfigApplied => "system configuration applied",
            Self::SystemConfigRejected => "system configuration rejected",
            Self::CpuOpsRoutineSelected => "cpu accelerated routine selected",
            Self::CryptoSelfTestFailed => "crypto power-on self-test failed",
            Self::VolumeDegraded => "volume backing device degraded",
            Self::VolumeRecovering => "volume backing device recovering",
            Self::VolumeRecovered => "volume backing device recovered",
            Self::SystemPower => "system power transition admitted",
            Self::HwNodeRemoved => "hardware-tree node removed",
            Self::HwNodeRemoveRefused => "hardware-tree node removal refused (busy)",
            Self::VolumeWritebackFailed => "volume write-back publish failed",
            Self::VolumeReserveEntered => "volume down to its reserve",
            Self::VolumeReserveLeft => "volume has room outside its reserve again",
            Self::ServedReplyMalformed => "served filesystem answered with a malformed reply",
            Self::ServedCallStalled => "served filesystem stalled on a call; abandoned",
            Self::WallClockSetRefused => "wall clock write refused by its provenance ladder",
            Self::ViewBound => "volume bound at a view path",
            Self::ViewBindRefused => "view bind refused",
            Self::ViewUnbound => "view binding withdrawn",
            Self::ViewUnbindRefused => "view unbind refused",
            Self::TaskLatencyOverrun => "interactive surface overran its frame budget",
        }
    }
}

/// Emit one audit record through a `Sink`.
pub(crate) fn emit(sink: &dyn Sink, level: Level, event: AuditEvent, fields: &[Field<'_>]) {
    log(
        sink,
        &Event {
            level,
            id: event.id(),
            message: event.message(),
            fields,
        },
    );
}

/// The boot's audit sink, for the kernel code that reports with no sink of
/// its own: the lockup watchdog, the sleeping lock and the in-kernel yield.
static KERNEL_SINK: OnceCell<&'static (dyn Sink + Sync)> = OnceCell::new();

/// Install the sink the kernel's own reports go through. The boot path
/// installs exactly one; a later call is a benign no-op.
pub fn install_kernel_sink(sink: &'static (dyn Sink + Sync)) {
    let _ = KERNEL_SINK.set(sink);
}

/// The installed kernel sink, or [`None`] before boot installs it — until
/// then those records are not written.
pub(crate) fn kernel_sink() -> Option<&'static dyn Sink> {
    let sink: &'static (dyn Sink + Sync) = *KERNEL_SINK.get().ok().flatten()?;
    Some(sink)
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeSet;
    use alloc::vec::Vec;

    use super::AuditEvent;

    /// Every event, shared by every case below. The `id` and `message`
    /// matches are exhaustive, so the compiler names those sites for a new
    /// variant; this list is the one it cannot, so its length is asserted.
    const ALL: &[AuditEvent] = &[
        AuditEvent::BootStarted,
        AuditEvent::PhaseStarted,
        AuditEvent::PhaseReady,
        AuditEvent::PhaseFailed,
        AuditEvent::BootCompleted,
        AuditEvent::RamSelfTest,
        AuditEvent::KernelHeapGrowth,
        AuditEvent::Panic,
        AuditEvent::KernelFault,
        AuditEvent::KernelStackCorrupt,
        AuditEvent::SyscallNoCallerContext,
        AuditEvent::ProcessSpawned,
        AuditEvent::ProcessSpawnDenied,
        AuditEvent::ProcessSpawnFailed,
        AuditEvent::DriverUnloaded,
        AuditEvent::TaskFaultKilled,
        AuditEvent::TaskExitedNonzero,
        AuditEvent::ProcessSignalCrossPrincipal,
        AuditEvent::ProcessPriorityChange,
        AuditEvent::SessionMemberEnded,
        AuditEvent::SessionReaperUnavailable,
        AuditEvent::UsersDbLoaded,
        AuditEvent::UsersDbRejected,
        AuditEvent::DriverStoreScanned,
        AuditEvent::GroupsDbLoaded,
        AuditEvent::GroupsDbRejected,
        AuditEvent::UserAdminApplied,
        AuditEvent::UserAdminRejected,
        AuditEvent::InputDelivered,
        AuditEvent::SeatSwitched,
        AuditEvent::SeatLeaseRevoked,
        AuditEvent::SeatCreated,
        AuditEvent::SeatDestroyed,
        AuditEvent::SeatLeaseReclaimed,
        AuditEvent::EntropyReserveSeeded,
        AuditEvent::EntropyReserveUnseeded,
        AuditEvent::BootIdMinted,
        AuditEvent::BootIdUnavailable,
        AuditEvent::HashKeyPublished,
        AuditEvent::HashKeyUnavailable,
        AuditEvent::SecondaryCpuStarted,
        AuditEvent::SecondaryCpuStartFailed,
        AuditEvent::SecondaryCpuOnline,
        AuditEvent::IrqWakeLate,
        AuditEvent::IrqCompletionOverdue,
        AuditEvent::DeviceOperationSlow,
        AuditEvent::LockAcquiredLate,
        AuditEvent::LockReleasedLate,
        AuditEvent::KernelYieldLate,
        AuditEvent::CpuHeldInKernel,
        AuditEvent::IpiUndelivered,
        AuditEvent::CpuStallDetected,
        AuditEvent::CpuStallCleared,
        AuditEvent::CpuHardLockupDetected,
        AuditEvent::CpuHardLockupCleared,
        AuditEvent::CpuLockupRecovery,
        AuditEvent::CpuLockupDiagnostic,
        AuditEvent::CpuWatchdogSelfSample,
        AuditEvent::IrqLineQuarantined,
        AuditEvent::DmaQuarantined,
        AuditEvent::DmaQuarantineReleased,
        AuditEvent::HwNodeGrantsRevoked,
        AuditEvent::DmaTranslationBypass,
        AuditEvent::DmaTranslationFault,
        AuditEvent::DmaTranslationStorm,
        AuditEvent::DmaTranslationUnit,
        AuditEvent::DmaTranslationUnconfirmed,
        AuditEvent::DmaBusMaster,
        AuditEvent::DmaGroupRefused,
        AuditEvent::DmaFreesUnbatched,
        AuditEvent::DmaUnitsMalformed,
        AuditEvent::PortIoRefused,
        AuditEvent::InterruptRemapping,
        AuditEvent::FsNodeMutated,
        AuditEvent::FsMutationDenied,
        AuditEvent::SystemConfigApplied,
        AuditEvent::SystemConfigRejected,
        AuditEvent::CpuOpsRoutineSelected,
        AuditEvent::CryptoSelfTestFailed,
        AuditEvent::VolumeDegraded,
        AuditEvent::VolumeRecovering,
        AuditEvent::VolumeRecovered,
        AuditEvent::SystemPower,
        AuditEvent::VolumeWritebackFailed,
        AuditEvent::HwNodeRemoved,
        AuditEvent::HwNodeRemoveRefused,
        AuditEvent::TaskLatencyOverrun,
        AuditEvent::VolumeReserveEntered,
        AuditEvent::VolumeReserveLeft,
        AuditEvent::WallClockSetRefused,
        AuditEvent::ViewBound,
        AuditEvent::ViewBindRefused,
        AuditEvent::ViewUnbound,
        AuditEvent::ViewUnbindRefused,
        AuditEvent::ServedReplyMalformed,
        AuditEvent::ServedCallStalled,
    ];

    #[test]
    fn every_event_is_listed_for_both_checks() {
        // A guard on the list itself: the count is the one thing neither
        // exhaustive match can enforce, so it is asserted rather than
        // assumed.
        assert_eq!(ALL.len(), 96, "a new event belongs in `ALL`");
    }

    #[test]
    fn event_ids_are_in_the_kernel_range() {
        for ev in ALL {
            assert!(
                tairix_log::registry::KERNEL.contains(ev.id()),
                "{ev:?} id {} escapes the kernel's range",
                ev.id().0
            );
        }
    }

    /// The kernel page's catalogue lists exactly the events recorded here, so
    /// neither can gain or lose one alone.
    #[test]
    fn the_kernel_page_catalogues_every_event() {
        const PAGE: &str = include_str!("../../../docs/src/architecture/kernel.md");
        let (_, section) = PAGE
            .split_once("\n## Audit event catalogue\n")
            .expect("the kernel page has an audit event catalogue");
        let listed: BTreeSet<u32> = section
            .lines()
            .skip_while(|line| !line.starts_with('|'))
            .take_while(|line| line.starts_with('|'))
            .filter_map(|row| row.split('|').nth(1)?.trim().parse().ok())
            .collect();
        let recorded: BTreeSet<u32> = ALL.iter().map(|ev| ev.id().0).collect();
        let missing: Vec<&u32> = recorded.difference(&listed).collect();
        let stale: Vec<&u32> = listed.difference(&recorded).collect();
        assert!(
            missing.is_empty() && stale.is_empty(),
            "the catalogue lacks {missing:?} and lists {stale:?}, which no event has"
        );
    }

    #[test]
    fn event_ids_are_unique() {
        let ids: Vec<u32> = ALL.iter().map(|ev| ev.id().0).collect();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i], ids[j], "duplicate event id");
            }
        }
    }
}
