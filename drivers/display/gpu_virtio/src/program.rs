//! A `Run` program's bring-up: the node's grants made a driver host, its
//! interrupt bound, its transport mapped, and the engine opened over them,
//! then handed to what the program does with it. The bundle's own program
//! serves the seat's display; a vertical's drives the engine first.

use tairix_abi::driver::display::{DamageRect, HostedConsole};
use tairix_abi::driver::virtio::VirtioHost;
use tairix_abi::driver::virtio_pci::{virtio_pci_windows, VirtioPciWindows};
use tairix_abi::driver::{sole_register_window, DeviceWake, DmaReach};
use tairix_abi::gpu::{
    desk_endpoint_for, GPU_CHANNEL_ENDPOINTS, GPU_MAX_REPLY, GPU_MAX_REQUEST, GPU_NODE_COMPATIBLE,
};
use tairix_abi::hwtree::{HwDeviceClass, HwResource, HwResourceKind};
use tairix_abi::{CapabilityId, DriverError, Errno, HwProperty, MmioMapper};
use tairix_display::RtDeviceMemory;
use tairix_drvrt::exit::{end, error, step, DriverExit};
use tairix_drvrt::serve::{log_event, publish_channel, BoundChannel, DeviceChannel};
use tairix_drvrt::{Recorder, RtDriverHost, RtGrantSyscalls};
use tairix_gpu::{Device, RuntimeThreads, MAX_SERVE_WORKERS};
use tairix_inline::ArrayVec;
use tairix_log::{EventId, Field, FieldValue, Level};
use tairix_virtio::{MmioTransport, PciTransport, Transport};

use crate::wire::SHM_HOST_VISIBLE;
use crate::{
    ConsoleSource, NoVenus, Record, VirtioGpu, CONSOLE_SHOWN_MESSAGE, HOST_VISIBLE_MESSAGE,
    NO_VENUS_MESSAGE, REQUIRED_CAPABILITIES, VENUS_MESSAGE,
};

const CONSOLE_UNHOSTED: EventId = EventId(24_920);
const SHOW_REFUSED: EventId = EventId(24_921);
const CONSOLE_UNSHOWN: EventId = EventId(24_922);
const CONSOLE_SHOWN: EventId = EventId(24_923);
const HOST_VISIBLE_REGION: EventId = EventId(24_924);
const VENUS: EventId = EventId(24_925);
const NO_VENUS: EventId = EventId(24_926);
const GPU_SERVED: EventId = EventId(24_927);
const GPU_UNSERVED: EventId = EventId(24_928);
const GPU_ENDED: EventId = EventId(24_929);

/// Calls one GPU client may hold at once: its threads each waiting on one.
const GPU_CALL_DEPTH: usize = 8;

/// Rouses the display loop may hold unread: one is enough to bring it round.
const DESK_ROUSE_DEPTH: usize = 2;

/// The driver host a node's grants make.
pub type Host = RtDriverHost<RtGrantSyscalls>;

/// The engine a program drives over transport `T`.
pub type Engine<'h, T> = VirtioGpu<'h, T, RtDeviceMemory, KernelConsole, Log>;

/// What a program does with the device once it is up.
pub trait Drive {
    /// Drive `gpu`, whose grants `host` holds, waking on `wakes`, answering
    /// the status the program ends with.
    fn drive<'h, T: Transport>(
        self,
        gpu: Engine<'h, T>,
        host: &'h Host,
        wakes: &[DeviceWake],
    ) -> i32;
}

/// The system log, as the engine records to it.
pub struct Log;

