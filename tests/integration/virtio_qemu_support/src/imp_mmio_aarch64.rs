//! Freestanding (`aarch64-unknown-none`) virtio-MMIO bring-up for the
//! QEMU `virt` board — the EL1 / GICv2 analogue of [`crate::imp_mmio`]
//! (riscv64).
//!
//! The device-agnostic lifecycle and the per-device tails live in
//! [`crate::common`]; this module owns only the aarch64-specific bring-up
//! that produces an [`MmioTransport`] and an interrupt path: build the
//! `virt`-board virtio-MMIO bus from the published device tree, provision
//! the transport through the `CAP_MMIO_MAP`-gated [`KernelMmioMapper`],
//! walk the DTB for the device's GICv2 SPI, route + arm it on the
//! [`GicController`], wire the EL1 device-IRQ dispatch to an [`IrqTable`],
//! and park on a race-free `wfi`.
//!
//! Unlike riscv64 (which runs the full boot pipeline so the boot info
//! publishes a usable memory map), this vertical owns its DMA frames in a
//! static pool: a page-aligned `.bss` `Arena`, exclusively ours and
//! reached at its physical address through the kernel's direct map.

extern crate alloc;

use alloc::boxed::Box;
use core::num::NonZeroU16;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};

use tairix_abi::{CapabilityId, DmaCoherence, IrqHandle};
use tairix_arch_aarch64::gic::{self, ActiveGic, GicController, MAX_INTID, MIN_SPI_INTID};
use tairix_arch_aarch64::{exceptions, paging, qemu_exit, SERIAL_SINK};
use tairix_caps::CapabilitySet;
use tairix_drv_bus_mmio::virtio_mmio_bus_from_dtb;
use tairix_drv_bus_virtio::MmioTransport;
use tairix_fdt::Fdt;
use tairix_kalloc::{Arena, FreeListAllocator};
use tairix_kernel::aarch64::boot::DeviceWindows;
use tairix_kernel::aarch64::spawn_producer::SPAWN_TABLE_PHYSMAP;
use tairix_kernel_irq::{IrqController, IrqTable, IrqWaitAbort, IrqWaiter, MaskError};
use tairix_kernel_mem::bootinfo::{BootMemoryMap, MemoryRegion, RegionKind};
use tairix_kernel_mem::{
    AddressSpace, DmaPool, FrameAllocator, HostPageTable, MmioMap, PhysAddr, VirtAddr, PAGE_SIZE,
};
use tairix_kernel_sec::captable::{ProcessId, TaskCapabilities};
use tairix_kernel_sec::identity::UserId;
use tairix_kernel_virtio::{
    first_virtio_slot, provision_virtio_mmio, KernelMmioMapper, KernelVirtioHost,
};
use tairix_log::{Event, EventId, Level, Sink};
use tairix_virtio::transport_mmio::{regs, COMPATIBLE};
use tairix_virtio::{PoolId, VirtioHost};

use crate::common::{drive_driver_lifecycle, FixtureLine, QemuEnv, ScenarioConfig};
use tairix_itest_finisher::fail_point;

/// Re-export so the verticals name the concrete transport for the shared
/// device-tail turbofish under the same name as the riscv64 / PCI
/// verticals.
pub use tairix_drv_bus_virtio::MmioTransport as ScenarioTransport;

// Re-exports the `define_mmio_boot_harness_aarch64!` macro expands
// against via `$crate::...`.
#[doc(hidden)]
pub use tairix_arch_aarch64::handle_panic_via_serial;
#[doc(hidden)]
pub use tairix_arch_aarch64::{
    SerialSink as HarnessSerialSink, SERIAL_SINK as HARNESS_SERIAL_SINK,
};

// --- Global allocator (static `.bss` bump heap) ----------------------

/// The test's heap, in bytes.
const HEAP_SIZE: usize = 8 * 1024 * 1024;

/// The global allocator's arena.
static HEAP: Arena<HEAP_SIZE> = Arena::ZERO;

