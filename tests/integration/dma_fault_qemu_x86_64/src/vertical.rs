//! `plans/IOMMU.md` MI0, MI2 and IOM6: provoke a live DMA translation fault
//! behind a real emulated unit and witness it delivered through the unit's
//! fault interrupt, the path the register models cannot exercise. One binary
//! runs behind an `intel-iommu`, the other behind an `amd-iommu`.
//!
//! A bin-local PID 1 seam admits a misbehaving in-kernel virtio-blk-PCI
//! driver, brought up through the floor's own bring-up with its DMA required
//! to be translated. It reads the sector the runner planted through its node's
//! domain, declares the canary page it is about to aim at, then points a
//! device write at that page's physical address, an IOVA its domain does not
//! map. The unit refuses the write and raises its fault interrupt; the per-unit
//! fault service drains the record into `DmaTranslationFault`. The driver ends
//! its owner, and its function stops mastering (IOM7).
//!
//! PASS once every witness has arrived in its order: the unit translating with
//! no function found mastering; mastering granted the node; the driver's
//! declaration; an `unmapped` write fault on the declared unit and stream at
//! the canary's page; the canary, judged only after that, clean; and the
//! function stopped mastering.
//!
//! A fault storm is proven against the register models: one needs more
//! refusals inside the budget's window than a device can be made to raise on a
//! loaded host.

#[cfg(itest_x86_64)]
mod kernel {
    use core::convert::Infallible;
    use core::panic::PanicInfo;
    use core::ptr::NonNull;
    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    use alloc::boxed::Box;

    use tairix_abi::blkio::BlkDeviceClass;
    use tairix_abi::hwtree::snapshot_nodes;
    use tairix_abi::{CapabilityId, DriverError, HwMatchKey, HwNode, IommuStreams};
    use tairix_arch_x86_64::qemu_exit;
    use tairix_caps::CapabilitySet;
    use tairix_drv_bus_virtio::PciTransport;
    use tairix_drv_storage_virtio_blk::{wire, VIRTIO_BLK_DEVICE_ID};
    use tairix_itest_witness::{field_str, field_u64, sector0_byte};
    use tairix_kernel::floor_dma::Confinement;
    use tairix_kernel::hwtree_store::HW_TREE_SOURCE;
    use tairix_kernel::kalloc::{Heap, HEAP_BYTES};
    use tairix_kernel::x86_64::floor::{bring_up_virtio_pci, FloorDevice, FloorHost};
    use tairix_kernel::x86_64::init_spawn::X86_64InitSpawn;
    use tairix_kernel::x86_64::spawn_producer::SPAWN_TABLE_PHYSMAP;
    use tairix_kernel::SERIAL_SINK;
    use tairix_kernel::{
        boot_with_init, handle_panic_via_kernel_core, FreeListAllocator, SerialSink,
    };
    use tairix_kernel_core::iommu::KERNEL_OWNER;
    use tairix_kernel_core::waitq::{wait_arch, Parker, WaitQueue};
    use tairix_kernel_core::{AuditEvent, HwTreeSource, InitSpawnCtx, YieldHandle};
    use tairix_kernel_mem::{FrameAllocator, MemoryClass, PhysMap, PAGE_SIZE};
    use tairix_kernel_sec::captable::TaskCapabilities;
    use tairix_kernel_sec::identity::UserId;
    use tairix_log::{Event, EventId, Field, FieldValue, Level, Sink};
    use tairix_virtio::{
        ChainSegment, Direction, DmaHost, DmaSlab, RequestQueue, SplitQueue, Status, Transport,
        TRANSPORT_FEATURES, VIRTIO_F_ACCESS_PLATFORM,
    };

    /// The boot heap, in `.bss`.
    static HEAP: Heap = Heap::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and the
    /// allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

