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
//! PASS once `AuditEvent::InputDelivered` with `kind=key` follows
//! `AuditEvent::DmaTranslationUnit` with `outcome=translating`. A unit left
//! untranslated, or a key delivered before any unit translates, fails the run
//! at once rather than passing on DMA that bypassed the unit.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
mod kernel {
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, Ordering};

    use tairix_arch_x86_64::qemu_exit;
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

    /// Replays every event to serial and judges the run on the two witnesses.
    struct TranslationSink {
        translating: AtomicBool,
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
                ) {
                    self.translating.store(true, Ordering::Release);
                } else {
                    qemu_exit::exit_failure();
                }
            } else if event.id.0 == AuditEvent::InputDelivered.id().0
                && matches!(field(event, "kind"), Some(FieldValue::Str("key")))
            {
                if self.translating.load(Ordering::Acquire) {
                    qemu_exit::exit_success();
                }
                qemu_exit::exit_failure();
            }
        }
    }

    static AUDIT_SINK: TranslationSink = TranslationSink {
        translating: AtomicBool::new(false),
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
