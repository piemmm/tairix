//! The endpoint over scripted clock hardware, answered by the shared fake
//! kernel.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::driver::clock::{ClockOp, ClockState, CLOCK_CONTROLLER_ENDPOINTS};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{DriverError, Errno, ProcId, PROC_ID_LEN};

use crate::testkit::Rig;
use crate::{Applied, ClockHardware, Halt, Policy, Record};

const PWM: u32 = 30;
const PCM: u32 = 31;
const HSM: u32 = 13;
const CORE: u32 = 4;
/// What the fixed clock runs at, as something else set it.
const CORE_HZ: u64 = 500_000_000;
/// What the floored clock runs at before anything holds it.
const BOOT_HZ: u64 = 400_000_000;
const MOST_HZ: u64 = 600_000_000;
/// The fastest an exact clock is made.
const EXACT_MOST_HZ: u64 = 1_000_000_000;

const fn instance(byte: u8) -> ProcId {
    ProcId::from_raw([byte; PROC_ID_LEN])
}

const JACK: ProcId = instance(1);
const I2S: ProcId = instance(2);
const HDMI0: ProcId = instance(3);
const HDMI1: ProcId = instance(4);

fn endpoint() -> u64 {
    CLOCK_CONTROLLER_ENDPOINTS.endpoint(5)
}

fn link(clock: u32, index: u8) -> LinkRequest {
    LinkRequest::new(endpoint(), index, &[clock], b"").expect("valid")
}

/// Two exact clocks that round to a kilohertz and one floored clock that
/// rounds up to a megahertz.
struct Hardware {
    state: Probe,
}

#[derive(Default)]
struct HardwareState {
    rates: [u64; 4],
    runs: Vec<(usize, u64)>,
    stuck: bool,
    cut_next: bool,
    /// How far below a setting the hardware makes, as firmware rounding does.
    shortfall: u64,
}

const SERVED: [(u32, Policy); 4] = [
    (PWM, Policy::Exact),
    (PCM, Policy::Exact),
    (HSM, Policy::Floor),
    (CORE, Policy::Fixed),
];

impl ClockHardware for Hardware {
    type Setting = u64;

    fn count(&self) -> usize {
        SERVED.len()
    }

    fn served(&self, selector: &[u32]) -> Option<usize> {
        let &[id] = selector else {
            return None;
        };
        SERVED.iter().position(|&(served, _)| served == id)
    }

    fn id(&self, at: usize) -> u32 {
        SERVED[at].0
    }

    fn policy(&self, at: usize) -> Policy {
        SERVED[at].1
    }

    fn rate(&self, at: usize) -> Result<u64, DriverError> {
        Ok(self.state.0.borrow().rates[at])
    }

    fn max_rate(&self, at: usize) -> Result<u64, DriverError> {
        Ok(match SERVED[at].1 {
            Policy::Exact => EXACT_MOST_HZ,
            Policy::Floor => MOST_HZ,
            Policy::Fixed => self.state.0.borrow().rates[at],
        })
    }

    fn nearest(&self, at: usize, hz: u64) -> Result<u64, DriverError> {
        match SERVED[at].1 {
            Policy::Exact if hz >= 1_000 => Ok(hz - hz % 1_000),
            Policy::Floor if hz <= MOST_HZ => Ok(hz.div_ceil(1_000_000) * 1_000_000),
            _ => Err(DriverError::OutOfRange),
        }
    }

    fn run(&mut self, at: usize, setting: u64) -> Result<Applied, DriverError> {
        let mut state = self.state.0.borrow_mut();
        if state.stuck {
            return Err(DriverError::DeviceFault);
        }
        let made = setting - state.shortfall;
        state.rates[at] = made;
        state.runs.push((at, setting));
        let halt = if core::mem::take(&mut state.cut_next) {
            Halt::Cut
        } else {
            Halt::Clean
        };
        Ok(Applied { halt, hz: made })
    }

