//! The `Run` program of the HD Audio driver: autoloaded by `devmgr` against
//! a discovered HD Audio controller, it brings the controller and its codecs
//! up and serves `audiochan-v1` to the mixer for the life of the process.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::sole_register_window;
    use tairix_abi::{DriverError, Errno, MmioMapper};
    use tairix_audiochan::{exit, fail};
    use tairix_caps::CapabilitySet;
    use tairix_drv_audio_hda::engine::Hda;
    use tairix_drv_audio_hda::{Wait, REQUIRED_CAPS};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_rt::ClockDelay;

    /// The controller's interrupt line, parked on with a budget.
    struct IrqWait {
        handle: u64,
    }

    impl Wait for IrqWait {
        fn park(&self, budget_ns: u64) -> Result<(), DriverError> {
            match tairix_rt::irq_wait(self.handle, budget_ns) {
                0 => Ok(()),
                // A timeout is the caller's deadline to judge.
                ret if Errno::from_syscall(ret) == Errno::TimedOut => Ok(()),
                _ => Err(DriverError::DeviceFault),
            }
        }
    }

    fn driver_caps() -> CapabilitySet {
        let mut caps = CapabilitySet::empty();
        for &cap in REQUIRED_CAPS {
            caps.insert(cap);
        }
        caps
    }

    fn main() -> i32 {
        let host = match RtDriverHost::from_grants_query(driver_caps(), RtGrantSyscalls, None) {
            Ok(host) => host,
            Err(err) => {
                return fail(
                    exit::NO_HOST,
                    "hda: the driver host could not be built from the delivered grants",
                    Some(err),
                )
            }
        };
        let (base, len) = match sole_register_window(host.resources()) {
            Ok(window) => window,
            Err(err) => {
                return fail(
                    exit::NO_RESOURCES,
                    "hda: the matched node granted no single register window",
                    Some(err),
                )
            }
        };
        if let Err(err) = host.bind_irq() {
            return fail(
                exit::BRINGUP_FAILED,
                "hda: the granted interrupt line could not be bound",
                Some(err),
            );
        }
        let Some(handle) = host.irq_handle() else {
            return fail(
                exit::BRINGUP_FAILED,
                "hda: the interrupt line bound but minted no handle",
                None,
            );
        };
        let window = match host.map_window(base, len) {
            Ok(window) => window,
            Err(err) => {
                return fail(
                    exit::BRINGUP_FAILED,
                    "hda: the granted register window could not be mapped",
                    Some(err.as_driver_error()),
                )
            }
        };
        let clock = ClockDelay::new();
        let audio = match Hda::open(window, IrqWait { handle }, ClockDelay::new(), &host, &clock) {
            Ok(audio) => audio,
            Err(err) => {
                return fail(
                    exit::BRINGUP_FAILED,
                    "hda: the controller or its codecs refused their bring-up",
                    Some(err),
                )
            }
        };
        tairix_audiochan::serve(audio, &[tairix_audiochan::Wake::Irq(handle)])
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
