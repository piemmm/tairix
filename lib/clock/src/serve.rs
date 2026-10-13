//! The freestanding half: a clock controller driver's bring-up from its
//! node's grants, the loop that serves the node's endpoint, and the log the
//! endpoint records to.
//!
//! A driver [`open`]s its node, reads the rates the tree states for the clocks
//! it is fed from, builds its [`ClockHardware`] — over the mapped register
//! window, for a controller that has one — and hands it to [`serve`]. Every
//! failure ends the driver with a stable exit code and its reason logged.

use tairix_abi::driver::clock::{ClockOp, CLOCK_MAX_REPLY, CLOCK_MAX_REQUEST};
use tairix_abi::hwlink::LinkRole;
use tairix_abi::hwtree::HwResource;
use tairix_abi::ipc::IPC_CALL_CAPACITY_MAX;
use tairix_abi::PROC_ID_HEX_LEN;
use tairix_caps::CapabilitySet;
use tairix_drvrt::exit::{end, step, DriverExit};
use tairix_drvrt::serve::{instance_field, log_event, DutyNode, Endpoint, Service};
use tairix_drvrt::{Recorder, RtSupplier};
use tairix_log::{EventId, Field, FieldValue, Level};

use crate::{ClockHardware, Controller, Record};

const EVENT_READY: EventId = EventId(24_300);
const EVENT_RAN: EventId = EventId(24_302);
const EVENT_JOINED: EventId = EventId(24_303);
const EVENT_STOPPED: EventId = EventId(24_304);
const EVENT_ABANDONED: EventId = EventId(24_305);
const EVENT_CUT: EventId = EventId(24_306);
const EVENT_STUCK: EventId = EventId(24_307);
const EVENT_REFUSED: EventId = EventId(24_308);
const EVENT_UNRETURNED: EventId = EventId(24_309);

/// The clock controller node the driver running with `caps` was loaded for.
///
/// # Errors
///
/// The exit code to end with, its reason logged: grants that cannot be read,
/// or a node carrying no clock duty.
pub fn open(caps: CapabilitySet) -> Result<DutyNode, i32> {
    DutyNode::open(caps, LinkRole::Clock)
}

/// The rate the tree states for the fixed clock at `entry` of `node`'s
/// `clocks`.
///
/// # Errors
///
/// The exit code to end with when the tree states none.
pub fn fixed_rate(node: &DutyNode, entry: u8) -> Result<u64, i32> {
    node.resources()
        .filter_map(HwResource::fixed_clock)
        .find_map(|(at, hz)| (at == entry).then_some(hz))
        .ok_or_else(|| {
            end(
                DriverExit::NoResources,
                &[step(
                    "clock: the tree states no rate for a clock the node is fed from",
                )],
            )
        })
}

/// Bind `node`'s endpoint under its clock duty and serve `hardware` from it
/// for the life of the driver process.
///
/// Never returns on the success path; a refusal ends the driver with its
/// reason logged.
pub fn serve<H: ClockHardware>(node: &DutyNode, hardware: H) -> i32 {
    let endpoint = node.duty().endpoint();
    let Ok(mut controller) = Controller::new(hardware, RtSupplier::new(endpoint), Log, endpoint)
    else {
        return end(
            DriverExit::BringupFailed,
            &[step("clock: the clocks' table could not be made")],
        );
    };
    let mut request = [0u8; CLOCK_MAX_REQUEST];
    // Every call is answered at once and each consumer has only its own in
    // flight, so the containment bound is the only bound.
    let service = Service::bind(
        Endpoint {
            id: endpoint,
            request: &mut request,
            max_reply: CLOCK_MAX_REPLY,
            capacity: IPC_CALL_CAPACITY_MAX,
        },
        &[],
    );
    let ended = match service {
        Ok(service) => {
            log_event(Level::Info, EVENT_READY, "clock: controller serving", &[]);
            service.run(&mut controller)
        }
        Err(ended) => ended,
    };
    ended.end()
}

/// The system log, as a clock endpoint records to it.
struct Log;

impl Recorder<Record> for Log {
    fn record(&mut self, record: Record) {
        let mut hex = [0u8; PROC_ID_HEX_LEN];
        match record {
            Record::Ran { clock, hz, holder } => log_event(
                Level::Info,
                EVENT_RAN,
                "clock: running",
                &[
                    clock_field(clock),
                    Field {
                        key: "hz",
                        value: FieldValue::UnsignedInt(hz),
                    },
                    instance_field("holder", holder, &mut hex),
                ],
            ),
            Record::Joined { clock, holder } => log_event(
                Level::Info,
                EVENT_JOINED,
                "clock: joined at the rate it runs at",
                &[
                    clock_field(clock),
                    instance_field("holder", holder, &mut hex),
                ],
            ),
            Record::Stopped { clock } => log_event(
                Level::Info,
                EVENT_STOPPED,
                "clock: returned with its last holder",
                &[clock_field(clock)],
            ),
            Record::Abandoned { clock, holder } => log_event(
                Level::Info,
                EVENT_ABANDONED,
                "clock: hold dropped with its ended holder",
                &[
                    clock_field(clock),
                    instance_field("holder", holder, &mut hex),
                ],
            ),
            Record::Cut { clock } => log_event(
                Level::Warn,
                EVENT_CUT,
                "clock: old setting cut off mid-period",
                &[clock_field(clock)],
            ),
            Record::Stuck { clock } => log_event(
                Level::Error,
                EVENT_STUCK,
                "clock: would not change; left as it is",
                &[clock_field(clock)],
            ),
            Record::Unreturned { clock } => log_event(
                Level::Warn,
                EVENT_UNRETURNED,
                "clock: could not be returned with its last holder; left running",
                &[clock_field(clock)],
            ),
            Record::Refused { op, caller, reason } => {
                let op = Field {
                    key: "op",
                    value: FieldValue::Str(op_name(op)),
                };
                let reason = Field {
                    key: "reason",
                    value: FieldValue::Error(reason),
                };
                match caller {
                    Some(caller) => log_event(
                        Level::Warn,
                        EVENT_REFUSED,
                        "clock: request refused",
                        &[op, reason, instance_field("caller", caller, &mut hex)],
                    ),
                    None => log_event(
                        Level::Warn,
                        EVENT_REFUSED,
                        "clock: request refused",
                        &[op, reason],
                    ),
                }
            }
        }
    }
}

fn op_name(op: Option<ClockOp>) -> &'static str {
    match op {
        None => "frame",
        Some(ClockOp::Describe) => "describe",
        Some(ClockOp::Run) => "run",
        Some(ClockOp::Release) => "release",
        Some(ClockOp::Floor) => "floor",
    }
}

fn clock_field(clock: u32) -> Field<'static> {
    Field {
        key: "clock",
        value: FieldValue::UnsignedInt(u64::from(clock)),
    }
}
