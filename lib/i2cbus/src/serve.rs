//! The freestanding half: an I²C controller driver's bring-up from its node's
//! grants, the loop that serves the node's endpoint, and the log the endpoint
//! records to.
//!
//! A driver [`open`]s its node, reads the speed it [`declared_speed`]s, builds
//! its [`I2cBus`] over the mapped register window and bound interrupt, and
//! hands it to [`serve`]. Every failure ends the driver with a stable exit code
//! and its reason logged.

use core::num::NonZeroU32;

use tairix_abi::driver::i2c::{
    I2cFault, I2cOp, I2cReach, I2C_BUFFER_LEN, I2C_MAX_REPLY, I2C_MAX_REQUEST, STANDARD_MODE_HZ,
};
use tairix_abi::hwlink::LinkRole;
use tairix_abi::hwtree::HwProperty;
use tairix_abi::ipc::IPC_CALL_CAPACITY_MAX;
use tairix_abi::{Errno, ProcId, PROC_ID_HEX_LEN};
use tairix_caps::CapabilitySet;
use tairix_drvrt::exit::{end, step, DriverExit};
use tairix_drvrt::serve::{instance_field, log_event, DutyNode, Endpoint, Service};
use tairix_drvrt::{Recorder, RtSupplier};
use tairix_log::{EventId, Field, FieldValue, Level};
use tairix_rt::shm::SharedRegion;

use crate::{BufferHost, Controller, I2cBus, Record};

const EVENT_READY: EventId = EventId(24_100);
const EVENT_OPENED: EventId = EventId(24_102);
const EVENT_CLOSED: EventId = EventId(24_103);
const EVENT_ABANDONED: EventId = EventId(24_104);
const EVENT_FAULTED: EventId = EventId(24_105);
const EVENT_REFUSED: EventId = EventId(24_106);

/// The I²C controller node the driver running with `caps` was loaded for.
///
/// # Errors
///
/// The exit code to end with, its reason logged: grants that cannot be read,
/// or a node carrying no I²C duty.
pub fn open(caps: CapabilitySet) -> Result<DutyNode, i32> {
    DutyNode::open(caps, LinkRole::I2c)
}

/// The speed `node` declares its bus runs at: its `clock-frequency`, or
/// standard mode when it declares none.
///
/// # Errors
///
/// The exit code to end with for a speed no bus runs at.
pub fn declared_speed(node: &DutyNode) -> Result<NonZeroU32, i32> {
    let hz = node
        .host()
        .property(HwProperty::BusSpeed)
        .unwrap_or(u64::from(STANDARD_MODE_HZ));
    u32::try_from(hz)
        .ok()
        .and_then(NonZeroU32::new)
        .ok_or_else(|| {
            end(
                DriverExit::NoResources,
                &[step("i2c: the node declares a speed no bus runs at")],
            )
        })
}

/// Bind `node`'s endpoint under its I²C duty and serve `bus` from it for the
/// life of the driver process.
///
/// Never returns on the success path; a refusal ends the driver with its
/// reason logged.
pub fn serve<B: I2cBus>(node: &DutyNode, bus: B) -> i32 {
    let Ok(instance) = tairix_rt::self_origin().map(|origin| origin.proc_id()) else {
        return end(
            DriverExit::BringupFailed,
            &[step("i2c: this process's instance is unknown")],
        );
    };
    let endpoint = node.duty().endpoint();
    let mut controller = Controller::new(
        bus,
        Kernel { endpoint, instance },
        RtSupplier::new(endpoint),
        Log,
        endpoint,
    );
    let mut request = [0u8; I2C_MAX_REQUEST];
    // Each consumer blocks on its own call, so the containment bound is the
    // only bound.
    let service = Service::bind(
        Endpoint {
            id: endpoint,
            request: &mut request,
            max_reply: I2C_MAX_REPLY,
            capacity: IPC_CALL_CAPACITY_MAX,
        },
        &[],
    );
    let ended = match service {
        Ok(service) => {
            log_event(Level::Info, EVENT_READY, "i2c: controller serving", &[]);
            service.run(&mut controller)
        }
        Err(ended) => ended,
    };
    ended.end()
}

