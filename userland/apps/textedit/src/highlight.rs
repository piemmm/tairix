//! A window's colouring: lexer states kept at sparse checkpoints up to a
//! valid frontier, and the spans of the lines around the view, kept current
//! by bounded batches the sandbox lexes.
//!
//! The editor never lexes. It asks [`Highlight::next_job`] for the next batch
//! worth sending, sends it, and hands the answer to [`Highlight::adopt`]; an
//! edit is reported through [`Highlight::edited`], which moves the frontier
//! back to the edited line and shifts the spans it keeps so a line keeps its
//! colours until the lexer has spoken again.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::ops::{ControlFlow, Range};

use tairix_sandbox::textsyntax::{LexedBatch, MAX_LEX_BATCH_BYTES, MAX_LEX_BATCH_LINES};
use tairix_syntax::{Format, LineState, Span, MAX_LEX_LINE};

use crate::document::{Document, OutOfMemory};

/// Lines between two kept lexer states.
const CHECKPOINT_LINES: usize = 64;

/// How far past the frontier a view may be before it is coloured
/// provisionally, from a fresh start, instead of waiting for the frontier.
const FAR_LINES: usize = 2048;

/// Lines above a far view a provisional colouring starts at, so a construct
/// opened just above it is usually seen.
const PROVISIONAL_LEAD: usize = 64;

/// Lines kept either side of the view, so a short scroll finds its colours.
const MARGIN_LINES: usize = 64;

/// Failed batches in a row after which a window stops asking.
const MAX_FAILURES: u32 = 3;

/// Why a window's colouring stopped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Stopped {
    /// The lexer failed on this document repeatedly.
    Failed,
    /// The allocator refused the colouring room.
    OutOfMemory,
}

/// What an edit did to the lines: where it started, how many line feeds it
/// took out and put in, and where the text after it moved.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LineEdit {
    /// The line the edit starts on.
    pub line: usize,
    /// Where on that line, in bytes.
    pub column: usize,
    /// Line feeds taken out.
    pub removed_lines: usize,
    /// Line feeds put in.
    pub inserted_lines: usize,
    /// Where the text after the edit started, on line
    /// `line + removed_lines` before it.
    pub old_tail: usize,
    /// Where that text starts, on line `line + inserted_lines` after it.
    pub new_tail: usize,
}

/// The text an edit is about to take out, measured before it goes: the
/// first half of a [`LineEdit`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Removal {
    at: usize,
    line: usize,
    column: usize,
    removed_lines: usize,
    old_tail: usize,
}

impl LineEdit {
    /// Measure `range` of `document`, which an edit is about to replace.
    #[must_use]
    pub fn removing(document: &Document, range: Range<usize>) -> Removal {
        let line = document.line_of(range.start);
        let last = document.line_of(range.end);
        Removal {
            at: range.start,
            line,
            column: range.start - document.line_start(line),
            removed_lines: last - line,
            old_tail: range.end - document.line_start(last),
        }
    }
}

impl Removal {
    /// The whole edit, once `document` holds the `inserted` bytes put where
    /// the removal was.
    #[must_use]
    pub fn inserted(self, document: &Document, inserted: usize) -> LineEdit {
        let end = self.at + inserted;
        let last = document.line_of(end);
        LineEdit {
            line: self.line,
            column: self.column,
            removed_lines: self.removed_lines,
            inserted_lines: last - self.line,
            old_tail: self.old_tail,
            new_tail: end - document.line_start(last),
        }
    }
}

/// One batch for the lexer: consecutive lines, the first starting in
/// `state`.
#[derive(Debug)]
pub struct LexJob {
    /// Which batch this is, echoed with its answer.
    pub id: u64,
    /// The format to lex as.
    pub format: Format,
    /// The state the first line starts in.
    pub state: LineState,
    text: Vec<u8>,
    ends: Vec<usize>,
}

impl LexJob {
    /// The batch's lines, each cut to what a lexer reads.
    pub fn lines(&self) -> impl Iterator<Item = &[u8]> {
        let starts = core::iter::once(0).chain(self.ends.iter().copied());
        starts
            .zip(self.ends.iter().copied())
            .map(|(start, end)| &self.text[start..end])
    }

    /// How many lines the batch holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// Whether the batch holds no line.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }
}