/// Global allocator backed by [`HEAP`].
///
/// SAFETY: the page-aligned `HEAP` static outlives the binary and the
/// allocator is its only consumer.
#[global_allocator]
static ALLOCATOR: FreeListAllocator =
    unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_SIZE) };

// --- Static DMA frame pool -------------------------------------------

/// Number of 4 KiB frames the per-device DMA allocator owns: the direct
/// driving pool ([`POOL_PAGES`]) plus the transient pool the `.rxe` load
/// mints, with slack. 512 frames is 2 MiB.
const DMA_POOL_PAGES: usize = 512;

/// Page-aligned `.bss` backing store for the DMA frame pool, exclusively
/// ours — the aarch64 stand-in for riscv64's carved-from-the-boot-map DMA
/// region.
static DMA_FRAMES: Arena<{ PAGE_SIZE * DMA_POOL_PAGES }> = Arena::ZERO;

/// Build a single-region [`BootMemoryMap`] over the static [`DMA_FRAMES`]
/// pool for the per-device [`FrameAllocator`].
fn static_dma_map() -> BootMemoryMap {
    let base = paging::image_phys(DMA_FRAMES.as_mut_ptr() as u64);
    let mut m = BootMemoryMap::new();
    m.push(MemoryRegion {
        kind: RegionKind::Usable,
        start: PhysAddr::new(base),
        length: (PAGE_SIZE * DMA_POOL_PAGES) as u64,
    });
    m
}

// --- Stable identifiers / bring-up parameters ------------------------

/// Milestone event id namespace for the shared serial breadcrumbs.
const MILESTONE_ID: EventId = EventId(9110);
/// Failure finisher codes, distinct per failure site.
const FAIL_SCENARIO: NonZeroU16 = fail_point!(1);
const FAIL_REENTRY: NonZeroU16 = fail_point!(2);

/// Synthetic owner process id for the bus-driver context.
const TASK: ProcessId = ProcessId(0x5b3);

/// Capacity, in pages, of each per-device DMA window.
const POOL_PAGES: usize = 64;

/// Base virtual address of each minted DMA window (bookkeeping only; the
/// driver reaches buffers through the direct map).
const POOL_VBASE: u64 = 0x2000_0000;

/// Base virtual address of the MMIO register-window map (bookkeeping).
const MMIO_VBASE: u64 = 0x6000_0000;

/// Capacity, in pages, of the MMIO register-window map.
const MMIO_CAP_PAGES: usize = 64;

/// The CPU the device SPI is routed to: the boot CPU.
const IRQ_CPU: tairix_arch_api::CpuId = 0;

// --- QEMU environment ------------------------------------------------

/// aarch64 [`QemuEnv`]: serial breadcrumbs over the PL011 UART sink, exit
/// through the ARM semihosting `SYS_EXIT` finisher.
///
/// Public so freestanding aarch64 `virt`-board verticals beyond the
/// virtio scenario (e.g. the framebuffer-display vertical) reuse the
/// same serial-breadcrumb + semihosting-exit seam.
pub struct AArch64QemuEnv;

impl QemuEnv for AArch64QemuEnv {
    fn log(&self, msg: &str) {
        SERIAL_SINK.write_event(&Event {
            level: Level::Info,
            id: MILESTONE_ID,
            message: msg,
            fields: &[],
        });
    }

    fn fail(&self, msg: &str) -> ! {
        self.log(msg);
        qemu_exit::exit_failure(FAIL_SCENARIO)
    }

    fn succeed(&self) -> ! {
        qemu_exit::exit_success()
    }

    fn audit_sink(&self) -> &'static dyn Sink {
        &SERIAL_SINK
    }
}

// --- Device-tree helper ----------------------------------------------

