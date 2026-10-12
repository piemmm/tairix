//! The `Run` binary of the Broadcom L2 interrupt controller driver, autoloaded
//! into user space by `devmgr` for a discovered controller node.
//!
//! It learns the register layout from the bind key its node matched, maps the
//! window, masks every line, binds its own line on the root controller and the
//! node's endpoint under its interrupt-line duty, and serves both. On the host
//! it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::irqline::{IRQLINE_MAX_REPLY, IRQLINE_MAX_REQUEST};
    use tairix_abi::driver::DeviceWake;
    use tairix_abi::hwlink::LinkRole;
    use tairix_abi::hwtree::HwResourceKind;
    use tairix_abi::PROC_ID_HEX_LEN;
    use tairix_drv_irq_brcmstb_l2::controller::{Controller, Record, LINES};
    use tairix_drv_irq_brcmstb_l2::{BIND_KEYS, LAYOUTS, REQUIRED_CAPABILITIES};
    use tairix_drvrt::serve::{exit, fail, instance_field, log_event, DutyNode, Endpoint, Service};
    use tairix_drvrt::{Recorder, RtSupplier};
    use tairix_log::{EventId, Field, FieldValue, Level};

    const EVENT_READY: EventId = EventId(24_600);
    const EVENT_FAILED: EventId = EventId(24_601);
    const EVENT_CLAIMED: EventId = EventId(24_602);
    const EVENT_RECLAIMED: EventId = EventId(24_603);
    const EVENT_REFUSED: EventId = EventId(24_604);
    const EVENT_ABANDONED: EventId = EventId(24_605);
    const EVENT_UNANSWERED: EventId = EventId(24_606);

    fn main() -> i32 {
        let layout = tairix_rt::hw_self_node().ok().and_then(|record| {
            tairix_devmatch::matched_bind_key(record.match_keys(), BIND_KEYS)
                .and_then(|matched| LAYOUTS.get(matched).copied())
        });
        let Some(layout) = layout else {
            return fail(
                EVENT_FAILED,
                exit::NO_RESOURCES,
                "l2-intc: the node matches no layout",
            );
        };
        let caps = REQUIRED_CAPABILITIES.iter().copied().collect();
        let node = match DutyNode::open(caps, LinkRole::InterruptLine, EVENT_FAILED) {
            Ok(node) => node,
            Err(code) => return code,
        };
        let len = match node.window() {
            Ok((_, len)) => len,
            Err(code) => return code,
        };
        if len < layout.window_len() {
            return node.fail(
                exit::NO_RESOURCES,
                "l2-intc: the window is shorter than the registers",
            );
        }
        let Some(line) = node
            .resources()
            .find(|r| r.kind() == Some(HwResourceKind::Irq))
            .and_then(|r| u32::try_from(r.base()).ok())
        else {
            return node.fail(
                exit::NO_RESOURCES,
                "l2-intc: the node names no line of its own",
            );
        };
        let regs = match node.map() {
            Ok(regs) => regs,
            Err(code) => return code,
        };
        let endpoint = node.duty().endpoint();
        let Ok(mut controller) =
            Controller::new(&regs, layout, RtSupplier::new(endpoint), Log, endpoint)
        else {
            return node.fail(
                exit::BRINGUP_FAILED,
                "l2-intc: the lines could not be masked",
            );
        };
        let Ok(own_line) = u64::try_from(tairix_rt::irq_bind(line)) else {
            return node.fail(
                exit::BRINGUP_FAILED,
                "l2-intc: its own line could not be bound",
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
                capacity: LINES,
            },
            &[DeviceWake::Irq(own_line)],
        );
        let ended = match service {
            Ok(service) => {
                log_event(Level::Info, EVENT_READY, "l2-intc: controller serving", &[]);
                service.run(&mut controller)
            }
            Err(ended) => ended,
        };
        node.fail(exit::SERVE_FAILED, ended.reason())
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
                Record::Claimed { line, owner } => log_event(
                    Level::Info,
                    EVENT_CLAIMED,
                    "l2-intc: line claimed",
                    &[line_field(line), instance_field("owner", owner, &mut hex)],
                ),
                Record::Reclaimed { line, from } => log_event(
                    Level::Warn,
                    EVENT_RECLAIMED,
                    "l2-intc: line reclaimed from an ended consumer",
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
                            "l2-intc: request refused",
                            &[reason, instance_field("caller", caller, &mut hex)],
                        ),
                        None => log_event(
                            Level::Warn,
                            EVENT_REFUSED,
                            "l2-intc: request refused",
                            &[reason],
                        ),
                    }
                }
                Record::Abandoned { line } => log_event(
                    Level::Info,
                    EVENT_ABANDONED,
                    "l2-intc: line released with its ended consumer",
                    &[line_field(line)],
                ),
                Record::Unanswered { line } => log_event(
                    Level::Warn,
                    EVENT_UNANSWERED,
                    "l2-intc: line fired with its consumer unreachable; left masked",
                    &[line_field(line)],
                ),
            }
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
