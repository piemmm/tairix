use super::*;
use crate::driver::dmaengine::DMA_CONTROLLER_ENDPOINTS;

fn link() -> LinkRequest {
    LinkRequest::new(CLOCK_CONTROLLER_ENDPOINTS.endpoint(8), 0, &[0x1E], b"pwm").expect("valid")
}

/// The request's frame, with room for one byte past it, and its length.
fn encoded(request: &ClockRequest) -> ([u8; CLOCK_MAX_REQUEST + 1], usize) {
    let mut out = [0u8; CLOCK_MAX_REQUEST + 1];
    let len = request.encode(&mut out).expect("fits");
    (out, len)
}

#[test]
fn every_request_round_trips_at_exactly_its_length() {
    for request in [
        ClockRequest::Describe(link()),
        ClockRequest::Run {
            link: link(),
            hz: 100_000_000,
        },
        ClockRequest::Release(link()),
    ] {
        let (frame, len) = encoded(&request);
        assert_eq!(ClockRequest::decode(&frame[..len]), Ok(request));
        assert_eq!(
            ClockRequest::decode(&frame[..=len]),
            Err(Errno::LengthOutOfRange)
        );
        assert_eq!(
            ClockRequest::decode(&frame[..len - 1]),
            Err(Errno::LengthOutOfRange)
        );
    }
}

#[test]
fn a_frame_that_is_not_a_canonical_clock_request_is_refused() {
    let (good, len) = encoded(&ClockRequest::Describe(link()));
    let altered = |at: usize, byte: u8| {
        let mut frame = good;
        frame[at] = byte;
        ClockRequest::decode(&frame[..len])
    };
    assert_eq!(altered(0, b'X'), Err(Errno::BadMagic));
    assert_eq!(altered(4, 2), Err(Errno::AbiVersionUnsupported));
    assert_eq!(altered(6, 9), Err(Errno::OutOfRange));
    assert_eq!(altered(7, 1), Err(Errno::BadMagic), "the reserved byte");
    let dma =
        LinkRequest::new(DMA_CONTROLLER_ENDPOINTS.endpoint(8), 0, &[2], b"tx").expect("valid");
    let (other, other_len) = encoded(&ClockRequest::Describe(dma));
    assert_eq!(
        ClockRequest::decode(&other[..other_len]),
        Err(Errno::BadMagic),
        "a link of another role"
    );
    assert_eq!(
        ClockRequest::decode(&good[..4]),
        Err(Errno::LengthOutOfRange)
    );
}

#[test]
fn every_reply_round_trips_and_a_refusal_carries_its_reason() {
    let mut out = [0u8; CLOCK_MAX_REPLY];
    for state in [
        ClockState {
            hz: 0,
            held_elsewhere: false,
        },
        ClockState {
            hz: 3_072_000,
            held_elsewhere: true,
        },
    ] {
        let len = encode_describe_reply(&mut out, state).expect("fits");
        assert_eq!(decode_describe_reply(&out[..len]), Ok(state));
    }
    let len = encode_run_reply(&mut out, 99_999_744).expect("fits");
    assert_eq!(decode_run_reply(&out[..len]), Ok(99_999_744));
    let len = encode_release_reply(&mut out).expect("fits");
    assert_eq!(decode_release_reply(&out[..len]), Ok(()));
    let len = encode_error_reply(&mut out, Errno::Busy).expect("fits");
    assert_eq!(decode_run_reply(&out[..len]), Err(Errno::Busy));
    assert_eq!(decode_describe_reply(&out[..len]), Err(Errno::Busy));
}

#[test]
fn a_reply_with_an_undefined_bit_or_the_wrong_length_is_refused() {
    let mut out = [0u8; CLOCK_MAX_REPLY];
    let len = encode_describe_reply(
        &mut out,
        ClockState {
            hz: 1,
            held_elsewhere: false,
        },
    )
    .expect("fits");
    let mut flagged = out;
    flagged[REPLY_HEADER_LEN + 8] = 2;
    assert_eq!(decode_describe_reply(&flagged[..len]), Err(Errno::BadMagic));
    let mut reserved = out;
    reserved[STATUS_REPLY_LEN] = 1;
    assert_eq!(
        decode_describe_reply(&reserved[..len]),
        Err(Errno::BadMagic)
    );
    assert_eq!(decode_run_reply(&out[..len]), Err(Errno::BadMagic));
}
