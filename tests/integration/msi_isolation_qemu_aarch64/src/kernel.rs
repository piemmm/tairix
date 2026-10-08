//! The MSI-isolation vertical's run.

use alloc::vec;
use core::num::NonZeroU16;
use core::panic::PanicInfo;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use tairix_abi::driver::msix::MsiMessage;
use tairix_abi::driver::pci::{function_address, requester_id, PciBus};
use tairix_abi::{MmioMapError, MmioMapper, RegisterWindow};
use tairix_arch_aarch64::gicv3::{LpiTables, FIRST_LPI};
use tairix_arch_aarch64::its::{Its, ItsRoute, VolatileItsMmio, TRANSLATER};
use tairix_arch_aarch64::{exceptions, gic, handle_panic_via_serial, qemu_exit, SERIAL_SINK};
use tairix_arch_api::{PageTableFrames, TableFrame, PAGE_TABLE_ENTRIES};
use tairix_fdt::pci::{each_pci_host, PciSpace};
use tairix_fdt::Fdt;
use tairix_itest_finisher::fail_point;
use tairix_kalloc::{FreeListAllocator, Heap, HEAP_BYTES};
use tairix_log::{log, Event, EventId, Level};
use tairix_pci::{Aperture, Apertures, EcamRegion, PciResources, Windows};

use crate::tree::DTB_BLOB;

/// The boot heap, in `.bss`.
static HEAP: Heap = Heap::ZERO;

/// Global allocator backed by [`HEAP`].
///
/// SAFETY: the page-aligned `HEAP` static outlives the binary and the
/// allocator is its only consumer.
#[global_allocator]
static ALLOCATOR: FreeListAllocator =
    unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

const RUN_START: EventId = EventId(4240);
const RUN_PASS: EventId = EventId(4241);

/// The GIC, its ITS or the ECAM host was not found in the tree.
const FAIL_DISCOVERY: NonZeroU16 = fail_point!(1);
/// The GIC, its LPIs or the ITS would not come up.
const FAIL_BRING_UP: NonZeroU16 = fail_point!(2);
/// The routes would not map.
const FAIL_MAP: NonZeroU16 = fail_point!(3);
/// A source's resources or MSI would not be set out.
const FAIL_SOURCE: NonZeroU16 = fail_point!(4);
/// A message raised an LPI it was not given.
const FAIL_FORGED: NonZeroU16 = fail_point!(5);

/// The two `edu` functions: A in slot 2, B in slot 3 (`tools/qemu`'s
/// `MESSAGE_SOURCE_SLOT`).
const A: (u8, u8, u8) = (0, 2, 0);
const B: (u8, u8, u8) = (0, 3, 0);
const EDU_ID: u32 = 0x11E8_1234;
/// The register a write to raises the function's interrupt.
const EDU_RAISE: usize = 0x60;

/// The LPIs mapped: A's event 0, then B's events 0 and 1.
const A_OWN: u32 = FIRST_LPI;
const B_FIRST: u32 = FIRST_LPI + 1;
const B_SECOND: u32 = FIRST_LPI + 2;
/// The event A forges: one only B was given.
const FORGED_EVENT: u32 = 1;

/// How often each mapped LPI arrived, by its offset from [`FIRST_LPI`].
static ARRIVED: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];

static POOL: Pool = Pool {
    bytes: PoolBytes(core::cell::UnsafeCell::new([0; POOL_BYTES])),
    next: AtomicUsize::new(0),
};

const POOL_BYTES: usize = 1 << 20;

/// The memory the ITS and the redistributor read.
#[repr(C, align(65536))]
struct PoolBytes(core::cell::UnsafeCell<[u8; POOL_BYTES]>);

/// Hands out zeroed, size-aligned blocks of [`PoolBytes`], never reused.
struct Pool {
    bytes: PoolBytes,
    next: AtomicUsize,
}

// SAFETY: the cursor hands each block out once, so no two holders share
// bytes; MMU off, a block's address is its physical address.
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

/// Registers at their physical address: the MMU is off.
struct Physical;

impl MmioMapper for Physical {
    fn map_window(&self, phys_base: u64, len: usize) -> Result<RegisterWindow, MmioMapError> {
        let base = usize::try_from(phys_base)
            .ok()
            .and_then(|base| NonNull::new(base as *mut u8))
            .ok_or(MmioMapError::InvalidRegion)?;
        // SAFETY: MMU off, `len` bytes at `phys_base` are the device's
        // registers, reached as Device memory, and this run alone drives
        // them.
        Ok(unsafe { RegisterWindow::from_mapping(phys_base, base, len) })
    }
}

