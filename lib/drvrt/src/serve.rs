//! A link supplier's bring-up from its node's grants, the loop it serves its
//! node's endpoint from, a device channel's bind and publication, the board
//! clock's endpoint, a temperature sensor's channel, and the log record
//! helpers their decisions are written with.

use tairix_abi::driver::rtc::Rtc;
use tairix_abi::driver::thermal::{
    self, ThermalSensor, THERMAL_SENSOR_ENDPOINTS, THERMAL_SENSOR_NODE_COMPATIBLE,
};
use tairix_abi::driver::{named_register_window, sole_register_window, DeviceWake};
use tairix_abi::hwlink::{LinkDuty, LinkRole};
use tairix_abi::hwtree::{HwResource, NodeEndpointBlock};
use tairix_abi::ipc::IPC_CALL_CAPACITY_MAX;
use tairix_abi::rtc_ipc::{self, RTC_ENDPOINT};
use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
use tairix_abi::{
    CapabilityId, Delay, Errno, HwDeviceClass, HwNode, MmioMapper, ProcId, RegisterWindow,
    PROC_ID_HEX_LEN,
};
use tairix_caps::CapabilitySet;
use tairix_log::{log, Event, EventId, Field, FieldValue, Level};
use tairix_rt::LogSink;

use crate::exit::{end, step, DriverExit};
use crate::supplier::{timer, Timer};
use crate::{RtDriverHost, RtGrantSyscalls, Supplier};

/// A link supplier's node as its driver received it: the grants and the duty
/// it serves.
pub struct DutyNode {
    host: RtDriverHost<RtGrantSyscalls>,
    duty: LinkDuty,
}

impl DutyNode {
    /// The node the driver running with `caps` was loaded for, carrying a
    /// duty of `role`.
    ///
    /// # Errors
    ///
    /// The exit status to end with, its reason recorded: grants that cannot
    /// be read, or no duty of `role`.
    pub fn open(caps: CapabilitySet, role: LinkRole) -> Result<Self, i32> {
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return Err(end(DriverExit::NoHost, &[]));
        };
        let duty = host
            .resources()
            .find_map(|resource| resource.link_duty().ok().filter(|duty| duty.role() == role));
        let Some(duty) = duty else {
            return Err(end(
                DriverExit::NoResources,
                &[step(
                    "the node carries no duty of the role its driver serves",
                )],
            ));
        };
        Ok(Self { host, duty })
    }

    /// The host the node's grants were read into.
    #[must_use]
    pub const fn host(&self) -> &RtDriverHost<RtGrantSyscalls> {
        &self.host
    }

    /// The host, for narrowing what it carves.
    pub fn host_mut(&mut self) -> &mut RtDriverHost<RtGrantSyscalls> {
        &mut self.host
    }

    /// The duty the node's driver serves.
    #[must_use]
    pub const fn duty(&self) -> LinkDuty {
        self.duty
    }

    /// Every resource the node was granted.
    pub fn resources(&self) -> impl Iterator<Item = &HwResource> {
        self.host.resources()
    }

    /// The node's single register window.
    ///
    /// # Errors
    ///
    /// The exit code to end with when the node names no single window.
    pub fn window(&self) -> Result<(u64, usize), i32> {
        sole_register_window(self.resources()).map_err(|_| {
            end(
                DriverExit::NoResources,
                &[step("the node names no single register window")],
            )
        })
    }

    /// Map the node's single register window.
    ///
    /// # Errors
    ///
    /// The exit code to end with when the node names no single window, or it
    /// will not map.
    pub fn map(&self) -> Result<RegisterWindow, i32> {
        let (base, len) = self.window()?;
        self.host.map_window(base, len).map_err(|_| {
            end(
                DriverExit::BringupFailed,
                &[step("the register window would not map")],
            )
        })
    }

    /// Map the node's register window named `name` in its `reg-names`.
    ///
    /// # Errors
    ///
    /// The exit code to end with when the node names no such single window,
    /// or it will not map.
    pub fn map_named(&self, name: &[u8]) -> Result<RegisterWindow, i32> {
        let (base, len) = named_register_window(self.resources(), name).map_err(|_| {
            end(
                DriverExit::NoResources,
                &[step(
                    "the node names no single register window of a name its driver needs",
                )],
            )
        })?;
        self.host.map_window(base, len).map_err(|_| {
            end(
                DriverExit::BringupFailed,
                &[step("a register window would not map")],
            )
        })
    }
}

