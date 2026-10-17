//! The `Run` entry-point binary of the USB **host-controller driver** (HCD),
//! installed as a signed `/System/Drivers/` bundle and autoloaded into user
//! space by `devmgr` when an xHCI controller is discovered — the `usb,xhci`
//! node a bus driver publishes, or a PCI function of the xHCI class
//! (`plans/USB.md` U3b).
//!
//! This process is the **sole owner** of one xHCI controller: it maps the
//! controller's register BAR, owns its DMA rings and root-hub ports, brings
//! it up, and enumerates every attached device. It then **publishes one
//! hardware-tree node per USB interface** (carrying the device's
//! `vid:pid:class` match keys) so `devmgr` autoloads the matching **class**
//! driver (`drivers/input/usb_hid`, …), and **serves that class driver's URB
//! transfers** over the bus-agnostic URB transport seam. It names no class
//! driver, no board, and no private bus implementation.
//!
//! It holds no class-specific authority: the class driver interprets what its
//! device sends; this HCD only moves bytes between the controller and the
//! shared buffer.
//!
//! # The asynchronous event loop (`plans/USB.md` §1.1)
//!
//! The HCD multiplexes two independent event streams on one kernel **wait-set**
//! (`U3a3`) and never waits on either inside the other's handler:
//!
//! * **A URB-submit IPC call** on the per-interface endpoint it serves: it
//!   `call_recv`s the call and drives it. One that does not finish at once —
//!   a report not yet arrived, a control or bulk transfer, an interface
//!   operation the engine runs over several commands — is held, and its
//!   class driver's `ipc_call` parks in the kernel until it is answered.
//! * **The controller's completion interrupt**, acknowledged with the single
//!   `USBSTS` read that also carries the fault latch.
//!
//! Every wake then runs one service step of the engine
//! ([`tairix_usb::device::UsbHost::service`]): it routes every event the
//! controller posted, captures reports with their endpoints re-armed, and runs
//! every operation — an enumeration, a port reset, a pipe recovery — that an
//! event or a due timer moved. Its topology report publishes and retracts
//! interface nodes, held calls are driven against what it did, and the loop
//! parks until the earliest deadline the engine, a held URB or a retry owes.
//! One slow device holds no other device's step.
//!
//! Capturing reports in the service step rather than only when a class driver
//! submits decouples device polling from a CPU-starved class driver, so reports
//! are never dropped under load (`plans/USB.md`).
//!
//! # Per-interrupt cost
//!
//! A device streaming reports — a mouse in motion, at the 1 ms
//! interrupt-moderation ceiling — makes this path run ~1000 times a second, so
//! what it touches per pass is the driver's whole steady-state CPU cost. Three
//! properties keep it small, each with a budget regression in `lib/usb`:
//!
//! * **One register read.** The port scan is armed by a latched event rather
//!   than run unconditionally, and the fault check rides the acknowledgement's
//!   own read. On a PCIe controller a register read is a non-posted round trip
//!   and is the most expensive operation here.
//! * **One TRB per ring probe.** The engine reads the single 16-byte entry at
//!   the dequeue point, never the whole segment out of non-cacheable memory.
//! * **No allocation.** The path holds only fixed-capacity state; the heap is
//!   touched on attach and detach alone.
//!
//! # Data path (`plans/USB.md` U3a2, Option B)
//!
//! The URB data buffer is a cross-process **shared-memory** region this HCD
//! creates (`shm_create`) for each interface node it publishes, and no other
//! node ever carries, forwarded as a grant on that node; the class driver
//! inherits the grant and `shm_map`s the same frames. The buffer is plain
//! cacheable RAM with no DMA properties — the class driver holds **zero** DMA
//! authority — and the HCD bounce-copies between it and its own DMA-granted
//! ring.
//!
//! It is a **pure-Rust** program: it links the Rust userland
//! runtime `tairix-rt` (`_start`, the stack canary, the panic handler, the
//! syscall wrappers); on the host it is an inert stub so `cargo build
//! --workspace`, clippy, and fmt still cover the file. The live controller
//! bring-up and report path are metal-only because QEMU models no Pi USB; the
//! HCD's host-testable logic lives in the crate's `lib` target
//! ([`tairix_drv_bus_usb::bringup`], [`tairix_drv_bus_usb::serve`],
//! [`tairix_drv_bus_usb::interfaces`]).

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(any(freestanding, test))]
fn waitset_ctl_result(ret: i64) -> Result<(), i64> {
    if ret == 0 {
        Ok(())
    } else {
        Err(ret)
    }
}

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {
    use tairix_abi::hwtree::HW_NODE_ROOT;
    use tairix_abi::usb_urb::{IsoNotify, UsbTransferType, USB_REPLY_MAX_LEN, USB_REQUEST_MAX_LEN};
    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind, WAITSET_TIMEOUT_NONE};
    use tairix_abi::{CapabilityId, DriverError, Errno, HwNode, ProcId, RegisterWindow};
    use tairix_caps::CapabilitySet;
    use tairix_drv_bus_usb::bringup::{
        bring_up_controller_diagnostic, derive_controller_resources, BringupPhase, ControllerHost,
    };
    use tairix_drv_bus_usb::domain::{ControllerDomainEvent, ControllerHealth, Retries};
    use tairix_drv_bus_usb::interfaces::{Interfaces, Note, Seam, UrbBuffer, ENDPOINT_CAPACITY};
    use tairix_drv_bus_usb::serve::{urb_failure_level, UrbReply};
    use tairix_drvrt::exit::{end, DriverExit};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_log::{log, Event, EventId, Field, Level};
    use tairix_rt::shm::SharedRegion;
    use tairix_rt::LogSink;
    use tairix_usb::device::{
        DeviceEngine, DeviceIdentity, HubEvent, Serviced, MAX_INTERFACES, XHCI_MAX_SLOTS,
    };
    use tairix_usb::{SlabBank, XhciOpenStage};
    use tairix_util::fmt::format_hex_u64;

    /// Diagnostic event id: a one-shot controller bring-up failure.
    const HCD_BRINGUP_FAILED: EventId = EventId(27_026);

    /// Diagnostic event id: the one-shot "controller up, serving URBs" beacon.
    const HCD_READY: EventId = EventId(27_001);

    /// Diagnostic event id: the device disconnected and its interface node was
    /// retracted.
    const HCD_DISCONNECT: EventId = EventId(27_027);

    /// Diagnostic event id: a device (re)attached behind the hub and a fresh
    /// interface node was published.
    const HCD_ATTACHED: EventId = EventId(27_056);

    /// Diagnostic event id: URB transport setup or IRQ arming state.
    const HCD_URB_SETUP: EventId = EventId(27_049);

    /// Diagnostic event id: a URB reply was sent or attempted.
    const HCD_URB_REPLY: EventId = EventId(27_052);

    /// Diagnostic event id: a wait-set or IPC transport error happened.
    const HCD_WAIT_ERROR: EventId = EventId(27_054);

    /// Diagnostic event id: a URB failed, was refused, or was abandoned at its
    /// deadline, with the controller's verdict on the endpoint it ran on.
    const HCD_URB_FAILED: EventId = EventId(27_153);

    /// Diagnostic event id: the endpoint of a transfer abandoned at its
    /// deadline stopped, and where the controller then stood against it.
    const HCD_ABANDON_SETTLED: EventId = EventId(27_154);

    /// Security event id: a class driver's control request reached past its
    /// own interface and was refused.
    const HCD_URB_REFUSED: EventId = EventId(27_149);

    /// Interior fault-domain event id: the controller faulted and the whole
    /// subtree entered its shared recovery grace window (`plans/FIX-IO.md`
    /// IO4/IO5). Classified through the shared `for_fault_domain` vocabulary.
    const HCD_DOMAIN_RECOVERING: EventId = EventId(27_090);

    /// Interior fault-domain event id: the controller demonstrably returned and
    /// the subtree recovered with no reboot.
    const HCD_DOMAIN_RECOVERED: EventId = EventId(27_091);

    /// Interior fault-domain event id: the recovery grace window elapsed with
    /// the controller still faulted — the subtree is failed closed (the
    /// fault-domain owner's own distinct fail-closed event, sticky but
    /// recoverable).
    const HCD_DOMAIN_OFFLINE: EventId = EventId(27_092);

    /// Reserved base of the URB call-endpoint id range the HCDs allocate from.
    ///
    /// A grant-restricted endpoint id the class driver reaches only through
    /// the kernel-minted grant on its matched node — distinct from the
    /// well-known `DRIVER_STORE_ENDPOINT`. Each controller claims one
    /// contiguous block of [`URB_ENDPOINT_BLOCK`] ids ([`claim_urb_block`]):
    /// creating a block's *base* id claims the whole block, so a second
    /// controller's HCD probes on to the next block and two controllers
    /// never collide on one id; the block's interior ids are bound lazily,
    /// one per transport, as the interface table opens them.
    const URB_ENDPOINT_BASE: u64 = 0x0055_5242_0000_0000;

    /// Ids per claimed endpoint block: one per transport, and a controller
    /// never needs more transports than interfaces it serves at once — the
    /// xHCI protocol's 255-slot ceiling plus the DCBAA's scratchpad slot
    /// ([`XHCI_MAX_SLOTS`] + 1), times the servable interfaces a composite
    /// device can put on one slot ([`MAX_INTERFACES`]). Derived from
    /// protocol maxima, never a tuning knob, so any controller's full
    /// device complement fits its block.
    const URB_ENDPOINT_BLOCK: u64 = ((XHCI_MAX_SLOTS + 1) * MAX_INTERFACES) as u64;

    /// How many [`URB_ENDPOINT_BLOCK`]-id blocks to probe before giving up
    /// (a generous bound on simultaneous controllers; the first HCD claims
    /// the first block on its first try).
    const URB_ENDPOINT_BLOCKS: u64 = 64;

    /// Wait-set token for "the controller completion interrupt fired".
    const TOKEN_IRQ: u64 = 0;
    /// Base wait-set token for "a URB submit arrived on transport slot
    /// `token - TOKEN_URB_BASE`'s endpoint".
    const TOKEN_URB_BASE: u64 = 1;

    fn log_hex_event(
        id: EventId,
        level: Level,
        message: &'static str,
        key: &'static str,
        value: u64,
    ) {
        let mut value_buf = [0u8; 16];
        log(
            &LogSink,
            &Event {
                level,
                id,
                message,
                fields: &[Field {
                    key,
                    value: tairix_log::FieldValue::Str(format_hex_u64(value, &mut value_buf)),
                }],
            },
        );
    }

    /// Diagnostic event id: an isochronous stream ended other than at its
    /// class driver's word.
    const HCD_STREAM_ENDED: EventId = EventId(27_151);

    /// Diagnostic event id: an endpoint recovery that failed was deferred to a
    /// retry, or a hub's failed status transfer was recovered, or the deferred
    /// retries settled.
    const HCD_RECOVERY: EventId = EventId(27_152);

    fn reply_to_urb(endpoint_id: u64, reply: UrbReply) {
        let ret = tairix_rt::call_reply(endpoint_id, reply.ticket, &reply.bytes[..reply.len]);
        if ret == 0 {
            log_hex_event(
                HCD_URB_REPLY,
                Level::Debug,
                "usb-hcd: URB reply sent",
                "ticket_hex",
                reply.ticket,
            );
        } else {
            log_hex_event(
                HCD_URB_REPLY,
                Level::Warn,
                "usb-hcd: URB reply failed",
                "ret_hex",
                ret.unsigned_abs(),
            );
        }
    }

    /// The HCD's own mapping of one interface node's shared URB buffer.
    struct NodeBuffer(SharedRegion);

    impl UrbBuffer for NodeBuffer {
        fn region(&self) -> u64 {
            self.0.id()
        }

        fn bytes(&mut self) -> &mut [u8] {
            self.0.bytes_mut()
        }
    }

    /// The live controller and kernel the interface table drives.
    struct Live<'d, 'h> {
        host: &'d mut ControllerHost<'h>,
        /// The event loop's wait-set.
        set: u64,
        /// This controller's endpoint block ([`claim_urb_block`]).
        urb_base: u64,
        /// This process's instance, the grantor of every stream region.
        instance: ProcId,
    }

    impl<'h> Seam for Live<'_, 'h> {
        type Buffer = NodeBuffer;
        type Engine<'a>
            = DeviceEngine<'a, 'h, RegisterWindow, SlabBank<'h>>
        where
            Self: 'a;

        fn table_len(&self) -> usize {
            self.host.view().map_or(0, |core| core.device_table_len())
        }

        fn identity(&self, index: usize) -> Option<DeviceIdentity> {
            self.host.view().ok()?.device_identity(index)
        }

        fn describe(&self, index: usize) -> Result<HwNode, DriverError> {
            self.host.view()?.describe_device(index, HW_NODE_ROOT, 0)
        }

        fn engine(&mut self, index: usize) -> Self::Engine<'_> {
            self.host.engine_for(index)
        }

        fn confirm_departure(&mut self, index: usize) -> Result<(), DriverError> {
            self.host.confirm_departure(index)
        }

        fn departure_settled(&self, index: usize) -> bool {
            self.host.departure_settled(index)
        }

        fn faulted(&mut self) -> bool {
            self.host.controller_faulted()
        }

        fn reset(&mut self) -> Result<(), DriverError> {
            // Which USBSTS fault bit latched is the only evidence a metal
            // capture gets for why the controller died (QEMU models no Pi USB).
            let (usbsts, usbcmd) = self.host.device().map_or((None, None), |mut core| {
                (core.read_usbsts(), core.read_usbcmd())
            });
            log(
                &LogSink,
                &Event {
                    level: Level::Warn,
                    id: HCD_DISCONNECT,
                    message: "usb-hcd: resetting the controller to recover it",
                    fields: &[
                        opt_u32_field("usbsts", usbsts),
                        opt_u32_field("usbcmd", usbcmd),
                    ],
                },
            );
            self.host.reset_and_reenumerate()
        }

        fn open_endpoint(&mut self, slot: usize) -> Option<u64> {
            let offset = u64::try_from(slot)
                .ok()
                .filter(|&offset| offset < URB_ENDPOINT_BLOCK)?;
            let endpoint = self.urb_base + offset;
            // Claiming the block bound its base, which serves slot 0.
            (offset == 0 || bind_urb_endpoint(endpoint)).then_some(endpoint)
        }

        fn watch_endpoint(&mut self, slot: usize, endpoint: u64) -> bool {
            let Some(token) = u64::try_from(slot)
                .ok()
                .and_then(|slot| TOKEN_URB_BASE.checked_add(slot))
            else {
                return false;
            };
            let added = tairix_rt::waitset_ctl(
                self.set,
                WaitSetOp::Add,
                WaitSourceKind::Endpoint,
                endpoint,
                token,
            );
            if super::waitset_ctl_result(added).is_err() {
                return false;
            }
            log_hex_event(
                HCD_URB_SETUP,
                Level::Info,
                "usb-hcd: URB transport created",
                "endpoint_hex",
                endpoint,
            );
            true
        }

        fn create_buffer(&mut self, len: usize) -> Option<NodeBuffer> {
            SharedRegion::create(len).map(NodeBuffer)
        }

        fn caller_pid(&mut self, endpoint: u64, ticket: u64) -> Result<u64, Errno> {
            tairix_rt::peer_origin(endpoint, ticket).map(|origin| origin.pid())
        }

        fn grant_peer(&mut self, region: u64, endpoint: u64, ticket: u64) -> Result<u64, Errno> {
            let handle = tairix_rt::shm_grant_peer(region, endpoint, ticket);
            Errno::syscall_result(handle)
        }

        fn self_instance(&self) -> ProcId {
            self.instance
        }

        fn notify(&mut self, port: u64, notice: &IsoNotify) -> Result<(), Errno> {
            match tairix_rt::ipc_send(port, &notice.encode()) {
                0 => Ok(()),
                ret => Err(Errno::from_syscall(ret)),
            }
        }

        fn receive(
            &mut self,
            endpoint: u64,
            request: &mut [u8],
        ) -> Result<Option<(u64, usize)>, Errno> {
            let mut ticket = 0u64;
            match tairix_rt::call_recv_nonblock(endpoint, request, &mut ticket) {
                Ok(len) => Ok(Some((ticket, len))),
                Err(err) => match Errno::from_syscall(err) {
                    Errno::WouldBlock => Ok(None),
                    errno => Err(errno),
                },
            }
        }

        fn reply(&mut self, endpoint: u64, reply: UrbReply) {
            reply_to_urb(endpoint, reply);
        }

        fn emit(&mut self, node: &HwNode) -> Option<u32> {
            // A negative return is the errno; anything else is the node id.
            u32::try_from(tairix_rt::hw_emit_node(node)).ok()
        }

        fn remove(&mut self, id: u32) {
            // A device that vanished is never refused for being in use, so
            // the removal is a surprise one: an empty flag set.
            if tairix_rt::hw_remove_node(id, tairix_abi::HwRemoveFlags::empty()) < 0 {
                log_hex_event(
                    HCD_WAIT_ERROR,
                    Level::Warn,
                    "usb-hcd: interface retraction failed",
                    "node_hex",
                    u64::from(id),
                );
            }
        }

        fn now_ns(&self) -> u64 {
            tairix_rt::clock_get()
        }

        fn note(&mut self, note: Note) {
            match note {
                Note::Published { node } => {
                    log_hex_event(
                        HCD_ATTACHED,
                        Level::Info,
                        "usb-hcd: interface node emitted",
                        "node_hex",
                        u64::from(node),
                    );
                }
                Note::UrbFailed {
                    index,
                    transfer,
                    errno,
                } => log_urb_error(self.host, index, transfer, errno),
                Note::FaultDetached => {
                    log(
                        &LogSink,
                        &Event {
                            level: Level::Info,
                            id: HCD_DISCONNECT,
                            message: "usb-hcd: device transfer fault confirmed disconnect, interface retracted",
                            fields: &[],
                        },
                    );
                }
                Note::DetachUnconfirmed(err) => log_hex_event(
                    HCD_WAIT_ERROR,
                    Level::Warn,
                    "usb-hcd: disconnect confirmation after transfer fault failed",
                    "err_hex",
                    err as u64,
                ),
                Note::ReceiveFailed(errno) => log_hex_event(
                    HCD_WAIT_ERROR,
                    Level::Warn,
                    "usb-hcd: call_recv failed after endpoint wake",
                    "errno_hex",
                    errno as u64,
                ),
                Note::Domain { event, owner } => log_domain_event(event, owner),
                Note::StreamEnded {
                    index,
                    endpoint,
                    reason,
                } => {
                    log(
                        &LogSink,
                        &Event {
                            level: Level::Info,
                            id: HCD_STREAM_ENDED,
                            message: "usb-hcd: isochronous stream ended",
                            fields: &[
                                Field {
                                    key: "index",
                                    value: tairix_log::FieldValue::UnsignedInt(index as u64),
                                },
                                Field {
                                    key: "endpoint",
                                    value: tairix_log::FieldValue::UnsignedInt(u64::from(endpoint)),
                                },
                                Field {
                                    key: "errno",
                                    value: tairix_log::FieldValue::UnsignedInt(reason as u64),
                                },
                            ],
                        },
                    );
                }
            }
        }
    }

    /// Log what one service step did to the topology.
    fn log_topology(event: HubEvent) {
        let (level, id, message) = match event {
            HubEvent::Attached(_) => (Level::Info, HCD_READY, "usb-hcd: device attached and served"),
            HubEvent::HubAttached(_) => (Level::Info, HCD_READY, "usb-hcd: hub attached and watched"),
            HubEvent::Detached(_) => (
                Level::Info,
                HCD_DISCONNECT,
                "usb-hcd: device disconnected, interface retracted",
            ),
            HubEvent::HubDetached(_) => (
                Level::Info,
                HCD_DISCONNECT,
                "usb-hcd: hub disconnected with everything behind it",
            ),
            HubEvent::HubRecovered(_) => (
                Level::Warn,
                HCD_RECOVERY,
                "usb-hcd: a hub's status transfer failed; its endpoint was recovered and its ports reconciled",
            ),
        };
        log(
            &LogSink,
            &Event {
                level,
                id,
                message,
                fields: &[],
            },
        );
    }

    /// Audit a controller fault-domain edge, naming the controller's owner
    /// id, and publish the state it leaves onto the controller's own
    /// hardware-tree node (`plans/FIX-IO.md` IO4), so the leaves below read
    /// one recovery episode rather than a failure each. The publish is
    /// best-effort: the audit record is authoritative, and a refused publish
    /// never fails the recovery.
    fn log_domain_event(event: ControllerDomainEvent, owner: u32) {
        let (id, level, message, health) = match event {
            ControllerDomainEvent::Recovering => (
                HCD_DOMAIN_RECOVERING,
                Level::Warn,
                "usb-hcd: controller faulted, subtree held recovering under one grace window",
                tairix_abi::blkio::FaultDomainState::Recovering,
            ),
            ControllerDomainEvent::Recovered => (
                HCD_DOMAIN_RECOVERED,
                Level::Info,
                "usb-hcd: controller returned, subtree recovered",
                tairix_abi::blkio::FaultDomainState::Healthy,
            ),
            // The serve loop exits on this edge, so it states why.
            ControllerDomainEvent::FailedClosed => (
                HCD_DOMAIN_OFFLINE,
                Level::Error,
                "usb-hcd: controller did not come back within its grace window; interfaces retracted, exiting",
                tairix_abi::blkio::FaultDomainState::Offline,
            ),
        };
        log_hex_event(id, level, message, "owner_hex", u64::from(owner));
        let _ = tairix_rt::hw_node_health(health);
    }

    /// Spend the single deferred re-attach the bring-up walk owed a port it
    /// could not serve (`SkippedPortRetry`): every connected but unserved
    /// port is looked at again, its end reported as the walk's settle.
    ///
    /// A port skipped by the walk had its connect latch consumed there, so no
    /// hot-plug event will ever wake it again — this is the one chance a
    /// device that merely lost the boot race gets before a user has to unplug
    /// it. The retry is never re-armed, so a genuinely broken device cannot
    /// loop.
    fn retry_skipped_ports(host: &mut ControllerHost<'_>) {
        host.retry_skipped_ports();
    }

    /// Run the deferred retry of every endpoint recovery that failed.
    fn retry_owed_recoveries(host: &mut ControllerHost<'_>) {
        host.retry_owed_recoveries();
        if !host.recovery_owed() {
            log(
                &LogSink,
                &Event {
                    level: Level::Info,
                    id: HCD_RECOVERY,
                    message: "usb-hcd: deferred endpoint recovery settled",
                    fields: &[],
                },
            );
        }
    }

    /// Bind the URB transport endpoint `id`. Binding it grant-restricted
    /// (`send_caps` carries `CAP_IPC_ENDPOINT`) makes the kernel mint this
    /// HCD the matching per-endpoint grant, which it forwards onto the
    /// interface node so the class driver inherits exactly the right to
    /// submit URBs on this one interface. `false` if the kernel refused the
    /// id (already bound, or the create was rejected).
    fn bind_urb_endpoint(id: u64) -> bool {
        let mut send_caps = CapabilitySet::empty();
        send_caps.insert(CapabilityId::IPC_ENDPOINT);
        let recv_caps = CapabilitySet::empty();
        tairix_rt::call_create(
            id,
            &send_caps,
            &recv_caps,
            USB_REQUEST_MAX_LEN,
            USB_REPLY_MAX_LEN,
            ENDPOINT_CAPACITY,
        ) == 0
    }

    /// Claim this controller's URB endpoint-id block: binding a block's
    /// *base* id claims the whole block — that create is the only contended
    /// one, so a second controller's HCD moves on to the next block and two
    /// controllers never collide on an id. The block's interior ids — one
    /// per transport — are bound as the interface table opens them
    /// ([`Seam::open_endpoint`]), so an idle controller holds one endpoint,
    /// not a table of them. Returns the claimed base id, or `None` when
    /// every block is taken.
    fn claim_urb_block() -> Option<u64> {
        for block in 0..URB_ENDPOINT_BLOCKS {
            let base = URB_ENDPOINT_BASE + block * URB_ENDPOINT_BLOCK;
            if bind_urb_endpoint(base) {
                return Some(base);
            }
        }
        None
    }

    /// Log the interrupt reports dropped since the last look, because a class
    /// driver stalled past the buffer depth, so a stuck consumer is never
    /// silent.
    fn note_dropped_reports(host: &ControllerHost<'_>, reported_drops: &mut u64) {
        let dropped = host
            .view()
            .map_or(*reported_drops, |core| core.dropped_report_total());
        if dropped > *reported_drops {
            log_hex_event(
                HCD_WAIT_ERROR,
                Level::Warn,
                "usb-hcd: interrupt reports dropped; class driver stalled",
                "dropped",
                dropped,
            );
            *reported_drops = dropped;
        }
    }

    /// Log where each abandoned transfer's stopped endpoint stood against it:
    /// a device still working on the transfer is told from a completion the
    /// host lost.
    fn log_abandon_reports(host: &ControllerHost<'_>) {
        let u = tairix_log::FieldValue::UnsignedInt;
        while let Some((index, at)) = host
            .device()
            .ok()
            .and_then(|mut core| core.take_abandon_report())
        {
            log(
                &LogSink,
                &Event {
                    level: Level::Warn,
                    id: HCD_ABANDON_SETTLED,
                    message: "usb-hcd: an abandoned transfer's endpoint stopped",
                    fields: &[
                        Field {
                            key: "index",
                            value: u(index as u64),
                        },
                        Field {
                            key: "dci",
                            value: u(u64::from(at.dci)),
                        },
                        Field {
                            key: "td_progress",
                            value: tairix_log::FieldValue::Str(at.progress.as_str()),
                        },
                    ],
                },
            );
        }
    }

    /// A diagnostic field carrying a controller value that may not have been
    /// readable: an unreadable register is logged `Null`, never a fabricated
    /// zero.
    fn opt_u32_field(key: &'static str, value: Option<u32>) -> Field<'static> {
        Field {
            key,
            value: value.map_or(tairix_log::FieldValue::Null, |v| {
                tairix_log::FieldValue::UnsignedInt(u64::from(v))
            }),
        }
    }

    /// A URB failed: log the errno the class driver will see **and** the
    /// engine's latched raw completion code for the endpoint it ran on — the
    /// report endpoint's for a report poll, the bulk pipe's for a bulk
    /// transfer — so a metal capture shows the controller's verdict
    /// (transaction error, stall, babble, …) behind the coarse errno. One
    /// abandoned at its deadline carries its endpoint's EP State instead;
    /// where the controller stood against it follows once the endpoint
    /// stops ([`log_abandon_reports`]).
    fn log_urb_error(
        host: &ControllerHost<'_>,
        index: usize,
        transfer: UsbTransferType,
        errno: Errno,
    ) {
        let (id, message) = if errno == Errno::PermissionDenied {
            (
                HCD_URB_REFUSED,
                "usb-hcd: a class driver's request outside its interface was refused",
            )
        } else {
            (HCD_URB_FAILED, "usb-hcd: URB failed")
        };
        // A transfer abandoned at its deadline ended with no completion.
        let fault_code = host.view().map_or(0, |core| match transfer {
            _ if errno == Errno::TimedOut => 0,
            UsbTransferType::Bulk => core.last_bulk_fault_code(index),
            UsbTransferType::Interrupt => core.last_report_fault_code(index),
            UsbTransferType::Control => 0,
        });
        let ep_state = (errno == Errno::TimedOut)
            .then(|| host.view().ok()?.abandoned_ep_state(index))
            .flatten();
        let u = tairix_log::FieldValue::UnsignedInt;
        log(
            &LogSink,
            &Event {
                level: urb_failure_level(errno),
                id,
                message,
                fields: &[
                    Field {
                        key: "index",
                        value: u(index as u64),
                    },
                    Field {
                        key: "errno",
                        value: u(errno as u64),
                    },
                    Field {
                        key: "fault_code",
                        value: u(u64::from(fault_code)),
                    },
                    Field {
                        key: "ep_state",
                        value: ep_state
                            .map_or(tairix_log::FieldValue::Null, |state| u(u64::from(state))),
                    },
                ],
            },
        );
    }

    /// A service step that could not run: the event ring could not be read.
    fn log_service_failure(err: DriverError) {
        log_hex_event(
            HCD_WAIT_ERROR,
            Level::Warn,
            "usb-hcd: controller service step failed",
            "err_hex",
            err as u64,
        );
    }

    /// Emit the one-shot controller bring-up failure with its **whole**
    /// breadcrumb: QEMU models no Pi USB, so this diagnostic is how a metal
    /// run localises the stall, the phase-specific controller state always
    /// included.
    fn log_bringup_failure(err: &tairix_drv_bus_usb::bringup::ControllerBringupError) {
        let phase = Field {
            key: "phase",
            value: tairix_log::FieldValue::Str(err.phase.as_str()),
        };
        let error = Field {
            key: "error",
            value: tairix_log::FieldValue::UnsignedInt(err.error as u64),
        };
        let event = |fields: &[Field<'_>]| {
            log(
                &LogSink,
                &Event {
                    level: Level::Error,
                    id: HCD_BRINGUP_FAILED,
                    message: "usb-hcd: controller bring-up failed",
                    fields,
                },
            );
        };
        match err.phase {
            BringupPhase::ControllerOpen => event(&[
                phase,
                error,
                Field {
                    key: "open_stage",
                    value: tairix_log::FieldValue::Str(
                        err.open_stage.map_or("-", XhciOpenStage::as_str),
                    ),
                },
                opt_u32_field("usbcmd", err.usbcmd),
                opt_u32_field("usbsts", err.usbsts),
            ]),
            BringupPhase::PortPower => event(&[
                phase,
                error,
                opt_u32_field("usbcmd", err.usbcmd),
                opt_u32_field("usbsts", err.usbsts),
            ]),
            BringupPhase::Setup | BringupPhase::BarMap | BringupPhase::ControllerStart => {
                event(&[phase, error]);
            }
        }
    }

    /// Emit the topology a walk settled on, so a metal capture shows what it
    /// actually served — and warn when a connected device was present but
    /// failed enumeration and was skipped, which otherwise looks exactly like
    /// an empty port.
    fn log_walk_settled(host: &ControllerHost<'_>) {
        let Ok(mut core) = host.device() else {
            return;
        };
        let live = (0..core.device_table_len())
            .filter(|&index| core.device_live(index))
            .count();
        log(
            &LogSink,
            &Event {
                level: Level::Info,
                id: HCD_READY,
                message: "usb-hcd: walk of the ports complete",
                fields: &[
                    Field {
                        key: "devices",
                        value: tairix_log::FieldValue::UnsignedInt(live as u64),
                    },
                    Field {
                        key: "hub_watch",
                        value: tairix_log::FieldValue::Bool(core.hub_watch_active()),
                    },
                ],
            },
        );
        log_skipped_ports(&mut core);
    }

    /// Warn that connected device(s) were present but left unserved, naming
    /// the **first failing port's snapshot** — the port, the enumeration step
    /// it failed in, and the completion and event type the failing command or
    /// transfer ended with, or that its deadline did. QEMU models no Pi USB,
    /// so this is the diagnostic a metal capture localises an unserved device
    /// with.
    fn log_skipped_ports(device: &mut tairix_usb::device::UsbDevice<RegisterWindow, SlabBank<'_>>) {
        if device.skipped_port_count() == 0 {
            return;
        }
        let u = |v: u64| tairix_log::FieldValue::UnsignedInt(v);
        let skipped = Field {
            key: "skipped_ports",
            value: u(u64::from(device.skipped_port_count())),
        };
        let message = "usb-hcd: connected device(s) failed enumeration and were skipped";
        match device.take_attach_fault() {
            Some(fault) => {
                log(
                    &LogSink,
                    &Event {
                        level: Level::Warn,
                        id: HCD_BRINGUP_FAILED,
                        message,
                        fields: &[
                            skipped,
                            Field {
                                key: "err",
                                value: u(fault.error as u64),
                            },
                            Field {
                                key: "attach_port",
                                value: u(u64::from(fault.port)),
                            },
                            Field {
                                key: "enum_stage",
                                value: u(u64::from(fault.stage.as_u8())),
                            },
                            Field {
                                key: "completion",
                                value: u(u64::from(fault.completion)),
                            },
                            Field {
                                key: "event_type",
                                value: u(u64::from(fault.event_type)),
                            },
                            Field {
                                key: "timed_out",
                                value: tairix_log::FieldValue::Bool(fault.timed_out),
                            },
                            Field {
                                key: "port_status",
                                value: u(u64::from(fault.port_status)),
                            },
                        ],
                    },
                );
            }
            None => {
                log(
                    &LogSink,
                    &Event {
                        level: Level::Warn,
                        id: HCD_BRINGUP_FAILED,
                        message,
                        fields: &[skipped],
                    },
                );
            }
        }
    }

    /// Enter the strict-priority real-time scheduling class so the
    /// interrupt-woken service step preempts CPU-bound work and cannot be
    /// starved: under a load like `stress --cpu N` the step that drains the
    /// interrupt-IN endpoints and re-arms them must run before the armed
    /// transfer ring fills, no matter how busy userland is, or reports are
    /// dropped at the hardware (the on-metal "missed keypresses under load"
    /// defect; `plans/USB.md`).
    ///
    /// The manifest grants `CAP_SCHED_REALTIME`; a build that somehow runs
    /// without it degrades gracefully to fair scheduling — the service still
    /// runs, only without the strict guarantee — rather than refusing to
    /// start.
    fn enter_realtime_class() {
        let rt = tairix_rt::sched_set_realtime(true);
        if rt == 0 {
            log(
                &LogSink,
                &Event {
                    level: Level::Info,
                    id: HCD_READY,
                    message: "usb-hcd: entered real-time scheduling class (report capture cannot be starved)",
                    fields: &[],
                },
            );
        } else {
            log_hex_event(
                HCD_WAIT_ERROR,
                Level::Warn,
                "usb-hcd: real-time scheduling class refused; serving time-shared",
                "err_hex",
                rt.unsigned_abs(),
            );
        }
    }

    /// Bind the controller's interrupt line, returning the kernel handle.
    ///
    /// Called **before** the controller is brought up, whose interrupter is
    /// enabled as part of starting it, so a completion posted the moment
    /// interrupts are enabled latches instead of going astray. [`None`]
    /// refuses the controller before any register is touched: one with no
    /// usable interrupt line cannot be served event-driven.
    fn bind_controller_irq(line: Option<u32>) -> Option<u64> {
        let Some(line) = line else {
            log(
                &LogSink,
                &Event {
                    level: Level::Warn,
                    id: HCD_URB_SETUP,
                    message: "usb-hcd: no IRQ line grant for event-driven service",
                    fields: &[],
                },
            );
            return None;
        };
        // A negative return is the errno; anything else is the bound handle.
        let Ok(handle) = u64::try_from(tairix_rt::irq_bind(line)) else {
            log_hex_event(
                HCD_URB_SETUP,
                Level::Warn,
                "usb-hcd: IRQ bind failed",
                "line_hex",
                u64::from(line),
            );
            return None;
        };
        log_hex_event(
            HCD_URB_SETUP,
            Level::Info,
            "usb-hcd: controller IRQ line bound",
            "handle_hex",
            handle,
        );
        Some(handle)
    }

    /// Create the wait-set the event loop parks on and register the controller
    /// interrupt on it under [`TOKEN_IRQ`]; each transport endpoint joins as it
    /// is created. All of this must succeed before any interface is published,
    /// because interrupt-IN URBs complete only through that event-driven wake
    /// path, so [`None`] refuses the controller.
    fn create_event_set(irq_handle: u64) -> Option<u64> {
        // A negative return is the errno; anything else is the wait-set handle.
        let set = u64::try_from(tairix_rt::waitset_create()).ok()?;
        let irq_add = tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::Irq,
            irq_handle,
            TOKEN_IRQ,
        );
        if let Err(ret) = super::waitset_ctl_result(irq_add) {
            log_hex_event(
                HCD_URB_SETUP,
                Level::Warn,
                "usb-hcd: IRQ source add to wait-set failed",
                "ret_hex",
                ret.unsigned_abs(),
            );
            return None;
        }
        log_hex_event(
            HCD_URB_SETUP,
            Level::Info,
            "usb-hcd: IRQ source added to wait-set",
            "handle_hex",
            irq_handle,
        );
        Some(set)
    }

    /// Tell the kernel every device present as the controller came up is
    /// published — the walk's own and the one re-attach owed a port it could
    /// not serve — so a device published later arrived on a running machine.
    /// A refused settle only leaves the device manager to wait out its bound
    /// for this node.
    fn settle() {
        let _ = tairix_rt::hw_node_settle();
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime is
    /// set up and routes its return value through the `exit` syscall.
    fn main() -> i32 {
        // Coherent DMA is carved kernel-side, so no architecture-specific
        // cache-maintenance shim is supplied (`coherency = None`).
        let caps = tairix_drv_bus_usb::REQUIRED_CAPABILITIES
            .iter()
            .copied()
            .collect();
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return end(DriverExit::NoHost, &[]);
        };
        enter_realtime_class();
        let Ok(resources) = derive_controller_resources(host.resources()) else {
            return end(DriverExit::NoResources, &[]);
        };
        let Some(irq_handle) = bind_controller_irq(host.irq_line()) else {
            return end(DriverExit::BringupFailed, &[]);
        };
        let mut controller = match bring_up_controller_diagnostic(
            &host,
            resources.bar_base,
            resources.bar_len,
            resources.dma_aperture_top,
        ) {
            Ok(controller) => controller,
            Err(err) => {
                log_bringup_failure(&err);
                return end(DriverExit::BringupFailed, &[]);
            }
        };

        let Some(set) = create_event_set(irq_handle) else {
            return end(DriverExit::NoService, &[]);
        };

        // Transports are opened as devices need them, so the controller pays
        // for the devices attached, never a fixed table.
        let Some(urb_base) = claim_urb_block() else {
            return end(DriverExit::NoService, &[]);
        };
        let Ok(origin) = tairix_rt::self_origin() else {
            return end(DriverExit::NoService, &[]);
        };
        let mut live = Live {
            host: &mut controller,
            set,
            urb_base,
            instance: origin.proc_id(),
        };

        // The owner id names the controller in the audit log: its own
        // discovered endpoint block, never a board constant.
        let controller_owner = u32::try_from(urb_base & 0xFFFF_FFFF).unwrap_or(u32::MAX);
        let mut health = ControllerHealth::new(controller_owner);
        log(
            &LogSink,
            &Event {
                level: Level::Info,
                id: HCD_READY,
                message: "usb-hcd: controller up, serving URB transport",
                fields: &[],
            },
        );
        serve_events(&mut live, &mut Interfaces::new(), &mut health)
    }

    /// Where the bring-up stands: its walk, then the one deferred re-attach
    /// of a port the walk could not serve. Once both are done the kernel is
    /// told every device present at boot is published.
    #[derive(Copy, Clone, Eq, PartialEq)]
    enum Boot {
        Walking,
        Retrying,
        Settled,
    }

    /// The asynchronous event loop: serve the engine, act on what it did,
    /// and park — never spinning a quiet controller — until the controller
    /// interrupts, a transport endpoint is ready, or the earliest deadline an
    /// operation, a held URB or a retry owes is due.
    ///
    /// Returns the process exit code: once the controller fails closed, or
    /// once the wait-set is torn down under us.
    fn serve_events(
        live: &mut Live<'_, '_>,
        interfaces: &mut Interfaces<NodeBuffer>,
        health: &mut ControllerHealth,
    ) -> i32 {
        let mut reported_drops = 0u64;
        let mut retries = Retries::default();
        let mut boot = Boot::Walking;
        loop {
            let now_ns = tairix_rt::clock_get();
            let (engine_deadline, torn_down) = match live.host.service(now_ns) {
                Ok(serviced) => {
                    act_on(
                        &serviced,
                        live,
                        interfaces,
                        health,
                        &mut retries,
                        &mut boot,
                        now_ns,
                    );
                    let torn_down = serviced.topology.iter().any(|event| {
                        matches!(event, HubEvent::Detached(_) | HubEvent::HubDetached(_))
                    });
                    (serviced.next_deadline, torn_down)
                }
                Err(err) => {
                    log_service_failure(err);
                    (None, false)
                }
            };
            note_dropped_reports(live.host, &mut reported_drops);
            log_abandon_reports(live.host);
            let serving = !(health.is_recovering() || health.is_failed_closed());
            if serving {
                interfaces.drive_busy(live);
                interfaces.deliver_streams(live);
            }
            // A teardown can leave the controller halted with a latched Host
            // System Error on the Pi 4 VL805, and a faulted controller raises
            // no further interrupt (xHCI §4.24.1). Only a teardown pays for
            // the register read; an attempt that failed is retried off the
            // grace timer, never on every pass.
            let faulted =
                live.host.command_ring_dead() || (torn_down && live.host.controller_faulted());
            if serving && faulted {
                interfaces.recover(health, live);
            }
            run_due_retries(live, interfaces, health, &mut retries, now_ns);
            if interfaces.next_deadline().is_some_and(|at| at <= now_ns) {
                interfaces.expire_overdue(now_ns, health, live);
            }
            // Nothing is retried below a controller failed closed; its reason
            // is already logged and every node retracted.
            if health.is_failed_closed() {
                return end(DriverExit::DeviceFault, &[]);
            }
            if live.host.work_ready() {
                continue;
            }
            // A reset's walk owns the recovery while it runs, so the grace
            // window bounds the park only between attempts.
            let grace = if interfaces.is_resetting() {
                None
            } else {
                health.wait_timeout(now_ns)
            };
            let timeout = [
                engine_deadline.map(|at| at.saturating_sub(now_ns)),
                grace,
                retries.wait_timeout(now_ns),
                interfaces
                    .next_deadline()
                    .map(|at| at.saturating_sub(now_ns)),
            ]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(WAITSET_TIMEOUT_NONE);
            let mut token = 0u64;
            let wait_ret = tairix_rt::waitset_wait(live.set, timeout, &mut token);
            if wait_ret >= 0 {
                match token {
                    TOKEN_IRQ => service_controller_interrupt(live, interfaces, health),
                    // Registered as `TOKEN_URB_BASE + slot`.
                    token => {
                        let slot = token
                            .checked_sub(TOKEN_URB_BASE)
                            .and_then(|slot| usize::try_from(slot).ok());
                        if let Some(slot) = slot {
                            interfaces.serve_submit(slot, health, live);
                        }
                    }
                }
            } else if Errno::from_syscall(wait_ret) != Errno::TimedOut {
                log_hex_event(
                    HCD_WAIT_ERROR,
                    Level::Error,
                    "usb-hcd: wait-set wait failed; stopping",
                    "ret_hex",
                    wait_ret.unsigned_abs(),
                );
                interfaces.retract_all(live);
                return end(DriverExit::ServeFailed, &[]);
            }
        }
    }

    /// Act on what one service step did: publish and retract the nodes its
    /// topology changes call for, and close the walk that settled.
    fn act_on(
        serviced: &Serviced,
        live: &mut Live<'_, '_>,
        interfaces: &mut Interfaces<NodeBuffer>,
        health: &mut ControllerHealth,
        retries: &mut Retries,
        boot: &mut Boot,
        now_ns: u64,
    ) {
        if !serviced.topology.is_empty() {
            for &event in &serviced.topology {
                log_topology(event);
            }
            // A reset's walk is matched against the nodes once it settles.
            if !interfaces.is_resetting() {
                interfaces.reconcile(live);
            }
        }
        if !serviced.settled {
            return;
        }
        interfaces.finish_recovery(health, live);
        log_walk_settled(live.host);
        match *boot {
            Boot::Walking => {
                let (skipped, any) = live.host.view().map_or((0, false), |core| {
                    (core.skipped_port_count(), core.any_device_live())
                });
                if !any {
                    log(
                        &LogSink,
                        &Event {
                            level: Level::Info,
                            id: HCD_READY,
                            message: "usb-hcd: controller up, awaiting first device connect",
                            fields: &[],
                        },
                    );
                }
                if skipped > 0 {
                    retries.arm_port(now_ns);
                    *boot = Boot::Retrying;
                } else {
                    settle();
                    *boot = Boot::Settled;
                }
            }
            Boot::Retrying if !retries.port_pending() => {
                settle();
                *boot = Boot::Settled;
            }
            Boot::Retrying | Boot::Settled => {}
        }
    }

    /// Arm a failed endpoint recovery's retry, and run each retry whose
    /// deadline has passed: spending it even when a controller recovery runs
    /// instead, since an overdue one-shot left armed would bound every later
    /// park at zero and spin the loop.
    fn run_due_retries(
        live: &mut Live<'_, '_>,
        interfaces: &mut Interfaces<NodeBuffer>,
        health: &mut ControllerHealth,
        retries: &mut Retries,
        now_ns: u64,
    ) {
        let serving =
            !(interfaces.is_resetting() || health.is_recovering() || health.is_failed_closed());
        if serving && live.host.recovery_owed() && retries.arm_recovery(now_ns) {
            log(
                &LogSink,
                &Event {
                    level: Level::Warn,
                    id: HCD_RECOVERY,
                    message:
                        "usb-hcd: an endpoint recovery failed; retrying it once the path settles",
                    fields: &[],
                },
            );
        }
        if health.wait_timeout(now_ns).is_some_and(|left| left == 0) {
            interfaces.recover(health, live);
        }
        // A controller recovery re-walks every port, which subsumes both.
        let serving = serving && !interfaces.is_resetting();
        let due = retries.take_due(now_ns);
        if due.port && serving {
            retry_skipped_ports(live.host);
        }
        if due.recovery && serving {
            retry_owed_recoveries(live.host);
        }
    }

    /// Acknowledge the controller interrupt. The acknowledgement's single
    /// `USBSTS` read also carries the fault latch: a controller already
    /// faulted when we woke raises no further interrupt, so it is recovered
    /// here. The service step at the top of the loop does the rest.
    ///
    /// `IMAN.IP` is acknowledged before the ring is drained so a completion
    /// posted during the drain re-asserts rather than being lost; Event
    /// Handler Busy is released only by the per-event `ERDP` advance the
    /// drain performs, never by a standalone write on an empty ring.
    fn service_controller_interrupt(
        live: &mut Live<'_, '_>,
        interfaces: &mut Interfaces<NodeBuffer>,
        health: &mut ControllerHealth,
    ) {
        let faulted = live
            .host
            .device()
            .and_then(|mut core| core.acknowledge_interrupt())
            .is_ok_and(|status| status.faulted);
        if faulted && !health.is_recovering() {
            interfaces.recover(health, live);
        }
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
#[cfg(not(freestanding))]
fn main() {
    // On the host this binary is an inert stub: the freestanding `Run` program
    // above is built only for the bare-metal driver targets. Keeping a host
    // `main` lets `cargo build --workspace`, clippy, and fmt still cover the
    // file, mirroring the other driver `Run` binaries.
}

#[cfg(test)]
mod tests {
    use super::waitset_ctl_result;

    #[test]
    fn waitset_ctl_result_preserves_failure_code() {
        assert_eq!(waitset_ctl_result(0), Ok(()));
        assert_eq!(waitset_ctl_result(-2), Err(-2));
    }
}
