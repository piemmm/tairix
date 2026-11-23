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
use tairix_abi::{CapabilityId, DriverError, HwProperty, MmioMapper};
use tairix_display::RtDeviceMemory;
use tairix_drvrt::exit::{end, error, step, DriverExit};
use tairix_drvrt::serve::{log_event, publish_channel, DeviceChannel};
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

/// Record that the device's GPU is not served, and why.
fn unserved(why: &'static str, reason: DriverError) {
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
}

/// Serve the device's Venus host as a GPU on its node's channel from a
/// thread of its own, answering the port the display loop must wake on for
/// the GPU's orders; [`None`] where the device takes no Venus work or the
/// GPU could not be served, its reason recorded.
pub fn serve_gpu<'h, T: Transport>(gpu: &mut Engine<'h, T>, host: &'h Host) -> Option<u64> {
    start_gpu(gpu, host, serve_on)
}

/// Bind the device's node's channel and, on a thread of its own, make its
/// Venus host a GPU and hand `then` the GPU, or why the host would not be
/// one, with the channel's endpoint, answering the port the display loop
/// must wake on for the GPU's orders; [`None`] where the device takes no
/// Venus work or the thread could not be started, its reason recorded.
pub fn start_gpu<'h, T: Transport>(
    gpu: &mut Engine<'h, T>,
    host: &'h Host,
    then: impl FnOnce(Result<Device, DriverError>, u64) + Send + 'static,
) -> Option<u64> {
    gpu.venus()?;
    let Some(region) = host_visible_region(host) else {
        unserved("no host-visible region", DriverError::NotFound);
        return None;
    };
    let channel = DeviceChannel {
        block: GPU_CHANNEL_ENDPOINTS,
        gate: CapabilityId::GPU,
        max_request: GPU_MAX_REQUEST,
        max_reply: GPU_MAX_REPLY,
        per_sender: GPU_CALL_DEPTH,
    };
    let Some(endpoint) = channel.bind() else {
        unserved("channel unbound", DriverError::Busy);
        return None;
    };
    let Ok(origin) = tairix_rt::self_origin() else {
        unserved("no identity", DriverError::NotFound);
        return None;
    };
    let port = desk_endpoint_for(origin.pid());
    if tairix_rt::port_bind(port, 1, DESK_ROUSE_DEPTH) != 0 {
        unserved("desk port unbound", DriverError::Busy);
        return None;
    }
    let refused = |why: &'static str, reason: DriverError| -> Option<u64> {
        unserved(why, reason);
        // Nothing will rouse the loop, so the port and its queue go back.
        let _ = tairix_rt::port_unbind(port);
        None
    };
    // The channel's server is this process: no other may rouse its loop.
    if let Err(errno) = tairix_rt::port_admit(port, endpoint) {
        return refused("desk port open", DriverError::from_errno(errno));
    }
    let venus = match gpu.serve_venus(region, host, port) {
        Ok(venus) => venus,
        Err(reason) => return refused("desk", reason),
    };
    let started = tairix_rt::thread::Thread::spawn(move || {
        then(venus.device(&RuntimeThreads), endpoint);
    });
    if let Err(errno) = started {
        return refused("no thread", DriverError::from_errno(errno));
    }
    Some(port)
}

/// Publish the GPU's channel and serve it there.
fn serve_on(made: Result<Device, DriverError>, endpoint: u64) {
    let device = match made {
        Ok(device) => device,
        Err(refused) => return unserved("host", refused),
    };
    if publish_channel(
        HwDeviceClass::Accelerator,
        GPU_NODE_COMPATIBLE,
        endpoint,
        &[],
    )
    .is_none()
    {
        return unserved("channel unpublished", DriverError::PermissionDenied);
    }
    log_event(Level::Info, GPU_SERVED, "display: the GPU is served", &[]);
    // Every worker spends a request waiting on the host, so the most the
    // runtime allows serve at once.
    let why = tairix_gpu::serve(&device, endpoint, &RuntimeThreads, MAX_SERVE_WORKERS);
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
