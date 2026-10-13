//! The freestanding half: a controller driver's bring-up from its node's
//! grants, and the loop that serves the node's endpoint.
//!
//! A driver opens its [`Node`], maps the register window, states its engines'
//! reach, builds its engine over the window and the node's carves, and hands
//! the engine to [`serve`]. Every failure ends the driver with a stable exit
//! code and its reason logged.

use tairix_abi::driver::dma::{DmaHost, DmaReach};
use tairix_abi::driver::dmaengine::{
    DmaEngine, DmaEngineOp, DMA_ENGINE_MAX_REPLY, DMA_ENGINE_MAX_REQUEST,
};
use tairix_abi::driver::DeviceWake;
use tairix_abi::hwlink::LinkRole;
use tairix_abi::hwtree::{HwResource, HwResourceKind};
use tairix_abi::time::Duration64;
use tairix_abi::{
    DriverError, Errno, ProcId, RegisterWindow, HW_NODE_MAX_RESOURCES, PROC_ID_HEX_LEN,
};
use tairix_caps::CapabilitySet;
use tairix_drvrt::exit::{end, step, DriverExit};
use tairix_drvrt::serve::{instance_field, log_event, DutyNode, Endpoint, Service};
use tairix_drvrt::{RtSupplier, Supplier};
use tairix_log::{EventId, Field, FieldValue, Level};

use crate::{split_windows, Buffer, Controller, ControllerHost, Record};

const EVENT_READY: EventId = EventId(24_200);
const EVENT_CLAIMED: EventId = EventId(24_202);
const EVENT_RECLAIMED: EventId = EventId(24_203);
const EVENT_REFUSED: EventId = EventId(24_204);
const EVENT_FAULTED: EventId = EventId(24_205);
const EVENT_LOST: EventId = EventId(24_206);
const EVENT_ABANDONED: EventId = EventId(24_207);
const EVENT_UNDRAINED: EventId = EventId(24_208);
const EVENT_LINE_UNBOUND: EventId = EventId(24_209);
const EVENT_UNRESET: EventId = EventId(24_210);

/// One bound interrupt line and the channels it serves.
#[derive(Copy, Clone)]
struct Line {
    handle: u64,
    channels: u64,
}

/// A controller node as its driver received it: the grants and duty, the
/// channels the tree leaves this system, and the windows the engines reach
/// the world through.
pub struct Node {
    granted: DutyNode,
    mask: u64,
    peripheral: Option<HwResource>,
    memory: u64,
}

impl Node {
    /// The node the driver running with `caps` was loaded for, with every
    /// carve placed under its memory window.
    ///
    /// # Errors
    ///
    /// The exit code to end with, its reason logged: grants that cannot be
    /// read, or a node naming no single register window, no duty, no channel
    /// mask, or no memory window.
    pub fn open(caps: CapabilitySet) -> Result<Self, i32> {
        let mut node = DutyNode::open(caps, LinkRole::Dma)?;
        let (base, len) = node.window()?;
        // The binding makes the mask mandatory; the firmware owns what it
        // leaves out.
        let Some(mask) = node.duty().channels() else {
            return Err(end(
                DriverExit::NoResources,
                &[step("dma: the tree states no channel mask")],
            ));
        };
        let (peripheral, memory) = split_windows(
            || node.resources().filter(|r| r.is_translated_dma_window()),
            base,
            len as u64,
        );
        let Some(memory) = memory else {
            return Err(end(
                DriverExit::NoResources,
                &[step("dma: the node reaches no memory")],
            ));
        };
        let memory_grant = node.host().grant_handle(&memory);
        let (Some(memory_grant), Ok(())) =
            (memory_grant, node.host_mut().select_dma_window(&memory))
        else {
            return Err(end(
                DriverExit::NoResources,
                &[step("dma: the memory window was not granted")],
            ));
        };
        Ok(Self {
            granted: node,
            mask,
            peripheral,
            memory: memory_grant,
        })
    }

    /// Map the node's register window.
    ///
    /// # Errors
    ///
    /// The exit code to end with when the window will not map.
    pub fn map(&self) -> Result<RegisterWindow, i32> {
        self.granted.map()
    }

    /// Hold every carve within `reach`, what the engines' address registers
    /// span.
    ///
    /// # Errors
    ///
    /// The exit code to end with when the carves cannot be held there.
    pub fn narrow(&self, reach: DmaReach) -> Result<(), i32> {
        self.granted.host().narrow_dma_reach(reach).map_err(|_| {
            end(
                DriverExit::BringupFailed,
                &[step("dma: chains cannot be held within the engines' reach")],
            )
        })
    }

    /// The carves an engine builds its chains from.
    #[must_use]
    pub fn store(&self) -> &dyn DmaHost {
        self.granted.host()
    }
}

