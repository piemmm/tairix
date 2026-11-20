//! Loom models of a device lane: the lane itself, over loom's locks and
//! threads, its runner against the machine in every interleaving the bound
//! allows. Driven by `cargo xtask loom`; directly, that is:
//!
//! ```text
//! cargo test -p tairix-openmicro --features loom --test loom --release
//! ```

#![cfg(feature = "loom")]

use loom::thread::{self, JoinHandle};

use tairix_openmicro::machine::lane::{Device, Joiner, Lane, Spawner};

/// A device that records each input with the cycle it was at when it took
/// it, and the furthest cycle it was ever brought to.
struct Recorder {
    now: u64,
    furthest: u64,
    taken: Vec<(u64, u32)>,
}

impl Recorder {
    const fn at(now: u64) -> Self {
        Self {
            now,
            furthest: now,
            taken: Vec::new(),
        }
    }

    fn reach(&mut self, to: u64) {
        self.now = self.now.max(to);
        self.furthest = self.furthest.max(self.now);
    }
}

impl Device for Recorder {
    type Input = u32;

    fn catch_up(&mut self, to: u64) {
        self.reach(to);
    }

    fn run_ahead(&mut self, limit: u64) -> bool {
        if self.now >= limit {
            return false;
        }
        self.reach(limit.min(self.now + 2_500));
        true
    }

    fn take(&mut self, _: u64, input: u32) {
        self.taken.push((self.now, input));
    }
}

struct LoomThreads;

struct LoomJoin(JoinHandle<()>);

impl Joiner for LoomJoin {
    fn join(self: Box<Self>) {
        assert!(self.0.join().is_ok(), "the runner ended cleanly");
    }
}

impl Spawner for LoomThreads {
    fn spawn(&self, body: Box<dyn FnOnce() + Send>) -> Option<Box<dyn Joiner>> {
        Some(Box::new(LoomJoin(thread::spawn(body))))
    }
}

fn bounded() -> loom::model::Builder {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder
}

/// Inputs are taken where they land, a device is run no further than the
/// machine has said it has run, and a device put back in time is never run
/// past the machine's restored time by a limit read before.
#[test]
fn a_runner_takes_a_device_only_where_the_machine_has_been() {
    bounded().check(|| {
        let mut lane = Lane::new(Recorder::at(0));
        lane.spread(&LoomThreads);
        lane.post(100, 1);
        lane.publish(5_000);
        {
            let device = lane.at(3_000);
            assert!(device.now >= 3_000);
            assert!(device.furthest <= 5_000, "past {}", device.furthest);
            assert_eq!(device.taken, [(100, 1)]);
        }
        lane.replace(200, Recorder::at(200));
        lane.post(300, 2);
        let device = lane.at(400);
        assert_eq!(device.now, 400);
        assert!(device.furthest <= 400, "past {}", device.furthest);
        assert_eq!(device.taken, [(300, 2)]);
    });
}

/// A lane let go ends its runner, parked or not yet begun.
#[test]
fn a_lane_let_go_ends_its_runner() {
    loom::model(|| {
        let mut lane = Lane::new(Recorder::at(0));
        lane.spread(&LoomThreads);
        lane.publish(2_048);
        drop(lane);
    });
}
