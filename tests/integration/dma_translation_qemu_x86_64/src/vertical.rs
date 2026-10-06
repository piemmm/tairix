//! `plans/IOMMU.md` MI0 and MI2: boot the production x86_64 pipeline on `q35`
//! behind a translation unit, every virtio function `iommu_platform=on`, and
//! prove the boot's DMA confined. One binary runs behind an `intel-iommu`,
//! the other behind an `amd-iommu`, each judged by the shared translation
//! witness.
//!
//! The key the runner injects reaches the input-focus arbiter only through
//! the unit: the floor disk reads the driver store through the kernel's
//! domain, and the autoloaded keyboard driver's buffers are reachable only
//! through its node's. The keyboard sits behind a PCIe-to-PCI bridge, so its
//! DMA arrives under the bridge's alias (IOM8), and its interrupt is its own
//! remapping entry, in extended mode with the CPU in x2APIC mode.

#[cfg(itest_x86_64)]
mod kernel {
    use core::panic::PanicInfo;

    use tairix_arch_x86_64::qemu_exit;
    use tairix_itest_translation_witness::{
        first_input_node, Interrupts, Stage, TranslationWitness, Verdict,
    };
    use tairix_kernel::hwtree_store::HW_TREE_SOURCE;
    use tairix_kernel::kalloc::{Heap, HEAP_BYTES};
    use tairix_kernel::{
        boot, handle_panic_via_kernel_core, FreeListAllocator, SerialSink, SERIAL_SINK,
    };
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

    /// Replays every event to serial and ends the run on the witness's
    /// verdict.
    struct TranslationSink(TranslationWitness);

    impl Sink for TranslationSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            match self.0.observe(event, || first_input_node(&HW_TREE_SOURCE)) {
                Verdict::Pending => {}
                Verdict::Pass => qemu_exit::exit_success(),
                Verdict::Fail(_) => qemu_exit::exit_failure(),
            }
        }
    }

    /// The CPUs take remapped interrupts in x2APIC mode.
    static AUDIT_SINK: TranslationSink = TranslationSink(TranslationWitness::new(
        Interrupts::Remapped {
            extended: tairix_arch_x86_64::apic::x2apic,
        },
        Stage::Second,
    ));

    /// A panic halts the guest; the run times out and fails loud.
    #[panic_handler]
    fn tairix_dma_translation_qemu_x86_64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_kernel_core(info)
    }

    /// The symbol the arch crate's boot trampoline calls. Boots at the
    /// `Debug` filter because the runner injects the key on the driver's
    /// audited `irq_bind`, whose record is `Debug`.
    #[no_mangle]
    pub extern "C" fn kernel_main(multiboot_info: u64) -> ! {
        boot(
            multiboot_info,
            &ALLOCATOR,
            &SERIAL_SINK,
            &AUDIT_SINK,
            tairix_log::Level::Debug,
        )
    }
}
