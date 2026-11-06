//! The items of a paste, a removal or a move to Trash, each carried out in
//! turn by an operation of the shared engine, and what those finished did.
//!
//! Generic over the tree seam's [`Dir`], so what an item does at a question —
//! asks, or takes the answer its user gave for the rest of its kind — and what
//! it reports it left under its name are tested on the host against the
//! in-memory tree.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::{Errno, FileId};
use tairix_browse::Tally;
use tairix_fscopy::op::{Conflict, Decision, Named, OpError, Operation, Progress, Report, Root};
use tairix_fscopy::tree::{Dir, Roots};
use tairix_fscopy::Omissions;

use crate::operation::{item_refusal, merges, operation_refusal};
use crate::operations::Step;

/// Why an operation stopped at a state its own record says cannot arise:
/// going on would act on a guess.
const LOST_TRACK: &str =
    "the file manager lost track of its progress and stopped rather than guess";

/// The items of an operation, each carried out in turn by an operation of the
/// shared engine, and what those finished did.
pub struct Turns<D: Dir> {
    /// The next item to begin.
    next: usize,
    /// The item in hand: the source its refusal names, and its operation.
    in_hand: Option<(Vec<String>, Operation<D>)>,
    /// What the items finished did.
    finished: Tally,
    /// One copy buffer for every file, grown to at most a chunk by the
    /// thread doing the copy.
    buf: Vec<u8>,
    /// The extended attributes the finished items' copies could not keep.
    omissions: Omissions,
    /// The answers its user gave for the rest, each taken by every later
    /// question of its own kind.
    rest: Rest,
    /// What the item last finished left under its name.
    landed: Option<Root>,
}

impl<D: Dir> Default for Turns<D> {
    fn default() -> Self {
        Self {
            next: 0,
            in_hand: None,
            finished: Tally::default(),
            buf: Vec::new(),
            omissions: Omissions::default(),
            rest: Rest::default(),
            landed: None,
        }
    }
}

impl<D: Dir> Turns<D> {
    /// The next item to begin.
    #[must_use]
    pub const fn next(&self) -> usize {
        self.next
    }

    /// The next item is begun.
    pub fn pass(&mut self) {
        self.next += 1;
    }

    /// Take `op`, carrying out the item at `source`, in hand.
    pub fn take(&mut self, source: &[String], op: Operation<D>) {
        self.in_hand = Some((source.to_vec(), op));
    }

    /// Whether no item is in hand.
    #[must_use]
    pub const fn idle(&self) -> bool {
        self.in_hand.is_none()
    }

    /// An item was done with nothing to carry out: one a move leaves where it
    /// already is.
    pub fn count_done(&mut self) {
        self.finished.items = self.finished.items.saturating_add(1);
    }

    /// What the item last finished left under its name; none for one that
    /// stopped before it settled the name.
    #[must_use]
    pub const fn landed(&self) -> Option<Root> {
        self.landed
    }

    /// One unit of the item in hand, a refusal stated with `verb`: once it
    /// ends, what it did joins what the items before it did, and the next
    /// unit begins the next item. A name already taken is asked about, or
    /// answered as its user said for the rest of its kind. `None` with no item
    /// in hand.
    pub fn unit(&mut self, verb: &str) -> Option<Step> {
        let (source, op) = self.in_hand.as_mut()?;
        let source = source.as_slice();
        let refusal = match op.advance(1, &mut self.buf) {
            Progress::Yielded => return Some(Step::Working),
            Progress::Paused => {
                if let Some(conflict) = op.conflict() {
                    let Some(decision) = self.rest.of(conflict) else {
                        return Some(Step::Asks(conflict.clone()));
                    };
                    op.resolve(decision);
                    return Some(Step::Working);
                }
                wind(op, &mut self.buf);
                Some(item_refusal(verb, source, LOST_TRACK))
            }
            Progress::Done => op
                .report()
                .kept
                .first()
                .map(|kept| operation_refusal(verb, source, &OpError::Changed(kept.clone()))),
            Progress::Failed(error) => Some(operation_refusal(verb, source, &error)),
        };
        self.finished = with_done(self.finished, op.report());
        self.omissions.merge(op.omissions());
        self.landed = op.report().root;
        self.in_hand = None;
        Some(refusal.map_or(Step::Working, Step::Stopped))
    }

