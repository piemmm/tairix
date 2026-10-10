use tairix_abi::driver::audio::{ChannelPosition, STANDARD_RATES};
use tairix_usb::periodic::IsoSync;

use super::*;
use crate::fixtures::{
    configuration, endpoint_v1, headset_v2, implicit_v2, interface, qemu_usb_audio,
};

fn rate(hz: u32) -> Rate {
    Rate::new(hz).expect("a valid rate")
}

#[test]
fn qemus_stream_is_48_khz_s16_stereo_on_a_synchronous_out_endpoint() {
    let config = qemu_usb_audio();
    let topology = Topology::parse(&config, 0).expect("parses");
    let streams = streaming_interfaces(&config, &topology, 0).expect("parses");
    assert_eq!(streams.len(), 1);
    let stream = &streams[0];
    assert_eq!(stream.interface, 1);
    assert_eq!(stream.direction, StreamDirection::Playback);
    assert_eq!(stream.terminal, 1);
    assert_eq!(stream.formats.len(), 1);
    let format = &stream.formats[0];
    assert_eq!(format.alternate, 1);
    assert_eq!(format.format, SampleFormat::S16);
    assert_eq!(format.channel_map, ChannelMap::STEREO);
    assert_eq!(format.frame_bytes(), 4);
    assert_eq!(
        format.rates,
        AltRates::Listed(RateSupport::Discrete(
            RateSet::new(&[Rate::HZ_48000]).expect("one rate")
        ))
    );
    assert_eq!(format.data.address, 0x01);
    assert_eq!(format.data.max_packet, 192);
    assert_eq!(
        format.data.kind,
        TransferKind::Isochronous {
            sync: IsoSync::Synchronous,
            usage: IsoUsage::Data
        }
    );
    assert_eq!(format.feedback, None);
    assert!(
        !format.frequency_control,
        "QEMU states no frequency control"
    );
}

#[test]
fn a_version_two_headset_has_a_fed_back_playback_and_a_mono_capture() {
    let config = headset_v2();
    let topology = Topology::parse(&config, 0).expect("parses");
    let streams = streaming_interfaces(&config, &topology, 0).expect("parses");
    assert_eq!(streams.len(), 2);
    let playback = &streams[0];
    assert_eq!(playback.direction, StreamDirection::Playback);
    let format = &playback.formats[0];
    assert_eq!(format.format, SampleFormat::S24);
    assert_eq!(format.channel_map, ChannelMap::STEREO);
    assert_eq!(format.rates, AltRates::Clock);
    assert_eq!(format.data.address, 0x01);
    assert_eq!(format.feedback.map(|feedback| feedback.address), Some(0x81));
    let capture = &streams[1];
    assert_eq!(capture.interface, 2);
    assert_eq!(capture.direction, StreamDirection::Capture);
    assert_eq!(capture.terminal, 6);
    let format = &capture.formats[0];
    assert_eq!(format.format, SampleFormat::S16);
    assert_eq!(format.channel_map, ChannelMap::MONO);
    assert_eq!(format.feedback, None, "capture is never paced by feedback");
}

#[test]
fn an_implicit_feedback_capture_endpoint_is_still_its_interfaces_data() {
    let config = implicit_v2();
    let topology = Topology::parse(&config, 0).expect("parses");
    let streams = streaming_interfaces(&config, &topology, 0).expect("parses");
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].formats[0].feedback, None);
    assert_eq!(
        streams[1].formats[0].data.kind,
        TransferKind::Isochronous {
            sync: IsoSync::Asynchronous,
            usage: IsoUsage::ImplicitFeedbackData
        }
    );
}

/// A 1.0 function with QEMU's control interface and one streaming interface
/// whose only setting carries `general`, `format` and `endpoints`.
fn v1_stream(general: &[u8], format: &[u8], endpoints: &[&[u8]]) -> Vec<u8> {
    let qemu = qemu_usb_audio();
    // QEMU's control interface: everything before its streaming interface.
    let control = &qemu[9..9 + 9 + 9 + 12 + 13 + 9];
    let zero = interface(1, 0, 0, 0x01_02_00);
    let one = interface(
        1,
        1,
        u8::try_from(endpoints.len()).expect("few"),
        0x01_02_00,
    );
    let mut body: Vec<&[u8]> = alloc::vec![control, &zero, &one, general, format];
    body.extend_from_slice(endpoints);
    configuration(&body)
}

fn v1_formats(config: &[u8]) -> Vec<AltFormat> {
    let topology = Topology::parse(config, 0).expect("parses");
    streaming_interfaces(config, &topology, 0)
        .expect("parses")
        .into_iter()
        .flat_map(|stream| stream.formats)
        .collect()
}

const PCM: [u8; 7] = [7, 0x24, 0x01, 0x01, 0x00, 0x01, 0x00];

#[test]
fn a_version_one_sync_endpoint_named_by_the_data_endpoint_is_its_feedback() {
    // The sync endpoint comes first and states data usage, as 1.0 devices do;
    // the data endpoint names it.
    let config = v1_stream(
        &PCM,
        &[
            11, 0x24, 0x02, 0x01, 0x02, 0x02, 0x10, 0x01, 0x44, 0xAC, 0x00,
        ],
        &[
            &endpoint_v1(0x82, 0x01, 3, 1, 0),
            &endpoint_v1(0x01, 0x05, 196, 1, 0x82),
        ],
    );
    let formats = v1_formats(&config);
    assert_eq!(formats.len(), 1);
    assert_eq!(formats[0].data.address, 0x01);
    assert_eq!(
        formats[0].feedback.map(|feedback| feedback.address),
        Some(0x82)
    );
    assert_eq!(
        formats[0].rates,
        AltRates::Listed(RateSupport::Discrete(
            RateSet::new(&[rate(44_100)]).expect("one rate")
        ))
    );
}

