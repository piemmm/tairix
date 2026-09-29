//! The aarch64 floating-point isolation test kernel: build three isolated EL0
//! programs — two `fp-probe` tasks with different seeds and one `entry-hygiene`
//! task — timeshare them under the live scheduler, and do floating point in the
//! dispatch loop while a probe holds a hostile `FPCR`. The run proves per-task
//! `q0`–`q31`/`FPCR` isolation, clean first entry, and the kernel's own FP
//! environment (`plans/OPEN-DEFECTS.md` D359/D360).

use core::num::NonZeroU16;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use alloc::sync::Arc;

use tairix_abi::rxe::LoadImage;
use tairix_abi::{CapabilityId, CapabilityQuery, SyscallNumber, SYSCALL_MAX_ARGS};
use tairix_arch_aarch64::context_hal::ContextSwitchHal;
use tairix_arch_aarch64::kernel_arch::timer_frequency_hz;
use tairix_arch_aarch64::paging::{
    self, activate_user_root, AddressSpace as ArchAddressSpace, PageTablePool,
};
use tairix_arch_aarch64::userentry::UserMode;
use tairix_arch_aarch64::{
    enable_fp_el1, exceptions, gic, handle_panic_via_serial, qemu_exit, syscall_entry, SERIAL_SINK,
};
use tairix_arch_api::{EnterUser, UserEntry, BOOT_CPU};
use tairix_fdt::Fdt;
use tairix_itest_finisher::fail_point;
use tairix_kalloc::FreeListAllocator;
use tairix_kernel_core::{
    reschedule_current, spawn_image, spawn_user_kthread, RescheduleAction, SpawnMode, SpawnRequest,
    Yielder,
};
use tairix_kernel_mem::{AddressSpace, DirectPhysMap, Frame, PhysAddr, UserStack};
use tairix_kernel_sched_eevdf::{Priority, Scheduler, SchedulerConfig};
use tairix_kernel_syscall::SYSCALL_TABLE_HASH;
use tairix_log::{log, Event, EventId, Level};

// `PROGRAM_RXE`, `HYGIENE_RXE`, `USER_BIAS`, and `ROUNDS_PER_TASK`.
include!(concat!(env!("OUT_DIR"), "/program_rxe.rs"));
// The canonical QEMU `virt` device tree (GICv2 base and timer rate).
include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));

/// The two floating-point probe tasks (the hygiene task is separate).
const PROBE_COUNT: u64 = 2;

/// Gigabytes of identity map each EL0 address space provides.
const IDENTITY_GIB: usize = 2;

/// User stack base and size.
const USER_STACK_BASE: u64 = USER_BIAS + 0x10_0000;
/// User stack pages (256 KiB).
const USER_STACK_PAGES: u64 = 64;
/// User virtual address the startup-vector block is written at.
const USER_BLOCK_BASE: u64 = USER_BIAS + 0x30_0000;

/// Per-process stack-canary seed handed to each program.
const CANARY: u64 = 0x5520_C000_D15E_A5ED;

/// Physical frames the three spawn builds draw from one monotonic cursor.
const FRAME_COUNT: usize = 384;

/// Cooperative-loop watchdog: maximum `step` iterations before deadlock.
const MAX_STEPS: u64 = 5_000_000;

/// Stable audit-event ids for the QEMU transcript.
const TEST_START: EventId = EventId(4340);
const TEST_SPAWNED: EventId = EventId(4341);
const TEST_PASS: EventId = EventId(4342);

/// Failure finisher codes, distinct per failure site.
const FAIL_ZERO_FREQ: NonZeroU16 = fail_point!(1);
const FAIL_GIC_NOT_DISCOVERED: NonZeroU16 = fail_point!(2);
const FAIL_POOL: NonZeroU16 = fail_point!(3);
const FAIL_PARSE: NonZeroU16 = fail_point!(4);
const FAIL_BUILD: NonZeroU16 = fail_point!(5);
const FAIL_SCHED_NEW: NonZeroU16 = fail_point!(6);
const FAIL_SPAWN: NonZeroU16 = fail_point!(7);
const FAIL_DEADLOCK: NonZeroU16 = fail_point!(8);
const FAIL_YIELD_COUNT: NonZeroU16 = fail_point!(9);
const FAIL_EXIT_COUNT: NonZeroU16 = fail_point!(10);
const FAIL_UNEXPECTED_SYSCALL: NonZeroU16 = fail_point!(11);
const FAIL_FP_CLOBBERED: NonZeroU16 = fail_point!(12);
const FAIL_KERNEL_FP: NonZeroU16 = fail_point!(13);
const FAIL_KERNEL_FP_NEVER: NonZeroU16 = fail_point!(14);

