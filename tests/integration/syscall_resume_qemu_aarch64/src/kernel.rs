//! Freestanding kernel for the deterministic syscall-continuation witness.

use core::num::NonZeroU16;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use tairix_arch_api::AddressTag;

use alloc::sync::Arc;

use tairix_abi::rxe::LoadImage;
use tairix_abi::{CapabilityId, CapabilityQuery, SyscallNumber, SYSCALL_MAX_ARGS};
use tairix_arch_aarch64::context_hal::ContextSwitchHal;
use tairix_arch_aarch64::kernel_arch::timer_frequency_hz;
use tairix_arch_aarch64::paging::{
    self, activate_user_root, AddressSpace as ArchAddressSpace, PageTablePool,
};
use tairix_arch_aarch64::userentry::USER_MODE;
use tairix_arch_aarch64::{
    exceptions, gic, handle_panic_via_serial, qemu_exit, syscall_entry, SERIAL_SINK,
};
use tairix_arch_api::BOOT_CPU;
use tairix_fdt::Fdt;
use tairix_itest_finisher::fail_point;
use tairix_kalloc::{Arena, FreeListAllocator};
use tairix_kernel::aarch64::spawn_producer::SPAWN_TABLE_PHYSMAP;
use tairix_kernel_core::UserThreadEntry;
use tairix_kernel_core::{
    note_preempt_tick, reschedule_current, spawn_image, spawn_user_kthread, take_preempt_pending,
    RescheduleAction, SpawnMode, SpawnRequest, Yielder,
};
use tairix_kernel_mem::{AddressSpace, Frame, PhysAddr, UserStack};
use tairix_kernel_sched_eevdf::{Priority, Scheduler, SchedulerConfig};
use tairix_kernel_syscall::SYSCALL_TABLE_HASH;
use tairix_log::{log, Event, EventId, Level};

include!(concat!(env!("OUT_DIR"), "/program_rxe.rs"));
include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));

const USER_STACK_BASE: u64 = USER_BIAS + 0x10_0000;
const USER_STACK_PAGES: u64 = 64;
const USER_BLOCK_BASE: u64 = USER_BIAS + 0x30_0000;
const CANARY: u64 = 0x51a7_c011_71a0_ca5e;
const EXPECTED_READING: u64 = 0x51a7_c011_71a0_0001;
const FRAME_COUNT: usize = 256;
/// The test's heap, in bytes.
const HEAP_SIZE: usize = 2 * 1024 * 1024;
const MAX_STEPS: u64 = 1_000_000;

const TEST_START: EventId = EventId(4293);
const TEST_PASS: EventId = EventId(4294);

const FAIL_FDT: NonZeroU16 = fail_point!(1);
const FAIL_PARSE: NonZeroU16 = fail_point!(2);
const FAIL_POOL: NonZeroU16 = fail_point!(3);
const FAIL_BUILD: NonZeroU16 = fail_point!(4);
const FAIL_SCHEDULER: NonZeroU16 = fail_point!(5);
const FAIL_PARENT: NonZeroU16 = fail_point!(6);
const FAIL_CHILD: NonZeroU16 = fail_point!(7);
const FAIL_UNEXPECTED_SYSCALL: NonZeroU16 = fail_point!(8);
const FAIL_RESCHEDULE: NonZeroU16 = fail_point!(9);
const FAIL_ORDER: NonZeroU16 = fail_point!(10);
const FAIL_DEADLOCK: NonZeroU16 = fail_point!(11);
const FAIL_RESULT: NonZeroU16 = fail_point!(12);

static CLOCK_CALLS: AtomicU64 = AtomicU64::new(0);
static PARENT_EXIT_STATUS: AtomicU64 = AtomicU64::new(u64::MAX);
static EXIT_CALLS: AtomicU64 = AtomicU64::new(0);
static CHILD_PARK_ENTERED: AtomicBool = AtomicBool::new(false);
static CHILD_RESUMED: AtomicBool = AtomicBool::new(false);

/// The global allocator's arena.
static HEAP: Arena<HEAP_SIZE> = Arena::ZERO;

/// SAFETY: the page-aligned static outlives the kernel and this allocator is
/// its sole owner.
#[global_allocator]
static ALLOCATOR: FreeListAllocator =
    unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_SIZE) };

static PARENT_PAGE_TABLES: PageTablePool = PageTablePool::new();
static CHILD_PAGE_TABLES: PageTablePool = PageTablePool::new();

static FRAME_POOL: Arena<{ paging::PAGE_SIZE * FRAME_COUNT }> = Arena::ZERO;
static FRAME_CURSOR: AtomicUsize = AtomicUsize::new(0);

