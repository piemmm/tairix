//! Loom model-checking harness for the run-queue's lock-free fast path.
//!
//! Run with:
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --test loom \
//!     -p tairix-kernel-sched-mlfq --release
//! ```
//!
//! When the `loom` cfg is *not* enabled, the file compiles to an empty
//! test binary so the default `cargo test` workflow stays fast.
//!
//! **Nothing runs these models today, and the command above does not build.**
//! `cargo xtask loom` enrols `tairix-sync` only, and under `--cfg loom` the
//! crate graph fails to compile: loom's atomics have no `const` constructor,
//! so every `const fn`-built static below this crate is rejected in a static
//! initialiser (`kernel/arch/api`'s `ACTIVE_FRAMES` is the first). See
//! `plans/OPEN-DEFECTS.md` D131 — until that is resolved these models are
//! unverified, so do not read their presence as coverage.
//!
//! Each model checks that a task is handed to exactly one party, never lost
//! and never duplicated: two consumers racing for the last entry, and two
//! producers pushing at once.

#![cfg(loom)]

use loom::sync::Arc as LoomArc;
use loom::thread;
use std::sync::Arc;

use tairix_kernel_sched_mlfq::runqueue::{RunDeque, Steal};

fn taken(steal: Steal<Arc<u64>>) -> Option<u64> {
    match steal {
        Steal::Stolen(task) => Some(*task),
        Steal::Empty | Steal::Retry => None,
    }
}

#[test]
fn loom_push_then_steal_single_thread() {
    loom::model(|| {
        let q = RunDeque::try_new(4).expect("cap 4");
        q.push(Arc::new(7)).expect("push");
        assert_eq!(taken(q.steal()), Some(7));
        assert_eq!(q.steal(), Steal::Empty);
    });
}

#[test]
fn loom_two_consumers_race_for_the_last_entry() {
    loom::model(|| {
        let q = LoomArc::new(RunDeque::try_new(2).expect("cap 2"));
        q.push(Arc::new(42)).expect("push");

        let q2 = q.clone();
        let rival = thread::spawn(move || taken(q2.steal()));
        let mine = taken(q.steal());
        let theirs = rival.join().expect("join");

        let count = mine.iter().count() + theirs.iter().count();
        assert!(count <= 1, "an entry is never handed to both");
        if count == 0 {
            assert_eq!(taken(q.steal()), Some(42), "a lost race leaves it queued");
        } else {
            assert_eq!(mine.or(theirs), Some(42));
            assert_eq!(q.steal(), Steal::Empty);
        }
    });
}

#[test]
fn loom_two_producers_never_share_a_slot() {
    loom::model(|| {
        let q = LoomArc::new(RunDeque::try_new(2).expect("cap 2"));
        let q2 = q.clone();
        let rival = thread::spawn(move || q2.push(Arc::new(2)).is_ok());
        assert!(q.push(Arc::new(1)).is_ok());
        assert!(rival.join().expect("join"));

        let mut got = [taken(q.steal()), taken(q.steal())];
        got.sort_unstable();
        assert_eq!(got, [Some(1), Some(2)], "both pushes kept");
        assert_eq!(q.steal(), Steal::Empty);
    });
}
