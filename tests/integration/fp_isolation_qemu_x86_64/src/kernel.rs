//! The x86_64 floating-point isolation test kernel: boot the production
//! pipeline, then on `BootCompleted` build three isolated ring-3 programs —
//! two `fp-probe` tasks with different seeds and one `entry-hygiene` task — and
//! timeshare them as resumable user kthreads. The kernel does floating point in
//! the dispatch loop while a probe holds a hostile `MXCSR`, so the run proves
//! the per-entry SSE framing, the per-task extended-state save, and clean first
//! entry all hold (`plans/OPEN-DEFECTS.md` D359/D360/D362).

extern crate alloc;

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use tairix_abi::rxe::LoadImage;
use tairix_abi::{CapabilityId, CapabilityQuery, SyscallNumber, SYSCALL_MAX_ARGS};
use tairix_arch_api::{EnterUser, UserEntry};
use tairix_arch_x86_64::context_hal::ContextSwitchHal;
use tairix_arch_x86_64::kernel_arch::{X86_64Arch, X86_64ArchStorage};
use tairix_arch_x86_64::paging::{self, activate_user_root, KERNEL_VMA_BASE};
use tairix_arch_x86_64::userentry::UserMode;
use tairix_arch_x86_64::{qemu_exit, smp, syscall_entry};
use tairix_kernel::kalloc::{Heap, HEAP_BYTES};
use tairix_kernel::{
    boot, handle_panic_via_kernel_core, FreeListAllocator, SerialSink, SERIAL_SINK,
};
use tairix_kernel_core::{
    reschedule_current, spawn_image, spawn_user_kthread, RescheduleAction, SpawnMode, SpawnRequest,
    Yielder,
};
use tairix_kernel_mem::{AddressSpace, DirectPhysMap, Frame, PhysAddr, UserStack};
use tairix_kernel_sched_eevdf::{Priority, Scheduler, SchedulerConfig};
use tairix_kernel_syscall::SYSCALL_TABLE_HASH;
use tairix_log::{log, Event, EventId, Level, Sink};

// `PROGRAM_RXE`, `HYGIENE_RXE`, `USER_BIAS`, and `ROUNDS_PER_TASK`, generated
// by `build.rs`.
include!(concat!(env!("OUT_DIR"), "/program_rxe.rs"));

/// `EventId` emitted when every boot init phase completed.
const BOOT_COMPLETED_EVENT_ID: EventId = EventId(4004);

/// Stable audit-event ids for the QEMU transcript.
const TEST_START: EventId = EventId(4330);
const TEST_SPAWNED: EventId = EventId(4331);
const TEST_PASS: EventId = EventId(4332);
const TEST_FAIL: EventId = EventId(4333);

/// The single-core slice runs logical CPU 0 on the boot processor.
const BOOT_CPU: u32 = 0;

/// The two floating-point probe tasks (the hygiene task is separate).
const PROBE_COUNT: u64 = 2;

/// User stack base and size. `tairix-rt`'s `_start` aligns the stack and
/// calls; the probe holds its file in registers and a small array.
const USER_STACK_BASE: u64 = USER_BIAS + 0x10_0000;
/// User stack pages (256 KiB).
const USER_STACK_PAGES: u64 = 64;
/// User virtual address the startup-vector block is written at.
const USER_BLOCK_BASE: u64 = USER_BIAS + 0x30_0000;

/// Per-process stack-canary seed handed to each program.
const CANARY: u64 = 0x5520_C000_D15E_A5ED;

/// `IA32_EFER` MSR number and its No-Execute-Enable bit (bit 11).
const IA32_EFER: u32 = 0xC000_0080;
const EFER_NXE: u64 = 1 << 11;

/// Physical frames the three spawn builds draw from one monotonic cursor, so
/// the address spaces never share a data frame.
const FRAME_COUNT: usize = 384;

/// Cooperative-loop watchdog: maximum `step` iterations before deadlock.
const MAX_STEPS: u64 = 5_000_000;

/// Total `yield` syscalls observed across both probe tasks.
static YIELDS: AtomicU64 = AtomicU64::new(0);
/// Total `exit` syscalls observed across all three tasks.
static EXITS: AtomicU64 = AtomicU64::new(0);
/// Tasks that exited non-zero — a clobbered register file, a leaked entry
/// register, or a missing seed.
static BAD_EXITS: AtomicU64 = AtomicU64::new(0);
/// Set non-zero if a kernel floating-point op in the dispatch loop did not
/// round to nearest — the sign the entry stub failed to install the kernel
/// `MXCSR` and the kernel ran under a probe's round-toward-zero.
static KERNEL_FP_BAD: AtomicU64 = AtomicU64::new(0);
/// Kernel floating-point ops performed, so the test proves at least one ran
/// while a probe held its hostile `MXCSR`.
static KERNEL_FP_RUNS: AtomicU64 = AtomicU64::new(0);

