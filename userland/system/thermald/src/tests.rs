extern crate std;

use alloc::collections::btree_map::Entry as Slot;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::gpio::{HeaderLines, Holder, LineEntry, LineState, LineTable, Polarity};
use tairix_abi::driver::thermal::Millicelsius;
use tairix_abi::thermal_ipc::{FanFault, FanMode, FanPolicy};
use tairix_abi::Errno;

use crate::{
    Entry, Inventory, Journal, Machine, Reading, Thermald, READING_MAX_AGE_US, SAMPLE_PERIOD_US,
};

/// Every request the test binary makes, so a test can refuse them.
#[global_allocator]
static ALLOC: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

const HEADER: u64 = 0x4748_0000_0000_0005;
const SENSOR: u64 = 0x5453_0000_0000_0003;

/// How the header answers a call.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Answer {
    /// At once.
    #[default]
    Prompt,
    /// Not before the call's deadline, which withdrew it unreceived.
    Withdrawn,
    /// Not before the call's deadline, though it was carried out.
    Late,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Call {
    Claim(u8, Polarity, bool),
    Drive(u8, bool),
    Release(u8),
}

/// A header that records claims as the controller does: a held line is
/// refused a second claim, and only a held line is driven or given back.
#[derive(Default)]
struct FakeMachine {
    sensors: BTreeMap<u64, Result<Millicelsius, Errno>>,
    /// The lines held for the caller, and whether each is asserted.
    held: BTreeMap<u8, bool>,
    /// A line another holder has.
    foreign: Option<u8>,
    refuse_claim: Option<Errno>,
    refuse_drive: Option<Errno>,
    answer: Answer,
    calls: Vec<Call>,
    sensor_reads: usize,
    line_reads: usize,
    now_us: u64,
}

impl FakeMachine {
    fn at(celsius: i16) -> Self {
        let mut machine = Self::default();
        machine.set(celsius);
        machine
    }

    fn set(&mut self, celsius: i16) {
        self.sensors
            .insert(SENSOR, Ok(Millicelsius::from_celsius(celsius)));
    }

    /// What the service is answered for a call the header carried out to
    /// `outcome`.
    fn reply(&self, outcome: Result<(), Errno>) -> Result<(), Errno> {
        if self.answer == Answer::Late {
            Err(Errno::TimedOut)
        } else {
            outcome
        }
    }
}

impl Machine for FakeMachine {
    fn read_sensor(&mut self, endpoint: u64) -> Result<Millicelsius, Errno> {
        self.sensor_reads += 1;
        self.sensors
            .get(&endpoint)
            .copied()
            .unwrap_or(Err(Errno::NotFound))
    }

    fn claim(
        &mut self,
        endpoint: u64,
        line: u8,
        polarity: Polarity,
        asserted: bool,
    ) -> Result<(), Errno> {
        assert_eq!(endpoint, HEADER);
        self.calls.push(Call::Claim(line, polarity, asserted));
        if self.answer == Answer::Withdrawn {
            return Err(Errno::TimedOut);
        }
        let outcome = if let Some(refusal) = self.refuse_claim {
            Err(refusal)
        } else {
            match self.held.entry(line) {
                Slot::Occupied(_) => Err(Errno::Busy),
                Slot::Vacant(slot) => {
                    slot.insert(asserted);
                    Ok(())
                }
            }
        };
        self.reply(outcome)
    }

    fn drive(&mut self, endpoint: u64, line: u8, asserted: bool) -> Result<(), Errno> {
        assert_eq!(endpoint, HEADER);
        self.calls.push(Call::Drive(line, asserted));
        if self.answer == Answer::Withdrawn {
            return Err(Errno::TimedOut);
        }
        let outcome = match (self.refuse_drive, self.held.get_mut(&line)) {
            (Some(refusal), _) => Err(refusal),
            (None, Some(held)) => {
                *held = asserted;
                Ok(())
            }
            (None, None) => Err(Errno::NotFound),
        };
        self.reply(outcome)
    }

