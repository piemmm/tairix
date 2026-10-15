//! Loom models of the park/unpark handshake.
//!
//! The handshake is generic over `ParkableTask`, so the models drive the real
//! protocol over a lifecycle word held in loom's own atomic: the crate graph
//! builds as usual and only the `loom` feature links the checker. Driven by
//! `cargo xtask loom`; directly, that is:
//!
//! ```text
//! cargo test -p tairix-kernel-sched-api --features loom --test loom --release
//! ```

#![cfg(feature = "loom")]

use loom::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use loom::sync::Arc;
use loom::thread;

use tairix_kernel_sched_api::park::{self, Lifecycle, Settled};
use tairix_kernel_sched_api::{ParkableTask, TaskAction, TaskState};

/// A task whose lifecycle word the model checker interleaves.
struct Task {
    word: AtomicU8,
    admitted: AtomicUsize,
}

impl Task {
    fn new(state: TaskState) -> Arc<Self> {
        Arc::new(Self {
            word: AtomicU8::new(Lifecycle::of(state).bits()),
            admitted: AtomicUsize::new(0),
        })
    }

    fn admit(&self) {
        self.admitted.fetch_add(1, Ordering::AcqRel);
    }

    fn admitted(&self) -> usize {
        self.admitted.load(Ordering::Acquire)
    }

    /// Settle a body that asked to park, admitting the task as a policy's
    /// dispatch does when the park was cancelled.
    fn settle_park(&self) -> Settled {
        let settled = park::settle(self, TaskAction::Park, false, |transition| transition());
        if settled == Settled::Woken {
            self.admit();
        }
        settled
    }
}

impl ParkableTask for Task {
    fn load_lifecycle(&self) -> Lifecycle {
        Lifecycle::from_bits(self.word.load(Ordering::Acquire))
    }

    fn cas_lifecycle(&self, expected: Lifecycle, new: Lifecycle) -> Result<(), Lifecycle> {
        self.word
            .compare_exchange(
                expected.bits(),
                new.bits(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(Lifecycle::from_bits)
    }

    fn swap_lifecycle(&self, new: Lifecycle) -> Lifecycle {
        Lifecycle::from_bits(self.word.swap(new.bits(), Ordering::AcqRel))
    }
}

fn wait_until(ready: impl Fn() -> bool) {
    while !ready() {
        thread::yield_now();
    }
}

/// A wake racing the park it answers ends the park exactly once, whichever
/// side the exchange orders first.
#[test]
fn a_wake_racing_a_park_is_never_lost() {
    loom::model(|| {
        let task = Task::new(TaskState::Running);
        let waker = {
            let task = Arc::clone(&task);
            thread::spawn(move || park::unpark_task(&*task, Task::admit))
        };
        let settled = task.settle_park();
        assert_eq!(waker.join().expect("waker"), Ok(()));
        assert!(matches!(settled, Settled::Woken | Settled::Released));
        assert_eq!(task.admitted(), 1, "admitted once, by one side");
        assert_eq!(task.load_lifecycle(), Lifecycle::of(TaskState::Ready));
    });
}

/// CPU A parks the task on a first lock while that lock's releaser wakes it;
/// CPU B then runs it, registers it on a second lock and parks it again while
/// the second releaser wakes it. CPU A is never joined before CPU B acts, so
/// anything it did about the first park after deciding it would race the
/// second park here; the second wake must still end the second park.
#[test]
fn a_wake_left_for_a_later_park_is_kept_for_that_park() {
    loom::model(|| {
        let task = Task::new(TaskState::Running);
        let cpu_a = {
            let task = Arc::clone(&task);
            thread::spawn(move || task.settle_park())
        };

        assert_eq!(
            park::unpark_task(&*task, Task::admit),
            Ok(()),
            "first release"
        );
        wait_until(|| task.admitted() == 1);
        assert_eq!(task.cas_state(TaskState::Ready, TaskState::Running), Ok(()));
        let second_release = {
            let task = Arc::clone(&task);
            thread::spawn(move || park::unpark_task(&*task, Task::admit))
        };
        let second = task.settle_park();

        assert!(matches!(
            cpu_a.join().expect("cpu a"),
            Settled::Woken | Settled::Released
        ));
        assert_eq!(second_release.join().expect("second release"), Ok(()));
        assert!(matches!(second, Settled::Woken | Settled::Released));
        assert_eq!(task.admitted(), 2, "each park ended by its own wake");
        assert_eq!(task.load_lifecycle(), Lifecycle::of(TaskState::Ready));
    });
}

/// A stop and its continue landing anywhere around a park and the wake that
/// answers it neither lose the wake nor admit the task twice.
#[test]
fn a_stop_and_continue_around_a_park_keep_its_wake() {
    loom::model(|| {
        let task = Task::new(TaskState::Running);
        let job_control = {
            let task = Arc::clone(&task);
            thread::spawn(move || {
                park::stop_task(&*task).expect("live task");
                park::resume_task(&*task, Task::admit)
            })
        };
        let waker = {
            let task = Arc::clone(&task);
            thread::spawn(move || park::unpark_task(&*task, Task::admit))
        };
        let settled = task.settle_park();

        assert_eq!(job_control.join().expect("job control"), Ok(()));
        assert_eq!(waker.join().expect("waker"), Ok(()));
        assert!(matches!(settled, Settled::Woken | Settled::Released));
        assert_eq!(task.admitted(), 1, "runnable once, by exactly one party");
        assert_eq!(task.load_lifecycle(), Lifecycle::of(TaskState::Ready));
    });
}
