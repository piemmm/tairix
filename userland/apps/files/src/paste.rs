//! A paste as the file manager carries it out: each item of a captured plan
//! one operation of the shared engine, a unit at a time, a name already taken
//! asked about in the operation's own window — replaced, a folder merged
//! into, or skipped, or the paste stopped — and the drop onto the desktop it
//! lands stepped around each item with what became of it.
//!
//! Generic over the [`Roots`] its paths are opened through and the
//! [`LandingSink`] its drop is stepped through, so what it does at a name
//! already taken is tested on the host against the in-memory tree. Every step
//! is the user's own permission-checked call — no new capability, no ambient
//! authority — so the read-only picker never builds one.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::LandingOutcome;
use tairix_abi::FileId;
use tairix_browse::{ClipboardOp, PasteItem, Tally, VolumeId};
use tairix_fscopy::op::{Decision, Operation, Stager, Taken, Verb};
use tairix_fscopy::tree::{Node, Roots};
use tairix_fscopy::Omissions;

use crate::landing::{outcome_of, DropLanding, LandingSink};
use crate::operation::{destination_refusal, item_refusal, reach_refusal};
use crate::operations::Step;
use crate::turns::{name_into, named, Turns};

/// Where a paste's items land and how: what a count needs to tell an item a
/// move renames in place, which carries nothing, from one it copies.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Landing {
    /// Whether the items are moved or copied.
    pub op: ClipboardOp,
    /// The volume they land on.
    pub volume: VolumeId,
}

/// A paste in progress: each item of a captured plan carried out by an
/// operation of the shared engine, one bounded unit at a time, so the event
/// loop stays responsive.
pub struct Paste<R: Roots, S: LandingSink> {
    /// What its paths are opened through.
    roots: R,
    /// How a replacement is named beside what it replaces.
    stage: Stager,
    /// Whether the plan moves (`Cut`) or copies (`Copy`) its items.
    op: ClipboardOp,
    /// The paste target directory.
    dest: Vec<String>,
    /// The target directory's volume once the count has read it.
    dest_vol: Option<VolumeId>,
    /// The resolved plan items, carried out in order.
    items: Vec<PasteItem>,
    turns: Turns<R::Dir>,
    /// Where the item in hand is landing, spelled, until it is carried out.
    writing: Option<String>,
    /// The drop onto the desktop the paste carries out, stepped around each
    /// item; none for any other paste.
    steps: DropLanding<S>,
}

impl<R: Roots, S: LandingSink> Paste<R, S> {
    /// A paste of `items` (in order) into the folder `dest`, `op` saying
    /// whether it moves or copies them, nothing begun yet: its paths opened
    /// through `roots`, a replacement named by `stage`, and the drop `steps`
    /// lands stepped around each item.
    #[must_use]
    pub fn new(
        roots: R,
        stage: Stager,
        (op, dest, items): (ClipboardOp, Vec<String>, Vec<PasteItem>),
        steps: DropLanding<S>,
    ) -> Self {
        Self {
            roots,
            stage,
            op,
            dest,
            dest_vol: None,
            items,
            turns: Turns::default(),
            writing: None,
            steps,
        }
    }

    /// The folder it pastes into.
    #[must_use]
    pub fn dest(&self) -> &[String] {
        &self.dest
    }

    /// The items it carries out, in order.
    #[must_use]
    pub fn items(&self) -> &[PasteItem] {
        &self.items
    }

    /// Where the item in hand is landing, spelled, while it is written.
    #[must_use]
    pub fn writing(&self) -> Option<&str> {
        self.writing.as_deref()
    }

    /// What it has done so far.
    #[must_use]
    pub fn done(&self) -> Tally {
        self.turns.done()
    }

    /// Whether a move across volumes is removing the originals it carried.
    #[must_use]
    pub fn removing(&self) -> bool {
        self.turns.removing()
    }

    /// The extended attributes its copies could not keep.
    #[must_use]
    pub fn omissions(&self) -> Omissions {
        self.turns.omissions()
    }