/// Where a batch started, which decides what its answer may update.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Start {
    /// At the frontier: its states carry the frontier on.
    Frontier,
    /// At a checkpoint behind the frontier, refilling lines the view lost.
    Checkpoint,
    /// From a fresh start near a view far past the frontier: its colours
    /// are provisional and its states are not kept.
    Provisional,
}

/// Where a refill that is not the frontier's stopped short of the view, and
/// the state its last line left: the next refill of the same kind carries on
/// from there, since a batch of long lines may not reach the view at all
/// from its checkpoint.
#[derive(Copy, Clone, Debug)]
struct Resume {
    line: usize,
    state: LineState,
    start: Start,
}

/// The batch in flight.
#[derive(Copy, Clone, Debug)]
struct Pending {
    id: u64,
    first: usize,
    start: Start,
    /// The first line an edit has touched since the batch was built: its
    /// answer holds for the lines before it and no further.
    dirty_from: usize,
}

/// One window's colouring.
#[derive(Debug)]
pub struct Highlight {
    format: Format,
    /// Checkpoint `i` is the state line `i * CHECKPOINT_LINES` starts in,
    /// for every such line up to the frontier.
    checkpoints: Vec<LineState>,
    /// Every line before this one has been lexed since it last changed.
    frontier: usize,
    /// The state the frontier line starts in.
    frontier_state: LineState,
    /// The line the kept spans start at.
    first: usize,
    kept: VecDeque<Option<Vec<Span>>>,
    pending: Option<Pending>,
    resume: Option<Resume>,
    next_id: u64,
    failures: u32,
    stopped: Option<Stopped>,
}

impl Highlight {
    /// A colouring of a document as `format`, with nothing lexed yet.
    #[must_use]
    pub fn new(format: Format) -> Self {
        Self {
            format,
            checkpoints: alloc::vec![LineState::START],
            frontier: 0,
            frontier_state: LineState::START,
            first: 0,
            kept: VecDeque::new(),
            pending: None,
            resume: None,
            next_id: 1,
            failures: 0,
            stopped: None,
        }
    }

    /// The format the document is coloured as.
    #[must_use]
    pub const fn format(&self) -> Format {
        self.format
    }

    /// Colour the document as `format` from scratch; a batch in flight is
    /// answered into nothing.
    pub fn set_format(&mut self, format: Format) {
        let next_id = self.next_id;
        *self = Self::new(format);
        self.next_id = next_id;
    }

    /// Why colouring stopped, if it has.
    #[must_use]
    pub const fn stopped(&self) -> Option<Stopped> {
        self.stopped
    }

    /// The spans kept for `line`, if any.
    #[must_use]
    pub fn spans(&self, line: usize) -> Option<&[Span]> {
        let index = line.checked_sub(self.first)?;
        self.kept.get(index)?.as_deref()
    }

    /// Report an edit: the frontier falls back to the edited line, and the
    /// kept spans move with the lines they belong to.
    pub fn edited(&mut self, edit: LineEdit) {
        if edit.line < self.frontier {
            let kept = edit.line / CHECKPOINT_LINES;
            self.checkpoints.truncate(kept + 1);
            self.frontier = kept * CHECKPOINT_LINES;
            self.frontier_state = self
                .checkpoints
                .get(kept)
                .copied()
                .unwrap_or(LineState::START);
        }
        if let Some(pending) = &mut self.pending {
            pending.dirty_from = pending.dirty_from.min(edit.line);
        }
        if self.resume.is_some_and(|resume| edit.line < resume.line) {
            self.resume = None;
        }
        self.shift(edit);
    }

