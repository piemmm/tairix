//! Optional content (ISO 32000-2 §8.11): the groups a document's default
//! configuration hides, and whether content they govern is shown.

use core::cell::RefCell;

use tairix_collections::{OrderedMap, OrderedSet};

use crate::document::Document;
use crate::object::{Dict, Object};
use crate::PdfError;

/// How deeply a visibility expression nests.
const MAX_EXPRESSION_DEPTH: usize = 16;

/// The optional content groups hidden by default, by object number.
#[derive(Default)]
pub(crate) struct Visibility {
    hidden: OrderedSet<u32>,
    /// Whether each visibility expression object holds, found once: an
    /// expression may share another however many times over.
    expressions: RefCell<OrderedMap<u32, bool>>,
}

impl Visibility {
    /// What the default configuration of `catalog`'s optional content hides.
    /// A configuration that cannot be read hides nothing, so content is shown
    /// rather than lost.
    pub(crate) fn of(document: &Document<'_>, catalog: &Dict) -> Result<Self, PdfError> {
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
    pub(crate) fn shows(&self, document: &Document<'_>, object: &Object) -> Result<bool, PdfError> {
        if self.hidden.is_empty() {
            return Ok(true);
        }
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
            return self.expression(document, expression, 0);
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
        let mut shown = members.iter().map(group);
        Ok(match document.get(dict, b"P")?.as_name() {
            Some(b"AllOn") => shown.all(|on| on),
            Some(b"AnyOff") => shown.any(|on| !on),
            Some(b"AllOff") => shown.all(|on| !on),
            _ => shown.any(|on| on),
        })
    }

    /// Whether visibility expression `expression` holds: a group, or an
    /// operator over expressions, each object of which is evaluated once.
    fn expression(
        &self,
        document: &Document<'_>,
        expression: &Object,
        depth: usize,
    ) -> Result<bool, PdfError> {
        // No expression written in earnest nests deeper: past it lies one
        // that contains itself.
        if depth > MAX_EXPRESSION_DEPTH {
            return Err(PdfError::Malformed);
        }
        let reference = expression.as_reference();
        if let Some(reference) = reference {
            let held = self.expressions.try_borrow().ok();
            if let Some(&holds) = held.as_ref().and_then(|held| held.get(&reference.num)) {
                return Ok(holds);
            }
        }
        let resolved = document.resolve(expression)?;
        let holds = match resolved.as_array().and_then(<[Object]>::split_first) {
            Some((operator, operands)) => {
                let mut results = operands
                    .iter()
                    .map(|operand| self.expression(document, operand, depth + 1));
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
            }
            None => reference.is_none_or(|group| !self.hidden.contains(&group.num)),
        };
        if let (Some(reference), Ok(mut held)) = (reference, self.expressions.try_borrow_mut()) {
            held.try_insert(reference.num, holds)?;
        }
        Ok(holds)
    }
}

#[cfg(test)]
#[path = "optional_tests.rs"]
mod tests;