    fn release(&mut self, endpoint: u64, line: u8) -> Result<(), Errno> {
        assert_eq!(endpoint, HEADER);
        self.calls.push(Call::Release(line));
        if self.answer == Answer::Withdrawn {
            return Err(Errno::TimedOut);
        }
        let outcome = self.held.remove(&line).map(|_| ()).ok_or(Errno::NotFound);
        self.reply(outcome)
    }

    fn lines(&mut self, endpoint: u64) -> Result<LineTable, Errno> {
        assert_eq!(endpoint, HEADER);
        self.line_reads += 1;
        let mut table = LineTable::default();
        for line in 2..=27 {
            let state = match self.held.get(&line) {
                Some(&asserted) => LineState::Output {
                    holder: Holder::Caller,
                    asserted,
                },
                None if self.foreign == Some(line) => LineState::Output {
                    holder: Holder::Other,
                    asserted: false,
                },
                None => LineState::Free,
            };
            table.push(LineEntry { line, state })?;
        }
        Ok(table)
    }

    fn now_us(&self) -> u64 {
        self.now_us
    }
}

#[derive(Default)]
struct Log(Vec<Entry>);

impl Journal for Log {
    fn record(&mut self, entry: Entry) {
        self.0.push(entry);
    }
}

fn policy(mode: FanMode, line: Option<u8>) -> FanPolicy {
    FanPolicy::new(mode, line, Polarity::ActiveHigh, 65, 10).unwrap()
}

fn present() -> Inventory {
    Inventory {
        header: Some(HEADER),
        sensors: vec![SENSOR],
    }
}

/// A service on `machine` with the header and the sensor present, following
/// `policy`.
fn service(machine: &mut FakeMachine, policy: FanPolicy, log: &mut Log) -> Thermald {
    let mut service = Thermald::default();
    service.inventory_changed(present(), machine, log);
    service.policy_changed(policy, machine, log);
    service
}

/// Advance to the next sample and take it.
fn tick(service: &mut Thermald, machine: &mut FakeMachine, log: &mut Log) {
    let due = service.deadline_us().expect("the fan is owed a deadline");
    machine.now_us = machine.now_us.max(due);
    service.expired(machine, log);
}

#[test]
fn an_off_fan_holds_no_line_and_samples_nothing() {
    let (mut machine, mut log) = (FakeMachine::at(90), Log::default());
    let service = service(&mut machine, policy(FanMode::Off, Some(18)), &mut log);
    assert!(machine.calls.is_empty());
    assert_eq!(service.deadline_us(), None);
    assert_eq!(service.fan().fault(), None);
}

#[test]
fn an_always_on_fan_is_claimed_running_and_samples_nothing() {
    let (mut machine, mut log) = (FakeMachine::at(30), Log::default());
    let service = service(&mut machine, policy(FanMode::On, Some(18)), &mut log);
    assert_eq!(machine.calls, [Call::Claim(18, Polarity::ActiveHigh, true)]);
    assert!(service.fan().running());
    assert_eq!(service.deadline_us(), None, "tickless while always on");
    assert!(log.0.contains(&Entry::Claimed { line: 18 }));
}

#[test]
fn a_fan_with_no_line_is_a_stated_fault() {
    let (mut machine, mut log) = (FakeMachine::at(30), Log::default());
    let service = service(&mut machine, policy(FanMode::On, None), &mut log);
    assert!(machine.calls.is_empty());
    assert_eq!(service.fan().fault(), Some(FanFault::NoLine));
    assert!(log.0.contains(&Entry::Fault {
        fault: FanFault::NoLine,
        refusal: None
    }));
}

#[test]
fn a_fan_waits_for_its_header_and_takes_the_line_when_it_appears() {
    let (mut machine, mut log) = (FakeMachine::at(30), Log::default());
    let mut service = Thermald::default();
    service.policy_changed(policy(FanMode::On, Some(18)), &mut machine, &mut log);
    assert_eq!(service.fan().fault(), Some(FanFault::NoHeader));
    service.inventory_changed(present(), &mut machine, &mut log);
    assert_eq!(service.fan().fault(), None);
    assert_eq!(service.fan().line(), Some(18));
    assert!(log.0.contains(&Entry::Recovered));
}

