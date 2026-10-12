//! The interrupt-line client driven against a controller that decodes every
//! frame.

extern crate std;

use std::cell::RefCell;
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::driver::irqline::{encode_reply, IrqLineRequest, INTERRUPT_LINE_ENDPOINTS};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::Errno;

use crate::{IrqLineClient, LinkCall};

fn line() -> LinkRequest {
    LinkRequest::new(INTERRUPT_LINE_ENDPOINTS.endpoint(21), 4, &[4], b"").expect("valid")
}

/// A controller answering posted waits only when the test fires the line.
#[derive(Default)]
struct Controller {
    requests: Vec<IrqLineRequest>,
    refuse: Option<Errno>,
    fired: Vec<u64>,
    next: u64,
}

#[derive(Clone, Default)]
struct Shared(Rc<RefCell<Controller>>);

impl LinkCall for Shared {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let mut controller = self.0.borrow_mut();
        controller
            .requests
            .push(IrqLineRequest::decode(request).expect("a canonical frame"));
        encode_reply(reply, controller.refuse.map_or(Ok(()), Err))
    }

    fn post(&mut self, request: &[u8], _deadline_ns: u64) -> Result<u64, Errno> {
        let mut controller = self.0.borrow_mut();
        controller
            .requests
            .push(IrqLineRequest::decode(request).expect("a canonical frame"));
        controller.next += 1;
        Ok(controller.next)
    }

    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        let controller = self.0.borrow();
        if !controller.fired.contains(&ticket) {
            return Ok(None);
        }
        encode_reply(reply, controller.refuse.map_or(Ok(()), Err)).map(Some)
    }
}

#[test]
fn a_blocking_wait_names_the_line_and_returns_when_it_fires() {
    let controller = Shared::default();
    let mut client = IrqLineClient::new(controller.clone(), line());
    assert_eq!(client.wait(), Ok(()));
    assert_eq!(client.line(), &line());
    assert_eq!(
        controller.0.borrow().requests,
        [IrqLineRequest::Wait(line())]
    );
}

#[test]
fn a_posted_wait_is_reaped_once_the_line_fires_and_only_once() {
    let controller = Shared::default();
    let mut client = IrqLineClient::new(controller.clone(), line());
    assert_eq!(client.reap_wait(), Ok(false), "nothing posted");
    client.post_wait(1_000).expect("posts");
    assert!(client.is_waiting());
    assert_eq!(client.post_wait(1_000), Err(Errno::Busy));
    assert_eq!(client.wait(), Err(Errno::Busy));
    assert_eq!(client.reap_wait(), Ok(false), "pending");
    controller.0.borrow_mut().fired.push(1);
    assert_eq!(client.reap_wait(), Ok(true));
    assert!(!client.is_waiting());
    assert_eq!(client.reap_wait(), Ok(false), "spent");
}

#[test]
fn a_refused_wait_is_spent_and_reports_the_refusal() {
    let controller = Shared::default();
    controller.0.borrow_mut().refuse = Some(Errno::NotFound);
    let mut client = IrqLineClient::new(controller.clone(), line());
    assert_eq!(client.wait(), Err(Errno::NotFound));
    client.post_wait(1_000).expect("posts");
    controller.0.borrow_mut().fired.push(1);
    assert_eq!(client.reap_wait(), Err(Errno::NotFound));
    assert!(!client.is_waiting());
}
