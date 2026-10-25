use super::{
    decode_lines, decode_status, encode_lines, encode_status, HeaderLines, HeaderRequest, Holder,
    LineEntry, LineState, LineTable, Polarity, GPIO_HEADER_ENDPOINTS, GPIO_HEADER_LINES_MAX,
    HEADER_MAX_REPLY, HEADER_REQUEST_LEN,
};
use crate::hwtree::NodeEndpointBlock;
use crate::Errno;

fn framed(request: HeaderRequest) -> [u8; HEADER_REQUEST_LEN] {
    let mut frame = [0u8; HEADER_REQUEST_LEN];
    assert_eq!(request.encode(&mut frame), Ok(HEADER_REQUEST_LEN));
    frame
}

fn table(entries: &[LineEntry]) -> LineTable {
    let mut table = LineTable::default();
    for &entry in entries {
        table.push(entry).unwrap();
    }
    table
}

#[test]
fn every_polarity_is_listed_once() {
    for byte in 0..=u8::MAX {
        let listed = Polarity::ALL
            .iter()
            .filter(|polarity| **polarity as u8 == byte)
            .count();
        assert_eq!(
            listed,
            usize::from(Polarity::from_u8(byte).is_some()),
            "{byte}"
        );
    }
}

#[test]
fn a_span_names_exactly_its_lines() {
    let pi = HeaderLines::span(2, 27);
    assert_eq!(pi.len(), 26);
    assert!(!pi.contains(1) && pi.contains(2) && pi.contains(27) && !pi.contains(28));
    assert!(pi.iter().eq(2..=27));
    assert_eq!(HeaderLines::span(0, 63).mask(), u64::MAX);
    assert_eq!(HeaderLines::span(63, 63).mask(), 1 << 63);
    assert!(HeaderLines::span(5, 4).is_empty());
    assert!(HeaderLines::span(0, 64).is_empty());
    assert!(!HeaderLines::from_mask(u64::MAX).contains(64));
}

#[test]
fn every_request_round_trips() {
    for request in [
        HeaderRequest::Lines,
        HeaderRequest::Claim {
            line: 18,
            polarity: Polarity::ActiveLow,
            asserted: true,
        },
        HeaderRequest::Claim {
            line: 2,
            polarity: Polarity::ActiveHigh,
            asserted: false,
        },
        HeaderRequest::Drive {
            line: 27,
            asserted: true,
        },
        HeaderRequest::Release { line: 63 },
    ] {
        assert_eq!(HeaderRequest::decode(&framed(request)), Ok(request));
    }
}

#[test]
fn a_malformed_request_is_refused_whole() {
    let claim = framed(HeaderRequest::Claim {
        line: 4,
        polarity: Polarity::ActiveHigh,
        asserted: true,
    });
    assert_eq!(
        HeaderRequest::decode(&claim[..11]),
        Err(Errno::LengthOutOfRange)
    );
    for (at, value, refusal) in [
        (0, 0, Errno::BadMagic),
        (4, 2, Errno::AbiVersionUnsupported),
        (6, 9, Errno::OutOfRange),
        (7, 1, Errno::BadMagic),
        (8, 64, Errno::OutOfRange),
        (9, 2, Errno::BadMagic),
        (10, 2, Errno::BadMagic),
        (11, 1, Errno::BadMagic),
    ] {
        let mut frame = claim;
        frame[at] = value;
        assert_eq!(HeaderRequest::decode(&frame), Err(refusal), "byte {at}");
    }
    let mut lines = framed(HeaderRequest::Lines);
    lines[8] = 3;
    assert_eq!(HeaderRequest::decode(&lines), Err(Errno::BadMagic));
    let mut drive = framed(HeaderRequest::Drive {
        line: 3,
        asserted: false,
    });
    drive[9] = 1;
    assert_eq!(HeaderRequest::decode(&drive), Err(Errno::BadMagic));
    let mut release = framed(HeaderRequest::Release { line: 3 });
    release[10] = 1;
    assert_eq!(HeaderRequest::decode(&release), Err(Errno::BadMagic));
}

