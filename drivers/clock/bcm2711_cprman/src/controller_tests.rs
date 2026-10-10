//! The endpoint driven over the real generators and the register-level model,
//! with a kernel that answers the grant questions by the kernel's own rule.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::driver::clock::{
    decode_describe_reply, decode_release_reply, decode_run_reply, ClockOp, ClockRequest,
    ClockState, CLOCK_CONTROLLER_ENDPOINTS, CLOCK_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{Errno, ProcId, PROC_ID_LEN};
use tairix_drvrt::SupplierHost;
use tairix_fuzzseed::Prng;

use crate::controller::{Controller, Record, Recorder, CLOCK_PCM, CLOCK_PWM};
use crate::cprman::Cprman;
use crate::model::{Model, Stopping, OSCILLATOR, PCM_CTL, PWM_CTL, SOURCE_OSCILLATOR};

const fn instance(byte: u8) -> ProcId {
    ProcId::from_raw([byte; PROC_ID_LEN])
}

const PWM0: ProcId = instance(1);
const PWM1: ProcId = instance(2);
const I2S: ProcId = instance(3);

/// The consumer nodes' positions in the `clocks` lists, distinct per node so
/// each link is its own grant.
const PWM0_NODE: u8 = 0;
const PWM1_NODE: u8 = 1;
const I2S_NODE: u8 = 2;

fn endpoint() -> u64 {
    CLOCK_CONTROLLER_ENDPOINTS.endpoint(8)
}

fn link(clock: u32, node: u8) -> LinkRequest {
    LinkRequest::new(endpoint(), node, &[clock], b"").expect("valid")
}

fn pwm0() -> LinkRequest {
    link(CLOCK_PWM, PWM0_NODE)
}

fn pwm1() -> LinkRequest {
    link(CLOCK_PWM, PWM1_NODE)
}

fn i2s() -> LinkRequest {
    link(CLOCK_PCM, I2S_NODE)
}

fn run(link: LinkRequest, hz: u64) -> ClockRequest {
    ClockRequest::Run { link, hz }
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

struct Rig<'m> {
    kernel: Rc<RefCell<Kernel>>,
    controller: Controller<'m, Model, Host, Host>,
}

impl<'m> Rig<'m> {
    /// The endpoint over `model`, with the PWM blocks' drivers holding their
    /// links to the PWM clock and the I2S driver its link to the PCM clock.
    fn new(model: &'m Model) -> Self {
        let kernel = Rc::new(RefCell::new(Kernel::default()));
        for (holder, held) in [(PWM0, pwm0()), (PWM1, pwm1()), (I2S, i2s())] {
            kernel
                .borrow_mut()
                .holdings
                .push((holder, HwResource::request(&held)));
        }
        let cprman = Cprman::new(model, OSCILLATOR).expect("valid");
        let controller = Controller::new(
            cprman,
            Host(kernel.clone()),
            Host(kernel.clone()),
            endpoint(),
        );
        Self { kernel, controller }
    }

    fn call(&mut self, caller: ProcId, request: &ClockRequest) -> Vec<u8> {
        let mut frame = [0u8; CLOCK_MAX_REQUEST];
        let len = request.encode(&mut frame).expect("fits");
        self.raw(Some(caller), &frame[..len])
    }

    /// Serve `frame` from `caller`, or as a call the kernel can name no
    /// caller of, answering the reply.
    fn raw(&mut self, caller: Option<ProcId>, frame: &[u8]) -> Vec<u8> {
        let ticket = {
            let mut kernel = self.kernel.borrow_mut();
            let ticket = kernel.next_ticket;
            kernel.next_ticket += 1;
            if let Some(caller) = caller {
                kernel.callers.insert(ticket, caller);
            }
            ticket
        };
        self.controller.serve(ticket, frame);
        let mut kernel = self.kernel.borrow_mut();
        kernel.callers.remove(&ticket);
        let (answered, reply) = kernel.replies.pop().expect("every call is answered");
        assert_eq!(answered, ticket);
        reply
    }

    fn end(&mut self, peer: ProcId) {
        self.kernel.borrow_mut().ended.push(peer);
        self.controller.peer_exited(peer);
    }

    fn watched(&self) -> Vec<ProcId> {
        self.kernel.borrow().watched.clone()
    }

    fn records(&self) -> Vec<Record> {
        self.kernel.borrow().records.clone()
    }