/// Serve `engine`, built over `node`'s register window and carves with every
/// carve held within `reach`, for the life of the driver process.
///
/// Binds the line of every channel the tree leaves this system, resets
/// those channels, declares the device quiesced, and only then binds the
/// node's endpoint and serves it. Never returns on the success path; a
/// refusal ends the driver with its reason logged.
pub fn serve<E: DmaEngine>(node: &Node, engine: E, reach: DmaReach) -> i32 {
    let Ok(instance) = tairix_rt::self_origin().map(|origin| origin.proc_id()) else {
        return end(
            DriverExit::BringupFailed,
            &[step("dma: this process's instance is unknown")],
        );
    };
    let (lines, line_count, usable) =
        bind_lines(node.granted.resources(), node.mask, engine.channel_count());
    if usable == 0 {
        return end(
            DriverExit::NoResources,
            &[step("dma: no channel is both unmasked and interruptible")],
        );
    }
    let endpoint = node.granted.duty().endpoint();
    let kernel = Kernel {
        endpoint,
        memory: node.memory,
        reach,
        instance,
    };
    let Some(mut controller) = Controller::new(
        engine,
        RtSupplier::new(endpoint),
        kernel,
        endpoint,
        (node.mask, usable),
        node.peripheral,
    ) else {
        return end(
            DriverExit::BringupFailed,
            &[step(
                "dma: a channel would not take its reset; its memory stays quarantined",
            )],
        );
    };
    node.granted.host().device_quiesced();

    let lines = &lines[..line_count];
    let mut wakes = [DeviceWake::Irq(0); HW_NODE_MAX_RESOURCES];
    for (wake, line) in wakes.iter_mut().zip(lines) {
        *wake = DeviceWake::Irq(line.handle);
    }
    let mut request = [0u8; DMA_ENGINE_MAX_REQUEST];
    // Each owned channel may hold a posted wait and one call beside it.
    let capacity = 2 * controller.usable().count_ones() as usize;
    let service = Service::bind(
        Endpoint {
            id: endpoint,
            request: &mut request,
            max_reply: DMA_ENGINE_MAX_REPLY,
            capacity,
        },
        &wakes[..line_count],
    );
    let ended = match service {
        Ok(service) => {
            log_ready(controller.usable());
            service.run(&mut Serving {
                controller: &mut controller,
                lines,
            })
        }
        Err(ended) => ended,
    };
    ended.end()
}

/// The controller as the shared loop drives it: each bound line serves the
/// channels it was bound for.
struct Serving<'c, E: DmaEngine> {
    controller: &'c mut Controller<E, RtSupplier, Kernel>,
    lines: &'c [Line],
}

impl<E: DmaEngine> Supplier for Serving<'_, E> {
    fn serve(&mut self, ticket: u64, frame: &[u8]) {
        self.controller.serve(ticket, frame);
    }

    fn peer_exited(&mut self, peer: ProcId) {
        self.controller.peer_exited(peer);
    }

    fn woke(&mut self, index: usize) -> Result<(), DriverError> {
        if let Some(line) = self.lines.get(index) {
            self.controller.interrupt(line.channels);
        }
        Ok(())
    }
}

/// Bind, once each, the lines of the channels in `mask` below `count`,
/// answering the bound lines, how many, and the channels they serve.
fn bind_lines<'r>(
    resources: impl Iterator<Item = &'r HwResource>,
    mask: u64,
    count: u8,
) -> ([Line; HW_NODE_MAX_RESOURCES], usize, u64) {
    let mut wanted = [(0u32, 0u64); HW_NODE_MAX_RESOURCES];
    let mut distinct = 0;
    for resource in resources.filter(|r| r.kind() == Some(HwResourceKind::Irq)) {
        // The binding lists one interrupt per channel, in channel order.
        let (Some(channel), Ok(line)) = (
            resource.interrupt_position(),
            u32::try_from(resource.base()),
        ) else {
            continue;
        };
        if channel >= u32::from(count) || mask & 1 << channel == 0 {
            continue;
        }
        match wanted[..distinct].iter_mut().find(|(id, _)| *id == line) {
            Some((_, channels)) => *channels |= 1 << channel,
            None if distinct < HW_NODE_MAX_RESOURCES => {
                wanted[distinct] = (line, 1 << channel);
                distinct += 1;
            }
            None => {}
        }
    }
    let mut lines = [Line {
        handle: 0,
        channels: 0,
    }; HW_NODE_MAX_RESOURCES];
    let mut bound = 0;
    let mut usable = 0;
    for &(line, channels) in &wanted[..distinct] {
        let Ok(handle) = u64::try_from(tairix_rt::irq_bind(line)) else {
            log_line_unbound(line);
            continue;
        };
        lines[bound] = Line { handle, channels };
        bound += 1;
        usable |= channels;
    }
    (lines, bound, usable)
}

