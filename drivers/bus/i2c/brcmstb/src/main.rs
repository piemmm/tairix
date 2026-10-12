//! The `Run` binary of the BCM2711 HDMI DDC controller driver, autoloaded into
//! user space by `devmgr` for a discovered `brcm,bcm2711-hdmi-i2c` node.
//!
//! It maps the controller's and the automatic I²C engine's windows, takes the
//! controller from that engine, and serves `i2c-v1` under the node's I²C duty.
//! On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::DriverError;
    use tairix_drv_bus_i2c_brcmstb::{Ddc, AUTO_I2C_WINDOW, BSC_WINDOW, REQUIRED_CAPABILITIES};
    use tairix_i2cbus::{bringup_failed, declared_speed, serve};
    use tairix_rt::ClockDelay;

    fn run() -> Result<i32, i32> {
        let node = tairix_i2cbus::open(REQUIRED_CAPABILITIES.iter().copied().collect())?;
        let speed = declared_speed(&node)?;
        let bsc = node.map_named(BSC_WINDOW)?;
        let auto_i2c = node.map_named(AUTO_I2C_WINDOW)?;
        let ddc = Ddc::new(&bsc, &auto_i2c, ClockDelay::new(), speed).map_err(|err| {
            bringup_failed(if err == DriverError::OutOfRange {
                "i2c: the controller makes no bus clock at or below the declared speed"
            } else {
                "i2c: the register windows do not reach the controller"
            })
        })?;
        Ok(serve(&node, ddc))
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