    /// Answer the question the item in hand waits on with `decision`, kept
    /// for every later question of its kind where `rest` says so.
    pub fn answer(&mut self, decision: Decision, rest: bool) {
        let Some((_, op)) = self.in_hand.as_mut() else {
            return;
        };
        if let Some(conflict) = op.conflict().filter(|_| rest) {
            self.rest.keep(conflict, decision);
        }
        op.resolve(decision);
    }

    /// What the items finished and the one in hand have done.
    #[must_use]
    pub fn done(&self) -> Tally {
        match &self.in_hand {
            Some((_, op)) => with_done(self.finished, op.report()),
            None => self.finished,
        }
    }

    /// Whether the item in hand is a move removing the originals it carried.
    #[must_use]
    pub fn removing(&self) -> bool {
        self.in_hand.as_ref().is_some_and(|(_, op)| op.removing())
    }

    /// The operation ends part-way: what the item in hand made is settled,
    /// and a file it was writing removed rather than left half-copied.
    pub fn abandon(&mut self) {
        if let Some((_, op)) = self.in_hand.as_mut() {
            wind(op, &mut self.buf);
        }
    }

    /// The extended attributes the copies could not keep, the item in
    /// hand's among them.
    #[must_use]
    pub fn omissions(&self) -> Omissions {
        let mut omissions = self.omissions.clone();
        if let Some((_, op)) = &self.in_hand {
            omissions.merge(op.omissions());
        }
        omissions
    }
}

/// The answers given for the rest, one for each kind of question a name
/// already taken asks: a folder met by a folder, which merges, and a file or a
/// link met by one, which replaces. A merge for the rest never answers a
/// file's question.
#[derive(Copy, Clone, Debug, Default)]
struct Rest {
    folders: Option<Decision>,
    files: Option<Decision>,
}

impl Rest {
    /// The answer a question about `conflict` takes, where one was given for
    /// the rest of its kind.
    const fn of(self, conflict: &Conflict) -> Option<Decision> {
        if merges(conflict) {
            self.folders
        } else {
            self.files
        }
    }

    /// Give `decision` for every later question of `conflict`'s kind.
    fn keep(&mut self, conflict: &Conflict, decision: Decision) {
        if merges(conflict) {
            self.folders = Some(decision);
        } else {
            self.files = Some(decision);
        }
    }
}

/// `tally` with what `report` says was done added.
fn with_done(tally: Tally, report: &Report) -> Tally {
    Tally {
        items: tally
            .items
            .saturating_add(u64::try_from(report.done).unwrap_or(u64::MAX)),
        bytes: tally.bytes.saturating_add(report.bytes),
    }
}

/// Cancel `op` and let it settle what it made: its folders given their
/// modes, a file part-written removed.
fn wind<D: Dir>(op: &mut Operation<D>, buf: &mut Vec<u8>) {
    op.resolve(Decision::Cancel);
    // Settling ends within the units it takes; a refusal on the way is moot
    // beside the stop already being stated.
    let _ = op.advance(usize::MAX, buf);
}

/// The name `path` ends in, in its folder opened through `roots` from the
/// rest of it, to hold the node `seen` ([`Named::seen`]).
///
/// # Errors
///
/// A path no name can be spelled from, or the open's refusal.
pub fn named<R: Roots>(roots: &R, path: &[String], seen: FileId) -> Result<Named<R::Dir>, Errno> {
    let spelled = tairix_browse::vfs::absolute_path(path)?;
    Ok(Named {
        seen,
        ..Named::parent(roots, &spelled)?
    })
}

/// Put the last component of `path`, the name a reader knows it by, into
/// `name`, reusing what it holds.
pub fn name_into(name: &mut String, path: &[String]) {
    name.clear();
    if let Some(leaf) = path.last() {
        name.push_str(leaf);
    }
}
