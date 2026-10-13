//! The `Run` binary of the Maxim DS3231 / DS1307 real-time-clock driver,
//! autoloaded by `devmgr` for a discovered `maxim,ds3231` node
//! (`plans/TIMESYNC.md` TS-4).
//!
//! It reaches its chip only through the I²C target channel its node's link
//! names, which the controller confines to that one address. The part needs no
//! bring-up: a chip that cannot vouch for itself says so at the first read.
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
    use tairix_abi::hwlink::{sole_link, LinkRole};
    use tairix_drv_rtc_ds3231::Ds3231;
    use tairix_drvrt::exit::{end, DriverExit};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_linkclient::{I2cTarget, RtLinkCall};
    use tairix_rt::shm::MappedGrant;

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// On success this never returns: the driver serves its chip for life.
    fn main() -> i32 {
        let caps = tairix_drv_rtc_ds3231::REQUIRED_CAPABILITIES
            .iter()
            .copied()
            .collect();
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return end(DriverExit::NoHost, &[]);
        };
        let Some(link) = sole_link(host.resources(), LinkRole::I2c) else {
            return end(DriverExit::NoResources, &[]);
        };
        let Ok(port) = I2cTarget::open(RtLinkCall::new(link.endpoint()), link, MappedGrant::map)
        else {
            return end(DriverExit::BringupFailed, &[]);
        };
        let mut rtc = Ds3231::new(port);

        tairix_drvrt::serve::rtc(&mut rtc)
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
