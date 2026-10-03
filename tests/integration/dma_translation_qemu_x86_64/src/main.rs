//! `plans/IOMMU.md` MI0 QEMU integration test: boot the production x86_64
//! `tairix-kernel` pipeline on `q35` behind an `intel-iommu`, every virtio
//! function reaching memory through it, and prove the boot's DMA is confined.
//!
//! With `iommu_platform=on` a virtio function's every access goes through the
//! unit, so nothing here works unless translation does: the kernel takes over
//! the unit the ACPI DMAR describes and enables it, the in-kernel floor disk
//! reads the `/System` driver store through the kernel's own domain, and the
//! autoloaded user-space `virtio_kbd` driver's event buffers are reachable
//! only through its node's domain. The key the runner injects arriving at the
//! input-focus arbiter is therefore a transfer that crossed the unit twice —
//! the driver's queue setup and the device's event write.
//!
//! The keyboard sits behind a PCIe-to-PCI bridge, which tags its requests
//! with the bridge's secondary bus and function `00.0`: its DMA reaches the
//! unit under that alias, never its own requester id, so the key arrives only
//! if its domain translates the alias too (`plans/IOMMU.md` IOM8). Any
//! translation fault fails the run at once.
//!
//! No function may be a bus master before its unit translates and its
//! owner's domain is attached (`plans/IOMMU.md` IOM7): the unit's record
//! reports, read from every function behind it as it took over, that none
//! was, and each function is granted bus mastering only once its owner's
//! domain holds its stream.
//!
//! PASS once `AuditEvent::InputDelivered` with `kind=key` follows
//! `AuditEvent::DmaTranslationUnit` with `outcome=translating`, `stopped=0`
//! and `refused=0`, and the keyboard's node was granted bus mastering
//! (`AuditEvent::DmaBusMaster` `master=on`) after it. A unit left
//! untranslated or taking over a function still mastering, a function made a
//! bus master before any unit translates, a translation fault, or a key
//! delivered before both, fails the run at once rather than passing on DMA
//! that bypassed the unit.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
mod kernel {
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, Ordering};

    use tairix_arch_x86_64::qemu_exit;
    use tairix_kernel::hwtree_node_ids::VIRTIO_PCI_INPUT_PROBE_NODE_BASE_ID;
    use tairix_kernel::kalloc::{Heap, HEAP_BYTES};
    use tairix_kernel::{
        boot, handle_panic_via_kernel_core, FreeListAllocator, SerialSink, SERIAL_SINK,
    };
    use tairix_kernel_core::AuditEvent;
    use tairix_log::{Event, FieldValue, Sink};

    /// The boot heap, in `.bss`.
    static HEAP: Heap = Heap::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and the
    /// allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

    /// Replays every event to serial and judges the run on the witnesses.
    struct TranslationSink {
        translating: AtomicBool,
        keyboard_mastered: AtomicBool,
    }

    fn field<'e>(event: &'e Event<'_>, key: &str) -> Option<&'e FieldValue<'e>> {
        event
            .fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| &field.value)
    }

    impl Sink for TranslationSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            if event.id.0 == AuditEvent::DmaTranslationUnit.id().0 {
                if matches!(
                    field(event, "outcome"),
                    Some(FieldValue::Str("translating"))
                ) && matches!(field(event, "stopped"), Some(FieldValue::UnsignedInt(0)))
                    && matches!(field(event, "refused"), Some(FieldValue::UnsignedInt(0)))
                {
                    self.translating.store(true, Ordering::Release);
                } else {
                    qemu_exit::exit_failure();
                }
            } else if event.id.0 == AuditEvent::DmaBusMaster.id().0
                && matches!(field(event, "master"), Some(FieldValue::Str("on")))
            {
                if !self.translating.load(Ordering::Acquire) {
                    qemu_exit::exit_failure();
                }
                if matches!(field(event, "node"), Some(FieldValue::UnsignedInt(node))
                        if *node == u64::from(VIRTIO_PCI_INPUT_PROBE_NODE_BASE_ID))
                    && matches!(field(event, "outcome"), Some(FieldValue::Str("applied")))
                {
                    self.keyboard_mastered.store(true, Ordering::Release);
                }
            } else if event.id.0 == AuditEvent::DmaTranslationFault.id().0 {
                qemu_exit::exit_failure();
            } else if event.id.0 == AuditEvent::InputDelivered.id().0
                && matches!(field(event, "kind"), Some(FieldValue::Str("key")))
            {
                if self.translating.load(Ordering::Acquire)
                    && self.keyboard_mastered.load(Ordering::Acquire)
                {
                    qemu_exit::exit_success();
                }
                qemu_exit::exit_failure();
            }
        }
    }

    static AUDIT_SINK: TranslationSink = TranslationSink {
        translating: AtomicBool::new(false),
        keyboard_mastered: AtomicBool::new(false),
    };

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

#[cfg(not(itest_x86_64))]
fn main() {}
