//! Host tests of the held-writes questions: which volumes await an answer,
//! what a question says and its buttons give, what a refusal tells, and an
//! answer carried with a read behind it, its volume not asked about again
//! until that read lands.

use alloc::string::String;
use alloc::vec::Vec;
use alloc::{format, vec};

use tairix_abi::driver::filesystem::{MountFlags, VolumeStats};
use tairix_abi::sysinfo::{
    MountAvailability, MountRecord, MountVolumeState, VolumeClass, VOLUME_ID_UNPUBLISHED,
};
use tairix_abi::volume::{ReplayAnswer, VolumeReplayRequest};
use tairix_abi::window_ipc::{AlertButtonRole, AlertSeverity};
use tairix_abi::Errno;
use tairix_wm::Compositor;

use super::{
    answer_of, question, refusal, work, Held, HeldDone, HeldJob, HeldQuestions, HeldSystem,
    HeldWord,
};
use crate::alert::{Alerts, HELD_QUESTIONS_MAX};
use crate::hold::Holds;
use crate::shell::DesktopShell;
use crate::tests::headless_desktop;

/// A removable device's volume attached at `/Storage/{name}` under key
/// `volume`, `availability`.
fn record(name: &str, volume: [u8; 16], availability: MountAvailability) -> MountRecord {
    MountRecord::new(
        name.as_bytes(),
        format!("/Storage/{name}").as_bytes(),
        b"fat32",
        MountFlags::NOEXEC,
        MountVolumeState {
            usage: VolumeStats::default(),
            availability,
            medium: None,
        },
        volume,
    )
    .expect("a valid record")
    .with_class(VolumeClass::Removable)
}

/// A mount table and a kernel standing in for the real ones: what the table
/// lists, what the kernel answers, and each answer it was asked.
struct Machine {
    mounts: Result<Vec<MountRecord>, Errno>,
    answer: Result<(), Errno>,
    asked: Vec<VolumeReplayRequest>,
}

impl Machine {
    fn listing(mounts: Vec<MountRecord>) -> Self {
        Self {
            mounts: Ok(mounts),
            answer: Ok(()),
            asked: Vec::new(),
        }
    }
}

impl HeldSystem for Machine {
    fn mounts(&mut self, sink: &mut dyn FnMut(&MountRecord)) -> Result<(), Errno> {
        for record in self.mounts.as_ref().map_err(|errno| *errno)? {
            sink(record);
        }
        Ok(())
    }

    fn replay(&mut self, request: &VolumeReplayRequest) -> Result<(), Errno> {
        self.asked.push(*request);
        self.answer
    }
}

#[test]
fn only_a_removable_volume_served_for_the_seats_answer_and_shown_here_awaits_one() {
    let backup = [0x11; 16];
    let mut machine = Machine::listing(vec![
        record("Backup", backup, MountAvailability::ReplayUnconfirmed),
        record("Music", [0x22; 16], MountAvailability::Available),
        record("Clash", [0x33; 16], MountAvailability::RecoveryConflict),
        record("Away", [0x44; 16], MountAvailability::UnavailableDirty),
        record("Theirs", [0x55; 16], MountAvailability::ReplayUnconfirmed).withheld(),
        record("Internal", [0x66; 16], MountAvailability::ReplayUnconfirmed)
            .with_class(VolumeClass::Local),
        record(
            "Layout",
            VOLUME_ID_UNPUBLISHED,
            MountAvailability::ReplayUnconfirmed,
        ),
    ]);
    assert_eq!(
        work(&mut machine, &mut HeldJob::Read),
        HeldDone::Read(Ok(vec![Held {
            volume: backup,
            name: String::from("Backup"),
        }]))
    );
    machine.mounts = Err(Errno::PermissionDenied);
    assert_eq!(
        work(&mut machine, &mut HeldJob::Read),
        HeldDone::Read(Err(Errno::PermissionDenied)),
        "a table not read is said so, never read as empty"
    );
}

