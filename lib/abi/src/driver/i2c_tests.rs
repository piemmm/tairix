extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::driver::clock::CLOCK_CONTROLLER_ENDPOINTS;
use crate::origin::{ProcId, PROC_ID_LEN};

fn link(selector: &[u32]) -> LinkRequest {
    LinkRequest::new(I2C_CONTROLLER_ENDPOINTS.endpoint(5), 0, selector, b"").expect("valid")
}

fn address(raw: u8) -> I2cAddress {
    I2cAddress::new(raw).expect("a device address")
}

fn message(raw: u8, direction: Direction, offset: u16, len: u16) -> I2cMessage {
    I2cMessage {
        address: address(raw),
        direction,
        offset,
        len,
    }
}

fn register_read() -> I2cTransaction {
    I2cTransaction::new(&[
        message(0x68, Direction::Write, 0, 1),
        message(0x68, Direction::Read, 1, 7),
    ])
    .expect("valid")
}

fn encoded(request: &I2cRequest) -> Vec<u8> {
    let mut out = vec![0u8; I2C_MAX_REQUEST];
    let len = request.encode(&mut out).expect("fits");
    out.truncate(len);
    out
}

#[test]
fn every_reserved_address_is_refused() {
    for raw in (0..0x08).chain(0x78..=0xFF) {
        assert_eq!(I2cAddress::new(raw), Err(Errno::OutOfRange), "{raw:#x}");
    }
    assert_eq!(address(0x50).get(), 0x50);
    assert_eq!(I2cAddress::from_cell(0x168), Err(Errno::OutOfRange));
    assert_eq!(I2cAddress::from_cell(0x68), Ok(address(0x68)));
}

#[test]
fn a_target_link_reaches_its_address_alone_and_a_bus_link_every_one() {
    let target = I2cReach::of(&link(&[0x68])).expect("a target");
    assert_eq!(target, I2cReach::Target(address(0x68)));
    assert!(target.reaches(address(0x68)));
    assert!(!target.reaches(address(0x69)));
    let bus = I2cReach::of(&link(&[])).expect("a bus");
    assert!(bus.reaches(address(0x50)) && bus.reaches(address(0x30)));
    assert_eq!(I2cReach::of(&link(&[0x68, 0])), Err(Errno::OutOfRange));
    assert_eq!(I2cReach::of(&link(&[0x05])), Err(Errno::OutOfRange));
    let clock =
        LinkRequest::new(CLOCK_CONTROLLER_ENDPOINTS.endpoint(5), 0, &[0x68], b"").expect("valid");
    assert_eq!(
        I2cReach::of(&clock),
        Err(Errno::OutOfRange),
        "another role's link"
    );
}

#[test]
fn a_transaction_holds_one_to_eight_messages_within_their_bounds() {
    assert!(
        I2cTransaction::new(&[message(0x68, Direction::Write, 0, 0)]).is_ok(),
        "a quick write"
    );
    let full = [message(0x68, Direction::Write, 0, 1); I2C_MAX_MESSAGES];
    assert_eq!(
        I2cTransaction::new(&full).map(|t| t.messages().len()),
        Ok(8)
    );
    let over = [message(0x68, Direction::Write, 0, 1); I2C_MAX_MESSAGES + 1];
    for refused in [
        &[][..],
        &over,
        &[message(0x68, Direction::Read, 0, 0)],
        &[message(0x68, Direction::Read, 0, 4097)],
        &[message(0x68, Direction::Write, 8191, 2)],
    ] {
        assert_eq!(I2cTransaction::new(refused), Err(Errno::LengthOutOfRange));
    }
    assert!(
        I2cTransaction::new(&[
            message(0x68, Direction::Write, 0, 2),
            message(0x68, Direction::Read, 4096, 4096),
        ])
        .is_ok(),
        "a longest read after its register write"
    );
}

