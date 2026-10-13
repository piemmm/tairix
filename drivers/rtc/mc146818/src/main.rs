//! The `Run` binary of the MC146818 CMOS real-time-clock driver, autoloaded
//! by `devmgr` for a discovered `motorola,mc146818` node (`plans/TIMESYNC.md`
//! TS-3).
//!
//! It has no window to map: it reaches the chip one bounded byte at a time
//! through the port-I/O traps over its node's granted range, which the kernel
//! re-checks against `CAP_MMIO_MAP` and bounds within the grant.
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
    use tairix_abi::driver::sole_port_range;
    use tairix_abi::{DriverError, PortWidth};
    use tairix_drv_rtc_mc146818::{CmosPorts, Mc146818, PORT_RANGE_LEN};
    use tairix_drvrt::exit::{end, DriverExit};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};

    /// The chip's granted port range, addressed through the capability-gated
    /// byte-wide port traps.
    struct GrantedPorts {
        /// The unforgeable grant handle the kernel resolves owner-checked on
        /// every trap.
        handle: u64,
        /// First port of the granted range.
        base: u16,
        /// Ports the range covers.
        count: u16,
    }

    impl GrantedPorts {
        /// The absolute port `offset` names, refused if it falls outside the
        /// granted range. The kernel re-bounds the transfer regardless; this
        /// keeps a driver bug from becoming a trap the kernel has to reject.
        fn port(&self, offset: u16) -> Result<u16, DriverError> {
            if offset >= self.count {
                return Err(DriverError::DeviceFault);
            }
            self.base
                .checked_add(offset)
                .ok_or(DriverError::DeviceFault)
        }
    }

    impl CmosPorts for GrantedPorts {
        fn read(&mut self, offset: u16) -> Result<u8, DriverError> {
            let port = self.port(offset)?;
            let value = tairix_rt::port_read(self.handle, port, PortWidth::Byte);
            // A byte transfer answers `0..=255`; a negative result is
            // `-errno` and anything wider is a kernel-side inconsistency.
            // Both fail closed rather than yielding a fabricated byte.
            u8::try_from(value).map_err(|_| DriverError::DeviceFault)
        }

        fn write(&mut self, offset: u16, value: u8) -> Result<(), DriverError> {
            let port = self.port(offset)?;
            if tairix_rt::port_write(self.handle, port, PortWidth::Byte, u32::from(value)) != 0 {
                return Err(DriverError::DeviceFault);
            }
            Ok(())
        }
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime
    /// is set up and routes its return value through the `exit` syscall.
    ///
    /// On success this never returns: the driver serves its chip for life.
    fn main() -> i32 {
        let caps = tairix_drv_rtc_mc146818::REQUIRED_CAPABILITIES
            .iter()
            .copied()
            .collect();
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return end(DriverExit::NoHost, &[]);
        };
        let Ok((base, count)) = sole_port_range(host.resources()) else {
            return end(DriverExit::NoResources, &[]);
        };
        let Some(handle) = host.port_grant() else {
            return end(DriverExit::NoResources, &[]);
        };
        if count < PORT_RANGE_LEN {
            return end(DriverExit::BringupFailed, &[]);
        }
        let mut rtc = Mc146818::new(GrantedPorts {
            handle,
            base,
            count,
        });

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
