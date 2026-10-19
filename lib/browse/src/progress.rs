//! The progress + cancel model for a long file operation
//! (`plans/NEW-FILEMANAGER.md` `FM7b`).
//!
//! A recursive delete ([`DeleteWalk`](crate::DeleteWalk)) or copy
//! ([`CopyWalk`](crate::CopyWalk)) can touch many nodes, so the file manager
//! carries it out on a worker, showing its progress in a window of its own and
//! honouring a cancel between units. This module is that window's state: what
//! the operation is and where it goes, what is done of what whole, the item in
//! hand, and how fast it is going.
//!
//! The whole is *counted*, never estimated: before it begins, an operation
//! that cannot know its size up front totals its items and bytes in a
//! preparing pass ([`Survey`](crate::survey::Survey)). Until that lands, and
//! for good where part of the tree could not be counted, the model has no
//! fraction to show and says what it has done. Cancel is *latched*, so a
//! second press cannot un-cancel a run already stopping.
//!
//! The model holds no authority and does no I/O — it is the display face of a
//! walk the app drives — so composing it grants nothing and the read-only
//! trusted picker (which never deletes or copies) never builds one.

use alloc::format;
use alloc::string::String;
use core::fmt::Write;

use tairix_util::utf8::shown;

use crate::format::format_size;

/// Which long file operation a [`ProgressModel`] reports.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProgressOp {
    /// A recursive removal (driven by [`DeleteWalk`](crate::DeleteWalk)).
    Delete,
    /// A recursive copy (driven by [`CopyWalk`](crate::CopyWalk)).
    Copy,
    /// A move: a rename on one volume, or a copy then a removal of the source
    /// across two.
    Move,
    /// A move to Trash: a recoverable delete carried out as same-volume
    /// renames into the user's Trash directory (`plans/NEW-FILEMANAGER.md`
    /// `FM10`), one item per step.
    Trash,
}

/// What an operation acts on, as its headline names it: the one item by its
/// name, or how many.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Subject {
    /// One item, by the name the user sees.
    One(String),
    /// This many items.
    Many(usize),
}

/// Items and bytes: what an operation has done, or the whole it will do.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Tally {
    /// Files, folders and links.
    pub items: u64,
    /// The bytes of the files among them.
    pub bytes: u64,
}

/// What the walk driving an operation says of itself at one reading.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Reading {
    /// What is done; while preparing, what has been counted.
    pub done: Tally,
    /// The whole, once counted.
    pub total: Option<Tally>,
    /// Whether it is still counting what it will do.
    pub preparing: bool,
    /// Whether a move across volumes is removing originals it has copied:
    /// work the bar, which measures what is carried, does not count.
    pub removing: bool,
}

/// How long a reading must hold before a rate is stated: long enough that the
/// first files, and the folders made before them, do not set it.
const PACE_SETTLE_NS: u64 = 2_000_000_000;

/// How far back the smoothed rate effectively looks, so one slow file or one
/// burst of small ones moves the time left only as far as it should.
const PACE_SPAN_NS: u64 = 3_000_000_000;

/// How fast an operation goes, smoothed, in the units its fraction is taken
/// in.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Pace {
    /// When the first working reading came, and the last, with its count.
    started_ns: Option<u64>,
    last: Option<(u64, u64)>,
    /// Units a second, smoothed.
    per_second: u64,
}

impl Pace {
    /// Leave out the time until the next reading: what passes before it is
    /// not time spent carrying anything.
    fn pause(&mut self) {
        self.last = None;
    }

    /// Fold in a reading of `units` done at `now_ns`.
    fn observe(&mut self, now_ns: u64, units: u64) {
        let started = *self.started_ns.get_or_insert(now_ns);
        let Some((then, before)) = self.last.replace((now_ns, units)) else {
            return;
        };
        let elapsed = now_ns.saturating_sub(then);
        if elapsed == 0 || units < before {
            return;
        }
        let instant = u128::from(units - before) * 1_000_000_000 / u128::from(elapsed);
        let instant = u64::try_from(instant).unwrap_or(u64::MAX);
        if now_ns.saturating_sub(started) <= elapsed {
            self.per_second = instant;
            return;
        }
        // An exponential average over `PACE_SPAN_NS`, weighted by how long
        // this reading covers.
        let weight = u128::from(elapsed);
        let span = u128::from(PACE_SPAN_NS) + weight;
        let blended =
            (u128::from(self.per_second) * (span - weight) + u128::from(instant) * weight) / span;
        self.per_second = u64::try_from(blended).unwrap_or(u64::MAX);
    }