#[test]
fn version_one_rates_are_a_sorted_list_or_a_range() {
    // Three rates, unsorted and one repeated, with frequency control stated.
    let listed = [
        17, 0x24, 0x02, 0x01, 0x02, 0x02, 0x10, 0x03, 0x80, 0xBB, 0x00, 0x44, 0xAC, 0x00, 0x80,
        0xBB, 0x00,
    ];
    let control = [7, 0x25, 0x01, 0x01, 0x00, 0x00, 0x00];
    let config = v1_stream(
        &PCM,
        &listed,
        &[&endpoint_v1(0x01, 0x09, 192, 1, 0), &control],
    );
    let formats = v1_formats(&config);
    assert_eq!(
        formats[0].rates,
        AltRates::Listed(RateSupport::Discrete(
            RateSet::new(&[rate(44_100), Rate::HZ_48000]).expect("two rates")
        ))
    );
    assert!(formats[0].frequency_control);
    let range = [
        14, 0x24, 0x02, 0x01, 0x02, 0x02, 0x10, 0x00, 0x40, 0x1F, 0x00, 0x00, 0x77, 0x01,
    ];
    let config = v1_stream(&PCM, &range, &[&endpoint_v1(0x01, 0x09, 384, 1, 0)]);
    assert_eq!(
        v1_formats(&config)[0].rates,
        AltRates::Listed(RateSupport::Continuous {
            min: rate(8_000),
            max: rate(96_000)
        })
    );
}

#[test]
fn a_setting_the_vocabulary_cannot_carry_exactly_is_left_out() {
    let endpoint = endpoint_v1(0x01, 0x09, 192, 1, 0);
    let format = |subframe: u8, bits: u8| {
        [
            11, 0x24, 0x02, 0x01, 0x02, subframe, bits, 0x01, 0x80, 0xBB, 0x00,
        ]
    };
    // Signed 8-bit PCM, more bits than the subframe holds, A-law, and a rate
    // no converter runs at.
    for (general, format) in [
        (PCM, format(1, 8)),
        (PCM, format(2, 17)),
        ([7, 0x24, 0x01, 0x01, 0x00, 0x04, 0x00], format(1, 8)),
        (
            PCM,
            [
                11, 0x24, 0x02, 0x01, 0x02, 0x02, 0x10, 0x01, 0x10, 0x00, 0x00,
            ],
        ),
    ] {
        let config = v1_stream(&general, &format, &[&endpoint]);
        assert!(v1_formats(&config).is_empty());
    }
    // Unsigned 8-bit and float are carried.
    let config = v1_stream(
        &[7, 0x24, 0x01, 0x01, 0x00, 0x02, 0x00],
        &format(1, 8),
        &[&endpoint],
    );
    assert_eq!(v1_formats(&config)[0].format, SampleFormat::U8);
    let config = v1_stream(
        &[7, 0x24, 0x01, 0x01, 0x00, 0x03, 0x00],
        &format(4, 32),
        &[&endpoint],
    );
    assert_eq!(v1_formats(&config)[0].format, SampleFormat::F32);
}

#[test]
fn a_24_bit_sample_in_a_4_byte_slot_is_a_32_bit_sample() {
    assert_eq!(sample_format(Encoding::Pcm, 4, 24), Some(SampleFormat::S32));
    assert_eq!(sample_format(Encoding::Pcm, 3, 24), Some(SampleFormat::S24));
    assert_eq!(sample_format(Encoding::Pcm, 2, 12), Some(SampleFormat::S16));
    assert_eq!(sample_format(Encoding::Float, 4, 24), None);
    assert_eq!(sample_format(Encoding::Pcm, 2, 0), None);
}

#[test]
fn channel_maps_follow_the_stated_positions_or_the_conventional_reading() {
    let map = |channels, config| channel_map(Cluster { channels, config });
    assert_eq!(map(1, 0x4), Some(ChannelMap::MONO));
    assert_eq!(map(2, 0x3), Some(ChannelMap::STEREO));
    assert_eq!(map(2, 0), Some(ChannelMap::STEREO));
    assert_eq!(map(6, 0x3F), ChannelMap::conventional(6));
    assert_eq!(map(8, 0x63F), ChannelMap::conventional(8));
    let quad = map(4, 0x33).expect("front and rear pairs");
    assert_eq!(
        quad.positions(),
        &[
            ChannelPosition::FrontLeft,
            ChannelPosition::FrontRight,
            ChannelPosition::RearLeft,
            ChannelPosition::RearRight
        ]
    );
    // Left of centre has no word; positions for the wrong count describe
    // nothing; five channels with none stated have no conventional reading.
    assert_eq!(map(2, 0x40 | 0x1), None);
    assert_eq!(map(3, 0x3), None);
    assert_eq!(map(5, 0), None);
    assert_eq!(map(0, 0x3), None);
}

#[test]
fn a_version_two_function_no_association_groups_is_refused() {
    let mut config = headset_v2();
    // Turn the association into an unknown descriptor type.
    config[10] = 0x7F;
    let topology = Topology::parse(&config, 0).expect("the control interface still parses");
    assert_eq!(
        streaming_interfaces(&config, &topology, 0),
        Err(DriverError::BadMagic)
    );
}

#[test]
fn the_standard_rates_include_the_rates_qemu_and_cds_run_at() {
    assert!(STANDARD_RATES.contains(&Rate::HZ_48000));
    assert!(STANDARD_RATES.contains(&rate(44_100)));
}
