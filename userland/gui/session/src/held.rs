//! The writes a re-inserted volume holds while nothing proves its medium the
//! one they were made on (`docs/src/filesystem/drives.md` §8): the question
//! put to the user at the seat, and the answer carried to the kernel.
//!
//! The kernel serves such a volume read-only, its writes kept, until it is
//! answered for, and takes the seat's answer only from the process holding the
//! seat and only for a removable device's volume, so only such a volume is
//! asked about: any other is the mount authority's. One worker reads which
//! volumes wait and carries each answer out, off the serve loop since writing
//! held writes is the medium's own I/O, and in the order they were asked: the
//! read asked behind an answer sees what it did, so a question is never put up
//! again for an answer the kernel already has.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::sysinfo::{MountAvailability, MountRecord, VolumeClass, VOLUME_ID_UNPUBLISHED};
use tairix_abi::volume::{ReplayAnswer, VolumeReplayRequest, VOLUME_ID_LEN};
use tairix_abi::window_ipc::{AlertButton, AlertButtonRole, AlertSeverity, AlertSpec};
use tairix_abi::Errno;
use tairix_browse::Volume;
use tairix_util::utf8::shown;
use tairix_wm::Compositor;

use crate::alert::{Alerts, HELD_QUESTIONS_MAX};
use crate::desktop::GestureRefusal;
use crate::hold::Holds;
use crate::shell::DesktopShell;

/// The most jobs the worker holds at once: an answer for each question the
/// bound admits, and the read under way.
pub const HELD_JOBS: usize = HELD_QUESTIONS_MAX + 1;

/// The answers a question offers, in the order its buttons stand. The
/// keyboard starts on writing, since the medium put back is almost always the
/// one pulled; discarding destroys, so no stray key gives it.
const ANSWERS: [(&str, AlertButtonRole, ReplayAnswer); 2] = [
    (
        "Discard",
        AlertButtonRole::Destructive,
        ReplayAnswer::Discard,
    ),
    ("Write", AlertButtonRole::Default, ReplayAnswer::Write),
];

/// What a question says beneath its headline.
const DETAIL: &str = "The disk was put back, but nothing tells it apart from a copy of the one \
                      the changes were made on. Until you choose, it stays read-only and the \
                      changes are kept.\nWrite them if it is that disk; discard them if it is \
                      a copy.";

/// A volume awaiting the seat's answer for the writes it holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Held {
    /// The key it is published under, which an answer names.
    pub volume: [u8; VOLUME_ID_LEN],
    /// Its catalog name, which its question shows.
    pub name: String,
}

/// What the worker is asked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HeldJob {
    /// Read which volumes await an answer.
    Read,
    /// Carry an answer to the kernel.
    Answer(VolumeReplayRequest),
}

/// What the worker did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeldDone {
    /// The volumes awaiting an answer, or why the mount table was not read.
    Read(Result<Vec<Held>, Errno>),
    /// The kernel's word on an answer.
    Answered(VolumeReplayRequest, Result<(), Errno>),
}

/// What the desktop owes the user of the worker's word.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeldWord {
    /// Nothing beyond the questions following it.
    Nothing,
    /// A line for `stderr`.
    Note(String),
    /// A refusal for the user to read.
    Refusal(GestureRefusal),
}

/// The system calls the worker makes, apart so a host test stands in for
/// them.
pub trait HeldSystem {
    /// Hand every record of the mount table to `sink`, in the order the
    /// service lists them.
    ///
    /// # Errors
    ///
    /// Why the table was not read whole.
    fn mounts(&mut self, sink: &mut dyn FnMut(&MountRecord)) -> Result<(), Errno>;

    /// Carry `request` to the kernel.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    fn replay(&mut self, request: &VolumeReplayRequest) -> Result<(), Errno>;
}

/// The worker's work: `job`, carried out over `system`.
pub fn work<S: HeldSystem>(system: &mut S, job: &mut HeldJob) -> HeldDone {
    match *job {
        HeldJob::Read => HeldDone::Read(awaiting(system)),
        HeldJob::Answer(request) => HeldDone::Answered(request, system.replay(&request)),
    }
}