#[test]
fn an_automatic_fan_switches_with_hysteresis() {
    let (mut machine, mut log) = (FakeMachine::at(50), Log::default());
    let mut service = service(&mut machine, policy(FanMode::Automatic, Some(18)), &mut log);
    assert_eq!(
        machine.calls,
        [Call::Claim(18, Polarity::ActiveHigh, true)],
        "runs until the first sample decides"
    );
    assert_eq!(service.deadline_us(), Some(0), "sampled at once");
    tick(&mut service, &mut machine, &mut log);
    assert!(!service.fan().running(), "50 is below the start");
    for (celsius, running) in [(64, false), (65, true), (60, true), (56, true), (55, false)] {
        machine.set(celsius);
        tick(&mut service, &mut machine, &mut log);
        assert_eq!(service.fan().running(), running, "at {celsius}");
    }
    assert_eq!(
        service.deadline_us(),
        Some(machine.now_us + SAMPLE_PERIOD_US)
    );
    let switches = machine
        .calls
        .iter()
        .filter(|call| matches!(call, Call::Drive(..)))
        .count();
    assert_eq!(switches, 3, "only a change of state is driven");
}

#[test]
fn a_reading_that_cannot_be_taken_runs_the_fan_until_one_can() {
    let (mut machine, mut log) = (FakeMachine::at(40), Log::default());
    let mut service = service(&mut machine, policy(FanMode::Automatic, Some(18)), &mut log);
    tick(&mut service, &mut machine, &mut log);
    assert!(!service.fan().running());
    machine.sensors.insert(SENSOR, Err(Errno::NoData));
    tick(&mut service, &mut machine, &mut log);
    assert!(service.fan().running());
    assert_eq!(service.fan().fault(), Some(FanFault::SensorFailed));
    let fault = log
        .0
        .iter()
        .position(|entry| {
            *entry
                == Entry::Fault {
                    fault: FanFault::SensorFailed,
                    refusal: Some(Errno::NoData),
                }
        })
        .expect("the fault is recorded");
    let started = log
        .0
        .iter()
        .rposition(|entry| *entry == Entry::Switched { running: true })
        .expect("the fan is started");
    assert!(
        fault < started,
        "the fault is recorded ahead of the fan it starts"
    );
    machine.set(40);
    tick(&mut service, &mut machine, &mut log);
    assert_eq!(service.fan().fault(), None);
    assert!(!service.fan().running());
    assert!(log.0.contains(&Entry::Recovered));
}

#[test]
fn an_automatic_fan_with_no_sensor_runs_and_says_why() {
    let (mut machine, mut log) = (FakeMachine::default(), Log::default());
    let mut service = Thermald::default();
    let header_only = Inventory {
        header: Some(HEADER),
        sensors: Vec::new(),
    };
    service.inventory_changed(header_only, &mut machine, &mut log);
    service.policy_changed(policy(FanMode::Automatic, Some(18)), &mut machine, &mut log);
    tick(&mut service, &mut machine, &mut log);
    assert!(service.fan().running());
    assert_eq!(service.fan().fault(), Some(FanFault::NoSensor));
}

#[test]
fn a_new_line_or_polarity_gives_the_old_claim_back_first() {
    let (mut machine, mut log) = (FakeMachine::at(30), Log::default());
    let mut service = service(&mut machine, policy(FanMode::On, Some(18)), &mut log);
    service.policy_changed(policy(FanMode::On, Some(23)), &mut machine, &mut log);
    let low = FanPolicy::new(FanMode::On, Some(23), Polarity::ActiveLow, 65, 10).unwrap();
    service.policy_changed(low, &mut machine, &mut log);
    assert_eq!(
        machine.calls,
        [
            Call::Claim(18, Polarity::ActiveHigh, true),
            Call::Release(18),
            Call::Claim(23, Polarity::ActiveHigh, true),
            Call::Release(23),
            Call::Claim(23, Polarity::ActiveLow, true),
        ]
    );
}

