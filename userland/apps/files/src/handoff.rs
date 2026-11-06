//! What a double-click that opens its item somewhere else owes the window it
//! was made in: the item deselected, once the hand-off has gone ahead.
//!
//! # Why this is its own module
//!
//! The `Run` binary around it is a freestanding program — it only exists when
//! the crate is built for a bare-metal target — so nothing inside it can be
//! reached by a host test. Which hand-offs deselect their item is worth
//! testing: one that went ahead does, once; one refused never does; and a disk
//! image's mount is answered long after the double-click, by a worker that
//! answers in turn. All of that is a pure function of what was asked and what
//! each answer came to, so it lives here and is covered by the tests beside it.

use alloc::vec::Vec;

use tairix_browse::Listed;

/// A double-clicked entry handed to something outside its window: a document
/// to its application, an application started or reached, a disk image
/// mounted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Handoff {
    /// The window the double-click was made in.
    pub window_id: u64,
    /// The entry, as that window listed it.
    pub entry: Listed,
}

/// The hand-offs awaiting their answer, and those that went ahead and have
/// still to deselect their item.
///
/// Both are bounded by what is in flight: the mounts by the acts the launcher
/// and its mount worker hold, and what went ahead by one turn of the loop,
/// which takes it.
#[derive(Debug, Default)]
pub struct Handoffs {
    /// The disk images being mounted for a double-click, each by the number
    /// its attach carries.
    mounting: Vec<(u64, Handoff)>,
    /// The number the next is given: never the same one twice.
    next: u64,
    /// What went ahead since the loop last took it.
    handed: Vec<Handoff>,
}

impl Handoffs {
    /// `handoff`, where a gesture made one, went ahead.
    pub fn handed(&mut self, handoff: Option<Handoff>) {
        self.handed.extend(handoff);
    }

    /// `handoff`, where a gesture made one, waits on the mount of a disk
    /// image: the number its attach carries, so the attach's refusal or
    /// answer settles this hand-off and no other ask of the image.
    pub fn mounting(&mut self, handoff: Option<Handoff>) -> Option<u64> {
        let handoff = handoff?;
        let number = self.next;
        self.next = self.next.wrapping_add(1);
        self.mounting.push((number, handoff));
        Some(number)
    }

    /// The attach carrying `handoff` was refused before it began: that
    /// hand-off goes with it.
    pub fn refused(&mut self, handoff: Option<u64>) {
        drop(self.settle(handoff));
    }

    /// The attach carrying `handoff` was answered, its volume shown when
    /// `shown`: that hand-off goes ahead or goes.
    pub fn mounted(&mut self, handoff: Option<u64>, shown: bool) {
        if let Some(handed) = self.settle(handoff).filter(|_| shown) {
            self.handed.push(handed);
        }
    }

    /// Take the hand-off numbered `handoff` out of those awaiting an answer.
    fn settle(&mut self, handoff: Option<u64>) -> Option<Handoff> {
        let number = handoff?;
        let at = self.mounting.iter().position(|(held, _)| *held == number)?;
        Some(self.mounting.remove(at).1)
    }

    /// Every hand-off that went ahead since this was last asked, each once.
    #[must_use]
    pub fn take(&mut self) -> Vec<Handoff> {
        core::mem::take(&mut self.handed)
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use tairix_abi::fs::FileId;
    use tairix_browse::Listed;

    use super::{Handoff, Handoffs};

    /// The entry `name` in the folder `/Users/ada`, double-clicked in the
    /// window `window_id`.
    fn handoff(window_id: u64, name: &str) -> Handoff {
        Handoff {
            window_id,
            entry: Listed {
                path: alloc::vec![
                    String::from("Users"),
                    String::from("ada"),
                    String::from(name)
                ],
                id: FileId::NONE,
            },
        }
    }

    /// A hand-off is reported once it went ahead, and only once; a gesture
    /// that handed nothing reports nothing.
    #[test]
    fn a_hand_off_is_reported_once_it_went_ahead() {
        let mut handoffs = Handoffs::default();
        handoffs.handed(None);
        assert!(handoffs.take().is_empty(), "a gesture that handed nothing");
        handoffs.handed(Some(handoff(1, "notes.txt")));
        assert_eq!(handoffs.take(), [handoff(1, "notes.txt")]);
        assert!(handoffs.take().is_empty(), "each is reported once");
    }

    /// A disk image's double-click is reported when its volume is shown, and
    /// never when its mount is refused, before it began or once answered.
    #[test]
    fn a_mount_reports_its_hand_off_only_when_its_volume_is_shown() {
        let mut handoffs = Handoffs::default();
        assert_eq!(handoffs.mounting(None), None, "a gesture handed nothing");
        let a = handoffs.mounting(Some(handoff(1, "a.adf")));
        let b = handoffs.mounting(Some(handoff(1, "b.iso")));
        let c = handoffs.mounting(Some(handoff(1, "c.hdf")));
        handoffs.mounted(b, false);
        handoffs.refused(c);
        assert!(
            handoffs.take().is_empty(),
            "a refused mount deselects nothing"
        );
        handoffs.mounted(a, true);
        assert_eq!(handoffs.take(), [handoff(1, "a.adf")]);
        handoffs.mounted(a, true);
        handoffs.mounted(b, true);
        assert!(handoffs.take().is_empty(), "an answer is paired once");
    }

    /// A refusal settles only the hand-off its own attach carried: one that
    /// carried none — a crowded *Mount Read-Only* of the image a double-click
    /// is mounting — leaves the double-click's to be deselected once its own
    /// mount is shown.
    #[test]
    fn a_refusal_settles_only_its_own_hand_off() {
        let mut handoffs = Handoffs::default();
        let clicked = handoffs.mounting(Some(handoff(1, "a.adf")));
        let again = handoffs.mounting(Some(handoff(2, "a.adf")));
        handoffs.refused(None);
        handoffs.refused(again);
        handoffs.mounted(clicked, true);
        assert_eq!(handoffs.take(), [handoff(1, "a.adf")]);
    }
}
