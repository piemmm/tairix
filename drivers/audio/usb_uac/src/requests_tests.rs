use super::*;

#[test]
fn an_entity_request_names_its_control_channel_entity_and_interface() {
    // GET_MIN of channel 1's volume on feature unit 2 of interface 0.
    assert_eq!(
        entity(Direction::In, v1::GET_MIN, selector::VOLUME, 1, 2, 0, 2),
        [0xA1, 0x82, 0x01, 0x02, 0x00, 0x02, 0x02, 0x00]
    );
    // A 2.0 SET CUR of clock 0x29's frequency through interface 3.
    assert_eq!(
        entity(
            Direction::Out,
            v2::CUR,
            selector::CLOCK_FREQUENCY,
            0,
            0x29,
            3,
            FREQUENCY_V2_LEN
        ),
        [0x21, 0x01, 0x00, 0x01, 0x03, 0x29, 0x04, 0x00]
    );
}

#[test]
fn an_endpoint_request_names_the_endpoint_address() {
    assert_eq!(
        endpoint(
            Direction::Out,
            v1::SET_CUR,
            selector::SAMPLING_FREQUENCY,
            0x01,
            FREQUENCY_V1_LEN
        ),
        [0x22, 0x01, 0x00, 0x01, 0x01, 0x00, 0x03, 0x00]
    );
}

#[test]
fn frequencies_round_trip_through_their_wire_widths() {
    assert_eq!(frequency_v1(48_000), [0x80, 0xBB, 0x00]);
    assert_eq!(read_frequency_v1(&[0x44, 0xAC, 0x00]), Some(44_100));
    assert_eq!(read_frequency_v1(&[0x44, 0xAC]), None);
    assert_eq!(read_u32(&192_000u32.to_le_bytes()), Some(192_000));
}

#[test]
fn range_blocks_answer_each_subrange_and_nothing_past_them() {
    let mut block = alloc::vec![2u8, 0];
    for (min, max, res) in [(44_100u32, 44_100u32, 0u32), (8_000, 96_000, 8_000)] {
        block.extend_from_slice(&min.to_le_bytes());
        block.extend_from_slice(&max.to_le_bytes());
        block.extend_from_slice(&res.to_le_bytes());
    }
    assert_eq!(block.len(), usize::from(range_len(2, 4)));
    assert_eq!(
        subrange_u32(&block, 1),
        Some(Subrange {
            min: 8_000,
            max: 96_000,
            res: 8_000
        })
    );
    assert_eq!(subrange_u32(&block, 2), None, "past its count");
    assert_eq!(subrange_u32(&block[..20], 1), None, "past its bytes");
    let volume = [1u8, 0, 0x01, 0x80, 0x00, 0x08, 0x88, 0x00];
    assert_eq!(
        subrange_i16(&volume, 0),
        Some(Subrange {
            min: -32767,
            max: 0x0800,
            res: 0x0088
        })
    );
}

#[test]
fn volumes_convert_to_millibel_rounded_down() {
    assert_eq!(millibel(0), 0);
    assert_eq!(millibel(256), 100);
    assert_eq!(millibel(0x0800), 800);
    assert_eq!(millibel(-32767), -12800);
    assert_eq!(millibel(128), 50);
    assert_eq!(millibel(-100), -40, "-0.39 dB");
    assert_eq!(millibel(1), 0);
}

#[test]
fn both_ends_of_a_reported_range_are_levels_the_device_reaches() {
    for (min, max, res) in [
        (-32767i16, 0x0800i16, 0x0088i16),
        (-100, 100, 3),
        (-12345, -100, 7),
        (-1, 1, 1),
    ] {
        assert_eq!(
            volume_at_least(millibel(min), min, max, res),
            min,
            "the device's own minimum, not a step above it"
        );
        let top = volume_at_least(millibel(max), min, max, res);
        assert!(top <= max && millibel(top) >= millibel(max), "{top}");
    }
}

#[test]
fn a_volume_is_rounded_to_the_device_step_above() {
    // QEMU's control: -127.996 dB to +8 dB in steps of 0.53 dB.
    let (min, max, res) = (-32767, 0x0800, 0x0088);
    // 0 dB is not on the grid from the minimum; the step above it is.
    let zero = volume_at_least(0, min, max, res);
    assert!(zero >= 0 && zero < res, "{zero}");
    assert_eq!((i32::from(zero) - i32::from(min)) % i32::from(res), 0);
    assert_eq!(
        volume_at_least(10_000, min, max, res),
        max,
        "held to the top"
    );
    assert_eq!(
        volume_at_least(-20_000, min, max, res),
        min,
        "held to the bottom"
    );
    // A resolution of zero steps one unit at a time: 0.1 dB is 25.6/256 dB,
    // so the step above is 26.
    assert_eq!(volume_at_least(10, -100, 100, 0), 26);
}