#[test]
fn turning_the_fan_off_gives_its_line_back_and_stops_sampling() {
    let (mut machine, mut log) = (FakeMachine::at(30), Log::default());
    let mut service = service(&mut machine, policy(FanMode::Automatic, Some(18)), &mut log);
    service.policy_changed(policy(FanMode::Off, Some(18)), &mut machine, &mut log);
    assert_eq!(machine.calls.last(), Some(&Call::Release(18)));
    assert_eq!(service.fan().line(), None);
    assert_eq!(service.deadline_us(), None);
    assert!(log.0.contains(&Entry::Released { line: 18 }));
}

#[test]
fn a_refused_claim_is_a_fault_and_samples_nothing() {
    let mut machine = FakeMachine::at(90);
    machine.refuse_claim = Some(Errno::Busy);
    let mut log = Log::default();
    let service = service(&mut machine, policy(FanMode::Automatic, Some(14)), &mut log);
    assert_eq!(service.fan().fault(), Some(FanFault::LineUnavailable));
    assert_eq!(service.deadline_us(), None);
    assert!(log.0.contains(&Entry::Fault {
        fault: FanFault::LineUnavailable,
        refusal: Some(Errno::Busy)
    }));
}

#[test]
fn a_refused_switch_gives_the_line_back_and_takes_it_again() {
    let (mut machine, mut log) = (FakeMachine::at(40), Log::default());
    let mut service = service(&mut machine, policy(FanMode::Automatic, Some(18)), &mut log);
    machine.refuse_drive = Some(Errno::DeviceFault);
    tick(&mut service, &mut machine, &mut log);
    assert_eq!(
        machine.calls[1..],
        [
            Call::Drive(18, false),
            Call::Release(18),
            Call::Claim(18, Polarity::ActiveHigh, true),
        ]
    );
    assert!(log.0.contains(&Entry::Lost {
        line: 18,
        refusal: Errno::DeviceFault
    }));
    assert_eq!(service.fan().fault(), None);
    assert!(service.fan().running(), "runs until a sample decides");
    machine.refuse_drive = None;
    tick(&mut service, &mut machine, &mut log);
    assert!(!service.fan().running());
}

#[test]
fn a_controller_that_restarted_is_found_out_when_the_tree_moves() {
    let (mut machine, mut log) = (FakeMachine::at(30), Log::default());
    let mut service = service(&mut machine, policy(FanMode::On, Some(18)), &mut log);
    // A restarted controller records no claim.
    machine.held.clear();
    service.inventory_changed(present(), &mut machine, &mut log);
    assert_eq!(
        machine.calls[1..],
        [
            Call::Drive(18, true),
            Call::Release(18),
            Call::Claim(18, Polarity::ActiveHigh, true),
        ]
    );
    assert!(log.0.contains(&Entry::Lost {
        line: 18,
        refusal: Errno::NotFound
    }));
    assert_eq!(machine.held.get(&18), Some(&true), "held again, asserted");
    assert_eq!(service.fan().fault(), None);
}

#[test]
fn a_line_that_cannot_be_taken_again_waits_for_the_tree_to_move() {
    let (mut machine, mut log) = (FakeMachine::at(70), Log::default());
    let mut service = service(&mut machine, policy(FanMode::Automatic, Some(18)), &mut log);
    tick(&mut service, &mut machine, &mut log);
    machine.set(40);
    // An ended controller answers nothing.
    machine.refuse_drive = Some(Errno::NotFound);
    machine.refuse_claim = Some(Errno::NotFound);
    tick(&mut service, &mut machine, &mut log);
    assert_eq!(service.fan().fault(), Some(FanFault::LineUnavailable));
    assert_eq!(service.fan().line(), None);
    assert_eq!(service.deadline_us(), None, "never retried on a timer");
    machine.refuse_drive = None;
    machine.refuse_claim = None;
    service.inventory_changed(present(), &mut machine, &mut log);
    assert_eq!(service.fan().line(), Some(18));
    assert_eq!(service.fan().fault(), None);
    assert!(log.0.contains(&Entry::Recovered));
    assert_eq!(
        service.deadline_us(),
        Some(machine.now_us),
        "sampled at once"
    );
}

