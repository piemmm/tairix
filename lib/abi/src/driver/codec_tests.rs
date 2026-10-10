use super::*;

#[test]
fn a_link_round_trips_through_its_selector_cells() {
    for format in [
        DaiFormat::I2s,
        DaiFormat::LeftJustified,
        DaiFormat::RightJustified,
        DaiFormat::DspA,
        DaiFormat::DspB,
    ] {
        for (bit, frame) in [(false, false), (true, false), (false, true), (true, true)] {
            for inversion in [
                ClockInversion::Normal,
                ClockInversion::BitClock,
                ClockInversion::FrameClock,
                ClockInversion::Both,
            ] {
                let link = DaiLink {
                    format,
                    codec_drives_bit_clock: bit,
                    codec_drives_frame_clock: frame,
                    inversion,
                    cpu_dai: 3,
                    codec_dai: 0x0001_0002,
                };
                assert_eq!(DaiLink::from_cells(&link.to_cells()), Ok(link));
            }
        }
    }
}

#[test]
fn a_selector_no_link_produces_is_refused() {
    let good = DaiLink {
        format: DaiFormat::I2s,
        codec_drives_bit_clock: false,
        codec_drives_frame_clock: false,
        inversion: ClockInversion::Normal,
        cpu_dai: 0,
        codec_dai: 0,
    }
    .to_cells();
    assert_eq!(DaiLink::from_cells(&good[..1]), Err(Errno::BadMagic));
    assert_eq!(DaiLink::from_cells(&[good[0], 0, 0]), Err(Errno::BadMagic));
    assert_eq!(
        DaiLink::from_cells(&[0, 0]),
        Err(Errno::BadMagic),
        "no format"
    );
    assert_eq!(
        DaiLink::from_cells(&[6, 0]),
        Err(Errno::BadMagic),
        "unknown format"
    );
    assert_eq!(
        DaiLink::from_cells(&[good[0] | 1 << 12, 0]),
        Err(Errno::BadMagic),
        "an undefined bit"
    );
}

#[test]
fn the_binding_names_its_formats() {
    assert_eq!(DaiFormat::from_binding(b"i2s"), Some(DaiFormat::I2s));
    assert_eq!(
        DaiFormat::from_binding(b"left_j"),
        Some(DaiFormat::LeftJustified)
    );
    assert_eq!(
        DaiFormat::from_binding(b"right_j"),
        Some(DaiFormat::RightJustified)
    );
    assert_eq!(DaiFormat::from_binding(b"dsp_a"), Some(DaiFormat::DspA));
    assert_eq!(DaiFormat::from_binding(b"dsp_b"), Some(DaiFormat::DspB));
    assert_eq!(DaiFormat::from_binding(b"ac97"), None);
}

#[test]
fn each_refusal_a_codec_driver_makes_reaches_its_caller_as_itself() {
    for err in [
        DriverError::Unsupported,
        DriverError::Busy,
        DriverError::NotImplemented,
        DriverError::DeviceFault,
        DriverError::OutOfRange,
        DriverError::PermissionDenied,
    ] {
        assert_eq!(refusal(refusal_reason(err)), err, "{err:?}");
    }
    assert_eq!(
        refusal(Errno::NotFound),
        DriverError::DeviceFault,
        "a codec gone is a codec failing"
    );
}

mod protocol {
    use super::super::*;
    use crate::driver::audio::{GainRange, Rate, RateSet, RateSupport};
    use crate::driver::clock::CLOCK_CONTROLLER_ENDPOINTS;
    use crate::hwlink::LinkRequest;
    use crate::Errno;

    fn link() -> LinkRequest {
        let cells = DaiLink {
            format: DaiFormat::I2s,
            codec_drives_bit_clock: false,
            codec_drives_frame_clock: false,
            inversion: ClockInversion::Normal,
            cpu_dai: 0,
            codec_dai: 0,
        }
        .to_cells();
        LinkRequest::new(CODEC_ENDPOINTS.endpoint(40), 0, &cells, b"").expect("valid")
    }

    fn rate(hz: u32) -> Rate {
        Rate::new(hz).expect("a rate")
    }

    /// The request's frame, with room for one byte past it, and its length.
    fn encoded(request: &CodecRequest) -> ([u8; CODEC_MAX_REQUEST + 1], usize) {
        let mut out = [0u8; CODEC_MAX_REQUEST + 1];
        let len = request.encode(&mut out).expect("fits");
        (out, len)
    }

    #[test]
    fn every_request_round_trips_at_exactly_its_length() {
        for request in [
            CodecRequest::Describe(link()),
            CodecRequest::Configure {
                link: link(),
                rate: rate(48_000),
                width: 24,
            },
            CodecRequest::Gain {
                link: link(),
                millibel: -650,
                mute: true,
            },
            CodecRequest::Start(link()),
            CodecRequest::Stop(link()),
        ] {
            let (frame, len) = encoded(&request);
            assert_eq!(CodecRequest::decode(&frame[..len]), Ok(request));
            assert_eq!(
                CodecRequest::decode(&frame[..=len]),
                Err(Errno::LengthOutOfRange)
            );
            assert_eq!(
                CodecRequest::decode(&frame[..len - 1]),
                Err(Errno::LengthOutOfRange)
            );
        }
    }

