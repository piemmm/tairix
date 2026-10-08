//! The SMP/IPI vertical's run, shared by the GICv2 and GICv3 binaries.

use core::num::NonZeroU16;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use tairix_arch_aarch64::kernel_arch::{read_cntfrq, SecondaryStart};
use tairix_arch_aarch64::{
    enable_fp_el1, exceptions, fdt, gic, handle_panic_via_serial, preempt, qemu_exit, smp,
    Aarch64Arch, Aarch64ArchStorage, SERIAL_SINK,
};
use tairix_arch_api::{CpuId, SchedulerArch, SecondaryBringup, BOOT_CPU};
use tairix_fdt::Fdt;
use tairix_itest_finisher::fail_point;
use tairix_log::{log, Event, EventId, Level};

// The board's device tree, embedded at build time: the GIC is read from it
// (P3), proving the IPI is delivered over a *discovered* controller.
use crate::tree::DTB_BLOB;

/// `u32` sentinel for "no IPI callback has fired yet".
const NO_CPU: u32 = u32::MAX;

/// Dense id of the first secondary core this test starts.
const SECONDARY_CPU: CpuId = 1;

/// Number of CPUs in the Raspberry Pi 4-shaped QEMU topology.
const CPU_COUNT: CpuId = 4;

/// The GIC's slot for each of this test's CPUs.
static GIC_CPUS: [gic::GicCpu; CPU_COUNT as usize] =
    [const { gic::GicCpu::new() }; CPU_COUNT as usize];

/// Local generic-timer frequency used to prove each secondary's PPI.
const TICK_HZ: u64 = 100;

/// The PSCI conduit the QEMU `virt` board declares (no EL3 → `hvc`).
/// This is the *expected* result of discovery, asserted against the
/// conduit `fdt::psci_method` reads from the embedded tree — the
/// vertical drives bring-up over the discovered value, not this
/// constant (`plans/PI.md` P5).
const VIRT_EXPECTED_PSCI_METHOD: fdt::PsciMethod = fdt::PsciMethod::Hvc;

/// Stable audit-event ids for the QEMU transcript.
const SMP_TEST_START: EventId = EventId(4230);
const SMP_SECONDARY_UP: EventId = EventId(4231);
const SMP_TEST_PASS: EventId = EventId(4232);
const SMP_TEST_FAIL: EventId = EventId(4233);

/// Failure finisher code: the secondary core never came up.
const FAIL_SECONDARY_START: NonZeroU16 = fail_point!(1);
/// Failure finisher code: the IPI fired on the wrong core.
const FAIL_WRONG_CPU: NonZeroU16 = fail_point!(2);
/// Failure finisher code: `CNTFRQ_EL0` reported a zero frequency.
const FAIL_ZERO_FREQ: NonZeroU16 = fail_point!(3);
/// Failure finisher code: the GICv2 bases were not discovered from the
/// embedded `virt` device tree (P3).
const FAIL_GIC_NOT_DISCOVERED: NonZeroU16 = fail_point!(4);
/// Failure finisher code: the PSCI conduit was not discovered from the
/// embedded `virt` device tree, or did not match the board's `hvc`
/// (P5).
const FAIL_PSCI_NOT_DISCOVERED: NonZeroU16 = fail_point!(5);

/// A deliberately-wrong GICv2 distributor/CPU-interface base installed
/// before discovery runs. It is **not** the `virt` GICv2 base, so a
/// later successful IPI delivery can only mean discovery overwrote it
/// with the base read from the device tree.
const POISON_GIC_BASE: usize = 0xdead_0000;

/// Bit `cpu` is set by each secondary once its vector table, GICv2
/// interface, and IPI SGI enable are in place.
static SECONDARY_READY: AtomicU32 = AtomicU32::new(0);

/// Bit `cpu` is set after that secondary services its local timer PPI.
static TIMER_FIRED: AtomicU32 = AtomicU32::new(0);

/// Counter ticks in one test quantum, published before secondaries start.
static TIMER_INTERVAL: AtomicU64 = AtomicU64::new(0);

/// Count of IPI callbacks serviced (incremented on the core that
/// takes the SGI IRQ).
static IPI_COUNT: AtomicU32 = AtomicU32::new(0);

/// The dense id of the core the most recent IPI callback fired on;
/// `NO_CPU` until one fires.
static IPI_CPU: AtomicU32 = AtomicU32::new(NO_CPU);

/// The IPI callback the SGI IRQ path invokes. A real scheduler would
/// request a reschedule on `cpu`; the test only needs to prove the
/// IPI was delivered and dispatched on the right core.
extern "C" fn on_ipi(cpu: CpuId) {
    IPI_CPU.store(cpu, Ordering::SeqCst);
    IPI_COUNT.fetch_add(1, Ordering::SeqCst);
}

