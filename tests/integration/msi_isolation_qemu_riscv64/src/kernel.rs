//! The MSI-isolation vertical's run.

use alloc::vec;
use core::num::NonZeroU16;
use core::panic::PanicInfo;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use tairix_abi::driver::msix::MsiMessage;
use tairix_abi::driver::pci::{function_address, requester_id, PciBus};
use tairix_abi::{MmioMapError, MmioMapper, RegisterWindow};
use tairix_arch_api::{PageTableFrames, TableFrame, PAGE_TABLE_ENTRIES};
use tairix_arch_riscv64::fdt::{supervisor_aia, Fdt};
use tairix_arch_riscv64::imsic::{HartFile, Imsic, InterruptFile};
use tairix_arch_riscv64::{handle_panic_via_serial, qemu_exit, trap, SERIAL_SINK};
use tairix_fdt::pci::{each_pci_host, PciSpace};
use tairix_itest_finisher::fail_point;
use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
use tairix_kernel_iommu_api::{Clock, IommuUnit, Notice, Signalling};
use tairix_kernel_iommu_riscv::RiscvUnit;
use tairix_log::{log, Event, EventId, Level};
use tairix_pci::{Aperture, Apertures, EcamRegion, PciResources, Windows};

/// The boot heap, in `.bss`.
static HEAP: Heap = Heap::ZERO;

/// Global allocator backed by [`HEAP`].
///
/// SAFETY: the page-aligned `HEAP` static outlives the binary and the
/// allocator is its only consumer.
#[global_allocator]
static ALLOCATOR: FreeListAllocator =
    unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

const RUN_START: EventId = EventId(4242);
const RUN_PASS: EventId = EventId(4243);

/// The AIA, the unit or the ECAM host was not found in the tree.
const FAIL_DISCOVERY: NonZeroU16 = fail_point!(1);
/// The hart's file or the unit would not come up.
const FAIL_BRING_UP: NonZeroU16 = fail_point!(2);
/// A function's messages could not be confined.
const FAIL_CONFINE: NonZeroU16 = fail_point!(3);
/// A source's resources or MSI would not be set out.
const FAIL_SOURCE: NonZeroU16 = fail_point!(4);
/// A message raised a notice it was not given, or landed outside its file.
const FAIL_FORGED: NonZeroU16 = fail_point!(5);

/// The two `edu` functions: A in slot 2, B in slot 3 (`tools/qemu`'s
/// `MESSAGE_SOURCE_SLOT`).
const A: (u8, u8, u8) = (0, 2, 0);
const B: (u8, u8, u8) = (0, 3, 0);
const EDU_ID: u32 = 0x11E8_1234;
/// The register a write to raises the function's interrupt.
const EDU_RAISE: usize = 0x60;

/// The identity each function's vector writes into its own file.
const VECTOR: u32 = 1;
/// The notices A's and B's files raise in the hart's.
const A_NOTICE: u32 = 1;
const B_NOTICE: u32 = 2;

/// How often each notice arrived, by identity.
static ARRIVED: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];

static POOL: Pool = Pool {
    bytes: PoolBytes(core::cell::UnsafeCell::new([0; POOL_BYTES])),
    next: AtomicUsize::new(0),
};

const POOL_BYTES: usize = 1 << 20;

/// The memory the unit reads and writes.
#[repr(C, align(65536))]
struct PoolBytes(core::cell::UnsafeCell<[u8; POOL_BYTES]>);

/// Hands out zeroed, size-aligned blocks of [`PoolBytes`], never reused.
struct Pool {
    bytes: PoolBytes,
    next: AtomicUsize,
}

// SAFETY: the cursor hands each block out once, so no two holders share
// bytes; translation off, a block's address is its physical address.
unsafe impl Sync for Pool {}

impl Pool {
    fn base(&self) -> *mut u8 {
        self.bytes.0.get().cast()
    }
}

impl PageTableFrames for Pool {
    fn alloc_table(&self) -> Option<TableFrame> {
        let phys = self.alloc_block(0)?;
        let entries = self.table_at(phys)?;
        // SAFETY: a fresh block of 512 zeroed words that only this frame
        // names.
        Some(TableFrame {
            phys,
            entries: unsafe { &mut *entries },
        })
    }
    fn table_at(&self, phys: u64) -> Option<*mut [u64; PAGE_TABLE_ENTRIES]> {
        self.block_at(phys, 0).map(<*mut u64>::cast)
    }
    fn free_table(&self, _phys: u64) {}
    fn alloc_block(&self, order: u32) -> Option<u64> {
        let size = 4096usize << order;
        let base = self.base() as usize;
        let mut taken = self.next.load(Ordering::Acquire);
        loop {
            let start = (base + taken).next_multiple_of(size) - base;
            let end = start.checked_add(size).filter(|&end| end <= POOL_BYTES)?;
            match self
                .next
                .compare_exchange(taken, end, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return Some((base + start) as u64),
                Err(now) => taken = now,
            }
        }
    }
    fn block_at(&self, phys: u64, order: u32) -> Option<*mut u64> {
        let offset = usize::try_from(phys.checked_sub(self.base() as u64)?).ok()?;
        (offset + (4096 << order) <= POOL_BYTES).then(|| self.base().wrapping_add(offset).cast())
    }
    fn free_block(&self, _phys: u64, _order: u32) {}
}