    fn stop(&mut self, at: usize) -> Result<Halt, DriverError> {
        let mut state = self.state.0.borrow_mut();
        if state.stuck {
            return Err(DriverError::DeviceFault);
        }
        state.rates[at] = if SERVED[at].1 == Policy::Floor {
            BOOT_HZ
        } else {
            0
        };
        Ok(Halt::Clean)
    }
}

/// The scripted hardware's state, as a test reads and steers it.
#[derive(Clone, Default)]
struct Probe(Rc<RefCell<HardwareState>>);

impl Probe {
    fn rate(&self, at: usize) -> u64 {
        self.0.borrow().rates[at]
    }

    fn stick(&self, stuck: bool) {
        self.0.borrow_mut().stuck = stuck;
    }

    /// The firmware raised clock `at` itself, past the highest rate it
    /// states.
    fn raise(&self, at: usize, hz: u64) {
        self.0.borrow_mut().rates[at] = hz;
    }

    fn cut_next(&self) {
        self.0.borrow_mut().cut_next = true;
    }
}

fn rig() -> (Rig<Hardware>, Probe) {
    let probe = Probe(Rc::new(RefCell::new(HardwareState {
        rates: [0, 0, BOOT_HZ, CORE_HZ],
        ..HardwareState::default()
    })));
    let rig = Rig::new(
        Hardware {
            state: probe.clone(),
        },
        endpoint(),
        &[
            (JACK, link(PWM, 0)),
            (I2S, link(PWM, 1)),
            (I2S, link(PCM, 0)),
            (HDMI0, link(HSM, 0)),
            (HDMI1, link(HSM, 0)),
            (HDMI0, link(CORE, 1)),
        ],
    );
    (rig, probe)
}

#[test]
fn a_consumer_runs_its_clock_at_the_nearest_rate_and_describes_it() {
    let (mut rig, hardware) = rig();
    assert_eq!(rig.run(JACK, link(PWM, 0), 375_000_123), Ok(375_000_000));
    assert_eq!(hardware.rate(0), 375_000_000);
    assert_eq!(
        rig.describe(JACK, link(PWM, 0)),
        Ok(ClockState {
            hz: 375_000_000,
            max_hz: EXACT_MOST_HZ,
            held_elsewhere: false
        })
    );
    assert!(rig.records().contains(&Record::Ran {
        clock: PWM,
        hz: 375_000_000,
        holder: JACK
    }));
    assert_eq!(rig.kernel.watched(), [JACK]);
}

#[test]
fn a_link_the_kernel_does_not_attest_or_naming_another_controller_is_refused() {
    let (mut rig, hardware) = rig();
    assert_eq!(
        rig.run(JACK, link(PCM, 0), 1_000_000),
        Err(Errno::PermissionDenied)
    );
    let foreign =
        LinkRequest::new(CLOCK_CONTROLLER_ENDPOINTS.endpoint(2), 0, &[PWM], b"").expect("valid");
    assert_eq!(rig.run(JACK, foreign, 1_000_000), Err(Errno::OutOfRange));
    assert_eq!(hardware.rate(1), 0);
}

#[test]
fn a_clock_the_endpoint_does_not_serve_is_refused_and_names_its_operation() {
    let (mut rig, _) = rig();
    let other = link(7, 0);
    rig.kernel.grant(JACK, HwResource::request(&other));
    assert_eq!(rig.run(JACK, other, 1_000_000), Err(Errno::NotSupported));
    assert!(rig.records().contains(&Record::Refused {
        op: Some(ClockOp::Run),
        caller: Some(JACK),
        reason: Errno::NotSupported
    }));
}

#[test]
fn a_shared_exact_clock_runs_at_its_first_holders_rate_and_another_rate_is_busy() {
    let (mut rig, hardware) = rig();
    assert_eq!(rig.run(JACK, link(PWM, 0), 100_000_000), Ok(100_000_000));
    assert_eq!(rig.run(I2S, link(PWM, 1), 100_000_000), Ok(100_000_000));
    assert!(rig.records().contains(&Record::Joined {
        clock: PWM,
        holder: I2S
    }));
    assert_eq!(rig.run(I2S, link(PWM, 1), 50_000_000), Err(Errno::Busy));
    assert_eq!(hardware.rate(0), 100_000_000);
    assert!(
        rig.describe(I2S, link(PWM, 1))
            .expect("described")
            .held_elsewhere
    );
}