impl Recorder<Record> for Log {
    fn record(&mut self, record: Record) {
        let (id, message, reason) = match record {
            Record::ConsoleShown { width, height } => {
                let number = |key, value: u32| Field {
                    key,
                    value: FieldValue::UnsignedInt(u64::from(value)),
                };
                log_event(
                    Level::Info,
                    CONSOLE_SHOWN,
                    CONSOLE_SHOWN_MESSAGE,
                    &[number("width", width), number("height", height)],
                );
                return;
            }
            Record::Venus { version } => {
                log_event(
                    Level::Info,
                    VENUS,
                    VENUS_MESSAGE,
                    &[Field {
                        key: "version",
                        value: FieldValue::UnsignedInt(u64::from(version)),
                    }],
                );
                return;
            }
            Record::NoVenus { why } => {
                let why_field = Field {
                    key: "why",
                    value: FieldValue::Str(why.name()),
                };
                match why {
                    NoVenus::Refused(reason) => log_event(
                        Level::Warn,
                        NO_VENUS,
                        NO_VENUS_MESSAGE,
                        &[
                            why_field,
                            Field {
                                key: "reason",
                                value: FieldValue::Error(reason.as_errno()),
                            },
                        ],
                    ),
                    _ => log_event(Level::Info, NO_VENUS, NO_VENUS_MESSAGE, &[why_field]),
                }
                return;
            }
            Record::ConsoleUnhosted { reason } => (
                CONSOLE_UNHOSTED,
                "display: the console could not be hosted at the output's mode",
                reason,
            ),
            Record::ShowRefused { reason } => (
                SHOW_REFUSED,
                "display: the device would not show what the seat shows",
                reason,
            ),
            Record::ConsoleUnshown { reason } => (
                CONSOLE_UNSHOWN,
                "display: the console's newest rows could not be shown",
                reason,
            ),
        };
        log_event(
            Level::Warn,
            id,
            message,
            &[Field {
                key: "reason",
                value: FieldValue::Error(reason.as_errno()),
            }],
        );
    }
}

/// The kernel console the node hosts, painted in memory the kernel carves
/// under the node's DMA grant.
pub struct KernelConsole {
    grant: u64,
}

impl ConsoleSource for KernelConsole {
    fn adopt(&mut self, width: u32, height: u32) -> Result<HostedConsole, DriverError> {
        tairix_rt::console_adopt(self.grant, width, height, DmaReach::FULL)
            .map_err(DriverError::from_errno)
    }

    fn relinquish(&mut self) -> Result<(), DriverError> {
        tairix_rt::console_relinquish().map_err(DriverError::from_errno)
    }

    fn damage(&mut self) -> Result<Option<DamageRect>, DriverError> {
        tairix_rt::console_damage().map_err(DriverError::from_errno)
    }
}

/// What the device is served with once its transport is up.
struct Serving {
    memory: RtDeviceMemory,
    console: Option<KernelConsole>,
    wakes: ArrayVec<DeviceWake, 2>,
    /// Bytes of the host-visible region the node holds.
    host_visible: Option<u64>,
}

/// The host-visible region the node was granted, which a host-visible blob
/// is mapped into.
#[must_use]
pub fn host_visible_region(host: &Host) -> Option<HwResource> {
    host.resources()
        .find(|resource| resource.memory_region_id() == Some(SHM_HOST_VISIBLE))
        .copied()
}

/// The driver host built from the node's grants, its interrupt line bound,
/// or the status the driver ends with.
fn host_with_line() -> Result<(Host, u64), i32> {
    let caps = REQUIRED_CAPABILITIES.iter().copied().collect();
    let host = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None).map_err(|err| {
        end(
            DriverExit::NoHost,
            &[
                step("display: the driver host could not be built from the delivered grants"),
                error(err),
            ],
        )
    })?;
    if host.irq_line().is_none() {
        return Err(end(
            DriverExit::NoResources,
            &[step(
                "display: the virtio GPU's node granted no interrupt line",
            )],
        ));
    }
    // Bound through the host, which caches the handle the command waits park
    // on: a second bind of the same line is refused.
    host.bind_irq().map_err(|err| {
        end(
            DriverExit::BringupFailed,
            &[
                step("display: the virtio GPU's interrupt line could not be bound"),
                error(err),
            ],
        )
    })?;
    let Some(irq) = host.irq_handle() else {
        return Err(end(
            DriverExit::BringupFailed,
            &[step(
                "display: the interrupt line bound but minted no handle",
            )],
        ));
    };
    Ok((host, irq))
}

/// Record the host-visible region the node was granted, answering its
/// bytes.
fn record_host_visible(host: &Host) -> Option<u64> {
    let region = host_visible_region(host)?;
    log_event(
        Level::Info,
        HOST_VISIBLE_REGION,
        HOST_VISIBLE_MESSAGE,
        &[Field {
            key: "bytes",
            value: FieldValue::UnsignedInt(region.length()),
        }],
    );
    Some(region.length())
}