#[test]
fn a_header_that_goes_takes_the_claim_with_it() {
    let (mut machine, mut log) = (FakeMachine::at(30), Log::default());
    let mut service = service(&mut machine, policy(FanMode::Automatic, Some(18)), &mut log);
    let gone = Inventory {
        header: None,
        sensors: vec![SENSOR],
    };
    // Its claims went with the controller.
    machine.held.clear();
    service.inventory_changed(gone, &mut machine, &mut log);
    assert_eq!(service.fan().line(), None);
    assert_eq!(service.fan().fault(), Some(FanFault::NoHeader));
    assert_eq!(service.deadline_us(), None);
    service.inventory_changed(present(), &mut machine, &mut log);
    assert_eq!(service.fan().line(), Some(18), "taken again as it returns");
}

#[test]
fn a_status_reads_the_sensor_and_the_lines_at_most_once_a_period() {
    let (mut machine, mut log) = (FakeMachine::at(48), Log::default());
    let mut service = service(&mut machine, policy(FanMode::On, Some(18)), &mut log);
    let reads = machine.sensor_reads;
    let first = service.status(&mut machine);
    let second = service.status(&mut machine);
    assert_eq!(first, second);
    assert_eq!(machine.sensor_reads, reads + 1);
    assert_eq!(machine.line_reads, 1);
    assert_eq!(first.temperature, Some(Millicelsius::from_celsius(48)));
    assert!(first.running);
    let lines = first.lines.expect("the header's lines");
    assert!(lines.free().contains(18), "the fan's own line");
    assert_eq!(lines.lent(), HeaderLines::span(2, 27));
    machine.now_us += READING_MAX_AGE_US;
    service.status(&mut machine);
    assert_eq!(machine.sensor_reads, reads + 2);
    assert_eq!(machine.line_reads, 2);
}

#[test]
fn a_change_to_the_fan_stales_the_lines_a_status_reports() {
    let (mut machine, mut log) = (FakeMachine::at(48), Log::default());
    let mut service = service(&mut machine, policy(FanMode::Off, Some(18)), &mut log);
    machine.foreign = Some(18);
    assert!(service
        .status(&mut machine)
        .lines
        .unwrap()
        .held()
        .contains(18));
    machine.foreign = None;
    service.policy_changed(policy(FanMode::On, Some(18)), &mut machine, &mut log);
    assert!(service
        .status(&mut machine)
        .lines
        .unwrap()
        .free()
        .contains(18));
}

/// A status tells which lines the fan may take and nothing of how the rest
/// are routed or held.
#[test]
fn a_status_tells_only_which_lines_the_fan_may_take() {
    let mut table = LineTable::default();
    for (line, state) in [
        (2, LineState::Peripheral { function: 4 }),
        (4, LineState::Interrupt),
        (
            7,
            LineState::Output {
                holder: Holder::Other,
                asserted: true,
            },
        ),
        (
            18,
            LineState::Output {
                holder: Holder::Caller,
                asserted: false,
            },
        ),
        (23, LineState::Free),
    ] {
        table.push(LineEntry { line, state }).unwrap();
    }
    let lines = crate::fan_lines(&table).expect("free lines are lent");
    assert_eq!(lines.free().iter().collect::<Vec<_>>(), [18, 23]);
    assert_eq!(lines.held().iter().collect::<Vec<_>>(), [2, 4, 7]);
}

#[test]
fn the_hottest_reading_decides_and_any_failure_fails_the_sample() {
    let at = Millicelsius::from_celsius;
    assert_eq!(
        Reading::hottest([Ok(at(40)), Ok(at(71)), Ok(at(55))]),
        Reading::Measured(at(71))
    );
    assert_eq!(
        Reading::hottest([Ok(at(40)), Err(Errno::NoData)]),
        Reading::Failed(Errno::NoData)
    );
    assert_eq!(Reading::hottest([]), Reading::NoSensor);
}