    /// The kernel service identity the misbehaving driver binds its device MSI
    /// line to and derives its capabilities under: a fixed kernel id below the
    /// task-id draw, distinct from the unlock and fault-service identities.
    const MISBEHAVE_OWNER: tairix_kernel_sec::captable::ProcessId =
        tairix_kernel_sec::captable::ProcessId(0x5b8);

    /// Witness marker ids the driver emits onto the audit sink, far above the
    /// `AuditEvent` id range so they cannot collide with a kernel event.
    const CANARY_CLEAN_EVENT_ID: u32 = 0x7E57_0001;
    const DRIVER_STAGE_EVENT_ID: u32 = 0x7E57_0002;
    const DECLARED_EVENT_ID: u32 = 0x7E57_0003;

    /// Descriptors the driver's queue holds: a request at a time, the rest
    /// letting them rotate.
    const REQUESTS: u16 = 8;

    /// Budgets of the device class the driver gives the fault service to
    /// record the refusal before it calls the run failed.
    const FAULT_RECORD_WAITS: u64 = 3;

    /// Where the driver waits for the fault service's record of the refusal.
    static FAULT_RECORDED: WaitQueue = WaitQueue::new();

    /// What the run has witnessed, a bit each.
    const TRANSLATING: u32 = 1 << 0;
    const MASTERED: u32 = 1 << 1;
    const DECLARED: u32 = 1 << 2;
    const FAULTED: u32 = 1 << 3;
    const CANARY_CLEAN: u32 = 1 << 4;
    const WITHDRAWN: u32 = 1 << 5;
    const EVERY_WITNESS: u32 =
        TRANSLATING | MASTERED | DECLARED | FAULTED | CANARY_CLEAN | WITHDRAWN;

    /// The canary's page: an IOVA's low twelve bits are the offset a unit's
    /// record need not keep.
    const PAGE_MASK: u64 = !(PAGE_SIZE as u64 - 1);

    /// Replays every event to serial and judges the run on its witnesses, each
    /// accepted only once those it follows have arrived.
    struct FaultSink {
        seen: AtomicU32,
        /// The node granted bus mastering once the unit translates.
        node: AtomicU64,
        /// What the driver declared it would aim at: its unit, its streams'
        /// first and count, and the canary's page.
        unit: AtomicU64,
        first_stream: AtomicU64,
        streams: AtomicU64,
        canary: AtomicU64,
    }

    impl FaultSink {
        /// Record `witness`, failing the run unless every one of `after` had
        /// arrived first; the one that completes the set passes it.
        fn witness(&self, witness: u32, after: u32) {
            let seen = self.seen.fetch_or(witness, Ordering::AcqRel);
            if seen & after != after {
                qemu_exit::exit_failure();
            }
            if seen | witness == EVERY_WITNESS {
                qemu_exit::exit_success();
            }
        }

        fn seen(&self, witness: u32) -> bool {
            self.seen.load(Ordering::Acquire) & witness != 0
        }

        /// A function's bus mastering changed: granted only once a unit
        /// translates, and withdrawn, read back, from the node granted it once
        /// the canary is judged.
        fn mastering(&self, event: &Event<'_>) {
            let node = field_u64(event, "node");
            let applied = field_str(event, "outcome") == Some("applied");
            match (field_str(event, "master"), node) {
                (Some("on"), Some(node)) if applied => {
                    self.node.store(node, Ordering::Release);
                    self.witness(MASTERED, TRANSLATING);
                }
                (Some("off"), Some(node))
                    if applied && node == self.node.load(Ordering::Acquire) =>
                {
                    self.witness(WITHDRAWN, MASTERED | CANARY_CLEAN);
                }
                _ => qemu_exit::exit_failure(),
            }
        }