#[test]
fn the_only_holder_may_retune_and_the_last_release_stops_the_clock() {
    let (mut rig, hardware) = rig();
    rig.run(JACK, link(PWM, 0), 100_000_000).expect("runs");
    assert_eq!(rig.run(JACK, link(PWM, 0), 50_000_000), Ok(50_000_000));
    assert_eq!(rig.release(JACK, link(PWM, 0)), Ok(()));
    assert_eq!(hardware.rate(0), 0);
    assert!(rig.records().contains(&Record::Stopped { clock: PWM }));
    assert_eq!(rig.release(JACK, link(PWM, 0)), Err(Errno::NotFound));
    assert!(rig.kernel.watched().is_empty());
}

#[test]
fn a_holder_that_ends_has_its_holds_dropped_and_a_restarted_driver_is_not_refused() {
    let (mut rig, hardware) = rig();
    rig.run(JACK, link(PWM, 0), 100_000_000).expect("runs");
    rig.run(I2S, link(PWM, 1), 100_000_000).expect("joins");
    rig.end(JACK);
    assert!(rig.records().contains(&Record::Abandoned {
        clock: PWM,
        holder: JACK
    }));
    assert_eq!(hardware.rate(0), 100_000_000, "still held");
    // The I2S's predecessor ended without the kernel's notice arriving yet.
    rig.kernel.end(I2S);
    let successor = instance(9);
    rig.kernel
        .grant(successor, HwResource::request(&link(PWM, 1)));
    assert_eq!(rig.run(successor, link(PWM, 1), 25_000_000), Ok(25_000_000));
}

#[test]
fn a_zero_rate_or_a_caller_already_ended_gains_no_clock() {
    let (mut rig, hardware) = rig();
    assert_eq!(rig.run(JACK, link(PWM, 0), 0), Err(Errno::OutOfRange));
    rig.kernel.end(JACK);
    assert_eq!(rig.run(JACK, link(PWM, 0), 1_000_000), Err(Errno::NotFound));
    assert_eq!(hardware.rate(0), 0);
}

#[test]
fn a_clock_that_will_not_change_refuses_the_run_and_leaves_no_hold() {
    let (mut rig, hardware) = rig();
    hardware.stick(true);
    assert_eq!(
        rig.run(JACK, link(PWM, 0), 1_000_000),
        Err(Errno::DeviceFault)
    );
    assert!(rig.records().contains(&Record::Stuck { clock: PWM }));
    assert!(rig.kernel.watched().is_empty());
    hardware.stick(false);
    assert_eq!(
        rig.run(I2S, link(PWM, 1), 2_000_000),
        Ok(2_000_000),
        "no hold left behind"
    );
}

#[test]
fn a_change_cut_off_mid_period_is_recorded() {
    let (mut rig, hardware) = rig();
    hardware.cut_next();
    rig.run(JACK, link(PWM, 0), 1_000_000).expect("runs");
    assert!(rig.records().contains(&Record::Cut { clock: PWM }));
}

#[test]
fn a_floored_clock_runs_at_the_highest_floor_its_holders_state() {
    let (mut rig, hardware) = rig();
    assert_eq!(rig.floor(HDMI0, link(HSM, 0), 148_500_001), Ok(149_000_000));
    assert_eq!(rig.floor(HDMI1, link(HSM, 0), 297_000_000), Ok(297_000_000));
    assert_eq!(hardware.rate(2), 297_000_000);
    // A lower floor rides above the higher one already stated.
    assert_eq!(rig.floor(HDMI0, link(HSM, 0), 100_000_000), Ok(297_000_000));
    // The higher holder lowering its floor brings the clock down to the next.
    assert_eq!(rig.floor(HDMI1, link(HSM, 0), 50_000_000), Ok(100_000_000));
    assert_eq!(hardware.rate(2), 100_000_000);
}

