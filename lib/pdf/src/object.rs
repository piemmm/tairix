//! The objects of ISO 32000-2 §7.3.

use alloc::borrow::Cow;
use alloc::vec::Vec;
use core::borrow::Borrow;
use core::ops::Range;

use tairix_util::fallible;

use crate::PdfError;

/// An indirect object's number and generation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct ObjRef {
    pub(crate) num: u32,
    pub(crate) gen: u16,
}

/// A name: its bytes, escapes resolved.
#[derive(Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct Name(pub(crate) Vec<u8>);

impl Name {
    /// The name `bytes` spell, the copy refusable.
    pub(crate) fn of(bytes: &[u8]) -> Result<Self, PdfError> {
        Ok(Self(copied(bytes)?))
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Borrow<[u8]> for Name {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

/// `bytes` copied, the allocation refusable.
fn copied(bytes: &[u8]) -> Result<Vec<u8>, PdfError> {
    fallible::copied(bytes).ok_or(PdfError::OutOfMemory)
}

/// Dictionaries this small are sorted in place; a larger one sorts its
/// positions so the sort's scratch is an allocation that can be refused.
const SORTED_IN_PLACE: usize = 16;

/// A dictionary: its entries sorted by key, so a lookup is a binary search
/// however many a page's resources hold.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Dict {
    entries: Vec<(Name, Object)>,
}

impl Dict {
    pub(crate) const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// The dictionary `entries` make in the order they were written, the last
    /// value of a repeated key kept, as every reader does.
    pub(crate) fn from_written(mut entries: Vec<(Name, Object)>) -> Result<Self, PdfError> {
        sort_stably(&mut entries)?;
        entries.dedup_by(|later, kept| {
            let repeated = later.0 == kept.0;
            if repeated {
                core::mem::swap(later, kept);
            }
            repeated
        });
        Ok(Self { entries })
    }

    fn position(&self, key: &[u8]) -> Result<usize, usize> {
        self.entries
            .binary_search_by(|(name, _)| name.as_bytes().cmp(key))
    }

    /// The value of `key`, if it has one.
    pub(crate) fn get(&self, key: &[u8]) -> Option<&Object> {
        self.position(key).ok().map(|at| &self.entries[at].1)
    }

    /// Its entries, by key, taken.
    pub(crate) fn into_entries(self) -> Vec<(Name, Object)> {
        self.entries
    }

    /// Its entries, by key.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Name, &Object)> {
        self.entries.iter().map(|(name, value)| (name, value))
    }

    /// Its values, mutably.
    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut Object> {
        self.entries.iter_mut().map(|(_, value)| value)
    }

    /// The name `key` names, if it is a name.
    pub(crate) fn name(&self, key: &[u8]) -> Option<&[u8]> {
        self.get(key).and_then(Object::as_name)
    }

    /// Whether `/Type` is `kind`.
    pub(crate) fn is_type(&self, kind: &[u8]) -> bool {
        self.name(b"Type") == Some(kind)
    }

    /// Add every entry of `older` whose key it does not hold: one merge of
    /// the two sorted lists, however many keys either holds.
    pub(crate) fn complete_from(&mut self, older: &Self) -> Result<(), PdfError> {
        let mut merged = Vec::new();
        merged.try_reserve_exact(self.entries.len() + older.entries.len())?;
        let mut own = core::mem::take(&mut self.entries).into_iter().peekable();
        let mut theirs = older.entries.iter().peekable();
        loop {
            let take_own = match (own.peek(), theirs.peek()) {
                (Some(mine), Some(&(key, _))) => mine.0 <= *key,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if take_own {
                if let Some(entry) = own.next() {
                    if theirs.peek().is_some_and(|&(key, _)| *key == entry.0) {
                        theirs.next();
                    }
                    merged.push(entry);
                }
            } else if let Some((key, value)) = theirs.next() {
                merged.push((Name::of(key.as_bytes())?, value.try_clone()?));
            }
        }
        self.entries = merged;
        Ok(())
    }

    /// A copy of it, every allocation the copy takes refusable.
    pub(crate) fn try_clone(&self) -> Result<Self, PdfError> {
        let mut entries = Vec::new();
        entries.try_reserve_exact(self.entries.len())?;
        for (key, value) in &self.entries {
            entries.push((Name::of(key.as_bytes())?, value.try_clone()?));
        }
        Ok(Self { entries })
    }
}

/// Sort `entries` by key, the entries of a repeated key left in the order
/// they were written. `sort_by` would take its scratch from the heap without
/// asking.
fn sort_stably(entries: &mut Vec<(Name, Object)>) -> Result<(), PdfError> {
    if entries.is_sorted_by(|a, b| a.0 <= b.0) {
        return Ok(());
    }
    if entries.len() <= SORTED_IN_PLACE {
        for end in 1..entries.len() {
            let mut at = end;
            while at > 0 && entries[at - 1].0 > entries[at].0 {
                entries.swap(at - 1, at);
                at -= 1;
            }
        }
        return Ok(());
    }
    let count = entries.len();
    let mut order = fallible::collected(count, 0..count).ok_or(PdfError::OutOfMemory)?;
    order.sort_unstable_by(|&a, &b| entries[a].0.cmp(&entries[b].0).then(a.cmp(&b)));
    let mut sorted = Vec::new();
    sorted.try_reserve_exact(count)?;
    sorted.extend(
        order
            .into_iter()
            .map(|at| core::mem::take(&mut entries[at])),
    );
    *entries = sorted;
    Ok(())
}

/// A stream: its dictionary, and where its bytes are in the file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stream {
    pub(crate) dict: Dict,
    /// The span of the file its encoded bytes occupy.
    pub(crate) data: Range<usize>,
    /// The object it is, which its decryption key derives from.
    pub(crate) owner: ObjRef,
}

/// An object.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Object {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    /// A string of bytes.
    String(Vec<u8>),
    Name(Name),
    Array(Vec<Object>),
    Dict(Dict),
    Stream(Stream),
    /// A reference to an indirect object.
    Ref(ObjRef),
}

