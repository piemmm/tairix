//! The server answered through the shared fake kernel, over an encoder that
//! records what it was asked.

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::driver::display::{Axis, Polarity, Scan, Timing};
use tairix_abi::driver::encoder::{
    decode_done_reply, decode_state_reply, Connector, Encoder, EncoderRequest, Phase, PipeLimits,
    ENCODER_ENDPOINTS, ENCODER_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{DriverError, Errno, ProcId, PROC_ID_LEN};
use tairix_drvrt::testkit::{FakeKernel, Records};
use tairix_drvrt::Supplier;

use crate::{EncoderServer, Record};

const fn instance(byte: u8) -> ProcId {
    ProcId::from_raw([byte; PROC_ID_LEN])
}

const ENGINE: ProcId = instance(1);
const OTHER: ProcId = instance(2);

fn endpoint() -> u64 {
    ENCODER_ENDPOINTS.endpoint(12)
}

fn link(index: u8) -> LinkRequest {
    LinkRequest::new(endpoint(), index, &[], b"hdmi0").expect("valid")
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

const SLOW: Timing = Timing {
    clock_khz: 74_250,
    horizontal: Axis {
        active: 1280,
        front: 110,
        sync: 40,
        back: 220,
    },
    vertical: Axis {
        active: 720,
        front: 5,
        sync: 5,
        back: 20,
    },
    ..MODE
};

/// What the encoder was asked, in order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Call {
    Configure,
    Prepare,
    Enable,
    Disable,
    PowerDown,
}

/// What the test sets the encoder's hardware to answer.
struct Hardware {
    connector: Connector,
    calls: Vec<Call>,
    /// The next event moves the connector.
    moves: bool,
    /// A configure fails with this.
    refuses: Option<DriverError>,
    /// The sink's best mode, which the connector runs while the pipe
    /// carries it.
    best: Timing,
    /// What the sink falls back to.
    fallback: Timing,
    /// When the encoder next needs its check.
    deadline: Option<u64>,
    /// The check moves the connector.
    check_moves: bool,
}

#[derive(Clone)]
struct Part {
    hardware: Rc<RefCell<Hardware>>,
    phase: Phase,
}

impl Encoder for Part {
    fn connector(&self) -> Connector {
        self.hardware.borrow().connector
    }
    fn phase(&self) -> Phase {
        self.phase
    }
    fn limit(&mut self, limits: &PipeLimits) -> Result<bool, DriverError> {
        let mut hardware = self.hardware.borrow_mut();
        let mode = if limits.carries(&hardware.best) {
            hardware.best
        } else {
            hardware.fallback
        };
        let moved = hardware.connector.mode != Some(mode);
        hardware.connector.mode = Some(mode);
        Ok(moved)
    }

    fn service_event(&mut self, _index: usize) -> Result<bool, DriverError> {
        Ok(core::mem::take(&mut self.hardware.borrow_mut().moves))
    }
    fn deadline_us(&self) -> Option<u64> {
        self.hardware.borrow().deadline
    }
    fn expired(&mut self) -> Result<bool, DriverError> {
        let mut hardware = self.hardware.borrow_mut();
        hardware.deadline = None;
        Ok(core::mem::take(&mut hardware.check_moves))
    }
    fn configure(&mut self, timing: &Timing) -> Result<(), DriverError> {
        let mut hardware = self.hardware.borrow_mut();
        assert_eq!(Some(*timing), hardware.connector.mode);
        if let Some(err) = hardware.refuses {
            return Err(err);
        }
        hardware.calls.push(Call::Configure);
        Ok(())
    }
    fn prepare(&mut self) -> Result<(), DriverError> {
        self.hardware.borrow_mut().calls.push(Call::Prepare);
        Ok(())
    }
    fn enable(&mut self) -> Result<(), DriverError> {
        self.hardware.borrow_mut().calls.push(Call::Enable);
        Ok(())
    }
    fn disable(&mut self) -> Result<(), DriverError> {
        self.hardware.borrow_mut().calls.push(Call::Disable);
        Ok(())
    }
    fn power_down(&mut self) -> Result<(), DriverError> {
        self.hardware.borrow_mut().calls.push(Call::PowerDown);
        Ok(())
    }
}

struct Rig {
    kernel: FakeKernel,
    records: Records<Record>,
    hardware: Rc<RefCell<Hardware>>,
    server: EncoderServer<Part, FakeKernel, Records<Record>>,
}

impl Rig {
    fn new(phase: Phase) -> Self {
        let kernel = FakeKernel::new();
        for (holder, index) in [(ENGINE, 0), (OTHER, 1)] {
            kernel.grant(holder, HwResource::request(&link(index)));
        }
        let records = Records::default();
        let hardware = Rc::new(RefCell::new(Hardware {
            connector: Connector {
                connected: true,
                mode: Some(MODE),
                link: 0,
            },
            calls: Vec::new(),
            moves: false,
            refuses: None,
            best: MODE,
            fallback: SLOW,
            deadline: None,
            check_moves: false,
        }));
        let server = EncoderServer::new(
            Part {
                hardware: hardware.clone(),
                phase,
            },
            kernel.clone(),
            records.clone(),
            endpoint(),
        );
        Self {
            kernel,
            records,
            hardware,
            server,
        }
    }

    fn post(&mut self, caller: ProcId, request: &EncoderRequest) -> u64 {
        let mut frame = [0u8; ENCODER_MAX_REQUEST];
        let len = request.encode(&mut frame).expect("fits");
        let ticket = self.kernel.call(caller);
        self.server.serve(ticket, &frame[..len]);
        ticket
    }

    fn call(&mut self, caller: ProcId, request: &EncoderRequest) -> Vec<u8> {
        let ticket = self.post(caller, request);
        self.kernel.answer(ticket).expect("answered at once")
    }

    fn step(&mut self, request: &EncoderRequest) -> Result<(), Errno> {
        decode_done_reply(&self.call(ENGINE, request))
    }

    fn phase(&mut self) -> Phase {
        decode_state_reply(&self.call(OTHER, &EncoderRequest::Describe(link(1))))
            .expect("a state")
            .phase
    }

    fn calls(&self) -> Vec<Call> {
        self.hardware.borrow().calls.clone()
    }
}

fn configure() -> EncoderRequest {
    EncoderRequest::Configure {
        link: link(0),
        timing: MODE,
    }
}

#[test]
fn any_attested_caller_reads_the_connector_without_taking_the_encoder() {
    let mut rig = Rig::new(Phase::Off);
    let state =
        decode_state_reply(&rig.call(OTHER, &EncoderRequest::Describe(link(1)))).expect("state");
    assert_eq!(state.connector.mode, Some(MODE));
    assert_eq!(state.phase, Phase::Off);
    assert!(rig.kernel.watched().is_empty(), "nothing held");
}

#[test]
fn a_mode_set_goes_up_and_comes_down_in_its_order() {
    let mut rig = Rig::new(Phase::Off);
    for (request, phase) in [
        (configure(), Phase::Configured),
        (EncoderRequest::Prepare(link(0)), Phase::Prepared),
        (EncoderRequest::Enable(link(0)), Phase::Enabled),
        (EncoderRequest::Disable(link(0)), Phase::Disabled),
        (EncoderRequest::PowerDown(link(0)), Phase::Off),
    ] {
        assert_eq!(rig.step(&request), Ok(()));
        assert_eq!(rig.phase(), phase);
    }
    assert_eq!(
        rig.calls(),
        [
            Call::Configure,
            Call::Prepare,
            Call::Enable,
            Call::Disable,
            Call::PowerDown
        ]
    );
    assert_eq!(rig.records.all()[0], Record::Held { holder: ENGINE });
}

#[test]
fn a_step_out_of_order_is_refused_and_reaches_no_hardware() {
    let mut rig = Rig::new(Phase::Off);
    assert_eq!(
        rig.step(&EncoderRequest::Enable(link(0))),
        Err(Errno::OutOfRange)
    );
    assert_eq!(rig.step(&configure()), Ok(()));
    assert_eq!(rig.step(&configure()), Err(Errno::OutOfRange));
    assert_eq!(
        rig.step(&EncoderRequest::Disable(link(0))),
        Err(Errno::OutOfRange)
    );
    assert_eq!(rig.calls(), [Call::Configure]);
    assert_eq!(rig.phase(), Phase::Configured);
}

#[test]
fn a_mode_that_is_not_the_connectors_is_stale() {
    let mut rig = Rig::new(Phase::Off);
    let other = Timing {
        clock_khz: 74_250,
        ..MODE
    };
    assert_eq!(
        rig.step(&EncoderRequest::Configure {
            link: link(0),
            timing: other,
        }),
        Err(Errno::Stale)
    );
    rig.hardware.borrow_mut().connector = Connector {
        connected: false,
        mode: None,
        link: 0,
    };
    assert_eq!(rig.step(&configure()), Err(Errno::Stale));
    assert!(rig.calls().is_empty());
    assert_eq!(rig.phase(), Phase::Off);
}

/// The firmware leaves its pipe running: an engine taking over finds the
/// output sending, and must bring it down in order before setting a mode.
#[test]
fn the_server_starts_from_the_phase_the_hardware_is_in() {
    let mut rig = Rig::new(Phase::Enabled);
    assert_eq!(rig.phase(), Phase::Enabled);
    assert_eq!(rig.step(&configure()), Err(Errno::OutOfRange));
    assert_eq!(
        rig.step(&EncoderRequest::PowerDown(link(0))),
        Err(Errno::OutOfRange),
        "not while sending"
    );
    assert_eq!(rig.step(&EncoderRequest::Disable(link(0))), Ok(()));
    assert_eq!(rig.step(&EncoderRequest::PowerDown(link(0))), Ok(()));
    assert_eq!(rig.step(&configure()), Ok(()));
}

#[test]
fn powering_down_what_is_already_off_changes_nothing() {
    let mut rig = Rig::new(Phase::Off);
    assert_eq!(rig.step(&EncoderRequest::PowerDown(link(0))), Ok(()));
    assert!(rig.calls().is_empty());
}

#[test]
fn a_device_refusal_leaves_the_phase_where_it_was() {
    let mut rig = Rig::new(Phase::Off);
    rig.hardware.borrow_mut().refuses = Some(DriverError::Unsupported);
    assert_eq!(rig.step(&configure()), Err(Errno::NotSupported));
    assert_eq!(rig.phase(), Phase::Off);
}

#[test]
fn a_posted_wait_is_answered_when_the_connector_moves() {
    let mut rig = Rig::new(Phase::Off);
    let ticket = rig.post(
        ENGINE,
        &EncoderRequest::Wait {
            link: link(0),
            seen: 0,
        },
    );
    assert_eq!(rig.kernel.answer(ticket), None, "posted");
    assert_eq!(rig.server.woke(0), Ok(()));
    assert_eq!(
        rig.kernel.answer(ticket),
        None,
        "an event that moved nothing"
    );
    {
        let mut hardware = rig.hardware.borrow_mut();
        hardware.moves = true;
        hardware.connector = Connector {
            connected: false,
            mode: None,
            link: 0,
        };
    }
    assert_eq!(rig.server.woke(0), Ok(()));
    let state = decode_state_reply(&rig.kernel.answer(ticket).expect("answered")).expect("state");
    assert_eq!(state.moves, 1);
    assert!(!state.connector.connected);
    assert_eq!(
        rig.records.all().last(),
        Some(&Record::Moved {
            moves: 1,
            connected: false
        })
    );
}

#[test]
fn the_encoder_s_deadline_is_the_server_s_and_a_check_that_moves_answers_the_wait() {
    let mut rig = Rig::new(Phase::Off);
    assert_eq!(rig.server.deadline_us(), None);
    rig.hardware.borrow_mut().deadline = Some(1_000_000);
    assert_eq!(rig.server.deadline_us(), Some(1_000_000));
    let ticket = rig.post(
        ENGINE,
        &EncoderRequest::Wait {
            link: link(0),
            seen: 0,
        },
    );
    assert_eq!(rig.server.expired(), Ok(()));
    assert_eq!(rig.server.deadline_us(), None, "the check cleared it");
    assert_eq!(
        rig.kernel.answer(ticket),
        None,
        "a check that moved nothing"
    );
    rig.hardware.borrow_mut().check_moves = true;
    assert_eq!(rig.server.expired(), Ok(()));
    let state = decode_state_reply(&rig.kernel.answer(ticket).expect("answered")).expect("state");
    assert_eq!(state.moves, 1);
}

#[test]
fn a_wait_behind_the_count_is_answered_at_once_and_one_waits_at_a_time() {
    let mut rig = Rig::new(Phase::Off);
    let state = decode_state_reply(&rig.call(
        ENGINE,
        &EncoderRequest::Wait {
            link: link(0),
            seen: 9,
        },
    ))
    .expect("answered at once");
    assert_eq!(state.moves, 0);
    let _posted = rig.post(
        ENGINE,
        &EncoderRequest::Wait {
            link: link(0),
            seen: 0,
        },
    );
    assert_eq!(
        decode_done_reply(&rig.call(
            ENGINE,
            &EncoderRequest::Wait {
                link: link(0),
                seen: 0,
            }
        )),
        Err(Errno::Busy)
    );
}

#[test]
fn another_engine_is_refused_until_the_holder_ends_and_the_phase_stands() {
    let mut rig = Rig::new(Phase::Off);
    assert_eq!(rig.step(&configure()), Ok(()));
    let posted = rig.post(
        ENGINE,
        &EncoderRequest::Wait {
            link: link(0),
            seen: 0,
        },
    );
    let other_configure = EncoderRequest::Prepare(link(1));
    assert_eq!(
        decode_done_reply(&rig.call(OTHER, &other_configure)),
        Err(Errno::Busy)
    );
    rig.kernel.end(ENGINE);
    rig.server.peer_exited(ENGINE);
    assert_eq!(
        rig.records.all().last(),
        Some(&Record::Abandoned { holder: ENGINE })
    );
    rig.hardware.borrow_mut().moves = true;
    assert_eq!(rig.server.woke(0), Ok(()));
    assert_eq!(rig.kernel.answer(posted), None, "the ended wait is gone");
    assert_eq!(rig.phase(), Phase::Configured, "the phase stands");
    assert_eq!(
        decode_done_reply(&rig.call(OTHER, &other_configure)),
        Ok(())
    );
}

#[test]
fn a_caller_without_the_link_or_with_another_encoders_is_refused() {
    let mut rig = Rig::new(Phase::Off);
    let stranger = instance(9);
    assert_eq!(
        decode_done_reply(&rig.call(stranger, &configure())),
        Err(Errno::PermissionDenied)
    );
    let elsewhere = LinkRequest::new(ENCODER_ENDPOINTS.endpoint(13), 0, &[], b"").expect("valid");
    assert_eq!(
        decode_done_reply(&rig.call(ENGINE, &EncoderRequest::Prepare(elsewhere))),
        Err(Errno::OutOfRange)
    );
    assert!(rig.calls().is_empty());
}

#[test]
fn a_pipe_that_cannot_carry_the_mode_moves_the_connector_once() {
    let mut rig = Rig::new(Phase::Off);
    let posted = rig.post(
        ENGINE,
        &EncoderRequest::Wait {
            link: link(0),
            seen: 0,
        },
    );
    let narrow = PipeLimits {
        max_clock_khz: 100_000,
        max_active_khz: 100_000,
    };
    let limit = |index, limits| EncoderRequest::Limit {
        link: link(index),
        limits,
    };
    assert_eq!(rig.step(&limit(0, narrow)), Ok(()));
    let state = decode_state_reply(&rig.kernel.answer(posted).expect("answered")).expect("state");
    assert_eq!((state.moves, state.connector.mode), (1, Some(SLOW)));
    assert_eq!(rig.step(&limit(0, narrow)), Ok(()));
    assert_eq!(
        decode_state_reply(&rig.call(OTHER, &EncoderRequest::Describe(link(1))))
            .expect("state")
            .moves,
        1,
        "the same limit moves nothing"
    );
    assert_eq!(
        rig.step(&EncoderRequest::Configure {
            link: link(0),
            timing: MODE,
        }),
        Err(Errno::Stale),
        "the mode the pipe cannot carry is no longer the connector's"
    );
    assert_eq!(
        decode_done_reply(&rig.call(OTHER, &limit(1, narrow))),
        Err(Errno::Busy),
        "the limit is the holder's to state"
    );
}
