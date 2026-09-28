//! The `Run` entry-point binary of the Raspberry Pi firmware-framebuffer
//! display service, installed as a signed `/System/Drivers/` bundle and
//! autoloaded by `devmgr` when the boot display node carries the firmware
//! framebuffer's binding.
//!
//! `main` builds the driver host from the kernel-issued grants, maps the
//! granted surface, and hands it — switched through the firmware over the
//! host's mailbox channel — to the one display-service loop
//! (`tairix_display::service`). The surface's base, geometry and pixel format
//! come from the grants, never a board constant.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy,
//! and fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {
    use tairix_caps::CapabilitySet;
    use tairix_display::service::{open_surface, serve, EXIT_NO_HOST};
    use tairix_drv_display_rpi_fb::{FirmwareDisplay, REQUIRED_CAPABILITIES};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};

    /// The capabilities the host re-checks before a trap or a property
    /// exchange, from the one definition the manifest is built from. The
    /// kernel re-checks every trap regardless.
    fn service_caps() -> CapabilitySet {
        let mut caps = CapabilitySet::empty();
        for cap in REQUIRED_CAPABILITIES {
            caps.insert(*cap);
        }
        caps
    }

    /// Program entry point; on success it never returns.
    fn main() -> i32 {
        let Ok(host) = RtDriverHost::from_grants_query(service_caps(), RtGrantSyscalls, None)
        else {
            return EXIT_NO_HOST;
        };
        let surface = match open_surface(&host) {
            Ok(surface) => surface,
            Err(code) => return code,
        };
        serve(&mut FirmwareDisplay::new(surface, &host))
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// On the host the program's real entry is not compiled, so this inert `main`
// keeps the crate building under the host tooling. It performs no I/O.
#[cfg(not(freestanding))]
fn main() {}