fn next_frame() -> Option<Frame> {
    let index = FRAME_CURSOR.fetch_add(1, Ordering::SeqCst);
    if index >= FRAME_COUNT {
        FRAME_CURSOR.store(FRAME_COUNT, Ordering::SeqCst);
        return None;
    }
    let pool = paging::image_phys(FRAME_POOL.as_mut_ptr() as u64);
    let base = pool + (index * paging::PAGE_SIZE) as u64;
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

#[panic_handler]
fn syscall_resume_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_serial(info)
}

struct SpawnAuthority;

impl CapabilityQuery for SpawnAuthority {
    fn holds(&self, cap: CapabilityId) -> bool {
        cap == CapabilityId::PROC_SPAWN
    }
}

extern "C" fn dispatch(number: u64, args_ptr: *const [u64; SYSCALL_MAX_ARGS]) -> u64 {
    let call = SyscallNumber::from_register(number).ok();
    if call == Some(SyscallNumber::CLOCK_GET) {
        match CLOCK_CALLS.fetch_add(1, Ordering::SeqCst) {
            0 => {
                note_preempt_tick(BOOT_CPU);
                if !take_preempt_pending(BOOT_CPU)
                    || !reschedule_current(BOOT_CPU, RescheduleAction::Yield)
                {
                    qemu_exit::exit_failure(FAIL_RESCHEDULE);
                }
                if !CHILD_PARK_ENTERED.load(Ordering::SeqCst)
                    || CHILD_RESUMED.load(Ordering::SeqCst)
                {
                    qemu_exit::exit_failure(FAIL_ORDER);
                }
            }
            1 => {
                CHILD_PARK_ENTERED.store(true, Ordering::SeqCst);
                if !reschedule_current(BOOT_CPU, RescheduleAction::Park) {
                    qemu_exit::exit_failure(FAIL_RESCHEDULE);
                }
                CHILD_RESUMED.store(true, Ordering::SeqCst);
            }
            _ => qemu_exit::exit_failure(FAIL_UNEXPECTED_SYSCALL),
        }
        EXPECTED_READING
    } else if call == Some(SyscallNumber::EXIT) {
        // SAFETY: the architecture syscall trampoline passes a live pointer to
        // its fixed-size copied argument array for the duration of this call.
        let status = unsafe { (*args_ptr)[0] };
        let exit_index = EXIT_CALLS.fetch_add(1, Ordering::SeqCst);
        if exit_index == 0 {
            if !CHILD_PARK_ENTERED.load(Ordering::SeqCst) || CHILD_RESUMED.load(Ordering::SeqCst) {
                qemu_exit::exit_failure(FAIL_ORDER);
            }
            PARENT_EXIT_STATUS.store(status, Ordering::SeqCst);
        } else if exit_index > 1 {
            qemu_exit::exit_failure(FAIL_UNEXPECTED_SYSCALL);
        }
        if !reschedule_current(BOOT_CPU, RescheduleAction::Exit) {
            qemu_exit::exit_failure(FAIL_RESCHEDULE);
        }
        0
    } else {
        qemu_exit::exit_failure(FAIL_UNEXPECTED_SYSCALL)
    }
}

fn build_el0_space(
    image: &LoadImage,
    page_tables: &'static PageTablePool,
) -> (u64, tairix_arch_api::UserEntry) {
    let Some(arch) = ArchAddressSpace::new_user(page_tables) else {
        qemu_exit::exit_failure(FAIL_POOL);
    };
    let root_phys = arch.root_phys();
    // SAFETY: activating a root of this port loads `TTBR0_EL1` alone; the
    // kernel's code, stack and registers stay mapped through `TTBR1_EL1`.
    unsafe { tairix_arch_api::mmu::AddressSpace::activate(&arch) };
    let mut space = AddressSpace::new(arch);
    let request = SpawnRequest {
        image,
        image_bytes: PROGRAM_RXE,
        bias: USER_BIAS,
        stack: UserStack {
            base: USER_STACK_BASE,
            page_count: USER_STACK_PAGES,
        },
        start_block_base: USER_BLOCK_BASE,
        args: &[b"syscall-resume"],
        env: &[],
        canary: CANARY,
        heap: tairix_abi::HeapRegion {
            base: USER_HEAP_BASE,
            len: USER_HEAP_LEN,
        },
    };
    // SAFETY: the returned entry is used only after its retained page-table
    // root is activated by the user kthread's pre-resume hook.
    let entry = match unsafe {
        spawn_image(
            &SpawnAuthority,
            SpawnMode::General,
            &SERIAL_SINK,
            &mut space,
            &SPAWN_TABLE_PHYSMAP,
            &request,
            next_frame,
        )
    } {
        Ok(entry) => entry,
        Err(_) => qemu_exit::exit_failure(FAIL_BUILD),
    };
    (root_phys, entry)
}

