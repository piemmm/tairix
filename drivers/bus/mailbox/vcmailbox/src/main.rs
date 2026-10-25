//! The `Run` entry-point binary of the `VideoCore` firmware property-mailbox
//! **service driver**, installed as a signed `/System/Drivers/` bundle and
//! **autoloaded into user space** by `devmgr` when the BCM2711 mailbox node is
//! discovered (`plans/PI.md` P10 D3).
//!
//! This moves the `VideoCore` mailbox out of the kernel (the floor stays
//! storage-only) into a user-space service: it owns the
//! discovered doorbell MMIO window and a DMA-carved property buffer, builds the
//! BCM2711 `VideoCore` transport (`lib/vcmailbox::DmaMailbox`), and answers
//! *synchronous* property exchanges from other user-space drivers — the VL805
//! USB firmware reload (`drivers/bus/usb/vl805`) — over the well-known
//! `tairix_abi::mailbox_ipc::MAILBOX_ENDPOINT` call endpoint.
//!
//! The hardware mechanism (doorbell registers, DMA buffer, bus-address
//! translation, cache coherency) lives entirely behind the transport. The
//! endpoint's `CAP_MAILBOX` send gate decides who may ask; what reaches the
//! firmware is `lib/vcmailbox::admit_request`'s answer, taken against the
//! hardware-tree node the kernel attests the caller is the driver of, because
//! the property interface reaches the whole `VideoCore` and the GPU masters
//! the bus. A refused request is answered in-band and logged with its caller,
//! its tag and the answer.
//!
//! It is a **pure-Rust** program: it links the Rust userland
//! runtime `tairix-rt` (`_start`, the stack canary, the panic handler,
//! and the `call_*` and `clock_get` syscall wrappers), never the C ABI.
//! `main` wires the real seams:
//!
//! * `RtDriverHost::from_grants_query` over `RtGrantSyscalls`: the host learns
//!   its kernel-issued grants (the doorbell window, its inbox interrupt, and a
//!   DMA constraint) and maps/carves them. Every capability and bound is
//!   re-checked kernel-side; the host adds no authority. The kernel carves
//!   coherent DMA, so no architecture-specific cache shim is supplied
//!   (`coherency = None`, keeping the program free of arch code).
//! * `sole_register_window` over the delivered grants: the doorbell window
//!   `(base, len)` comes from the grants, never a build-time board constant.
//! * `host.alloc_dma_zeroed` carves the `PROPERTY_LEN_BYTES` property buffer;
//!   its device-visible base is the firmware's bus address for the buffer.
//! * `host.bind_irq` binds the granted inbox interrupt; a service that cannot
//!   bind it exits rather than poll the doorbell.
//! * `DmaMailbox::new` over the doorbell window and the property buffer, which
//!   it withholds rather than frees while the firmware owes it a reply, and
//!   whose reply waits park on that interrupt, each until its own deadline;
//!   then a firmware-revision probe whose answer proves the firmware has
//!   finished with any request an earlier instance left in flight, so the
//!   kernel may release that instance's quarantined buffer
//!   (`host.device_quiesced`), then `call_create` to bind the
//!   restricted-sender endpoint, then the serve loop.
//!
//! After bring-up `main` serves forever: it blocks in `call_recv`, transforms
//! each request while parked on the inbox interrupt for the firmware's
//! answer, and replies with `call_reply`. A bring-up failure exits with a reserved
//! fail-closed code, leaving the system without a mailbox service rather than
//! wedged; the spawning supervisor decides whether to
//! relaunch.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {
    use core::cell::RefCell;

    use tairix_abi::driver::dma::DmaHost;
    use tairix_abi::driver::mailbox::{MailboxChannel, MAILBOX_PROPERTY_WORDS};
    use tairix_abi::driver::sole_register_window;
    use tairix_abi::mailbox_ipc::{self, MAILBOX_ENDPOINT};
    use tairix_abi::time::MonotonicClock;
    use tairix_abi::{CapabilityId, DriverError, Errno, HwNode, MmioMapper};
    use tairix_caps::CapabilitySet;
    use tairix_drvrt::exit::{end, DriverExit};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_log::{Event, EventId, Field, FieldValue, Level};
    use tairix_rt::LogSink;
    use tairix_vcmailbox::{
        admit_request, decode_firmware_revision_response, encode_firmware_revision_query,
        DmaMailbox, InboxInterrupt, MailboxError, MailboxTransport, PROPERTY_LEN_BYTES,
        SERVICE_CAPS,
    };

    /// A firmware request was refused: a frame that does not decode, a tag
    /// outside every driver's working set or in another shape, or one from a
    /// caller that is not the driver of the device it concerns.
    const REQUEST_REFUSED: EventId = EventId(24_500);

    /// How long each reply wait parks for the firmware's answer: the ≈4 s the
    /// ten-million-poll spin it replaces measured on metal.
    const REPLY_WINDOW_NS: u64 = 4_000_000_000;

    /// Bound on the number of in-flight requests the endpoint queues. The
    /// service answers each request before receiving the next, so a small
    /// capacity suffices; it is a queue bound, not a hardware capacity.
    const ENDPOINT_CAPACITY: usize = 4;

    /// The inbox interrupt the host bound, and the clock the reply deadlines
    /// run on.
    struct HostInbox<'a>(&'a RtDriverHost<RtGrantSyscalls>);

    impl MonotonicClock for HostInbox<'_> {
        fn now_ns(&self) -> u64 {
            tairix_rt::clock_get()
        }
    }

    impl InboxInterrupt for HostInbox<'_> {
        fn park(&mut self, timeout_ns: u64) -> bool {
            self.0.wait_irq(timeout_ns)
        }
    }

    /// The required-sender capability set of the served endpoint: a caller
    /// must hold `CAP_MAILBOX` to post a request.
    fn endpoint_send_caps() -> CapabilitySet {
        let mut caps = CapabilitySet::empty();
        caps.insert(CapabilityId::MAILBOX);
        caps
    }

    /// Adapts the `lib/vcmailbox` `&mut self` [`MailboxTransport`] onto the
    /// `&self` [`MailboxChannel`] the wire-level server transform consumes.
    ///
    /// Sound because the service is the transport's only, single-threaded
    /// caller (the service serialises access). A transport
    /// [`MailboxError`] is mapped to the board-neutral [`DriverError`] the
    /// seam reports, which [`mailbox_ipc::serve_request`] then frames as an
    /// in-band error reply (fail closed).
    struct ServiceChannel<'a> {
        mailbox: RefCell<DmaMailbox<HostInbox<'a>>>,
    }

    impl MailboxChannel for ServiceChannel<'_> {
        fn exchange(&self, message: &mut [u32; MAILBOX_PROPERTY_WORDS]) -> Result<(), DriverError> {
            self.mailbox
                .borrow_mut()
                .exchange(message)
                .map_err(MailboxError::as_driver_error)
        }
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// On success this never returns: the serve loop runs for the life of the
    /// service process.
    fn main() -> i32 {
        // Build the host from the grants the kernel minted. The kernel carves
        // coherent DMA, so no architecture-specific cache shim is supplied.
        let caps = SERVICE_CAPS.iter().copied().collect();
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return end(DriverExit::NoHost, &[]);
        };
        // Resolve and map the single granted doorbell window.
        let Ok((base, len)) = sole_register_window(host.resources()) else {
            return end(DriverExit::NoResources, &[]);
        };
        let Ok(regs) = host.map_window(base, len) else {
            return end(DriverExit::BringupFailed, &[]);
        };
        let Ok(buffer) = host.alloc_dma_zeroed(PROPERTY_LEN_BYTES) else {
            return end(DriverExit::BringupFailed, &[]);
        };
        // Bound before the mailbox turns the interrupt on, so a service that
        // cannot take it leaves the controller as it found it.
        if host.bind_irq().is_err() {
            return end(DriverExit::BringupFailed, &[]);
        }
        let Ok(mut mailbox) = DmaMailbox::new(regs, buffer, HostInbox(&host), REPLY_WINDOW_NS)
        else {
            return end(DriverExit::BringupFailed, &[]);
        };
        // The firmware answers property requests one at a time in posting
        // order, so its answer to this probe means it has finished with any
        // request, and so any buffer, an earlier instance left in flight.
        let mut probe = encode_firmware_revision_query();
        if mailbox
            .exchange(&mut probe)
            .and_then(|()| decode_firmware_revision_response(&probe))
            .is_err()
        {
            return end(DriverExit::BringupFailed, &[]);
        }
        host.device_quiesced();
        let channel = ServiceChannel {
            mailbox: RefCell::new(mailbox),
        };

        // Create the restricted-sender call endpoint other drivers reach the
        // service through. A non-zero result is a fail-closed refusal (the id
        // is already bound, or the bind privilege is missing).
        let send_caps = endpoint_send_caps();
        let recv_caps = CapabilitySet::empty();
        if tairix_rt::call_create(
            MAILBOX_ENDPOINT,
            &send_caps,
            &recv_caps,
            mailbox_ipc::REQUEST_LEN,
            mailbox_ipc::REPLY_LEN,
            ENDPOINT_CAPACITY,
        ) != 0
        {
            return end(DriverExit::NoService, &[]);
        }

        serve(&channel)
    }

    /// The serve loop: block in `call_recv` (a real park between requests),
    /// judge each request against the device its caller is the driver of,
    /// transform it through the mailbox transport, and answer with
    /// `call_reply`.
    ///
    /// A `call_recv` error is terminal — the endpoint is sized so an
    /// oversize request is refused at send time, leaving only a destroyed
    /// endpoint or a torn-down task — so it ends the service fail-loud
    /// (the supervisor decides whether to relaunch) rather than yield-retry
    /// a dead channel forever, which is a busy spin. A reply that fails to
    /// encode is dropped to a zero-length reply, which the client decodes
    /// as a fail-closed truncation.
    fn serve(channel: &ServiceChannel<'_>) -> i32 {
        let mut request = [0u8; mailbox_ipc::REQUEST_LEN];
        let mut reply = [0u8; mailbox_ipc::REPLY_LEN];
        loop {
            let mut ticket = 0u64;
            let Ok(n) = tairix_rt::call_recv(MAILBOX_ENDPOINT, &mut request, &mut ticket) else {
                return end(DriverExit::ServeFailed, &[]);
            };
            let mut named = None;
            let admit = |message: &[u32; MAILBOX_PROPERTY_WORDS]| {
                admit_request(message, attested_node(ticket).as_ref()).map_err(|refusal| {
                    named = refusal.tag;
                    refusal.errno
                })
            };
            let len = match mailbox_ipc::serve_request(channel, &request[..n], &mut reply, admit) {
                Ok(served) => {
                    if let Some(errno) = served.refused {
                        log_refusal(ticket, named, errno);
                    }
                    served.reply_len
                }
                Err(_) => 0,
            };
            let _ = tairix_rt::call_reply(MAILBOX_ENDPOINT, ticket, &reply[..len]);
        }
    }

    /// The hardware-tree node the kernel attests the caller of `ticket` is the
    /// driver of, or [`None`] when it is no driver loaded for a node.
    fn attested_node(ticket: u64) -> Option<HwNode> {
        let mut bytes = [0u8; HwNode::WIRE_LEN];
        let len = tairix_rt::call_peer_node(MAILBOX_ENDPOINT, ticket, &mut bytes).ok()?;
        if len != bytes.len() {
            return None;
        }
        HwNode::from_bytes(&bytes).ok()
    }

    /// Record that the caller of `ticket`, still in service, was refused with
    /// `errno` over the tag it `named`.
    fn log_refusal(ticket: u64, named: Option<u32>, errno: Errno) {
        let pid = tairix_rt::peer_origin(MAILBOX_ENDPOINT, ticket)
            .map_or(FieldValue::Null, |origin| {
                FieldValue::UnsignedInt(origin.pid())
            });
        let tag = named.map_or(FieldValue::Null, |tag| {
            FieldValue::UnsignedInt(u64::from(tag))
        });
        tairix_log::log(
            &LogSink,
            &Event {
                level: Level::Warn,
                id: REQUEST_REFUSED,
                message: "vcmailbox: firmware request refused",
                fields: &[
                    Field {
                        key: "pid",
                        value: pid,
                    },
                    Field {
                        key: "tag",
                        value: tag,
                    },
                    Field {
                        key: "errno",
                        value: FieldValue::SignedInt(i64::from(errno.as_i32())),
                    },
                ],
            },
        );
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// On the host (`cargo build --workspace`, clippy, fmt) the program's real
// entry — the freestanding `tairix-rt` `_start` path — is not compiled, so
// this inert `main` keeps the crate building under the host tooling. It
// performs no I/O.
#[cfg(not(freestanding))]
fn main() {}