/// Why a device's GPU is not served.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Unserved {
    /// The device takes no Venus work, as its bring-up recorded.
    NoVenus,
    /// Serving it was refused, as recorded.
    Refused {
        /// The step that was refused.
        why: &'static str,
        /// The refusal.
        reason: DriverError,
    },
}

/// Record that the device's GPU is not served, and why, answering the
/// refusal.
fn unserved(why: &'static str, reason: DriverError) -> Unserved {
    log_event(
        Level::Warn,
        GPU_UNSERVED,
        "display: the GPU is not served",
        &[
            Field {
                key: "why",
                value: FieldValue::Str(why),
            },
            Field {
                key: "reason",
                value: FieldValue::Error(reason.as_errno()),
            },
        ],
    );
    Unserved::Refused { why, reason }
}

/// Serve the device's Venus host as a GPU on its node's channel from a
/// thread of its own, answering the port the display loop must wake on for
/// the GPU's orders.
///
/// # Errors
///
/// Why the GPU is not served, recorded.
pub fn serve_gpu<'h, T: Transport>(
    gpu: &mut Engine<'h, T>,
    host: &'h Host,
) -> Result<u64, Unserved> {
    start_gpu(gpu, host, serve_on)
}

/// Bind the device's node's channel and, on a thread of its own, make its
/// Venus host a GPU and hand `then` the GPU, or why the host would not be
/// one, with the channel, answering the port the display loop must wake on
/// for the GPU's orders. The channel is taken down wherever it goes unserved.
///
/// # Errors
///
/// Why the GPU is not served, recorded: the kernel refuses the channel to a
/// node a platform's DMA translation unit does not translate, and the driver
/// then drives the display alone.
pub fn start_gpu<'h, T: Transport>(
    gpu: &mut Engine<'h, T>,
    host: &'h Host,
    then: impl FnOnce(Result<Device, DriverError>, BoundChannel) + Send + 'static,
) -> Result<u64, Unserved> {
    if gpu.venus().is_none() {
        return Err(Unserved::NoVenus);
    }
    let region = host_visible_region(host)
        .ok_or_else(|| unserved("no host-visible region", DriverError::NotFound))?;
    let channel = DeviceChannel {
        block: GPU_CHANNEL_ENDPOINTS,
        gate: CapabilityId::GPU,
        max_request: GPU_MAX_REQUEST,
        max_reply: GPU_MAX_REPLY,
        per_sender: GPU_CALL_DEPTH,
    };
    let refused_by = |why| move |errno| unserved(why, DriverError::from_errno(errno));
    let channel = channel.bind().map_err(refused_by("channel unbound"))?;
    let endpoint = channel.endpoint();
    let origin = tairix_rt::self_origin().map_err(|raw| {
        unserved(
            "no identity",
            DriverError::from_errno(Errno::from_syscall(raw)),
        )
    })?;
    let port = desk_endpoint_for(origin.pid());
    Errno::syscall_result(tairix_rt::port_bind(port, 1, DESK_ROUSE_DEPTH))
        .map_err(refused_by("desk port unbound"))?;
    let refused = |why: &'static str, reason: DriverError| {
        // Nothing will rouse the loop, so the port and its queue go back.
        let _ = tairix_rt::port_unbind(port);
        unserved(why, reason)
    };
    // The channel's server is this process: no other may rouse its loop.
    if let Err(errno) = tairix_rt::port_admit(port, endpoint) {
        return Err(refused("desk port open", DriverError::from_errno(errno)));
    }
    let venus = gpu
        .serve_venus(region, host, port)
        .map_err(|reason| refused("desk", reason))?;
    let started = tairix_rt::thread::Thread::spawn(move || {
        then(venus.device(&RuntimeThreads), channel);
    });
    if let Err(errno) = started {
        return Err(refused("no thread", DriverError::from_errno(errno)));
    }
    Ok(port)
}

/// Publish the GPU's channel and serve it there.
fn serve_on(made: Result<Device, DriverError>, channel: BoundChannel) {
    let endpoint = channel.endpoint();
    let device = match made {
        Ok(device) => device,
        Err(refused) => {
            unserved("host", refused);
            return;
        }
    };
    if let Err(errno) = publish_channel(
        HwDeviceClass::Accelerator,
        GPU_NODE_COMPATIBLE,
        endpoint,
        &[],
    ) {
        unserved("channel unpublished", DriverError::from_errno(errno));
        return;
    }
    log_event(Level::Info, GPU_SERVED, "display: the GPU is served", &[]);
    // Every worker spends a request waiting on the host, so the most the
    // runtime allows serve at once.
    let why = tairix_gpu::serve(&device, endpoint, &RuntimeThreads, MAX_SERVE_WORKERS);
    // Nothing serves it any more, so nobody may wait on it.
    drop(channel);
    log_event(
        Level::Error,
        GPU_ENDED,
        "display: the GPU's service ended",
        &[Field {
            key: "reason",
            value: FieldValue::Error(why),
        }],
    );
}

