//! The clock client driven against a controller that decodes every frame.

extern crate std;

use std::vec::Vec;

use tairix_abi::driver::clock::{
    encode_describe_reply, encode_error_reply, encode_release_reply, encode_run_reply,
    ClockRequest, ClockState, CLOCK_CONTROLLER_ENDPOINTS,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::Errno;

use crate::{ClockClient, LinkCall};

fn link() -> LinkRequest {
    LinkRequest::new(CLOCK_CONTROLLER_ENDPOINTS.endpoint(8), 0, &[30], b"").expect("valid")
}

#[derive(Default)]
struct Controller {
    requests: Vec<ClockRequest>,
    refuse: Option<Errno>,
}

impl LinkCall for &mut Controller {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let decoded = ClockRequest::decode(request).expect("a canonical frame");
        self.requests.push(decoded);
        if let Some(reason) = self.refuse {
            return encode_error_reply(reply, reason);
        }
        match decoded {
            ClockRequest::Describe(_) => encode_describe_reply(
                reply,
                ClockState {
                    hz: 100_000_000,
                    held_elsewhere: true,
                },
            ),
            ClockRequest::Run { hz, .. } => encode_run_reply(reply, hz - 1),
            ClockRequest::Release(_) => encode_release_reply(reply),
        }
    }

    fn post(&mut self, _request: &[u8], _deadline_ns: u64) -> Result<u64, Errno> {
        panic!("no clock request is posted")
    }

    fn reap(&mut self, _ticket: u64, _reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        panic!("no clock request is posted")
    }
}

#[test]
fn every_request_names_the_link_and_the_reply_is_decoded() {
    let mut controller = Controller::default();
    let mut clock = ClockClient::new(&mut controller, link());
    assert_eq!(
        clock.describe(),
        Ok(ClockState {
            hz: 100_000_000,
            held_elsewhere: true,
        })
    );
    assert_eq!(
        clock.run(100_000_000),
        Ok(99_999_999),
        "the rate it runs at"
    );
    assert_eq!(clock.release(), Ok(()));
    assert_eq!(
        controller.requests,
        [
            ClockRequest::Describe(link()),
            ClockRequest::Run {
                link: link(),
                hz: 100_000_000,
            },
            ClockRequest::Release(link()),
        ]
    );
}

#[test]
fn a_refusal_comes_back_as_the_controllers_reason() {
    let mut controller = Controller {
        refuse: Some(Errno::Busy),
        ..Controller::default()
    };
    let mut clock = ClockClient::new(&mut controller, link());
    assert_eq!(clock.run(50_000_000), Err(Errno::Busy));
}