/// Find the GICv2 SPI *number* of the `virtio,mmio` slot whose `reg` base
/// equals `slot_base`. The `virt` board's virtio-MMIO nodes carry a
/// three-cell `interrupts` triplet `<type number flags>` where `type == 0`
/// (SPI); the GIC INTID is then [`MIN_SPI_INTID`]` + number`.
fn device_spi_number(dtb: &Fdt<'_>, slot_base: u64) -> Option<u32> {
    for node in dtb.nodes() {
        let node = node.ok()?;
        if !node.is_compatible(COMPATIBLE) {
            continue;
        }
        let reg = node.property("reg")?;
        if reg.read_be_u64(0).ok()? != slot_base {
            continue;
        }
        let interrupts = node.property("interrupts")?;
        // Cell 0 (byte offset 0) is the interrupt type; only SPIs
        // (type 0) are routable via `GICD_ITARGETSR`. Cell 1 (byte
        // offset 4) is the SPI number.
        if interrupts.read_be_u32(0).ok()? != 0 {
            return None;
        }
        return interrupts.read_be_u32(4).ok();
    }
    None
}

// --- IRQ controller bridge -------------------------------------------

/// kernel/irq ↔ aarch64 [`GicController`] bridge.
///
/// the charter forbids the architecture crate from depending on `kernel/irq`,
/// so the bridge lives here (the test crate may depend on both), mirroring
/// `tests/integration/irq_qemu_aarch64`. [`IrqController::mask`] delegates
/// to the HAL [`tairix_arch_api::IrqController`] mask (which clears the
/// distributor enable bit and emits the `SeqCst` mask-before-wake fence);
/// [`GicBridge::unmask`] re-enables the line for the next completion.
struct GicBridge {
    ctrl: GicController<ActiveGic>,
}

/// The bridge instance. Const-constructible (the GIC controller holds a
/// zero-sized MMIO handle and the max-INTID bound), so it lives in a
/// `static` the interrupt-context dispatch and the waiter reference.
static BRIDGE: GicBridge = GicBridge {
    ctrl: GicController::new(ActiveGic, MAX_INTID),
};

impl IrqController for GicBridge {
    fn mask(&self, line: u32) -> Result<(), MaskError> {
        tairix_arch_api::IrqController::mask(&self.ctrl, line).map_err(|_| MaskError::OutOfRange)
    }
}

impl GicBridge {
    /// Re-enable `line` at the distributor (priority is left at the
    /// mid value the controller installs).
    fn unmask(&self, line: u32) {
        let _ = tairix_arch_api::IrqController::unmask(&self.ctrl, line);
    }
}

// --- EL1 device-IRQ dispatch -----------------------------------------

/// The IRQ table the device dispatch fires into. Published before IRQs
/// are unmasked; `null` until set.
static DISPATCH_TABLE: AtomicPtr<IrqTable> = AtomicPtr::new(core::ptr::null_mut());

/// GIC INTID of the provisioned device. Published with [`DISPATCH_TABLE`].
static DISPATCH_INTID: AtomicU32 = AtomicU32::new(0);

/// Direct-map base of the provisioned device's virtio-MMIO register window,
/// so the dispatch can acknowledge the device-level interrupt (`0` until
/// set).
static DISPATCH_DEV_BASE: AtomicU64 = AtomicU64::new(0);