/// The volumes the mount table shows awaiting the seat's answer: a removable
/// device's, shown to this session and published under a key an answer can
/// name.
fn awaiting<S: HeldSystem>(system: &mut S) -> Result<Vec<Held>, Errno> {
    let mut held = Vec::new();
    let mut room = Ok(());
    system.mounts(&mut |record: &MountRecord| {
        if room.is_err()
            || record.availability() != MountAvailability::ReplayUnconfirmed
            || record.class() != VolumeClass::Removable
            || record.is_withheld()
            || record.volume_id() == VOLUME_ID_UNPUBLISHED
        {
            return;
        }
        let Some(volume) = Volume::from_mount(record) else {
            return;
        };
        room = held.try_reserve(1).map_err(|_| Errno::OutOfMemory);
        if room.is_ok() {
            held.push(Held {
                volume: volume.facts.volume_id,
                name: volume.label,
            });
        }
    })?;
    room.map(|()| held)
}

/// The question put to the user at the seat for the writes the volume shown
/// as `name` holds: its buttons give the answers [`answer_of`] reads.
#[must_use]
pub fn question(name: &str) -> Option<AlertSpec> {
    let [discard, write] = ANSWERS.map(|(label, role, _)| AlertButton::new(label, role).ok());
    AlertSpec::fitted(
        AlertSeverity::Question,
        &format!(
            "Write the changes waiting for \u{201c}{}\u{201d}?",
            shown(name)
        ),
        DETAIL,
        &[discard?, write?],
    )
    .ok()
}

/// The answer a question's button `index` gives.
#[must_use]
pub fn answer_of(index: u8) -> Option<ReplayAnswer> {
    ANSWERS
        .get(usize::from(index))
        .map(|&(_, _, answer)| answer)
}

/// What the user is told of an answer for the volume shown as `name` the
/// kernel did not carry out, or `None` where the question's own fate tells it:
/// it comes down for a volume no longer awaiting an answer, and is asked again
/// for one still awaiting it.
#[must_use]
pub fn refusal(name: &str, answer: ReplayAnswer, errno: Errno) -> Option<GestureRefusal> {
    let why = match errno {
        Errno::NotFound
        | Errno::Stale
        | Errno::Busy
        | Errno::OutOfMemory
        | Errno::LimitExceeded => return None,
        Errno::ReadOnly => String::from(
            "The disk no longer matches the one they were made on, or would not take them. It \
             stays read-only, and the changes are kept.",
        ),
        Errno::DeviceOffline => String::from(
            "The disk went away first. The changes are kept, and you are asked again when it \
             is put back.",
        ),
        Errno::PermissionDenied => String::from(
            "Only the user at this computer may answer, from the desktop showing the question.",
        ),
        errno => format!("The system refused it: {errno}."),
    };
    let undone = match answer {
        ReplayAnswer::Write => "written",
        ReplayAnswer::Discard => "discarded",
    };
    Some(GestureRefusal::new(
        format!(
            "The changes waiting for \u{201c}{}\u{201d} were not {undone}",
            shown(name)
        ),
        why,
    ))
}

/// Where the desktop stands with held writes: what the last read found
/// awaiting the seat's answer, the answers with the worker, and the read owed
/// or under way.
#[derive(Default)]
pub struct HeldQuestions {
    awaiting: Vec<Held>,
    /// Each answer with the worker, and whether the kernel has taken it: its
    /// volume is not asked about again until the read behind it lands.
    answering: Vec<(Held, bool)>,
    owed: bool,
    reading: bool,
}

impl HeldQuestions {
    /// The mount table moved, or has never been read: a read is owed.
    pub fn moved(&mut self) {
        self.owed = true;
    }

    /// Carry the answer button `index` gave for the volume published as
    /// `volume` to `submit`, owing a read behind it. Answers whether it is
    /// dealt with — carried, or dropped for a volume the last read did not
    /// find awaiting one — and `false` while the worker has no room, which
    /// leaves it for the next pass.
    pub fn carry(
        &mut self,
        (volume, index): ([u8; VOLUME_ID_LEN], u8),
        submit: &mut dyn FnMut(HeldJob) -> Result<(), HeldJob>,
    ) -> bool {
        let at = self.awaiting.iter().position(|held| held.volume == volume);
        let (Some(at), Some(answer)) = (at, answer_of(index)) else {
            return true;
        };
        if self.answering.try_reserve(1).is_err() {
            return false;
        }
        let request = VolumeReplayRequest {
            volume_id: volume,
            answer,
        };
        if submit(HeldJob::Answer(request)).is_err() {
            return false;
        }
        self.answering.push((self.awaiting.swap_remove(at), false));
        self.owed = true;
        true
    }

