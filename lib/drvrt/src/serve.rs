//! A link supplier's bring-up from its node's grants, the loop it serves its
//! node's endpoint from, and the log record helpers its decisions are written
//! with.

use tairix_abi::driver::{named_register_window, sole_register_window, DeviceWake};
use tairix_abi::hwlink::{LinkDuty, LinkRole};
use tairix_abi::hwtree::HwResource;
use tairix_abi::ipc::IPC_CALL_CAPACITY_MAX;
use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
use tairix_abi::{CapabilityId, Delay, Errno, MmioMapper, ProcId, RegisterWindow, PROC_ID_HEX_LEN};
use tairix_caps::CapabilitySet;
use tairix_log::{log, Event, EventId, Field, FieldValue, Level};
use tairix_rt::LogSink;

use crate::supplier::{timer, Timer};
use crate::{RtDriverHost, RtGrantSyscalls, Supplier};

/// Exit codes a link supplier's driver ends with.
pub mod exit {
    /// The node's grants could not be read.
    pub const NO_HOST: i32 = 90;
    /// The node lacks a resource its driver needs.
    pub const NO_RESOURCES: i32 = 91;
    /// The driver's own bring-up failed.
    pub const BRINGUP_FAILED: i32 = 92;
    /// The service ended.
    pub const SERVE_FAILED: i32 = 95;
}

/// Record under `id` why the driver is ending, and answer `code`, the code it
/// ends with.
#[must_use]
pub fn fail(id: EventId, code: i32, reason: &'static str) -> i32 {
    log_event(Level::Error, id, reason, &[]);
    code
}

/// A link supplier's node as its driver received it: the grants, the duty it
/// serves, and the event its driver's failures are recorded under.
pub struct DutyNode {
    host: RtDriverHost<RtGrantSyscalls>,
    duty: LinkDuty,
    failed: EventId,
}

impl DutyNode {
    /// The node the driver running with `caps` was loaded for, carrying a
    /// duty of `role`; every failure is recorded under `failed`.
    ///
    /// # Errors
    ///
    /// The exit code to end with, its reason recorded: grants that cannot be
    /// read, or no duty of `role`.
    pub fn open(caps: CapabilitySet, role: LinkRole, failed: EventId) -> Result<Self, i32> {
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return Err(fail(
                failed,
                exit::NO_HOST,
                "the node's grants could not be read",
            ));
        };
        let duty = host
            .resources()
            .find_map(|resource| resource.link_duty().ok().filter(|duty| duty.role() == role));
        let Some(duty) = duty else {
            return Err(fail(
                failed,
                exit::NO_RESOURCES,
                "the node carries no duty of the role its driver serves",
            ));
        };
        Ok(Self { host, duty, failed })
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
            self.fail(
                exit::NO_RESOURCES,
                "the node names no single register window",
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
        self.host
            .map_window(base, len)
            .map_err(|_| self.fail(exit::BRINGUP_FAILED, "the register window would not map"))
    }

    /// Map the node's register window named `name` in its `reg-names`.
    ///
    /// # Errors
    ///
    /// The exit code to end with when the node names no such single window,
    /// or it will not map.
    pub fn map_named(&self, name: &[u8]) -> Result<RegisterWindow, i32> {
        let (base, len) = named_register_window(self.resources(), name).map_err(|_| {
            self.fail(
                exit::NO_RESOURCES,
                "the node names no single register window of a name its driver needs",
            )
        })?;
        self.host
            .map_window(base, len)
            .map_err(|_| self.fail(exit::BRINGUP_FAILED, "a register window would not map"))
    }

    /// Record why the driver is ending, and answer `code`.
    #[must_use]
    pub fn fail(&self, code: i32, reason: &'static str) -> i32 {
        fail(self.failed, code, reason)
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
