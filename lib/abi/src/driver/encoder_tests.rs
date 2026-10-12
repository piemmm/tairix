use super::*;
use crate::driver::display::{Axis, Polarity, Scan};
use crate::driver::i2c::I2C_CONTROLLER_ENDPOINTS;

fn link() -> LinkRequest {
    LinkRequest::new(ENCODER_ENDPOINTS.endpoint(9), 0, &[], b"hdmi0").expect("valid")
}

const MODE: Timing = Timing {
    clock_khz: 148_500,
    horizontal: Axis {
        active: 1920,
        front: 88,
        sync: 44,
        back: 148,
    },
    vertical: Axis {
        active: 1080,
        front: 4,
        sync: 5,
        back: 36,
    },
    hsync: Polarity::Positive,
    vsync: Polarity::Positive,
    scan: Scan::Progressive,
    repetition: 1,
};

const PIPE: PipeLimits = PipeLimits {
    max_clock_khz: 600_000,
    max_active_khz: 542_000,
};

fn every_request() -> [EncoderRequest; 8] {
    let link = link();
    [
        EncoderRequest::Describe(link),
        EncoderRequest::Wait { link, seen: 41 },
        EncoderRequest::Configure { link, timing: MODE },
        EncoderRequest::Prepare(link),
        EncoderRequest::Enable(link),
        EncoderRequest::Disable(link),
        EncoderRequest::PowerDown(link),
        EncoderRequest::Limit { link, limits: PIPE },
    ]
}

fn encoded(request: &EncoderRequest) -> ([u8; ENCODER_MAX_REQUEST], usize) {
    let mut frame = [0u8; ENCODER_MAX_REQUEST];
    let len = request.encode(&mut frame).expect("fits");
    (frame, len)
}

#[test]
fn every_request_round_trips() {
    for request in every_request() {
        let (frame, len) = encoded(&request);
        assert_eq!(EncoderRequest::decode(&frame[..len]), Ok(request));
    }
}

#[test]
fn a_malformed_request_is_refused() {
    let (good, len) = encoded(&EncoderRequest::Describe(link()));
    let mut magic = good;
    magic[0] ^= 1;
    assert_eq!(EncoderRequest::decode(&magic[..len]), Err(Errno::BadMagic));
    let mut reserved = good;
    reserved[7] = 1;
    assert_eq!(
        EncoderRequest::decode(&reserved[..len]),
        Err(Errno::BadMagic)
    );
    let mut version = good;
    version[4] = 9;
    assert_eq!(
        EncoderRequest::decode(&version[..len]),
        Err(Errno::AbiVersionUnsupported)
    );
    let mut op = good;
    op[6] = 99;
    assert_eq!(EncoderRequest::decode(&op[..len]), Err(Errno::OutOfRange));
    assert_eq!(
        EncoderRequest::decode(&good[..len - 1]),
        Err(Errno::LengthOutOfRange)
    );
    // A link of another role is not authority over an encoder.
    let foreign =
        LinkRequest::new(I2C_CONTROLLER_ENDPOINTS.endpoint(9), 0, &[0x50], b"").expect("valid");
    let mut quoted = good;
    quoted[HEADER_LEN..LINKED_LEN].copy_from_slice(&HwResource::request(&foreign).to_le_bytes());
    assert_eq!(EncoderRequest::decode(&quoted[..len]), Err(Errno::BadMagic));
}

#[test]
fn a_pipe_that_carries_nothing_is_refused() {
    for limits in [
        PipeLimits {
            max_clock_khz: 0,
            ..PIPE
        },
        PipeLimits {
            max_active_khz: 0,
            ..PIPE
        },
    ] {
        let (frame, len) = encoded(&EncoderRequest::Limit {
            link: link(),
            limits,
        });
        assert_eq!(
            EncoderRequest::decode(&frame[..len]),
            Err(Errno::OutOfRange)
        );
    }
}

/// 3840x2160 at 60 Hz clocks 594 MHz over 4400-pixel lines, so its active
/// pixels pass at 518.4 MHz; the 4096-pixel-wide mode at the same clock
/// passes them at 552.96 MHz.
#[test]
fn a_pipe_carries_a_mode_by_its_clock_and_its_active_rate() {
    let uhd = Timing {
        clock_khz: 594_000,
        horizontal: Axis {
            active: 3840,
            front: 176,
            sync: 88,
            back: 296,
        },
        vertical: Axis {
            active: 2160,
            front: 8,
            sync: 10,
            back: 72,
        },
        ..MODE
    };
    let dci = Timing {
        horizontal: Axis {
            active: 4096,
            front: 88,
            sync: 88,
            back: 128,
        },
        ..uhd
    };
    assert_eq!(uhd.active_rate_khz(), 518_400);
    assert_eq!(dci.active_rate_khz(), 552_960);
    assert!(PIPE.carries(&MODE));
    assert!(PIPE.carries(&uhd));
    assert!(
        !PIPE.carries(&dci),
        "its active pixels outpace the compositor"
    );
    let slow = PipeLimits {
        max_clock_khz: 340_000,
        max_active_khz: 340_000,
    };
    assert!(
        !slow.carries(&uhd),
        "its clock outpaces the timing generator"
    );
}

#[test]
fn a_configure_for_a_timing_nothing_could_clock_out_is_refused() {
    let (mut frame, len) = encoded(&EncoderRequest::Configure {
        link: link(),
        timing: MODE,
    });
    frame[LINKED_LEN..LINKED_LEN + 4].fill(0);
    assert_eq!(
        EncoderRequest::decode(&frame[..len]),
        Err(Errno::LengthOutOfRange)
    );
}