/// The plausible range's ends are readings; one past either fails the sample.
#[test]
fn only_a_temperature_a_processor_can_be_at_is_a_reading() {
    let at = Millicelsius::from_celsius;
    for edge in [Millicelsius(-55_000), Millicelsius(150_000)] {
        assert_eq!(Reading::hottest([Ok(edge)]), Reading::Measured(edge));
    }
    for past in [
        Millicelsius(-55_001),
        Millicelsius(150_001),
        Millicelsius(i32::MIN),
        Millicelsius(i32::MAX),
    ] {
        assert_eq!(
            Reading::hottest([Ok(at(60)), Ok(past)]),
            Reading::Failed(Errno::OutOfRange)
        );
    }
}

/// D1331: a sensor stuck at its code's end reads what no processor could be
/// at: a failure that runs the fan, and no temperature on the status.
#[test]
fn a_reading_no_processor_could_be_at_runs_the_fan() {
    let (mut machine, mut log) = (FakeMachine::at(40), Log::default());
    let mut service = service(&mut machine, policy(FanMode::Automatic, Some(18)), &mut log);
    tick(&mut service, &mut machine, &mut log);
    assert!(!service.fan().running());
    machine.sensors.insert(SENSOR, Ok(Millicelsius(-88_161)));
    tick(&mut service, &mut machine, &mut log);
    assert!(service.fan().running());
    assert_eq!(service.fan().fault(), Some(FanFault::SensorFailed));
    assert!(log.0.contains(&Entry::Fault {
        fault: FanFault::SensorFailed,
        refusal: Some(Errno::OutOfRange)
    }));
    assert_eq!(service.status(&mut machine).temperature, None);
}

/// D1329: an automatic fan that last decided to stop runs the moment it is
/// set always on.
#[test]
fn a_stopped_automatic_fan_set_always_on_starts() {
    let (mut machine, mut log) = (FakeMachine::at(40), Log::default());
    let mut service = service(&mut machine, policy(FanMode::Automatic, Some(18)), &mut log);
    tick(&mut service, &mut machine, &mut log);
    assert!(!service.fan().running());
    service.policy_changed(policy(FanMode::On, Some(18)), &mut machine, &mut log);
    assert_eq!(machine.calls.last(), Some(&Call::Drive(18, true)));
    assert_eq!(machine.held.get(&18), Some(&true));
    assert!(service.fan().running());
    assert_eq!(
        service.deadline_us(),
        None,
        "an always-on fan samples nothing"
    );
}

/// D1330: a switch the stalled header never answers keeps the claim and is
/// asked again a period later; the line is neither given back nor claimed
/// again, so it is never refused as busy.
#[test]
fn a_switch_left_unanswered_keeps_the_claim_and_is_asked_again() {
    let (mut machine, mut log) = (FakeMachine::at(70), Log::default());
    let mut service = service(&mut machine, policy(FanMode::Automatic, Some(18)), &mut log);
    tick(&mut service, &mut machine, &mut log);
    assert!(service.fan().running());
    machine.set(40);
    machine.answer = Answer::Withdrawn;
    let asked = machine.calls.len();
    tick(&mut service, &mut machine, &mut log);
    assert_eq!(machine.calls[asked..], [Call::Drive(18, false)]);
    assert_eq!(service.fan().line(), Some(18), "the claim is kept");
    assert_eq!(service.fan().fault(), Some(FanFault::LineUnavailable));
    assert!(log.0.contains(&Entry::Fault {
        fault: FanFault::LineUnavailable,
        refusal: Some(Errno::TimedOut)
    }));
    assert_eq!(
        service.deadline_us(),
        Some(machine.now_us + SAMPLE_PERIOD_US)
    );
    machine.answer = Answer::Prompt;
    tick(&mut service, &mut machine, &mut log);
    assert_eq!(
        machine.calls[asked..],
        [Call::Drive(18, false), Call::Drive(18, false)]
    );
    assert!(!service.fan().running());
    assert_eq!(service.fan().fault(), None);
    assert!(log.0.contains(&Entry::Recovered));
}

