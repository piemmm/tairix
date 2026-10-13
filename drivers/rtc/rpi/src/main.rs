//! The `Run` binary of the Raspberry Pi real-time-clock driver, autoloaded by
//! `devmgr` for a discovered `raspberrypi,rpi-rtc` node (`plans/TIMESYNC.md`
//! TS-4).
//!
//! The clock is in the board's power-management IC, so its node grants no
//! window: the driver reaches it by property exchanges with the `VideoCore`
//! firmware through the `vcmailbox` service, gated kernel-side by
//! `CAP_MAILBOX`.
//!
//! It holds no clock authority: `timed`, the one holder of `CAP_TIME_SET`,
//! reads the RTC endpoint this driver serves through `tairix_drvrt::serve::rtc`
//! and tags the reading `Firmware` itself, so no driver can assert its own
//! provenance and roll a network sync back. On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {
    use tairix_drv_rtc_rpi::RpiRtc;
    use tairix_drvrt::exit::{end, DriverExit};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// On success this never returns: the driver serves its chip for life.
    fn main() -> i32 {
        let caps = tairix_drv_rtc_rpi::REQUIRED_CAPABILITIES
            .iter()
            .copied()
            .collect();
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return end(DriverExit::NoHost, &[]);
        };
        let mut rtc = RpiRtc::new(host);

        tairix_drvrt::serve::rtc(&mut rtc)
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
