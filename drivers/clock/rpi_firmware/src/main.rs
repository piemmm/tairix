//! The `Run` binary of the Raspberry Pi firmware-clocks driver, autoloaded
//! into user space by `devmgr` for a discovered `raspberrypi,firmware-clocks`
//! node.
//!
//! It takes the machine's frequency mechanism role for the ARM clock and
//! applies the kernel governor's targets on a thread of its own, parked in
//! `cpufreq_wait` between them; and it serves the node's `clock-v1` endpoint
//! for the display's clocks on its main thread. The two touch different
//! clocks and share nothing. Either ending ends the driver with its reason
//! logged, so the kernel releases the mechanism role and the endpoint with
//! the process and a replacement can take both. On the host it is an inert
//! stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::cpufreq::CpuFreqTarget;
    use tairix_clock::serve;
    use tairix_drv_clock_rpi_firmware::{ArmClock, LinkedClocks, REQUIRED_CAPABILITIES};
    use tairix_drvrt::exit::{end, step, DriverExit};
    use tairix_drvrt::serve::log_event;
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_log::{EventId, Level};
    use tairix_rt::thread::Thread;

    const EVENT_MECHANISM_READY: EventId = EventId(24_700);

    /// Consecutive refused targets tolerated before the mechanism stands
    /// down: one is worth riding out, the governor's next arriving within a
    /// response window, but a mechanism that has refused several in a row is
    /// applying nothing and keeps a working replacement out of the role.
    const APPLY_FAILURE_BUDGET: u32 = 4;

    fn run() -> Result<i32, i32> {
        let node = tairix_clock::open(REQUIRED_CAPABILITIES.iter().copied().collect())?;
        let limits = ArmClock::new(node.host()).limits().map_err(|_| {
            end(
                DriverExit::BringupFailed,
                &[step(
                    "cpufreq: the firmware would not report the ARM clock's range",
                )],
            )
        })?;
        let handle = tairix_rt::cpufreq_bind(&limits);
        if handle < 0 {
            return Err(end(
                DriverExit::NoService,
                &[step("cpufreq: the mechanism role could not be taken")],
            ));
        }
        Thread::spawn(move || apply_targets(handle))
            .map_err(|_| {
                end(
                    DriverExit::NoService,
                    &[step("cpufreq: the mechanism's thread could not be started")],
                )
            })?
            .detach();
        log_event(
            Level::Info,
            EVENT_MECHANISM_READY,
            "cpufreq: mechanism applying targets",
            &[],
        );
        let clocks = LinkedClocks::new(node.host()).map_err(|_| {
            end(
                DriverExit::BringupFailed,
                &[step(
                    "clock: the firmware would not report the linked clocks' ranges",
                )],
            )
        })?;
        Ok(serve(&node, clocks))
    }

    /// Block in `cpufreq_wait` for the governor's next target, a genuine park
    /// in the kernel, and ask the firmware for it; end the driver once the
    /// mechanism cannot go on.
    ///
    /// The applied rate is not reported back: the per-CPU estimator measures
    /// the live core clock from the silicon's own counters, a better witness
    /// than this driver's word.
    fn apply_targets(handle: i64) {
        let caps = REQUIRED_CAPABILITIES.iter().copied().collect();
        let Ok(host) = RtDriverHost::new(caps, RtGrantSyscalls, &[], None) else {
            tairix_rt::exit(end(
                DriverExit::NoHost,
                &[step("cpufreq: the mechanism's host could not be built")],
            ));
        };
        let arm = ArmClock::new(&host);
        let mut target = CpuFreqTarget::default();
        let mut refused = 0u32;
        loop {
            if tairix_rt::cpufreq_wait(handle, target.seq, &mut target) != 0 {
                tairix_rt::exit(end(
                    DriverExit::ServeFailed,
                    &[step("cpufreq: the wait for a target failed")],
                ));
            }
            if arm.apply(target.target_hz).is_ok() {
                refused = 0;
            } else {
                refused += 1;
                if refused >= APPLY_FAILURE_BUDGET {
                    tairix_rt::exit(end(
                        DriverExit::DeviceFault,
                        &[step("cpufreq: the firmware refused every recent target")],
                    ));
                }
            }
        }
    }

    fn main() -> i32 {
        match run() {
            Ok(code) | Err(code) => code,
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