    /// Answer the question it waits on with `decision`, kept for every later
    /// one of its kind where `rest` says so.
    pub fn answer(&mut self, decision: Decision, rest: bool) {
        self.turns.answer(decision, rest);
    }

    /// The paste ends part-way: the item in hand is settled — a file it was
    /// writing removed — and the drop ends with that item failed.
    pub fn abandon(&mut self) {
        self.turns.abandon();
        self.steps.made(LandingOutcome::Failed);
        self.steps.end();
    }

    /// Where the items land and how, the destination read on first ask.
    ///
    /// # Errors
    ///
    /// The step stopping the paste, its drop ended, where the destination
    /// cannot be read.
    pub fn landing(&mut self) -> Result<Landing, Step> {
        if let Some(volume) = self.dest_vol {
            return Ok(Landing {
                op: self.op,
                volume,
            });
        }
        let found = tairix_browse::vfs::absolute_path(&self.dest)
            .and_then(|path| self.roots.open_node(&path, true))
            .and_then(|node| node.stat());
        let Ok(stat) = found else {
            self.steps.end();
            return Err(Step::Stopped(item_refusal(
                "paste into",
                &self.dest,
                "it could not be read",
            )));
        };
        let volume = VolumeId::new(stat.id.volume);
        self.dest_vol = Some(volume);
        Ok(Landing {
            op: self.op,
            volume,
        })
    }

    /// One unit of the paste, its refusal stated naming the item, and the
    /// item in hand named in `current`.
    ///
    /// A `Cut` back into the item's own directory is already done; a `Copy`
    /// onto itself is refused rather than duplicating a file onto itself. The
    /// drop it lands is stepped around each item and ended with the paste.
    pub fn unit(&mut self, current: &mut String) -> Step {
        let step = self.advance(current);
        if matches!(step, Step::Done | Step::Stopped(_)) {
            self.steps.end();
        }
        step
    }

    /// One unit of the paste, as [`unit`](Self::unit) takes it.
    fn advance(&mut self, current: &mut String) -> Step {
        let verb = match self.op {
            ClipboardOp::Copy => "copy",
            ClipboardOp::Cut => "move",
        };
        if let Some(step) = self.turns.unit(verb) {
            if self.turns.idle() {
                self.writing = None;
                self.steps.made(if matches!(step, Step::Stopped(_)) {
                    LandingOutcome::Failed
                } else {
                    outcome_of(self.turns.landed())
                });
            }
            return step;
        }
        let index = self.turns.next();
        let Some(item) = self.items.get(index) else {
            return Step::Done;
        };
        self.turns.pass();
        name_into(current, item.source());
        if item.overwrites_source() {
            return match self.op {
                ClipboardOp::Cut => {
                    // Done, as the count of the whole counted it.
                    self.turns.count_done();
                    Step::Working
                }
                ClipboardOp::Copy => Step::Stopped(item_refusal(
                    verb,
                    item.source(),
                    "an item cannot be copied onto itself",
                )),
            };
        }
        let source = match named(&self.roots, item.source(), item.source_id()) {
            Ok(source) => source,
            Err(errno) => {
                return Step::Stopped(item_refusal(verb, item.source(), reach_refusal(errno)))
            }
        };
        let target = match named(&self.roots, item.dest(), FileId::NONE) {
            Ok(target) => target,
            Err(errno) => {
                return Step::Stopped(item_refusal(
                    verb,
                    item.source(),
                    destination_refusal(errno),
                ))
            }
        };
        let carried = match self.op {
            ClipboardOp::Copy => Verb::Copy,
            ClipboardOp::Cut => Verb::Move,
        };
        self.writing = Some(target.path.clone());
        if let (Ok(number), Some(name)) = (u32::try_from(index), item.dest().last()) {
            self.steps.making(number, name);
        }
        self.turns.take(
            item.source(),
            Operation::transfer(carried, source, target, Taken::Ask, self.stage),
        );
        Step::Working
    }
}

#[cfg(test)]
#[path = "paste_tests.rs"]
mod tests;
