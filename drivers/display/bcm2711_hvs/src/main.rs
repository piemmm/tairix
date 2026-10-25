//! The `Run` program of the BCM2711 display engine: autoloaded by `devmgr`
//! against the HVS node once the encoders and the core clock it links to
//! serve, it hosts the kernel console on memory the kernel owns, tells the
//! firmware the display is the ARM's, takes the pipes over and serves the
//! seat's display for the life of the process: its presenter's buffers
//! carved under the node's DMA window, its latches heard on the HVS's
//! interrupt.
//!
//! On the host it is an inert stub so the workspace build, clippy and fmt
//! still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::display::DisplayFormat;
    use tairix_abi::driver::mailbox::MailboxChannel;
    use tairix_abi::driver::{named_register_window, DeviceWake, DmaReach};
    use tairix_abi::hwlink::{LinkRequest, LinkRole};
    use tairix_abi::hwtree::HwResource;
    use tairix_abi::hwtree::HwResourceKind;
    use tairix_abi::MmioMapper;
    use tairix_abi::{DriverError, Errno, HwProperty, HW_NODE_MAX_RESOURCES};
    use tairix_display::service::serve;
    use tairix_display::RtDeviceMemory;
    use tairix_drv_display_bcm2711_hvs::{
        Engine, Parts, Record, Surface, HVS_WINDOW, OUTPUTS_MAX, REQUIRED_CAPABILITIES, VALVES,
    };
    use tairix_drvrt::exit::{end, step, DriverExit};
    use tairix_drvrt::serve::log_event;
    use tairix_drvrt::{Recorder, RtDriverHost, RtGrantSyscalls};
    use tairix_inline::ArrayVec;
    use tairix_linkclient::{ClockClient, EncoderClient, RtLinkCall};
    use tairix_log::{EventId, Field, FieldValue, Level};
    use tairix_rt::ClockDelay;
    use tairix_vcmailbox::{
        decode_display_done_response, decode_display_size_response, encode_display_done,
        encode_display_size_query, DisplaySize, MailboxError, Unconfirmed,
    };

    const TOOK_OVER: EventId = EventId(24_900);
    const OUTPUT_UP: EventId = EventId(24_901);
    const OUTPUT_DOWN: EventId = EventId(24_902);
    const OUTPUT_FAILED: EventId = EventId(24_903);
    const ENCODER_GONE: EventId = EventId(24_904);
    const STOP_OVERRAN: EventId = EventId(24_905);
    const CLOCK_REFUSED: EventId = EventId(24_906);
    const FRAME_ENDS_UNHEARD: EventId = EventId(24_907);
    const FIRMWARE_RELEASED: EventId = EventId(24_908);
    const FIRMWARE_UNCONFIRMED: EventId = EventId(24_909);

    /// The share of the node's DMA window one presenter's buffers may hold.
    const BUDGET_SHARE: u64 = 8;

    /// The system log, as the engine records to it.
    struct Log;

    impl Recorder<Record> for Log {
        fn record(&mut self, record: Record) {
            let name = |key, value| Field {
                key,
                value: FieldValue::Bytes(value),
            };
            let error = |reason: DriverError| Field {
                key: "reason",
                value: FieldValue::Error(reason.as_errno()),
            };
            let number = |key, value: u64| Field {
                key,
                value: FieldValue::UnsignedInt(value),
            };
            match record {
                Record::TookOver { valve } => log_event(
                    Level::Info,
                    TOOK_OVER,
                    "display: the firmware's pipe brought down",
                    &[name("valve", valve)],
                ),
                Record::Up {
                    encoder,
                    width,
                    height,
                    refresh_mhz,
                } => log_event(
                    Level::Info,
                    OUTPUT_UP,
                    "display: output up",
                    &[
                        name("encoder", encoder),
                        number("width", u64::from(width)),
                        number("height", u64::from(height)),
                        number("refresh_mhz", u64::from(refresh_mhz)),
                    ],
                ),
                Record::Down { encoder } => log_event(
                    Level::Info,
                    OUTPUT_DOWN,
                    "display: output down",
                    &[name("encoder", encoder)],
                ),
                Record::Failed { encoder, reason } => log_event(
                    Level::Warn,
                    OUTPUT_FAILED,
                    "display: output left dark until its connector moves",
                    &[name("encoder", encoder), error(reason)],
                ),
                Record::EncoderGone { encoder } => log_event(
                    Level::Warn,
                    ENCODER_GONE,
                    "display: the encoder's driver ended",
                    &[name("encoder", encoder)],
                ),
                Record::StopOverran { valve } => log_event(
                    Level::Warn,
                    STOP_OVERRAN,
                    "display: a pixel valve outran its budget stopping",
                    &[name("valve", valve)],
                ),
                Record::ClockRefused { hz, reason } => log_event(
                    Level::Warn,
                    CLOCK_REFUSED,
                    "display: the core clock refused the floor the outputs need",
                    &[number("hz", hz), error(reason)],
                ),
            }
        }
    }

    /// The size of the screen the firmware lit, or `None` when it lit none.
    ///
    /// # Errors
    ///
    /// The exit code to end with, its reason recorded.
    fn firmware_screen(host: &RtDriverHost<RtGrantSyscalls>) -> Result<Option<DisplaySize>, i32> {
        let mut query = encode_display_size_query();
        host.exchange(&mut query)
            .and_then(|()| {
                decode_display_size_response(&query).map_err(MailboxError::as_driver_error)
            })
            .map(|size| Some(size).filter(DisplaySize::is_attached))
            .map_err(DriverError::as_errno)
            .map_err(|errno| {
                end(
                    DriverExit::BringupFailed,
                    &[
                        step("display: the firmware would not say what screen it lit"),
                        Field {
                            key: "reason",
                            value: FieldValue::Error(errno),
                        },
                    ],
                )
            })
    }

    /// Host the kernel console on memory the kernel carves under the node's
    /// DMA window from the bottom of memory, at the mode of the screen the
    /// firmware lit, and answer where the HVS reads it; `None` where the node
    /// does not host the console or the firmware lit no screen.
    ///
    /// # Errors
    ///
    /// The exit code to end with, its reason recorded. The firmware has not
    /// been told anything, so it keeps the display and the console on it.
    fn adopt_console(
        host: &RtDriverHost<RtGrantSyscalls>,
        granted: &[HwResource],
    ) -> Result<Option<Surface>, i32> {
        if host.property(HwProperty::ConsoleHost).is_none() {
            return Ok(None);
        }
        let Some(screen) = firmware_screen(host)? else {
            return Ok(None);
        };
        let refused = |what: &'static str, reason: Errno| {
            end(
                DriverExit::BringupFailed,
                &[
                    step(what),
                    Field {
                        key: "reason",
                        value: FieldValue::Error(reason),
                    },
                ],
            )
        };
        let window = granted
            .iter()
            .find(|resource| resource.dma_from_bottom_of_memory())
            .and_then(|window| host.grant_handle(window))
            .ok_or_else(|| {
                refused(
                    "display: the node names no DMA window from the bottom of memory",
                    Errno::NotFound,
                )
            })?;
        let hosted = tairix_rt::console_adopt(
            window,
            screen.width_px,
            screen.height_px,
            DmaReach::of::<32>(),
        )
        .map_err(|errno| refused("display: the console could not be hosted", errno))?;
        let bytes = u64::from(hosted.stride_bytes) * u64::from(screen.height_px);
        let bus = hosted
            .device_addr
            .checked_add(bytes)
            .filter(|&end| end <= 1 << 32)
            .and_then(|_| u32::try_from(hosted.device_addr).ok())
            .ok_or_else(|| {
                refused(
                    "display: the console's memory lies outside the engine's reach",
                    Errno::OutOfRange,
                )
            })?;
        Ok(Some(Surface {
            bus,
            stride: hosted.stride_bytes,
            width: screen.width_px,
            height: screen.height_px,
            format: DisplayFormat::Bgra8888,
        }))
    }

    /// Tell the firmware the display is the ARM's, and record its answer.
    ///
    /// The engine takes the display over whatever the answer: nothing of the
    /// ARM's is left in the firmware's memory, and
    /// a firmware that kept the display would only fight the engine where a
    /// refusal would leave the screen with no driver at all.
    fn release_firmware(host: &RtDriverHost<RtGrantSyscalls>) {
        let mut notice = encode_display_done();
        match host
            .exchange(&mut notice)
            .map(|()| decode_display_done_response(&notice))
        {
            Ok(Ok(())) => log_event(
                Level::Info,
                FIRMWARE_RELEASED,
                "display: the firmware accepted the hand-over",
                &[],
            ),
            Ok(Err(Unconfirmed { header, tag })) => log_event(
                Level::Warn,
                FIRMWARE_UNCONFIRMED,
                "display: the firmware refused the hand-over; it is taken over",
                &[
                    Field {
                        key: "header",
                        value: FieldValue::UnsignedInt(u64::from(header)),
                    },
                    Field {
                        key: "tag",
                        value: tag.map_or(FieldValue::Null, |code| {
                            FieldValue::UnsignedInt(u64::from(code))
                        }),
                    },
                ],
            ),
            Err(reason) => log_event(
                Level::Warn,
                FIRMWARE_UNCONFIRMED,
                "display: the firmware could not be told the display is the ARM's; it is taken over",
                &[Field {
                    key: "reason",
                    value: FieldValue::Error(reason.as_errno()),
                }],
            ),
        }
    }

    /// Where the presenter's buffers are carved: the node's DMA window, and
    /// the share of it one presenter may hold.
    fn carving(
        host: &RtDriverHost<RtGrantSyscalls>,
        granted: &[HwResource],
    ) -> Option<(RtDeviceMemory, usize)> {
        granted
            .iter()
            .filter(|resource| resource.kind() == Some(HwResourceKind::Dma))
            .find_map(|window| {
                host.grant_handle(window).map(|handle| {
                    (
                        RtDeviceMemory::new(handle, DmaReach::of::<32>()),
                        usize::try_from(window.length() / BUDGET_SHARE).unwrap_or(usize::MAX),
                    )
                })
            })
    }

    /// The HVS's interrupt, bound to wake the engine at a frame's end; `None`,
    /// recorded, where it cannot be. A present is still latched without it,
    /// and completed at the display service's deadline instead.
    fn frame_ends(granted: &[HwResource]) -> Option<DeviceWake> {
        let bound = granted
            .iter()
            .find(|resource| resource.kind() == Some(HwResourceKind::Irq))
            .and_then(|line| u32::try_from(line.base()).ok())
            .map(tairix_rt::irq_bind)
            .and_then(|handle| u64::try_from(handle).ok());
        if bound.is_none() {
            log_event(
                Level::Warn,
                FRAME_ENDS_UNHEARD,
                "display: the HVS's interrupt could not be bound; presents complete at the service's deadline",
                &[],
            );
        }
        bound.map(DeviceWake::Irq)
    }

    /// Program entry point; on success it never returns.
    fn main() -> i32 {
        let caps = REQUIRED_CAPABILITIES.iter().copied().collect();
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return end(
                DriverExit::NoHost,
                &[step(
                    "display: the driver host could not be built from the delivered grants",
                )],
            );
        };
        let mut granted = ArrayVec::<HwResource, HW_NODE_MAX_RESOURCES>::new();
        for resource in host.resources() {
            if granted.try_push(*resource).is_err() {
                break;
            }
        }
        let map = |name: &[u8]| {
            named_register_window(granted.iter(), name)
                .ok()
                .and_then(|(base, len)| host.map_window(base, len).ok())
        };
        let links = || {
            granted
                .iter()
                .filter_map(|resource| resource.link_request().ok())
        };
        let encoder = |name: &[u8]| {
            links()
                .find(|link: &LinkRequest| link.role() == LinkRole::Encoder && link.name() == name)
        };
        let (Some(hvs), Some(clock)) = (
            map(HVS_WINDOW),
            links().find(|link| link.role() == LinkRole::Clock),
        ) else {
            return end(
                DriverExit::NoResources,
                &[step("display: the node names no HVS window and core clock")],
            );
        };
        // Each output's encoder wakes the engine when its connector moves.
        let mut valves = ArrayVec::<_, OUTPUTS_MAX>::new();
        let mut wakes = ArrayVec::<DeviceWake, { OUTPUTS_MAX + 1 }>::new();
        for kind in VALVES {
            let (Some(window), Some(link)) = (map(kind.window), encoder(kind.encoder)) else {
                continue;
            };
            let client = EncoderClient::new(RtLinkCall::new(link.endpoint()), link);
            if valves.try_push((window, kind, client)).is_err()
                || wakes
                    .try_push(DeviceWake::CallReply(link.endpoint()))
                    .is_err()
            {
                break;
            }
        }
        let Some((alloc, budget)) = carving(&host, &granted) else {
            return end(
                DriverExit::NoResources,
                &[step(
                    "display: the node names no DMA window to carve buffers in",
                )],
            );
        };
        if let Some(wake) = frame_ends(&granted) {
            if wakes.try_push(wake).is_err() {
                return end(
                    DriverExit::NoResources,
                    &[step(
                        "display: the frame-end interrupt has no room to wake the engine",
                    )],
                );
            }
        }
        let console = match adopt_console(&host, &granted) {
            Ok(console) => console,
            Err(code) => return code,
        };
        release_firmware(&host);
        let parts = Parts {
            hvs,
            valves,
            clock: ClockClient::new(RtLinkCall::new(clock.endpoint()), clock),
            console,
            alloc,
            budget,
            delay: ClockDelay::new(),
            log: Log,
        };
        let Ok(mut engine) = Engine::open(parts) else {
            return end(
                DriverExit::BringupFailed,
                &[step("display: the HVS refused its set-up")],
            );
        };
        serve(&mut engine, &wakes)
    }

    tairix_rt::entry!(main);
}

// On the host the program's real entry is not compiled, so this inert `main`
// keeps the crate building under the host tooling.
#[cfg(not(freestanding))]
fn main() {}
