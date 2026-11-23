//! The `abi-v1` syscalls [`RtDriverHost`](crate::RtDriverHost) issues,
//! behind a host-testable seam.
//!
//! [`GrantSyscalls`] abstracts the syscall wrappers — `mmio_map`, `dma_alloc`
//! and the rest — so the host's grant-resolution and address-translation logic is
//! unit-tested without a kernel — exactly as the bus drivers
//! mock their register windows. Production driver processes use
//! [`RtGrantSyscalls`], the zero-sized forwarder to `tairix_rt`, so the one
//! syscall trap is not duplicated.

use core::num::NonZeroUsize;
use core::ptr::NonNull;

use tairix_abi::driver::{DmaReach, RegionCaching};

/// The `mmio_map` / `dma_alloc` syscalls the user-space driver host issues.
///
/// A call that maps answers the base of its mapping as a pointer, or the
/// kernel's raw signed `abi-v1` result when it refused (`-errno`; recover the
/// [`tairix_abi::Errno`] discriminant as `-ret`). Every other call returns
/// that raw result itself. The host adds no authority; the kernel validates
/// the grant handle and every bound on the far side of the trap.
///
/// # Safety
///
/// The host builds register windows, DMA slabs and the shared buffer from the
/// bases these methods answer, so an implementation must answer as the kernel
/// does: an `Ok` base from [`Self::mmio_map`], [`Self::dma_alloc`] or
/// [`Self::shm_map`] is a pointer valid for reads and writes of at least the
/// length asked for (for `shm_map`, the length it writes), aliased by nothing
/// else in this process, and mapped until the process ends — a carve until
/// its [`DmaRelease`] frees it, a window until [`Self::mmio_unmap`] does.
pub unsafe trait GrantSyscalls {
    /// Map the `[offset, offset + len)` sub-region of the device MMIO window
    /// named by the kernel-issued grant `handle` into the calling task's own
    /// address space, with the caching its grant names or, for a memory
    /// region, `caching`, answering the base of the mapped sub-region.
    ///
    /// Mapping a sub-region (not the whole grant) is what lets a driver
    /// granted a large outbound bus aperture map just the single BAR it
    /// enumerated rather than the entire window.
    ///
    /// Mirrors [`tairix_rt::mmio_map`].
    fn mmio_map(
        &self,
        handle: u64,
        offset: u64,
        len: usize,
        caching: Option<RegionCaching>,
    ) -> Result<NonNull<u8>, i64>;

    /// Unmap the window a prior [`Self::mmio_map`] mapped at `base`,
    /// returning `0` or `-errno`.
    ///
    /// Mirrors [`tairix_rt::mmio_unmap`].
    fn mmio_unmap(&self, base: u64) -> i64;

    /// The kernel-attested machine summary, or `None` when the call
    /// refused. Drivers size their pinned DMA staging from it.
    ///
    /// Mirrors [`tairix_rt::boot_facts`].
    fn boot_facts(&self) -> Option<tairix_abi::BootFacts>;

    /// Carve a coherent DMA buffer of `len` bytes bounded by the constraint
    /// named by the kernel-issued grant `handle` and the `reach` address bits
    /// the device drives, write the buffer's device-visible base to
    /// `device_out`, and answer its base (`device_out` is left untouched on a
    /// refusal).
    ///
    /// Mirrors [`tairix_rt::dma_alloc`].
    fn dma_alloc(
        &self,
        handle: u64,
        len: usize,
        reach: DmaReach,
        device_out: &mut u64,
    ) -> Result<NonNull<u8>, i64>;

    /// What a carve's free is issued through.
    type Release: DmaRelease + 'static;

    /// The seam a carve's free goes through, which its slab keeps whatever
    /// becomes of the host that carved it.
    fn dma_release(&self) -> Self::Release;

    /// Declare the device quiesced, releasing what earlier instances of this
    /// driver left in its node's DMA quarantine; returns the bytes freed (or
    /// `-errno`).
    ///
    /// Mirrors [`tairix_rt::dma_quiesced`].
    fn dma_quiesced(&self) -> i64;

    /// Map the cross-process shared-memory region named by the kernel-issued
    /// grant `handle` into the calling task's own address space, answering
    /// the base of the mapping and writing the region's byte length — the
    /// kernel's own record, never the granting task's claim — to `len_out`
    /// on success.
    ///
    /// A class driver maps the URB data buffer its host-controller driver
    /// created and forwarded as a grant on the matched interface node, holding
    /// no DMA authority of its own.
    ///
    /// Mirrors [`tairix_rt::shm_map`].
    fn shm_map(&self, handle: u64, len_out: &mut u64) -> Result<NonNull<u8>, i64>;