/// Bring the device up over its node's grants and have `drive` drive it,
/// answering the status the program ends with.
pub fn run(drive: impl Drive) -> i32 {
    let (host, irq) = match host_with_line() {
        Ok(bound) => bound,
        Err(status) => return status,
    };
    let host_visible = record_host_visible(&host);
    let Some(grant) = host
        .resources()
        .find(|resource| resource.kind() == Some(HwResourceKind::Dma))
        .and_then(|resource| host.grant_handle(resource))
    else {
        return end(
            DriverExit::NoResources,
            &[step(
                "display: the virtio GPU's node names no DMA grant to carve frames under",
            )],
        );
    };
    let hosts_console = host.property(HwProperty::ConsoleHost).is_some();
    let mut wakes = ArrayVec::new();
    let _ = wakes.try_push(DeviceWake::Irq(irq));
    if hosts_console {
        let _ = wakes.try_push(DeviceWake::ConsoleDamage);
    }
    let serving = Serving {
        memory: RtDeviceMemory::new(grant, DmaReach::FULL),
        console: hosts_console.then_some(KernelConsole { grant }),
        wakes,
        host_visible,
    };
    match virtio_pci_windows(host.resources()) {
        Ok(windows) => drive_pci(&host, &windows, serving, drive),
        // No role-tagged window at all: a single-aperture MMIO delivery.
        Err(DriverError::NotFound) => drive_mmio(&host, serving, drive),
        Err(err) => end(
            DriverExit::NoResources,
            &[
                step("display: the node's virtio-PCI windows are incomplete"),
                error(err),
            ],
        ),
    }
}

/// Drive the device over the virtio-PCI config `windows` the node granted.
fn drive_pci(host: &Host, windows: &VirtioPciWindows, serving: Serving, drive: impl Drive) -> i32 {
    match PciTransport::map(windows, host) {
        Ok(transport) => open_and_drive(transport, host, serving, drive),
        Err(err) => end(
            DriverExit::BringupFailed,
            &[
                step("display: the granted virtio-PCI config windows are no transport"),
                error(err),
            ],
        ),
    }
}

/// Drive the device over the node's one virtio-MMIO register window.
fn drive_mmio(host: &Host, serving: Serving, drive: impl Drive) -> i32 {
    let (base, len) = match sole_register_window(host.resources()) {
        Ok(window) => window,
        Err(err) => {
            return end(
                DriverExit::NoResources,
                &[
                    step("display: the virtio GPU's node granted no single register window"),
                    error(err),
                ],
            )
        }
    };
    let window = match host.map_window(base, len) {
        Ok(window) => window,
        Err(err) => {
            return end(
                DriverExit::BringupFailed,
                &[
                    step("display: the granted register window could not be mapped"),
                    error(err.as_driver_error()),
                ],
            )
        }
    };
    match MmioTransport::new(window) {
        Ok(transport) => open_and_drive(transport, host, serving, drive),
        Err(err) => end(
            DriverExit::BringupFailed,
            &[
                step("display: the mapped window is not a virtio-MMIO transport"),
                error(err.as_driver_error()),
            ],
        ),
    }
}

/// Bring the device up over `transport` and have `drive` drive it.
fn open_and_drive<T: Transport>(
    transport: T,
    host: &Host,
    serving: Serving,
    drive: impl Drive,
) -> i32 {
    let vhost: &dyn VirtioHost = host;
    match VirtioGpu::open(
        transport,
        vhost,
        serving.memory,
        serving.console,
        Log,
        serving.host_visible,
    ) {
        Ok(gpu) => drive.drive(gpu, host, &serving.wakes),
        Err(err) => end(
            DriverExit::BringupFailed,
            &[
                step("display: the virtio GPU refused its bring-up"),
                error(err),
            ],
        ),
    }
}