    /// Move the kept spans as `edit` moved their lines.
    fn shift(&mut self, edit: LineEdit) {
        let end = self.first + self.kept.len();
        let removed_last = edit.line + edit.removed_lines;
        if edit.line >= end {
            return;
        }
        if removed_last < self.first {
            self.first = self.first - edit.removed_lines + edit.inserted_lines;
            return;
        }
        if edit.line < self.first {
            // The removal reaches into the kept lines from above them.
            let gone = (removed_last + 1 - self.first).min(self.kept.len());
            self.kept.drain(..gone);
            self.first = edit.line + 1 + edit.inserted_lines;
            return;
        }
        let index = edit.line - self.first;
        if edit.removed_lines == 0 && edit.inserted_lines == 0 {
            if let Some(Some(spans)) = self.kept.get_mut(index) {
                stretch(
                    spans,
                    edit.column,
                    edit.old_tail - edit.column,
                    edit.new_tail - edit.column,
                );
            }
            return;
        }
        let tail = self
            .kept
            .get(removed_last - self.first)
            .and_then(Option::as_deref)
            .and_then(|spans| moved_tail(spans, edit.old_tail, edit.new_tail));
        let after = index + 1;
        let gone = edit.removed_lines.min(self.kept.len() - after);
        self.kept.drain(after..after + gone);
        let Some(head) = self.kept.get_mut(index) else {
            return;
        };
        if let Some(spans) = head {
            clip(spans, edit.column);
        }
        if edit.inserted_lines == 0 {
            // A join: the tail follows the head on the one line.
            let joined = match (head.as_mut(), tail) {
                (Some(spans), Some(tail)) => spans
                    .try_reserve(tail.len())
                    .map(|()| spans.extend(tail))
                    .is_ok(),
                _ => false,
            };
            if !joined {
                *head = None;
            }
            return;
        }
        let fresh = edit.inserted_lines;
        if fresh >= self.kept.len() - after {
            // Every later kept line moves past the kept span.
            self.kept.truncate(after);
            return;
        }
        self.kept.extend(core::iter::repeat_n(None, fresh));
        let lines = self.kept.make_contiguous();
        lines[after..].rotate_right(fresh);
        lines[after + fresh - 1] = tail;
    }

    /// The next batch worth lexing to colour the lines `view`, or `None`
    /// when a batch is in flight or the view is coloured as well as it can
    /// be.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the batch cannot be held; colouring then stops.
    pub fn next_job(
        &mut self,
        document: &Document,
        view: Range<usize>,
    ) -> Result<Option<LexJob>, OutOfMemory> {
        if self.pending.is_some() || self.stopped.is_some() || self.format == Format::PlainText {
            return Ok(None);
        }
        let lines = document.line_count();
        let view = view.start.min(lines)..view.end.clamp(view.start.min(lines), lines);
        self.fit(view.start.saturating_sub(MARGIN_LINES)..(view.end + MARGIN_LINES).min(lines));
        let missing = view.clone().find(|&line| self.spans(line).is_none());
        let (first, state, start) = if self.frontier < view.end {
            match missing {
                Some(line) if line >= self.frontier + FAR_LINES => self.resumed(
                    line.saturating_sub(PROVISIONAL_LEAD),
                    LineState::START,
                    Start::Provisional,
                    line,
                ),
                _ => (self.frontier, self.frontier_state, Start::Frontier),
            }
        } else if let Some(line) = missing {
            let checkpoint = line / CHECKPOINT_LINES;
            let state = self
                .checkpoints
                .get(checkpoint)
                .copied()
                .unwrap_or(LineState::START);
            self.resumed(
                checkpoint * CHECKPOINT_LINES,
                state,
                Start::Checkpoint,
                line,
            )
        } else {
            return Ok(None);
        };
        let job = match self.build(document, first..view.end.max(first + 1), state) {
            Ok(job) => job,
            Err(refused) => {
                self.stop(Stopped::OutOfMemory);
                return Err(refused);
            }
        };
        self.pending = Some(Pending {
            id: job.id,
            first,
            start,
            dirty_from: usize::MAX,
        });
        Ok(Some(job))
    }

    /// Where a `start` refill for the uncoloured line `missing` begins: at
    /// `first` in `state`, or where the last such refill stopped, if that
    /// lies between the two.
    fn resumed(
        &self,
        first: usize,
        state: LineState,
        start: Start,
        missing: usize,
    ) -> (usize, LineState, Start) {
        match self.resume {
            Some(resume) if resume.start == start && (first..=missing).contains(&resume.line) => {
                (resume.line, resume.state, start)
            }
            _ => (first, state, start),
        }
    }