#[test]
fn a_floor_leaving_brings_the_clock_down_and_the_last_returns_it_to_its_boot_rate() {
    let (mut rig, hardware) = rig();
    rig.floor(HDMI0, link(HSM, 0), 150_000_000).expect("floors");
    rig.floor(HDMI1, link(HSM, 0), 300_000_000).expect("floors");
    rig.release(HDMI1, link(HSM, 0)).expect("released");
    assert_eq!(hardware.rate(2), 150_000_000);
    rig.end(HDMI0);
    assert_eq!(hardware.rate(2), BOOT_HZ);
    assert!(rig.records().contains(&Record::Stopped { clock: HSM }));
}

#[test]
fn a_clock_its_last_holder_left_that_will_not_return_is_reported_running() {
    let (mut rig, hardware) = rig();
    rig.floor(HDMI0, link(HSM, 0), 150_000_000).expect("floors");
    hardware.stick(true);
    assert_eq!(rig.release(HDMI0, link(HSM, 0)), Ok(()));
    assert_eq!(hardware.rate(2), 150_000_000, "left running as it was held");
    assert!(rig.records().contains(&Record::Unreturned { clock: HSM }));
    assert!(!rig.records().contains(&Record::Stuck { clock: HSM }));
    hardware.stick(false);
    assert_eq!(
        rig.floor(HDMI1, link(HSM, 0), 300_000_000),
        Ok(300_000_000),
        "a later holder sets it afresh"
    );
}

#[test]
fn a_floor_raise_that_fails_leaves_every_other_holders_floor_standing() {
    let (mut rig, hardware) = rig();
    rig.floor(HDMI0, link(HSM, 0), 150_000_000).expect("floors");
    hardware.stick(true);
    assert_eq!(
        rig.floor(HDMI1, link(HSM, 0), 300_000_000),
        Err(Errno::DeviceFault)
    );
    assert_eq!(hardware.rate(2), 150_000_000, "left as it was");
    assert_eq!(rig.kernel.watched(), [HDMI0]);
    hardware.stick(false);
    assert_eq!(rig.release(HDMI0, link(HSM, 0)), Ok(()));
    assert_eq!(hardware.rate(2), BOOT_HZ);
}

#[test]
fn a_reply_states_the_rate_the_hardware_made_not_the_one_asked() {
    let (mut rig, hardware) = rig();
    hardware.0.borrow_mut().shortfall = 250_000;
    assert_eq!(rig.floor(HDMI0, link(HSM, 0), 150_000_000), Ok(149_750_000));
    assert_eq!(
        rig.floor(HDMI1, link(HSM, 0), 100_000_000),
        Ok(149_750_000),
        "a holder the clock already satisfies is told what it runs at"
    );
    assert!(rig.records().contains(&Record::Ran {
        clock: HSM,
        hz: 149_750_000,
        holder: HDMI0
    }));
}

#[test]
fn a_clock_is_described_with_the_highest_rate_it_can_run_at() {
    let (mut rig, hardware) = rig();
    let state = rig.describe(HDMI0, link(HSM, 0)).expect("described");
    assert_eq!((state.hz, state.max_hz), (BOOT_HZ, MOST_HZ));
    // A clock running past the rate it states can demonstrably run there.
    hardware.raise(2, MOST_HZ + 1);
    let state = rig.describe(HDMI0, link(HSM, 0)).expect("described");
    assert_eq!((state.hz, state.max_hz), (MOST_HZ + 1, MOST_HZ + 1));
}

#[test]
fn a_fixed_clock_is_described_and_never_run_floored_or_held() {
    let (mut rig, hardware) = rig();
    let core = link(CORE, 1);
    assert_eq!(
        rig.describe(HDMI0, core),
        Ok(ClockState {
            hz: CORE_HZ,
            max_hz: CORE_HZ,
            held_elsewhere: false
        })
    );
    assert_eq!(rig.run(HDMI0, core, 300_000_000), Err(Errno::NotSupported));
    assert_eq!(
        rig.floor(HDMI0, core, 300_000_000),
        Err(Errno::NotSupported)
    );
    assert_eq!(rig.release(HDMI0, core), Err(Errno::NotFound));
    assert_eq!(hardware.rate(3), CORE_HZ);
    assert!(rig.kernel.watched().is_empty());
}