    /// Enumerate the device-resource grants the kernel minted for the
    /// calling task into `buf`, returning the total number of bytes written
    /// — consecutive [`tairix_abi::hwtree::GrantedResource`] records — when
    /// non-negative, else `-errno`. A buffer too small for the whole set
    /// fails closed.
    ///
    /// Mirrors [`tairix_rt::resource_grants`].
    fn resource_grants(&self, buf: &mut [u8]) -> i64;

    /// Bind interrupt `line` (an [`HwResourceKind::Irq`] grant the driver's
    /// matched node carried) to the calling task, returning the raw
    /// [`tairix_abi::IrqHandle`] (or `-errno`). Carries `CAP_IRQ_BIND`.
    ///
    /// [`HwResourceKind::Irq`]: tairix_abi::hwtree::HwResourceKind
    ///
    /// Mirrors [`tairix_rt::irq_bind`].
    fn irq_bind(&self, line: u32) -> i64;

    /// Park the calling task until the interrupt bound to `handle` fires (or
    /// `timeout_ns` elapses), returning `0` on a fire (or `-errno`). The
    /// kernel re-arms the bound line on the driver's behalf across the park
    /// (the driver holds no controller access).
    ///
    /// Mirrors [`tairix_rt::irq_wait`].
    fn irq_wait(&self, handle: u64, timeout_ns: u64) -> i64;

    /// Read the kernel monotonic clock in nanoseconds, the clock
    /// [`Self::irq_wait`]'s timeouts run on.
    ///
    /// Mirrors [`tairix_rt::clock_get`].
    fn clock_get(&self) -> u64;

    /// Make a synchronous capability-checked call to the kernel-owned call
    /// endpoint `endpoint`: post `request`, block until the reply arrives,
    /// and copy it into `reply`, returning the number of reply bytes written
    /// (or `-errno`). The host uses this only to reach the firmware
    /// property-mailbox service ([`MailboxChannel`](tairix_abi::driver::MailboxChannel)
    /// over [`tairix_abi::mailbox_ipc::MAILBOX_ENDPOINT`]); the kernel gates
    /// the call by the endpoint's required send capability and copies both
    /// buffers through the validated boundary.
    ///
    /// Mirrors [`tairix_rt::ipc_call`].
    fn ipc_call(&self, endpoint: u64, request: &[u8], reply: &mut [u8]) -> i64;

    /// Publish a discovered child device `node` into the live hardware tree,
    /// returning `0` once published, else `-errno`. A user-space **bus**
    /// driver calls this for each device it enumerates so the device manager
    /// autoloads the matching driver in turn; the kernel admits the node only
    /// when every resource it requests is covered by one of the calling
    /// driver's own grants, so a child can never carry more authority than
    /// its emitter.
    ///
    /// Mirrors [`tairix_rt::hw_emit_node`].
    fn hw_emit_node(&self, node: &tairix_abi::HwNode) -> i64;

    /// Publish the fault-domain `health` of the interior node the calling
    /// driver owns into the live hardware tree, returning `0` once recorded,
    /// else `-errno`. A bus/hub/controller driver reports its own node's
    /// [`tairix_abi::blkio::FaultDomainState`] so the device manager reacts to
    /// *one* coherent fault-domain recovery episode across the subtree rather
    /// than N spurious child removals; the node stays present (a distinct
    /// signal from a surprise removal), and the kernel records only the
    /// calling driver's *own* matched node's health (no ambient authority).
    ///
    /// Mirrors [`tairix_rt::hw_node_health`].
    fn hw_node_health(&self, health: tairix_abi::blkio::FaultDomainState) -> i64;

    /// Allocate a message-signalled interrupt (MSI) vector for a PCI
    /// function, returning the [`tairix_abi::MsiAllocation`] the kernel
    /// minted (the virtual interrupt line plus the doorbell to program into
    /// the function's MSI capability), or the raw negative kernel result
    /// (`-errno`) on failure. Carries `CAP_IRQ_BIND`.
    ///
    /// Mirrors [`tairix_rt::msi_alloc`].
    fn msi_alloc(&self) -> Result<tairix_abi::MsiAllocation, i64>;
}

/// The production [`GrantSyscalls`]: forward to `tairix_rt`'s wrappers.
///
/// Zero-sized; a driver process constructs one and hands it to
/// [`RtDriverHost::new`](crate::RtDriverHost::new). It carries no state and no
/// authority — it only routes through the one shared syscall trap.
#[derive(Debug, Default, Copy, Clone)]
pub struct RtGrantSyscalls;