/// Total `yield` syscalls observed across both probe tasks.
static YIELDS: AtomicU64 = AtomicU64::new(0);
/// Total `exit` syscalls observed across all three tasks.
static EXITS: AtomicU64 = AtomicU64::new(0);
/// Tasks that exited non-zero — a clobbered file or a leaked entry register.
static BAD_EXITS: AtomicU64 = AtomicU64::new(0);
/// Set non-zero if a kernel float op did not round to nearest — the sign the
/// trap trampoline failed to reset `FPCR` and the kernel ran under a probe's.
static KERNEL_FP_BAD: AtomicU64 = AtomicU64::new(0);
/// Kernel float ops performed under a probe's live `FPCR`.
static KERNEL_FP_RUNS: AtomicU64 = AtomicU64::new(0);

/// Size of the test's bump heap.
const HEAP_SIZE: usize = 4 * 1024 * 1024;

/// Page-aligned backing store for the bump heap.
#[repr(C, align(4096))]
struct HeapStore([u8; HEAP_SIZE]);

static mut HEAP: HeapStore = HeapStore([0; HEAP_SIZE]);

/// Global allocator backed by [`HEAP`].
///
/// SAFETY: the page-aligned `HEAP` static outlives the binary and the
/// allocator is its only consumer.
#[global_allocator]
static ALLOCATOR: FreeListAllocator =
    unsafe { FreeListAllocator::new(core::ptr::addr_of!(HEAP) as *mut u8, HEAP_SIZE) };

/// Per-space page-table pools, one per EL0 address space.
static PAGE_TABLES_A: PageTablePool = PageTablePool::new();
static PAGE_TABLES_B: PageTablePool = PageTablePool::new();
static PAGE_TABLES_H: PageTablePool = PageTablePool::new();

/// Physical-frame backing store the spawn builders allocate user pages from.
#[repr(C, align(4096))]
struct FramePool([u8; paging::PAGE_SIZE * FRAME_COUNT]);

static mut FRAME_POOL: FramePool = FramePool([0; paging::PAGE_SIZE * FRAME_COUNT]);

/// Monotonic index of the next free [`FRAME_POOL`] frame.
static FRAME_CURSOR: AtomicUsize = AtomicUsize::new(0);

/// Hand out the next identity-mapped physical frame, or `None` when exhausted.
fn next_frame() -> Option<Frame> {
    let idx = FRAME_CURSOR.fetch_add(1, Ordering::SeqCst);
    if idx >= FRAME_COUNT {
        FRAME_CURSOR.store(FRAME_COUNT, Ordering::SeqCst);
        return None;
    }
    let offset = idx * paging::PAGE_SIZE;
    let base = core::ptr::addr_of!(FRAME_POOL) as u64 + offset as u64;
    Some(Frame::containing(PhysAddr::new(base)))
}

fn note(id: EventId, message: &'static str) {
    log(
        &SERIAL_SINK,
        &Event {
            level: Level::Info,
            id,
            message,
            fields: &[],
        },
    );
}

/// Forward to the shared aarch64 panic bridge.
#[panic_handler]
fn fp_isolation_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_serial(info)
}

/// A [`CapabilityQuery`] granting exactly `CAP_PROC_SPAWN`.
struct SpawnAuthority;
impl CapabilityQuery for SpawnAuthority {
    fn holds(&self, cap: CapabilityId) -> bool {
        cap == CapabilityId::PROC_SPAWN
    }
}

/// Do floating point in the kernel: a division whose round-to-nearest result
/// is known. If the trap trampoline failed to reset `FPCR`, the kernel runs
/// under the yielding probe's round-toward-zero and the quotient mismatches.
fn kernel_fp_check() {
    const EXPECT: u64 = {
        let q = 2.0_f64 / 3.0_f64;
        q.to_bits()
    };
    let numerator = core::hint::black_box(2.0_f64);
    let denominator = core::hint::black_box(3.0_f64);
    let quotient = core::hint::black_box(numerator / denominator);
    if quotient.to_bits() != EXPECT {
        KERNEL_FP_BAD.store(1, Ordering::SeqCst);
    }
    KERNEL_FP_RUNS.fetch_add(1, Ordering::SeqCst);
}