        /// The driver named what it is about to aim at.
        fn declared(&self, event: &Event<'_>) {
            let (Some(unit), Some(first), Some(count), Some(canary)) = (
                field_u64(event, "unit"),
                field_u64(event, "first_stream"),
                field_u64(event, "streams"),
                field_u64(event, "canary"),
            ) else {
                qemu_exit::exit_failure();
            };
            self.unit.store(unit, Ordering::Release);
            self.first_stream.store(first, Ordering::Release);
            self.streams.store(count, Ordering::Release);
            self.canary.store(canary & PAGE_MASK, Ordering::Release);
            self.witness(DECLARED, MASTERED);
        }

        /// The refused write, recorded against everything the driver declared.
        fn fault(&self, event: &Event<'_>) {
            let first = self.first_stream.load(Ordering::Acquire);
            let ours = self.seen(DECLARED)
                && field_u64(event, "node") == Some(self.node.load(Ordering::Acquire))
                && field_u64(event, "unit") == Some(self.unit.load(Ordering::Acquire))
                && field_u64(event, "stream").is_some_and(|stream| {
                    (first..first + self.streams.load(Ordering::Acquire)).contains(&stream)
                })
                && (field_u64(event, "iova").map(|iova| iova & PAGE_MASK)
                    == Some(self.canary.load(Ordering::Acquire))
                    || !crate::ADDRESS_RECORDED)
                && field_str(event, "reason") == Some("unmapped")
                && (field_str(event, "access") == Some("write") || !crate::DIRECTION_RECORDED);
            if !ours {
                qemu_exit::exit_failure();
            }
            self.witness(FAULTED, TRANSLATING | DECLARED);
            // The service drains with the unit unlocked, in its own task.
            if let Some(arch) = wait_arch() {
                FAULT_RECORDED.wake_all(arch);
            }
        }
    }