/// The kernel, as the endpoint sees it.
struct Kernel {
    endpoint: u64,
    memory: u64,
    reach: DmaReach,
    instance: ProcId,
}

impl ControllerHost for Kernel {
    fn carve(&mut self, bytes: u32) -> Result<Buffer, Errno> {
        let len = usize::try_from(bytes).map_err(|_| Errno::LengthOutOfRange)?;
        let (mut region, mut bus) = (0, 0);
        let base = Errno::syscall_result(tairix_rt::shm_create_dma(
            self.memory,
            len,
            self.reach,
            &mut region,
            &mut bus,
        ))?;
        Ok(Buffer {
            region,
            base,
            len,
            bus,
        })
    }

    fn release(&mut self, buffer: &Buffer) {
        // A mapping the kernel will not drop is held until this process
        // ends, when the node's quarantine takes it.
        let _ = tairix_rt::shm_unmap(buffer.base, buffer.len);
    }

    fn grant(&mut self, buffer: &Buffer, ticket: u64) -> Result<u64, Errno> {
        Errno::syscall_result(tairix_rt::shm_grant_peer(
            buffer.region,
            self.endpoint,
            ticket,
        ))
    }

    fn now(&self) -> Duration64 {
        Duration64::from_nanos(tairix_rt::clock_get())
    }

    fn instance(&self) -> ProcId {
        self.instance
    }

    fn record(&mut self, record: Record) {
        log_record(&record);
    }
}

fn op_name(op: Option<DmaEngineOp>) -> &'static str {
    match op {
        None => "frame",
        Some(DmaEngineOp::Open) => "open",
        Some(DmaEngineOp::Prepare) => "prepare",
        Some(DmaEngineOp::Start) => "start",
        Some(DmaEngineOp::Stop) => "stop",
        Some(DmaEngineOp::Position) => "position",
        Some(DmaEngineOp::Close) => "close",
        Some(DmaEngineOp::Wait) => "wait",
    }
}

fn log_record(record: &Record) {
    let mut hex = [0u8; PROC_ID_HEX_LEN];
    match *record {
        Record::Claimed { channel, owner } => log_event(
            Level::Info,
            EVENT_CLAIMED,
            "dma: channel claimed",
            &[
                channel_field(channel),
                instance_field("owner", owner, &mut hex),
            ],
        ),
        Record::Reclaimed { channel, from } => log_event(
            Level::Warn,
            EVENT_RECLAIMED,
            "dma: channel reclaimed from an ended instance",
            &[
                channel_field(channel),
                instance_field("from", from, &mut hex),
            ],
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
                    "dma: request refused",
                    &[op, reason, instance_field("caller", caller, &mut hex)],
                ),
                None => log_event(
                    Level::Warn,
                    EVENT_REFUSED,
                    "dma: request refused",
                    &[op, reason],
                ),
            }
        }
        Record::Faulted { channel, bits } => log_event(
            Level::Warn,
            EVENT_FAULTED,
            "dma: channel faulted",
            &[
                channel_field(channel),
                Field {
                    key: "errors",
                    value: FieldValue::UnsignedInt(u64::from(bits.get())),
                },
            ],
        ),
        Record::Lost { channel } => log_event(
            Level::Warn,
            EVENT_LOST,
            "dma: channel position lost",
            &[channel_field(channel)],
        ),
        Record::Abandoned { channel } => log_event(
            Level::Info,
            EVENT_ABANDONED,
            "dma: channel released with its ended owner",
            &[channel_field(channel)],
        ),
        Record::Undrained { channel } => log_event(
            Level::Warn,
            EVENT_UNDRAINED,
            "dma: channel reset before its writes drained",
            &[channel_field(channel)],
        ),
        Record::Unreset { channel } => log_event(
            Level::Error,
            EVENT_UNRESET,
            "dma: channel would not take its reset; withdrawn, its memory kept",
            &[channel_field(channel)],
        ),
    }
}

fn channel_field(channel: u8) -> Field<'static> {
    Field {
        key: "channel",
        value: FieldValue::UnsignedInt(u64::from(channel)),
    }
}

fn log_ready(usable: u64) {
    let mut hex = [0u8; 16];
    log_event(
        Level::Info,
        EVENT_READY,
        "dma: controller serving",
        &crate::ready_fields(usable, &mut hex),
    );
}

fn log_line_unbound(line: u32) {
    log_event(
        Level::Warn,
        EVENT_LINE_UNBOUND,
        "dma: an interrupt line would not bind; its channels go unserved",
        &[Field {
            key: "line",
            value: FieldValue::UnsignedInt(u64::from(line)),
        }],
    );
}