    #[test]
    fn a_frame_that_is_not_a_canonical_codec_request_is_refused() {
        let configure = CodecRequest::Configure {
            link: link(),
            rate: rate(48_000),
            width: 24,
        };
        let (good, len) = encoded(&configure);
        let argument = 8 + crate::hwtree::HwResource::WIRE_LEN;
        let altered = |at: usize, byte: u8| {
            let mut frame = good;
            frame[at] = byte;
            CodecRequest::decode(&frame[..len])
        };
        assert_eq!(altered(0, b'X'), Err(Errno::BadMagic));
        assert_eq!(altered(4, 2), Err(Errno::AbiVersionUnsupported));
        assert_eq!(altered(6, 9), Err(Errno::OutOfRange));
        assert_eq!(altered(7, 1), Err(Errno::BadMagic), "the reserved byte");
        assert_eq!(altered(argument + 4, 18), Err(Errno::BadMagic), "a width");
        assert_eq!(altered(argument + 5, 1), Err(Errno::BadMagic), "reserved");
        assert_eq!(altered(argument + 3, 0xFF), Err(Errno::BadMagic), "a rate");
        let (gain, gain_len) = encoded(&CodecRequest::Gain {
            link: link(),
            millibel: 0,
            mute: false,
        });
        let mut frame = gain;
        frame[argument + 4] = 2;
        assert_eq!(
            CodecRequest::decode(&frame[..gain_len]),
            Err(Errno::BadMagic),
            "a mute that is neither"
        );
        let clock =
            LinkRequest::new(CLOCK_CONTROLLER_ENDPOINTS.endpoint(8), 0, &[30], b"").expect("valid");
        let (other, other_len) = encoded(&CodecRequest::Describe(clock));
        assert_eq!(
            CodecRequest::decode(&other[..other_len]),
            Err(Errno::BadMagic),
            "a link of another role"
        );
    }

    fn facts(gain: Option<GainRange>) -> CodecFacts {
        CodecFacts {
            rates: RateSupport::Discrete(
                RateSet::new(&[rate(44_100), rate(48_000), rate(96_000)]).expect("rates"),
            ),
            widths: SampleWidths::EMPTY
                .with(16)
                .and_then(|w| w.with(24))
                .and_then(|w| w.with(32))
                .expect("widths"),
            formats: DaiFormats::EMPTY
                .with(DaiFormat::I2s)
                .with(DaiFormat::LeftJustified),
            drives_clocks: false,
            gain,
        }
    }

    #[test]
    fn facts_round_trip_with_and_without_a_gain_control() {
        let mut out = [0u8; CODEC_MAX_REPLY];
        for gain in [
            None,
            Some(GainRange::new(-10_300, 2_400, 50).expect("range")),
        ] {
            let len = encode_describe_reply(&mut out, &facts(gain)).expect("fits");
            assert_eq!(decode_describe_reply(&out[..len]), Ok(facts(gain)));
        }
        let facts = facts(None);
        assert!(facts.widths.contains(24) && !facts.widths.contains(20));
        assert!(facts.formats.contains(DaiFormat::LeftJustified));
        assert!(!facts.formats.contains(DaiFormat::DspA));
    }

    #[test]
    fn a_facts_reply_with_an_empty_set_or_an_undefined_bit_is_refused() {
        let mut out = [0u8; CODEC_MAX_REPLY];
        let len = encode_describe_reply(&mut out, &facts(None)).expect("fits");
        let flags = 8 + crate::driver::audio::RATE_SUPPORT_WIRE_LEN;
        for (at, byte) in [
            (flags, 0),
            (flags, 0x10),
            (flags + 1, 0),
            (flags + 1, 1),
            (flags + 2, 2),
            (flags + 3, 1),
        ] {
            let mut frame = out;
            frame[at] = byte;
            assert_eq!(
                decode_describe_reply(&frame[..len]),
                Err(Errno::BadMagic),
                "{at} {byte}"
            );
        }
    }

    #[test]
    fn gain_and_done_replies_round_trip_and_a_refusal_carries_its_reason() {
        let mut out = [0u8; CODEC_MAX_REPLY];
        let len = encode_gain_reply(&mut out, -1_250).expect("fits");
        assert_eq!(decode_gain_reply(&out[..len]), Ok(-1_250));
        let len = encode_done_reply(&mut out).expect("fits");
        assert_eq!(decode_done_reply(&out[..len]), Ok(()));
        let len = encode_error_reply(&mut out, Errno::NotSupported).expect("fits");
        assert_eq!(decode_done_reply(&out[..len]), Err(Errno::NotSupported));
        assert_eq!(decode_gain_reply(&out[..len]), Err(Errno::NotSupported));
        assert_eq!(decode_describe_reply(&out[..len]), Err(Errno::NotSupported));
    }
}