#[no_mangle]
pub extern "C" fn kernel_main(_dtb: u64) -> ! {
    note(TEST_START, "aarch64 syscall continuation test: starting");

    let Ok(fdt) = Fdt::new(DTB_BLOB) else {
        qemu_exit::exit_failure(FAIL_FDT);
    };
    let counter_hz = timer_frequency_hz(&fdt);
    if counter_hz == 0 || gic::configure_from_fdt(&fdt).is_none() {
        qemu_exit::exit_failure(FAIL_FDT);
    }
    let Ok(image) = LoadImage::parse(PROGRAM_RXE, &SYSCALL_TABLE_HASH) else {
        qemu_exit::exit_failure(FAIL_PARSE);
    };
    let (parent_root, parent_entry) = build_el0_space(&image, &PARENT_PAGE_TABLES);
    let (child_root, child_entry) = build_el0_space(&image, &CHILD_PAGE_TABLES);

    // SAFETY: one boot CPU, with MMU and stack established.
    unsafe {
        exceptions::init_vectors();
        tairix_itest_gic::init_boot_cpu().expect("the GIC comes up");
    }
    syscall_entry::set_dispatch_callback(dispatch);

    static ARCH_STORAGE: tairix_arch_aarch64::Aarch64ArchStorage<1> =
        tairix_arch_aarch64::Aarch64ArchStorage::new();
    let arch = Arc::new(tairix_arch_aarch64::Aarch64Arch::new(
        &ARCH_STORAGE,
        BOOT_CPU,
        counter_hz,
    ));
    if tairix_kernel_core::initialize_cpu_state(1).is_err() {
        qemu_exit::exit_failure(FAIL_SCHEDULER);
    }
    let Ok(scheduler) = Scheduler::new(SchedulerConfig::defaults_for(1), arch) else {
        qemu_exit::exit_failure(FAIL_SCHEDULER);
    };

    let context_switch = ContextSwitchHal::new();
    let parent_pre_resume = move |_stack_top: u64, tag: AddressTag| {
        // SAFETY: `parent_root` is the retained L1 root built above, and the
        // kernel runs from `TTBR1_EL1`.
        unsafe { activate_user_root(parent_root, tag) };
    };
    let parent = spawn_user_kthread(
        &scheduler,
        context_switch,
        BOOT_CPU,
        Priority::Normal,
        parent_pre_resume,
        move |yielder: &mut Yielder<ContextSwitchHal>| {
            // SAFETY: the parent space is active and vectors plus the syscall
            // callback are installed, so both its clock and exit traps return
            // through this kernel.
            unsafe {
                UserThreadEntry {
                    port: &USER_MODE,
                    regs: parent_entry,
                }
                .enter(yielder)
            }
        },
    );
    if parent.is_err() {
        qemu_exit::exit_failure(FAIL_PARENT);
    }

    let child_pre_resume = move |_stack_top: u64, tag: AddressTag| {
        // SAFETY: `child_root` is the retained L1 root built above, and the
        // kernel runs from `TTBR1_EL1`.
        unsafe { activate_user_root(child_root, tag) };
    };
    let child = match spawn_user_kthread(
        &scheduler,
        context_switch,
        BOOT_CPU,
        Priority::Normal,
        child_pre_resume,
        move |yielder: &mut Yielder<ContextSwitchHal>| {
            // SAFETY: the child space is active and its ordinary syscall is
            // deliberately parked by `dispatch` before returning.
            unsafe {
                UserThreadEntry {
                    port: &USER_MODE,
                    regs: child_entry,
                }
                .enter(yielder)
            }
        },
    ) {
        Ok(id) => id,
        Err(_) => qemu_exit::exit_failure(FAIL_CHILD),
    };

    let mut child_woken = false;
    let mut steps = 0u64;
    while scheduler.live_task_count() != 0 && steps < MAX_STEPS {
        let _ = scheduler.step(BOOT_CPU);
        if !child_woken && PARENT_EXIT_STATUS.load(Ordering::SeqCst) != u64::MAX {
            if scheduler.unpark(child).is_err() {
                qemu_exit::exit_failure(FAIL_CHILD);
            }
            child_woken = true;
        }
        steps += 1;
    }

    if scheduler.live_task_count() != 0 {
        qemu_exit::exit_failure(FAIL_DEADLOCK);
    }
    if CLOCK_CALLS.load(Ordering::SeqCst) != 2
        || EXIT_CALLS.load(Ordering::SeqCst) != 2
        || PARENT_EXIT_STATUS.load(Ordering::SeqCst) != 0
        || !CHILD_PARK_ENTERED.load(Ordering::SeqCst)
        || !CHILD_RESUMED.load(Ordering::SeqCst)
    {
        qemu_exit::exit_failure(FAIL_RESULT);
    }

    note(
        TEST_PASS,
        "aarch64 syscall continuation test: parent resumed after child park",
    );
    qemu_exit::exit_success();
}