/// Set once the round-trip has been driven so a duplicate `BootCompleted`
/// cannot re-enter the test logic.
static TEST_DRIVEN: AtomicU32 = AtomicU32::new(0);

/// Static heap for the bump allocator (per the production bin).
static mut HEAP: Heap = Heap::ZERO;

/// Global allocator backed by [`HEAP`].
///
/// SAFETY: the page-aligned `HEAP` static outlives the binary and the
/// allocator is its only consumer.
#[global_allocator]
static ALLOCATOR: FreeListAllocator =
    unsafe { FreeListAllocator::new(core::ptr::addr_of!(HEAP) as *mut u8, HEAP_BYTES) };

/// Per-space page-table pools, one per ring-3 address space.
static PAGE_TABLE_POOL_A: paging::PageTablePool = paging::PageTablePool::new();
static PAGE_TABLE_POOL_B: paging::PageTablePool = paging::PageTablePool::new();
static PAGE_TABLE_POOL_H: paging::PageTablePool = paging::PageTablePool::new();

/// Physical-frame backing the spawn builders draw user pages from.
#[repr(C, align(4096))]
struct FramePool([u8; paging::PAGE_SIZE * FRAME_COUNT]);

static mut FRAME_POOL: FramePool = FramePool([0; paging::PAGE_SIZE * FRAME_COUNT]);

/// Monotonic index of the next free [`FRAME_POOL`] frame.
static FRAME_CURSOR: AtomicUsize = AtomicUsize::new(0);

/// Hand out the next physical frame from [`FRAME_POOL`], or `None` when
/// exhausted. The pool is a higher-half kernel static, so its physical address
/// is its virtual address minus [`KERNEL_VMA_BASE`].
fn next_frame() -> Option<Frame> {
    let idx = FRAME_CURSOR.fetch_add(1, Ordering::SeqCst);
    if idx >= FRAME_COUNT {
        FRAME_CURSOR.store(FRAME_COUNT, Ordering::SeqCst);
        return None;
    }
    let offset = idx * paging::PAGE_SIZE;
    let virt = core::ptr::addr_of!(FRAME_POOL) as u64 + offset as u64;
    Some(Frame::containing(PhysAddr::new(virt - KERNEL_VMA_BASE)))
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

/// Forward to the shared bridge in `tairix_kernel`.
#[panic_handler]
fn fp_isolation_qemu_x86_64_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_kernel_core(info)
}

/// A [`CapabilityQuery`] granting exactly `CAP_PROC_SPAWN`.
struct SpawnAuthority;
impl CapabilityQuery for SpawnAuthority {
    fn holds(&self, cap: CapabilityId) -> bool {
        cap == CapabilityId::PROC_SPAWN
    }
}

/// Do floating point in the kernel: a division whose round-to-nearest result
/// is known, plus an invalid operation. If the entry stub failed to install
/// the kernel `MXCSR`, the kernel runs under the calling probe's
/// round-toward-zero and unmasked invalid-op, so the quotient mismatches and
/// the `0.0/0.0` raises `#XM` (a fatal kernel fault — the run dies).
fn kernel_fp_check() {
    // Const-evaluated round-to-nearest, the answer the kernel must produce.
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
    // An invalid operation: masked (the kernel `MXCSR`), this is a quiet NaN;
    // unmasked (a probe's `MXCSR` wrongly left live), it faults.
    let zero = core::hint::black_box(0.0_f64);
    let _ = core::hint::black_box(zero / zero);
    KERNEL_FP_RUNS.fetch_add(1, Ordering::SeqCst);
}

/// The syscall-dispatch callback all three tasks' `syscall` traps reach.
extern "C" fn dispatch(number: u64, args_ptr: *const [u64; SYSCALL_MAX_ARGS]) -> u64 {
    let call = SyscallNumber::from_register(number).ok();
    if call == Some(SyscallNumber::YIELD) {
        YIELDS.fetch_add(1, Ordering::SeqCst);
        // Compute in the kernel while the yielding probe holds its hostile
        // `MXCSR`, then suspend the caller.
        kernel_fp_check();
        let _ = reschedule_current(BOOT_CPU, RescheduleAction::Yield);
        0
    } else if call == Some(SyscallNumber::EXIT) {
        EXITS.fetch_add(1, Ordering::SeqCst);
        // The fixture's own verdict: it exits non-zero on the first register
        // that came back wrong (or that leaked kernel state).
        // SAFETY: the dispatch callback receives the syscall argument block the
        // trap path filled from the caller's registers; `exit` carries its
        // status in the first slot.
        // SAFETY: the dispatch callback receives the syscall argument block the
        // trap path filled from the caller's registers; `exit` carries its
        // status in the first slot.
        if unsafe { (*args_ptr)[0] } != 0 {
            BAD_EXITS.fetch_add(1, Ordering::SeqCst);
        }
        let _ = reschedule_current(BOOT_CPU, RescheduleAction::Exit);
        0
    } else {
        note(TEST_FAIL, "fixture program issued an unexpected syscall");
        qemu_exit::exit_failure();
    }
}

