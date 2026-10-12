//! The client driven against a controller that decodes every frame it is sent
//! and answers through the protocol's own encoders.

extern crate std;

use std::cell::RefCell;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use tairix_abi::driver::i2c::{
    encode_close_reply, encode_error_reply, encode_open_reply, encode_transfer_reply, BusFacts,
    Direction, I2cAddress, I2cChannel, I2cFault, I2cMessage, I2cPort, I2cRequest, I2cTransaction,
    RepeatedStart, I2C_BUFFER_LEN, I2C_CONTROLLER_ENDPOINTS, I2C_MAX_REPLY,
};
use tairix_abi::hwlink::{LinkRequest, RegionGrant};
use tairix_abi::{DriverError, Errno, ProcId, PROC_ID_LEN};

use super::{ChannelBuffer, I2cClient, I2cTarget};
use crate::LinkCall;

const RTC: u32 = 0x68;
const GRANT: RegionGrant = RegionGrant {
    grant: 0x51,
    grantor: ProcId::from_raw([0xB5; PROC_ID_LEN]),
};

fn target_link() -> LinkRequest {
    LinkRequest::new(I2C_CONTROLLER_ENDPOINTS.endpoint(3), 0, &[RTC], b"").expect("valid")
}

fn bus_link() -> LinkRequest {
    LinkRequest::new(I2C_CONTROLLER_ENDPOINTS.endpoint(3), 0, &[], b"ddc").expect("valid")
}

fn address(raw: u32) -> I2cAddress {
    I2cAddress::from_cell(raw).expect("usable")
}

fn message(raw: u32, direction: Direction, offset: u16, len: u16) -> I2cMessage {
    I2cMessage {
        address: address(raw),
        direction,
        offset,
        len,
    }
}

/// A controller: every request decoded, so a frame the protocol would refuse
/// fails the test, and answered by the encoders.
struct State {
    requests: Vec<I2cRequest>,
    repeated_start: RepeatedStart,
    outcome: Result<Result<(), I2cFault>, Errno>,
}

#[derive(Clone)]
struct Controller(Rc<RefCell<State>>);

impl Controller {
    fn new(repeated_start: RepeatedStart) -> Self {
        Self(Rc::new(RefCell::new(State {
            requests: Vec::new(),
            repeated_start,
            outcome: Ok(Ok(())),
        })))
    }

    fn requests(&self) -> Vec<I2cRequest> {
        self.0.borrow().requests.clone()
    }
}

impl LinkCall for Controller {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let decoded = I2cRequest::decode(request).expect("a well-formed request");
        let mut state = self.0.borrow_mut();
        state.requests.push(decoded);
        let mut out = [0u8; I2C_MAX_REPLY];
        let len = match decoded {
            I2cRequest::Open(_) => encode_open_reply(
                &mut out,
                &I2cChannel {
                    facts: BusFacts {
                        speed_hz: 100_000,
                        repeated_start: state.repeated_start,
                    },
                    buffer: GRANT,
                },
            ),
            I2cRequest::Transfer { .. } => match state.outcome {
                Ok(outcome) => encode_transfer_reply(&mut out, outcome),
                Err(reason) => encode_error_reply(&mut out, reason),
            },
            I2cRequest::Close(_) => encode_close_reply(&mut out),
        }
        .expect("fits");
        reply[..len].copy_from_slice(&out[..len]);
        Ok(len)
    }

    fn post(&mut self, _request: &[u8], _deadline_ns: u64) -> Result<u64, Errno> {
        Err(Errno::NotSupported)
    }

    fn reap(&mut self, _ticket: u64, _reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        Err(Errno::NotSupported)
    }
}

struct Buffer(Vec<u8>);