/// Record one local generic-timer interrupt on `cpu`.
extern "C" fn on_tick(cpu: CpuId) {
    TIMER_FIRED.fetch_or(1u32 << cpu, Ordering::SeqCst);
}

/// Entry the secondary core runs (via the `smp.s` trampoline) once
/// the boot core starts it. Brings up its interrupt path, signals
/// ready, and idles waiting for the IPI.
extern "C" fn secondary_entry(cpu: CpuId) -> ! {
    if smp::current_cpu_index() != cpu {
        qemu_exit::exit_failure(FAIL_WRONG_CPU);
    }
    // SAFETY: this is the secondary core's first action; it has a
    // private stack (smp.s) and no source is armed on it yet. The
    // shared IPI callback was installed by the boot core before it
    // started this core. The vector table and GIC CPU interface are
    // per-CPU, so each must be installed on the core that uses them.
    unsafe {
        enable_fp_el1();
        exceptions::init_vectors();
        gic::init_secondary().expect("the GIC interface comes up");
        preempt::enable_ipi();
        preempt::init_local_preempt(cpu, TIMER_INTERVAL.load(Ordering::Acquire));
        preempt::arm_oneshot(TIMER_INTERVAL.load(Ordering::Acquire));
        exceptions::enable_irq();
    }
    // Publish readiness only after interrupts are enabled, so the
    // boot core cannot send the IPI before this core can take it.
    SECONDARY_READY.fetch_or(1u32 << cpu, Ordering::SeqCst);

    loop {
        // SAFETY: `wfi` is a wait-for-interrupt hint with no
        // architectural side effects; the delivered IPI wakes it.
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}

/// Forward to the shared aarch64 panic bridge (parks the core; the
/// run then times out and the harness reports the failure).
#[panic_handler]
fn tairix_ipi_smp_aarch64_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_serial(info)
}