/// The order a mode set goes up and comes down in, and nothing else: an
/// engine cannot start an output its encoder has not prepared, nor power an
/// output down while it is still sending.
#[test]
fn the_phases_admit_only_the_order_of_a_mode_set() {
    use EncoderOp::{Configure, Describe, Disable, Enable, Limit, PowerDown, Prepare, Wait};
    use Phase::{Configured, Disabled, Enabled, Off, Prepared};
    let up = [(Off, Configure), (Configured, Prepare), (Prepared, Enable)];
    let down = [(Enabled, Disable), (Disabled, PowerDown)];
    for (from, op) in up.into_iter().chain(down) {
        assert!(from.after(op).is_some(), "{op:?} from {from:?}");
    }
    assert_eq!(Configured.after(PowerDown), Some(Off), "an abandoned set");
    assert_eq!(Prepared.after(PowerDown), Some(Off), "an abandoned set");
    assert_eq!(Off.after(PowerDown), Some(Off), "nothing powered");
    for (from, op) in [
        (Off, Prepare),
        (Off, Enable),
        (Off, Disable),
        (Configured, Configure),
        (Configured, Enable),
        (Prepared, Configure),
        (Enabled, Configure),
        (Enabled, Enable),
        (Enabled, PowerDown),
        (Disabled, Enable),
        (Disabled, Configure),
    ] {
        assert_eq!(from.after(op), None, "{op:?} from {from:?}");
    }
    for phase in [Off, Configured, Prepared, Enabled, Disabled] {
        assert_eq!(phase.after(Describe), Some(phase));
        assert_eq!(phase.after(Wait), Some(phase));
        assert_eq!(phase.after(Limit), Some(phase));
    }
}

fn state(connected: bool, mode: Option<Timing>, phase: Phase) -> EncoderState {
    EncoderState {
        moves: 7,
        connector: Connector {
            connected,
            mode,
            link: 3,
        },
        phase,
    }
}

fn state_frame(state: &EncoderState) -> [u8; ENCODER_MAX_REPLY] {
    let mut out = [0u8; ENCODER_MAX_REPLY];
    assert_eq!(encode_state_reply(&mut out, state), Ok(ENCODER_MAX_REPLY));
    out
}

#[test]
fn every_state_round_trips() {
    for state in [
        state(true, Some(MODE), Phase::Enabled),
        state(true, None, Phase::Off),
        state(false, None, Phase::Disabled),
    ] {
        assert_eq!(decode_state_reply(&state_frame(&state)), Ok(state));
    }
}

#[test]
fn the_link_s_generation_rides_beside_the_flags() {
    let frame = state_frame(&state(true, Some(MODE), Phase::Enabled));
    assert_eq!(
        frame[REPLY_HEADER_LEN + 12..REPLY_HEADER_LEN + 16],
        3u32.to_le_bytes()
    );
    let mut later = frame;
    later[REPLY_HEADER_LEN + 15] = 0x80;
    assert_eq!(
        decode_state_reply(&later).map(|state| state.connector.link),
        Ok(0x8000_0003)
    );
}

#[test]
fn a_state_that_contradicts_itself_is_refused() {
    let good = state_frame(&state(true, Some(MODE), Phase::Enabled));
    let body = REPLY_HEADER_LEN;
    let mut flag = good;
    flag[body + 8] |= 1 << 2;
    assert_eq!(decode_state_reply(&flag), Err(Errno::BadMagic));
    let mut unplugged_mode = good;
    unplugged_mode[body + 8] = HAS_MODE;
    assert_eq!(decode_state_reply(&unplugged_mode), Err(Errno::BadMagic));
    let mut phase = good;
    phase[body + 9] = 5;
    assert_eq!(decode_state_reply(&phase), Err(Errno::BadMagic));
    let mut reserved = good;
    reserved[body + 11] = 1;
    assert_eq!(decode_state_reply(&reserved), Err(Errno::BadMagic));
    let mut stray = state_frame(&state(true, None, Phase::Off));
    stray[body + 16] = 1;
    assert_eq!(decode_state_reply(&stray), Err(Errno::BadMagic));
    assert_eq!(
        decode_state_reply(&good[..ENCODER_MAX_REPLY - 1]),
        Err(Errno::BadMagic)
    );
}

#[test]
fn a_refusal_and_an_acknowledgement_round_trip() {
    let mut out = [0u8; ENCODER_MAX_REPLY];
    let len = encode_done_reply(&mut out).expect("fits");
    assert_eq!(decode_done_reply(&out[..len]), Ok(()));
    for err in [
        DriverError::Unsupported,
        DriverError::Busy,
        DriverError::Stale,
        DriverError::OutOfRange,
        DriverError::DeviceFault,
    ] {
        let len = encode_error_reply(&mut out, refusal_reason(err)).expect("fits");
        assert_eq!(
            decode_done_reply(&out[..len]).map_err(refusal),
            Err(err),
            "{err:?}"
        );
    }
}

/// Only the transport says an encoder is gone: a device's own `NotFound`
/// must not read as its driver having ended.
#[test]
fn only_the_transport_says_the_encoder_is_gone() {
    assert_eq!(refusal(Errno::NotFound), DriverError::NotFound);
    assert_eq!(
        refusal(refusal_reason(DriverError::NotFound)),
        DriverError::DeviceFault
    );
}