impl ChannelBuffer for Buffer {
    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

/// A mapping of the grant the controller made; the kernel finds no other.
fn map(grantor: ProcId, grant: u64, least: usize) -> Result<Buffer, Errno> {
    if (grantor, grant) != (GRANT.grantor, GRANT.grant) {
        return Err(Errno::NotFound);
    }
    Ok(Buffer(vec![0; least]))
}

fn open_target(controller: &Controller) -> I2cTarget<Controller, Buffer> {
    I2cTarget::open(controller.clone(), target_link(), map).expect("opened")
}

#[test]
fn a_target_opens_its_channel_and_maps_the_buffer_it_was_granted() {
    let controller = Controller::new(RepeatedStart::BeforeFinalRead);
    let target = open_target(&controller);
    assert_eq!(target.facts().speed_hz, 100_000);
    assert_eq!(controller.requests(), [I2cRequest::Open(target_link())]);
    target.close().expect("closed");
    assert_eq!(
        controller.requests().last(),
        Some(&I2cRequest::Close(target_link()))
    );
}

#[test]
fn a_link_naming_no_one_target_is_no_target() {
    let controller = Controller::new(RepeatedStart::Any);
    assert!(matches!(
        I2cTarget::open(controller.clone(), bus_link(), map),
        Err(Errno::OutOfRange)
    ));
    assert!(controller.requests().is_empty(), "refused before any call");
}

#[test]
fn a_bus_that_cannot_follow_a_write_with_a_read_is_refused_and_its_channel_closed() {
    let controller = Controller::new(RepeatedStart::None);
    assert!(matches!(
        I2cTarget::open(controller.clone(), target_link(), map),
        Err(Errno::NotSupported)
    ));
    assert_eq!(
        controller.requests(),
        [
            I2cRequest::Open(target_link()),
            I2cRequest::Close(target_link())
        ]
    );
}

#[test]
fn a_buffer_that_cannot_be_mapped_closes_its_channel() {
    let controller = Controller::new(RepeatedStart::Any);
    let refused = |_, _, _| Err::<Buffer, _>(Errno::PermissionDenied);
    assert!(matches!(
        I2cClient::open(controller.clone(), target_link(), refused),
        Err(Errno::PermissionDenied)
    ));
    let short = |_, _, least: usize| Ok(Buffer(vec![0; least - 1]));
    assert!(matches!(
        I2cClient::open(controller.clone(), target_link(), short),
        Err(Errno::LengthOutOfRange)
    ));
    let closes = controller
        .requests()
        .iter()
        .filter(|request| matches!(request, I2cRequest::Close(_)))
        .count();
    assert_eq!(closes, 2);
}

#[test]
fn a_register_read_is_one_write_then_read_through_the_buffer() {
    let controller = Controller::new(RepeatedStart::BeforeFinalRead);
    let mut target = open_target(&controller);
    // What the controller leaves in the read's span.
    target.channel.buffer()[1..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
    let mut read = [0u8; 7];
    target.transfer(&[0x02], &mut read).expect("read");
    assert_eq!(read, [1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(target.channel.buffer()[0], 0x02, "the pointer staged");
    assert_eq!(
        controller.requests().last(),
        Some(&I2cRequest::Transfer {
            link: target_link(),
            transaction: I2cTransaction::new(&[
                message(RTC, Direction::Write, 0, 1),
                message(RTC, Direction::Read, 1, 7),
            ])
            .expect("valid"),
        })
    );
}

#[test]
fn a_lone_write_a_lone_read_and_a_probe_are_each_one_message() {
    let controller = Controller::new(RepeatedStart::BeforeFinalRead);
    let mut target = open_target(&controller);
    target.transfer(&[0x10, 0x20], &mut []).expect("written");
    target.transfer(&[], &mut [0u8; 3]).expect("read");
    target.transfer(&[], &mut []).expect("answered");
    let transactions: Vec<I2cTransaction> = controller
        .requests()
        .into_iter()
        .filter_map(|request| match request {
            I2cRequest::Transfer { transaction, .. } => Some(transaction),
            _ => None,
        })
        .collect();
    let single = |message: I2cMessage| I2cTransaction::new(&[message]).expect("valid");
    assert_eq!(
        transactions,
        [
            single(message(RTC, Direction::Write, 0, 2)),
            single(message(RTC, Direction::Read, 0, 3)),
            single(message(RTC, Direction::Write, 0, 0)),
        ]
    );
}

#[test]
fn an_unanswered_or_faulted_transaction_is_the_part_s_error() {
    let controller = Controller::new(RepeatedStart::BeforeFinalRead);
    let mut target = open_target(&controller);
    let mut read = [0xEEu8; 2];
    for (outcome, expected) in [
        (
            Ok(Err(I2cFault::NoAck { message: 0 })),
            DriverError::NotFound,
        ),
        (
            Ok(Err(I2cFault::NoAck { message: 1 })),
            DriverError::DeviceFault,
        ),
        (
            Ok(Err(I2cFault::TimedOut { message: 0 })),
            DriverError::DeviceFault,
        ),
        (Err(Errno::PermissionDenied), DriverError::PermissionDenied),
    ] {
        controller.0.borrow_mut().outcome = outcome;
        assert_eq!(target.transfer(&[0], &mut read), Err(expected));
        assert_eq!(read, [0xEE; 2], "a failed read is never filled");
    }
}

#[test]
fn a_transfer_past_the_protocol_s_bounds_is_refused_before_any_call() {
    let controller = Controller::new(RepeatedStart::Any);
    let mut target = open_target(&controller);
    let long = vec![0u8; 4097];
    assert_eq!(
        target.transfer(&long, &mut []),
        Err(DriverError::LengthOutOfRange)
    );
    let mut both = vec![0u8; 4096];
    let half = vec![0u8; I2C_BUFFER_LEN - 4096 + 1];
    assert_eq!(
        target.transfer(&half, &mut both),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(controller.requests().len(), 1, "only the open");
}

#[test]
fn a_bus_channel_runs_any_transaction_its_bus_can_issue() {
    let controller = Controller::new(RepeatedStart::BeforeFinalRead);
    let mut client = I2cClient::open(controller.clone(), bus_link(), map).expect("opened");
    client.buffer()[..2].copy_from_slice(&[1, 0x80]);
    let edid = I2cTransaction::new(&[
        message(0x30, Direction::Write, 0, 1),
        message(0x50, Direction::Write, 1, 1),
        message(0x50, Direction::Read, 2, 128),
    ])
    .expect("valid");
    assert_eq!(client.run(&edid), Ok(Ok(())));
    assert_eq!(
        controller.requests().last(),
        Some(&I2cRequest::Transfer {
            link: bus_link(),
            transaction: edid,
        })
    );
}