#[test]
fn an_answer_is_carried_to_the_kernel_as_given_and_its_word_returned() {
    let write = VolumeReplayRequest {
        volume_id: [0x11; 16],
        answer: ReplayAnswer::Write,
    };
    let mut machine = Machine::listing(Vec::new());
    machine.answer = Err(Errno::ReadOnly);
    assert_eq!(
        work(&mut machine, &mut HeldJob::Answer(write)),
        HeldDone::Answered(write, Err(Errno::ReadOnly))
    );
    assert_eq!(machine.asked, [write]);
}

#[test]
fn the_question_names_the_volume_and_its_buttons_give_discard_then_write() {
    let spec = question("Backup").expect("a question");
    assert_eq!(spec.severity(), AlertSeverity::Question);
    assert_eq!(
        spec.headline(),
        "Write the changes waiting for \u{201c}Backup\u{201d}?"
    );
    let buttons: Vec<(&str, AlertButtonRole)> = spec
        .buttons()
        .map(|button| (button.label(), button.role()))
        .collect();
    assert_eq!(
        buttons,
        [
            ("Discard", AlertButtonRole::Destructive),
            ("Write", AlertButtonRole::Default)
        ]
    );
    assert_eq!(answer_of(0), Some(ReplayAnswer::Discard));
    assert_eq!(answer_of(1), Some(ReplayAnswer::Write));
    assert_eq!(answer_of(2), None);
    let hostile = question("Back\u{202e}pu").expect("a question");
    assert!(
        !hostile.headline().contains('\u{202e}'),
        "nothing in a name turns its line around"
    );
}

#[test]
fn a_refusal_is_told_where_the_questions_own_fate_does_not_tell_it() {
    for errno in [
        Errno::NotFound,
        Errno::Stale,
        Errno::Busy,
        Errno::OutOfMemory,
        Errno::LimitExceeded,
    ] {
        assert_eq!(
            refusal("Backup", ReplayAnswer::Write, errno),
            None,
            "{errno:?}"
        );
    }
    let unwritten = refusal("Backup", ReplayAnswer::Write, Errno::ReadOnly).expect("told");
    assert_eq!(
        unwritten.headline(),
        "The changes waiting for \u{201c}Backup\u{201d} were not written"
    );
    assert!(unwritten.detail().contains("read-only"));
    let undiscarded = refusal("Backup", ReplayAnswer::Discard, Errno::DeviceOffline).expect("told");
    assert_eq!(
        undiscarded.headline(),
        "The changes waiting for \u{201c}Backup\u{201d} were not discarded"
    );
    for errno in [Errno::PermissionDenied, Errno::MediumError] {
        assert!(
            refusal("Backup", ReplayAnswer::Write, errno).is_some(),
            "{errno:?}"
        );
    }
}

/// The worker's desk: every job handed to it, in order, or none while `full`.
#[derive(Default)]
struct Desk {
    jobs: Vec<HeldJob>,
    full: bool,
}

impl Desk {
    fn submit(&mut self, job: HeldJob) -> Result<(), HeldJob> {
        if self.full {
            return Err(job);
        }
        self.jobs.push(job);
        Ok(())
    }
}

/// The desktop the questions are put up on, the worker's desk, and the
/// questions' own state.
struct Fixture {
    shell: DesktopShell,
    comp: Compositor,
    alerts: Alerts,
    holds: Holds,
    desk: Desk,
    questions: HeldQuestions,
}

impl Fixture {
    fn new() -> Self {
        let (shell, comp) = headless_desktop();
        Self {
            shell,
            comp,
            alerts: Alerts::default(),
            holds: Holds::new(),
            desk: Desk::default(),
            questions: HeldQuestions::default(),
        }
    }