#[test]
fn a_port_writes_then_reads_through_the_buffer_in_one_transaction() {
    let rtc = address(0x68);
    let messages = |write, read| {
        I2cTransaction::write_then_read(rtc, write, read)
            .map(|transaction| transaction.messages().to_vec())
    };
    assert_eq!(
        messages(1, 7),
        Ok(vec![
            message(0x68, Direction::Write, 0, 1),
            message(0x68, Direction::Read, 1, 7),
        ])
    );
    assert_eq!(
        messages(3, 0),
        Ok(vec![message(0x68, Direction::Write, 0, 3)])
    );
    assert_eq!(
        messages(0, 3),
        Ok(vec![message(0x68, Direction::Read, 0, 3)])
    );
    assert_eq!(
        messages(0, 0),
        Ok(vec![message(0x68, Direction::Write, 0, 0)]),
        "both empty asks whether the part answers"
    );
    for (write, read) in [
        (I2C_MAX_MESSAGE_LEN + 1, 0),
        (0, I2C_MAX_MESSAGE_LEN + 1),
        (I2C_MAX_MESSAGE_LEN, I2C_MAX_MESSAGE_LEN + 1),
        (usize::from(u16::MAX) + 1, 0),
    ] {
        assert_eq!(messages(write, read), Err(Errno::LengthOutOfRange));
    }
    assert!(I2cTransaction::write_then_read(rtc, I2C_MAX_MESSAGE_LEN, I2C_MAX_MESSAGE_LEN).is_ok());
}

#[test]
fn each_bus_admits_only_the_repeated_starts_it_can_issue() {
    let single = I2cTransaction::new(&[message(0x68, Direction::Read, 0, 4)]).expect("valid");
    let writes_then_read = I2cTransaction::new(&[
        message(0x30, Direction::Write, 0, 1),
        message(0x50, Direction::Write, 1, 1),
        message(0x50, Direction::Read, 2, 128),
    ])
    .expect("valid");
    let read_then_write = I2cTransaction::new(&[
        message(0x68, Direction::Read, 0, 1),
        message(0x68, Direction::Write, 1, 1),
    ])
    .expect("valid");
    assert!(RepeatedStart::None.admits(&single));
    assert!(!RepeatedStart::None.admits(&writes_then_read));
    let empty_write_then_read = I2cTransaction::new(&[
        message(0x68, Direction::Write, 0, 0),
        message(0x68, Direction::Read, 0, 1),
    ])
    .expect("valid");
    assert!(RepeatedStart::BeforeFinalRead.admits(&writes_then_read));
    assert!(!RepeatedStart::BeforeFinalRead.admits(&read_then_write));
    assert!(
        !RepeatedStart::BeforeFinalRead.admits(&empty_write_then_read),
        "a chaining controller has no byte to chain the read onto"
    );
    assert!(RepeatedStart::Any.admits(&read_then_write));
    assert!(RepeatedStart::Any.admits(&empty_write_then_read));
}

#[test]
fn a_request_round_trips_and_each_frame_has_its_exact_length() {
    for request in [
        I2cRequest::Open(link(&[0x68])),
        I2cRequest::Transfer {
            link: link(&[0x68]),
            transaction: register_read(),
        },
        I2cRequest::Close(link(&[])),
    ] {
        let frame = encoded(&request);
        assert_eq!(I2cRequest::decode(&frame), Ok(request));
        assert_eq!(
            I2cRequest::decode(&frame[..frame.len() - 1]),
            Err(Errno::LengthOutOfRange)
        );
        let mut longer = frame.clone();
        longer.push(0);
        assert_eq!(I2cRequest::decode(&longer), Err(Errno::LengthOutOfRange));
    }
}

#[test]
fn a_malformed_request_is_refused() {
    let frame = encoded(&I2cRequest::Transfer {
        link: link(&[0x68]),
        transaction: register_read(),
    });
    let message_at = HEADER_LEN + TRANSFER_HEAD_LEN;
    let mut cases: Vec<(Vec<u8>, Errno)> = Vec::new();
    let mut edit = |at: usize, value: u8, errno: Errno| {
        let mut bad = frame.clone();
        bad[at] = value;
        cases.push((bad, errno));
    };
    edit(0, b'X', Errno::BadMagic);
    edit(4, 2, Errno::AbiVersionUnsupported);
    edit(6, 9, Errno::OutOfRange);
    edit(7, 1, Errno::BadMagic);
    edit(message_at + 1, 2, Errno::BadMagic);
    edit(message_at + 2, 1, Errno::BadMagic);
    edit(message_at, 0x05, Errno::OutOfRange);
    edit(
        HEADER_LEN + HwResource::WIRE_LEN,
        0,
        Errno::LengthOutOfRange,
    );
    edit(
        HEADER_LEN + HwResource::WIRE_LEN,
        9,
        Errno::LengthOutOfRange,
    );
    edit(
        HEADER_LEN + HwResource::WIRE_LEN,
        3,
        Errno::LengthOutOfRange,
    );
    for (bad, errno) in cases {
        assert_eq!(I2cRequest::decode(&bad), Err(errno), "{bad:x?}");
    }
    let clock =
        LinkRequest::new(CLOCK_CONTROLLER_ENDPOINTS.endpoint(5), 0, &[0x68], b"").expect("valid");
    let mut foreign = frame;
    foreign[HEADER_LEN..HEADER_LEN + HwResource::WIRE_LEN]
        .copy_from_slice(&HwResource::request(&clock).to_le_bytes());
    assert_eq!(
        I2cRequest::decode(&foreign),
        Err(Errno::BadMagic),
        "a clock link"
    );
}