/// The syscall-dispatch callback all three tasks' `svc` traps reach.
extern "C" fn dispatch(number: u64, args_ptr: *const [u64; SYSCALL_MAX_ARGS]) -> u64 {
    let call = SyscallNumber::from_register(number).ok();
    if call == Some(SyscallNumber::YIELD) {
        YIELDS.fetch_add(1, Ordering::SeqCst);
        kernel_fp_check();
        let _ = reschedule_current(BOOT_CPU, RescheduleAction::Yield);
        0
    } else if call == Some(SyscallNumber::EXIT) {
        EXITS.fetch_add(1, Ordering::SeqCst);
        // SAFETY: the dispatch callback receives the syscall argument block the
        // trap path filled from the caller's registers; `exit` carries its
        // status in the first slot.
        if unsafe { (*args_ptr)[0] } != 0 {
            BAD_EXITS.fetch_add(1, Ordering::SeqCst);
        }
        let _ = reschedule_current(BOOT_CPU, RescheduleAction::Exit);
        0
    } else {
        note(TEST_START, "fixture program issued an unexpected syscall");
        qemu_exit::exit_failure(FAIL_UNEXPECTED_SYSCALL);
    }
}

/// Build one isolated EL0 address space from `image`/`image_bytes` over `pool`,
/// with `args`, returning its stage-1 root and entry register state.
fn build_space(
    pool: &'static PageTablePool,
    image: &LoadImage,
    image_bytes: &'static [u8],
    args: &[&[u8]],
) -> (u64, UserEntry) {
    let Some(arch) = ArchAddressSpace::new_identity_gigapages(pool, IDENTITY_GIB) else {
        qemu_exit::exit_failure(FAIL_POOL);
    };
    let root_phys = arch.root_phys();
    // SAFETY: the identity map covers the kernel's current `pc`, `sp`, the
    // heap, the frame pool, and the device MMIO, so switching it does not move
    // the ground under the running code. Called on the boot CPU.
    unsafe { arch.switch() };

    let mut space = AddressSpace::new(arch);
    // SAFETY: the boot code identity-maps this window and never unmaps it.
    let physmap = unsafe { DirectPhysMap::identity((IDENTITY_GIB as u64) << 30) }
        .expect("the boot direct map addresses its window");
    let request = SpawnRequest {
        image,
        image_bytes,
        bias: USER_BIAS,
        stack: UserStack {
            base: USER_STACK_BASE,
            page_count: USER_STACK_PAGES,
        },
        start_block_base: USER_BLOCK_BASE,
        args,
        env: &[],
        canary: CANARY,
    };

    // SAFETY: building the image is safe; the returned `UserEntry` is only
    // entered later, once its space is reactivated and the EL1 trap path is
    // installed. Frames are drawn identity-mapped from `FRAME_POOL`.
    let entry = match unsafe {
        spawn_image(
            &SpawnAuthority,
            SpawnMode::General,
            &SERIAL_SINK,
            &mut space,
            &physmap,
            &request,
            next_frame,
        )
    } {
        Ok(entry) => entry,
        Err(_) => qemu_exit::exit_failure(FAIL_BUILD),
    };
    (root_phys, entry)
}

/// Admit a built space as a resumable user kthread.
fn admit(
    sched: &Scheduler<tairix_arch_aarch64::Aarch64Arch>,
    cs: ContextSwitchHal,
    root_phys: u64,
    entry: UserEntry,
) {
    let user_mode = UserMode::new();
    let pre_resume = move |_stack_top: u64| {
        // SAFETY: the MMU is enabled and `root_phys` is the L1 root of a space
        // that identity-maps the low kernel window the running kernel executes
        // from — `activate_user_root`'s contract.
        unsafe { activate_user_root(root_phys) };
    };
    let work = move |_yielder: &mut Yielder<ContextSwitchHal>| {
        // SAFETY: the entered space is active and the EL1 trap vector + dispatch
        // callback are installed, so the program's first `svc` is handled.
        unsafe { user_mode.enter_user(entry) }
    };
    if spawn_user_kthread(sched, cs, BOOT_CPU, Priority::Normal, pre_resume, work).is_err() {
        qemu_exit::exit_failure(FAIL_SPAWN);
    }
}

