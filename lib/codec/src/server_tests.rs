//! The server driven with a kernel that answers the grant questions by the
//! kernel's own rule, over a codec that records what it was asked.

extern crate std;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::driver::audio::{GainRange, Rate, RateSupport};
use tairix_abi::driver::codec::{
    decode_describe_reply, decode_done_reply, decode_gain_reply, ClockInversion, Codec, CodecFacts,
    CodecRequest, DaiFormat, DaiFormats, DaiLink, SampleWidths, CODEC_ENDPOINTS, CODEC_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{DriverError, Errno, ProcId, PROC_ID_LEN};
use tairix_drvrt::SupplierHost;

use crate::{CodecServer, Record, Recorder};

const fn instance(byte: u8) -> ProcId {
    ProcId::from_raw([byte; PROC_ID_LEN])
}

const I2S: ProcId = instance(1);
const OTHER: ProcId = instance(2);

fn endpoint() -> u64 {
    CODEC_ENDPOINTS.endpoint(40)
}

fn dai(format: DaiFormat) -> DaiLink {
    DaiLink {
        format,
        codec_drives_bit_clock: false,
        codec_drives_frame_clock: false,
        inversion: ClockInversion::Normal,
        cpu_dai: 0,
        codec_dai: 0,
    }
}

fn link(format: DaiFormat, index: u8) -> LinkRequest {
    LinkRequest::new(endpoint(), index, &dai(format).to_cells(), b"").expect("valid")
}

fn rate() -> Rate {
    Rate::new(48_000).expect("a rate")
}

#[derive(Default)]
struct Kernel {
    callers: BTreeMap<u64, ProcId>,
    next_ticket: u64,
    holdings: Vec<(ProcId, HwResource)>,
    ended: Vec<ProcId>,
    watched: Vec<ProcId>,
    replies: Vec<(u64, Vec<u8>)>,
    records: Vec<Record>,
}

#[derive(Clone)]
struct Host(Rc<RefCell<Kernel>>);

impl SupplierHost for Host {
    fn caller(&self, ticket: u64) -> Result<ProcId, Errno> {
        self.0
            .borrow()
            .callers
            .get(&ticket)
            .copied()
            .ok_or(Errno::NotFound)
    }
    fn caller_holds(&self, ticket: u64, record: &HwResource) -> Result<bool, Errno> {
        let kernel = self.0.borrow();
        let caller = kernel.callers.get(&ticket).ok_or(Errno::NotFound)?;
        Ok(kernel
            .holdings
            .iter()
            .any(|(holder, grant)| holder == caller && grant.covers(record)))
    }
    fn reply(&mut self, ticket: u64, frame: &[u8]) -> Result<(), Errno> {
        self.0.borrow_mut().replies.push((ticket, frame.to_vec()));
        Ok(())
    }
    fn watch(&mut self, peer: ProcId) -> Result<(), Errno> {
        let mut kernel = self.0.borrow_mut();
        if kernel.ended.contains(&peer) {
            return Err(Errno::NotFound);
        }
        if !kernel.watched.contains(&peer) {
            kernel.watched.push(peer);
        }
        Ok(())
    }
    fn unwatch(&mut self, peer: ProcId) {
        self.0
            .borrow_mut()
            .watched
            .retain(|watched| *watched != peer);
    }
}

impl Recorder for Host {
    fn record(&mut self, record: Record) {
        self.0.borrow_mut().records.push(record);
    }
}

/// What the codec was asked, in order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Call {
    Configure(DaiFormat, u32, u8),
    Gain(i32, bool),
    Start,
    Stop,
}

/// A codec taking I2S alone, its gain in half-decibel steps.
#[derive(Clone)]
struct Part(Rc<RefCell<Vec<Call>>>);

impl Codec for Part {
    fn facts(&self) -> CodecFacts {
        CodecFacts {
            rates: RateSupport::Continuous {
                min: Rate::new(8_000).expect("a rate"),
                max: Rate::new(192_000).expect("a rate"),
            },
            widths: SampleWidths::EMPTY.with(32).expect("a width"),
            formats: DaiFormats::EMPTY.with(DaiFormat::I2s),
            drives_clocks: false,
            gain: Some(GainRange::new(-10_300, 2_400, 50).expect("range")),
        }
    }
    fn configure(&mut self, link: &DaiLink, rate: Rate, width: u8) -> Result<(), DriverError> {
        if link.format != DaiFormat::I2s {
            return Err(DriverError::Unsupported);
        }
        self.0
            .borrow_mut()
            .push(Call::Configure(link.format, rate.hz(), width));
        Ok(())
    }
    fn set_gain(&mut self, millibel: i32, mute: bool) -> Result<i32, DriverError> {
        self.0.borrow_mut().push(Call::Gain(millibel, mute));
        Ok(-(-millibel).div_euclid(50) * 50)
    }
    fn start(&mut self) -> Result<(), DriverError> {
        self.0.borrow_mut().push(Call::Start);
        Ok(())
    }
    fn stop(&mut self) -> Result<(), DriverError> {
        self.0.borrow_mut().push(Call::Stop);
        Ok(())
    }
}

struct Rig {
    kernel: Rc<RefCell<Kernel>>,
    calls: Rc<RefCell<Vec<Call>>>,
    server: CodecServer<Part, Host, Host>,
}

