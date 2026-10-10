//! The `Run` binary of the BCM2711 clock manager driver, autoloaded into user
//! space by `devmgr` for a discovered `brcm,bcm2711-cprman` node.
//!
//! It maps the node's register window, reads the oscillator's rate from the
//! node's first `clocks` entry, binds the node's endpoint under its clock
//! `LinkDuty` and serves it. It touches no clock until a consumer asks. On the
//! host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::clock::{ClockOp, CLOCK_MAX_REPLY, CLOCK_MAX_REQUEST};
    use tairix_abi::driver::sole_register_window;
    use tairix_abi::hwlink::{LinkDuty, LinkRole};
    use tairix_abi::hwtree::HwResource;
    use tairix_abi::ipc::IPC_CALL_CAPACITY_MAX;
    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind, WAITSET_TIMEOUT_NONE};
    use tairix_abi::{CapabilityId, MmioMapper, ProcId, HW_NODE_MAX_RESOURCES, PROC_ID_HEX_LEN};
    use tairix_caps::CapabilitySet;
    use tairix_drv_clock_bcm2711_cprman::controller::{Controller, Record, Recorder};
    use tairix_drv_clock_bcm2711_cprman::cprman::Cprman;
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls, RtSupplier};
    use tairix_log::{log, Event, EventId, Field, FieldValue, Level};
    use tairix_rt::LogSink;

    const EXIT_NO_HOST: i32 = 90;
    const EXIT_NO_RESOURCES: i32 = 91;
    const EXIT_BRINGUP_FAILED: i32 = 92;
    const EXIT_SERVE_FAILED: i32 = 95;

    const EVENT_READY: EventId = EventId(24_300);
    const EVENT_FAILED: EventId = EventId(24_301);
    const EVENT_RAN: EventId = EventId(24_302);
    const EVENT_JOINED: EventId = EventId(24_303);
    const EVENT_STOPPED: EventId = EventId(24_304);
    const EVENT_ABANDONED: EventId = EventId(24_305);
    const EVENT_KILLED: EventId = EventId(24_306);
    const EVENT_STUCK: EventId = EventId(24_307);
    const EVENT_REFUSED: EventId = EventId(24_308);

    const ENDPOINT_TOKEN: u64 = 0;
    const PEER_EXIT_TOKEN: u64 = 1;

    /// The binding's position of the oscillator among the node's `clocks`.
    const OSCILLATOR_ENTRY: u8 = 0;

    fn driver_caps() -> CapabilitySet {
        let mut caps = CapabilitySet::empty();
        for cap in tairix_drv_clock_bcm2711_cprman::REQUIRED_CAPABILITIES {
            caps.insert(*cap);
        }
        caps
    }

    /// Record why the driver is ending, and answer the code it ends with.
    fn fail(code: i32, reason: &'static str) -> i32 {
        emit(Level::Error, EVENT_FAILED, reason, &[]);
        code
    }

    fn main() -> i32 {
        let Ok(host) = RtDriverHost::from_grants_query(driver_caps(), RtGrantSyscalls, None) else {
            return fail(EXIT_NO_HOST, "clock: the node's grants could not be read");
        };
        let mut granted = [None; HW_NODE_MAX_RESOURCES];
        for (slot, resource) in granted.iter_mut().zip(host.resources()) {
            *slot = Some(*resource);
        }
        let resources = || granted.iter().flatten();
        let Ok((base, len)) = sole_register_window(resources()) else {
            return fail(
                EXIT_NO_RESOURCES,
                "clock: the node names no single register window",
            );
        };
        let Some(duty) = resources()
            .find_map(|r| r.link_duty().ok())
            .filter(|duty| duty.role() == LinkRole::Clock)
        else {
            return fail(EXIT_NO_RESOURCES, "clock: the node carries no clock duty");
        };
        let Some(oscillator) = resources()
            .filter_map(HwResource::fixed_clock)
            .find_map(|(entry, hz)| (entry == OSCILLATOR_ENTRY).then_some(hz))
        else {
            return fail(
                EXIT_NO_RESOURCES,
                "clock: the tree states no rate for the oscillator",
            );
        };
        let Ok(regs) = host.map_window(base, len) else {
            return fail(
                EXIT_BRINGUP_FAILED,
                "clock: the register window would not map",
            );
        };
        let Ok(cprman) = Cprman::new(&regs, oscillator) else {
            return fail(
                EXIT_BRINGUP_FAILED,
                "clock: the register window does not reach the PLLs",
            );
        };
        let mut controller = Controller::new(
            cprman,
            RtSupplier::new(duty.endpoint()),
            Log,
            duty.endpoint(),
        );
        let Some(set) = serve_set(&duty) else {
            return fail(
                EXIT_SERVE_FAILED,
                "clock: the endpoint or its wait set could not be made",
            );
        };
        emit(
            Level::Info,
            EVENT_READY,
            "clock: controller serving",
            &[Field {
                key: "oscillator",
                value: FieldValue::UnsignedInt(oscillator),
            }],
        );
        serve(&mut controller, set, duty.endpoint())
    }

    /// Bind the node's endpoint under its duty and gather it and peer exits
    /// into one wait set.
    fn serve_set(duty: &LinkDuty) -> Option<u64> {
        // Only a caller whose grant covers this endpoint gets in: a clock
        // link on this controller.
        let mut send_caps = CapabilitySet::empty();
        send_caps.insert(CapabilityId::IPC_ENDPOINT);
        // Every call is answered at once and each consumer has only its own
        // in flight, so the containment bound is the only bound: a consumer
        // the queue turned away would have no answer to wait for.
        if tairix_rt::call_create(
            duty.endpoint(),
            &send_caps,
            &CapabilitySet::empty(),
            CLOCK_MAX_REQUEST,
            CLOCK_MAX_REPLY,
            IPC_CALL_CAPACITY_MAX,
        ) != 0
        {
            return None;
        }
        let set = u64::try_from(tairix_rt::waitset_create()).ok()?;
        let add =
            |kind, id, token| tairix_rt::waitset_ctl(set, WaitSetOp::Add, kind, id, token) == 0;
        (add(WaitSourceKind::Endpoint, duty.endpoint(), ENDPOINT_TOKEN)
            && add(WaitSourceKind::PeerExit, 0, PEER_EXIT_TOKEN))
        .then_some(set)
    }

    fn serve<R: tairix_abi::RegisterBlock + ?Sized>(
        controller: &mut Controller<'_, R, RtSupplier, Log>,
        set: u64,
        endpoint: u64,
    ) -> i32 {
        let mut request = [0u8; CLOCK_MAX_REQUEST];
        loop {
            let mut token = 0u64;
            let woke = tairix_rt::waitset_wait(set, WAITSET_TIMEOUT_NONE, &mut token);
            if woke < 0 {
                return fail(EXIT_SERVE_FAILED, "clock: the serve wait set faulted");
            }
            if woke != 0 {
                continue;
            }
            match token {
                ENDPOINT_TOKEN => match tairix_rt::call_recv_ready(endpoint, &mut request) {
                    Ok(Some(call)) => controller.serve(call.ticket, &request[..call.len]),
                    Ok(None) => {}
                    Err(_) => return fail(EXIT_SERVE_FAILED, "clock: the endpoint was lost"),
                },
                PEER_EXIT_TOKEN => {
                    while let Ok(peer) = tairix_rt::peer_exit_take() {
                        controller.peer_exited(peer);
                    }
                }
                _ => {}
            }
        }
    }

    /// The system log, as the endpoint records to it.
    struct Log;

    impl Recorder for Log {
        fn record(&mut self, record: Record) {
            let mut hex = [0u8; PROC_ID_HEX_LEN];
            match record {
                Record::Ran { clock, hz, holder } => emit(
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
                Record::Joined { clock, holder } => emit(
                    Level::Info,
                    EVENT_JOINED,
                    "clock: joined at the rate it runs at",
                    &[
                        clock_field(clock),
                        instance_field("holder", holder, &mut hex),
                    ],
                ),
                Record::Stopped { clock } => emit(
                    Level::Info,
                    EVENT_STOPPED,
                    "clock: stopped with its last holder",
                    &[clock_field(clock)],
                ),
                Record::Abandoned { clock, holder } => emit(
                    Level::Info,
                    EVENT_ABANDONED,
                    "clock: hold dropped with its ended holder",
                    &[
                        clock_field(clock),
                        instance_field("holder", holder, &mut hex),
                    ],
                ),
                Record::Killed { clock } => emit(
                    Level::Warn,
                    EVENT_KILLED,
                    "clock: generator reset before it finished its period",
                    &[clock_field(clock)],
                ),
                Record::Stuck { clock } => emit(
                    Level::Error,
                    EVENT_STUCK,
                    "clock: generator kept running through its reset; left as it is",
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
                        Some(caller) => emit(
                            Level::Warn,
                            EVENT_REFUSED,
                            "clock: request refused",
                            &[op, reason, instance_field("caller", caller, &mut hex)],
                        ),
                        None => emit(
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
        }
    }

    fn emit(level: Level, id: EventId, message: &'static str, fields: &[Field<'_>]) {
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

    fn clock_field(clock: u32) -> Field<'static> {
        Field {
            key: "clock",
            value: FieldValue::UnsignedInt(u64::from(clock)),
        }
    }

    fn instance_field<'h>(
        key: &'static str,
        instance: ProcId,
        hex: &'h mut [u8; PROC_ID_HEX_LEN],
    ) -> Field<'h> {
        Field {
            key,
            value: FieldValue::Str(instance.write_hex(hex)),
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