    /// Adopt `done`, as the worker landing it.
    fn adopt(&mut self, done: HeldDone) -> HeldWord {
        let desk = &mut self.desk;
        self.questions.adopt(
            done,
            &mut |job| desk.submit(job),
            (&mut self.alerts, &mut self.holds),
            &mut self.shell,
            &mut self.comp,
        )
    }

    /// The mount table moved.
    fn moved(&mut self) {
        let desk = &mut self.desk;
        self.questions.moved();
        self.questions.read(&mut |job| desk.submit(job));
    }

    /// The user answers button `index` for volume `n`: its question comes
    /// down, and the answer goes to the worker with a read owed behind it.
    fn answer(&mut self, n: u8, index: u8) -> bool {
        self.alerts.withdraw_held(
            |volume| *volume != [n; 16],
            &mut self.holds,
            &mut self.shell,
            &mut self.comp,
        );
        let desk = &mut self.desk;
        let carried = self
            .questions
            .carry(([n; 16], index), &mut |job| desk.submit(job));
        self.questions.read(&mut |job| desk.submit(job));
        carried
    }
}

/// What a read finds: volumes `n` awaiting an answer, each named `Disk n`.
fn found(volumes: &[u8]) -> HeldDone {
    HeldDone::Read(Ok(volumes
        .iter()
        .map(|&n| Held {
            volume: [n; 16],
            name: format!("Disk {n}"),
        })
        .collect()))
}

fn write(n: u8) -> VolumeReplayRequest {
    VolumeReplayRequest {
        volume_id: [n; 16],
        answer: ReplayAnswer::Write,
    }
}

#[test]
fn each_volume_awaiting_an_answer_is_asked_once_and_its_question_comes_down_once_it_does_not() {
    let mut fixture = Fixture::new();
    fixture.moved();
    fixture.moved();
    assert_eq!(
        fixture.desk.jobs,
        [HeldJob::Read],
        "one read under way at once"
    );
    assert_eq!(fixture.adopt(found(&[1, 2])), HeldWord::Nothing);
    assert_eq!(
        fixture.desk.jobs,
        [HeldJob::Read, HeldJob::Read],
        "a move during a read is read again"
    );
    assert!(fixture.alerts.asks_held(&[1; 16]) && fixture.alerts.asks_held(&[2; 16]));
    assert_eq!(fixture.adopt(found(&[1, 2])), HeldWord::Nothing);
    assert_eq!(fixture.alerts.held_questions(), 2, "asked once each");
    fixture.moved();
    assert_eq!(fixture.adopt(found(&[2])), HeldWord::Nothing);
    assert!(
        !fixture.alerts.asks_held(&[1; 16]),
        "down once it no longer awaits"
    );
    assert!(fixture.alerts.asks_held(&[2; 16]));
}

#[test]
fn an_answer_goes_with_a_read_behind_it_and_its_volume_is_not_asked_again_until_that_read_lands() {
    let mut fixture = Fixture::new();
    fixture.moved();
    let _ = fixture.adopt(found(&[1]));
    // A move is read before the answer is carried, and lands after it.
    fixture.moved();
    assert!(fixture.answer(1, 1), "carried");
    assert_eq!(
        fixture.desk.jobs,
        [HeldJob::Read, HeldJob::Read, HeldJob::Answer(write(1))],
        "the read behind the answer waits for the one under way"
    );
    assert_eq!(fixture.adopt(found(&[1])), HeldWord::Nothing);
    assert!(
        !fixture.alerts.asks_held(&[1; 16]),
        "a read from before the answer asks nothing"
    );
    assert_eq!(
        fixture.desk.jobs.last(),
        Some(&HeldJob::Read),
        "the read behind it is asked now"
    );
    assert_eq!(
        fixture.adopt(HeldDone::Answered(write(1), Ok(()))),
        HeldWord::Nothing
    );
    assert_eq!(fixture.adopt(found(&[])), HeldWord::Nothing);
    assert_eq!(fixture.alerts.held_questions(), 0);
}