#[test]
fn an_open_reply_carries_the_bus_facts_and_the_buffer_grant() {
    let channel = I2cChannel {
        facts: BusFacts {
            speed_hz: 400_000,
            repeated_start: RepeatedStart::BeforeFinalRead,
        },
        buffer: RegionGrant {
            grant: 0x6001,
            grantor: ProcId::from_raw([0x5A; PROC_ID_LEN]),
        },
    };
    let mut out = [0u8; I2C_MAX_REPLY];
    let len = encode_open_reply(&mut out, &channel).expect("fits");
    assert_eq!(decode_open_reply(&out[..len]), Ok(channel));
    let mut no_speed = out;
    no_speed[REPLY_HEADER_LEN..REPLY_HEADER_LEN + 4].fill(0);
    assert_eq!(decode_open_reply(&no_speed[..len]), Err(Errno::OutOfRange));
    let mut bad_restart = out;
    bad_restart[REPLY_HEADER_LEN + 4] = 3;
    assert_eq!(decode_open_reply(&bad_restart[..len]), Err(Errno::BadMagic));
    let mut no_grant = out;
    no_grant[REPLY_HEADER_LEN + 8..REPLY_HEADER_LEN + 16].fill(0);
    assert_eq!(decode_open_reply(&no_grant[..len]), Err(Errno::OutOfRange));
    let mut refused = [0u8; I2C_MAX_REPLY];
    let len = encode_error_reply(&mut refused, Errno::PermissionDenied).expect("fits");
    assert_eq!(
        decode_open_reply(&refused[..len]),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn a_transfer_reply_states_how_the_transaction_ended() {
    for outcome in [
        Ok(()),
        Err(I2cFault::NoAck { message: 0 }),
        Err(I2cFault::ArbitrationLost { message: 2 }),
        Err(I2cFault::BusFault { message: 7 }),
        Err(I2cFault::TimedOut { message: 1 }),
    ] {
        let mut out = [0u8; I2C_MAX_REPLY];
        let len = encode_transfer_reply(&mut out, outcome).expect("fits");
        assert_eq!(decode_transfer_reply(&out[..len]), Ok(outcome));
    }
    let mut out = [0u8; I2C_MAX_REPLY];
    let len = encode_transfer_reply(&mut out, Err(I2cFault::NoAck { message: 0 })).expect("fits");
    let mut past = out;
    past[REPLY_HEADER_LEN + 1] = 8;
    assert_eq!(decode_transfer_reply(&past[..len]), Err(Errno::BadMagic));
    let mut unknown = out;
    unknown[REPLY_HEADER_LEN] = 9;
    assert_eq!(decode_transfer_reply(&unknown[..len]), Err(Errno::BadMagic));
    let mut dirty = out;
    dirty[REPLY_HEADER_LEN + 3] = 1;
    assert_eq!(decode_transfer_reply(&dirty[..len]), Err(Errno::BadMagic));
    let mut close = [0u8; I2C_MAX_REPLY];
    let len = encode_close_reply(&mut close).expect("fits");
    assert_eq!(decode_close_reply(&close[..len]), Ok(()));
}

#[test]
fn a_target_that_does_not_answer_is_not_found_and_any_other_fault_is_the_device_s() {
    assert_eq!(
        I2cFault::NoAck { message: 0 }.as_driver_error(),
        DriverError::NotFound
    );
    for fault in [
        I2cFault::NoAck { message: 1 },
        I2cFault::ArbitrationLost { message: 0 },
        I2cFault::BusFault { message: 0 },
        I2cFault::TimedOut { message: 0 },
    ] {
        assert_eq!(fault.as_driver_error(), DriverError::DeviceFault);
    }
}
