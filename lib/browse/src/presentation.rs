//! How a browser presents what it lists: the endings names are shown without,
//! whether hidden items are listed, and which entries are pictured as
//! themselves rather than as their kind.
//!
//! The engine presents every entry exactly ([`Presentation::EXACT`]), which is
//! what the trusted picker shows, since it is where an application is granted
//! a file; the file manager and the desktop present by their user's own
//! settings. Presentation decides only what is *shown*: every operation still
//! names an entry by the name the volume holds, and no ending is hidden that
//! would leave a name reading as another type or application.

use tairix_abi::appinfo::{bundle_stem, is_bundle_name};

use crate::entry::{Entry, EntryKind};
use crate::media::{media_for_name, Ending};

/// The endings a name is shown without.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct HiddenEndings {
    /// An application bundle's `.app`.
    pub applications: bool,
    /// The ending of a file of a type the content-type registry knows.
    pub known_types: bool,
}

impl HiddenEndings {
    /// No ending hidden: every name shown as the volume holds it.
    pub const NONE: Self = Self {
        applications: false,
        known_types: false,
    };

    /// The name `entry` is shown as: the label it is shown by, where it has
    /// one ([`Entry::label`]), else its name less the ending these hide.
    ///
    /// A folder's name is shown whole, since a folder has no type an ending
    /// names; so is a name that would be shown empty, and one whose ending
    /// would leave a typed name or a bundle's — `invoice.pdf.app` is never
    /// shown as `invoice.pdf`.
    #[must_use]
    pub fn shown(self, entry: &Entry) -> &str {
        entry
            .label()
            .unwrap_or_else(|| self.shown_name(entry.name(), entry.kind()))
    }

    /// The name `name`, listed as an entry of `kind`, is shown as, by
    /// [`shown`](Self::shown)'s rule.
    #[must_use]
    pub fn shown_name(self, name: &str, kind: EntryKind) -> &str {
        name.get(..name.len() - self.hidden_len(name, kind))
            .unwrap_or(name)
    }

    /// The ending [`shown`](Self::shown) leaves off `entry`'s name, its dot or
    /// comma included; empty when it leaves nothing off.
    #[must_use]
    pub fn hidden(self, entry: &Entry) -> &str {
        let name = entry.name();
        name.get(name.len() - self.hidden_len(name, entry.kind())..)
            .unwrap_or_default()
    }

    /// How many bytes at the end of `name`, an entry of `kind`'s, these leave
    /// off: none where what would be left reads as a type or an application of
    /// its own, so a hidden ending never disguises what an entry is.
    fn hidden_len(self, name: &str, kind: EntryKind) -> usize {
        let left = match kind.resolved() {
            Some(EntryKind::Bundle) if self.applications => bundle_stem(name),
            Some(EntryKind::File) if self.known_types && media_for_name(name).is_some() => {
                Some(Ending::of(name).stem)
            }
            _ => None,
        };
        left.filter(|stem| media_for_name(stem).is_none() && !is_bundle_name(stem))
            .map_or(0, |stem| name.len() - stem.len())
    }
}

/// Which entries are pictured as themselves rather than as their kind.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Pictures {
    /// A picture file is pictured by its own content.
    pub thumbnails: bool,
    /// A folder's icon shows the kinds of file inside it.
    pub folder_contents: bool,
}

impl Pictures {
    /// Every entry that can be pictured as itself is.
    pub const ALL: Self = Self {
        thumbnails: true,
        folder_contents: true,
    };
}

impl Default for Pictures {
    fn default() -> Self {
        Self::ALL
    }
}

/// How a browser presents the entries it lists.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Presentation {
    /// The endings names are shown without.
    pub endings: HiddenEndings,
    /// Whether an entry whose name marks it a hidden item
    /// ([`tairix_path::is_hidden_name`]) is listed.
    pub hidden_items: bool,
    /// Which entries are pictured as themselves.
    pub pictures: Pictures,
}

impl Presentation {
    /// Every entry listed, named as the volume holds it, and pictured as
    /// itself wherever it can be.
    pub const EXACT: Self = Self {
        endings: HiddenEndings::NONE,
        hidden_items: true,
        pictures: Pictures::ALL,
    };

    /// Whether an entry named `name` is listed.
    #[must_use]
    pub fn lists(&self, name: &str) -> bool {
        self.hidden_items || !tairix_path::is_hidden_name(name)
    }
}

impl Default for Presentation {
    fn default() -> Self {
        Self::EXACT
    }
}

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;