/// Boot entry point.
#[no_mangle]
pub extern "C" fn kernel_main(_dtb: u64) -> ! {
    note(TEST_START, "aarch64 fp isolation test: starting");

    // Enable FP/SIMD at EL1 before any code that may use it (the `rxe` decoder
    // and the image fills compile to NEON).
    // SAFETY: boot CPU, once, before any FP/SIMD executes.
    unsafe { enable_fp_el1() };

    let Ok(fdt) = Fdt::new(DTB_BLOB) else {
        qemu_exit::exit_failure(FAIL_GIC_NOT_DISCOVERED);
    };
    let counter_hz = timer_frequency_hz(&fdt);
    if counter_hz == 0 {
        qemu_exit::exit_failure(FAIL_ZERO_FREQ);
    }
    if gic::configure_from_fdt(&fdt).is_none() {
        qemu_exit::exit_failure(FAIL_GIC_NOT_DISCOVERED);
    }

    let Ok(probe) = LoadImage::parse(PROGRAM_RXE, &SYSCALL_TABLE_HASH) else {
        qemu_exit::exit_failure(FAIL_PARSE);
    };
    let Ok(hygiene) = LoadImage::parse(HYGIENE_RXE, &SYSCALL_TABLE_HASH) else {
        qemu_exit::exit_failure(FAIL_PARSE);
    };

    let (root_a, entry_a) = build_space(&PAGE_TABLES_A, &probe, PROGRAM_RXE, &[b"A"]);
    let (root_b, entry_b) = build_space(&PAGE_TABLES_B, &probe, PROGRAM_RXE, &[b"B"]);
    let (root_h, entry_h) = build_space(&PAGE_TABLES_H, &hygiene, HYGIENE_RXE, &[b"h"]);

    // SAFETY: called once on the boot CPU with a stack established and the MMU
    // enabled (the address-space builds switched it on).
    unsafe {
        exceptions::init_vectors();
        gic::init();
    }
    syscall_entry::set_dispatch_callback(dispatch);

    static ARCH_STORAGE: tairix_arch_aarch64::Aarch64ArchStorage<1> =
        tairix_arch_aarch64::Aarch64ArchStorage::new();
    let arch = Arc::new(tairix_arch_aarch64::Aarch64Arch::new(
        &ARCH_STORAGE,
        BOOT_CPU,
        counter_hz,
    ));
    let Ok(sched) = Scheduler::new(SchedulerConfig::defaults_for(1), arch) else {
        qemu_exit::exit_failure(FAIL_SCHED_NEW);
    };

    let cs = ContextSwitchHal::new();
    admit(&sched, cs, root_a, entry_a);
    admit(&sched, cs, root_b, entry_b);
    admit(&sched, cs, root_h, entry_h);
    note(
        TEST_SPAWNED,
        "aarch64 fp isolation test: three EL0 tasks spawned",
    );

    let mut steps = 0u64;
    while sched.live_task_count() != 0 && steps < MAX_STEPS {
        let _ = sched.step(BOOT_CPU);
        steps += 1;
    }
    if sched.live_task_count() != 0 {
        qemu_exit::exit_failure(FAIL_DEADLOCK);
    }
    if BAD_EXITS.load(Ordering::SeqCst) != 0 {
        qemu_exit::exit_failure(FAIL_FP_CLOBBERED);
    }
    if EXITS.load(Ordering::SeqCst) != PROBE_COUNT + 1 {
        qemu_exit::exit_failure(FAIL_EXIT_COUNT);
    }
    if YIELDS.load(Ordering::SeqCst) != PROBE_COUNT * ROUNDS_PER_TASK {
        qemu_exit::exit_failure(FAIL_YIELD_COUNT);
    }
    if KERNEL_FP_RUNS.load(Ordering::SeqCst) == 0 {
        qemu_exit::exit_failure(FAIL_KERNEL_FP_NEVER);
    }
    if KERNEL_FP_BAD.load(Ordering::SeqCst) != 0 {
        qemu_exit::exit_failure(FAIL_KERNEL_FP);
    }

    note(
        TEST_PASS,
        "aarch64 fp isolation: files isolated, entry clean, kernel FP sound",
    );
    qemu_exit::exit_success();
}