// SAFETY: every answer is the kernel's own. It maps each window, carve and
// shared region into this task alone, and unmaps none while the task runs but
// a carve or window this task hands back.
unsafe impl GrantSyscalls for RtGrantSyscalls {
    #[inline]
    fn mmio_map(
        &self,
        handle: u64,
        offset: u64,
        len: usize,
        caching: Option<RegionCaching>,
    ) -> Result<NonNull<u8>, i64> {
        kernel_mapping(tairix_rt::mmio_map(handle, offset, len, caching))
    }

    #[inline]
    fn mmio_unmap(&self, base: u64) -> i64 {
        tairix_rt::mmio_unmap(base)
    }

    #[inline]
    fn boot_facts(&self) -> Option<tairix_abi::BootFacts> {
        tairix_rt::boot_facts().ok()
    }

    #[inline]
    fn dma_alloc(
        &self,
        handle: u64,
        len: usize,
        reach: DmaReach,
        device_out: &mut u64,
    ) -> Result<NonNull<u8>, i64> {
        kernel_mapping(tairix_rt::dma_alloc(handle, len, reach, device_out))
    }

    type Release = RtDmaRelease;

    #[inline]
    fn dma_release(&self) -> RtDmaRelease {
        RtDmaRelease
    }

    #[inline]
    fn dma_quiesced(&self) -> i64 {
        tairix_rt::dma_quiesced()
    }

    #[inline]
    fn shm_map(&self, handle: u64, len_out: &mut u64) -> Result<NonNull<u8>, i64> {
        kernel_mapping(tairix_rt::shm_map(handle, len_out))
    }

    #[inline]
    fn resource_grants(&self, buf: &mut [u8]) -> i64 {
        tairix_rt::resource_grants(buf)
    }

    #[inline]
    fn irq_bind(&self, line: u32) -> i64 {
        tairix_rt::irq_bind(line)
    }

    #[inline]
    fn irq_wait(&self, handle: u64, timeout_ns: u64) -> i64 {
        tairix_rt::irq_wait(handle, timeout_ns)
    }

    #[inline]
    fn clock_get(&self) -> u64 {
        tairix_rt::clock_get()
    }

    #[inline]
    fn ipc_call(&self, endpoint: u64, request: &[u8], reply: &mut [u8]) -> i64 {
        // `tairix_rt::ipc_call` already clamps a non-negative count to the
        // reply buffer; surface the raw signed result for the host to decode.
        #[allow(clippy::cast_possible_wrap)]
        match tairix_rt::ipc_call(endpoint, request, reply) {
            Ok(count) => count as i64,
            Err(errno) => errno,
        }
    }

    #[inline]
    fn hw_emit_node(&self, node: &tairix_abi::HwNode) -> i64 {
        tairix_rt::hw_emit_node(node)
    }

    #[inline]
    fn hw_node_health(&self, health: tairix_abi::blkio::FaultDomainState) -> i64 {
        tairix_rt::hw_node_health(health)
    }

    #[inline]
    fn msi_alloc(&self) -> Result<tairix_abi::MsiAllocation, i64> {
        tairix_rt::msi_alloc()
    }
}

/// The base a mapping call answered, as the pointer the host builds from, or
/// the raw result when it names none: `-errno`, or a base no pointer can be
/// formed at.
pub(crate) fn kernel_mapping(ret: i64) -> Result<NonNull<u8>, i64> {
    let base = usize::try_from(ret)
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or(ret)?;
    // The kernel made the mapping outside this program, so the pointer
    // carries the exposed provenance such memory has.
    Ok(NonNull::with_exposed_provenance(base))
}

/// The one call a DMA carve's slab makes as it drops, on whichever thread
/// drops it.
pub trait DmaRelease: Send + Sync {
    /// Release the coherent DMA buffer based at `cpu_va` (the user virtual
    /// base a prior [`GrantSyscalls::dma_alloc`] returned) bounded by the
    /// constraint named by grant `handle`, returning `0` on success (or
    /// `-errno`): a long-running driver reclaims each transfer's buffers
    /// through this rather than leaking DMA frames until it exits.
    ///
    /// Mirrors [`tairix_rt::dma_free`].
    fn dma_free(&self, handle: u64, cpu_va: u64) -> i64;
}

/// The production [`DmaRelease`]: the `dma_free` syscall, which needs no state
/// of the process's but the grant and the base it names.
#[derive(Debug, Default, Copy, Clone)]
pub struct RtDmaRelease;

impl DmaRelease for RtDmaRelease {
    #[inline]
    fn dma_free(&self, handle: u64, cpu_va: u64) -> i64 {
        tairix_rt::dma_free(handle, cpu_va)
    }
}
