//! A `clock-v1` endpoint under test: a driver's clock hardware behind the
//! controller, its callers answered through the fake kernel, and every
//! decision it records.

use alloc::vec::Vec;

use tairix_abi::driver::clock::{
    decode_describe_reply, decode_release_reply, decode_run_reply, ClockRequest, ClockState,
    CLOCK_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{Errno, ProcId};
use tairix_drvrt::testkit::{FakeKernel, Records};
use tairix_drvrt::Supplier;

use crate::{ClockHardware, Controller, Record};

/// An endpoint over `H`, and the kernel its callers hold their links in.
pub struct Rig<H: ClockHardware> {
    /// The kernel the endpoint's callers are answered through.
    pub kernel: FakeKernel,
    records: Records<Record>,
    /// The endpoint.
    pub controller: Controller<H, FakeKernel, Records<Record>>,
}

impl<H: ClockHardware> Rig<H> {
    /// The endpoint `endpoint` over `hardware`, each of `holders` granted the
    /// link it is paired with.
    ///
    /// # Panics
    ///
    /// When the clocks' table cannot be made.
    #[must_use]
    pub fn new(hardware: H, endpoint: u64, holders: &[(ProcId, LinkRequest)]) -> Self {
        let kernel = FakeKernel::new();
        for (holder, link) in holders {
            kernel.grant(*holder, HwResource::request(link));
        }
        let records = Records::default();
        let controller = Controller::new(hardware, kernel.clone(), records.clone(), endpoint)
            .expect("the clocks' table fits");
        Self {
            kernel,
            records,
            controller,
        }
    }

    /// Serve `frame` from `caller`, answering its reply.
    ///
    /// # Panics
    ///
    /// When the endpoint leaves the call unanswered.
    pub fn serve(&mut self, caller: ProcId, frame: &[u8]) -> Vec<u8> {
        let ticket = self.kernel.call(caller);
        self.controller.serve(ticket, frame);
        self.kernel
            .answer(ticket)
            .expect("every call is answered at once")
    }

    /// Serve `frame` as a call the kernel names no caller of.
    pub fn serve_out_of_service(&mut self, frame: &[u8]) {
        self.controller.serve(FakeKernel::OUT_OF_SERVICE, frame);
    }

    /// Serve `request` from `caller`, answering its reply.
    ///
    /// # Panics
    ///
    /// As [`serve`](Self::serve).
    pub fn call(&mut self, caller: ProcId, request: &ClockRequest) -> Vec<u8> {
        let mut frame = [0u8; CLOCK_MAX_REQUEST];
        let len = request
            .encode(&mut frame)
            .expect("a request fits its frame");
        self.serve(caller, &frame[..len])
    }

    /// `caller` runs `link`'s clock as near `hz` as it can be made.
    ///
    /// # Errors
    ///
    /// The endpoint's refusal.
    pub fn run(&mut self, caller: ProcId, link: LinkRequest, hz: u64) -> Result<u64, Errno> {
        decode_run_reply(&self.call(caller, &ClockRequest::Run { link, hz }))
    }

    /// `caller` floors `link`'s clock at `hz`.
    ///
    /// # Errors
    ///
    /// The endpoint's refusal.
    pub fn floor(&mut self, caller: ProcId, link: LinkRequest, hz: u64) -> Result<u64, Errno> {
        decode_run_reply(&self.call(caller, &ClockRequest::Floor { link, hz }))
    }

    /// `caller` gives up its hold on `link`'s clock.
    ///
    /// # Errors
    ///
    /// The endpoint's refusal.
    pub fn release(&mut self, caller: ProcId, link: LinkRequest) -> Result<(), Errno> {
        decode_release_reply(&self.call(caller, &ClockRequest::Release(link)))
    }

    /// `caller` asks after `link`'s clock.
    ///
    /// # Errors
    ///
    /// The endpoint's refusal.
    pub fn describe(&mut self, caller: ProcId, link: LinkRequest) -> Result<ClockState, Errno> {
        decode_describe_reply(&self.call(caller, &ClockRequest::Describe(link)))
    }

    /// End `peer`, and serve its exit.
    pub fn end(&mut self, peer: ProcId) {
        self.kernel.end(peer);
        self.controller.peer_exited(peer);
    }

    /// Every decision recorded, oldest first.
    #[must_use]
    pub fn records(&self) -> Vec<Record> {
        self.records.all()
    }

    /// The reason of every refusal recorded, oldest first.
    #[must_use]
    pub fn refusals(&self) -> Vec<Errno> {
        self.records()
            .into_iter()
            .filter_map(|record| match record {
                Record::Refused { reason, .. } => Some(reason),
                _ => None,
            })
            .collect()
    }
}