/// Boot entry point — the symbol the arch crate's `boot.s`
/// trampoline calls (via `tairix_arch_aarch64_main`).
#[no_mangle]
pub extern "C" fn kernel_main(_dtb: u64) -> ! {
    log(
        &SERIAL_SINK,
        &Event {
            level: Level::Info,
            id: SMP_TEST_START,
            message: "aarch64 IPI/SMP test: starting secondary core",
            fields: &[],
        },
    );

    // The counter frequency feeds the arch handle's monotonic clock;
    // fail closed if the timer reports zero rather than dividing by it.
    let counter_hz = read_cntfrq();
    if counter_hz == 0 {
        qemu_exit::exit_failure(FAIL_ZERO_FREQ);
    }

    // P5: discover the PSCI conduit from the embedded `virt` device
    // tree rather than naming it. Fail closed if the tree declares no
    // PSCI node, or declares one other than the board's `hvc`, so the
    // secondary is brought up only over a *discovered* conduit.
    let psci_method = match Fdt::new(DTB_BLOB) {
        Ok(fdt) => fdt::psci_method(&fdt),
        Err(_) => None,
    };
    let Some(psci_method) = psci_method else {
        qemu_exit::exit_failure(FAIL_PSCI_NOT_DISCOVERED);
    };
    if psci_method != VIRT_EXPECTED_PSCI_METHOD {
        qemu_exit::exit_failure(FAIL_PSCI_NOT_DISCOVERED);
    }

    // Build the arch handle with the four-core MPIDR map. Each core's dense
    // identity
    // is published through its per-CPU word before scheduler/IRQ use.
    // Install the *discovered*
    // PSCI conduit so the `SecondaryBringup` HAL trait issues `CPU_ON`
    // over the conduit read from the tree (`plans/PI.md` P5).
    // Per-CPU bookkeeping backing for this two-core vertical.
    static ARCH_STORAGE: Aarch64ArchStorage<4> = Aarch64ArchStorage::new();
    let arch = Aarch64Arch::with_cpus(&ARCH_STORAGE, BOOT_CPU, counter_hz, &[0, 1, 2, 3])
        .with_secondary_start(SecondaryStart::Psci(psci_method));
    smp::install_current_cpu_index(BOOT_CPU);
    if smp::current_cpu_index() != BOOT_CPU {
        qemu_exit::exit_failure(FAIL_WRONG_CPU);
    }

    // P3: prove the GIC is *discovered*, not assumed. Poison the runtime
    // base, then read the GIC from the embedded `virt` device tree; every
    // later GIC access on every core goes through what was read, so a
    // delivered IPI is the runtime proof the discovery works.
    gic::configure(POISON_GIC_BASE, POISON_GIC_BASE);
    let Ok(fdt) = Fdt::new(DTB_BLOB) else {
        qemu_exit::exit_failure(FAIL_GIC_NOT_DISCOVERED);
    };
    // The base must have moved off the poison value to the `virt`
    // distributor base read from the tree, a GICv2's or a GICv3's alike.
    if gic::configure_from_fdt(&fdt).is_none() || gic::current().0 != gic::DEFAULT_GICD_BASE {
        qemu_exit::exit_failure(FAIL_GIC_NOT_DISCOVERED);
    }
    let Some(topology) = tairix_itest_gic::topology(&fdt, &GIC_CPUS) else {
        qemu_exit::exit_failure(FAIL_GIC_NOT_DISCOVERED);
    };

    // Bring up the distributor and the boot core's interface so the
    // directed SGI it later raises is forwarded.
    // SAFETY: called once on the boot core during bring-up, before
    // any source is armed.
    unsafe {
        gic::init(topology).expect("the GIC comes up");
    }

    // Register the secondary-core stack pool sized to this four-core
    // vertical before any `CPU_ON`; the `smp.s` trampoline reads its
    // published base/stride to seed each started core's stack
    // (the pool scales with the machine's core
    // count, not a fixed `const`).
    static SECONDARY_STACKS: smp::SecondaryStackPool<4> = smp::SecondaryStackPool::new();
    if SECONDARY_STACKS.register().is_err() {
        qemu_exit::exit_failure(FAIL_SECONDARY_START);
    }

    // Install the shared callbacks and per-CPU timer storage before
    // starting any secondary, so every core observes complete state.
    preempt::set_ipi_callback(on_ipi);
    preempt::set_timer_callback(on_tick);
    static PREEMPT_STORAGE: preempt::PreemptStorage<4> = preempt::PreemptStorage::new();
    if PREEMPT_STORAGE.register().is_err() {
        qemu_exit::exit_failure(FAIL_SECONDARY_START);
    }
    TIMER_INTERVAL.store(
        preempt::interval_for_hz(counter_hz, TICK_HZ),
        Ordering::Release,
    );
    if smp::set_secondary_entry(secondary_entry).is_err() {
        qemu_exit::exit_failure(FAIL_SECONDARY_START);
    }

    // Start every secondary through the `SecondaryBringup` Arch HAL trait
    // (`plans/WIRING.md` Stage W14/W15) rather than the port-private
    // `smp::start_secondary`, so this vertical exercises the same
    // neutral bring-up surface the x86_64 SMP verticals use; the
    // handle issues PSCI `CPU_ON` over the installed conduit.
    // SAFETY: called on the boot core after the secondary-stack pool
    // was registered (above) and the secondary entry was installed;
    // each id maps to a real, parked, distinct core in the handle's
    // topology.
    for cpu in SECONDARY_CPU..CPU_COUNT {
        if unsafe { arch.start_secondary(cpu) }.is_err() {
            qemu_exit::exit_failure(FAIL_SECONDARY_START);
        }
    }

    // Wait until all three secondary cores have enabled interrupts.
    let ready_mask = ((1u32 << CPU_COUNT) - 1) & !1;
    while SECONDARY_READY.load(Ordering::SeqCst) != ready_mask {
        core::hint::spin_loop();
    }
    // Every secondary armed its own physical timer PPI before publishing
    // readiness. Require all three callbacks before testing SGIs, so the
    // timer mechanism CPU-bound user tasks depend on is covered too.
    while TIMER_FIRED.load(Ordering::SeqCst) != ready_mask {
        core::hint::spin_loop();
    }
    log(
        &SERIAL_SINK,
        &Event {
            level: Level::Info,
            id: SMP_SECONDARY_UP,
            message: "aarch64 IPI/SMP test: secondary cores up, sending IPIs",
            fields: &[],
        },
    );

    // Send one directed IPI to each secondary and wait for its callback
    // before targeting the next. This proves every target-list bit, not
    // only CPU 1's, reaches the intended GICv2 CPU interface.
    for cpu in SECONDARY_CPU..CPU_COUNT {
        arch.send_ipi(cpu);
        while IPI_COUNT.load(Ordering::SeqCst) < cpu {
            core::hint::spin_loop();
        }
        if IPI_CPU.load(Ordering::SeqCst) != cpu {
            log(
                &SERIAL_SINK,
                &Event {
                    level: Level::Error,
                    id: SMP_TEST_FAIL,
                    message: "aarch64 IPI/SMP test: IPI fired on the wrong core",
                    fields: &[],
                },
            );
            qemu_exit::exit_failure(FAIL_WRONG_CPU);
        }
    }

    log(
        &SERIAL_SINK,
        &Event {
            level: Level::Info,
            id: SMP_TEST_PASS,
            message: "aarch64 IPI/SMP test: IPIs delivered to every secondary core",
            fields: &[],
        },
    );
    qemu_exit::exit_success();
}
