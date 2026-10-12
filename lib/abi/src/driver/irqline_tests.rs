use super::*;
use crate::driver::clock::CLOCK_CONTROLLER_ENDPOINTS;

fn link() -> LinkRequest {
    LinkRequest::new(INTERRUPT_LINE_ENDPOINTS.endpoint(21), 4, &[4], b"").expect("valid")
}

fn encoded() -> [u8; IRQLINE_MAX_REQUEST + 1] {
    let mut out = [0u8; IRQLINE_MAX_REQUEST + 1];
    assert_eq!(
        IrqLineRequest::Wait(link()).encode(&mut out),
        Ok(IRQLINE_MAX_REQUEST)
    );
    out
}

#[test]
fn a_wait_round_trips_at_exactly_its_length() {
    let frame = encoded();
    let decoded = IrqLineRequest::decode(&frame[..IRQLINE_MAX_REQUEST]);
    assert_eq!(decoded, Ok(IrqLineRequest::Wait(link())));
    assert_eq!(decoded.map(|request| request.op()), Ok(IrqLineOp::Wait));
    assert_eq!(
        IrqLineRequest::decode(&frame[..=IRQLINE_MAX_REQUEST]),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        IrqLineRequest::decode(&frame[..IRQLINE_MAX_REQUEST - 1]),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        IrqLineRequest::Wait(link()).encode(&mut [0u8; IRQLINE_MAX_REQUEST - 1]),
        Err(Errno::BufferTooSmall)
    );
}

#[test]
fn a_frame_that_is_not_a_canonical_line_request_is_refused() {
    let good = encoded();
    let frame = |edit: &dyn Fn(&mut [u8])| {
        let mut frame = good;
        edit(&mut frame);
        IrqLineRequest::decode(&frame[..IRQLINE_MAX_REQUEST])
    };
    assert_eq!(frame(&|f| f[0] ^= 1), Err(Errno::BadMagic));
    assert_eq!(frame(&|f| f[7] = 1), Err(Errno::BadMagic));
    assert_eq!(frame(&|f| f[4] = 2), Err(Errno::AbiVersionUnsupported));
    assert_eq!(frame(&|f| f[6] = 2), Err(Errno::OutOfRange));
    // A clock link quoted on the interrupt-line protocol is not a line.
    let clock =
        LinkRequest::new(CLOCK_CONTROLLER_ENDPOINTS.endpoint(21), 0, &[4], b"").expect("valid");
    let mut quoted = good;
    quoted[HEADER_LEN..IRQLINE_MAX_REQUEST]
        .copy_from_slice(&HwResource::request(&clock).to_le_bytes());
    assert_eq!(
        IrqLineRequest::decode(&quoted[..IRQLINE_MAX_REQUEST]),
        Err(Errno::BadMagic)
    );
}

#[test]
fn a_reply_carries_the_wait_or_its_refusal() {
    let mut out = [0u8; IRQLINE_MAX_REPLY];
    assert_eq!(encode_reply(&mut out, Ok(())), Ok(IRQLINE_MAX_REPLY));
    assert_eq!(decode_reply(&out), Ok(()));
    assert_eq!(
        encode_reply(&mut out, Err(Errno::Busy)),
        Ok(IRQLINE_MAX_REPLY)
    );
    assert_eq!(decode_reply(&out), Err(Errno::Busy));
    assert_eq!(
        decode_reply(&[0u8; IRQLINE_MAX_REPLY + 1]),
        Err(Errno::BadMagic)
    );
    assert_eq!(
        encode_reply(&mut [0u8; 3], Ok(())),
        Err(Errno::BufferTooSmall)
    );
}