    /// Ask `submit` for the read owed, unless one is under way: the next is
    /// asked as that one lands.
    pub fn read(&mut self, submit: &mut dyn FnMut(HeldJob) -> Result<(), HeldJob>) {
        if self.owed && !self.reading && submit(HeldJob::Read).is_ok() {
            self.owed = false;
            self.reading = true;
        }
    }

    /// Adopt what the worker did, asking `submit` for a read owed meanwhile.
    /// A read brings the questions in `alerts` up to what it found: each
    /// volume awaiting an answer is asked about once, within
    /// [`HELD_QUESTIONS_MAX`] counting the answers with the worker, and each
    /// question no volume awaits comes down. The kernel's word on an answer is
    /// told where it did not do as asked.
    pub fn adopt(
        &mut self,
        done: HeldDone,
        submit: &mut dyn FnMut(HeldJob) -> Result<(), HeldJob>,
        (alerts, holds): (&mut Alerts, &mut Holds),
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> HeldWord {
        let word = match done {
            HeldDone::Read(Ok(awaiting)) => {
                self.reading = false;
                self.follow(awaiting, (alerts, holds), shell, compositor)
            }
            // Read again at the table's next move, never in a loop against a
            // service that keeps refusing.
            HeldDone::Read(Err(errno)) => {
                self.reading = false;
                HeldWord::Note(format!(
                    "cannot read which volumes await an answer ({errno})"
                ))
            }
            HeldDone::Answered(request, word) => self.taken(&request, word),
        };
        self.read(submit);
        word
    }

    /// The kernel took the answer `request`, giving `word`.
    fn taken(&mut self, request: &VolumeReplayRequest, word: Result<(), Errno>) -> HeldWord {
        let Some((held, taken)) = self
            .answering
            .iter_mut()
            .find(|(held, taken)| !*taken && held.volume == request.volume_id)
        else {
            return HeldWord::Nothing;
        };
        *taken = true;
        match word {
            Ok(()) => HeldWord::Nothing,
            Err(errno) => refusal(&held.name, request.answer, errno).map_or_else(
                || {
                    HeldWord::Note(format!(
                        "the answer for \u{201c}{}\u{201d} was not carried out ({errno})",
                        shown(&held.name)
                    ))
                },
                HeldWord::Refusal,
            ),
        }
    }

    /// Bring the questions up to `awaiting`, what a read found.
    fn follow(
        &mut self,
        awaiting: Vec<Held>,
        (alerts, holds): (&mut Alerts, &mut Holds),
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> HeldWord {
        // This read is behind every answer the kernel has taken, so it sees
        // what each did.
        self.answering.retain(|&(_, taken)| !taken);
        self.awaiting = awaiting;
        let (awaiting, answering) = (&self.awaiting, &self.answering);
        alerts.withdraw_held(
            |volume| awaiting.iter().any(|held| held.volume == *volume),
            holds,
            shell,
            compositor,
        );
        let mut unasked = 0usize;
        for held in awaiting {
            let answered = answering
                .iter()
                .any(|(answer, _)| answer.volume == held.volume);
            if answered || alerts.asks_held(&held.volume) {
                continue;
            }
            if alerts.held_questions() + answering.len() >= HELD_QUESTIONS_MAX {
                break;
            }
            let asked = question(&held.name)
                .is_some_and(|spec| alerts.ask_held(held.volume, &spec, shell, compositor));
            unasked += usize::from(!asked);
        }
        if unasked == 0 {
            HeldWord::Nothing
        } else {
            HeldWord::Note(format!(
                "{unasked} of the questions for held writes could not be put up"
            ))
        }
    }
}

#[cfg(test)]
#[path = "held_tests.rs"]
mod tests;
