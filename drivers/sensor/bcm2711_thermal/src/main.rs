//! The `Run` binary of the BCM2711 AVS temperature sensor driver, autoloaded
//! into user space by `devmgr` for a discovered sensor node: it maps the
//! monitor's window its node was granted and serves the chip's temperature on
//! the node's sensor channel. On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::sole_register_window;
    use tairix_abi::driver::thermal::{ThermalCalibration, ThermalZone};
    use tairix_abi::{HwProperty, MmioMapper};
    use tairix_drv_sensor_bcm2711_thermal::{AvsSensor, REQUIRED_CAPABILITIES};
    use tairix_drvrt::exit::{end, step, DriverExit};
    use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};

    fn main() -> i32 {
        let caps = REQUIRED_CAPABILITIES.iter().copied().collect();
        let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
            return end(DriverExit::NoHost, &[]);
        };
        let calibration = host
            .property(HwProperty::ThermalCalibration)
            .map(ThermalCalibration::from_word);
        let Some(calibration) = calibration else {
            return end(
                DriverExit::NoResources,
                &[step("sensor: the thermal zone states no calibration")],
            );
        };
        let Ok((base, len)) = sole_register_window(host.resources()) else {
            return end(
                DriverExit::NoResources,
                &[step("sensor: the node names no single window")],
            );
        };
        let Ok(regs) = host.map_window(base, len) else {
            return end(
                DriverExit::BringupFailed,
                &[step("sensor: the window would not map")],
            );
        };
        let Ok(mut sensor) = AvsSensor::new(regs, calibration) else {
            return end(
                DriverExit::NoResources,
                &[step("sensor: the window is shorter than the registers")],
            );
        };
        tairix_drvrt::serve::thermal_sensor(&mut sensor, ThermalZone::Processor)
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