#[test]
fn an_answer_the_kernel_did_not_carry_out_is_asked_again_or_told() {
    let mut fixture = Fixture::new();
    fixture.moved();
    let _ = fixture.adopt(found(&[1, 2]));
    assert!(fixture.answer(1, 1));
    assert!(fixture.answer(2, 1));
    let busy = fixture.adopt(HeldDone::Answered(write(1), Err(Errno::Busy)));
    assert!(matches!(busy, HeldWord::Note(_)), "{busy:?}");
    let _ = fixture.adopt(found(&[1, 2]));
    assert!(
        fixture.alerts.asks_held(&[1; 16]),
        "still awaited, so asked again"
    );
    assert!(
        !fixture.alerts.asks_held(&[2; 16]),
        "its answer is with the kernel"
    );
    let unwritten = fixture.adopt(HeldDone::Answered(write(2), Err(Errno::ReadOnly)));
    let HeldWord::Refusal(told) = unwritten else {
        panic!("{unwritten:?}");
    };
    assert_eq!(
        told.headline(),
        "The changes waiting for \u{201c}Disk 2\u{201d} were not written"
    );
    let _ = fixture.adopt(found(&[1]));
    assert!(!fixture.alerts.asks_held(&[2; 16]), "it no longer awaits");
}

#[test]
fn questions_and_the_answers_with_the_worker_stay_within_the_bound() {
    let mut fixture = Fixture::new();
    let all = [1, 2, 3, 4, 5, 6];
    fixture.moved();
    let _ = fixture.adopt(found(&all));
    assert_eq!(fixture.alerts.held_questions(), HELD_QUESTIONS_MAX);
    fixture.moved();
    assert!(fixture.answer(1, 0));
    let _ = fixture.adopt(found(&all));
    assert_eq!(
        fixture.alerts.held_questions(),
        HELD_QUESTIONS_MAX - 1,
        "an answer with the worker still counts"
    );
    let discard = VolumeReplayRequest {
        volume_id: [1; 16],
        answer: ReplayAnswer::Discard,
    };
    let _ = fixture.adopt(HeldDone::Answered(discard, Ok(())));
    let _ = fixture.adopt(found(&all[1..]));
    assert_eq!(fixture.alerts.held_questions(), HELD_QUESTIONS_MAX);
    assert!(
        fixture.alerts.asks_held(&[5; 16]),
        "the next is asked as one is answered"
    );
}

#[test]
fn an_answer_the_worker_has_no_room_for_waits_and_one_no_volume_awaits_is_dropped() {
    let mut fixture = Fixture::new();
    fixture.moved();
    let _ = fixture.adopt(found(&[1]));
    fixture.desk.full = true;
    assert!(!fixture.answer(1, 1), "no room: left for the next pass");
    fixture.desk.full = false;
    let jobs = fixture.desk.jobs.len();
    assert!(fixture.answer(1, 1));
    assert_eq!(
        fixture.desk.jobs.len(),
        jobs + 2,
        "the answer, and the read behind it"
    );
    assert!(fixture.answer(9, 1), "dealt with: no volume 9 awaits one");
    assert_eq!(
        fixture.desk.jobs.len(),
        jobs + 2,
        "and nothing carried for it"
    );
}

#[test]
fn an_unreadable_mount_table_is_noted_and_leaves_the_questions_standing() {
    let mut fixture = Fixture::new();
    fixture.moved();
    let _ = fixture.adopt(found(&[1]));
    fixture.moved();
    let jobs = fixture.desk.jobs.len();
    let word = fixture.adopt(HeldDone::Read(Err(Errno::PermissionDenied)));
    assert!(matches!(word, HeldWord::Note(_)), "{word:?}");
    assert!(fixture.alerts.asks_held(&[1; 16]), "the questions stand");
    assert_eq!(fixture.desk.jobs.len(), jobs, "not read again in a loop");
}