    /// Take in the answer to batch `id`: the lines before any edited since
    /// it was built are coloured, and a frontier batch carries the frontier
    /// on. Answers the lines it coloured.
    pub fn adopt(&mut self, id: u64, batch: &LexedBatch) -> Range<usize> {
        let Some(pending) = self.pending.filter(|pending| pending.id == id) else {
            return 0..0;
        };
        self.pending = None;
        self.failures = 0;
        let valid = batch
            .lines
            .len()
            .min(pending.dirty_from.saturating_sub(pending.first));
        for index in 0..valid {
            let line = pending.first + index;
            if pending.start == Start::Frontier && line == self.frontier {
                let state = batch.lines[index].1;
                let next = line + 1;
                if next % CHECKPOINT_LINES == 0 {
                    if self.checkpoints.try_reserve(1).is_err() {
                        self.stop(Stopped::OutOfMemory);
                        return pending.first..line;
                    }
                    self.checkpoints.push(state);
                }
                self.frontier = next;
                self.frontier_state = state;
            }
            self.keep(line, batch.line(index));
        }
        self.resume = match (pending.start, batch.lines.get(valid.wrapping_sub(1))) {
            (Start::Checkpoint | Start::Provisional, Some(&(_, state)))
                if valid == batch.lines.len() =>
            {
                Some(Resume {
                    line: pending.first + valid,
                    state,
                    start: pending.start,
                })
            }
            _ => None,
        };
        pending.first..pending.first + valid
    }

    /// Batch `id` failed; after too many in a row the window stops asking
    /// and draws plain.
    pub fn failed(&mut self, id: u64) {
        if self.pending.is_some_and(|pending| pending.id == id) {
            self.pending = None;
            self.failures += 1;
            if self.failures >= MAX_FAILURES {
                self.stop(Stopped::Failed);
            }
        }
    }

    fn stop(&mut self, why: Stopped) {
        self.stopped = Some(why);
        self.pending = None;
        self.kept.clear();
    }

    /// Keep spans for exactly `lines`, dropping the rest.
    fn fit(&mut self, lines: Range<usize>) {
        let end = self.first + self.kept.len();
        if lines.start >= end || lines.end <= self.first {
            self.kept.clear();
            self.first = lines.start;
        }
        while self.first < lines.start && !self.kept.is_empty() {
            self.kept.pop_front();
            self.first += 1;
        }
        if self.kept.is_empty() {
            self.first = lines.start;
        }
        while self.first > lines.start {
            self.kept.push_front(None);
            self.first -= 1;
        }
        self.kept.resize(lines.end - lines.start, None);
    }

    fn keep(&mut self, line: usize, spans: &[Span]) {
        let Some(slot) = line
            .checked_sub(self.first)
            .and_then(|index| self.kept.get_mut(index))
        else {
            return;
        };
        let mut owned = Vec::new();
        *slot = owned.try_reserve_exact(spans.len()).is_ok().then(|| {
            owned.extend_from_slice(spans);
            owned
        });
    }

    /// Copy the lines `lines` into a batch, each cut to what a lexer reads,
    /// stopping at the batch bounds.
    fn build(
        &mut self,
        document: &Document,
        lines: Range<usize>,
        state: LineState,
    ) -> Result<LexJob, OutOfMemory> {
        let id = self.next_id;
        self.next_id += 1;
        let mut batch = Batch {
            text: Vec::new(),
            ends: Vec::new(),
            line_len: 0,
            wanted: (lines.end - lines.start).min(MAX_LEX_BATCH_LINES),
            stop: None,
        };
        let mut line = lines.start;
        loop {
            // One walk carries on across short lines; a line longer than a
            // lexer reads ends it, and the next line is found by its index
            // rather than by reading the rest of that one.
            batch.stop = None;
            document.walk(document.line_start(line), |slice| batch.feed(slice));
            match batch.stop {
                Some(Stop::Refused) => return Err(OutOfMemory),
                Some(Stop::Full) => break,
                Some(Stop::Long) => {
                    batch.close_line(false)?;
                    line = lines.start + batch.ends.len();
                    if batch.full() || line >= document.line_count() {
                        break;
                    }
                }
                None => {
                    batch.close_line(false)?;
                    break;
                }
            }
        }
        Ok(LexJob {
            id,
            format: self.format,
            state,
            text: batch.text,
            ends: batch.ends,
        })
    }
}

/// Why copying a batch stopped before the document's end.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stop {
    /// The batch holds all it may.
    Full,
    /// The open line runs past what a lexer reads.
    Long,
    /// The allocator refused room.
    Refused,
}