#[test]
fn polarity_decides_the_level_that_means_on() {
    assert!(Polarity::ActiveHigh.level(true));
    assert!(!Polarity::ActiveHigh.level(false));
    assert!(!Polarity::ActiveLow.level(true));
    assert!(Polarity::ActiveLow.level(false));
}

#[test]
fn a_table_round_trips_with_every_state() {
    let held = table(&[
        LineEntry {
            line: 2,
            state: LineState::Peripheral { function: 0 },
        },
        LineEntry {
            line: 4,
            state: LineState::Free,
        },
        LineEntry {
            line: 17,
            state: LineState::Interrupt,
        },
        LineEntry {
            line: 18,
            state: LineState::Output {
                holder: Holder::Caller,
                asserted: true,
            },
        },
        LineEntry {
            line: 27,
            state: LineState::Output {
                holder: Holder::Other,
                asserted: false,
            },
        },
    ]);
    let mut out = [0u8; HEADER_MAX_REPLY];
    let len = encode_lines(&mut out, Ok(&held)).unwrap();
    assert_eq!(decode_lines(&out[..len]), Ok(held));
    assert_eq!(held.state(17), Some(LineState::Interrupt));
    assert_eq!(held.state(3), None);
    assert!(held.state(4).is_some_and(LineState::is_free));
}

#[test]
fn a_full_table_fits_the_longest_reply() {
    let mut full = LineTable::default();
    for line in 0..64 {
        full.push(LineEntry {
            line,
            state: LineState::Free,
        })
        .unwrap();
    }
    assert_eq!(full.entries().len(), GPIO_HEADER_LINES_MAX);
    let mut out = [0u8; HEADER_MAX_REPLY];
    assert_eq!(encode_lines(&mut out, Ok(&full)), Ok(HEADER_MAX_REPLY));
    assert_eq!(decode_lines(&out), Ok(full));
}

#[test]
fn a_table_refuses_lines_out_of_order() {
    let mut held = table(&[LineEntry {
        line: 9,
        state: LineState::Free,
    }]);
    let repeat = LineEntry {
        line: 9,
        state: LineState::Free,
    };
    assert_eq!(held.push(repeat), Err(Errno::OutOfRange));
    let past = LineEntry {
        line: 64,
        state: LineState::Free,
    };
    assert_eq!(held.push(past), Err(Errno::OutOfRange));
}

#[test]
fn a_malformed_table_is_refused_whole() {
    let held = table(&[
        LineEntry {
            line: 5,
            state: LineState::Free,
        },
        LineEntry {
            line: 6,
            state: LineState::Interrupt,
        },
    ]);
    let mut out = [0u8; HEADER_MAX_REPLY];
    let len = encode_lines(&mut out, Ok(&held)).unwrap();
    assert_eq!(decode_lines(&out[..len - 1]), Err(Errno::BadMagic));
    for (at, value) in [(4, 3), (5, 1), (8, 7), (9, 9), (10, 1), (11, 1), (13, 7)] {
        let mut frame = out;
        frame[at] = value;
        assert_eq!(
            decode_lines(&frame[..len]),
            Err(Errno::BadMagic),
            "byte {at}"
        );
    }
    let mut peripheral = out;
    peripheral[9] = 1;
    peripheral[10] = 6;
    assert_eq!(decode_lines(&peripheral[..len]), Err(Errno::BadMagic));
}

#[test]
fn a_refused_listing_and_every_status_round_trip() {
    let mut out = [0u8; HEADER_MAX_REPLY];
    let len = encode_lines(&mut out, Err(Errno::PermissionDenied)).unwrap();
    assert_eq!(decode_lines(&out[..len]), Err(Errno::PermissionDenied));
    for outcome in [Ok(()), Err(Errno::Busy), Err(Errno::NotFound)] {
        let len = encode_status(&mut out, outcome).unwrap();
        assert_eq!(decode_status(&out[..len]), outcome);
    }
    assert_eq!(decode_status(&out[..3]), Err(Errno::BadMagic));
}

#[test]
fn the_header_block_is_reserved_and_a_device_channel() {
    assert!(NodeEndpointBlock::ALL.contains(&GPIO_HEADER_ENDPOINTS));
    assert_eq!(
        NodeEndpointBlock::device_channel_node(GPIO_HEADER_ENDPOINTS.endpoint(3)),
        Some(3)
    );
}
