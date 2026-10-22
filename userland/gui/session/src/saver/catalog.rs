//! Which screensavers the desktop offers: its own two, Blank and Dim, then
//! every saver bundle of the system store and of the user's own, read from
//! its signed manifest through the shared store walk and judged once.
//!
//! A saver is offered only where its manifest is whole and its settings can
//! be kept, and a saver of the user's own only where it asks for no feed: the
//! machine's readings, keeping pictures and the shipped pictures are given to
//! the system's savers alone. Every other saver is still listed, with why it
//! cannot be chosen, so the user sees the bundle they installed and what is
//! wrong with it.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::appinfo::{AppInfoHeader, ProgramKind};
use tairix_abi::saver::{SaverFeeds, SaverOption};
use tairix_abi::window_ipc::{SaverOrigin, SaverStanding, ScreensaverEntry};
use tairix_abi::{read_saver_options, BundleId};
use tairix_appstore::{StoreReader, Verdict, WalkError};
use tairix_log::{log, Event, EventId, Field, FieldValue, Level, Sink};
use tairix_wallpaper::{keeps_values_of, standing_reason, ScreensaverChoice, SAVER_PREVIEW_PATH};

/// Event id of a saver bundle the desktop lists but will not run, in the
/// desktop session's reserved range: refusing what the user installed is a
/// decision they must be able to find.
pub const SAVER_REFUSED: EventId = EventId(20_031);

/// The exact message [`SAVER_REFUSED`] is emitted with.
pub const SAVER_REFUSED_MESSAGE: &str = "screensaver cannot be chosen";

/// One saver bundle the walk found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Saver {
    /// Its signed identifier, as its manifest spells it.
    pub choice: String,
    /// That identifier, where it is one.
    pub id: Option<BundleId>,
    /// Its bundle directory.
    pub bundle: String,
    /// Which store it is in.
    pub origin: SaverOrigin,
    /// What a chooser shows.
    pub title: String,
    /// Its one-line purpose; empty when it states none.
    pub purpose: String,
    /// The privileged feeds it declares.
    pub feeds: SaverFeeds,
    /// Whether it runs a thread per core.
    pub parallel: bool,
    /// Whether it asks for the large memory ceiling.
    pub large_memory: bool,
    /// The options it declares, in its manifest's order.
    pub options: Vec<SaverOption>,
    /// Whether it can be chosen.
    pub standing: SaverStanding,
}

impl Saver {
    /// Where its preview picture is.
    #[must_use]
    pub fn preview_path(&self) -> String {
        format!("{}/{SAVER_PREVIEW_PATH}", self.bundle)
    }

    /// Record through `sink` that it cannot be chosen, and why.
    pub fn log_refused<S: Sink + ?Sized>(&self, sink: &S) {
        log(
            sink,
            &Event {
                level: Level::Warn,
                id: SAVER_REFUSED,
                message: SAVER_REFUSED_MESSAGE,
                fields: &[
                    Field {
                        key: "saver",
                        value: FieldValue::Str(&self.choice),
                    },
                    Field {
                        key: "reason",
                        value: FieldValue::Str(standing_reason(self.standing)),
                    },
                ],
            },
        );
    }
}

/// The screensavers the desktop offers, its own two first.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SaverCatalog {
    savers: Vec<Saver>,
}

/// The catalogue's place of the desktop's own two.
const DESKTOP: [(ScreensaverChoice, &str, &str); 2] = [
    (ScreensaverChoice::Blank, "Black", "A black screen"),
    (
        ScreensaverChoice::Dim,
        "Dimmed desktop",
        "The desktop's own backdrop, dimmed, with no window on it",
    ),
];

impl SaverCatalog {
    /// A catalogue of the desktop's own two and `savers`.
    #[must_use]
    pub const fn of(savers: Vec<Saver>) -> Self {
        Self { savers }
    }

    /// How many screensavers it lists, the desktop's own two among them.
    #[must_use]
    pub fn total(&self) -> u16 {
        u16::try_from(DESKTOP.len() + self.savers.len()).unwrap_or(u16::MAX)
    }

    /// Entry `index`, with the options it declares; `None` past the last.
    #[must_use]
    pub fn entry(&self, index: u16) -> Option<(ScreensaverEntry<'_>, &[SaverOption])> {
        let index = usize::from(index);
        if let Some((choice, title, purpose)) = DESKTOP.get(index) {
            let entry = ScreensaverEntry::new(
                (choice.as_str(), title, purpose),
                SaverOrigin::Desktop,
                SaverStanding::Offered,
                SaverFeeds::NONE,
            );
            return Some((entry, &[]));
        }
        let saver = self.savers.get(index - DESKTOP.len())?;
        let entry = ScreensaverEntry::new(
            (&saver.choice, &saver.title, &saver.purpose),
            saver.origin,
            saver.standing,
            saver.feeds,
        );
        Some((entry, &saver.options))
    }