/// Registers at their physical address: translation is off.
struct Physical;

impl MmioMapper for Physical {
    fn map_window(&self, phys_base: u64, len: usize) -> Result<RegisterWindow, MmioMapError> {
        let base = usize::try_from(phys_base)
            .ok()
            .and_then(|base| NonNull::new(base as *mut u8))
            .ok_or(MmioMapError::InvalidRegion)?;
        // SAFETY: translation off, `len` bytes at `phys_base` are the
        // device's registers, and this run alone drives them.
        Ok(unsafe { RegisterWindow::from_mapping(phys_base, base, len) })
    }
}

/// The hart's `time` counter, at the tree's timebase.
struct Time(u64);

impl Clock for Time {
    fn now_ns(&self) -> u64 {
        let ticks: u64;
        // SAFETY: reading `time` has no side effects.
        unsafe { core::arch::asm!("rdtime {}", out(reg) ticks, options(nomem, nostack)) };
        u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(self.0.max(1)))
            .unwrap_or(u64::MAX)
    }
}

/// Count every identity the hart's file raised.
extern "C" fn dispatch() {
    loop {
        let identity = HartFile.claim();
        let Some(counter) = ARRIVED.get(identity as usize).filter(|_| identity != 0) else {
            break;
        };
        counter.fetch_add(1, Ordering::AcqRel);
    }
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

fn fail(code: NonZeroU16, why: &'static str) -> ! {
    note(RUN_START, why);
    qemu_exit::exit_failure(code)
}

fn arrived(notice: u32) -> u32 {
    ARRIVED[notice as usize].load(Ordering::Acquire)
}

/// Park until `notice` has arrived `count` times. The harness's budget bounds
/// a notice that never arrives.
fn await_notice(notice: u32, count: u32) {
    while arrived(notice) < count {
        // SAFETY: `wfi` parks until the next interrupt.
        unsafe { core::arch::asm!("wfi", options(nomem, nostack, preserves_flags)) };
    }
}

/// One function's file: a pending doubleword then an enable one per 64
/// identities, at its physical address.
struct File(*const AtomicU64);

impl File {
    fn pending(&self) -> u64 {
        // SAFETY: the file's first doubleword, in a pool block the run holds
        // for good.
        unsafe { (*self.0).load(Ordering::Acquire) }
    }

    fn enable(&self, identity: u32) {
        // SAFETY: as `pending`, the second doubleword.
        unsafe { (*self.0.add(1)).fetch_or(1 << identity, Ordering::AcqRel) };
    }
}

/// Boot entry point, called by the arch trampoline with the SBI hand-off.
#[no_mangle]
pub extern "C" fn kernel_main(hartid: u64, dtb: u64) -> ! {
    note(
        RUN_START,
        "msi isolation: confining two devices' messages to files of their own",
    );
    // SAFETY: the device tree OpenSBI handed the boot hart, immutable for the
    // run.
    let Ok(fdt) = (unsafe { Fdt::from_ptr(dtb as *const u8) }) else {
        fail(FAIL_DISCOVERY, "no tree");
    };
    let Some(aia) = supervisor_aia(&fdt, hartid) else {
        fail(FAIL_DISCOVERY, "no aia");
    };
    let Some(_file) = Imsic::take(HartFile, aia.imsic.ids) else {
        fail(FAIL_BRING_UP, "the hart's file refused");
    };
    if trap::set_trap_dispatch(dispatch).is_err() {
        fail(FAIL_BRING_UP, "dispatcher already installed");
    }
    // SAFETY: the boot hart, its stack set up by the trampoline, the
    // dispatcher installed before any identity can be raised.
    unsafe { trap::init_traps() };

    let unit = fdt
        .operational_nodes()
        .map_while(Result::ok)
        .find(|node| node.is_compatible("riscv,iommu"))
        .and_then(|node| {
            let reg = node.property("reg")?;
            Some((reg.read_be_u64(0).ok()?, reg.read_be_u64(8).ok()?))
        });
    let Some(regs) =
        unit.and_then(|(base, len)| Physical.map_window(base, usize::try_from(len).ok()?).ok())
    else {
        fail(FAIL_DISCOVERY, "no unit");
    };
    let clock: &'static Time = alloc::boxed::Box::leak(alloc::boxed::Box::new(Time(
        fdt.timebase_frequency().unwrap_or(10_000_000),
    )));
    let Ok(unit) = RiscvUnit::new(regs, &POOL, None, clock, Signalling::Wired) else {
        fail(FAIL_BRING_UP, "unit refused");
    };
    let Some(files) = unit.message_files() else {
        fail(FAIL_BRING_UP, "the unit confines no message");
    };

    let mut host = None;
    each_pci_host(&fdt, |found| host = host.or(Some(found)));
    let Some(host) = host else {
        fail(FAIL_DISCOVERY, "no ecam host");
    };
    let stream_of = |(bus, device, function): (u8, u8, u8)| {
        let rid = u32::from(function_address(bus, device, function).map(requester_id)?);
        host.iommu_map().ok()??.map(rid).map(|(_, stream)| stream)
    };
    let (Some(a_stream), Some(b_stream)) = (stream_of(A), stream_of(B)) else {
        fail(FAIL_DISCOVERY, "the host's iommu-map names no stream");
    };
    let Some(block) = POOL.alloc_block(0) else {
        fail(FAIL_BRING_UP, "no file memory");
    };
    let (a_file, b_file) = (block, block + 512);
    let page = aia.imsic.page;
    let confined = [(a_stream, a_file, A_NOTICE), (b_stream, b_file, B_NOTICE)]
        .into_iter()
        .all(|(stream, file, notice)| {
            let notice = Notice {
                address: page,
                data: notice,
            };
            files.confine_messages(stream, page, file, notice).is_ok()
        });
    let domain = unit.create_domain();
    let attached = domain.is_ok_and(|domain| {
        unit.attach(a_stream, domain).is_ok()
            && unit.attach(b_stream, domain).is_ok()
            && unit.enable().is_ok()
    });
    if !confined || !attached {
        fail(FAIL_CONFINE, "a function's messages could not be confined");
    }
    let (a_view, b_view) = (
        File(a_file as *const AtomicU64),
        File(b_file as *const AtomicU64),
    );
    a_view.enable(VECTOR);
    b_view.enable(VECTOR);

    let Ok(ecam) = usize::try_from(host.ecam.1)
        .map_err(|_| MmioMapError::InvalidRegion)
        .and_then(|len| Physical.map_window(host.ecam.0, len))
    else {
        fail(FAIL_DISCOVERY, "ecam unmapped");
    };
    let Some(window) = host
        .windows()
        .find(|window| window.space == PciSpace::Memory32)
    else {
        fail(FAIL_DISCOVERY, "no memory window");
    };
    let span = window.pci..window.pci + window.size;
    let bus = tairix_pci::mechanism_ecam(
        vec![EcamRegion::new(ecam, host.buses.0..=host.buses.1)],
        Apertures::new(
            vec![Aperture {
                pci: span.clone(),
                cpu: window.cpu,
            }],
            vec![],
        ),
    );
    let windows = Windows {
        memory: Some(span),
        ..Windows::default()
    };
    if bus.assign(host.buses.0..=host.buses.1, &windows).is_err() {
        fail(FAIL_SOURCE, "resources unassigned");
    }
    let source = |function: (u8, u8, u8)| {
        let bdf = function_address(function.0, function.1, function.2)?;
        (bus.read_config(bdf, 0).ok()? == EDU_ID).then_some(())?;
        bus.enable_memory_space(bdf).ok()?;
        bus.set_bus_master(bdf, true).ok()?;
        Some((bdf, bus.map_bar_window(bdf, 0, &Physical).ok()?))
    };
    let (Some((a_bdf, a_regs)), Some((b_bdf, b_regs))) = (source(A), source(B)) else {
        fail(FAIL_SOURCE, "an edu source is missing");
    };
    let raise = |bdf: u64, registers: &RegisterWindow, identity: u32| {
        let message = MsiMessage {
            address: page,
            data: identity,
        };
        if bus.route_msi(bdf, message).is_err() || registers.write_u32(EDU_RAISE, 1).is_err() {
            fail(FAIL_SOURCE, "a message could not be raised");
        }
    };

    // A writes B's notice identity, then its own vector: the second's notice
    // orders the first's landing before it.
    raise(a_bdf, &a_regs, B_NOTICE);
    raise(a_bdf, &a_regs, VECTOR);
    await_notice(A_NOTICE, 1);
    if arrived(B_NOTICE) != 0 {
        fail(FAIL_FORGED, "a's message raised b's notice");
    }
    if a_view.pending() & (1 << B_NOTICE) == 0 || b_view.pending() != 0 {
        fail(FAIL_FORGED, "a's message landed outside its own file");
    }
    // B's own vector raises the notice A could not: it is live.
    raise(b_bdf, &b_regs, VECTOR);
    await_notice(B_NOTICE, 1);
    if arrived(B_NOTICE) != 1 || arrived(A_NOTICE) != 1 {
        fail(FAIL_FORGED, "a notice arrived that no message was given");
    }
    note(
        RUN_PASS,
        "msi isolation: a's message naming b's notice stayed in a's file",
    );
    qemu_exit::exit_success()
}

/// A panic halts the guest; the run times out and fails loud.
#[panic_handler]
fn tairix_msi_isolation_qemu_riscv64_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_serial(info)
}
