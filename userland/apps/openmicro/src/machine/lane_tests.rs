use alloc::boxed::Box;
use alloc::vec::Vec;

use super::host::HostThreads;
use super::*;

/// A device that records every input with the cycle it was at when it took
/// it, and moves in pieces of seven cycles ahead.
#[derive(Default)]
struct Recorder {
    now: u64,
    taken: Vec<(u64, u32)>,
}

impl Device for Recorder {
    type Input = u32;

    fn catch_up(&mut self, to: u64) {
        self.now = self.now.max(to);
    }

    fn run_ahead(&mut self, limit: u64) -> bool {
        if self.now >= limit {
            return false;
        }
        self.now = limit.min(self.now + 7);
        true
    }

    fn take(&mut self, at: u64, input: u32) {
        assert_eq!(self.now, at, "an input is taken where it lands");
        self.taken.push((self.now, input));
    }
}

struct NoThreads;

impl Spawner for NoThreads {
    fn spawn(&self, _: Box<dyn FnOnce() + Send>) -> Option<Box<dyn Joiner>> {
        None
    }
}

/// A machine's run: inputs posted as it goes, its time published, and the
/// device read at cycles along the way.
fn run(lane: &Lane<Recorder>) -> Vec<(u64, u64, usize)> {
    let mut read = Vec::new();
    for cycle in 1..=20_000_u64 {
        if cycle % 13 == 0 {
            lane.post(cycle, u32::try_from(cycle).unwrap_or(u32::MAX));
        }
        if cycle % 64 == 0 {
            lane.publish(cycle);
        }
        if cycle % 997 == 0 {
            let device = lane.at(cycle);
            read.push((cycle, device.now, device.taken.len()));
        }
    }
    read
}

#[test]
fn a_runner_changes_nothing_a_device_is_read_as() {
    let inline = Lane::new(Recorder::default());
    let expected = run(&inline);
    let taken = inline.at(20_000).taken.clone();
    for _ in 0..8 {
        let mut threaded = Lane::new(Recorder::default());
        threaded.spread(&HostThreads);
        assert_eq!(run(&threaded), expected);
        assert_eq!(threaded.at(20_000).taken, taken);
    }
}

#[test]
fn every_read_finds_the_device_at_its_cycle_with_what_landed_by_then() {
    let mut lane = Lane::new(Recorder::default());
    lane.spread(&HostThreads);
    for (cycle, now, taken) in run(&lane) {
        assert_eq!(now, cycle);
        assert_eq!(taken, usize::try_from(cycle / 13).unwrap_or(usize::MAX));
    }
}

#[test]
fn a_device_put_back_in_time_is_not_run_past_the_machine() {
    let mut lane = Lane::new(Recorder::default());
    lane.spread(&HostThreads);
    lane.publish(50_000);
    lane.replace(
        100,
        Recorder {
            now: 100,
            taken: Vec::new(),
        },
    );
    lane.post(150, 7);
    let device = lane.at(200);
    assert_eq!(device.now, 200);
    assert_eq!(device.taken, alloc::vec![(150, 7)]);
}

#[test]
fn without_a_thread_the_machine_brings_the_device_up_itself() {
    let mut lane = Lane::new(Recorder::default());
    lane.spread(&NoThreads);
    lane.post(40, 1);
    assert_eq!(lane.at(40).taken, alloc::vec![(40, 1)], "taken at once");
    lane.publish(10_000);
    assert_eq!(lane.at(41).now, 41, "nothing ran ahead of the machine");
}

#[test]
fn dropping_a_lane_ends_its_runner() {
    let mut lane = Lane::new(Recorder::default());
    lane.spread(&HostThreads);
    lane.publish(5_000);
    drop(lane);
}
