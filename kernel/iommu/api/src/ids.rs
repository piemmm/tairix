//! Ids a unit hands out — domain ids, remapping table entries — reused as
//! late as possible.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::IommuError;

/// Ids `0..limit`: every fresh one first, then the released ones in the
/// order they were released, so an id is reused as late as possible. One the
/// unit could not confirm released is never handed out again.
///
/// What it records grows with the ids handed out, not with `limit`: a unit
/// naming a million domains costs nothing until it is given them.
pub struct Ids {
    fresh: u32,
    limit: u32,
    released: VecDeque<u32>,
    /// One bit per id below `fresh` handed out and not released.
    live: Vec<u64>,
}

impl Ids {
    /// Ids `0..limit`, with `reserved` ids from the start never handed out.
    #[must_use]
    pub const fn new(reserved: u32, limit: u32) -> Self {
        Self {
            fresh: if reserved < limit { reserved } else { limit },
            limit,
            released: VecDeque::new(),
            live: Vec::new(),
        }
    }

    /// The next id, or [`None`] when every id is live or lost, or a fresh one
    /// cannot be recorded.
    pub fn take(&mut self) -> Option<u32> {
        let id = if self.fresh < self.limit {
            let words = (self.fresh / 64) as usize + 1;
            if words > self.live.len() {
                self.live.try_reserve(words - self.live.len()).ok()?;
                self.live.resize(words, 0);
            }
            self.fresh += 1;
            self.fresh - 1
        } else {
            self.released.pop_front()?
        };
        self.mark(id, true);
        Some(id)
    }

    /// The next id, as a sixteen-bit domain field holds it.
    ///
    /// # Errors
    ///
    /// [`IommuError::Exhausted`] when none is left: ids are handed out below
    /// `limit`, so none is wider where `limit` fits the field, and one wider
    /// is let go, never to be handed out.
    pub fn take_sixteen_bits(&mut self) -> Result<u16, IommuError> {
        let id = self.take().ok_or(IommuError::Exhausted)?;
        u16::try_from(id).map_err(|_| {
            self.release(id, false);
            IommuError::Exhausted
        })
    }

    /// Whether `id` is handed out and not released.
    #[must_use]
    pub fn is_live(&self, id: u32) -> bool {
        self.live
            .get((id / 64) as usize)
            .is_some_and(|word| word & (1 << (id % 64)) != 0)
    }

    /// Release live id `id`, to be handed out again once every earlier
    /// release has been, where the unit `confirmed` it gone.
    pub fn release(&mut self, id: u32, confirmed: bool) {
        if !self.is_live(id) {
            return;
        }
        self.mark(id, false);
        if confirmed && self.released.try_reserve(1).is_ok() {
            self.released.push_back(id);
        }
    }

    fn mark(&mut self, id: u32, live: bool) {
        if let Some(word) = self.live.get_mut((id / 64) as usize) {
            if live {
                *word |= 1 << (id % 64);
            } else {
                *word &= !(1 << (id % 64));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_ids_come_first_then_released_ones_in_release_order() {
        let mut ids = Ids::new(1, 4);
        assert_eq!(
            [ids.take(), ids.take(), ids.take()],
            [Some(1), Some(2), Some(3)]
        );
        assert_eq!(ids.take(), None, "id 0 is reserved");
        ids.release(3, true);
        ids.release(1, true);
        assert_eq!(
            [ids.take(), ids.take(), ids.take()],
            [Some(3), Some(1), None]
        );
    }

    #[test]
    fn an_unconfirmed_release_is_never_handed_out_again() {
        let mut ids = Ids::new(0, 2);
        assert_eq!(ids.take(), Some(0));
        ids.release(0, false);
        assert!(!ids.is_live(0));
        assert_eq!(ids.take(), Some(1));
        assert_eq!(ids.take(), None);
        ids.release(5, true);
        ids.release(0, true);
        assert_eq!(ids.take(), None, "a release of an id not live is ignored");
    }

    #[test]
    fn ids_are_recorded_as_they_are_handed_out_not_as_many_as_exist() {
        let mut ids = Ids::new(64, 1 << 20);
        assert!(ids.live.is_empty());
        for expected in 64..130 {
            assert_eq!(ids.take(), Some(expected));
        }
        assert_eq!(ids.live.len(), 3, "ids 64 to 129 span words 1 and 2");
        assert!(ids.is_live(129) && !ids.is_live(130) && !ids.is_live(1 << 19));
    }

    /// An id too wide for a sixteen-bit field is let go rather than left
    /// live, and never handed out again.
    #[test]
    fn an_id_too_wide_for_sixteen_bits_is_let_go() {
        let mut ids = Ids::new(0xFFFF, 0x1_0002);
        assert_eq!(ids.take_sixteen_bits(), Ok(0xFFFF));
        assert_eq!(ids.take_sixteen_bits(), Err(IommuError::Exhausted));
        assert!(!ids.is_live(0x1_0000));
        assert_eq!(ids.take_sixteen_bits(), Err(IommuError::Exhausted));
        assert!(!ids.is_live(0x1_0001));
    }
}
