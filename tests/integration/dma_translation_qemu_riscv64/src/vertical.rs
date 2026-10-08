//! `plans/IOMMU.md` IOM16 and IOM17: boot the production riscv64 pipeline on
//! `virt` behind its RISC-V IOMMU or a virtio-iommu and prove the DMA of the
//! functions behind it confined. The keyboard and mouse are virtio-pci
//! functions `iommu_platform=on` on the ECAM host whose `iommu-map` names the
//! unit; the floor disk is a virtio-MMIO device the unit does not front.
//!
//! The key the runner injects reaches the input-focus arbiter only through
//! the unit: the autoloaded keyboard driver's rings are reachable only
//! through its node's domain, and the unit refuses every other access. The
//! run is judged by the shared translation witness and by where the unit's
//! registers lie.

use core::panic::PanicInfo;

use tairix_arch_riscv64::{handle_panic_via_serial, qemu_exit, SerialSink, SERIAL_SINK};
use tairix_itest_translation_witness::{
    first_input_node, registers_from, Faults, TranslationWitness, Verdict, FAIL_REGISTERS,
};
use tairix_kernel::hwtree_store::HW_TREE_SOURCE;
use tairix_kernel::kalloc::{Heap, HEAP_BYTES};
use tairix_kernel::riscv64::boot as boot_riscv64;
use tairix_kernel::FreeListAllocator;
use tairix_kernel_core::HwTreeSource;
use tairix_log::{Event, Sink};

/// The boot heap, in the layout's own section, which the boot path keeps out
/// of the usable memory map.
#[link_section = ".heap"]
static HEAP: Heap = Heap::ZERO;

/// Global allocator backed by [`HEAP`].
///
/// SAFETY: the page-aligned `HEAP` static outlives the binary and the
/// allocator is its only consumer.
#[global_allocator]
static ALLOCATOR: FreeListAllocator =
    unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

/// Replays every event to serial and ends the run on the witness's verdict,
/// or on a unit translating from registers below [`super::REGISTERS_FROM`].
struct TranslationSink(TranslationWitness);

impl Sink for TranslationSink {
    fn write_event(&self, event: &Event<'_>) {
        SerialSink::new().write_event(event);
        if !registers_from(event, super::REGISTERS_FROM, |node| {
            HW_TREE_SOURCE.node(node).ok().flatten()
        }) {
            qemu_exit::exit_failure(FAIL_REGISTERS);
        }
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

/// A panic parks the hart; the run times out and fails loud.
#[panic_handler]
fn tairix_dma_translation_qemu_riscv64_panic(info: &PanicInfo<'_>) -> ! {
    handle_panic_via_serial(info)
}

/// The symbol the arch crate's boot trampoline calls, with the SBI hand-off
/// values: the boot hart and the live device tree. Boots at the `Debug`
/// filter because the runner injects the key on the driver's audited
/// `irq_bind`, whose record is `Debug`.
#[no_mangle]
pub extern "C" fn kernel_main(hartid: u64, dtb: u64) -> ! {
    boot_riscv64::boot(
        hartid,
        dtb,
        &ALLOCATOR,
        &SERIAL_SINK,
        &AUDIT_SINK,
        tairix_log::Level::Debug,
    )
}