/// Outer audit sink: replays every event to serial and, on the single
/// [`BOOT_COMPLETED_EVENT_ID`], drives [`run_isolation`].
struct BootCompletedSink;

impl Sink for BootCompletedSink {
    fn write_event(&self, event: &Event<'_>) {
        SerialSink::new().write_event(event);

        if event.id == BOOT_COMPLETED_EVENT_ID
            && TEST_DRIVEN
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            run_isolation();
        }
    }
}

static AUDIT_SINK: BootCompletedSink = BootCompletedSink;

/// Build one isolated ring-3 address space from `image`/`image_bytes` over
/// `pool`, with `args`, returning its PML4 root and entry register state.
fn build_space(
    pool: &'static paging::PageTablePool,
    image: &LoadImage,
    image_bytes: &'static [u8],
    args: &[&[u8]],
) -> (u64, UserEntry) {
    let Some(arch_space) = paging::AddressSpace::new_boot_identity(pool) else {
        note(TEST_FAIL, "fp isolation: page-table pool exhausted");
        qemu_exit::exit_failure();
    };
    let root_phys = arch_space.pml4_phys();
    // SAFETY: the new space carries the live identity window and the
    // higher-half kernel window, so the executing RIP, the current stack, the
    // per-CPU `swapgs` TLS, the pools, the frame pool, the heap, `dispatch`,
    // and every LAPIC MMIO access stay mapped across the CR3 switch.
    unsafe { arch_space.switch() };

    let mut space = AddressSpace::new(arch_space);
    // SAFETY: the boot code installs this direct map and never unmaps it.
    let physmap = unsafe { DirectPhysMap::new(KERNEL_VMA_BASE, 1 << 30) }
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
    // entered later, once the task is dispatched and its `pre_resume` hook has
    // reloaded CR3. The GDT user selectors / TSS / `syscall` entry were
    // installed during boot, and `dispatch` is installed by the caller.
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
        Err(_) => {
            note(TEST_FAIL, "fp isolation: spawn_image failed");
            qemu_exit::exit_failure();
        }
    };
    (root_phys, entry)
}

/// Admit a built space as a resumable user kthread.
fn admit(sched: &Scheduler<X86_64Arch>, cs: ContextSwitchHal, root_phys: u64, entry: UserEntry) {
    let user_mode = UserMode::new();
    let pre_resume = move |kernel_stack_top: u64| {
        if syscall_entry::set_kernel_rsp0(BOOT_CPU as usize, kernel_stack_top).is_err() {
            note(
                TEST_FAIL,
                "fp isolation: set_kernel_rsp0 rejected the stack top",
            );
            qemu_exit::exit_failure();
        }
        // SAFETY: paging is enabled and `root_phys` is the PML4 of the task's
        // space, which maps the low identity + higher-half kernel window the
        // running dispatcher executes from — `activate_user_root`'s contract.
        unsafe { activate_user_root(root_phys) };
    };
    let work = move |_yielder: &mut Yielder<ContextSwitchHal>| {
        // SAFETY: by the time this body runs the task has been dispatched, so
        // its `pre_resume` hook reloaded CR3 + repointed the entry stack; the
        // GDT selectors / TSS / `syscall` entry + dispatch callback are
        // installed.
        unsafe { user_mode.enter_user(entry) }
    };
    if spawn_user_kthread(sched, cs, BOOT_CPU, Priority::Normal, pre_resume, work).is_err() {
        note(TEST_FAIL, "fp isolation: spawn_user_kthread failed");
        qemu_exit::exit_failure();
    }
}