    /// Where entry `index`'s preview picture is: the desktop's own two's in
    /// the system's graphics, a saver's in its bundle.
    #[must_use]
    pub fn preview_path(&self, index: u16) -> Option<String> {
        let index = usize::from(index);
        match DESKTOP.get(index) {
            Some((choice, _, _)) => choice.preview_path(),
            None => self
                .savers
                .get(index - DESKTOP.len())
                .map(Saver::preview_path),
        }
    }

    /// The saver `choice` names, if it can be chosen.
    #[must_use]
    pub fn offered(&self, choice: &ScreensaverChoice) -> Option<&Saver> {
        let ScreensaverChoice::Saver(id) = choice else {
            return None;
        };
        self.savers
            .iter()
            .find(|saver| saver.standing == SaverStanding::Offered && saver.id.as_ref() == Some(id))
    }

    /// The savers it lists that cannot be chosen.
    pub fn refused(&self) -> impl Iterator<Item = &Saver> {
        self.savers
            .iter()
            .filter(|saver| saver.standing != SaverStanding::Offered)
    }

    /// The savers it refuses that `before` did not refuse as they stand now:
    /// what a change to the stores has newly refused, which is all that is
    /// worth saying again.
    pub fn newly_refused<'a>(&'a self, before: &'a Self) -> impl Iterator<Item = &'a Saver> {
        let was: BTreeMap<&str, &Saver> = before
            .refused()
            .map(|saver| (saver.bundle.as_str(), saver))
            .collect();
        self.refused()
            .filter(move |saver| was.get(saver.bundle.as_str()) != Some(saver))
    }
}

/// Walk the screensaver stores for every saver bundle: the system's, then the
/// account's own, each walked alone, so the system's claim every identifier
/// they hold first however deep the account files its own, and an account's
/// store that cannot be read leaves the system's savers standing.
///
/// Answers the savers found, and why a store could not be read where one
/// could not: a system store that cannot be read leaves none.
pub fn load_savers<R: StoreReader + ?Sized>(
    reader: &R,
    home: Option<&str>,
) -> (SaverCatalog, Option<WalkError>) {
    let roots = tairix_appstore::saver_roots(home);
    let mut savers: Vec<Saver> = Vec::new();
    let mut claimed: BTreeSet<BundleId> = BTreeSet::new();
    for (index, root) in roots.iter().enumerate() {
        let origin = if index == 0 {
            SaverOrigin::System
        } else {
            SaverOrigin::User
        };
        let walked = tairix_appstore::walk(reader, core::slice::from_ref(root), |bundle| {
            let mut saver = judge(bundle.path, origin, bundle.header, bundle.manifest);
            if saver.standing == SaverStanding::Offered {
                if let Some(id) = saver.id {
                    if !claimed.insert(id) {
                        saver.standing = SaverStanding::Duplicate;
                    }
                }
            }
            savers.push(saver);
            Verdict::Accepted
        });
        if let Err(error) = walked {
            if origin == SaverOrigin::System {
                return (SaverCatalog::default(), Some(error));
            }
            return (SaverCatalog::of(savers), Some(error));
        }
    }
    (SaverCatalog::of(savers), None)
}

/// The saver the bundle at `path` declares itself, and whether it can be
/// chosen.
fn judge(path: &str, origin: SaverOrigin, header: &AppInfoHeader, manifest: &[u8]) -> Saver {
    let body = manifest.get(AppInfoHeader::WIRE_LEN..).unwrap_or(&[]);
    let mut options: Vec<SaverOption> = Vec::new();
    let read = read_saver_options(body, header, |option| options.push(option));
    let id = BundleId::new(header.bundle_id()).ok();
    let feeds = header.saver_feeds();
    let whole = header.program_kind() == Some(ProgramKind::Screensaver) && read.is_ok();
    let standing = match &id {
        None => SaverStanding::Damaged,
        Some(_) if !whole => SaverStanding::Damaged,
        Some(_) if origin == SaverOrigin::User && !feeds.is_empty() => SaverStanding::AsksForAFeed,
        Some(id) if !keeps_values_of(id) => SaverStanding::Unkeyable,
        Some(_) => SaverStanding::Offered,
    };
    Saver {
        choice: header.bundle_id().to_string(),
        id,
        bundle: path.to_string(),
        origin,
        title: header.bundle_title().to_string(),
        purpose: header.bundle_purpose().unwrap_or("").to_string(),
        feeds,
        parallel: header.runs_parallel(),
        large_memory: header.needs_large_memory(),
        options,
        standing,
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
