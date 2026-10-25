//! A node's extended attributes copied, and what could not be.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::stdinfo::{Human, JsonStr, Severity, StdInfoKind, StdInfoRecord};
use tairix_abi::Errno;
use tairix_util::quote::{Quoted, Style};

use crate::tree::Node;

/// How a copy treats a node's extended attributes.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Attributes {
    /// Carry every attribute the destination can keep, and report the rest.
    #[default]
    Carry,
    /// Carry every attribute, or fail (`cp --preserve=xattr`).
    Require,
    /// Carry none (`cp --no-preserve=xattr`).
    Skip,
}

/// Why a destination could not keep an attribute.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Refusal {
    /// Its volume keeps no attribute of the key's kind.
    Unsupported,
    /// Its volume keeps the key, but cannot represent the value.
    NotRepresentable,
    /// The node has no room left for the attribute.
    NoRoom,
    /// The copier may not set the key there.
    NotPermitted,
}

impl Refusal {
    /// The refusal `err` from setting an attribute names. The key came from
    /// the source's own listing, so it is well formed and an out-of-range
    /// answer is about the value.
    #[must_use]
    pub const fn of(err: Errno) -> Option<Self> {
        match err {
            Errno::NotSupported | Errno::ReadOnly => Some(Self::Unsupported),
            Errno::OutOfRange => Some(Self::NotRepresentable),
            Errno::NoSpace | Errno::LengthOutOfRange => Some(Self::NoRoom),
            Errno::PermissionDenied => Some(Self::NotPermitted),
            _ => None,
        }
    }

    /// The stable code an advisory record names the refusal by.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unsupported => "destination_unsupported",
            Self::NotRepresentable => "value_not_representable",
            Self::NoRoom => "no_room",
            Self::NotPermitted => "not_permitted",
        }
    }

    /// What a reader is told of it.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Unsupported => "the destination cannot store them",
            Self::NotRepresentable => "the destination cannot represent their values",
            Self::NoRoom => "the destination has no room for them",
            Self::NotPermitted => "they may not be set there",
        }
    }
}

/// An attribute copy that failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttrError {
    /// The source's attributes could not be read.
    Read(Errno),
    /// The destination refused `key`, and the copy could not go on without
    /// it.
    Write {
        /// The attribute refused.
        key: String,
        /// The refusal.
        error: Errno,
    },
}

/// Copy the held node `source`'s extended attributes to the held node
/// `destination` as `mode` says, answering each one the destination could not
/// keep, and why.
///
/// # Errors
///
/// [`AttrError::Read`] when the source's attributes cannot be read, and
/// [`AttrError::Write`] for a refusal that is no attribute's
/// ([`Refusal::of`]) or any refusal under [`Attributes::Require`].
pub fn copy_attributes<S: Node + ?Sized, D: Node + ?Sized>(
    source: &S,
    destination: &D,
    mode: Attributes,
) -> Result<Vec<(String, Refusal)>, AttrError> {
    let mut left = Vec::new();
    if mode == Attributes::Skip {
        return Ok(left);
    }
    let keys = match source.attr_keys() {
        Ok(keys) => keys,
        // A volume that keeps none has none to carry.
        Err(Errno::NotSupported) => return Ok(left),
        Err(err) => return Err(AttrError::Read(err)),
    };
    for key in keys {
        let value = match source.attr_value(&key) {
            Ok(value) => value,
            Err(Errno::NoData) => continue,
            Err(err) => return Err(AttrError::Read(err)),
        };
        let Err(error) = destination.attr_set(&key, &value) else {
            continue;
        };
        match Refusal::of(error) {
            Some(refusal) if mode == Attributes::Carry => left.push((key, refusal)),
            _ => return Err(AttrError::Write { key, error }),
        }
    }
    Ok(left)
}

/// Most distinct keys a report names.
const REPORT_KEYS: usize = 8;

/// What a run of copies left behind, gathered into one report rather than a
/// line a file.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Omissions {
    files: u64,
    keys: BTreeSet<String>,
    /// More distinct keys were left than the report names.
    more: bool,
    refusals: BTreeSet<Refusal>,
}

impl Omissions {
    /// Note what one node's copy left behind ([`copy_attributes`]).
    pub fn note(&mut self, left: &[(String, Refusal)]) {
        if left.is_empty() {
            return;
        }
        self.files += 1;
        for (key, refusal) in left {
            self.refusals.insert(*refusal);
            self.name(key);
        }
    }

    /// Gather `other`'s omissions into these: a run made of runs.
    pub fn merge(&mut self, other: &Self) {
        self.files += other.files;
        self.more |= other.more;
        self.refusals.extend(other.refusals.iter().copied());
        for key in &other.keys {
            self.name(key);
        }
    }

    fn name(&mut self, key: &str) {
        if self.keys.contains(key) {
            return;
        }
        if self.keys.len() < REPORT_KEYS {
            self.keys.insert(String::from(key));
        } else {
            self.more = true;
        }
    }

    /// Whether every copy kept every attribute.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files == 0
    }

    /// The report of the omissions `producer` makes, `None` when nothing was
    /// left.
    #[must_use]
    pub fn report(&self, producer: &str) -> Option<Report> {
        if self.is_empty() {
            return None;
        }
        let files = if self.files == 1 {
            String::from("1 file")
        } else {
            format!("{} files", self.files)
        };
        // A key's grammar bans only `/` and NUL, so a volume's own can carry
        // control bytes, and a comma that would pass for the list's own.
        let mut keys = self
            .keys
            .iter()
            .map(|key| format!("{}", Quoted::new(key, Style::ShellEscape).marking(",")))
            .collect::<Vec<_>>()
            .join(", ");
        if self.more {
            keys.push_str(", and others");
        }
        let reasons = self
            .refusals
            .iter()
            .map(|refusal| refusal.reason())
            .collect::<Vec<_>>()
            .join("; ");
        let message = format!("Extended attributes not kept on {files}: {keys}.");
        let codes = self
            .refusals
            .iter()
            .map(|refusal| format!("{}", JsonStr(refusal.code())))
            .collect::<Vec<_>>()
            .join(",");
        let listed = self
            .keys
            .iter()
            .map(|key| format!("{}", JsonStr(key)))
            .collect::<Vec<_>>()
            .join(",");
        let ai = format!(
            "{{\"subject\":\"copy\",\"omission\":{{\"reasons\":[{codes}],\"files\":{},\
             \"keys\":[{listed}],\"keys_exhaustive\":{}}}}}",
            self.files, !self.more
        );
        let record = StdInfoRecord::new(
            producer,
            StdInfoKind::Omission,
            "fs.attributes_not_kept",
            Severity::Info,
            Human::message(&message),
        )
        .with_ai(&ai);
        // Framing escapes a string field to at most six bytes a byte and
        // embeds `ai` as it stands, so this holds the whole line.
        let mut line = alloc::vec![0u8; 512 + 6 * (producer.len() + message.len()) + ai.len()];
        match record.write_jsonl(&mut line) {
            Ok(len) => line.truncate(len),
            Err(_) => line.clear(),
        }
        Some(Report {
            summary: format!("extended attributes not kept on {files} ({reasons}): {keys}"),
            record: line,
        })
    }
}

/// What a run of copies tells its user it left.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Report {
    /// What was left, and why, as one clause the producer frames: a
    /// diagnostic's warning, a status line, an alert's detail.
    pub summary: String,
    /// The advisory `omission` record for the information stream: one framed
    /// JSONL line.
    pub record: Vec<u8>,
}

#[cfg(test)]
#[path = "attrs_tests.rs"]
mod tests;
