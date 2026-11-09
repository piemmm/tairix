//! Optional content (ISO 32000-2 §8.11): the groups a document's default
//! configuration hides, and whether content they govern is shown.

use core::cell::RefCell;

use tairix_collections::{OrderedMap, OrderedSet};

use crate::document::Document;
use crate::limits::MAX_EXPRESSION_NODES;
use crate::memo::{Bounds, Memo, Node, Walk};
use crate::object::{Dict, Object};
use crate::PdfError;

/// What a visibility expression is read within: none written in earnest
/// nests deeper, and past it lies one that contains itself.
const BOUNDS: Bounds = Bounds {
    depth: 16,
    nodes: MAX_EXPRESSION_NODES,
};

/// Whether a visibility expression holds, and how deep it nests.
#[derive(Copy, Clone)]
struct Holds {
    holds: bool,
    nested: usize,
}

impl Node for Holds {
    fn nested(&self) -> usize {
        self.nested
    }
}

/// The optional content groups hidden by default, by object number.
#[derive(Default)]
pub(crate) struct Visibility {
    hidden: OrderedSet<u32>,
    /// Each visibility expression object, read once wherever it is met: an
    /// expression may share another however many times over.
    expressions: Memo<Holds>,
    /// Whether content each governing object shows, found once for each one
    /// read by number however much content it governs.
    governed: RefCell<OrderedMap<u32, Result<bool, PdfError>>>,
}

impl Visibility {
    /// What the default configuration of `catalog`'s optional content hides.
    /// A configuration that cannot be read hides nothing, so content is shown
    /// rather than lost.
    pub(crate) fn of(document: &Document, catalog: &Dict) -> Result<Self, PdfError> {
        let mut visibility = Self::default();
        let properties = document.get(catalog, b"OCProperties")?;
        let Some(properties) = properties.as_dict() else {
            return Ok(visibility);
        };
        let defaults = document.get(properties, b"D")?;
        let Some(defaults) = defaults.as_dict() else {
            return Ok(visibility);
        };
        let listed = |dict: &Dict, key: &[u8]| -> Result<OrderedSet<u32>, PdfError> {
            let object = document.get(dict, key)?;
            let mut groups = OrderedSet::new();
            for group in object.as_array().unwrap_or_default() {
                if let Some(group) = group.as_reference() {
                    groups.try_insert(group.num)?;
                }
            }
            Ok(groups)
        };
        if document.get(defaults, b"BaseState")?.as_name() == Some(b"OFF") {
            let on = listed(defaults, b"ON")?;
            for &group in &listed(properties, b"OCGs")? {
                if !on.contains(&group) {
                    visibility.hidden.try_insert(group)?;
                }
            }
        }
        for &group in &listed(defaults, b"OFF")? {
            visibility.hidden.try_insert(group)?;
        }
        Ok(visibility)
    }

    /// Whether content governed by `object` — a group or a membership
    /// dictionary — is shown.
    pub(crate) fn shows(&self, document: &Document, object: &Object) -> Result<bool, PdfError> {
        if self.hidden.is_empty() {
            return Ok(true);
        }
        let Some(reference) = object.as_reference() else {
            return self.governs(document, object);
        };
        let known = self
            .governed
            .try_borrow()
            .ok()
            .and_then(|held| held.get(&reference.num).copied());
        if let Some(known) = known {
            return known;
        }
        let shown = self.governs(document, object);
        // A bound passed or the heap refused says nothing of the object.
        if !matches!(shown, Err(PdfError::TooComplex | PdfError::OutOfMemory)) {
            if let Ok(mut held) = self.governed.try_borrow_mut() {
                held.try_insert(reference.num, shown)?;
            }
        }
        shown
    }

    /// Whether content governed by `object` is shown, read from the file.
    fn governs(&self, document: &Document, object: &Object) -> Result<bool, PdfError> {
        let group = |object: &Object| {
            object
                .as_reference()
                .is_none_or(|group| !self.hidden.contains(&group.num))
        };
        let resolved = document.resolve(object)?;
        let Some(dict) = resolved.as_dict() else {
            return Ok(true);
        };
        if !dict.is_type(b"OCMD") {
            return Ok(group(object));
        }
        if let Some(expression) = dict.get(b"VE") {
            let holds = self.expression(document, expression, 0, &mut Walk::default())?;
            return Ok(holds.holds);
        }
        let groups = document.get(dict, b"OCGs")?;
        let members: &[Object] = match dict.get(b"OCGs") {
            Some(single @ Object::Ref(_)) if groups.as_dict().is_some() => {
                core::slice::from_ref(single)
            }
            _ => groups.as_array().unwrap_or_default(),
        };
        if members.is_empty() {
            return Ok(true);
        }
        document.work().charge(members.len())?;
        let mut shown = members.iter().map(group);
        Ok(match document.get(dict, b"P")?.as_name() {
            Some(b"AllOn") => shown.all(|on| on),
            Some(b"AnyOff") => shown.any(|on| !on),
            Some(b"AllOff") => shown.all(|on| !on),
            _ => shown.any(|on| on),
        })
    }

    /// Whether visibility expression `expression` holds, met `depth` levels
    /// down `walk`: each object of it read once, wherever it is met, and
    /// every meeting charged.
    fn expression(
        &self,
        document: &Document,
        expression: &Object,
        depth: usize,
        walk: &mut Walk,
    ) -> Result<Holds, PdfError> {
        document.work().charge(1)?;
        self.expressions
            .node(expression, depth, &BOUNDS, walk, |walk| {
                self.evaluate(document, expression, depth, walk)
            })
    }

    /// Whether `expression` holds, read from the file: a group, or an
    /// operator over expressions.
    fn evaluate(
        &self,
        document: &Document,
        expression: &Object,
        depth: usize,
        walk: &mut Walk,
    ) -> Result<Holds, PdfError> {
        let resolved = document.resolve(expression)?;
        let Some((operator, operands)) = resolved.as_array().and_then(<[Object]>::split_first)
        else {
            let holds = expression
                .as_reference()
                .is_none_or(|group| !self.hidden.contains(&group.num));
            return Ok(Holds { holds, nested: 0 });
        };
        let mut nested = 0;
        let holds = {
            let mut results = operands.iter().map(|operand| {
                let read = self.expression(document, operand, depth + 1, walk)?;
                nested = nested.max(read.nested + 1);
                Ok::<_, PdfError>(read.holds)
            });
            match operator.as_name() {
                Some(b"And") => {
                    results.try_fold(true, |all, holds| Ok::<_, PdfError>(all & holds?))?
                }
                Some(b"Or") => {
                    results.try_fold(false, |any, holds| Ok::<_, PdfError>(any | holds?))?
                }
                Some(b"Not") => !results.next().unwrap_or(Ok(false))?,
                _ => true,
            }
        };
        Ok(Holds { holds, nested })
    }
}

#[cfg(test)]
#[path = "optional_tests.rs"]
mod tests;
