//! The `Run` binary of the Broadcom Serial Controller I²C bus driver,
//! autoloaded into user space by `devmgr` for a discovered `brcm,bcm2835-i2c`
//! node.
//!
//! It maps the controller's window, binds its interrupt, reaches the VPU
//! clock its bus clock is divided from through the node's clock link, and
//! serves `i2c-v1` under the node's I²C duty. On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::hwlink::{sole_link, LinkRole};
    use tairix_abi::DriverError;
    use tairix_drv_bus_i2c_bcm2835::{Bsc, BusWait, ParentClock, REQUIRED_CAPABILITIES};
    use tairix_drvrt::exit::{end, step, DriverExit};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
    use tairix_i2cbus::{declared_speed, serve};
    use tairix_linkclient::{ClockClient, RtLinkCall};

    /// Park on the controller's bound interrupt line, the caller's remaining
    /// budget the deadline.
    struct IrqWait<'a>(&'a RtDriverHost<RtGrantSyscalls>);

    impl BusWait for IrqWait<'_> {
        fn now_ns(&self) -> u64 {
            tairix_rt::clock_get()
        }

        fn wait(&self, timeout_ns: u64) {
            // A refused park returns to the caller's budget check rather than
            // spinning; the line was bound before the first transaction.
            let _ = self.0.wait_irq(timeout_ns);
        }
    }

    /// The VPU clock, as its controller describes it.
    struct Vpu(ClockClient<RtLinkCall>);

    impl ParentClock for Vpu {
        fn rate(&mut self) -> Result<u64, DriverError> {
            self.0
                .describe()
                .map(|state| state.hz)
                .map_err(DriverError::from_errno)
        }
    }

    fn run() -> Result<i32, i32> {
        let node = tairix_i2cbus::open(REQUIRED_CAPABILITIES.iter().copied().collect())?;
        let speed = declared_speed(&node)?;
        let Some(clock) = sole_link(node.resources(), LinkRole::Clock) else {
            return Err(end(
                DriverExit::NoResources,
                &[step("i2c: the node links no parent clock")],
            ));
        };
        let regs = node.map()?;
        node.host().bind_irq().map_err(|_| {
            end(
                DriverExit::BringupFailed,
                &[step("i2c: the interrupt line could not be bound")],
            )
        })?;
        let wait = IrqWait(node.host());
        let parent = Vpu(ClockClient::new(RtLinkCall::new(clock.endpoint()), clock));
        let bsc = Bsc::new(&regs, &wait, parent, speed).map_err(|_| {
            end(
                DriverExit::BringupFailed,
                &[step(
                    "i2c: the register window does not reach the controller",
                )],
            )
        })?;
        Ok(serve(&node, bsc))
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