const ENDPOINT_TOKEN: u64 = 0;
const PEER_EXIT_TOKEN: u64 = 1;
/// Token of the first event source; source `k` is `WAKE_TOKEN + k`.
const WAKE_TOKEN: u64 = 2;

/// Why a supplier's service ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Ended {
    /// The endpoint could not be bound.
    Unbound,
    /// The wait set could not be made or filled.
    NoWaitSet,
    /// Waiting faulted.
    WaitFaulted,
    /// The endpoint was lost.
    EndpointLost,
    /// Servicing one of the device's event sources faulted.
    EventFaulted,
}

impl Ended {
    /// The reason a log record states.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Unbound => "the endpoint could not be bound",
            Self::NoWaitSet => "the serve wait set could not be made",
            Self::WaitFaulted => "the serve wait set faulted",
            Self::EndpointLost => "the endpoint was lost",
            Self::EventFaulted => "an event source could not be serviced",
        }
    }

    /// The ending the driver exits with.
    #[must_use]
    pub const fn exit(self) -> DriverExit {
        match self {
            Self::Unbound | Self::NoWaitSet => DriverExit::NoService,
            Self::WaitFaulted | Self::EndpointLost | Self::EventFaulted => DriverExit::ServeFailed,
        }
    }

    /// End the driver for this, its reason recorded.
    #[must_use]
    pub fn end(self) -> i32 {
        end(self.exit(), &[step(self.reason())])
    }
}

/// The endpoint a supplier serves and its protocol's bounds.
pub struct Endpoint<'b> {
    /// The endpoint, as the node's duty names it.
    pub id: u64,
    /// Where each request is received: as long as the protocol's longest.
    pub request: &'b mut [u8],
    /// The protocol's longest reply.
    pub max_reply: usize,
    /// How many calls may be queued at once, clamped to the kernel's bound.
    pub capacity: usize,
}

/// A supplier's endpoint, bound and gathered with its holders' ends and its
/// device's event sources into one wait set.
pub struct Service<'b> {
    id: u64,
    request: &'b mut [u8],
    set: u64,
    wakes: usize,
}

impl<'b> Service<'b> {
    /// Bind `endpoint` so only a caller whose grant covers it may call, and
    /// gather it, peers' ends and the device's event sources, `wakes`, into
    /// one wait set.
    ///
    /// # Errors
    ///
    /// [`Ended::Unbound`] or [`Ended::NoWaitSet`].
    pub fn bind(
        Endpoint {
            id,
            request,
            max_reply,
            capacity,
        }: Endpoint<'b>,
        wakes: &[DeviceWake],
    ) -> Result<Self, Ended> {
        let mut send_caps = CapabilitySet::empty();
        send_caps.insert(CapabilityId::IPC_ENDPOINT);
        if tairix_rt::call_create(
            id,
            &send_caps,
            &CapabilitySet::empty(),
            request.len(),
            max_reply,
            capacity.clamp(1, IPC_CALL_CAPACITY_MAX),
        ) != 0
        {
            return Err(Ended::Unbound);
        }
        let set = wait_set(id, wakes).ok_or(Ended::NoWaitSet)?;
        Ok(Self {
            id,
            request,
            set,
            wakes: wakes.len(),
        })
    }

    /// Serve `supplier` until the service ends, answering why it did.
    pub fn run<S: Supplier + ?Sized>(self, supplier: &mut S) -> Ended {
        let clock = tairix_rt::ClockDelay::new();
        let timed_out = -i64::from(Errno::TimedOut.as_i32());
        loop {
            // Checked before waiting, so a stream of calls cannot starve it.
            let timeout_ns = match timer(supplier.deadline_us(), clock.now_us()) {
                Timer::Due => {
                    if supplier.expired().is_err() {
                        return Ended::EventFaulted;
                    }
                    continue;
                }
                Timer::Wait(ns) => ns,
            };
            let mut token = 0u64;
            let status = tairix_rt::waitset_wait(self.set, timeout_ns, &mut token);
            if status == timed_out {
                continue;
            }
            if status < 0 {
                return Ended::WaitFaulted;
            }
            if status != 0 {
                continue;
            }
            match token {
                ENDPOINT_TOKEN => match tairix_rt::call_recv_ready(self.id, self.request) {
                    Ok(Some(call)) => supplier.serve(call.ticket, &self.request[..call.len]),
                    Ok(None) => {}
                    Err(_) => return Ended::EndpointLost,
                },
                PEER_EXIT_TOKEN => {
                    while let Ok(peer) = tairix_rt::peer_exit_take() {
                        supplier.peer_exited(peer);
                    }
                }
                token => {
                    let wake = token
                        .checked_sub(WAKE_TOKEN)
                        .and_then(|index| usize::try_from(index).ok())
                        .filter(|&index| index < self.wakes);
                    if let Some(wake) = wake {
                        if supplier.woke(wake).is_err() {
                            return Ended::EventFaulted;
                        }
                    }
                }
            }
        }
    }
}

