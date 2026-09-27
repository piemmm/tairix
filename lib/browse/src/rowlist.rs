//! A list of rows and which of them is current.
//!
//! The *Open With…* chooser and the Properties window's attribute list both
//! keep a keyboard cursor over a set of rows that can shrink under it.
//! [`RowList`] is that cursor once, so both clamp it the same way. Where the
//! rows are scrolled to is the surface's own
//! [`ScrollColumn`](crate::ScrollColumn), which reveals the cursor through the
//! rows' geometry.

/// A cursor over `len` rows.
#[derive(Clone, Debug)]
pub struct RowList {
    len: usize,
    cursor: usize,
}

impl RowList {
    /// A list of `len` rows with the first row current.
    #[must_use]
    pub const fn new(len: usize) -> Self {
        Self { len, cursor: 0 }
    }

    /// How many rows the list holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the list holds no rows at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Adopt a refreshed row count, keeping the cursor where it is wherever
    /// the new set still reaches it.
    ///
    /// A set that shrank past the cursor clamps it rather than leaving it off
    /// the end: a removed row must not leave the cursor naming a row that is
    /// now somebody else's.
    pub fn resize(&mut self, len: usize) {
        self.len = len;
        self.cursor = self.cursor.min(len.saturating_sub(1));
    }

    /// Which row is current.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Make `index` current, clamped to the rows that exist, reporting whether
    /// the cursor moved.
    pub fn select(&mut self, index: usize) -> bool {
        let clamped = index.min(self.len.saturating_sub(1));
        let moved = clamped != self.cursor;
        self.cursor = clamped;
        moved
    }

    /// Move the cursor by `delta` rows (positive moves toward the end),
    /// stopping at either end, reporting whether it moved.
    pub fn step(&mut self, delta: i64) -> bool {
        let from = i64::try_from(self.cursor).unwrap_or(i64::MAX);
        let to = from.saturating_add(delta).max(0);
        self.select(usize::try_from(to).unwrap_or(usize::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::RowList;

    #[test]
    fn a_cursor_clamps_to_the_rows_that_exist() {
        let mut list = RowList::new(3);
        assert_eq!(list.cursor(), 0);
        assert!(list.select(2));
        assert!(!list.select(9), "clamped to the last row, so nothing moved");
        assert_eq!(list.cursor(), 2);
        assert!(list.step(-1));
        assert_eq!(list.cursor(), 1);
        assert!(list.step(-5));
        assert_eq!(list.cursor(), 0);
        assert!(!list.step(-1), "already at the top");
    }

    #[test]
    fn an_empty_list_has_no_row_to_make_current() {
        let mut list = RowList::new(0);
        assert!(list.is_empty());
        assert!(!list.select(4));
        assert!(!list.step(1));
        assert_eq!(list.cursor(), 0);
    }

    #[test]
    fn a_shrunk_set_never_leaves_the_cursor_naming_somebody_elses_row() {
        let mut list = RowList::new(8);
        assert!(list.select(7));
        list.resize(3);
        assert_eq!(list.cursor(), 2);
        assert_eq!(list.len(), 3);
        list.resize(0);
        assert_eq!(list.cursor(), 0);
    }
}