/// Build the three ring-3 images, admit each as a resumable user kthread, and
/// drive the cooperative `step` loop. Never returns.
fn run_isolation() -> ! {
    note(
        TEST_START,
        "x86_64 fp isolation: building three ring-3 images",
    );

    // Enable `IA32_EFER.NXE` so the W^X No-Execute leaf bit is honoured.
    // SAFETY: reading and writing `IA32_EFER` is the documented enable
    // sequence; it runs once on the BSP and only sets bit 11.
    unsafe {
        let lo: u32;
        let hi: u32;
        core::arch::asm!(
            "rdmsr",
            in("ecx") IA32_EFER,
            out("eax") lo,
            out("edx") hi,
            options(nostack, preserves_flags),
        );
        let efer = (((hi as u64) << 32) | lo as u64) | EFER_NXE;
        core::arch::asm!(
            "wrmsr",
            in("ecx") IA32_EFER,
            in("eax") efer as u32,
            in("edx") (efer >> 32) as u32,
            options(nostack, preserves_flags),
        );
    }

    syscall_entry::set_dispatch_callback(dispatch);

    let Ok(probe) = LoadImage::parse(PROGRAM_RXE, &SYSCALL_TABLE_HASH) else {
        note(TEST_FAIL, "fp isolation: probe rxe failed to parse");
        qemu_exit::exit_failure();
    };
    let Ok(hygiene) = LoadImage::parse(HYGIENE_RXE, &SYSCALL_TABLE_HASH) else {
        note(TEST_FAIL, "fp isolation: hygiene rxe failed to parse");
        qemu_exit::exit_failure();
    };

    // Two probe tasks with distinct one-character seeds, then the hygiene task.
    let (root_a, entry_a) = build_space(&PAGE_TABLE_POOL_A, &probe, PROGRAM_RXE, &[b"A"]);
    let (root_b, entry_b) = build_space(&PAGE_TABLE_POOL_B, &probe, PROGRAM_RXE, &[b"B"]);
    let (root_h, entry_h) = build_space(&PAGE_TABLE_POOL_H, &hygiene, HYGIENE_RXE, &[b"h"]);

    let bsp_id = smp::bsp_lapic_id();
    let cpu_to_lapic: [Option<u8>; 1] = [Some(bsp_id)];
    static ARCH_STORAGE: X86_64ArchStorage<1> = X86_64ArchStorage::new();
    let Ok(arch) = X86_64Arch::new(&ARCH_STORAGE, 0, bsp_id, &cpu_to_lapic) else {
        note(TEST_FAIL, "fp isolation: X86_64Arch::new failed");
        qemu_exit::exit_failure();
    };
    let arch = alloc::sync::Arc::new(arch);
    let Ok(sched) = Scheduler::new(SchedulerConfig::defaults_for(1), arch) else {
        note(TEST_FAIL, "fp isolation: Scheduler::new failed");
        qemu_exit::exit_failure();
    };

    let cs = ContextSwitchHal::new();
    admit(&sched, cs, root_a, entry_a);
    admit(&sched, cs, root_b, entry_b);
    admit(&sched, cs, root_h, entry_h);
    note(
        TEST_SPAWNED,
        "x86_64 fp isolation: three ring-3 tasks spawned",
    );

    let mut steps = 0u64;
    while sched.live_task_count() != 0 && steps < MAX_STEPS {
        let _ = sched.step(BOOT_CPU);
        steps += 1;
    }
    if sched.live_task_count() != 0 {
        note(TEST_FAIL, "fp isolation: deadlock — a task remained");
        qemu_exit::exit_failure();
    }
    if BAD_EXITS.load(Ordering::SeqCst) != 0 {
        note(
            TEST_FAIL,
            "fp isolation: a task saw clobbered or leaked register state",
        );
        qemu_exit::exit_failure();
    }
    if EXITS.load(Ordering::SeqCst) != PROBE_COUNT + 1 {
        note(
            TEST_FAIL,
            "fp isolation: not every task exited exactly once",
        );
        qemu_exit::exit_failure();
    }
    if YIELDS.load(Ordering::SeqCst) != PROBE_COUNT * ROUNDS_PER_TASK {
        note(TEST_FAIL, "fp isolation: wrong total probe yield count");
        qemu_exit::exit_failure();
    }
    if KERNEL_FP_RUNS.load(Ordering::SeqCst) == 0 {
        note(
            TEST_FAIL,
            "fp isolation: the kernel never computed under a probe MXCSR",
        );
        qemu_exit::exit_failure();
    }
    if KERNEL_FP_BAD.load(Ordering::SeqCst) != 0 {
        note(
            TEST_FAIL,
            "fp isolation: a kernel float op did not round to nearest",
        );
        qemu_exit::exit_failure();
    }

    note(
        TEST_PASS,
        "x86_64 fp isolation: files isolated, entry clean, kernel FP sound",
    );
    qemu_exit::exit_success();
}

/// The symbol the arch crate's boot trampoline calls.
#[no_mangle]
pub extern "C" fn kernel_main(multiboot_info: u64) -> ! {
    boot(
        multiboot_info,
        &ALLOCATOR,
        &SERIAL_SINK,
        &AUDIT_SINK,
        tairix_log::Level::Info,
    )
}