/// A batch being copied out of the document.
struct Batch {
    text: Vec<u8>,
    ends: Vec<usize>,
    /// Bytes of the open line copied so far.
    line_len: usize,
    wanted: usize,
    stop: Option<Stop>,
}

impl Batch {
    /// Whether no further line may join the batch.
    fn full(&self) -> bool {
        self.ends.len() >= self.wanted || self.text.len() + MAX_LEX_LINE > MAX_LEX_BATCH_BYTES
    }

    /// Take the next slice of the document.
    fn feed(&mut self, mut slice: &[u8]) -> ControlFlow<()> {
        loop {
            let feed = slice.iter().position(|&b| b == b'\n');
            let content = &slice[..feed.unwrap_or(slice.len())];
            let room = MAX_LEX_LINE - self.line_len;
            if self.text.try_reserve(content.len().min(room)).is_err() {
                self.stop = Some(Stop::Refused);
                return ControlFlow::Break(());
            }
            self.text
                .extend_from_slice(&content[..content.len().min(room)]);
            self.line_len += content.len().min(room);
            if content.len() > room {
                self.stop = Some(Stop::Long);
                return ControlFlow::Break(());
            }
            let Some(at) = feed else {
                return ControlFlow::Continue(());
            };
            if self.close_line(true).is_err() {
                return ControlFlow::Break(());
            }
            if self.full() {
                self.stop = Some(Stop::Full);
                return ControlFlow::Break(());
            }
            slice = &slice[at + 1..];
        }
    }

    /// End the open line; one a line feed ends drops the CR of its CRLF.
    fn close_line(&mut self, at_feed: bool) -> Result<(), OutOfMemory> {
        if at_feed && self.line_len > 0 && self.text.last() == Some(&b'\r') {
            self.text.pop();
        }
        if self.ends.try_reserve(1).is_err() {
            self.stop = Some(Stop::Refused);
            return Err(OutOfMemory);
        }
        self.ends.push(self.text.len());
        self.line_len = 0;
        Ok(())
    }
}

/// Move `spans` as replacing `removed` bytes at `column` with `inserted`
/// moved their text: a span the edit falls inside stretches over what was
/// typed; one the removal cuts is clipped.
fn stretch(spans: &mut Vec<Span>, column: usize, removed: usize, inserted: usize) {
    let removed_end = column + removed;
    let moved = |at: usize| (at - removed).checked_add(inserted);
    spans.retain_mut(|span| {
        let (start, end) = (span.start as usize, span.end as usize);
        let start = if start <= column {
            Some(start)
        } else if start >= removed_end {
            moved(start)
        } else {
            column.checked_add(inserted)
        };
        let end = if end <= column {
            Some(end)
        } else if end >= removed_end {
            moved(end)
        } else {
            Some(column)
        };
        set(span, start, end)
    });
}

/// Keep only what of `spans` lies before `column`.
fn clip(spans: &mut Vec<Span>, column: usize) {
    spans.retain_mut(|span| {
        let end = (span.end as usize).min(column);
        set(span, Some(span.start as usize), Some(end))
    });
}

/// What of `spans` lies at or after `from`, moved to start at `to`; `None`
/// when the allocator refuses the room.
fn moved_tail(spans: &[Span], from: usize, to: usize) -> Option<Vec<Span>> {
    let mut out = Vec::new();
    out.try_reserve_exact(spans.len()).ok()?;
    for &span in spans {
        let start = (span.start as usize).max(from) - from;
        let end = (span.end as usize).saturating_sub(from);
        let mut moved = span;
        if set(&mut moved, start.checked_add(to), end.checked_add(to)) {
            out.push(moved);
        }
    }
    Some(out)
}

/// Give `span` the bounds `start..end` if they are a non-empty run a span
/// can hold; whether it still stands.
fn set(span: &mut Span, start: Option<usize>, end: Option<usize>) -> bool {
    match (
        start.and_then(|at| u32::try_from(at).ok()),
        end.and_then(|at| u32::try_from(at).ok()),
    ) {
        (Some(start), Some(end)) if start < end => {
            span.start = start;
            span.end = end;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "highlight_tests.rs"]
mod tests;