/// EL1 device-IRQ dispatcher: acknowledge the device-level virtio-MMIO
/// interrupt, then forward the line to [`IrqTable::fire`] (which masks the
/// GIC line before any waiter observes `ready`). The GIC IAR/EOIR
/// handshake is owned by the arch EL1 IRQ path, so this only touches the
/// device + the table — the GICv2 analogue of riscv64's `trap_dispatch`.
extern "C" fn device_dispatch(intid: u32) {
    if intid != DISPATCH_INTID.load(Ordering::Acquire) {
        return;
    }
    let table_ptr = DISPATCH_TABLE.load(Ordering::Acquire);
    if table_ptr.is_null() {
        return;
    }
    // Acknowledge the device-level virtio-MMIO interrupt: read
    // `InterruptStatus` and write the same bits to `InterruptACK` so the
    // device deasserts its line (virtio 1.1 §4.2.2). Without this a
    // level-high source never re-edges.
    let dev_base = DISPATCH_DEV_BASE.load(Ordering::Acquire);
    if dev_base != 0 {
        // SAFETY: `dev_base` is the direct-map base of the provisioned
        // device's virtio-MMIO register window, typed as registers for the
        // life of the guest; both registers are 4-byte aligned.
        unsafe {
            let isr =
                core::ptr::read_volatile((dev_base + regs::INTERRUPT_STATUS as u64) as *const u32);
            core::ptr::write_volatile((dev_base + regs::INTERRUPT_ACK as u64) as *mut u32, isr);
        }
    }
    // SAFETY: `table_ptr` was published once, before IRQs were unmasked,
    // from a `Box::leak`ed `'static` allocation that is never freed; the
    // dispatch only takes `&` to it.
    let table: &IrqTable = unsafe { &*table_ptr };
    let _ = table.fire(intid, &BRIDGE);
}

// --- IRQ waiter ------------------------------------------------------

/// [`IrqWaiter`] that parks the boot CPU on a race-free `wfi`.
///
/// Before parking it unmasks the device's GIC line (a prior
/// [`IrqTable::fire`] masked it) so the next completion can deliver. The
/// park sets `DAIF.I` (masking interrupt *taking*, not pending), re-reads
/// the line's ready flag, parks on `wfi` only if still not ready, then
/// clears `DAIF.I`. A completion that lands between the check and the
/// `wfi` is held pending until `wfi` is entered, so no edge is lost and no
/// bounding timer is needed (no unbounded sleep loop).
struct WfiWaiter {
    source: u32,
    table: &'static IrqTable,
    handle: IrqHandle,
}

impl IrqWaiter for WfiWaiter {
    fn now_ns(&self) -> u64 {
        // A fixed reading: this vertical's bound is the completion itself,
        // not a clock (see the park below).
        0
    }

    fn yield_now(&self, _deadline_ns: u64) -> Result<(), IrqWaitAbort> {
        // The caller's deadline needs no timer here: the park ends on any
        // interrupt, so control always returns to the wait loop. With a fixed
        // clock the loop's deadline never elapses, so a device that never
        // completed leaves the *guest* to be bounded by the harness's own
        // no-progress budget rather than by this park — which is the scenario
        // the vertical is asserting against.
        BRIDGE.unmask(self.source);
        // SAFETY: setting `DAIF.I` masks interrupt *taking* (not
        // pending); `wfi` still wakes on a pending enabled interrupt;
        // clearing `DAIF.I` lets the EL1 IRQ path fire. The sequence is
        // the canonical race-free park.
        unsafe {
            core::arch::asm!("msr DAIFSet, #2", options(nomem, nostack, preserves_flags));
            if !self.table.ready_for(self.handle) {
                core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
            }
            core::arch::asm!("msr DAIFClr, #2", options(nomem, nostack, preserves_flags));
        }
        Ok(())
    }
}

/// Build the external-IRQ path the aarch64 verticals own: resolve the
/// device's GICv2 SPI from the device tree, build an [`IrqTable`] (leaked
/// to `'static` so the dispatch and waiter can hold it), bind the line,
/// install the device dispatch, bring up the GICv2, route + enable the SPI
/// on CPU 0, and unmask IRQs at the PE. Any failure flips
/// QEMU failure through `env`.
fn arm_external_irq(
    env: &AArch64QemuEnv,
    dtb: &Fdt<'_>,
    slot_base: u64,
) -> (&'static IrqTable, IrqHandle, u32) {
    let Some(number) = device_spi_number(dtb, slot_base) else {
        env.fail("no device interrupt in DTB");
    };
    let source = MIN_SPI_INTID + number;
    let table: &'static IrqTable = Box::leak(Box::new(IrqTable::new(source)));
    let Ok(bind) = table.bind(source, TASK) else {
        env.fail("bind device source");
    };

    DISPATCH_TABLE.store(core::ptr::from_ref(table).cast_mut(), Ordering::Release);
    DISPATCH_INTID.store(source, Ordering::Release);

    if exceptions::set_device_irq_dispatch(device_dispatch).is_err() {
        env.fail("install device-IRQ dispatch");
    }
    // SAFETY: called once on the boot CPU; the port entry armed the EL1
    // vectors and typed the GIC's gigabyte as registers. Bring up the GICv2
    // distributor + CPU interface and route the device SPI to CPU 0.
    unsafe {
        tairix_itest_gic::init_boot_cpu().expect("the GIC comes up");
        gic::route_spi(source, IRQ_CPU).expect("the boot CPU's interface is up");
    }
    BRIDGE.unmask(source);
    // SAFETY: the vectors, dispatch, and GIC routing are in place, so an
    // incoming device SPI dispatches through the installed path.
    unsafe {
        exceptions::enable_irq();
    }
    (table, bind.handle, source)
}

