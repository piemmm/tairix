//! The encoder client driven against an encoder that decodes every frame.

extern crate std;

use std::cell::RefCell;
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::driver::display::{Axis, Polarity, Scan, Timing};
use tairix_abi::driver::encoder::{
    encode_done_reply, encode_error_reply, encode_state_reply, refusal_reason, Connector,
    EncoderRequest, EncoderState, Phase, PipeLimits, ENCODER_ENDPOINTS,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::{DriverError, Errno, CALL_DEADLINE_NONE};

use crate::{EncoderClient, LinkCall};

fn link() -> LinkRequest {
    LinkRequest::new(ENCODER_ENDPOINTS.endpoint(30), 0, &[], b"hdmi0").expect("valid")
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

const STATE: EncoderState = EncoderState {
    moves: 3,
    connector: Connector {
        connected: true,
        mode: Some(MODE),
        link: 0,
    },
    phase: Phase::Off,
};

/// An encoder answering blocking calls at once and posted waits only once
/// the test moves the connector.
#[derive(Default)]
struct Part {
    requests: Vec<EncoderRequest>,
    deadlines: Vec<u64>,
    refuse: Option<DriverError>,
    moved: Vec<u64>,
    next: u64,
}

#[derive(Clone, Default)]
struct Shared(Rc<RefCell<Part>>);

impl Shared {
    fn answer(part: &Part, request: &EncoderRequest, reply: &mut [u8]) -> Result<usize, Errno> {
        if let Some(err) = part.refuse {
            return encode_error_reply(reply, refusal_reason(err));
        }
        match request {
            EncoderRequest::Describe(_) | EncoderRequest::Wait { .. } => {
                encode_state_reply(reply, &STATE)
            }
            _ => encode_done_reply(reply),
        }
    }
}

impl LinkCall for Shared {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let mut part = self.0.borrow_mut();
        let request = EncoderRequest::decode(request).expect("a canonical frame");
        part.requests.push(request);
        Self::answer(&part, &request, reply)
    }

    fn post(&mut self, request: &[u8], deadline_ns: u64) -> Result<u64, Errno> {
        let mut part = self.0.borrow_mut();
        part.requests
            .push(EncoderRequest::decode(request).expect("a canonical frame"));
        part.deadlines.push(deadline_ns);
        part.next += 1;
        Ok(part.next)
    }

    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        let part = self.0.borrow();
        if !part.moved.contains(&ticket) {
            return Ok(None);
        }
        let wait = EncoderRequest::Wait {
            link: link(),
            seen: 0,
        };
        Self::answer(&part, &wait, reply).map(Some)
    }
}

#[test]
fn a_mode_set_names_the_link_and_the_mode_in_its_order() {
    let encoder = Shared::default();
    let mut client = EncoderClient::new(encoder.clone(), link());
    let pipe = PipeLimits {
        max_clock_khz: 600_000,
        max_active_khz: 542_000,
    };
    assert_eq!(client.limit(&pipe), Ok(()));
    assert_eq!(client.describe(), Ok(STATE));
    assert_eq!(client.configure(&MODE), Ok(()));
    assert_eq!(client.prepare(), Ok(()));
    assert_eq!(client.enable(), Ok(()));
    assert_eq!(client.disable(), Ok(()));
    assert_eq!(client.power_down(), Ok(()));
    assert_eq!(client.link(), &link());
    let link = link();
    assert_eq!(
        encoder.0.borrow().requests,
        [
            EncoderRequest::Limit { link, limits: pipe },
            EncoderRequest::Describe(link),
            EncoderRequest::Configure { link, timing: MODE },
            EncoderRequest::Prepare(link),
            EncoderRequest::Enable(link),
            EncoderRequest::Disable(link),
            EncoderRequest::PowerDown(link),
        ]
    );
}

#[test]
fn the_encoders_refusals_reach_the_engine_as_it_sent_them() {
    let encoder = Shared::default();
    let mut client = EncoderClient::new(encoder.clone(), link());
    for err in [
        DriverError::Stale,
        DriverError::Busy,
        DriverError::Unsupported,
        DriverError::OutOfRange,
    ] {
        encoder.0.borrow_mut().refuse = Some(err);
        assert_eq!(client.configure(&MODE), Err(err));
    }
}

#[test]
fn a_posted_wait_has_no_deadline_and_is_reaped_once_the_connector_moves() {
    let encoder = Shared::default();
    let mut client = EncoderClient::new(encoder.clone(), link());
    assert_eq!(client.reap_wait(), Ok(None), "nothing posted");
    client.post_wait(3).expect("posts");
    assert!(client.is_waiting());
    assert_eq!(client.post_wait(3), Err(DriverError::Busy));
    assert_eq!(client.reap_wait(), Ok(None), "pending");
    assert_eq!(client.describe(), Ok(STATE), "steps go on beside it");
    encoder.0.borrow_mut().moved.push(1);
    assert_eq!(client.reap_wait(), Ok(Some(STATE)));
    assert!(!client.is_waiting());
    assert_eq!(client.reap_wait(), Ok(None), "spent");
    let part = encoder.0.borrow();
    assert_eq!(part.deadlines, [CALL_DEADLINE_NONE]);
    assert_eq!(
        part.requests[0],
        EncoderRequest::Wait {
            link: link(),
            seen: 3
        }
    );
}

#[test]
fn a_refused_wait_is_spent_and_reports_the_refusal() {
    let encoder = Shared::default();
    let mut client = EncoderClient::new(encoder.clone(), link());
    client.post_wait(0).expect("posts");
    {
        let mut part = encoder.0.borrow_mut();
        part.refuse = Some(DriverError::PermissionDenied);
        part.moved.push(1);
    }
    assert_eq!(client.reap_wait(), Err(DriverError::PermissionDenied));
    assert!(!client.is_waiting());
}

/// An encoder whose endpoint is torn down answers every call through the
/// transport alone.
struct Gone;

impl LinkCall for Gone {
    fn call(&mut self, _request: &[u8], _reply: &mut [u8]) -> Result<usize, Errno> {
        Err(Errno::NotFound)
    }
    fn post(&mut self, _request: &[u8], _deadline_ns: u64) -> Result<u64, Errno> {
        Ok(7)
    }
    fn reap(&mut self, _ticket: u64, _reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        Err(Errno::NotFound)
    }
}

#[test]
fn an_encoder_that_ended_is_told_from_one_that_failed() {
    let mut client = EncoderClient::new(Gone, link());
    assert_eq!(client.describe(), Err(DriverError::NotFound));
    client.post_wait(0).expect("posts");
    assert_eq!(client.reap_wait(), Err(DriverError::NotFound));
    assert!(!client.is_waiting());
}
