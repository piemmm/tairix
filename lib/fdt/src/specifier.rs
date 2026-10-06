//! Phandle-and-specifier lists: `dmas`, `iommus`, `clocks` and every other
//! property naming providers, each entry a phandle followed by as many cells
//! as the provider's own `#…-cells` asks (Devicetree spec v0.4 §2.4).

use crate::{be_u32, phandle_ref, FdtError};

/// One entry: the provider it names and the specifier it gives it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PhandleArgs<'a> {
    /// The provider's phandle.
    pub phandle: u32,
    cells: &'a [u8],
}

impl PhandleArgs<'_> {
    /// How many cells the specifier has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len() / 4
    }

    /// Whether the specifier has no cells.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The specifier's cell at `index`.
    #[must_use]
    pub fn cell(&self, index: usize) -> Option<u32> {
        be_u32(self.cells, index.checked_mul(4)?)
    }

    /// The specifier's cells.
    #[must_use = "an iterator does nothing until it is drained"]
    pub fn cells(&self) -> impl ExactSizeIterator<Item = u32> + '_ {
        self.cells
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&cell| u32::from_be_bytes(cell))
    }
}

/// Iterator over a phandle-and-specifier list, produced by [`phandle_args`].
#[derive(Clone)]
pub struct PhandleArgsIter<'a, F> {
    value: &'a [u8],
    off: usize,
    resolve: F,
}

/// Walk `value`, a phandle-and-specifier list. `resolve` answers each
/// entry's provider and its cell count; each item pairs the two.
///
/// An entry whose provider `resolve` cannot answer, or that the list ends
/// inside, yields [`FdtError::BadProperty`] and ends the walk: nothing after
/// it can be framed.
pub fn phandle_args<T, F>(value: &[u8], resolve: F) -> PhandleArgsIter<'_, F>
where
    F: FnMut(u32) -> Option<(T, u32)>,
{
    PhandleArgsIter {
        value,
        off: 0,
        resolve,
    }
}

impl<'a, T, F: FnMut(u32) -> Option<(T, u32)>> Iterator for PhandleArgsIter<'a, F> {
    type Item = Result<(T, PhandleArgs<'a>), FdtError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.off >= self.value.len() {
            return None;
        }
        let entry = self.frame();
        if entry.is_err() {
            self.off = self.value.len();
        }
        Some(entry)
    }
}

impl<'a, T, F: FnMut(u32) -> Option<(T, u32)>> PhandleArgsIter<'a, F> {
    fn frame(&mut self) -> Result<(T, PhandleArgs<'a>), FdtError> {
        let phandle = be_u32(self.value, self.off)
            .and_then(phandle_ref)
            .ok_or(FdtError::BadProperty)?;
        let (provider, cells) = (self.resolve)(phandle).ok_or(FdtError::BadProperty)?;
        let start = self.off + 4;
        let end = usize::try_from(cells)
            .ok()
            .and_then(|cells| cells.checked_mul(4))
            .and_then(|len| start.checked_add(len))
            .ok_or(FdtError::BadProperty)?;
        let cells = self.value.get(start..end).ok_or(FdtError::BadProperty)?;
        self.off = end;
        Ok((provider, PhandleArgs { phandle, cells }))
    }
}

impl<T, F: FnMut(u32) -> Option<(T, u32)>> core::iter::FusedIterator for PhandleArgsIter<'_, F> {}

#[cfg(test)]
mod tests {
    use super::phandle_args;
    use crate::FdtError;
    use alloc::vec;
    use alloc::vec::Vec;

    fn cells(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// Phandle 1 takes one cell, phandle 2 two, phandle 3 none; any other is
    /// unknown.
    fn width(phandle: u32) -> Option<(u32, u32)> {
        match phandle {
            1 => Some((10, 1)),
            2 => Some((20, 2)),
            3 => Some((30, 0)),
            _ => None,
        }
    }

    #[test]
    fn each_entry_is_framed_by_its_own_providers_width() {
        let value = cells(&[2, 7, 8, 3, 1, 9]);
        let entries: Vec<(u32, u32, Vec<u32>)> = phandle_args(&value, width)
            .map(|entry| {
                let (provider, args) = entry.expect("frames");
                (provider, args.phandle, args.cells().collect())
            })
            .collect();
        assert_eq!(
            entries,
            [(20, 2, vec![7, 8]), (30, 3, vec![]), (10, 1, vec![9])]
        );
    }

    #[test]
    fn an_entry_that_cannot_be_framed_ends_the_list() {
        for value in [
            cells(&[1, 5, 4, 6, 1, 7]),
            cells(&[1, 5, 2, 6]),
            cells(&[0, 1, 5]),
            cells(&[u32::MAX]),
        ] {
            let entries: Vec<_> = phandle_args(&value, width).collect();
            assert_eq!(entries.last().map(Result::is_err), Some(true));
            assert!(entries[..entries.len() - 1].iter().all(Result::is_ok));
            assert_eq!(entries.iter().filter(|e| e.is_err()).count(), 1);
        }
        let mut walk = phandle_args(&[0u8, 0, 0], width);
        assert_eq!(
            walk.next().map(Result::err),
            Some(Some(FdtError::BadProperty))
        );
        assert!(walk.next().is_none());
    }

    #[test]
    fn a_cell_past_the_specifier_is_none() {
        let value = cells(&[2, 7, 8]);
        let (_, args) = phandle_args(&value, width)
            .next()
            .expect("one")
            .expect("frames");
        assert_eq!((args.len(), args.cell(1), args.cell(2)), (2, Some(8), None));
        assert!(!args.is_empty());
    }
}