// --- Shared scenario -------------------------------------------------

/// Perform the aarch64 `virt`-board virtio-MMIO bring-up for the device
/// whose bare virtio type id is `device_id` (block = 2, net = 1), then
/// drive the shared `load → reload → device round-trip → unload`
/// lifecycle with `body` as the per-device tail. Never returns.
///
/// `dtb_bytes` is the flattened device tree of the running `virt` board.
/// Unlike riscv64 (OpenSBI hands the blob in `a1`), QEMU's `-kernel
/// <ELF>` aarch64 path treats the image as bare firmware and passes no
/// DTB pointer, so each vertical embeds the canonical `virt` DTB at build
/// time (dumped by `qemu ... dumpdtb`) and passes it here. The transport
/// bases and SPIs in that blob are the stable `virt`-board layout,
/// independent of which slot the backing device lands on.
pub fn run_virtio_mmio_scenario<F>(
    device_id: u32,
    dtb_bytes: &[u8],
    cfg: &ScenarioConfig<'_>,
    body: F,
) -> !
where
    F: FnOnce(&dyn QemuEnv, MmioTransport, &dyn VirtioHost) -> Result<(), &'static str>,
{
    let env = AArch64QemuEnv;
    env.log(cfg.start_msg);

    // 1. Parse the embedded device-tree blob.
    let Ok(dtb) = Fdt::new(dtb_bytes) else {
        env.fail("DTB parse");
    };

    // 2. Per-device DMA: the static frame pool, reached through the direct map.
    let dma_map = static_dma_map();
    let Ok(frames) = FrameAllocator::new(&dma_map) else {
        env.fail("frame allocator build");
    };

    // 3. Bus-driver task capability context.
    let mut grants = CapabilitySet::empty();
    grants.insert(CapabilityId::MMIO_MAP);
    grants.insert(CapabilityId::MEM_DMA);
    grants.insert(CapabilityId::DRV_LOAD);
    let Ok(caller) = TaskCapabilities::derive(TASK, UserId(0), grants, grants, &SERIAL_SINK) else {
        env.fail("caller capability record");
    };

    // 4. Build the `virt`-board MMIO bus and provision the transport.
    let registers = |base, len| paging::registers_virt(base, len).map(|at| at as *const u32);
    // SAFETY: `registers` yields the aperture's direct-map address only where
    // the direct map types it as registers, and the aperture is exclusively
    // the bus's to read.
    let Ok(bus) = (unsafe { virtio_mmio_bus_from_dtb(dtb_bytes, registers) }) else {
        env.fail("virtio-MMIO bus construct");
    };
    let Ok(mut mmio) = MmioMap::new(
        AddressSpace::new(HostPageTable::new()),
        VirtAddr::new(MMIO_VBASE),
        MMIO_CAP_PAGES,
        &DeviceWindows,
    ) else {
        env.fail("MMIO map construct");
    };
    let (transport, slot_base) = {
        let mapper = KernelMmioMapper::new(&mut mmio, &caller, &SERIAL_SINK);
        let slot = first_virtio_slot(&bus, device_id);
        let Ok(prov) = slot.and_then(|slot| {
            provision_virtio_mmio(&bus, device_id, slot, &mapper, MmioTransport::new)
        }) else {
            env.fail("virtio-MMIO provisioning walk");
        };
        (prov.transport, prov.base)
    };
    let Some(dev_base) = paging::registers_virt(slot_base, regs::WINDOW_MIN_LEN as u64) else {
        env.fail("virtio-MMIO registers outside the direct map");
    };
    DISPATCH_DEV_BASE.store(dev_base, Ordering::Release);
    env.log("virtio-qemu: MMIO transport provisioned");

    // 5. Build the external-IRQ path (GICv2 + EL1 vectors) from the DTB.
    let (table, handle, source) = arm_external_irq(&env, &dtb, slot_base);
    env.log("virtio-qemu: GICv2 SPI armed, EL1 IRQ path live");

    // 6. Mint the per-device DMA host the driver allocates through.
    let space = AddressSpace::new(HostPageTable::new());
    let Ok(pool) = DmaPool::new(
        space,
        VirtAddr::new(POOL_VBASE),
        POOL_PAGES,
        &frames,
        &SPAWN_TABLE_PHYSMAP,
        DmaCoherence::Snooped,
    ) else {
        env.fail("DMA pool construct");
    };
    let wait = FixtureLine {
        table,
        handle,
        owner: caller.process(),
        park: WfiWaiter {
            source,
            table,
            handle,
        },
    };
    let vhost = KernelVirtioHost::new(pool, &caller, &SERIAL_SINK, PoolId::fresh(), &wait);

    // 7. Drive the shared lifecycle with `body` against the reloaded driver.
    drive_driver_lifecycle(&env, cfg, transport, &vhost, body)
}