/// A channel's buffer: a region this process made and grants its client.
struct Region(SharedRegion);

impl AsMut<[u8]> for Region {
    fn as_mut(&mut self) -> &mut [u8] {
        self.0.bytes_mut()
    }
}

/// The kernel, as the endpoint makes and grants buffers through it.
struct Kernel {
    endpoint: u64,
    instance: ProcId,
}

impl BufferHost for Kernel {
    type Buffer = Region;

    fn make(&mut self) -> Result<Region, Errno> {
        SharedRegion::create(I2C_BUFFER_LEN)
            .map(Region)
            .ok_or(Errno::OutOfMemory)
    }

    fn grant(&mut self, buffer: &Region, ticket: u64) -> Result<u64, Errno> {
        Errno::syscall_result(tairix_rt::shm_grant_peer(
            buffer.0.id(),
            self.endpoint,
            ticket,
        ))
    }

    fn instance(&self) -> ProcId {
        self.instance
    }
}

/// The system log, as an I²C endpoint records to it.
struct Log;

impl Recorder<Record> for Log {
    fn record(&mut self, record: Record) {
        let mut hex = [0u8; PROC_ID_HEX_LEN];
        match record {
            Record::Opened { holder, reach } => log_event(
                Level::Info,
                EVENT_OPENED,
                "i2c: channel opened",
                &[
                    instance_field("holder", holder, &mut hex),
                    reach_field(reach),
                ],
            ),
            Record::Closed { holder } => log_event(
                Level::Info,
                EVENT_CLOSED,
                "i2c: channel closed",
                &[instance_field("holder", holder, &mut hex)],
            ),
            Record::Abandoned { holder } => log_event(
                Level::Info,
                EVENT_ABANDONED,
                "i2c: channels dropped with their ended holder",
                &[instance_field("holder", holder, &mut hex)],
            ),
            Record::Faulted { holder, fault } => {
                let (level, name, message) = fault_parts(fault);
                log_event(
                    level,
                    EVENT_FAULTED,
                    "i2c: transaction faulted",
                    &[
                        instance_field("holder", holder, &mut hex),
                        Field {
                            key: "fault",
                            value: FieldValue::Str(name),
                        },
                        Field {
                            key: "message",
                            value: FieldValue::UnsignedInt(u64::from(message)),
                        },
                    ],
                );
            }
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
                        "i2c: request refused",
                        &[op, reason, instance_field("caller", caller, &mut hex)],
                    ),
                    None => log_event(
                        Level::Warn,
                        EVENT_REFUSED,
                        "i2c: request refused",
                        &[op, reason],
                    ),
                }
            }
        }
    }
}

/// How loudly a fault is recorded, its name, and the message it ended in: a
/// target that does not answer is a finding, any other fault a warning.
const fn fault_parts(fault: I2cFault) -> (Level, &'static str, u8) {
    match fault {
        I2cFault::NoAck { message } => (Level::Info, "no-ack", message),
        I2cFault::ArbitrationLost { message } => (Level::Warn, "arbitration-lost", message),
        I2cFault::BusFault { message } => (Level::Warn, "bus-fault", message),
        I2cFault::TimedOut { message } => (Level::Warn, "timed-out", message),
    }
}

fn reach_field(reach: I2cReach) -> Field<'static> {
    match reach {
        I2cReach::Target(address) => Field {
            key: "address",
            value: FieldValue::UnsignedInt(u64::from(address.get())),
        },
        I2cReach::Bus => Field {
            key: "reach",
            value: FieldValue::Str("bus"),
        },
    }
}

fn op_name(op: Option<I2cOp>) -> &'static str {
    match op {
        None => "frame",
        Some(I2cOp::Open) => "open",
        Some(I2cOp::Transfer) => "transfer",
        Some(I2cOp::Close) => "close",
    }
}