impl Object {
    /// A copy of it, every allocation the copy takes refusable: what a read
    /// copies grows with the file, so no copy may abort the reader.
    pub(crate) fn try_clone(&self) -> Result<Self, PdfError> {
        Ok(match self {
            Self::Null => Self::Null,
            Self::Bool(value) => Self::Bool(*value),
            Self::Int(value) => Self::Int(*value),
            Self::Real(value) => Self::Real(*value),
            Self::String(bytes) => Self::String(copied(bytes)?),
            Self::Name(name) => Self::Name(Name::of(name.as_bytes())?),
            Self::Array(items) => {
                let mut copy = Vec::new();
                copy.try_reserve_exact(items.len())?;
                for item in items {
                    copy.push(item.try_clone()?);
                }
                Self::Array(copy)
            }
            Self::Dict(dict) => Self::Dict(dict.try_clone()?),
            Self::Stream(stream) => Self::Stream(Stream {
                dict: stream.dict.try_clone()?,
                data: stream.data.clone(),
                owner: stream.owner,
            }),
            Self::Ref(reference) => Self::Ref(*reference),
        })
    }

    /// Its value as a number, an integer or a real.
    pub(crate) const fn as_number(&self) -> Option<f64> {
        match *self {
            Self::Int(value) => Some(real(value)),
            Self::Real(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) const fn as_int(&self) -> Option<i64> {
        match *self {
            Self::Int(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn as_name(&self) -> Option<&[u8]> {
        match self {
            Self::Name(name) => Some(name.as_bytes()),
            _ => None,
        }
    }

    pub(crate) fn as_string(&self) -> Option<&[u8]> {
        match self {
            Self::String(bytes) => Some(bytes),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&[Object]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Its dictionary, if it is a dictionary or a stream.
    pub(crate) const fn as_dict(&self) -> Option<&Dict> {
        match self {
            Self::Dict(dict) | Self::Stream(Stream { dict, .. }) => Some(dict),
            _ => None,
        }
    }

    pub(crate) const fn as_stream(&self) -> Option<&Stream> {
        match self {
            Self::Stream(stream) => Some(stream),
            _ => None,
        }
    }

    pub(crate) const fn as_reference(&self) -> Option<ObjRef> {
        match *self {
            Self::Ref(reference) => Some(reference),
            _ => None,
        }
    }

    pub(crate) const fn as_bool(&self) -> Option<bool> {
        match *self {
            Self::Bool(value) => Some(value),
            _ => None,
        }
    }
}

/// `value` as a real: a PDF integer is read as one wherever a real is wanted.
#[allow(
    clippy::cast_precision_loss,
    reason = "an integer past 2^53 is read as the nearest real, as a real it is"
)]
pub(crate) const fn real(value: i64) -> f64 {
    value as f64
}

/// `object` owned: copied, refusably, where it is borrowed.
pub(crate) fn owned(object: Cow<'_, Object>) -> Result<Object, PdfError> {
    match object {
        Cow::Owned(object) => Ok(object),
        Cow::Borrowed(object) => object.try_clone(),
    }
}

#[cfg(test)]
#[path = "object_tests.rs"]
mod tests;
