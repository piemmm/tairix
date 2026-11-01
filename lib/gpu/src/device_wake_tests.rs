use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_abi_trap::seam;

use super::{waits, Wake};
use crate::device_tests::{
    buffer, context, queue, submit, unstarted, until, wakes, Unstarted, RAM,
};
use crate::model::Point;
use crate::Submission;

/// A clock that notes, at each reading, how many calls its thread had made.
#[derive(Default)]
struct Noting(std::sync::Mutex<Vec<usize>>);

impl MonotonicClock for Noting {
    fn now_ns(&self) -> u64 {
        self.0.lock().expect("unpoisoned").push(seam::calls().len());
        0
    }
}

#[test]
fn a_signal_wakes_its_waiters_before_the_work_it_readied_runs() {
    // On a thread of its own, as the seam stays armed for the thread.
    std::thread::spawn(|| {
        let clock = Arc::new(Noting::default());
        let device = crate::device_tests::software(
            RAM,
            Arc::clone(&clock) as Arc<dyn MonotonicClock + Send + Sync>,
        );
        let context = context(&device);
        let (_memory, target) = buffer(&context, 16);
        let gate = context.create_timeline(0).expect("timeline");
        let mut commands = context.command_buffer();
        commands.fill_buffer(&target, 0, 16, 7).expect("recorded");
        let fence = queue(&context)
            .submit(&Submission {
                command_buffers: &[&commands],
                waits: &[(&gate, 1)],
                signals: &[],
                deadline: Duration64::from_secs(1),
            })
            .expect("admitted");
        // A waiter on the gate, holding its wake as a parked one does.
        let point = gate.shared.model.lock().point(gate.context, gate.key, 1);
        let Ok(Point::Pending(_wake)) = point else {
            panic!("the gate is not yet reached");
        };
        let readings = clock.0.lock().expect("unpoisoned").len();
        seam::arm(0);
        gate.signal(1).expect("advances");
        let calls = seam::calls();
        let started = clock
            .0
            .lock()
            .expect("unpoisoned")
            .get(readings)
            .copied()
            .expect("the work read the clock as it started");
        assert_eq!(
            calls.get(..started).map(|calls| wakes(calls, u32::MAX)),
            Some(1),
            "the waiter woken before the work started"
        );
        assert_eq!(fence.is_signalled(), Ok(true));
    })
    .join()
    .expect("the test's thread");
}

#[test]
fn a_worker_poked_while_it_is_not_parked_is_not_woken() {
    std::thread::spawn(|| {
        let threads = Unstarted::default();
        let device = unstarted(&threads);
        let context = context(&device);
        let (_memory, target) = buffer(&context, 16);
        let mut commands = context.command_buffer();
        commands.fill_buffer(&target, 0, 16, 1).expect("recorded");
        seam::arm(0);
        let _fence = submit(&queue(&context), &commands).expect("admitted");
        assert!(device.shared.wake.state.lock().pending, "poked");
        assert_eq!(wakes(&seam::calls(), 1), 0, "with no call, as none waits");
    })
    .join()
    .expect("the test's thread");
}

#[test]
fn work_that_is_not_ready_pokes_no_worker() {
    let threads = Unstarted::default();
    let device = unstarted(&threads);
    let context = context(&device);
    let (_memory, target) = buffer(&context, 16);
    let gate = context.create_timeline(0).expect("timeline");
    let mut commands = context.command_buffer();
    commands.fill_buffer(&target, 0, 16, 1).expect("recorded");
    let _fence = queue(&context)
        .submit(&Submission {
            command_buffers: &[&commands],
            waits: &[(&gate, 1)],
            signals: &[],
            deadline: Duration64::from_secs(1),
        })
        .expect("admitted");
    assert!(
        !device.shared.wake.state.lock().pending,
        "nothing to run, so nothing poked"
    );
    gate.signal(1).expect("advances");
    assert!(
        device.shared.wake.state.lock().pending,
        "poked once it is ready"
    );
}

#[test]
fn a_poke_wakes_the_worker_only_while_it_waits() {
    std::thread::spawn(|| {
        let wake = Wake::new();
        seam::arm(0);
        wake.poke(false);
        assert_eq!(wakes(&seam::calls(), 1), 0, "not waiting, so not woken");
        assert!(wake.park(), "the poke kept for it");
        // As a worker waiting is.
        wake.state.lock().parked = true;
        let before = seam::calls().len();
        wake.poke(false);
        assert_eq!(wakes(&seam::calls()[before..], 1), 1, "waiting, so woken");
    })
    .join()
    .expect("the test's thread");
}

#[test]
fn a_worker_is_parked_only_while_it_waits() {
    let wake = Arc::new(Wake::new());
    let worker = {
        let wake = Arc::clone(&wake);
        std::thread::spawn(move || (wake.park(), waits::unflagged()))
    };
    let parked = until(|| wake.state.lock().parked);
    // Poked whatever it saw, so a worker that never parks still leaves.
    wake.poke(false);
    let (there, unflagged) = worker.join().expect("the worker");
    assert!(parked, "the worker never parked");
    assert!(there, "the device still there");
    // The host's waits return at once, so the worker made many.
    assert_eq!(unflagged, 0, "a wait made unflagged, which a poke skips");
    assert!(!wake.state.lock().parked, "not parked once it leaves");
}