impl Rig {
    fn new() -> Self {
        let kernel = Rc::new(RefCell::new(Kernel::default()));
        for (holder, index) in [(I2S, 0), (OTHER, 1)] {
            kernel
                .borrow_mut()
                .holdings
                .push((holder, HwResource::request(&link(DaiFormat::I2s, index))));
        }
        let calls = Rc::new(RefCell::new(Vec::new()));
        let server = CodecServer::new(
            Part(calls.clone()),
            Host(kernel.clone()),
            Host(kernel.clone()),
            endpoint(),
        );
        Self {
            kernel,
            calls,
            server,
        }
    }

    fn call(&mut self, caller: ProcId, request: &CodecRequest) -> Vec<u8> {
        let mut frame = [0u8; CODEC_MAX_REQUEST];
        let len = request.encode(&mut frame).expect("fits");
        self.raw(caller, &frame[..len])
    }

    fn raw(&mut self, caller: ProcId, frame: &[u8]) -> Vec<u8> {
        let ticket = {
            let mut kernel = self.kernel.borrow_mut();
            let ticket = kernel.next_ticket;
            kernel.next_ticket += 1;
            kernel.callers.insert(ticket, caller);
            ticket
        };
        self.server.serve(ticket, frame);
        let mut kernel = self.kernel.borrow_mut();
        kernel.callers.remove(&ticket);
        let (answered, reply) = kernel.replies.pop().expect("every call is answered");
        assert_eq!(answered, ticket);
        reply
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }
}

fn configure(index: u8) -> CodecRequest {
    CodecRequest::Configure {
        link: link(DaiFormat::I2s, index),
        rate: rate(),
        width: 32,
    }
}

#[test]
fn any_attested_caller_learns_what_the_codec_accepts_without_taking_it() {
    let mut rig = Rig::new();
    let facts =
        decode_describe_reply(&rig.call(OTHER, &CodecRequest::Describe(link(DaiFormat::I2s, 1))))
            .expect("facts");
    assert!(facts.formats.contains(DaiFormat::I2s));
    assert!(rig.kernel.borrow().watched.is_empty(), "nothing held");
}

#[test]
fn configuring_takes_the_codec_in_the_attested_links_framing() {
    let mut rig = Rig::new();
    assert_eq!(decode_done_reply(&rig.call(I2S, &configure(0))), Ok(()));
    assert_eq!(rig.calls(), [Call::Configure(DaiFormat::I2s, 48_000, 32)]);
    assert_eq!(rig.kernel.borrow().watched, [I2S]);
    assert_eq!(rig.kernel.borrow().records, [Record::Held { holder: I2S }]);
    assert_eq!(
        decode_gain_reply(&rig.call(
            I2S,
            &CodecRequest::Gain {
                link: link(DaiFormat::I2s, 0),
                millibel: -625,
                mute: false,
            }
        )),
        Ok(-600),
        "the gain the codec set, at or above the one asked"
    );
}

#[test]
fn another_live_driver_is_refused_until_the_holder_ends() {
    let mut rig = Rig::new();
    rig.call(I2S, &configure(0));
    assert_eq!(
        decode_done_reply(&rig.call(OTHER, &configure(1))),
        Err(Errno::Busy)
    );
    rig.kernel.borrow_mut().ended.push(I2S);
    rig.server.peer_exited(I2S);
    assert_eq!(
        rig.calls().last(),
        Some(&Call::Stop),
        "stopped with its holder"
    );
    assert_eq!(decode_done_reply(&rig.call(OTHER, &configure(1))), Ok(()));
}

#[test]
fn a_restarted_driver_takes_over_from_a_holder_whose_end_was_not_yet_served() {
    let mut rig = Rig::new();
    rig.call(I2S, &configure(0));
    rig.kernel.borrow_mut().ended.push(I2S);
    assert_eq!(decode_done_reply(&rig.call(OTHER, &configure(1))), Ok(()));
    assert!(rig
        .kernel
        .borrow()
        .records
        .contains(&Record::Abandoned { holder: I2S }));
}

#[test]
fn an_unattested_link_another_endpoint_or_a_bad_frame_is_refused() {
    let mut rig = Rig::new();
    let unheld = link(DaiFormat::I2s, 7);
    assert_eq!(
        decode_done_reply(&rig.call(I2S, &CodecRequest::Start(unheld))),
        Err(Errno::PermissionDenied)
    );
    let elsewhere = LinkRequest::new(
        CODEC_ENDPOINTS.endpoint(41),
        0,
        &dai(DaiFormat::I2s).to_cells(),
        b"",
    )
    .expect("valid");
    assert_eq!(
        decode_done_reply(&rig.call(I2S, &CodecRequest::Start(elsewhere))),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        decode_done_reply(&rig.raw(I2S, b"not a codec frame")),
        Err(Errno::BadMagic)
    );
    assert!(rig.calls().is_empty());
}

#[test]
fn a_framing_the_codec_refuses_comes_back_as_not_supported() {
    let mut rig = Rig::new();
    let left = link(DaiFormat::LeftJustified, 2);
    rig.kernel
        .borrow_mut()
        .holdings
        .push((I2S, HwResource::request(&left)));
    assert_eq!(
        decode_done_reply(&rig.call(
            I2S,
            &CodecRequest::Configure {
                link: left,
                rate: rate(),
                width: 32,
            }
        )),
        Err(Errno::NotSupported)
    );
}