    fn refusals(&self) -> Vec<Errno> {
        self.records()
            .into_iter()
            .filter_map(|record| match record {
                Record::Refused { reason, .. } => Some(reason),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn a_consumer_runs_its_clock_at_the_nearest_rate_and_describes_it() {
    let model = Model::new();
    let mut rig = Rig::new(&model);
    assert_eq!(
        decode_run_reply(&rig.call(I2S, &run(i2s(), 3_072_000))),
        Ok(3_072_000)
    );
    assert!(model.enabled(PCM_CTL));
    assert!(!model.enabled(PWM_CTL), "no other clock is touched");
    assert_eq!(
        decode_describe_reply(&rig.call(I2S, &ClockRequest::Describe(i2s()))),
        Ok(ClockState {
            hz: 3_072_000,
            held_elsewhere: false,
        })
    );
    assert_eq!(rig.watched(), [I2S]);
    assert_eq!(
        rig.records(),
        [Record::Ran {
            clock: CLOCK_PCM,
            hz: 3_072_000,
            holder: I2S,
        }]
    );
}

#[test]
fn a_link_the_kernel_does_not_attest_or_naming_another_controller_is_refused() {
    let model = Model::new();
    let mut rig = Rig::new(&model);
    assert_eq!(
        decode_run_reply(&rig.call(PWM0, &run(i2s(), 3_072_000))),
        Err(Errno::PermissionDenied),
        "another consumer's link"
    );
    let elsewhere = LinkRequest::new(CLOCK_CONTROLLER_ENDPOINTS.endpoint(9), 0, &[CLOCK_PCM], b"")
        .expect("valid");
    assert_eq!(
        decode_run_reply(&rig.call(I2S, &run(elsewhere, 3_072_000))),
        Err(Errno::OutOfRange)
    );
    assert!(!model.enabled(PCM_CTL));
    assert!(rig.watched().is_empty());
    assert_eq!(rig.refusals(), [Errno::PermissionDenied, Errno::OutOfRange]);
}

#[test]
fn a_clock_the_endpoint_does_not_serve_is_refused() {
    let model = Model::new();
    let mut rig = Rig::new(&model);
    let vpu = link(20, I2S_NODE);
    let two_cells = LinkRequest::new(endpoint(), I2S_NODE, &[CLOCK_PCM, 0], b"").expect("valid");
    for unserved in [vpu, two_cells] {
        rig.kernel
            .borrow_mut()
            .holdings
            .push((I2S, HwResource::request(&unserved)));
        assert_eq!(
            decode_run_reply(&rig.call(I2S, &run(unserved, 1_000_000))),
            Err(Errno::NotSupported)
        );
        assert_eq!(
            decode_describe_reply(&rig.call(I2S, &ClockRequest::Describe(unserved))),
            Err(Errno::NotSupported)
        );
    }
}

#[test]
fn a_shared_clock_runs_at_its_first_holders_rate_and_another_rate_is_busy() {
    let model = Model::new();
    let mut rig = Rig::new(&model);
    assert_eq!(
        decode_run_reply(&rig.call(PWM1, &run(pwm1(), 75_000_000))),
        Ok(75_000_000)
    );
    let writes = model.writes().len();
    assert_eq!(
        decode_run_reply(&rig.call(PWM0, &run(pwm0(), 45_000_000))),
        Err(Errno::Busy)
    );
    assert_eq!(
        decode_run_reply(&rig.call(PWM0, &run(pwm0(), 75_000_000))),
        Ok(75_000_000),
        "the rate it runs at"
    );
    assert_eq!(model.writes().len(), writes, "joining retunes nothing");
    assert_eq!(
        decode_describe_reply(&rig.call(PWM1, &ClockRequest::Describe(pwm1()))),
        Ok(ClockState {
            hz: 75_000_000,
            held_elsewhere: true,
        })
    );
    assert_eq!(
        decode_run_reply(&rig.call(PWM1, &run(pwm1(), 45_000_000))),
        Err(Errno::Busy),
        "nor may the first retune it once it is shared"
    );
    assert!(rig.records().contains(&Record::Joined {
        clock: CLOCK_PWM,
        holder: PWM0,
    }));
}

#[test]
fn the_only_holder_may_retune_its_clock() {
    let model = Model::new();
    let mut rig = Rig::new(&model);
    rig.call(PWM1, &run(pwm1(), 45_000_000));
    model.clear_writes();
    assert_eq!(
        decode_run_reply(&rig.call(PWM1, &run(pwm1(), 75_000_000))),
        Ok(75_000_000)
    );
    assert_eq!(model.writes().len(), 4, "stopped, set up and started again");
    model.clear_writes();
    assert_eq!(
        decode_run_reply(&rig.call(PWM1, &run(pwm1(), 75_000_000))),
        Ok(75_000_000)
    );
    assert!(model.writes().is_empty(), "the rate it already runs at");
}

#[test]
fn the_last_release_stops_the_clock_and_a_release_without_a_hold_is_not_found() {
    let model = Model::new();
    let mut rig = Rig::new(&model);
    rig.call(PWM1, &run(pwm1(), 75_000_000));
    rig.call(PWM0, &run(pwm0(), 75_000_000));
    assert_eq!(
        decode_release_reply(&rig.call(PWM1, &ClockRequest::Release(pwm1()))),
        Ok(())
    );
    assert!(model.enabled(PWM_CTL), "PWM0 still holds it");
    assert!(!rig.watched().contains(&PWM1));
    assert_eq!(
        decode_release_reply(&rig.call(PWM0, &ClockRequest::Release(pwm0()))),
        Ok(())
    );
    assert!(!model.enabled(PWM_CTL));
    assert!(rig
        .records()
        .contains(&Record::Stopped { clock: CLOCK_PWM }));
    assert_eq!(
        decode_release_reply(&rig.call(PWM0, &ClockRequest::Release(pwm0()))),
        Err(Errno::NotFound)
    );
    assert!(rig.watched().is_empty());
}

#[test]
fn a_holder_that_ends_has_its_holds_dropped_and_its_clock_stopped() {
    let model = Model::new();
    let mut rig = Rig::new(&model);
    rig.call(I2S, &run(i2s(), 3_072_000));
    rig.end(I2S);
    assert!(!model.enabled(PCM_CTL));
    assert!(rig.watched().is_empty());
    let records = rig.records();
    assert!(records.contains(&Record::Abandoned {
        clock: CLOCK_PCM,
        holder: I2S,
    }));
    assert!(records.contains(&Record::Stopped { clock: CLOCK_PCM }));
}

#[test]
fn a_restarted_driver_is_not_refused_for_its_predecessors_hold() {
    let model = Model::new();
    let mut rig = Rig::new(&model);
    rig.call(PWM1, &run(pwm1(), 75_000_000));
    // The predecessor has ended, but its exit has not been served yet.
    rig.kernel.borrow_mut().ended.push(PWM1);
    let successor = instance(9);
    rig.kernel
        .borrow_mut()
        .holdings
        .push((successor, HwResource::request(&pwm1())));
    assert_eq!(
        decode_run_reply(&rig.call(successor, &run(pwm1(), 45_000_000))),
        Ok(45_000_000)
    );
    assert!(rig.records().contains(&Record::Abandoned {
        clock: CLOCK_PWM,
        holder: PWM1,
    }));
    assert_eq!(rig.watched(), [successor]);
}

#[test]
fn a_zero_rate_or_a_caller_already_ended_gains_no_clock() {
    let model = Model::new();
    let mut rig = Rig::new(&model);
    assert_eq!(
        decode_run_reply(&rig.call(I2S, &run(i2s(), 0))),
        Err(Errno::OutOfRange)
    );
    rig.kernel.borrow_mut().ended.push(I2S);
    assert_eq!(
        decode_run_reply(&rig.call(I2S, &run(i2s(), 3_072_000))),
        Err(Errno::NotFound)
    );
    assert!(!model.enabled(PCM_CTL));
}

#[test]
fn a_generator_that_will_not_stop_refuses_the_run_and_leaves_no_hold() {
    let model = Model::new();
    model.running(PCM_CTL, SOURCE_OSCILLATOR, 9 << 12);
    model.stopping(PCM_CTL, Stopping::Never);
    let mut rig = Rig::new(&model);
    assert_eq!(
        decode_run_reply(&rig.call(I2S, &run(i2s(), 3_072_000))),
        Err(Errno::DeviceFault)
    );
    assert!(rig.records().contains(&Record::Stuck { clock: CLOCK_PCM }));
    assert!(rig.watched().is_empty());
    assert_eq!(
        decode_release_reply(&rig.call(I2S, &ClockRequest::Release(i2s()))),
        Err(Errno::NotFound)
    );
}

#[test]
fn a_clock_the_firmware_left_running_is_described_and_never_stopped_unasked() {
    let model = Model::new();
    model.running(PWM_CTL, SOURCE_OSCILLATOR, 27 << 12);
    let mut rig = Rig::new(&model);
    assert_eq!(
        decode_describe_reply(&rig.call(PWM0, &ClockRequest::Describe(pwm0()))),
        Ok(ClockState {
            hz: 2_000_000,
            held_elsewhere: false,
        })
    );
    rig.end(PWM0);
    assert!(model.enabled(PWM_CTL));
    assert!(model.writes().is_empty());
}

#[test]
fn an_undecodable_frame_or_a_caller_the_kernel_cannot_name_is_answered_with_its_refusal() {
    let model = Model::new();
    let mut rig = Rig::new(&model);
    assert_eq!(
        decode_run_reply(&rig.raw(Some(I2S), b"not a clock frame")),
        Err(Errno::BadMagic)
    );
    let mut frame = [0u8; CLOCK_MAX_REQUEST];
    let len = run(i2s(), 1).encode(&mut frame).expect("fits");
    assert_eq!(
        decode_run_reply(&rig.raw(None, &frame[..len])),
        Err(Errno::NotFound)
    );
    assert_eq!(
        rig.records(),
        [
            Record::Refused {
                op: None,
                caller: Some(I2S),
                reason: Errno::BadMagic,
            },
            Record::Refused {
                op: None,
                caller: None,
                reason: Errno::NotFound,
            },
        ]
    );
}

#[test]
fn a_refused_operation_names_itself() {
    let model = Model::new();
    let mut rig = Rig::new(&model);
    rig.call(I2S, &ClockRequest::Release(i2s()));
    assert_eq!(
        rig.records(),
        [Record::Refused {
            op: Some(ClockOp::Release),
            caller: Some(I2S),
            reason: Errno::NotFound,
        }]
    );
}

const WALK_STEPS: usize = 4_000;

/// Rates the walk asks for: some each source makes exactly, some neither.
const RATES: [u64; 5] = [3_072_000, 2_000_000, 100_000_000, 11_289_600, 37_000_123];

#[test]
fn a_random_walk_keeps_every_clock_running_exactly_while_held_at_its_first_holders_rate() {
    let model = Model::new();
    let mut rig = Rig::new(&model);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "a_random_walk_keeps_every_clock_running_exactly_while_held_at_its_first_holders_rate",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let consumers = [
        (PWM0, pwm0(), PWM_CTL),
        (PWM1, pwm1(), PWM_CTL),
        (I2S, i2s(), PCM_CTL),
    ];
    // The walk's own account: who holds each generator, and the rate it was
    // set to.
    let mut held: BTreeMap<usize, (Vec<ProcId>, u64)> = BTreeMap::new();
    let mut live = consumers.map(|(holder, ..)| holder);
    let mut successors = 0u32;
    for _ in 0..WALK_STEPS {
        let pick = rng.below(consumers.len());
        let (_, held_link, control) = consumers[pick];
        let caller = live[pick];
        match rng.below(4) {
            0 | 1 => {
                let hz = *rng.pick(&RATES);
                let entry = held.entry(control).or_insert_with(|| (Vec::new(), 0));
                let reply = decode_run_reply(&rig.call(caller, &run(held_link, hz)));
                let others = entry.0.iter().any(|holder| *holder != caller);
                match reply {
                    Ok(made) => {
                        assert!(!others || made == entry.1, "a shared clock retuned");
                        entry.1 = made;
                        if !entry.0.contains(&caller) {
                            entry.0.push(caller);
                        }
                    }
                    Err(Errno::Busy) => assert!(others, "refused with nobody else holding it"),
                    Err(other) => panic!("{other:?}"),
                }
            }
            2 => {
                let reply =
                    decode_release_reply(&rig.call(caller, &ClockRequest::Release(held_link)));
                let entry = held.entry(control).or_insert_with(|| (Vec::new(), 0));
                // Each consumer holds its clock through its one link.
                if entry.0.contains(&caller) {
                    assert_eq!(reply, Ok(()));
                    entry.0.retain(|holder| *holder != caller);
                } else {
                    assert_eq!(reply, Err(Errno::NotFound));
                }
            }
            _ => {
                rig.end(caller);
                for (holders, _) in held.values_mut() {
                    holders.retain(|holder| *holder != caller);
                }
                // No instance is ever named twice.
                successors += 1;
                let mut raw = [0xEE; PROC_ID_LEN];
                raw[..4].copy_from_slice(&successors.to_le_bytes());
                let successor = ProcId::from_raw(raw);
                rig.kernel
                    .borrow_mut()
                    .holdings
                    .push((successor, HwResource::request(&held_link)));
                live[pick] = successor;
            }
        }
        for (&control, (holders, made)) in &held {
            assert_eq!(
                model.enabled(control),
                !holders.is_empty(),
                "running exactly while held"
            );
            if let Some(&first) = holders.first() {
                let link = consumers
                    .iter()
                    .zip(live)
                    .find(|(_, current)| *current == first)
                    .map(|((_, link, _), _)| *link)
                    .expect("a live holder");
                let state = decode_describe_reply(&rig.call(first, &ClockRequest::Describe(link)))
                    .expect("described");
                assert_eq!(state.hz, *made);
            }
        }
    }
}
