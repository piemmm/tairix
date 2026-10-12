//! The `Run` program of the BCM2711 display engine: autoloaded by `devmgr`
//! against the HVS node once the encoders and the core clock it links to
//! serve, it moves the boot console off the firmware's memory, tells the
//! firmware the display is the ARM's, takes the pipes over and serves the
//! seat's display for the life of the process.
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
    use tairix_abi::driver::{named_register_window, sole_framebuffer, DeviceWake};
    use tairix_abi::hwlink::{LinkRequest, LinkRole};
    use tairix_abi::hwtree::HwResource;
    use tairix_abi::MmioMapper;
    use tairix_abi::{DriverError, Errno, HwProperty, HW_NODE_MAX_RESOURCES};
    use tairix_display::service::{serve, EXIT_BRINGUP_FAILED, EXIT_NO_HOST, EXIT_NO_RESOURCES};
    use tairix_drv_display_bcm2711_hvs::{
        DmaFrames, Engine, Parts, Record, Surface, HVS_WINDOW, OUTPUTS_MAX, REQUIRED_CAPABILITIES,
        VALVES,
    };
    use tairix_drvrt::serve::{fail, log_event};
    use tairix_drvrt::{Recorder, RtDriverHost, RtGrantSyscalls};
    use tairix_inline::ArrayVec;
    use tairix_linkclient::{ClockClient, EncoderClient, RtLinkCall};
    use tairix_log::{EventId, Field, FieldValue, Level};
    use tairix_rt::ClockDelay;
    use tairix_vcmailbox::{decode_display_done_response, encode_display_done, Unconfirmed};

    const TOOK_OVER: EventId = EventId(24_900);
    const OUTPUT_UP: EventId = EventId(24_901);
    const OUTPUT_DOWN: EventId = EventId(24_902);
    const OUTPUT_FAILED: EventId = EventId(24_903);
    const ENCODER_GONE: EventId = EventId(24_904);
    const STOP_OVERRAN: EventId = EventId(24_905);
    const CLOCK_REFUSED: EventId = EventId(24_906);
    const BRINGUP_FAILED: EventId = EventId(24_907);
    const FIRMWARE_RELEASED: EventId = EventId(24_908);
    const FIRMWARE_UNCONFIRMED: EventId = EventId(24_909);

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

    /// The console's `surface`, at the address the HVS reads it through one
    /// of the node's DMA `windows`.
    fn console(surface: &HwResource, windows: &[HwResource]) -> Option<Surface> {
        let (phys, mode) = sole_framebuffer(core::slice::from_ref(surface)).ok()?;
        let len = u64::from(mode.stride_bytes) * u64::from(mode.height_px);
        let bus = windows
            .iter()
            .find_map(|resource| resource.dma_bus_address(phys, len))?;
        Some(Surface {
            bus: u32::try_from(bus).ok()?,
            stride: mode.stride_bytes,
            width: mode.width_px,
            height: mode.height_px,
            format: match mode.format {
                DisplayFormat::Bgra8888 => DisplayFormat::Bgra8888,
                _ => DisplayFormat::Rgba8888,
            },
        })
    }

    /// Move the boot console off the firmware's memory, where the node says it
    /// is the engine's to move, and answer where the HVS reads it.
    ///
    /// # Errors
    ///
    /// The exit code to end with, its reason recorded. The firmware has not
    /// been told anything, so it keeps the display and the console on it.
    fn relocate_console(
        host: &RtDriverHost<RtGrantSyscalls>,
        granted: &[HwResource],
    ) -> Result<Option<Surface>, i32> {
        if host.property(HwProperty::BootConsole).is_none() {
            return Ok(None);
        }
        let surface = tairix_rt::console_relocate().map_err(|errno: Errno| {
            log_event(
                Level::Error,
                BRINGUP_FAILED,
                "display: the console would not move off the firmware's memory",
                &[Field {
                    key: "reason",
                    value: FieldValue::Error(errno),
                }],
            );
            EXIT_BRINGUP_FAILED
        })?;
        console(&surface, granted).map(Some).ok_or_else(|| {
            fail(
                BRINGUP_FAILED,
                EXIT_BRINGUP_FAILED,
                "display: the console's new surface lies outside the engine's reach",
            )
        })
    }

    /// Tell the firmware the display is the ARM's, and record its answer.
    ///
    /// The engine takes the display over whatever the answer, as Linux's
    /// `vc4` does: nothing of the ARM's is left in the firmware's memory, and
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
                "display: the firmware gave the display up",
                &[],
            ),
            Ok(Err(Unconfirmed { header, tag })) => log_event(
                Level::Warn,
                FIRMWARE_UNCONFIRMED,
                "display: the firmware did not confirm giving the display up; it is taken over",
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

    /// Program entry point; on success it never returns.
    fn main() -> i32 {
        let caps = REQUIRED_CAPABILITIES.iter().copied().collect();
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return fail(
                BRINGUP_FAILED,
                EXIT_NO_HOST,
                "display: the driver host could not be built from the delivered grants",
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
            return fail(
                BRINGUP_FAILED,
                EXIT_NO_RESOURCES,
                "display: the node names no HVS window and core clock",
            );
        };
        // Each output's encoder wakes the engine when its connector moves.
        let mut valves = ArrayVec::<_, OUTPUTS_MAX>::new();
        let mut wakes = ArrayVec::<DeviceWake, OUTPUTS_MAX>::new();
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
        let console = match relocate_console(&host, &granted) {
            Ok(console) => console,
            Err(code) => return code,
        };
        release_firmware(&host);
        let parts = Parts {
            hvs,
            valves,
            clock: ClockClient::new(RtLinkCall::new(clock.endpoint()), clock),
            console,
            alloc: DmaFrames(&host),
            delay: ClockDelay::new(),
            log: Log,
        };
        let Ok(mut engine) = Engine::open(parts) else {
            return fail(
                BRINGUP_FAILED,
                EXIT_BRINGUP_FAILED,
                "display: the HVS refused its set-up",
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
