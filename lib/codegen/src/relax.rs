//! Branch relaxation for fixed-width encodings: each jump takes the
//! shortest of its forms that reaches its label. Forms only ever lengthen,
//! one jump at a time, until every jump reaches, so the search ends within
//! one pass per jump and lands on the least code any such layout has.

use alloc::vec;
use alloc::vec::Vec;

use crate::lower::Label;
use crate::CompileError;

/// What relaxation reads of a machine instruction.
pub(crate) trait Relaxable {
    /// The label it binds, if it binds one.
    fn binds(&self) -> Option<Label>;
    /// The label it jumps to, if it jumps.
    fn target(&self) -> Option<Label>;
    /// Its bytes in form `form`: a jump has one or more, shortest first,
    /// anything else just the one.
    fn length(&self, form: usize) -> usize;
    /// How many forms it has.
    fn forms(&self) -> usize;
    /// Whether form `form` reaches `distance` bytes from where the
    /// instruction begins.
    fn reaches(&self, form: usize, distance: i64) -> bool;
}

/// A layout: each instruction's form and place, and each label's place.
pub(crate) struct Layout {
    /// Each instruction's form.
    pub(crate) forms: Vec<usize>,
    /// Where each instruction begins.
    pub(crate) places: Vec<usize>,
    /// Where each label is.
    pub(crate) labels: Vec<usize>,
    /// The code's length.
    pub(crate) length: usize,
}

impl Layout {
    /// The distance from instruction `n` to `label`.
    pub(crate) fn distance(&self, n: usize, label: Label) -> Result<i64, CompileError> {
        let to = i64::try_from(*self.labels.get(label.0).ok_or(CompileError::Malformed)?)
            .map_err(|_| CompileError::TooLarge)?;
        let from = i64::try_from(self.places[n]).map_err(|_| CompileError::TooLarge)?;
        Ok(to - from)
    }
}

/// Lay `insts` out, jumps relaxed.
///
/// # Errors
///
/// [`CompileError::Malformed`] for a jump to a label never bound, and
/// [`CompileError::TooLarge`] for one no form reaches.
pub(crate) fn relax<I: Relaxable>(insts: &[I], labels: usize) -> Result<Layout, CompileError> {
    let mut layout = Layout {
        forms: vec![0; insts.len()],
        places: vec![0; insts.len()],
        labels: vec![usize::MAX; labels],
        length: 0,
    };
    loop {
        let mut at = 0usize;
        for (n, inst) in insts.iter().enumerate() {
            layout.places[n] = at;
            if let Some(label) = inst.binds() {
                *layout
                    .labels
                    .get_mut(label.0)
                    .ok_or(CompileError::Malformed)? = at;
            }
            at += inst.length(layout.forms[n]);
        }
        layout.length = at;
        let mut grown = false;
        for (n, inst) in insts.iter().enumerate() {
            let Some(label) = inst.target() else {
                continue;
            };
            if layout
                .labels
                .get(label.0)
                .is_none_or(|&at| at == usize::MAX)
            {
                return Err(CompileError::Malformed);
            }
            let distance = layout.distance(n, label)?;
            if !inst.reaches(layout.forms[n], distance) {
                if layout.forms[n] + 1 >= inst.forms() {
                    return Err(CompileError::TooLarge);
                }
                layout.forms[n] += 1;
                grown = true;
            }
        }
        if !grown {
            return Ok(layout);
        }
    }
}

#[cfg(test)]
#[path = "relax_tests.rs"]
mod tests;