/// Calls the board clock's endpoint queues: its one client asks a handful of
/// times a day, each answered before the next is received.
const RTC_ENDPOINT_CAPACITY: usize = 4;

/// Serve the board's clock chip `chip` on the RTC endpoint for the life of
/// the driver, answering the status the driver ends with.
///
/// Only the holder of `CAP_TIME_SET` may call: the one principal that sets the
/// machine clock from a chip. A machine has one such endpoint, so the driver
/// of a second chip finds it bound and declines its node.
pub fn rtc<R: Rtc + ?Sized>(chip: &mut R) -> i32 {
    let mut send_caps = CapabilitySet::empty();
    send_caps.insert(CapabilityId::TIME_SET);
    let bound = tairix_rt::call_create(
        RTC_ENDPOINT,
        &send_caps,
        &CapabilitySet::empty(),
        rtc_ipc::REQUEST_LEN,
        rtc_ipc::REPLY_LEN,
        RTC_ENDPOINT_CAPACITY,
    );
    if bound == -i64::from(Errno::AlreadyExists.as_i32()) {
        return end(
            DriverExit::Declined,
            &[step("rtc: another clock chip serves the machine")],
        );
    }
    if bound != 0 {
        return end(
            DriverExit::NoService,
            &[step("rtc: the endpoint could not be bound")],
        );
    }
    let mut request = [0u8; rtc_ipc::REQUEST_LEN];
    let mut reply = [0u8; rtc_ipc::REPLY_LEN];
    loop {
        let mut ticket = 0u64;
        let Ok(len) = tairix_rt::call_recv(RTC_ENDPOINT, &mut request, &mut ticket) else {
            return end(
                DriverExit::ServeFailed,
                &[step("rtc: the endpoint was lost")],
            );
        };
        // A reply that cannot be framed goes out empty, which the client
        // decodes as a fail-closed truncation.
        let framed = rtc_ipc::serve_request(chip, &request[..len], &mut reply).unwrap_or(0);
        let _ = tairix_rt::call_reply(RTC_ENDPOINT, ticket, &reply[..framed]);
    }
}

/// A device channel's endpoint: the id its driver's node is given in
/// `block`, served to the holders of `gate`, and its protocol's bounds.
pub struct DeviceChannel {
    /// The block the channel's id is the driver's node's entry of.
    pub block: NodeEndpointBlock,
    /// The capability a caller must hold: the kernel admits nobody else.
    pub gate: CapabilityId,
    /// The protocol's longest request.
    pub max_request: usize,
    /// The protocol's longest reply.
    pub max_reply: usize,
    /// How many calls may be queued at once, clamped to the kernel's bound.
    pub capacity: usize,
}

impl DeviceChannel {
    /// Bind the channel of the node this driver was loaded for, answering
    /// its id; [`None`] on any refusal.
    #[must_use]
    pub fn bind(&self) -> Option<u64> {
        let id = self.block.endpoint(tairix_rt::hw_self_node().ok()?.node());
        let mut send_caps = CapabilitySet::empty();
        send_caps.insert(self.gate);
        let bound = tairix_rt::call_create(
            id,
            &send_caps,
            &CapabilitySet::empty(),
            self.max_request,
            self.max_reply,
            self.capacity.clamp(1, IPC_CALL_CAPACITY_MAX),
        );
        (bound == 0).then_some(id)
    }
}

