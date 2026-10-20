//! The `Run` program of the virtio-gpu display driver: autoloaded by
//! `devmgr` against a virtio GPU on either transport, it brings the device
//! up, hosts the kernel console where its node does, and serves the seat's
//! display for the life of the process: its presenter's frames carved under
//! the node's DMA grant, every command answered on the device's interrupt.
//!
//! On the host it is an inert stub so the workspace build, clippy and fmt
//! still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::display::{DamageRect, HostedConsole};
    use tairix_abi::driver::virtio::VirtioHost;
    use tairix_abi::driver::virtio_pci::{virtio_pci_windows, VirtioPciWindows};
    use tairix_abi::driver::{sole_register_window, DeviceWake, DmaReach};
    use tairix_abi::hwtree::HwResourceKind;
    use tairix_abi::{DriverError, HwProperty, MmioMapper};
    use tairix_display::service::serve;
    use tairix_display::RtDeviceMemory;
    use tairix_drv_display_gpu_virtio::{
        ConsoleSource, Record, VirtioGpu, CONSOLE_SHOWN_MESSAGE, REQUIRED_CAPABILITIES,
    };
    use tairix_drvrt::exit::{end, error, step, DriverExit};
    use tairix_drvrt::serve::log_event;
    use tairix_drvrt::{Recorder, RtDriverHost, RtGrantSyscalls};
    use tairix_inline::ArrayVec;
    use tairix_log::{EventId, Field, FieldValue, Level};
    use tairix_virtio::{MmioTransport, PciTransport, Transport};

    const CONSOLE_UNHOSTED: EventId = EventId(24_920);
    const SHOW_REFUSED: EventId = EventId(24_921);
    const CONSOLE_UNSHOWN: EventId = EventId(24_922);
    const CONSOLE_SHOWN: EventId = EventId(24_923);

    type Host = RtDriverHost<RtGrantSyscalls>;

    /// The system log, as the engine records to it.
    struct Log;

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

    /// The kernel console the node hosts, painted in memory the kernel
    /// carves under the node's DMA grant.
    struct KernelConsole {
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
    }

    /// The driver host built from the node's grants, its interrupt line
    /// bound, or the status the driver ends with.
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
        // Bound through the host, which caches the handle the command waits
        // park on: a second bind of the same line is refused.
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

    /// Program entry point; on success it never returns.
    fn main() -> i32 {
        let (host, irq) = match host_with_line() {
            Ok(bound) => bound,
            Err(status) => return status,
        };
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
        };
        match virtio_pci_windows(host.resources()) {
            Ok(windows) => serve_pci(&host, &windows, serving),
            // No role-tagged window at all: a single-aperture MMIO delivery.
            Err(DriverError::NotFound) => serve_mmio(&host, serving),
            Err(err) => end(
                DriverExit::NoResources,
                &[
                    step("display: the node's virtio-PCI windows are incomplete"),
                    error(err),
                ],
            ),
        }
    }

    /// Serve the device over the virtio-PCI config `windows` the node granted.
    fn serve_pci(host: &Host, windows: &VirtioPciWindows, serving: Serving) -> i32 {
        match PciTransport::map(windows, host) {
            Ok(transport) => open_and_serve(transport, host, serving),
            Err(err) => end(
                DriverExit::BringupFailed,
                &[
                    step("display: the granted virtio-PCI config windows are no transport"),
                    error(err),
                ],
            ),
        }
    }

    /// Serve the device over the node's one virtio-MMIO register window.
    fn serve_mmio(host: &Host, serving: Serving) -> i32 {
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
            Ok(transport) => open_and_serve(transport, host, serving),
            Err(err) => end(
                DriverExit::BringupFailed,
                &[
                    step("display: the mapped window is not a virtio-MMIO transport"),
                    error(err.as_driver_error()),
                ],
            ),
        }
    }

    /// Bring the device up over `transport` and serve the seat's display for
    /// life.
    fn open_and_serve<T: Transport>(transport: T, host: &Host, serving: Serving) -> i32 {
        let vhost: &dyn VirtioHost = host;
        match VirtioGpu::open(transport, vhost, serving.memory, serving.console, Log) {
            Ok(mut gpu) => serve(&mut gpu, &serving.wakes),
            Err(err) => end(
                DriverExit::BringupFailed,
                &[
                    step("display: the virtio GPU refused its bring-up"),
                    error(err),
                ],
            ),
        }
    }

    tairix_rt::entry!(main);
}

// On the host the program's real entry is not compiled, so this inert `main`
// keeps the crate building under the host tooling.
#[cfg(not(freestanding))]
fn main() {}