#[test]
fn each_policy_refuses_the_other_policys_request() {
    let (mut rig, hardware) = rig();
    assert_eq!(
        rig.run(HDMI0, link(HSM, 0), 150_000_000),
        Err(Errno::NotSupported)
    );
    assert_eq!(
        rig.floor(JACK, link(PWM, 0), 1_000_000),
        Err(Errno::NotSupported)
    );
    assert_eq!(
        rig.floor(HDMI0, link(HSM, 0), MOST_HZ + 1),
        Err(Errno::OutOfRange)
    );
    assert_eq!(hardware.rate(2), BOOT_HZ, "nothing changed");
}

#[test]
fn an_undecodable_frame_or_a_caller_the_kernel_cannot_name_is_refused() {
    let (mut rig, _) = rig();
    rig.serve(JACK, b"junk");
    assert!(matches!(
        rig.records().last(),
        Some(Record::Refused {
            op: None,
            caller: Some(JACK),
            ..
        })
    ));
    rig.serve_out_of_service(b"junk");
    assert!(matches!(
        rig.records().last(),
        Some(Record::Refused {
            op: None,
            caller: None,
            reason: Errno::NotFound
        })
    ));
}

/// Whatever order holders arrive, change and leave in, a clock runs exactly
/// while held: an exact one at its first holder's rate, a floored one at the
/// highest floor its holders state.
#[test]
fn a_random_walk_keeps_every_clock_at_its_policys_rate_exactly_while_held() {
    let (mut rig, hardware) = rig();
    let mut seed: u64 = 0x5DEE_CE66_D1CE_4E5B;
    let mut next = |bound: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % bound
    };
    let exact = [(JACK, link(PWM, 0)), (I2S, link(PWM, 1))];
    let floored = [(HDMI0, link(HSM, 0)), (HDMI1, link(HSM, 0))];
    let mut floors: BTreeMap<u8, u64> = BTreeMap::new();
    let mut first: Option<(ProcId, u64)> = None;
    let mut exact_holders: Vec<ProcId> = Vec::new();
    for _ in 0..4_000 {
        match next(4) {
            0 => {
                let (who, line) = exact[usize::try_from(next(2)).expect("small")];
                let hz = (1 + next(4)) * 1_000_000;
                let outcome = rig.run(who, line, hz);
                let allowed = first.is_none_or(|(holder, rate)| {
                    rate == hz || (holder == who && exact_holders.len() == 1)
                });
                assert_eq!(outcome.is_ok(), allowed);
                if outcome.is_ok() {
                    if !exact_holders.contains(&who) {
                        exact_holders.push(who);
                    }
                    first = Some((first.map_or(who, |(holder, _)| holder), hz));
                }
            }
            1 => {
                let (who, line) = exact[usize::try_from(next(2)).expect("small")];
                let held = exact_holders.contains(&who);
                assert_eq!(rig.release(who, line).is_ok(), held);
                exact_holders.retain(|holder| *holder != who);
                if exact_holders.is_empty() {
                    first = None;
                } else if let Some((holder, rate)) = first {
                    if holder == who {
                        first = Some((exact_holders[0], rate));
                    }
                }
            }
            2 => {
                let pick = usize::try_from(next(2)).expect("small");
                let (who, line) = floored[pick];
                let hz = next(5) * 100_000_000;
                rig.floor(who, line, hz).expect("a floor within the range");
                floors.insert(u8::try_from(pick).expect("small"), hz);
            }
            _ => {
                let pick = usize::try_from(next(2)).expect("small");
                let (who, line) = floored[pick];
                let held = floors.remove(&u8::try_from(pick).expect("small")).is_some();
                assert_eq!(rig.release(who, line).is_ok(), held);
            }
        }
        let expected_exact = first.map_or(0, |(_, rate)| rate);
        assert_eq!(hardware.rate(0), expected_exact);
        let expected_floor = floors.values().copied().max().unwrap_or(BOOT_HZ);
        assert_eq!(hardware.rate(2), expected_floor);
    }
}
