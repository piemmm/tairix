use std::cell::Cell;

use tairix_abi::driver::thermal::{Millicelsius, ThermalCalibration, ThermalSensor};
use tairix_abi::{CapabilityId, DriverError, DriverHost, DriverKind, Errno, RegisterBlock};

use crate::{register, AvsSensor, WINDOW_LEN};

/// Raw codes the sensor reads at about 48.7 and 69.1 degrees.
const CODE_742: u32 = 0x2E6;
const CODE_700: u32 = 0x2BC;

/// The BCM2711's own zone: `coefficients = <(-487) 410040>`.
const BCM2711: ThermalCalibration = ThermalCalibration {
    slope: -487,
    offset: 410_040,
};

/// The monitor's window: its temperature status, and whether reads fault.
struct Monitor {
    status: Cell<u32>,
    len: usize,
    faulty: bool,
}

impl Monitor {
    fn new(status: u32) -> Self {
        Self {
            status: Cell::new(status),
            len: 0xF00,
            faulty: false,
        }
    }
}

impl RegisterBlock for &Monitor {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        if self.faulty {
            return Err(DriverError::DeviceFault);
        }
        assert_eq!(offset, 0x200, "only the temperature status is read");
        Ok(self.status.get())
    }

    fn write32(&self, _offset: usize, _value: u32) -> Result<(), DriverError> {
        panic!("the sensor writes nothing");
    }

    fn block_len(&self) -> usize {
        self.len
    }
}

#[test]
fn a_valid_code_converts_through_the_zone_s_calibration() {
    let monitor = Monitor::new((1 << 10) | CODE_742);
    let mut sensor = AvsSensor::new(&monitor, BCM2711).unwrap();
    assert_eq!(sensor.read(), Ok(Millicelsius(48_686)));
    monitor.status.set((1 << 16) | CODE_700);
    assert_eq!(sensor.read(), Ok(Millicelsius(69_140)));
}

#[test]
fn the_code_is_read_beneath_the_valid_bits_alone() {
    let monitor = Monitor::new(0xFFFE_F800 | (1 << 10) | CODE_742);
    let mut sensor = AvsSensor::new(&monitor, BCM2711).unwrap();
    assert_eq!(sensor.read(), Ok(Millicelsius(48_686)));
}

#[test]
fn no_completed_conversion_is_no_data_and_a_faulting_read_a_fault() {
    let monitor = Monitor::new(CODE_742);
    let mut sensor = AvsSensor::new(&monitor, BCM2711).unwrap();
    assert_eq!(sensor.read(), Err(Errno::NoData));
    let faulty = Monitor {
        faulty: true,
        ..Monitor::new(1 << 10)
    };
    let mut sensor = AvsSensor::new(&faulty, BCM2711).unwrap();
    assert_eq!(sensor.read(), Err(Errno::DeviceFault));
}

#[test]
fn a_calibration_past_a_reading_s_range_is_a_fault_not_a_wrapped_value() {
    let monitor = Monitor::new((1 << 10) | 0x3FF);
    let steep = ThermalCalibration {
        slope: i32::MAX,
        offset: i32::MAX,
    };
    let mut sensor = AvsSensor::new(&monitor, steep).unwrap();
    assert_eq!(sensor.read(), Err(Errno::DeviceFault));
}

#[test]
fn a_window_short_of_the_status_register_is_refused() {
    let short = Monitor {
        len: WINDOW_LEN - 1,
        ..Monitor::new(0)
    };
    assert!(matches!(
        AvsSensor::new(&short, BCM2711),
        Err(DriverError::OutOfRange)
    ));
}

struct Host(bool);

impl DriverHost for Host {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        self.0 && cap == CapabilityId::DRV_LOAD
    }
    fn kind(&self) -> DriverKind {
        DriverKind::UserSpace
    }
}

#[test]
fn loading_needs_its_capability() {
    assert!(register(&Host(true)).is_ok());
    assert_eq!(register(&Host(false)), Err(DriverError::PermissionDenied));
}
