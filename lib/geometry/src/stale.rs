//! [`StaleFrames`]: what each frame of a presentation ring lacks of the
//! picture last presented.
//!
//! A present copies into the frame it uses only what changed, which leaves
//! every other frame a present behind wherever that was. Owing each change to
//! the frames that missed it keeps all of them current through copies the size
//! of the changes rather than of the picture. The display client brings its
//! back frame up to date from the compositor's picture and a window client from
//! the application's, both through this one record.

use alloc::vec::Vec;

use crate::{Rect, Region};

/// What each frame of a ring lacks of the picture last presented through it.
pub struct StaleFrames {
    owed: Vec<Region>,
    bounds: Rect,
    budget: usize,
}

impl StaleFrames {
    /// `count` frames covering `bounds`, each lacking all of it. A frame's
    /// owed region coarsens to its bounding box past `budget` rectangles.
    #[must_use]
    pub fn new(count: usize, bounds: Rect, budget: usize) -> Self {
        let mut owed = Vec::with_capacity(count);
        owed.resize_with(count, || Region::with_budget(budget));
        let mut stale = Self {
            owed,
            bounds,
            budget,
        };
        stale.reset(bounds);
        stale
    }

    /// Every frame lacks the whole of `bounds` again: the ring was mapped
    /// afresh or re-shaped.
    pub fn reset(&mut self, bounds: Rect) {
        self.bounds = bounds;
        for owed in &mut self.owed {
            owed.clear();
            owed.add(bounds);
        }
    }

    /// Bring frame `index` up to date for a present of which `changed`
    /// changed: `update` is handed everything the frame lacks, `changed`
    /// included, within the ring's bounds.
    ///
    /// Once `update` succeeds the frame lacks nothing and `changed` is owed to
    /// every other frame. A refusal leaves the frame lacking all of it, since
    /// what a refused update wrote is unknown, and owes the others nothing.
    /// A frame the ring does not have is handed its whole bounds.
    ///
    /// # Errors
    ///
    /// `update`'s refusal, unchanged.
    pub fn bring_up_to_date<E>(
        &mut self,
        index: usize,
        changed: &Region,
        update: impl FnOnce(&Region) -> Result<(), E>,
    ) -> Result<(), E> {
        let Some(slot) = self.owed.get_mut(index) else {
            return update(&Region::from(self.bounds));
        };
        let mut refresh = core::mem::replace(slot, Region::with_budget(self.budget));
        refresh.add_region(changed);
        refresh.clip(self.bounds);
        let updated = update(&refresh);
        if updated.is_ok() {
            refresh.clear();
            for (other, owed) in self.owed.iter_mut().enumerate() {
                if other != index {
                    owed.add_region(changed);
                    owed.clip(self.bounds);
                }
            }
        }
        if let Some(slot) = self.owed.get_mut(index) {
            *slot = refresh;
        }
        updated
    }
}

#[cfg(test)]
#[path = "stale_tests.rs"]
mod tests;
