//! The `Run` entry-point binary of the virtio sound driver, installed as a
//! signed `/System/Drivers/` bundle and **autoloaded into user space** by
//! `devmgr` when a virtio-snd device is discovered (`plans/SOUND.md` SND4).
//!
//! This is the "drivers in user space" steady state for audio: the process
//! owns the device (its register window, DMA, and interrupt line) and serves
//! the `audiochan-v1` device-channel contract to the mixer service
//! (`userland/system/audiod`), which runs in its own address space and owns
//! the shared PCM regions. The two never link each other — the driver is the
//! *server* of a claimed reserved endpoint and the mixer is the one *client*
//! the kernel admits — so any audio driver serves any mixer build. Nothing
//! about sound is in the kernel.
//!
//! It is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt`, never the C ABI (which exists solely for
//! non-Rust programs). It links no `drivers/*` crate — the virtio transport
//! and the device-channel serve loop are `lib/*` crates and the device engine
//! is this crate's own `lib` target — so the layering holds.
//!
//! # What `main` does
//!
//! 1. Builds the rt-backed `RtDriverHost` from the grants the kernel minted
//!    for this driver's matched node (a register window, a DMA constraint,
//!    and the device interrupt line — and no more).
//! 2. Binds the granted interrupt line the serve loop parks on.
//! 3. Brings the device online over the bus-agnostic virtio transport,
//!    shape-keyed by the grant set so one signed bundle binds on either bus.
//! 4. Hands the opened device to `tairix_audiochan::serve`, the shared
//!    device-channel serve loop every audio driver process runs: it claims a
//!    reserved endpoint bound restricted-sender on `CAP_AUDIO_DEVICE`,
//!    publishes the `audiochan` node `devmgr` hands to the mixer, and parks
//!    on {call endpoint, device IRQ} for the life of the driver.
//!
//! A bring-up failure exits with a reserved fail-closed code
//! (`tairix_drvrt::exit`), leaving the machine without sound rather than
//! wedged; the spawning supervisor decides whether to relaunch. On the host
//! it is an inert stub so `cargo build --workspace`, clippy, and fmt still
//! cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::virtio::VirtioHost;
    use tairix_abi::driver::virtio_pci::virtio_pci_windows;
    use tairix_abi::driver::{sole_register_window, DeviceWake};
    use tairix_abi::time::MonotonicClock;
    use tairix_abi::{DriverError, MmioMapper};
    use tairix_drv_audio_virtio_snd::VirtioSnd;
    use tairix_drvrt::exit::{end, error, step, DriverExit};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_rt::ClockDelay;
    use tairix_virtio::{MmioTransport, PciTransport};

    /// Why a bring-up gave up once the transport was built, on either bus.
    /// The `error` field the record carries is what the device or the kernel
    /// actually refused with.
    const OPEN_REFUSED: &str = "virtio-snd: the device refused its bring-up";

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// On success this never returns: the device-channel serve loop runs for
    /// the life of the driver process.
    fn main() -> i32 {
        let (host, irq_handle) = match host_with_line() {
            Ok(bound) => bound,
            Err(status) => return status,
        };
        // The clock every period's `(position, sampled_at)` pair is stamped
        // from. Monotonic, because a wall clock stepped by the time service
        // would corrupt the mixer's rate fit.
        let clock = ClockDelay::new();
        let wakes = [DeviceWake::Irq(irq_handle)];
        match virtio_pci_windows(host.resources()) {
            Ok(windows) => serve_pci(&host, &windows, clock, &wakes),
            // No role-tagged window at all: a single-aperture MMIO delivery.
            Err(DriverError::NotFound) => serve_mmio(&host, clock, &wakes),
            // Some virtio-PCI windows but not the full four — a malformed,
            // mis-provisioned node. Fail closed rather than half-bind.
            Err(err) => end(
                DriverExit::NoResources,
                &[
                    step("virtio-snd: the node's virtio-PCI windows are incomplete"),
                    error(err),
                ],
            ),
        }
    }

    /// The driver host built from the node's grants, with its interrupt line
    /// bound, or the status the driver ends with.
    fn host_with_line() -> Result<(RtDriverHost<RtGrantSyscalls>, u64), i32> {
        // The QEMU `virt` virtio interconnect snoops the CPU caches, so the
        // DMA carve is coherent kernel-side and no cache-maintenance shim is
        // supplied here, which keeps the program platform-neutral.
        let caps = tairix_drv_audio_virtio_snd::REQUIRED_CAPABILITIES
            .iter()
            .copied()
            .collect();
        let host = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None).map_err(|err| {
            end(
                DriverExit::NoHost,
                &[
                    step(
                        "virtio-snd: the driver host could not be built from the delivered grants",
                    ),
                    error(err),
                ],
            )
        })?;
        if host.irq_line().is_none() {
            return Err(end(
                DriverExit::NoResources,
                &[step(
                    "virtio-snd: the matched node granted no interrupt line",
                )],
            ));
        }
        // Bound through the host, which caches the handle, rather than by
        // calling the trap directly: the device bring-up parks on completions
        // through this same host, so a direct bind would bind the line a
        // second time and the kernel refuses that. Binding before the device
        // is live is safe because its event sources stay masked until the
        // mixer attaches a region.
        host.bind_irq().map_err(|err| {
            end(
                DriverExit::BringupFailed,
                &[
                    step("virtio-snd: the granted interrupt line could not be bound"),
                    error(err),
                ],
            )
        })?;
        let Some(irq_handle) = host.irq_handle() else {
            return Err(end(
                DriverExit::BringupFailed,
                &[step(
                    "virtio-snd: the interrupt line bound but minted no handle",
                )],
            ));
        };
        Ok((host, irq_handle))
    }

    /// Serve the device over the virtio-PCI config `windows` the node granted.
    fn serve_pci(
        host: &RtDriverHost<RtGrantSyscalls>,
        windows: &tairix_abi::driver::virtio_pci::VirtioPciWindows,
        clock: ClockDelay,
        wakes: &[DeviceWake],
    ) -> i32 {
        let transport =
            match PciTransport::map(windows, host) {
                Ok(transport) => transport,
                Err(err) => return end(
                    DriverExit::BringupFailed,
                    &[
                        step("virtio-snd: the granted virtio-PCI config windows are no transport"),
                        error(err),
                    ],
                ),
            };
        open_and_serve(transport, host, clock, wakes)
    }

    /// Serve the device over the node's one virtio-MMIO register window.
    fn serve_mmio(
        host: &RtDriverHost<RtGrantSyscalls>,
        clock: ClockDelay,
        wakes: &[DeviceWake],
    ) -> i32 {
        let (base, len) = match sole_register_window(host.resources()) {
            Ok(window) => window,
            Err(err) => {
                return end(
                    DriverExit::NoResources,
                    &[
                        step("virtio-snd: the matched node granted no single register window"),
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
                        step("virtio-snd: the granted register window could not be mapped"),
                        error(err.as_driver_error()),
                    ],
                )
            }
        };
        let transport = match MmioTransport::new(window) {
            Ok(transport) => transport,
            Err(err) => {
                return end(
                    DriverExit::BringupFailed,
                    &[
                        step("virtio-snd: the mapped window is not a virtio-MMIO transport"),
                        error(err.as_driver_error()),
                    ],
                )
            }
        };
        open_and_serve(transport, host, clock, wakes)
    }

    /// Bring the device up over `transport` and serve its channel for life.
    fn open_and_serve<T: tairix_virtio::Transport>(
        transport: T,
        host: &RtDriverHost<RtGrantSyscalls>,
        clock: ClockDelay,
        wakes: &[DeviceWake],
    ) -> i32 {
        let vhost: &dyn VirtioHost = host;
        let mclock: &dyn MonotonicClock = &clock;
        match VirtioSnd::open(transport, vhost, mclock) {
            Ok(audio) => tairix_audiochan::serve(audio, wakes),
            Err(err) => end(DriverExit::BringupFailed, &[step(OPEN_REFUSED), error(err)]),
        }
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// On the host (`cargo build --workspace`, clippy, fmt) the program's real
// entry — the freestanding `tairix-rt` `_start` path — is not compiled, so
// this inert `main` keeps the crate building under the host tooling. It
// performs no I/O.
#[cfg(not(freestanding))]
fn main() {}
