//! What the launcher's content pane lists: the sections of a page, or of a
//! search's results.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_proglib::{Catalog, EntryId, LibraryCategory, LibraryEntry, Shelf};
use tairix_util::utf8::reader_order;

use super::folder_label;
use super::search::{Hit, Indexed};

/// A page the launcher browses.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LibraryPage {
    /// The favourites, the frequently used, then every program.
    Home,
    /// One folder's programs.
    Folder(LibraryCategory),
}

/// What a section of the content pane holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SectionKind {
    /// The account's favourites, in its own order.
    Favourites,
    /// The frequently used that are not favourites, most used first: one row
    /// of them, as many as the pane is wide.
    Frequent,
    /// Every program, by name.
    All,
    /// One folder's programs, by name.
    Folder(LibraryCategory),
    /// What a search found, best first, in every folder or in one.
    Results(Option<LibraryCategory>),
}

impl SectionKind {
    /// The heading the section is introduced by.
    #[must_use]
    pub fn heading(self) -> String {
        match self {
            Self::Favourites => String::from("Favourites"),
            Self::Frequent => String::from("Frequently Used"),
            Self::All => String::from("All Programs"),
            Self::Folder(category) => String::from(folder_label(category)),
            Self::Results(None) => String::from("Results"),
            Self::Results(Some(category)) => {
                alloc::format!("Results in {}", folder_label(category))
            }
        }
    }

    /// Whether the section is a list of two-line rows rather than a grid of
    /// tiles: a search's results, where what each program is for is worth a
    /// line of its own.
    #[must_use]
    pub const fn is_list(self) -> bool {
        matches!(self, Self::Results(_))
    }
}

/// One program as a section shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryItem {
    /// The catalog entry it launches.
    pub id: EntryId,
    /// Its display name.
    pub name: String,
    /// The bytes of the name a search matched.
    pub emphasis: Vec<Range<usize>>,
}

impl LibraryItem {
    fn of(entry: &LibraryEntry) -> Self {
        Self {
            id: entry.id().clone(),
            name: String::from(entry.name().as_str()),
            emphasis: Vec::new(),
        }
    }
}

/// One run of programs under a heading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Section {
    /// What the section holds.
    pub kind: SectionKind,
    /// Its programs, in the order shown.
    pub items: Vec<LibraryItem>,
}

/// `entries` in the order a reader looks a name up in.
fn by_name<'a>(entries: impl Iterator<Item = &'a LibraryEntry>) -> Vec<LibraryItem> {
    let mut sorted: Vec<&LibraryEntry> = entries.collect();
    sorted.sort_by(|a, b| {
        reader_order(a.name().as_str(), b.name().as_str()).then_with(|| a.id().cmp(b.id()))
    });
    sorted.into_iter().map(LibraryItem::of).collect()
}

/// The entries of `catalog` that `ids` names, in that order: an entry the
/// account keeps but the catalog no longer lists is not shown.
fn listed(catalog: &Catalog, ids: &[EntryId]) -> Vec<LibraryItem> {
    ids.iter()
        .filter_map(|id| catalog.entry(id))
        .map(LibraryItem::of)
        .collect()
}

/// The sections `page` shows over `catalog` for an account whose shelf is
/// `shelf`. A section with nothing in it is left out, and so is every section
/// of an empty catalog.
pub(crate) fn page(catalog: &Catalog, shelf: &Shelf, page: LibraryPage) -> Vec<Section> {
    let sections = match page {
        LibraryPage::Home => alloc::vec![
            Section {
                kind: SectionKind::Favourites,
                items: listed(catalog, &shelf.favourites),
            },
            Section {
                kind: SectionKind::Frequent,
                items: listed(catalog, &shelf.frequent),
            },
            Section {
                kind: SectionKind::All,
                items: by_name(catalog.entries()),
            },
        ],
        LibraryPage::Folder(category) => alloc::vec![Section {
            kind: SectionKind::Folder(category),
            items: by_name(
                catalog
                    .entries()
                    .filter(|entry| entry.category() == category)
            ),
        }],
    };
    sections
        .into_iter()
        .filter(|section| !section.items.is_empty())
        .collect()
}

/// The one section a search's `hits` over `index` show, in `scope` — every
/// folder, or one — or none when nothing in it matched.
pub(crate) fn results(
    catalog: &Catalog,
    index: &[Indexed],
    hits: &[Hit],
    scope: Option<LibraryCategory>,
) -> Vec<Section> {
    let items: Vec<LibraryItem> = hits
        .iter()
        .filter_map(|hit| {
            let program = index.get(hit.index)?;
            if scope.is_some_and(|scope| scope != program.category()) {
                return None;
            }
            let entry = catalog.entry(program.id())?;
            Some(LibraryItem {
                emphasis: hit.emphasis.clone(),
                ..LibraryItem::of(entry)
            })
        })
        .collect();
    if items.is_empty() {
        return Vec::new();
    }
    alloc::vec![Section {
        kind: SectionKind::Results(scope),
        items,
    }]
}
