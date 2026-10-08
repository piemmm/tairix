//! `plans/IOMMU.md` IOM15 and IOM17: boot the production aarch64 pipeline on
//! `virt` behind an Arm `SMMUv3` or a virtio-iommu and prove the DMA of the
//! functions behind it confined. The keyboard and mouse are virtio-pci
//! functions `iommu_platform=on` on the ECAM host whose `iommu-map` names the
//! unit; the floor disk is a virtio-MMIO device the unit does not front.
//!
//! The key the runner injects reaches the input-focus arbiter only through
//! the unit: the autoloaded keyboard driver's rings are reachable only
//! through its node's domain, and the unit aborts every other access. The run
//! is judged by the shared translation witness, its interrupts wired.

use core::panic::PanicInfo;

use tairix_arch_aarch64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
use tairix_itest_translation_witness::{first_input_node, Faults, TranslationWitness, Verdict};
use tairix_kernel::aarch64::boot as boot_aarch64;
use tairix_kernel::hwtree_store::HW_TREE_SOURCE;
use tairix_kernel::kalloc::{Heap, HEAP_BYTES};
use tairix_kernel::FreeListAllocator;
use tairix_log::{Event, Sink};

/// The boot heap, in `.bss`.
static HEAP: Heap = Heap::ZERO;

/// Global allocator backed by [`HEAP`].
///
/// SAFETY: the page-aligned `HEAP` static outlives the binary and the
/// allocator is its only consumer.
#[global_allocator]
static ALLOCATOR: FreeListAllocator =
    unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

/// Replays every event to serial and ends the run on the witness's verdict.
struct TranslationSink(TranslationWitness);

impl Sink for TranslationSink {
    fn write_event(&self, event: &Event<'_>) {
        SerialSink::new().write_event(event);
        match self.0.observe(event, || first_input_node(&HW_TREE_SOURCE)) {
            Verdict::Pending => {}
            Verdict::Pass => qemu_exit::exit_success(),
            Verdict::Fail(code) => qemu_exit::exit_failure(code),
        }
    }
}

static AUDIT_SINK: TranslationSink = TranslationSink(TranslationWitness::new(
    super::INTERRUPTS,
    super::TABLES,
    Faults::Served,
));

/// A panic halts the guest; the run times out and fails loud.
#[panic_handler]
fn tairix_dma_translation_qemu_aarch64_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_serial(info)
}

/// Boot the production pipeline over `tree`, the embedded tree of the machine
/// this binary runs on: QEMU passes no tree to an ELF `-kernel`. Boots at the
/// `Debug` filter because the runner injects the key on the driver's audited
/// `irq_bind`, whose record is `Debug`.
pub fn boot(tree: &'static [u8]) -> ! {
    boot_aarch64::boot(
        tree.as_ptr() as u64,
        &ALLOCATOR,
        &SERIAL_SINK,
        &AUDIT_SINK,
        tairix_log::Level::Debug,
        &HW_TREE_SOURCE,
    )
}
