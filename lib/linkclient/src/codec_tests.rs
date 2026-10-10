//! The codec client driven against a codec that decodes every frame.

extern crate std;

use std::vec::Vec;

use tairix_abi::driver::audio::{Rate, RateSupport};
use tairix_abi::driver::codec::{
    encode_describe_reply, encode_done_reply, encode_error_reply, encode_gain_reply,
    refusal_reason, ClockInversion, CodecFacts, CodecRequest, DaiFormat, DaiFormats, DaiLink,
    SampleWidths, CODEC_ENDPOINTS,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::{DriverError, Errno};

use crate::{CodecClient, LinkCall};

const DAI: DaiLink = DaiLink {
    format: DaiFormat::LeftJustified,
    codec_drives_bit_clock: true,
    codec_drives_frame_clock: false,
    inversion: ClockInversion::FrameClock,
    cpu_dai: 0,
    codec_dai: 1,
};

fn link() -> LinkRequest {
    LinkRequest::new(CODEC_ENDPOINTS.endpoint(40), 0, &DAI.to_cells(), b"").expect("valid")
}

fn rate() -> Rate {
    Rate::new(48_000).expect("a rate")
}

fn facts() -> CodecFacts {
    CodecFacts {
        rates: RateSupport::Continuous {
            min: Rate::new(8_000).expect("a rate"),
            max: Rate::new(192_000).expect("a rate"),
        },
        widths: SampleWidths::EMPTY.with(32).expect("a width"),
        formats: DaiFormats::EMPTY.with(DaiFormat::LeftJustified),
        drives_clocks: false,
        gain: None,
    }
}

/// A codec, its gain in half-decibel steps when it has one, refusing what
/// its test says.
#[derive(Default)]
struct Part {
    requests: Vec<CodecRequest>,
    refuse: Option<DriverError>,
    has_gain: bool,
}

impl LinkCall for &mut Part {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let decoded = CodecRequest::decode(request).expect("a canonical frame");
        self.requests.push(decoded);
        if let Some(err) = self.refuse {
            return encode_error_reply(reply, refusal_reason(err));
        }
        match decoded {
            CodecRequest::Describe(_) => encode_describe_reply(reply, &facts()),
            CodecRequest::Gain { millibel, .. } if self.has_gain => {
                encode_gain_reply(reply, -(-millibel).div_euclid(50) * 50)
            }
            CodecRequest::Gain { .. } => {
                encode_error_reply(reply, refusal_reason(DriverError::NotImplemented))
            }
            CodecRequest::Configure { .. } | CodecRequest::Start(_) | CodecRequest::Stop(_) => {
                encode_done_reply(reply)
            }
        }
    }

    fn post(&mut self, _request: &[u8], _deadline_ns: u64) -> Result<u64, Errno> {
        panic!("no codec request is posted")
    }

    fn reap(&mut self, _ticket: u64, _reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        panic!("no codec request is posted")
    }
}

#[test]
fn every_request_names_the_link_and_a_missing_gain_is_told_apart_from_a_fault() {
    let mut part = Part::default();
    let mut codec = CodecClient::new(&mut part, link()).expect("a codec link");
    assert_eq!(codec.dai(), &DAI);
    assert_eq!(codec.describe(), Ok(facts()));
    assert_eq!(codec.configure(rate(), 32), Ok(()));
    assert_eq!(codec.gain(-600, false), Err(DriverError::NotImplemented));
    assert_eq!(codec.start(), Ok(()));
    assert_eq!(codec.stop(), Ok(()));
    assert_eq!(
        part.requests,
        [
            CodecRequest::Describe(link()),
            CodecRequest::Configure {
                link: link(),
                rate: rate(),
                width: 32,
            },
            CodecRequest::Gain {
                link: link(),
                millibel: -600,
                mute: false,
            },
            CodecRequest::Start(link()),
            CodecRequest::Stop(link()),
        ]
    );
}

#[test]
fn a_codec_with_gain_answers_the_step_it_set() {
    let mut part = Part {
        has_gain: true,
        ..Part::default()
    };
    let mut codec = CodecClient::new(&mut part, link()).expect("a codec link");
    assert_eq!(codec.gain(-625, true), Ok(-600));
}

#[test]
fn a_held_codec_and_a_framing_it_refuses_come_back_as_themselves() {
    for err in [
        DriverError::Busy,
        DriverError::Unsupported,
        DriverError::DeviceFault,
    ] {
        let mut part = Part {
            refuse: Some(err),
            ..Part::default()
        };
        let mut codec = CodecClient::new(&mut part, link()).expect("a codec link");
        assert_eq!(codec.configure(rate(), 32), Err(err));
    }
}

#[test]
fn a_link_whose_selector_is_no_dai_link_is_refused() {
    let mut part = Part::default();
    let other = LinkRequest::new(CODEC_ENDPOINTS.endpoint(40), 0, &[1], b"").expect("valid");
    assert!(matches!(
        CodecClient::new(&mut part, other),
        Err(Errno::BadMagic)
    ));
}
