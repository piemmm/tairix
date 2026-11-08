//! The document a window edits: its drawing, the identities its objects are
//! issued, and the history of what it was.
//!
//! Every state of the drawing is a persistent tree, so a step of history is
//! a drawing kept whole for the price of the nodes that changed; undoing is
//! taking it back. The history is bounded in steps, and memory pressure
//! shortens it from the oldest end.

use alloc::collections::VecDeque;

use tairix_collections::try_arc;
use tairix_drawing::{Children, Drawing, DrawingError, Ids, Kind, Node, SharedSlice, SharedStr};
use tairix_reclaim::PressureBand;

use crate::preferences::Preferences;

/// The most steps kept to undo when memory is plentiful.
pub const MOST_STEPS: usize = 1000;

/// A blank drawing as `preferences` say a new one starts: their page and
/// grid, holding one empty layer.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when it cannot be held.
pub fn blank(ids: &mut Ids, preferences: &Preferences) -> Result<Drawing, DrawingError> {
    let layer = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::new(),
            clip: None,
        },
    )?
    .with_name(Some(SharedStr::try_from_str("Layer 1")?))?;
    Drawing::new(
        preferences.page(),
        Children::single(try_arc(layer)?)?,
        SharedSlice::new(),
        preferences.grid(),
    )
}

/// One state the drawing was in, and what made the next.
#[derive(Clone, Debug)]
struct Step {
    drawing: Drawing,
    /// What the edit that left this state was called, for Undo's label.
    label: &'static str,
}

/// A window's document.
#[derive(Clone, Debug)]
pub struct Document {
    drawing: Drawing,
    ids: Ids,
    /// The states Undo takes back to, the nearest last.
    undo: VecDeque<Step>,
    /// The states Redo does again, the nearest last.
    redo: VecDeque<Step>,
    /// Counts every change, so a save of one state is told from another.
    generation: u64,
    /// The generation that last matched the file, if one does.
    saved: Option<u64>,
    /// How many steps may be kept.
    allowance: usize,
}

impl Document {
    /// A document of `drawing`, whose objects were issued from `ids`, matching
    /// its file when `saved`.
    #[must_use]
    pub fn new(drawing: Drawing, ids: Ids, saved: bool) -> Self {
        Self {
            drawing,
            ids,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            generation: 0,
            saved: saved.then_some(0),
            allowance: MOST_STEPS,
        }
    }

    /// The drawing as it stands.
    #[must_use]
    pub const fn drawing(&self) -> &Drawing {
        &self.drawing
    }

    /// Where new objects' identities are issued from.
    pub fn ids(&mut self) -> &mut Ids {
        &mut self.ids
    }

    /// Its generation: a different number for every state it has been in.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether it differs from what its file last held.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.saved != Some(self.generation)
    }

    /// Generation `generation` reached the file.
    pub fn saved(&mut self, generation: u64) {
        self.saved = Some(generation);
    }

    /// Make `drawing` the document's, as one step called `label`. The
    /// present drawing itself is no step at all.
    pub fn commit(&mut self, drawing: Drawing, label: &'static str) {
        if drawing.is(&self.drawing) {
            return;
        }
        let was = core::mem::replace(&mut self.drawing, drawing);
        self.redo.clear();
        keep(
            &mut self.undo,
            Step {
                drawing: was,
                label,
            },
        );
        self.trim();
        self.generation += 1;
    }

    /// What Undo would take back, if anything.
    #[must_use]
    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo.back().map(|step| step.label)
    }

    /// What Redo would do again, if anything.
    #[must_use]
    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo.back().map(|step| step.label)
    }

    /// Take the last step back, answering whether there was one.
    pub fn undo(&mut self) -> bool {
        let Some(step) = self.undo.pop_back() else {
            return false;
        };
        let now = core::mem::replace(&mut self.drawing, step.drawing);
        keep(
            &mut self.redo,
            Step {
                drawing: now,
                label: step.label,
            },
        );
        self.generation += 1;
        true
    }

    /// Do the last step taken back again, answering whether there was one.
    pub fn redo(&mut self) -> bool {
        let Some(step) = self.redo.pop_back() else {
            return false;
        };
        let was = core::mem::replace(&mut self.drawing, step.drawing);
        keep(
            &mut self.undo,
            Step {
                drawing: was,
                label: step.label,
            },
        );
        self.generation += 1;
        true
    }

    /// How many steps Undo can take back.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.undo.len()
    }

    /// Hold the history to what `band` allows: everything while memory is
    /// plentiful, less as it grows short, a handful in a crisis.
    pub fn adopt_pressure(&mut self, band: PressureBand) {
        self.allowance = match band {
            PressureBand::Normal | PressureBand::Mild => MOST_STEPS,
            PressureBand::Moderate => 64,
            PressureBand::Severe => 8,
            PressureBand::Critical => 1,
        };
        self.trim();
    }

    /// Hold the steps kept, both ways, to the allowance: the oldest to undo
    /// go first, then the farthest to redo.
    fn trim(&mut self) {
        let excess = (self.undo.len() + self.redo.len()).saturating_sub(self.allowance);
        let from_undo = excess.min(self.undo.len());
        self.undo.drain(..from_undo);
        self.redo.drain(..excess - from_undo);
    }
}

