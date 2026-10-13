//! The `Run` entry-point binary of the virtio-net driver, installed as a
//! signed `/System/Drivers/` bundle and **autoloaded into user space** by
//! `devmgr` when a virtio-net device is discovered (`plans/NETWORK.md` N4d).
//!
//! This is the "drivers in user space" steady state for networking: the process
//! owns the NIC (its register window, DMA, and interrupt line) and serves the
//! `netchan-v1` device-channel contract to the network stack
//! (`userland/net/netstack`), which runs in its own address space and owns the
//! shared frame-ring region. The two never link each other — the driver is the
//! *server* of a claimed reserved endpoint and the stack is the *client* — so
//! any NIC driver serves any stack build.
//!
//! It is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt`, never the C ABI (which exists solely for
//! non-Rust programs). `tairix-rt` provides `_start`, the per-process stack
//! canary, the panic handler, the allocator, and the syscall wrappers;
//! `tairix_rt::entry!` names this program's `main`. It links no `drivers/*`
//! crate — the virtio-MMIO transport and the virtio-net device engine are
//! `lib/*` crates — so the layering holds and the kernel never pulls the
//! userland runtime into its graph.
//!
//! # What `main` does
//!
//! 1. Builds the rt-backed `RtDriverHost` from the grants the kernel minted
//!    for this driver's matched node (a register window, a DMA constraint, and
//!    the device interrupt line — and no more), and maps the single register
//!    window the node named (never a build-time board constant).
//! 2. Brings the virtio-net device online over the bus-agnostic virtio
//!    transport (`VirtioNet::open`) and binds the granted interrupt line.
//! 3. Hands the opened device to `tairix_netchan::serve`, the shared
//!    device-channel serve loop every NIC driver process runs: it claims a
//!    reserved endpoint bound restricted-sender, publishes the `netchan` node
//!    `devmgr` hands to the stack, and parks on {call endpoint, device IRQ} for
//!    the life of the driver (never busy-polls).
//!
//! A bring-up failure exits with a reserved fail-closed code
//! (`tairix_drvrt::exit`), leaving the system without this NIC rather than
//! wedged; the spawning supervisor decides whether to relaunch. On the host it
//! is an inert stub so `cargo build --workspace`, clippy, and fmt still cover
//! the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::sole_register_window;
    use tairix_abi::driver::virtio::VirtioHost;
    use tairix_abi::driver::virtio_pci::virtio_pci_windows;
    use tairix_abi::{DriverError, MmioMapper};
    use tairix_drvrt::exit::{end, step, DriverExit};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_virtio::{MmioTransport, PciTransport};
    use tairix_virtio_net::VirtioNet;

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// On success this never returns: the device-channel serve loop runs for
    /// the life of the driver process.
    fn main() -> i32 {
        // The QEMU `virt` virtio interconnect snoops the CPU caches, so the
        // DMA carve is coherent kernel-side and no cache-maintenance shim is
        // supplied here (`coherency = None`, keeping the program neutral).
        let caps = tairix_drv_network_virtio_net_driver::REQUIRED_CAPABILITIES
            .iter()
            .copied()
            .collect();
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return end(DriverExit::NoHost, &[]);
        };
        let Some(irq_line) = host.irq_line() else {
            return end(
                DriverExit::NoResources,
                &[step("virtio-net: the node grants no interrupt line")],
            );
        };

        // Bind the device interrupt line the serve loop parks on. This is the
        // audited readiness witness, issued once the device is live: a driver
        // that cannot bind it must exit rather than degrade into a busy
        // re-poll. It is bound once here and shared by both bus paths (the
        // MMIO line and the kernel-routed MSI-X vector alike surface as one
        // bound handle).
        let irq_ret = tairix_rt::irq_bind(irq_line);
        if irq_ret <= 0 {
            return end(
                DriverExit::BringupFailed,
                &[step("virtio-net: the interrupt line would not bind")],
            );
        }
        #[allow(clippy::cast_sign_loss)] // `irq_ret > 0` is the minted IrqHandle.
        let irq_handle = irq_ret as u64;

        // Shape-key the transport by the grant set the kernel minted, so one
        // signed bundle binds on either virtio bus: a scattered PCI device
        // grants the four role-tagged config windows (`common`/`notify`/`isr`/
        // `device`), while a single-aperture MMIO device grants one register
        // window. The kernel resolved and role-tagged every window — the
        // driver never reads PCI configuration space.
        // Ring and staging depths scale to the machine the host attested,
        // through the shared ring-sizing policy.
        let machine = tairix_abi::DriverHost::machine(&host);
        match virtio_pci_windows(host.resources()) {
            Ok(windows) => {
                let Ok(transport) = PciTransport::map(&windows, &host) else {
                    return end(
                        DriverExit::BringupFailed,
                        &[step("virtio-net: the PCI windows would not map")],
                    );
                };
                let vhost: &dyn VirtioHost = &host;
                let Ok(net) = VirtioNet::open(transport, vhost, machine.as_ref()) else {
                    return end(
                        DriverExit::BringupFailed,
                        &[step("virtio-net: the device would not open")],
                    );
                };
                tairix_netchan::serve(net, irq_handle)
            }
            // No role-tagged window at all: a single-aperture MMIO delivery.
            Err(DriverError::NotFound) => {
                let Ok((base, len)) = sole_register_window(host.resources()) else {
                    return end(
                        DriverExit::NoResources,
                        &[step("virtio-net: the node names no single register window")],
                    );
                };
                let Ok(window) = host.map_window(base, len) else {
                    return end(
                        DriverExit::BringupFailed,
                        &[step("virtio-net: the register window would not map")],
                    );
                };
                let Ok(transport) = MmioTransport::new(window) else {
                    return end(
                        DriverExit::BringupFailed,
                        &[step("virtio-net: the MMIO transport would not open")],
                    );
                };
                let vhost: &dyn VirtioHost = &host;
                let Ok(net) = VirtioNet::open(transport, vhost, machine.as_ref()) else {
                    return end(
                        DriverExit::BringupFailed,
                        &[step("virtio-net: the device would not open")],
                    );
                };
                tairix_netchan::serve(net, irq_handle)
            }
            // Some virtio-PCI windows but not the full four — a malformed,
            // mis-provisioned node. Fail closed rather than half-bind.
            Err(_) => end(
                DriverExit::NoResources,
                &[step(
                    "virtio-net: the node grants some of the PCI windows, not all four",
                )],
            ),
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
