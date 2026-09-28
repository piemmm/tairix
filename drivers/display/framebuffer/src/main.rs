//! The `Run` entry-point binary of the framebuffer display service,
//! installed as a signed `/System/Drivers/` bundle and **autoloaded into
//! user space** by `devmgr` when a display node carrying a
//! `HwResourceKind::Framebuffer` resource is discovered
//! (`plans/DISPLAY.md` D7b).
//!
//! This is the display half of the zero-copy, lease-gated present path:
//! a desktop session composes frames into one `shm_grant`ed region and
//! presents by index over the reserved `DISPLAY_ENDPOINT`; this process
//! blits the presented frame to the scan-out surface. It names no board,
//! bus, or firmware detail: the surface's physical base, geometry, and
//! pixel format are read from the kernel-issued device-resource grants
//! its matched node requested, never a build-time constant.
//!
//! `main` builds the driver host from those grants and hands the surface to
//! the one display-service loop (`tairix_display::service`), which binds the
//! reserved rendezvous, parks between requests and serves them through the
//! shared engine. A linear surface has no power control, so a request to
//! switch the display off is answered as unsupported and the session blanks
//! the screen itself instead.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy,
//! and fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {
    use tairix_abi::CapabilityId;
    use tairix_caps::CapabilitySet;
    use tairix_display::service::{open_surface, serve, EXIT_NO_HOST};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};

    /// The capabilities the driver host re-checks before issuing a trap, so a
    /// missing grant fails fast: the scan-out window (`CAP_MMIO_MAP`) and the
    /// client frame regions the engine maps (`CAP_SHM`). The kernel re-checks
    /// every trap regardless.
    fn service_caps() -> CapabilitySet {
        let mut caps = CapabilitySet::empty();
        caps.insert(CapabilityId::MMIO_MAP);
        caps.insert(CapabilityId::SHM);
        caps
    }

    /// Program entry point; on success it never returns.
    fn main() -> i32 {
        let Ok(host) = RtDriverHost::from_grants_query(service_caps(), RtGrantSyscalls, None)
        else {
            return EXIT_NO_HOST;
        };
        match open_surface(&host) {
            Ok(mut surface) => serve(&mut surface),
            Err(code) => code,
        }
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// On the host the program's real entry — the freestanding `tairix-rt`
// `_start` path — is not compiled, so this inert `main` keeps the crate
// building under the host tooling. It performs no I/O.
#[cfg(not(freestanding))]
fn main() {}
