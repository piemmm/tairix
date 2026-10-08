//! `plans/NETWORK.md` N4e-β QEMU integration test: boot the production
//! aarch64 `tairix-kernel` pipeline on the `virt` board against the shared
//! whole-disk autoload-root image — whose read-only `/System` volume now
//! carries the **kernel-signed virtio-net driver bundle** in its `Drivers/`
//! store alongside the input and display bundles — with a `virtio-net-device`
//! attached and the harness-side `netstack_peer` link peer on the QEMU
//! `dgram` netdev, and prove the full **two-process** network path end to
//! end.
//!
//! ## What this vertical asserts — and how it differs from its siblings
//!
//! * The full **two-process** network path (N4e-β): the driver runs in its
//!   own user process, the stack in another, and they speak the `netchan-v1`
//!   device-channel contract across the boundary — unlike a single-process
//!   in-kernel engine test, the frame provably crosses a real process
//!   boundary here.
//! * `autoload_input_qemu_aarch64` proves the driver-loading-by-discovery
//!   autoload path for the *input* and *display* classes. This vertical
//!   composes the same production autoload path for the *network* class.
//!
//! The production boot path:
//!
//! 1. **Discovers** the virtio-block root *and* the virtio-net node
//!    (bootstrap-floor virtio-MMIO enumeration), each carrying its register
//!    window, DMA constraint, and GICv2 interrupt line as capability-grant
//!    requests.
//! 2. **Autoloads** the signed virtio-net bundle from the mounted `/System`
//!    store into its own user-space process (the pre-unlock `devmgr` autoload
//!    hook, verified against the kernel's embedded driver trust anchor); the
//!    driver brings the device up, claims its reserved device-channel
//!    endpoint under `CAP_IPC_BIND_PRIVILEGED`, and publishes a `netchan`
//!    hardware-tree node.
//! 3. The long-running user-space **`devmgr`** service observes the `netchan`
//!    node and calls **`netstack`** `BindDriver` under `CAP_NET_ADMIN`.
//! 4. **`netstack`** provisions the shared frame region, attaches the driver
//!    channel, and auto-configures the interface's EUI-64 IPv6 link-local
//!    address (no IPv4 — no DHCP/admin client at boot), then answers the host
//!    peer's link-local echo campaign.
//!
//! ## How the run completes — harness-driven, race-free
//!
//! The guest does **not** self-terminate. It boots the production pipeline and
//! keeps serving the host peer's link-local echo campaign; the harness ends the
//! run the instant the peer's out-of-guest observer confirms success — it
//! received the guest's echo reply. That confirmation is the *last* link in the
//! causal chain (driver autoloaded and bound, `netstack` bound and the
//! interface up, an inbound echo served and its reply transmitted back), so a
//! guest that instead self-exited on an intermediate witness would tear the
//! machine down before the reply left it and lose the race — the defect this
//! choreography removes. The three witness records (`devmgr`'s `NETSTACK_BOUND`,
//! `netstack`'s `DRIVER_BOUND` and `INBOUND_ECHO_SERVED`) still reach the serial
//! transcript for diagnosis, and the peer's echo verdict subsumes them: it
//! cannot be met unless all three occurred. A run that never earns the peer's
//! confirmation fails loud on the runner's inactivity/absolute deadline.
//!
//! ## How it differs from a production kernel
//!
//! It reuses the entire production aarch64 boot pipeline unchanged. The only
//! difference is that it is a dedicated test bin the harness drives to
//! completion through the peer's success gate — there is no in-kernel QEMU-exit
//! shortcut to leak into a production build (fail closed; the harness never
//! decides what the kernel does next).

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`aarch64-unknown-none`) ----------------

#[cfg(itest_aarch64)]
mod vertical;

#[cfg(itest_aarch64)]
mod tree {
    include!(concat!(env!("OUT_DIR"), "/dtb_fixture.rs"));
}

/// The symbol the arch crate's boot trampoline calls: the one-CPU machine.
#[cfg(itest_aarch64)]
#[no_mangle]
pub extern "C" fn kernel_main(_dtb: u64) -> ! {
    vertical::boot(tree::DTB_BLOB)
}

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_aarch64))]
fn main() {}