/// Publish the node `compatible` of `class` naming a bound channel's
/// `endpoint` and stating `facts`, so the service that uses the device finds
/// it in the hardware tree only once it serves. The kernel parents it under
/// the node the driver was loaded for. Answers the node's id; [`None`] on any
/// refusal.
///
/// The channel is the one child a channel-serving driver publishes, so the
/// driver's own node is then settled: the device manager waits on a node a
/// publishing driver holds until it settles, and the boot with it.
#[must_use]
pub fn publish_channel(
    class: HwDeviceClass,
    compatible: &[u8],
    endpoint: u64,
    facts: &[HwResource],
) -> Option<u32> {
    crate::tree::publish_channel(&mut RtTree, class, compatible, endpoint, facts)
}

/// The hardware-tree calls through the kernel.
struct RtTree;

impl crate::tree::TreeCalls for RtTree {
    fn emit(&mut self, node: &HwNode) -> i64 {
        tairix_rt::hw_emit_node(node)
    }

    fn settle(&mut self) -> i64 {
        tairix_rt::hw_node_settle()
    }
}

/// Calls a sensor's channel queues: its consumer asks once a period, each
/// answered before the next is received.
const SENSOR_CHANNEL_CAPACITY: usize = 4;

/// Serve `sensor` on its node's sensor channel for the life of the driver,
/// answering the status the driver ends with.
///
/// Only a holder of `CAP_SYSINFO_HW` may call: a sensor is hardware the
/// machine's inventory reads. The channel's node is published once the
/// channel serves; what the sensor measures is discovery's to state, on the
/// sensor's own node, never the driver's.
pub fn thermal_sensor<S: ThermalSensor + ?Sized>(sensor: &mut S) -> i32 {
    let channel = DeviceChannel {
        block: THERMAL_SENSOR_ENDPOINTS,
        gate: CapabilityId::SYSINFO_HW,
        max_request: thermal::SENSOR_REQUEST_LEN,
        max_reply: thermal::SENSOR_REPLY_LEN,
        capacity: SENSOR_CHANNEL_CAPACITY,
    };
    let Some(endpoint) = channel.bind() else {
        return end(
            DriverExit::NoService,
            &[step("sensor: the channel could not be bound")],
        );
    };
    if publish_channel(
        HwDeviceClass::Other,
        THERMAL_SENSOR_NODE_COMPATIBLE,
        endpoint,
        &[],
    )
    .is_none()
    {
        return end(
            DriverExit::NoService,
            &[step("sensor: the channel's node could not be published")],
        );
    }
    let mut request = [0u8; thermal::SENSOR_REQUEST_LEN];
    let mut reply = [0u8; thermal::SENSOR_REPLY_LEN];
    loop {
        let mut ticket = 0u64;
        let Ok(len) = tairix_rt::call_recv(endpoint, &mut request, &mut ticket) else {
            return end(
                DriverExit::ServeFailed,
                &[step("sensor: the channel was lost")],
            );
        };
        // A reply that cannot be framed goes out empty, which the client
        // decodes as a fail-closed refusal.
        let framed = thermal::serve_request(sensor, &request[..len], &mut reply).unwrap_or(0);
        let _ = tairix_rt::call_reply(endpoint, ticket, &reply[..framed]);
    }
}

fn wait_set(endpoint: u64, wakes: &[DeviceWake]) -> Option<u64> {
    let set = u64::try_from(tairix_rt::waitset_create()).ok()?;
    let add = |kind, id, token| tairix_rt::waitset_ctl(set, WaitSetOp::Add, kind, id, token) == 0;
    let waits = add(WaitSourceKind::Endpoint, endpoint, ENDPOINT_TOKEN)
        && add(WaitSourceKind::PeerExit, 0, PEER_EXIT_TOKEN)
        && (WAKE_TOKEN..).zip(wakes).all(|(token, wake)| {
            let (kind, id) = wake.member();
            add(kind, id, token)
        });
    waits.then_some(set)
}

/// Write one structured record to the system log.
pub fn log_event(level: Level, id: EventId, message: &'static str, fields: &[Field<'_>]) {
    log(
        &LogSink,
        &Event {
            level,
            id,
            message,
            fields,
        },
    );
}

/// A field naming the process instance `instance`, spelled into `hex`.
pub fn instance_field<'h>(
    key: &'static str,
    instance: ProcId,
    hex: &'h mut [u8; PROC_ID_HEX_LEN],
) -> Field<'h> {
    Field {
        key,
        value: FieldValue::Str(instance.write_hex(hex)),
    }
}