/// `step` kept nearest in `history`, the farthest step making room where
/// memory refuses more: a change is never refused for want of the history
/// that would take it back.
fn keep(history: &mut VecDeque<Step>, step: Step) {
    if history.try_reserve(1).is_err() && history.pop_front().is_none() {
        return;
    }
    history.push_back(step);
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "the values compared are set and read exactly"
)]
mod tests {
    use tairix_drawing::{Children, Drawing, Grid, Ids, Page, Unit};
    use tairix_reclaim::PressureBand;

    use super::Document;

    fn page(width: f64) -> Drawing {
        Drawing::new(
            Page {
                width,
                height: 100.0,
                unit: Unit::Px,
                background: None,
            },
            Children::new(),
            tairix_drawing::SharedSlice::new(),
            Grid::default(),
        )
        .expect("a drawing")
    }

    #[test]
    fn steps_are_taken_back_and_done_again() {
        let mut document = Document::new(page(100.0), Ids::new(), true);
        assert!(!document.is_modified());
        document.commit(page(200.0), "Resize page");
        document.commit(page(300.0), "Resize page");
        assert!(document.is_modified());
        assert_eq!(document.undo_label(), Some("Resize page"));
        assert!(document.undo());
        assert_eq!(document.drawing().page().width, 200.0);
        assert!(document.redo());
        assert_eq!(document.drawing().page().width, 300.0);
        assert!(document.undo() && document.undo());
        assert!(!document.undo(), "nothing more to take back");
        assert_eq!(document.drawing().page().width, 100.0);
        document.commit(page(400.0), "Resize page");
        assert!(!document.redo(), "a new step forgets what was taken back");
    }

    #[test]
    fn the_same_drawing_is_no_step() {
        let mut document = Document::new(page(100.0), Ids::new(), true);
        document.commit(document.drawing().clone(), "Nothing");
        assert_eq!(document.depth(), 0);
        assert!(!document.is_modified());
    }

    #[test]
    fn a_saved_generation_matches_until_the_next_change() {
        let mut document = Document::new(page(100.0), Ids::new(), false);
        assert!(document.is_modified(), "an untitled drawing has no file");
        document.commit(page(200.0), "Resize page");
        let generation = document.generation();
        document.saved(generation);
        assert!(!document.is_modified());
        document.undo();
        assert!(document.is_modified());
    }

    #[test]
    fn pressure_shortens_the_history_from_its_oldest_end() {
        let mut document = Document::new(page(1.0), Ids::new(), true);
        for width in 2..20 {
            document.commit(page(f64::from(width)), "Resize page");
        }
        document.adopt_pressure(PressureBand::Severe);
        assert_eq!(document.depth(), 8);
        for _ in 0..8 {
            assert!(document.undo());
        }
        assert_eq!(
            document.drawing().page().width,
            11.0,
            "the newest steps were kept"
        );
    }

    #[test]
    fn pressure_keeps_the_nearest_steps_to_redo() {
        let mut document = Document::new(page(1.0), Ids::new(), true);
        for width in 2..=10 {
            document.commit(page(f64::from(width)), "Resize page");
        }
        while document.undo() {}
        document.adopt_pressure(PressureBand::Severe);
        assert!(document.redo());
        assert_eq!(
            document.drawing().page().width,
            2.0,
            "the next step is kept"
        );
        let mut redone = 1;
        while document.redo() {
            redone += 1;
        }
        assert_eq!(redone, 8);
        assert_eq!(document.drawing().page().width, 9.0);
    }

    #[test]
    fn the_allowance_bounds_both_ways_together() {
        let mut document = Document::new(page(1.0), Ids::new(), true);
        for width in 2..=10 {
            document.commit(page(f64::from(width)), "Resize page");
        }
        for _ in 0..4 {
            document.undo();
        }
        document.adopt_pressure(PressureBand::Severe);
        let mut undone = 0;
        while document.undo() {
            undone += 1;
        }
        let mut redone = 0;
        while document.redo() {
            redone += 1;
        }
        assert_eq!(undone + 4, redone);
        assert_eq!(redone, 8, "eight steps are kept in all");
    }
}
