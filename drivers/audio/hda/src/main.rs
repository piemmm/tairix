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
    use tairix_drv_audio_hda::engine::Hda;
    use tairix_drv_audio_hda::{Wait, REQUIRED_CAPABILITIES};
    use tairix_drvrt::exit::{end, error, step, DriverExit};
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

    fn main() -> i32 {
        let caps = REQUIRED_CAPABILITIES.iter().copied().collect();
        let host = match RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) {
            Ok(host) => host,
            Err(err) => {
                return end(
                    DriverExit::NoHost,
                    &[
                        step("hda: the driver host could not be built from the delivered grants"),
                        error(err),
                    ],
                )
            }
        };
        let (base, len) = match sole_register_window(host.resources()) {
            Ok(window) => window,
            Err(err) => {
                return end(
                    DriverExit::NoResources,
                    &[
                        step("hda: the matched node granted no single register window"),
                        error(err),
                    ],
                )
            }
        };
        if let Err(err) = host.bind_irq() {
            return end(
                DriverExit::BringupFailed,
                &[
                    step("hda: the granted interrupt line could not be bound"),
                    error(err),
                ],
            );
        }
        let Some(handle) = host.irq_handle() else {
            return end(
                DriverExit::BringupFailed,
                &[step("hda: the interrupt line bound but minted no handle")],
            );
        };
        let window = match host.map_window(base, len) {
            Ok(window) => window,
            Err(err) => {
                return end(
                    DriverExit::BringupFailed,
                    &[
                        step("hda: the granted register window could not be mapped"),
                        error(err.as_driver_error()),
                    ],
                )
            }
        };
        let clock = ClockDelay::new();
        let audio = match Hda::open(window, IrqWait { handle }, ClockDelay::new(), &host, &clock) {
            Ok(audio) => audio,
            Err(err) => {
                return end(
                    DriverExit::BringupFailed,
                    &[
                        step("hda: the controller or its codecs refused their bring-up"),
                        error(err),
                    ],
                )
            }
        };
        tairix_audiochan::serve(audio, &[tairix_abi::driver::DeviceWake::Irq(handle)])
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