    /// The smoothed rate, once readings have held for long enough to state.
    fn settled(&self) -> Option<u64> {
        let (started, (last, _)) = (self.started_ns?, self.last?);
        (last.saturating_sub(started) >= PACE_SETTLE_NS && self.per_second > 0)
            .then_some(self.per_second)
    }
}

/// The running display + cancel state of a long file operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgressModel {
    op: ProgressOp,
    subject: Subject,
    /// The folder a copy or a move lands in, by its name.
    destination: Option<String>,
    /// What the walk last said of itself: its whole is `None` while
    /// preparing, or where part of it could not be counted.
    reading: Reading,
    /// The item in hand, by its name.
    current: String,
    pace: Pace,
    cancel_requested: bool,
    waiting: bool,
}

impl ProgressModel {
    /// A fresh progress model for `op` on `subject`, landing in `destination`
    /// where it goes anywhere: nothing done yet, no cancel requested.
    #[must_use]
    pub fn new(op: ProgressOp, subject: Subject, destination: Option<String>) -> Self {
        Self {
            op,
            subject,
            destination,
            reading: Reading::default(),
            current: String::new(),
            pace: Pace::default(),
            cancel_requested: false,
            waiting: false,
        }
    }

    /// Whether it waits for the operations asked before it, which run first.
    #[must_use]
    pub const fn is_waiting(&self) -> bool {
        self.waiting
    }

    /// Note whether it is still waiting for the operations before it.
    pub fn set_waiting(&mut self, waiting: bool) {
        self.waiting = waiting;
    }

    /// Which operation this reports.
    #[must_use]
    pub const fn op(&self) -> ProgressOp {
        self.op
    }

    /// What the driving walk last said of itself.
    #[must_use]
    pub const fn reading(&self) -> Reading {
        self.reading
    }

    /// The item in hand, by its name; empty before the first.
    #[must_use]
    pub fn current(&self) -> &str {
        &self.current
    }

    /// Take the driving walk's `reading` at `now_ns`, and the item in hand.
    /// The caller passes the walk's own figures, so the window can never claim
    /// more than actually happened.
    pub fn observe(&mut self, reading: Reading, current: &str, now_ns: u64) {
        self.reading = reading;
        if self.current != current {
            self.current.clear();
            self.current.push_str(current);
        }
        // Only carrying against a counted whole sets the pace: time spent
        // waiting its turn, counting or removing originals would read as a
        // crawl once it carried on.
        if reading.preparing || reading.removing || self.waiting || reading.total.is_none() {
            self.pace.pause();
        } else {
            self.pace.observe(now_ns, self.units(reading.done));
        }
    }

    /// Whether it measures its whole in bytes: a copy that has any to carry.
    /// A removal, a rename and a tree of empty files measure in items.
    fn by_bytes(&self) -> bool {
        self.reading.total.is_some_and(|total| total.bytes > 0)
            && matches!(self.op, ProgressOp::Copy | ProgressOp::Move)
    }

    /// `tally` in the units the fraction is taken in.
    fn units(&self, tally: Tally) -> u64 {
        if self.by_bytes() {
            tally.bytes
        } else {
            tally.items
        }
    }

    /// The share of the whole done, in permille, once the whole is counted.
    #[must_use]
    pub fn fraction(&self) -> Option<u16> {
        if self.reading.preparing || self.waiting {
            return None;
        }
        let total = self.units(self.reading.total?);
        if total == 0 {
            return Some(1000);
        }
        let done = self.units(self.reading.done).min(total);
        u16::try_from(u128::from(done) * 1000 / u128::from(total)).ok()
    }

    /// Latch a user cancel request. The app stops the walk at the next
    /// step/chunk boundary (never mid-operation on a single node), and the
    /// latch means a second press cannot revert a run already stopping.
    pub fn request_cancel(&mut self) {
        self.cancel_requested = true;
    }

    /// Whether the user has asked to cancel this run.
    #[must_use]
    pub const fn is_cancel_requested(&self) -> bool {
        self.cancel_requested
    }

