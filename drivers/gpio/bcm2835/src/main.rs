//! The `Run` binary of the BCM2835-family GPIO controller driver, autoloaded
//! into user space by `devmgr` for a discovered controller node.
//!
//! It learns the layout from the bind key its node matched, maps the window,
//! quiesces every line, binds the bank interrupts and the node's interrupt
//! endpoint, and — where the board leads lines to a header — binds the header
//! endpoint and publishes its channel node, then serves them all from one
//! loop. On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::gpio::{
        HeaderLines, GPIO_HEADER_ENDPOINTS, GPIO_HEADER_NODE_COMPATIBLE, HEADER_MAX_REPLY,
        HEADER_REQUEST_LEN,
    };
    use tairix_abi::driver::irqline::{IRQLINE_MAX_REPLY, IRQLINE_MAX_REQUEST};
    use tairix_abi::driver::DeviceWake;
    use tairix_abi::hwlink::LinkRole;
    use tairix_abi::hwtree::HwResourceKind;
    use tairix_abi::{CapabilityId, Errno, HwDeviceClass, HwProperty, ProcId, PROC_ID_HEX_LEN};
    use tairix_drv_gpio_bcm2835::block::{Block, WINDOW_LEN};
    use tairix_drv_gpio_bcm2835::controller::{
        Controller, HeaderPort, Record, Role, BANK_INTERRUPTS, HEADER_CALLS_PER_WAKE, LINES_MAX,
    };
    use tairix_drv_gpio_bcm2835::{BIND_KEYS, LAYOUTS, REQUIRED_CAPABILITIES};
    use tairix_drvrt::exit::{end, step, DriverExit};
    use tairix_drvrt::serve::{
        instance_field, log_event, publish_channel, DeviceChannel, DutyNode, Endpoint, Service,
    };
    use tairix_drvrt::{Recorder, RtSupplier, SupplierHost};
    use tairix_log::{EventId, Field, FieldValue, Level};

    const EVENT_READY: EventId = EventId(24_930);
    const EVENT_CLAIMED: EventId = EventId(24_931);
    const EVENT_RECLAIMED: EventId = EventId(24_932);
    const EVENT_REFUSED: EventId = EventId(24_933);
    const EVENT_RELEASED: EventId = EventId(24_934);
    const EVENT_UNANSWERED: EventId = EventId(24_935);

    fn main() -> i32 {
        let layout = tairix_rt::hw_self_node().ok().and_then(|record| {
            tairix_devmatch::matched_bind_key(record.match_keys(), BIND_KEYS)
                .and_then(|matched| LAYOUTS.get(matched).copied())
        });
        let Some(layout) = layout else {
            return end(
                DriverExit::NoResources,
                &[step("gpio: the node matches no layout")],
            );
        };
        let caps = REQUIRED_CAPABILITIES.iter().copied().collect();
        let node = match DutyNode::open(caps, LinkRole::InterruptLine) {
            Ok(node) => node,
            Err(code) => return code,
        };
        let len = match node.window() {
            Ok((_, len)) => len,
            Err(code) => return code,
        };
        if len < WINDOW_LEN {
            return end(
                DriverExit::NoResources,
                &[step("gpio: the window is shorter than the registers")],
            );
        }
        let mut wakes = [DeviceWake::Irq(0); BANK_INTERRUPTS + 1];
        let banks = match bind_banks(&node, &mut wakes) {
            Ok(banks) => banks,
            Err(code) => return code,
        };
        let header = match bind_header(&node) {
            Ok(header) => header,
            Err(code) => return code,
        };
        if let Some((port, _)) = &header {
            wakes[banks] = DeviceWake::Calls(port.endpoint());
        }
        let header_id = header.as_ref().map(|(port, _)| port.endpoint());
        let regs = match node.map() {
            Ok(regs) => regs,
            Err(code) => return code,
        };
        let endpoint = node.duty().endpoint();
        let Ok(mut controller) = Controller::new(
            Block::new(&regs, layout),
            RtSupplier::new(endpoint),
            endpoint,
            banks,
            header,
            Log,
        ) else {
            return end(
                DriverExit::BringupFailed,
                &[step("gpio: the lines could not be quiesced")],
            );
        };
        let mut request = [0u8; IRQLINE_MAX_REQUEST];
        // A consumer waits on each line at most once at a time, so the queue
        // needs no more than a call per line.
        let service = Service::bind(
            Endpoint {
                id: endpoint,
                request: &mut request,
                max_reply: IRQLINE_MAX_REPLY,
                capacity: LINES_MAX,
            },
            &wakes[..banks + usize::from(header_id.is_some())],
        );
        let ended = match service {
            Ok(service) => {
                // Published only once both endpoints serve, so a service
                // that finds the header can call it.
                if let Some(id) = header_id {
                    if publish_channel(HwDeviceClass::Other, GPIO_HEADER_NODE_COMPATIBLE, id, &[])
                        .is_none()
                    {
                        return end(
                            DriverExit::NoService,
                            &[step("gpio: the header could not be published")],
                        );
                    }
                }
                log_event(Level::Info, EVENT_READY, "gpio: controller serving", &[]);
                service.run(&mut controller)
            }
            Err(ended) => ended,
        };
        ended.end()
    }

    /// Bind the node's bank interrupts, at most [`BANK_INTERRUPTS`], into
    /// `wakes`, answering how many.
    fn bind_banks(node: &DutyNode, wakes: &mut [DeviceWake]) -> Result<usize, i32> {
        let lines = node
            .resources()
            .filter(|r| r.kind() == Some(HwResourceKind::Irq))
            .filter_map(|r| u32::try_from(r.base()).ok())
            .take(BANK_INTERRUPTS);
        let mut banks = 0;
        for (line, wake) in lines.zip(wakes.iter_mut()) {
            let Ok(handle) = u64::try_from(tairix_rt::irq_bind(line)) else {
                return Err(end(
                    DriverExit::BringupFailed,
                    &[step("gpio: a bank interrupt could not be bound")],
                ));
            };
            *wake = DeviceWake::Irq(handle);
            banks += 1;
        }
        Ok(banks)
    }

    /// Bind the header endpoint where the board leads lines to one.
    fn bind_header(node: &DutyNode) -> Result<Option<(RtHeader, HeaderLines)>, i32> {
        let lent = node
            .host()
            .property(HwProperty::GpioHeaderLines)
            .map(HeaderLines::from_mask)
            .filter(|lent| !lent.is_empty());
        let Some(lent) = lent else {
            return Ok(None);
        };
        match HEADER.bind() {
            Some(id) => Ok(Some((RtHeader::new(id), lent))),
            None => Err(end(
                DriverExit::NoService,
                &[step("gpio: the header endpoint could not be bound")],
            )),
        }
    }

    /// The header's channel: served restricted-sender to holders of
    /// `CAP_GPIO`, each calling once at a time.
    const HEADER: DeviceChannel = DeviceChannel {
        block: GPIO_HEADER_ENDPOINTS,
        gate: CapabilityId::GPIO,
        max_request: HEADER_REQUEST_LEN,
        max_reply: HEADER_MAX_REPLY,
        capacity: HEADER_CALLS_PER_WAKE,
    };

    /// The header endpoint over the runtime's call syscalls.
    struct RtHeader {
        supplier: RtSupplier,
    }

    impl RtHeader {
        const fn new(endpoint: u64) -> Self {
            Self {
                supplier: RtSupplier::new(endpoint),
            }
        }

        const fn endpoint(&self) -> u64 {
            self.supplier.endpoint()
        }
    }

    impl HeaderPort for RtHeader {
        fn receive(&mut self, frame: &mut [u8]) -> Result<Option<(u64, usize)>, Errno> {
            tairix_rt::call_recv_ready(self.endpoint(), frame)
                .map(|call| call.map(|call| (call.ticket, call.len)))
        }

        fn caller(&self, ticket: u64) -> Result<ProcId, Errno> {
            self.supplier.caller(ticket)
        }

        fn reply(&mut self, ticket: u64, frame: &[u8]) -> Result<(), Errno> {
            self.supplier.reply(ticket, frame)
        }
    }

    /// The system log, as the controller records to it.
    struct Log;

    impl Recorder<Record> for Log {
        fn record(&mut self, record: Record) {
            let mut hex = [0u8; PROC_ID_HEX_LEN];
            let line_field = |line: u8| Field {
                key: "line",
                value: FieldValue::UnsignedInt(u64::from(line)),
            };
            match record {
                Record::Claimed { line, holder, role } => log_event(
                    Level::Info,
                    EVENT_CLAIMED,
                    match role {
                        Role::Interrupt => "gpio: line claimed as an interrupt",
                        Role::Output => "gpio: header line claimed as an output",
                    },
                    &[line_field(line), instance_field("holder", holder, &mut hex)],
                ),
                Record::Reclaimed { line, from } => log_event(
                    Level::Warn,
                    EVENT_RECLAIMED,
                    "gpio: line reclaimed from an ended consumer",
                    &[line_field(line), instance_field("from", from, &mut hex)],
                ),
                Record::Refused { caller, reason } => {
                    let reason = Field {
                        key: "reason",
                        value: FieldValue::Error(reason),
                    };
                    match caller {
                        Some(caller) => log_event(
                            Level::Warn,
                            EVENT_REFUSED,
                            "gpio: request refused",
                            &[reason, instance_field("caller", caller, &mut hex)],
                        ),
                        None => log_event(
                            Level::Warn,
                            EVENT_REFUSED,
                            "gpio: request refused",
                            &[reason],
                        ),
                    }
                }
                Record::Released { line } => log_event(
                    Level::Info,
                    EVENT_RELEASED,
                    "gpio: line given back",
                    &[line_field(line)],
                ),
                Record::Unanswered { line } => log_event(
                    Level::Warn,
                    EVENT_UNANSWERED,
                    "gpio: line fired with its consumer unreachable",
                    &[line_field(line)],
                ),
            }
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
