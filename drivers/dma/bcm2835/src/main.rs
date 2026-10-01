//! The `Run` binary of the legacy DMA engine driver, autoloaded into user
//! space by `devmgr` for a discovered `brcm,bcm2835-dma` node.
//!
//! It maps the node's register window, binds the interrupt line of every
//! channel the tree leaves this system, resets those channels, declares the
//! device quiesced, and only then binds the node's endpoint under its
//! `DmaController` duty and serves it. On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::dma::DmaHost;
    use tairix_abi::driver::dmaengine::{
        DmaControllerDuty, DmaEngine, DmaEngineOp, DMA_ENGINE_MAX_REPLY, DMA_ENGINE_MAX_REQUEST,
    };
    use tairix_abi::driver::sole_register_window;
    use tairix_abi::hwtree::{HwResource, HwResourceKind};
    use tairix_abi::ipc::IPC_CALL_CAPACITY_MAX;
    use tairix_abi::time::Duration64;
    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind, WAITSET_TIMEOUT_NONE};
    use tairix_abi::{
        CapabilityId, Errno, MmioMapper, ProcId, HW_NODE_MAX_RESOURCES, PROC_ID_HEX_LEN,
    };
    use tairix_caps::CapabilitySet;
    use tairix_drv_dma_bcm2835::controller::{Buffer, Controller, ControllerHost, Record};
    use tairix_drv_dma_bcm2835::engine::Bcm2835Dma;
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_log::{log, Event, EventId, Field, FieldValue, Level};
    use tairix_rt::LogSink;

    const EXIT_NO_HOST: i32 = 90;
    const EXIT_NO_RESOURCES: i32 = 91;
    const EXIT_BRINGUP_FAILED: i32 = 92;
    const EXIT_NO_CHANNELS: i32 = 93;
    const EXIT_SERVE_FAILED: i32 = 95;

    const EVENT_READY: EventId = EventId(24_200);
    const EVENT_FAILED: EventId = EventId(24_201);
    const EVENT_CLAIMED: EventId = EventId(24_202);
    const EVENT_RECLAIMED: EventId = EventId(24_203);
    const EVENT_REFUSED: EventId = EventId(24_204);
    const EVENT_FAULTED: EventId = EventId(24_205);
    const EVENT_LOST: EventId = EventId(24_206);
    const EVENT_ABANDONED: EventId = EventId(24_207);
    const EVENT_UNDRAINED: EventId = EventId(24_208);
    const EVENT_LINE_UNBOUND: EventId = EventId(24_209);
    const EVENT_UNRESET: EventId = EventId(24_210);

    const ENDPOINT_TOKEN: u64 = 0;
    const PEER_EXIT_TOKEN: u64 = 1;
    /// Token of the first interrupt line; line `k` is `LINE_TOKEN + k`.
    const LINE_TOKEN: u64 = 2;

    /// One bound interrupt line and the channels it serves.
    #[derive(Copy, Clone)]
    struct Line {
        handle: u64,
        channels: u64,
    }

    fn driver_caps() -> CapabilitySet {
        let mut caps = CapabilitySet::empty();
        for cap in tairix_drv_dma_bcm2835::REQUIRED_CAPABILITIES {
            caps.insert(*cap);
        }
        caps
    }

    /// Record why the driver is ending, and answer the code it ends with.
    fn fail(code: i32, reason: &'static str) -> i32 {
        log(
            &LogSink,
            &Event {
                level: Level::Error,
                id: EVENT_FAILED,
                message: reason,
                fields: &[],
            },
        );
        code
    }

    fn main() -> i32 {
        let Ok(mut host) = RtDriverHost::from_grants_query(driver_caps(), RtGrantSyscalls, None)
        else {
            return fail(EXIT_NO_HOST, "dma: the node's grants could not be read");
        };
        let mut granted = [None; HW_NODE_MAX_RESOURCES];
        for (slot, resource) in granted.iter_mut().zip(host.resources()) {
            *slot = Some(*resource);
        }
        let resources = || granted.iter().flatten();
        let Ok((base, len)) = sole_register_window(resources()) else {
            return fail(
                EXIT_NO_RESOURCES,
                "dma: the node names no single register window",
            );
        };
        let Some(duty) = resources().find_map(|r| r.dma_controller_duty().ok()) else {
            return fail(
                EXIT_NO_RESOURCES,
                "dma: the node carries no controller duty",
            );
        };
        // The binding makes the mask mandatory; the firmware owns what it
        // leaves out.
        let Some(mask) = duty.channels() else {
            return fail(EXIT_NO_RESOURCES, "dma: the tree states no channel mask");
        };
        let mut windows = [HwResource::dma(0, 0); HW_NODE_MAX_RESOURCES];
        let mut window_count = 0;
        for window in resources().filter(|r| r.is_translated_dma_window()) {
            windows[window_count] = *window;
            window_count += 1;
        }
        let windows = &windows[..window_count];
        // The engines reach peripherals through the window covering their
        // own registers, and memory through the others.
        let covers_registers =
            |window: &HwResource| window.dma_bus_address(base, len as u64).is_some();
        let peripheral_window = windows
            .iter()
            .copied()
            .find(|window| covers_registers(window));
        let Some(memory) = windows.iter().find(|window| !covers_registers(window)) else {
            return fail(EXIT_NO_RESOURCES, "dma: the node reaches no memory");
        };
        let (Some(memory_grant), Ok(())) =
            (host.grant_handle(memory), host.select_dma_window(memory))
        else {
            return fail(EXIT_NO_RESOURCES, "dma: the memory window was not granted");
        };
        let Ok(instance) = tairix_rt::self_origin().map(|origin| origin.proc_id()) else {
            return fail(
                EXIT_BRINGUP_FAILED,
                "dma: this process's instance is unknown",
            );
        };
        let Ok(regs) = host.map_window(base, len) else {
            return fail(
                EXIT_BRINGUP_FAILED,
                "dma: the register window would not map",
            );
        };
        let store: &dyn DmaHost = &host;
        let Ok(engine) = Bcm2835Dma::new(&regs, &store) else {
            return fail(
                EXIT_BRINGUP_FAILED,
                "dma: the register window is not whole channels",
            );
        };

        let (lines, line_count, usable) = bind_lines(resources(), mask, engine.channel_count());
        if usable == 0 {
            return fail(
                EXIT_NO_CHANNELS,
                "dma: no channel is both unmasked and interruptible",
            );
        }
        let kernel = Kernel {
            endpoint: duty.endpoint(),
            memory: memory_grant,
            instance,
        };
        let Some(mut controller) = Controller::new(
            engine,
            kernel,
            duty.endpoint(),
            (mask, usable),
            peripheral_window,
        ) else {
            return fail(
                EXIT_BRINGUP_FAILED,
                "dma: a channel would not take its reset; its memory stays quarantined",
            );
        };
        host.device_quiesced();

        let Some(set) = serve_set(&duty, controller.usable(), &lines[..line_count]) else {
            return fail(
                EXIT_SERVE_FAILED,
                "dma: the endpoint or its wait set could not be made",
            );
        };
        log_ready(controller.usable());
        serve(&mut controller, set, duty.endpoint(), &lines[..line_count])
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

    /// Bind the node's endpoint under its duty and gather it, peer exits and
    /// every bound line into one wait set.
    fn serve_set(duty: &DmaControllerDuty, usable: u64, lines: &[Line]) -> Option<u64> {
        // Only a caller whose grant covers this endpoint gets in: a request
        // line on this controller.
        let mut send_caps = CapabilitySet::empty();
        send_caps.insert(CapabilityId::IPC_ENDPOINT);
        // Each owned channel may hold a posted wait and one call beside it.
        let capacity = (2 * usable.count_ones() as usize).clamp(1, IPC_CALL_CAPACITY_MAX);
        if tairix_rt::call_create(
            duty.endpoint(),
            &send_caps,
            &CapabilitySet::empty(),
            DMA_ENGINE_MAX_REQUEST,
            DMA_ENGINE_MAX_REPLY,
            capacity,
        ) != 0
        {
            return None;
        }
        let set = u64::try_from(tairix_rt::waitset_create()).ok()?;
        let add =
            |kind, id, token| tairix_rt::waitset_ctl(set, WaitSetOp::Add, kind, id, token) == 0;
        if !add(WaitSourceKind::Endpoint, duty.endpoint(), ENDPOINT_TOKEN)
            || !add(WaitSourceKind::PeerExit, 0, PEER_EXIT_TOKEN)
        {
            return None;
        }
        for (token, line) in (LINE_TOKEN..).zip(lines) {
            if !add(WaitSourceKind::Irq, line.handle, token) {
                return None;
            }
        }
        Some(set)
    }

    fn serve<E: DmaEngine>(
        controller: &mut Controller<E, Kernel>,
        set: u64,
        endpoint: u64,
        lines: &[Line],
    ) -> i32 {
        let mut request = [0u8; DMA_ENGINE_MAX_REQUEST];
        loop {
            let mut token = 0u64;
            let woke = tairix_rt::waitset_wait(set, WAITSET_TIMEOUT_NONE, &mut token);
            if woke < 0 {
                return fail(EXIT_SERVE_FAILED, "dma: the serve wait set faulted");
            }
            if woke != 0 {
                continue;
            }
            match token {
                ENDPOINT_TOKEN => match tairix_rt::call_recv_ready(endpoint, &mut request) {
                    Ok(Some(call)) => controller.serve(call.ticket, &request[..call.len]),
                    Ok(None) => {}
                    Err(_) => return fail(EXIT_SERVE_FAILED, "dma: the endpoint was lost"),
                },
                PEER_EXIT_TOKEN => {
                    while let Ok(peer) = tairix_rt::peer_exit_take() {
                        controller.peer_exited(peer);
                    }
                }
                token => {
                    let line = token
                        .checked_sub(LINE_TOKEN)
                        .and_then(|index| usize::try_from(index).ok())
                        .and_then(|index| lines.get(index));
                    if let Some(line) = line {
                        controller.interrupt(line.channels);
                    }
                }
            }
        }
    }

    /// The kernel, as the endpoint sees it.
    struct Kernel {
        endpoint: u64,
        memory: u64,
        instance: ProcId,
    }

    fn status(ret: i64) -> Result<u64, Errno> {
        u64::try_from(ret).map_err(|_| Errno::from_syscall(ret))
    }

    impl ControllerHost for Kernel {
        fn caller(&self, ticket: u64) -> Result<ProcId, Errno> {
            tairix_rt::peer_origin(self.endpoint, ticket).map(|origin| origin.proc_id())
        }

        fn caller_holds(&self, ticket: u64, record: &HwResource) -> Result<bool, Errno> {
            match status(tairix_rt::call_peer_holds(self.endpoint, ticket, record)) {
                Ok(_) => Ok(true),
                Err(Errno::PermissionDenied) => Ok(false),
                Err(reason) => Err(reason),
            }
        }

        fn carve(&mut self, bytes: u32) -> Result<Buffer, Errno> {
            let len = usize::try_from(bytes).map_err(|_| Errno::LengthOutOfRange)?;
            let (mut region, mut bus) = (0, 0);
            let base = status(tairix_rt::shm_create_dma(
                self.memory,
                len,
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
            status(tairix_rt::shm_grant_peer(
                buffer.region,
                self.endpoint,
                ticket,
            ))
        }

        fn reply(&mut self, ticket: u64, frame: &[u8]) -> Result<(), Errno> {
            status(tairix_rt::call_reply(self.endpoint, ticket, frame)).map(|_| ())
        }

        fn watch(&mut self, peer: ProcId) -> Result<(), Errno> {
            tairix_rt::peer_watch(peer)
        }

        fn unwatch(&mut self, peer: ProcId) {
            // A watch that already fired has nothing left to remove.
            let _ = tairix_rt::peer_unwatch(peer);
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
            Record::Claimed { channel, owner } => emit(
                Level::Info,
                EVENT_CLAIMED,
                "dma: channel claimed",
                &[
                    channel_field(channel),
                    instance_field("owner", owner, &mut hex),
                ],
            ),
            Record::Reclaimed { channel, from } => emit(
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
                    Some(caller) => emit(
                        Level::Warn,
                        EVENT_REFUSED,
                        "dma: request refused",
                        &[op, reason, instance_field("caller", caller, &mut hex)],
                    ),
                    None => emit(
                        Level::Warn,
                        EVENT_REFUSED,
                        "dma: request refused",
                        &[op, reason],
                    ),
                }
            }
            Record::Faulted { channel, bits } => emit(
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
            Record::Lost { channel } => emit(
                Level::Warn,
                EVENT_LOST,
                "dma: channel position lost",
                &[channel_field(channel)],
            ),
            Record::Abandoned { channel } => emit(
                Level::Info,
                EVENT_ABANDONED,
                "dma: channel released with its ended owner",
                &[channel_field(channel)],
            ),
            Record::Undrained { channel } => emit(
                Level::Warn,
                EVENT_UNDRAINED,
                "dma: channel reset before its writes drained",
                &[channel_field(channel)],
            ),
            Record::Unreset { channel } => emit(
                Level::Error,
                EVENT_UNRESET,
                "dma: channel would not take its reset; withdrawn, its memory kept",
                &[channel_field(channel)],
            ),
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

    fn channel_field(channel: u8) -> Field<'static> {
        Field {
            key: "channel",
            value: FieldValue::UnsignedInt(u64::from(channel)),
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

    fn log_ready(channels: u64) {
        log(
            &LogSink,
            &Event {
                level: Level::Info,
                id: EVENT_READY,
                message: "dma: controller serving",
                fields: &[Field {
                    key: "channels",
                    value: FieldValue::UnsignedInt(channels),
                }],
            },
        );
    }

    fn log_line_unbound(line: u32) {
        log(
            &LogSink,
            &Event {
                level: Level::Warn,
                id: EVENT_LINE_UNBOUND,
                message: "dma: an interrupt line would not bind; its channels go unserved",
                fields: &[Field {
                    key: "line",
                    value: FieldValue::UnsignedInt(u64::from(line)),
                }],
            },
        );
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