    /// The window title: the operation in progress, or "Cancelling…" once a
    /// cancel has been latched (so the user sees the request took effect while
    /// the current step finishes).
    #[must_use]
    pub const fn title(&self) -> &'static str {
        if self.cancel_requested {
            "Cancelling\u{2026}"
        } else {
            match self.op {
                ProgressOp::Delete => "Deleting\u{2026}",
                ProgressOp::Copy => "Copying\u{2026}",
                ProgressOp::Move => "Moving\u{2026}",
                ProgressOp::Trash => "Moving to Trash\u{2026}",
            }
        }
    }

    /// The title of the notice a refusal that stopped this operation is
    /// stated in.
    #[must_use]
    pub const fn stopped_title(&self) -> &'static str {
        match self.op {
            ProgressOp::Delete => "Delete stopped",
            ProgressOp::Copy => "Copy stopped",
            ProgressOp::Move => "Move stopped",
            ProgressOp::Trash => "Move to Trash stopped",
        }
    }

    /// What the operation is doing, to what, and where: "Copying “Notes” to
    /// “Documents”", "Deleting 12 items". A cancel is stated beneath it.
    #[must_use]
    pub fn headline(&self) -> String {
        let verb = match self.op {
            ProgressOp::Delete => "Deleting",
            ProgressOp::Copy => "Copying",
            ProgressOp::Move | ProgressOp::Trash => "Moving",
        };
        let what = match &self.subject {
            Subject::One(name) => format!("\u{201c}{}\u{201d}", shown(name)),
            Subject::Many(1) => String::from("1 item"),
            Subject::Many(count) => format!("{count} items"),
        };
        match (self.op, &self.destination) {
            (ProgressOp::Trash, _) => format!("{verb} {what} to Trash"),
            (ProgressOp::Copy | ProgressOp::Move, Some(to)) => {
                format!("{verb} {what} to \u{201c}{}\u{201d}", shown(to))
            }
            _ => format!("{verb} {what}"),
        }
    }

    /// What the window states beneath the bar: that it is cancelling, that it
    /// waits, what it has found while preparing, or what is done of the whole
    /// with the rate and the time left once those have settled.
    #[must_use]
    pub fn figures(&self) -> String {
        if self.cancel_requested {
            return String::from("Cancelling\u{2026}");
        }
        if self.waiting {
            return String::from("Waiting for the operations before it");
        }
        if self.reading.removing {
            return String::from("Removing the originals\u{2026}");
        }
        if self.reading.preparing {
            return format!("Preparing\u{2026} {}", self.amount(self.reading.done));
        }
        let Some(total) = self.reading.total else {
            return self.amount(self.reading.done);
        };
        let mut line = if self.by_bytes() {
            format!(
                "{} of {}",
                format_size(self.reading.done.bytes.min(total.bytes)),
                format_size(total.bytes)
            )
        } else {
            format!(
                "{} of {}",
                self.reading.done.items.min(total.items),
                items(total.items)
            )
        };
        if let Some(rate) = self.pace.settled() {
            let remaining = self
                .units(total)
                .saturating_sub(self.units(self.reading.done));
            if self.by_bytes() {
                let _ = write!(line, " \u{00b7} {}/s", format_size(rate));
            }
            let _ = write!(line, " \u{00b7} {}", time_left(remaining / rate.max(1)));
        }
        line
    }

    /// `tally` as an amount: its items, and its bytes where there are any.
    fn amount(&self, tally: Tally) -> String {
        if tally.bytes > 0 && matches!(self.op, ProgressOp::Copy | ProgressOp::Move) {
            format!("{}, {}", items(tally.items), format_size(tally.bytes))
        } else {
            items(tally.items)
        }
    }
}

/// `count` items, in words.
fn items(count: u64) -> String {
    if count == 1 {
        String::from("1 item")
    } else {
        format!("{count} items")
    }
}

/// `seconds` left, as a reader would say it: to the nearest five seconds
/// under a minute, the nearest minute under an hour, the nearest hour after.
fn time_left(seconds: u64) -> String {
    let (count, unit) = if seconds < 3 {
        return String::from("almost done");
    } else if seconds < 58 {
        ((seconds + 2) / 5 * 5, "second")
    } else if seconds < 3_570 {
        ((seconds + 30) / 60, "minute")
    } else {
        (seconds.saturating_add(1_800) / 3_600, "hour")
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("about {count} {unit}{plural} left")
}
