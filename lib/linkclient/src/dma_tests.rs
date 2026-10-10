//! The client driven against a controller that decodes every frame it is
//! sent and answers through the protocol's own encoders.

extern crate std;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::driver::dmaengine::{
    encode_done_reply, encode_error_reply, encode_open_reply, encode_position_reply,
    encode_prepare_reply, encode_wait_reply, CyclicParams, DmaBufferGrant, DmaDirection,
    DmaEngineRequest, WaitEnd, WaitReport, DMA_CONTROLLER_ENDPOINTS, DMA_ENGINE_MAX_REPLY,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::time::Duration64;
use tairix_abi::{Errno, ProcId, PROC_ID_LEN};

use crate::{DmaClient, LinkCall};

const CHANNEL: u8 = 5;
const GRANT: DmaBufferGrant = DmaBufferGrant {
    grant: 0x77,
    grantor: ProcId::from_raw([0xC0; PROC_ID_LEN]),
};

fn line() -> LinkRequest {
    LinkRequest::new(DMA_CONTROLLER_ENDPOINTS.endpoint(12), 0, &[1], b"tx").expect("valid")
}

fn params() -> CyclicParams {
    CyclicParams {
        fifo: 0xFE20_C818,
        direction: DmaDirection::MemoryToDevice,
        period_bytes: 4096,
        periods: 4,
    }
}

fn report(position: u64) -> WaitReport {
    WaitReport {
        end: WaitEnd::Boundary,
        position,
        serviced: Duration64::from_nanos(position),
    }
}

/// A controller holding one channel: every request is decoded, so a frame
/// the protocol would refuse fails the test, and is answered by the encoders.
#[derive(Default)]
struct Controller {
    requests: Vec<DmaEngineRequest>,
    refuse: Option<Errno>,
    next_ticket: u64,
    /// Posted waits and their answers, once given.
    posted: BTreeMap<u64, Option<Vec<u8>>>,
    deadlines: Vec<u64>,
    reap_error: Option<Errno>,
}

impl Controller {
    fn answer_waits(&mut self, end: WaitEnd, position: u64) {
        for answer in self.posted.values_mut().filter(|answer| answer.is_none()) {
            let mut out = [0u8; DMA_ENGINE_MAX_REPLY];
            let report = WaitReport {
                end,
                position,
                serviced: Duration64::from_nanos(position),
            };
            let len = encode_wait_reply(&mut out, &report).expect("fits");
            *answer = Some(out[..len].to_vec());
        }
    }
}

/// The client's handle on the controller, which the test keeps one of too.
#[derive(Clone)]
struct Fake(Rc<RefCell<Controller>>);

impl Fake {
    fn new(controller: Controller) -> Self {
        Self(Rc::new(RefCell::new(controller)))
    }
}

impl LinkCall for Fake {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let mut this = self.0.borrow_mut();
        let this = &mut *this;
        let decoded = DmaEngineRequest::decode(request).expect("a canonical frame");
        this.requests.push(decoded);
        if let Some(reason) = this.refuse {
            return encode_error_reply(reply, reason);
        }
        match decoded {
            DmaEngineRequest::Open(_) => encode_open_reply(reply, CHANNEL),
            DmaEngineRequest::Prepare { .. } => encode_prepare_reply(reply, &GRANT),
            DmaEngineRequest::Position { .. } => encode_position_reply(reply, 8192),
            DmaEngineRequest::Close { .. } | DmaEngineRequest::Stop { .. } => {
                this.answer_waits(WaitEnd::Stopped, 0);
                encode_done_reply(reply, decoded.op())
            }
            DmaEngineRequest::Start { .. } => encode_done_reply(reply, decoded.op()),
            DmaEngineRequest::Wait { after, .. } => encode_wait_reply(reply, &report(after + 4096)),
        }
    }

    fn post(&mut self, request: &[u8], deadline_ns: u64) -> Result<u64, Errno> {
        let mut this = self.0.borrow_mut();
        let this = &mut *this;
        let decoded = DmaEngineRequest::decode(request).expect("a canonical frame");
        assert!(matches!(decoded, DmaEngineRequest::Wait { .. }));
        this.requests.push(decoded);
        this.deadlines.push(deadline_ns);
        let ticket = this.next_ticket;
        this.next_ticket += 1;
        this.posted.insert(ticket, None);
        Ok(ticket)
    }

    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        let mut this = self.0.borrow_mut();
        let this = &mut *this;
        if let Some(reason) = this.reap_error {
            this.posted.remove(&ticket);
            return Err(reason);
        }
        match this.posted.get(&ticket) {
            None => Err(Errno::NotFound),
            Some(None) => Ok(None),
            Some(Some(answer)) => {
                reply[..answer.len()].copy_from_slice(answer);
                let len = answer.len();
                this.posted.remove(&ticket);
                Ok(Some(len))
            }
        }
    }
}