/// Count each mapped LPI's arrival.
extern "C" fn dispatch(intid: u32) {
    if let Some(counter) = intid
        .checked_sub(FIRST_LPI)
        .and_then(|index| ARRIVED.get(index as usize))
    {
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

fn arrived(lpi: u32) -> u32 {
    ARRIVED[(lpi - FIRST_LPI) as usize].load(Ordering::Acquire)
}

/// Park until `lpi` has arrived `count` times. The harness's budget bounds a
/// message that never arrives.
fn await_lpi(lpi: u32, count: u32) {
    while arrived(lpi) < count {
        // SAFETY: `wfi` parks until the next interrupt.
        unsafe { core::arch::asm!("wfi", options(nomem, nostack, preserves_flags)) };
    }
}

/// Boot entry point, called by the arch trampoline.
#[no_mangle]
pub extern "C" fn kernel_main(_dtb: u64) -> ! {
    note(
        RUN_START,
        "msi isolation: mapping two devices' events through the its",
    );
    let Ok(fdt) = Fdt::new(DTB_BLOB) else {
        fail(FAIL_DISCOVERY, "no tree");
    };
    if gic::configure_from_fdt(&fdt).is_none() {
        fail(FAIL_DISCOVERY, "no gic");
    }
    let Some(topology) = tairix_itest_gic::boot_cpu_topology(&fdt) else {
        fail(FAIL_DISCOVERY, "no redistributors");
    };
    if exceptions::set_device_irq_dispatch(dispatch).is_err() {
        fail(FAIL_BRING_UP, "dispatcher already installed");
    }
    // SAFETY: once, on the boot CPU, its vectors armed by the trampoline and
    // the dispatcher installed before any source is armed.
    if unsafe { gic::init(topology) }.is_err() {
        fail(FAIL_BRING_UP, "gic refused");
    }
    let Some(tables) = LpiTables::allocate(&POOL, 14, 3) else {
        fail(FAIL_BRING_UP, "no lpi tables");
    };
    // SAFETY: once, for this CPU's redistributor; the tables are its own.
    if unsafe { gic::enable_lpis(0, tables) }.is_err() {
        fail(FAIL_BRING_UP, "lpis refused");
    }
    let mut its_base = None;
    gic::for_each_its(&fdt, |_, base, _| its_base = its_base.or(Some(base)));
    let Some(its_base) = its_base.and_then(|base| usize::try_from(base).ok()) else {
        fail(FAIL_DISCOVERY, "no its");
    };
    // SAFETY: the tree's service beneath the GIC, Device memory with the MMU
    // off, which this run alone drives.
    let Ok(mut its) = Its::new(unsafe { VolatileItsMmio::new(its_base) }).take_over(&POOL, 1)
    else {
        fail(FAIL_BRING_UP, "its refused");
    };
    let Ok(target) = gic::collection_target(0, its.features().physical_targets) else {
        fail(FAIL_BRING_UP, "no collection target");
    };

    let mut host = None;
    each_pci_host(&fdt, |found| host = host.or(Some(found)));
    let Some(host) = host else {
        fail(FAIL_DISCOVERY, "no ecam host");
    };
    let rid = |(bus, device, function): (u8, u8, u8)| {
        function_address(bus, device, function).map(requester_id)
    };
    let device_of = |function| {
        rid(function)
            .and_then(|rid| host.msi_target(u32::from(rid)).ok().flatten())
            .map(|(_, device)| device)
    };
    let (Some(a), Some(b)) = (device_of(A), device_of(B)) else {
        fail(FAIL_DISCOVERY, "the host's msi-map names no device id");
    };
    let mut routes = [
        ItsRoute {
            device: a,
            event: 0,
            lpi: A_OWN,
        },
        ItsRoute {
            device: b,
            event: 0,
            lpi: B_FIRST,
        },
        ItsRoute {
            device: b,
            event: FORGED_EVENT,
            lpi: B_SECOND,
        },
    ];
    if its.map(0, target, 14, &mut routes).is_err() {
        fail(FAIL_MAP, "routes refused");
    }

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
    let raise = |bdf: u64, registers: &RegisterWindow, event: u32| {
        let message = MsiMessage {
            address: its_base as u64 + TRANSLATER,
            data: event,
        };
        if bus.route_msi(bdf, message).is_err() || registers.write_u32(EDU_RAISE, 1).is_err() {
            fail(FAIL_SOURCE, "a message could not be raised");
        }
    };
    // SAFETY: the dispatcher is installed and every mapped LPI enabled.
    unsafe { exceptions::enable_irq() };

    // A writes the event only B was given, then its own: the second's
    // arrival orders the first's translation before it.
    raise(a_bdf, &a_regs, FORGED_EVENT);
    raise(a_bdf, &a_regs, 0);
    await_lpi(A_OWN, 1);
    if arrived(B_SECOND) != 0 || arrived(B_FIRST) != 0 {
        fail(FAIL_FORGED, "a's forged event raised b's lpi");
    }
    // B's own event 1 raises the LPI A could not: it is live.
    raise(b_bdf, &b_regs, FORGED_EVENT);
    await_lpi(B_SECOND, 1);
    if arrived(B_SECOND) != 1 || arrived(B_FIRST) != 0 || arrived(A_OWN) != 1 {
        fail(FAIL_FORGED, "an lpi arrived that no message was given");
    }
    note(RUN_PASS, "msi isolation: the its dropped a's forged event");
    qemu_exit::exit_success()
}

/// A panic halts the guest; the run times out and fails loud.
#[panic_handler]
fn tairix_msi_isolation_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_serial(info)
}