    impl Sink for FaultSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            let id = event.id.0;
            if id == AuditEvent::DmaTranslationUnit.id().0 {
                // A unit that did not come up translating with its fault path
                // served cannot deliver the live fault, so the run proves
                // nothing.
                if field_str(event, "outcome") == Some("translating")
                    && field_u64(event, "stopped") == Some(0)
                    && field_u64(event, "refused") == Some(0)
                {
                    self.witness(TRANSLATING, 0);
                } else {
                    qemu_exit::exit_failure();
                }
            } else if id == AuditEvent::DmaTranslationFault.id().0 {
                self.fault(event);
            } else if id == AuditEvent::DmaBusMaster.id().0 {
                self.mastering(event);
            } else if id == DECLARED_EVENT_ID {
                self.declared(event);
            } else if id == CANARY_CLEAN_EVENT_ID {
                self.witness(CANARY_CLEAN, FAULTED);
            }
        }
    }

    static AUDIT_SINK: FaultSink = FaultSink {
        seen: AtomicU32::new(0),
        node: AtomicU64::new(u64::MAX),
        unit: AtomicU64::new(u64::MAX),
        first_stream: AtomicU64::new(u64::MAX),
        streams: AtomicU64::new(0),
        canary: AtomicU64::new(u64::MAX),
    };

    /// The bin-local PID 1 spawn seam: the production PID 1 build, with the
    /// misbehaving driver admitted in place of root unlock before the dispatch
    /// loop.
    static DMA_FAULT_INIT_SPAWN: X86_64InitSpawn =
        X86_64InitSpawn::with_pre_dispatch(misbehave_pre_dispatch);

    /// A panic halts the guest; the run times out and fails loud.
    #[panic_handler]
    fn tairix_dma_fault_qemu_x86_64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_kernel_core(info)
    }

    /// The symbol the arch crate's boot trampoline calls.
    #[no_mangle]
    pub extern "C" fn kernel_main(multiboot_info: u64) -> ! {
        boot_with_init(
            multiboot_info,
            &ALLOCATOR,
            &SERIAL_SINK,
            &AUDIT_SINK,
            Level::Info,
            &DMA_FAULT_INIT_SPAWN,
        )
    }

    /// Admit the misbehaving driver kthread before the dispatch loop diverges.
    /// It runs on first dispatch, so it parks normally. Nothing else is
    /// released: the run is judged on the witness sink alone.
    fn misbehave_pre_dispatch(ctx: &'static (dyn InitSpawnCtx + Sync)) {
        let Some(audit) = ctx.static_audit() else {
            qemu_exit::exit_failure();
        };
        let body = Box::new(move |yielder: &mut dyn YieldHandle| {
            let Err(stage) = run_driver(ctx, audit, yielder);
            log(audit, Level::Error, DRIVER_STAGE_EVENT_ID, stage, &[]);
            qemu_exit::exit_failure();
        });
        if ctx.spawn_kernel_service(body).is_none() {
            qemu_exit::exit_failure();
        }
    }

    fn log(audit: &(dyn Sink + Sync), level: Level, id: u32, message: &str, fields: &[Field<'_>]) {
        tairix_log::log(
            audit,
            &Event {
                level,
                id: EventId(id),
                message,
                fields,
            },
        );
    }

    /// The capabilities the driver reaches its hardware through: the DMA host's
    /// `alloc_dma` gates on `CAP_MEM_DMA`, the register-window map on
    /// `CAP_MMIO_MAP`.
    fn driver_caps() -> CapabilitySet {
        let mut caps = CapabilitySet::empty();
        caps.insert(CapabilityId::MMIO_MAP);
        caps.insert(CapabilityId::MEM_DMA);
        caps
    }

    /// The translated virtio-blk node the probe published, and the streams its
    /// unit knows its DMA by.
    fn translated_block_node() -> Result<(HwNode, IommuStreams), &'static str> {
        let snapshot = HW_TREE_SOURCE
            .snapshot()
            .map_err(|_| "hardware-tree snapshot")?;
        let key = HwMatchKey::virtio(VIRTIO_BLK_DEVICE_ID);
        snapshot_nodes(&snapshot)
            .ok_or("hardware-tree decode")?
            .find_map(|node| {
                let streams = node
                    .resources()
                    .iter()
                    .find_map(|resource| resource.iommu_streams().ok())?;
                node.match_keys().contains(&key).then_some((node, streams))
            })
            .ok_or("no translated virtio-blk node")
    }

    /// A carve, and the segment handing the device its first `len` bytes: a
    /// carve spans whole pages, which the device would otherwise take as
    /// part of the request.
    struct Staged {
        slab: DmaSlab,
        segment: ChainSegment,
    }

    impl Staged {
        fn carve(host: &FloorHost, len: usize, direction: Direction) -> Result<Self, &'static str> {
            let slab = host.alloc_dma_zeroed(len).map_err(|_| "request carve")?;
            let segment = ChainSegment {
                device_addr: slab.device_addr(),
                len: u32::try_from(len).map_err(|_| "segment length")?,
                direction,
            };
            Ok(Self { slab, segment })
        }
    }

    /// The device as the driver drives it: its one request queue, and the
    /// header and status every request it makes reuses.
    struct Block {
        transport: PciTransport,
        host: &'static FloorHost,
        rq: RequestQueue,
        header: Staged,
        status: Staged,
        budget_ns: u64,
    }

    impl Block {
        /// Drive `device` as a block device whose DMA its unit translates.
        fn open(device: FloorDevice) -> Result<Self, &'static str> {
            let FloorDevice {
                mut transport,
                host,
            } = device;
            let negotiated = tairix_virtio::negotiate(
                &mut transport,
                || host.device_quiesced(),
                |offered| Ok::<_, tairix_virtio::VirtioError>(offered & TRANSPORT_FEATURES),
            )
            .map_err(|_| "feature negotiation")?;
            if negotiated.features & VIRTIO_F_ACCESS_PLATFORM == 0 {
                return Err("the device would reach memory around its unit");
            }
            let mut header = Staged::carve(host, wire::HEADER_LEN, Direction::DeviceRead)?;
            header.slab.as_bytes_mut()[0..4].copy_from_slice(&wire::VIRTIO_BLK_T_IN.to_le_bytes());
            let status = Staged::carve(host, wire::STATUS_LEN, Direction::DeviceWrite)?;
            let queue = SplitQueue::new(&mut transport, host, 0, REQUESTS, wire::REQUEST_CHAIN_LEN)
                .map_err(|_| "queue setup")?;
            transport.set_status(negotiated.status.with(Status::DRIVER_OK));
            Ok(Self {
                transport,
                host,
                rq: RequestQueue::new(queue),
                header,
                status,
                budget_ns: BlkDeviceClass::Virtual.budget().deadline_ns,
            })
        }

        /// Ask the device to write sector 0 through `into`, answering the
        /// bytes it says it wrote.
        fn read_sector0(&mut self, into: ChainSegment) -> Result<u32, DriverError> {
            self.status.slab.as_bytes_mut()[0] = wire::STATUS_UNANSWERED;
            let chain = [self.header.segment, into, self.status.segment];
            self.rq
                .submit_and_wait(&mut self.transport, self.host, &chain, self.budget_ns)
                .map(|used| used.written)
        }

        fn answered(&self) -> u8 {
            self.status.slab.as_bytes()[0]
        }
    }

    /// A zeroed page of ours that the device's domain does not map: a write
    /// that leaked past the unit would change it.
    struct Canary {
        iova: u64,
        bytes: NonNull<u8>,
    }

    impl Canary {
        fn zeroed(frames: &FrameAllocator) -> Result<Self, &'static str> {
            let frame = frames
                .alloc(MemoryClass::Kernel)
                .map_err(|_| "canary frame")?;
            let bytes = SPAWN_TABLE_PHYSMAP
                .translate(frame.start(), PAGE_SIZE)
                .ok_or("canary not in direct map")?;
            for offset in 0..PAGE_SIZE {
                // SAFETY: `bytes` covers `PAGE_SIZE` bytes of a frame just
                // allocated, which no CPU mapping but the direct map's
                // reaches; the device is the one other writer it may have, so
                // every access is volatile.
                unsafe { bytes.as_ptr().add(offset).write_volatile(0) };
            }
            Ok(Self {
                iova: frame.start().as_u64(),
                bytes,
            })
        }

        fn clean(&self) -> bool {
            (0..PAGE_SIZE).all(|offset| {
                // SAFETY: the frame and pointer `zeroed` wrote through, read
                // volatile for the same reason.
                unsafe { self.bytes.as_ptr().add(offset).read_volatile() == 0 }
            })
        }
    }

    /// Declare to the witness sink the unit and `streams` the device masters
    /// as, and the `canary` it is about to be aimed at.
    fn declare(audit: &(dyn Sink + Sync), streams: IommuStreams, canary: &Canary) {
        let field = |key, value| Field {
            key,
            value: FieldValue::UnsignedInt(value),
        };
        log(
            audit,
            Level::Info,
            DECLARED_EVENT_ID,
            "dma-fault vertical: aiming a write at the canary",
            &[
                field("unit", u64::from(streams.unit())),
                field("first_stream", u64::from(streams.first())),
                field("streams", u64::from(streams.count())),
                field("canary", canary.iova),
            ],
        );
    }

    /// Park until the fault service has recorded the refusal, giving it
    /// [`FAULT_RECORD_WAITS`] of the device's budgets.
    fn await_fault_record(
        parker: &Parker,
        yielder: &mut dyn YieldHandle,
        budget_ns: u64,
    ) -> Result<(), &'static str> {
        let due = parker
            .now_ns()
            .saturating_add(budget_ns.saturating_mul(FAULT_RECORD_WAITS));
        loop {
            // Registered before the check, so a record landing in between
            // re-readies the park instead of being slept through.
            parker.register(&FAULT_RECORDED, Some(due));
            if AUDIT_SINK.seen(FAULTED) {
                return Ok(());
            }
            if parker.now_ns() >= due {
                return Err("the refused write was never recorded");
            }
            yielder.park();
        }
    }

    /// Bring the misbehaving virtio-blk-PCI device up through its node's
    /// translation domain, read the planted sector through it, then provoke
    /// one live fault and prove the refused write landed nowhere. On success
    /// it parks for life; the witness sink ends the run.
    fn run_driver(
        ctx: &'static (dyn InitSpawnCtx + Sync),
        audit: &'static (dyn Sink + Sync),
        yielder: &mut dyn YieldHandle,
    ) -> Result<Infallible, &'static str> {
        let frames = ctx.static_frames().ok_or("no kernel frame allocator")?;
        let translation = ctx.dma_translation().ok_or("no dma translation unit")?;
        let parker = Parker::current().ok_or("the driver cannot park")?;
        let (node, streams) = translated_block_node()?;
        let caller: &'static TaskCapabilities = Box::leak(Box::new(TaskCapabilities::derive(
            MISBEHAVE_OWNER,
            UserId(0),
            driver_caps(),
            driver_caps(),
            audit,
        )));
        let mut block = Block::open(bring_up_virtio_pci(
            ctx,
            &node,
            caller,
            audit,
            frames,
            Confinement::Translated,
        )?)?;

        // The device reaches its domain's mappings through the unit: the
        // planted sector reads back whole.
        let sector = usize::try_from(wire::SECTOR_SIZE).map_err(|_| "sector size")?;
        let data = Staged::carve(block.host, sector, Direction::DeviceWrite)?;
        let written = block
            .read_sector0(data.segment)
            .map_err(|_| "the planted sector's read did not complete through the unit")?;
        match block.answered() {
            wire::STATUS_OK => {}
            wire::STATUS_IOERR => return Err("the planted sector's read failed"),
            wire::STATUS_UNSUPP => return Err("the planted sector's read was refused"),
            wire::STATUS_UNANSWERED => return Err("the planted sector's read was not answered"),
            _ => return Err("the planted sector's read answered no status the device has"),
        }
        if usize::try_from(written).ok() != Some(sector + wire::STATUS_LEN) {
            return Err("the device vouched for other than the planted sector");
        }
        let planted = data.slab.as_bytes()[..sector]
            .iter()
            .enumerate()
            .all(|(i, &byte)| byte == sector0_byte(i));
        if !planted {
            return Err("the planted sector read back changed");
        }

        // QEMU bounces a mapping its unit refuses and drops the write when it
        // unmaps it, so the request may well complete: the refusal is proven by
        // the unit's record and the canary, never by the device's answer.
        let canary = Canary::zeroed(frames)?;
        declare(audit, streams, &canary);
        let aimed = ChainSegment {
            device_addr: canary.iova,
            len: wire::SECTOR_SIZE,
            direction: Direction::DeviceWrite,
        };
        match block.read_sector0(aimed) {
            Ok(_) | Err(DriverError::DeviceOffline) => {}
            Err(_) => return Err("the aimed request broke the queue"),
        }

        // Judged only once the unit's refusal is recorded: the device may try
        // the write after the request's wait gave up on it.
        await_fault_record(&parker, yielder, block.budget_ns)?;
        if !canary.clean() {
            return Err("refused DMA write reached the canary page");
        }
        log(
            audit,
            Level::Info,
            CANARY_CLEAN_EVENT_ID,
            "dma-fault vertical: canary clean",
            &[],
        );

        // End the owner the way every driver's end does: the function stops
        // mastering before its domain goes.
        if !translation.revoke(node.id(), KERNEL_OWNER) {
            return Err("the unit could not confirm the owner's end");
        }

        // The witness sink ends the run. Parked for life, so nothing the device
        // was handed is freed.
        loop {
            yielder.park();
        }
    }
}
