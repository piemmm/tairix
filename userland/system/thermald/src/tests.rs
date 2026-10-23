extern crate std;

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

const HEADER: u64 = 0x4748_0000_0000_0005;
const SENSOR: u64 = 0x5453_0000_0000_0003;

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
        if let Some(refusal) = self.refuse_claim {
            return Err(refusal);
        }
        if self.held.contains_key(&line) {
            return Err(Errno::Busy);
        }
        self.held.insert(line, asserted);
        Ok(())
    }

    fn drive(&mut self, endpoint: u64, line: u8, asserted: bool) -> Result<(), Errno> {
        assert_eq!(endpoint, HEADER);
        self.calls.push(Call::Drive(line, asserted));
        if let Some(refusal) = self.refuse_drive {
            return Err(refusal);
        }
        let held = self.held.get_mut(&line).ok_or(Errno::NotFound)?;
        *held = asserted;
        Ok(())
    }

    fn release(&mut self, endpoint: u64, line: u8) -> Result<(), Errno> {
        assert_eq!(endpoint, HEADER);
        self.calls.push(Call::Release(line));
        self.held.remove(&line).map(|_| ()).ok_or(Errno::NotFound)
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
    let due = service.deadline_us().expect("an automatic fan samples");
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
                    refusal: None,
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
        Reading::Failed
    );
    assert_eq!(Reading::hottest([]), Reading::NoSensor);
}