#[test]
fn a_channel_opens_on_its_line_and_every_call_names_it() {
    let controller = Fake::new(Controller::default());
    let mut client = DmaClient::open(controller.clone(), line()).expect("opened");
    assert_eq!(client.channel(), CHANNEL);
    assert_eq!(client.prepare(&params()), Ok(GRANT));
    assert_eq!(client.start(), Ok(()));
    assert_eq!(client.position(), Ok(8192));
    assert_eq!(client.stop(), Ok(()));
    assert_eq!(client.close(), Ok(()));
    assert_eq!(
        controller.0.borrow().requests,
        [
            DmaEngineRequest::Open(line()),
            DmaEngineRequest::Prepare {
                channel: CHANNEL,
                params: params(),
            },
            DmaEngineRequest::Start { channel: CHANNEL },
            DmaEngineRequest::Position { channel: CHANNEL },
            DmaEngineRequest::Stop { channel: CHANNEL },
            DmaEngineRequest::Close { channel: CHANNEL },
        ]
    );
}

#[test]
fn a_refusal_comes_back_as_the_controllers_reason() {
    let controller = Fake::new(Controller {
        refuse: Some(Errno::PermissionDenied),
        ..Controller::default()
    });
    assert!(matches!(
        DmaClient::open(controller, line()),
        Err(Errno::PermissionDenied)
    ));
}

#[test]
fn one_wait_is_outstanding_at_a_time_and_is_collected_once_answered() {
    let controller = Fake::new(Controller::default());
    let mut client = DmaClient::open(controller.clone(), line()).expect("opened");
    assert_eq!(client.reap_wait(), Ok(None), "nothing posted");
    assert_eq!(client.post_wait(4096, 40_000_000), Ok(()));
    assert!(client.is_waiting());
    assert_eq!(client.post_wait(4096, 40_000_000), Err(Errno::Busy));
    assert_eq!(client.reap_wait(), Ok(None), "not yet answered");
    controller
        .0
        .borrow_mut()
        .answer_waits(WaitEnd::Boundary, 8192);
    assert_eq!(client.reap_wait(), Ok(Some(report(8192))));
    assert!(!client.is_waiting());
    assert_eq!(client.post_wait(8192, 40_000_000), Ok(()), "the next");
    let controller = controller.0.borrow();
    assert_eq!(controller.deadlines, [40_000_000, 40_000_000]);
    assert_eq!(
        controller.requests.last(),
        Some(&DmaEngineRequest::Wait {
            channel: CHANNEL,
            after: 8192,
        })
    );
}

#[test]
fn a_wait_the_transport_refuses_is_spent_and_reported() {
    let controller = Fake::new(Controller {
        reap_error: Some(Errno::TimedOut),
        ..Controller::default()
    });
    let mut client = DmaClient::open(controller, line()).expect("opened");
    client.post_wait(0, 1).expect("posted");
    assert_eq!(client.reap_wait(), Err(Errno::TimedOut));
    assert!(!client.is_waiting());
}

#[test]
fn closing_collects_the_wait_the_close_answered() {
    let controller = Fake::new(Controller::default());
    let mut client = DmaClient::open(controller.clone(), line()).expect("opened");
    client.post_wait(0, 1).expect("posted");
    assert_eq!(client.close(), Ok(()));
    assert!(
        controller.0.borrow().posted.is_empty(),
        "no reply left behind"
    );
}

#[test]
fn a_blocking_wait_is_answered_in_line_and_refused_beside_a_posted_one() {
    let controller = Fake::new(Controller::default());
    let mut client = DmaClient::open(controller.clone(), line()).expect("opened");
    assert_eq!(client.wait(0), Ok(report(4096)));
    client.post_wait(4096, 1).expect("posted");
    assert_eq!(client.wait(4096), Err(Errno::Busy));
    assert_eq!(
        controller.0.borrow().requests[1],
        DmaEngineRequest::Wait {
            channel: CHANNEL,
            after: 0,
        }
    );
}
