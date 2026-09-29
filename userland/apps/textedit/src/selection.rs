//! The selection: an anchor where it began and a head where the caret is.

use core::ops::Range;

/// A selection of the document's bytes, by offset. Empty, it is a caret.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Selection {
    /// Where the selection began: the end that stays put as it extends.
    pub anchor: usize,
    /// Where the caret is: the end that moves.
    pub head: usize,
}

impl Selection {
    /// A caret at `at`.
    #[must_use]
    pub const fn caret(at: usize) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }

    /// The selected bytes, lowest first.
    #[must_use]
    pub fn range(self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    /// Whether nothing is selected.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.anchor == self.head
    }

    /// The caret moved to `to`, extending the selection when `extend`,
    /// collapsing it otherwise.
    #[must_use]
    pub const fn moved(self, to: usize, extend: bool) -> Self {
        if extend {
            Self {
                anchor: self.anchor,
                head: to,
            }
        } else {
            Self::caret(to)
        }
    }

    /// Both ends held within `len`.
    #[must_use]
    pub fn clamped(self, len: usize) -> Self {
        Self {
            anchor: self.anchor.min(len),
            head: self.head.min(len),
        }
    }
}
