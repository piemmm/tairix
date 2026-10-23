use super::{
    decode_reading, decode_request, encode_read, encode_reading, serve_request, Millicelsius,
    SensorOp, ThermalCalibration, ThermalSensor, ThermalZone, SENSOR_REPLY_LEN, SENSOR_REQUEST_LEN,
    THERMAL_SENSOR_ENDPOINTS,
};
use crate::hwtree::NodeEndpointBlock;
use crate::Errno;

/// The BCM2711's own zone: `coefficients = <(-487) 410040>`.
const BCM2711: ThermalCalibration = ThermalCalibration {
    slope: -487,
    offset: 410_040,
};

struct Fixed(Result<Millicelsius, Errno>);

impl ThermalSensor for Fixed {
    fn read(&mut self) -> Result<Millicelsius, Errno> {
        self.0
    }
}

fn request() -> [u8; SENSOR_REQUEST_LEN] {
    let mut frame = [0u8; SENSOR_REQUEST_LEN];
    assert_eq!(encode_read(&mut frame), Ok(SENSOR_REQUEST_LEN));
    frame
}

#[test]
fn a_read_round_trips_and_every_malformed_frame_is_refused() {
    let frame = request();
    assert_eq!(decode_request(&frame), Ok(SensorOp::Read));
    assert_eq!(decode_request(&frame[..7]), Err(Errno::LengthOutOfRange));
    let mut magic = frame;
    magic[0] ^= 1;
    assert_eq!(decode_request(&magic), Err(Errno::BadMagic));
    let mut reserved = frame;
    reserved[7] = 1;
    assert_eq!(decode_request(&reserved), Err(Errno::BadMagic));
    let mut version = frame;
    version[4] = 2;
    assert_eq!(decode_request(&version), Err(Errno::AbiVersionUnsupported));
    let mut op = frame;
    op[6] = 0;
    assert_eq!(decode_request(&op), Err(Errno::OutOfRange));
    assert_eq!(encode_read(&mut [0u8; 7]), Err(Errno::BufferTooSmall));
}

#[test]
fn a_reading_and_a_refusal_each_round_trip() {
    let mut out = [0u8; SENSOR_REPLY_LEN];
    for reading in [Millicelsius(-40_500), Millicelsius(0), Millicelsius(52_314)] {
        assert_eq!(encode_reading(&mut out, Ok(reading)), Ok(SENSOR_REPLY_LEN));
        assert_eq!(decode_reading(&out), Ok(reading));
    }
    assert_eq!(
        encode_reading(&mut out, Err(Errno::NoData)),
        Ok(SENSOR_REPLY_LEN)
    );
    assert_eq!(decode_reading(&out), Err(Errno::NoData));
}

#[test]
fn a_reply_that_carries_a_temperature_beside_a_refusal_is_refused() {
    let mut out = [0u8; SENSOR_REPLY_LEN];
    encode_reading(&mut out, Err(Errno::DeviceFault)).unwrap();
    out[SENSOR_REPLY_LEN - 1] = 1;
    assert_eq!(decode_reading(&out), Err(Errno::BadMagic));
    assert_eq!(decode_reading(&out[..4]), Err(Errno::BadMagic));
}

#[test]
fn the_server_answers_the_sensor_or_the_frame_s_refusal() {
    let mut out = [0u8; SENSOR_REPLY_LEN];
    let len = serve_request(&mut Fixed(Ok(Millicelsius(48_000))), &request(), &mut out).unwrap();
    assert_eq!(decode_reading(&out[..len]), Ok(Millicelsius(48_000)));
    let len = serve_request(&mut Fixed(Err(Errno::NoData)), &request(), &mut out).unwrap();
    assert_eq!(decode_reading(&out[..len]), Err(Errno::NoData));
    let len = serve_request(&mut Fixed(Ok(Millicelsius(1))), &[0u8; 3], &mut out).unwrap();
    assert_eq!(decode_reading(&out[..len]), Err(Errno::LengthOutOfRange));
}

#[test]
fn the_bcm2711_calibration_converts_its_documented_codes() {
    assert_eq!(BCM2711.convert(0), Some(Millicelsius(410_040)));
    // 742 counts is about 48.7 degrees.
    assert_eq!(BCM2711.convert(742), Some(Millicelsius(48_686)));
    assert_eq!(BCM2711.convert(0x3FF), Some(Millicelsius(-88_161)));
    let steep = ThermalCalibration {
        slope: i32::MAX,
        offset: i32::MAX,
    };
    assert_eq!(steep.convert(u32::MAX), None, "past an i32 is refused");
}

#[test]
fn a_calibration_survives_its_property_word() {
    for calibration in [
        BCM2711,
        ThermalCalibration {
            slope: i32::MIN,
            offset: -1,
        },
        ThermalCalibration {
            slope: 1,
            offset: i32::MAX,
        },
    ] {
        assert_eq!(
            ThermalCalibration::from_word(calibration.to_word()),
            calibration
        );
    }
}

#[test]
fn tenths_round_half_away_from_zero() {
    assert_eq!(Millicelsius(48_686).tenths(), 487);
    assert_eq!(Millicelsius(48_650).tenths(), 487);
    assert_eq!(Millicelsius(48_649).tenths(), 486);
    assert_eq!(Millicelsius(-1_050).tenths(), -11);
    assert_eq!(Millicelsius::from_celsius(65), Millicelsius(65_000));
}

#[test]
fn the_zone_survives_its_property_value_and_unknown_zones_are_refused() {
    assert_eq!(
        ThermalZone::from_u64(ThermalZone::Processor.as_u64()),
        Some(ThermalZone::Processor)
    );
    assert_eq!(ThermalZone::from_u64(0), None);
    assert_eq!(ThermalZone::from_u64(2), None);
}

#[test]
fn the_sensor_block_is_reserved_and_a_device_channel() {
    assert!(NodeEndpointBlock::ALL.contains(&THERMAL_SENSOR_ENDPOINTS));
    assert_eq!(
        NodeEndpointBlock::device_channel_node(THERMAL_SENSOR_ENDPOINTS.endpoint(7)),
        Some(7)
    );
}