/// D1330: an always-on fan samples nothing, so a switch its header leaves
/// unanswered is asked again on a deadline of its own until it is answered.
#[test]
fn an_always_on_fan_asks_its_header_again_until_it_answers() {
    let (mut machine, mut log) = (FakeMachine::at(30), Log::default());
    let mut service = service(&mut machine, policy(FanMode::On, Some(18)), &mut log);
    assert_eq!(service.deadline_us(), None);
    machine.answer = Answer::Withdrawn;
    service.inventory_changed(present(), &mut machine, &mut log);
    assert_eq!(service.fan().fault(), Some(FanFault::LineUnavailable));
    assert_eq!(
        service.deadline_us(),
        Some(machine.now_us + SAMPLE_PERIOD_US)
    );
    tick(&mut service, &mut machine, &mut log);
    assert_eq!(service.fan().fault(), Some(FanFault::LineUnavailable));
    machine.answer = Answer::Prompt;
    tick(&mut service, &mut machine, &mut log);
    assert_eq!(service.fan().fault(), None);
    assert_eq!(service.deadline_us(), None, "tickless once answered");
    assert_eq!(
        machine.calls[1..],
        [
            Call::Drive(18, true),
            Call::Drive(18, true),
            Call::Drive(18, true)
        ]
    );
    let faults = log
        .0
        .iter()
        .filter(|entry| matches!(entry, Entry::Fault { .. }))
        .count();
    assert_eq!(faults, 1, "recorded once as it begins");
}

/// D1330: a claim the header carries out but answers too late is switched,
/// which finds it held, never claimed again, which the header refuses as
/// busy.
#[test]
fn a_claim_answered_too_late_is_found_held_not_claimed_again() {
    let (mut machine, mut log) = (FakeMachine::at(30), Log::default());
    machine.answer = Answer::Late;
    let mut service = service(&mut machine, policy(FanMode::On, Some(18)), &mut log);
    assert_eq!(service.fan().fault(), Some(FanFault::LineUnavailable));
    assert_eq!(machine.held.get(&18), Some(&true), "the claim landed");
    machine.answer = Answer::Prompt;
    tick(&mut service, &mut machine, &mut log);
    assert_eq!(
        machine.calls,
        [
            Call::Claim(18, Polarity::ActiveHigh, true),
            Call::Drive(18, true)
        ]
    );
    assert_eq!(service.fan().fault(), None);
    assert!(service.fan().running());
}

/// D1330: a line given back unanswered may still be the fan's, so it is
/// given back again before another is taken.
#[test]
fn a_release_left_unanswered_is_asked_again_before_another_line_is_taken() {
    let (mut machine, mut log) = (FakeMachine::at(30), Log::default());
    let mut service = service(&mut machine, policy(FanMode::On, Some(18)), &mut log);
    machine.answer = Answer::Withdrawn;
    service.policy_changed(policy(FanMode::On, Some(23)), &mut machine, &mut log);
    assert_eq!(service.fan().line(), Some(18), "still the fan's");
    assert_eq!(service.fan().fault(), Some(FanFault::LineUnavailable));
    machine.answer = Answer::Prompt;
    tick(&mut service, &mut machine, &mut log);
    assert_eq!(
        machine.calls,
        [
            Call::Claim(18, Polarity::ActiveHigh, true),
            Call::Release(18),
            Call::Release(18),
            Call::Claim(23, Polarity::ActiveHigh, true),
        ]
    );
    assert_eq!(machine.held.keys().copied().collect::<Vec<_>>(), [23]);
    assert_eq!(service.fan().fault(), None);
}

/// D1335: a line refused at every tree change is recorded once, as the
/// fault begins, with the refusal behind it.
#[test]
fn a_fault_is_recorded_once_however_often_it_is_met() {
    let mut machine = FakeMachine::at(30);
    machine.refuse_claim = Some(Errno::Busy);
    let mut log = Log::default();
    let mut service = service(&mut machine, policy(FanMode::On, Some(14)), &mut log);
    for _ in 0..3 {
        service.inventory_changed(present(), &mut machine, &mut log);
    }
    let faults: Vec<_> = log
        .0
        .iter()
        .filter(|entry| matches!(entry, Entry::Fault { .. }))
        .collect();
    assert_eq!(
        faults,
        [&Entry::Fault {
            fault: FanFault::LineUnavailable,
            refusal: Some(Errno::Busy)
        }]
    );
}