// --- Boot harness ----------------------------------------------------

/// Fail-closed exit for a second `kernel_main` entry (a one-shot scenario
/// must never re-run). Exposed for the boot-harness macro.
#[doc(hidden)]
pub fn harness_fail_reentry() -> ! {
    qemu_exit::exit_failure(FAIL_REENTRY)
}

/// Generate the freestanding boot harness for an aarch64 virtio-MMIO QEMU
/// test bin: the `#[panic_handler]` bridge and the `kernel_main(dtb)`
/// entry point (the symbol `tairix_arch_aarch64_main` calls). It drives
/// `$scenario` exactly once.
///
/// `$scenario` must be a `fn() -> !`. Invoke exactly once at the crate
/// root of the freestanding bin. Unlike the riscv64 harness there is no
/// boot pipeline to observe: the `virt` board hands EL1 a usable machine
/// directly, so the vertical owns its bring-up from `kernel_main`. The
/// `dtb` hand-off argument is ignored — QEMU's `-kernel <ELF>` aarch64
/// path passes no DTB pointer, so the scenario uses the build-time
/// embedded `virt` DTB instead (see [`run_virtio_mmio_scenario`]).
#[macro_export]
macro_rules! define_mmio_boot_harness_aarch64 {
    ($scenario:path) => {
        /// Latch so the scenario runs exactly once.
        static SCENARIO_RAN: ::core::sync::atomic::AtomicBool =
            ::core::sync::atomic::AtomicBool::new(false);

        /// Forward to the shared aarch64 panic bridge.
        #[panic_handler]
        fn virtio_qemu_mmio_aarch64_panic(info: &::core::panic::PanicInfo<'_>) -> ! {
            $crate::handle_panic_via_serial(info)
        }

        /// Boot entry point — the symbol `tairix_arch_aarch64_main` calls.
        #[no_mangle]
        pub extern "C" fn kernel_main(_dtb: u64) -> ! {
            if SCENARIO_RAN.swap(true, ::core::sync::atomic::Ordering::SeqCst) {
                // A second entry would re-run a one-shot scenario; fail
                // closed.
                $crate::harness_fail_reentry();
            }
            $scenario();
        }
    };
}
