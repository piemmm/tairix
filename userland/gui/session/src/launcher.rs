//! The session's side of the program library (`plans/NEW-TASKBAR.md` T5,
//! T22): what a launch from it asks for and counts as, what it shows running,
//! and the account's own record of it, kept off the loop.
//!
//! [`RecordMemory`] is pure, and holds any record this session keeps in its
//! private app-data scope ([`Kept`]): the launcher's, and where the user put
//! their desktop icons ([`crate::pinboard`]). The record is a store away, so
//! the session's private-store worker reads and writes it ([`load_kept`],
//! [`store_personal`]) and hands back what came of it. The memory holds what
//! the store last answered — the durable record — and the edits it has not yet
//! confirmed, replays them in order over it for what is shown — the live
//! record — and says what to read or write next. Nothing is stored before the
//! record is read.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_appdata::{AppDataHost, Settings as SettingsStore};
use tairix_proglib::personal::{key_of, FORGOTTEN, KEY_PREFIX};
use tairix_proglib::{Catalog, Edit, EntryId, LibraryEntry, Personal, RecordFull, Shelf};
use tairix_taskbar::AppIdentity;

use crate::effects::{RETRY_MAX_NS, RETRY_NS};
use crate::elevate::{Elevation, Request as AuthoriseRequest};
use crate::launch::LaunchTable;
use crate::AppGroup;

/// What running a program as an administrator says, after the program's
/// name.
pub const RUN_ELEVATED_PURPOSE: &str = "wants to run as an administrator.";

/// The request the console's broker is asked to authorise to run `entry` as
/// an administrator: its bundle's entry-point binary, pictured by that bundle
/// and named as `attested` — what the bundle's signed manifest says it is.
///
/// Never by the entry's own name: the account's library overlay may name an
/// entry anything and point it at any installed bundle, and the one asked to
/// allow the run must be told what will run.
#[must_use]
pub fn elevated_launch(entry: &LibraryEntry, attested: &AppIdentity) -> AuthoriseRequest {
    let bundle = String::from(entry.bundle().as_str());
    AuthoriseRequest {
        program: tairix_appstore::entry_path(&bundle),
        elevation: Elevation::Launch,
        requester: attested.name.clone(),
        bundle: Some(bundle),
        purpose: String::from(RUN_ELEVATED_PURPOSE),
        asker: None,
    }
}

/// The catalogued entries the launches of `run_paths` started, in the order
/// launched: every entry naming the bundle each starts, and none for a
/// launch of nothing catalogued.
#[must_use]
pub fn launched_entries(catalog: &Catalog, run_paths: &[String]) -> Vec<EntryId> {
    run_paths
        .iter()
        .filter_map(|run_path| tairix_appstore::bundle_of_entry(run_path))
        .flat_map(|bundle| {
            catalog
                .entries()
                .filter(move |entry| entry.bundle().as_str() == bundle)
                .map(|entry| entry.id().clone())
        })
        .collect()
}

/// The bundle directories running: each a slot on the strip is attested to,
/// and each a child the desktop launched still runs from.
#[must_use]
pub fn running_bundles(strip: &[AppGroup], launched: &LaunchTable) -> BTreeSet<String> {
    strip
        .iter()
        .filter_map(|group| group.bundle.as_deref())
        .chain(launched.bundles())
        .map(String::from)
        .collect()
}

/// The most edits held unconfirmed at once.
///
/// A containment bound on what a store that will not answer can make the
/// session keep, far past any burst a person makes; an edit past it is
/// refused, and the caller says so.
pub const MAX_UNCONFIRMED: usize = 64;

/// A record this session keeps in its private scope, one store key per item:
/// what a [`RecordMemory`] asks of it.
pub trait Kept: Clone + Default + Eq + core::fmt::Debug {
    /// One change the record takes.
    type Edit: Clone + Eq + core::fmt::Debug;
    /// Why the record had no room for an edit.
    type Full: Copy + Eq + core::fmt::Debug;
    /// The refusal of an edit past [`MAX_UNCONFIRMED`] held ones.
    const OVERFULL: Self::Full;

    /// Read the record out of a scope's effective settings, ignoring keys
    /// outside it and leaving out every item it refuses, answered by key.
    fn read<'a>(settings: impl Iterator<Item = (&'a str, &'a str)>) -> (Self, Vec<String>);

    /// Make `edit`, answering the store keys whose value it changed.
    ///
    /// # Errors
    ///
    /// The record's refusal of an edit it has no room for; nothing changed.
    fn make(&mut self, edit: &Self::Edit) -> Result<Vec<String>, Self::Full>;

    /// What the store is to keep under `key`, `None` to keep nothing.
    fn kept(&self, key: &str) -> Option<String>;

    /// Take what the store answered under `key`, answering `false` for a value
    /// the record cannot read. A key outside the record changes nothing.
    fn take(&mut self, key: &str, value: Option<&str>) -> bool;
}

impl Kept for Personal {
    type Edit = Edit;
    type Full = RecordFull;
    const OVERFULL: RecordFull = RecordFull;

    fn read<'a>(settings: impl Iterator<Item = (&'a str, &'a str)>) -> (Self, Vec<String>) {
        Self::load(settings)
    }

    fn make(&mut self, edit: &Edit) -> Result<Vec<String>, RecordFull> {
        Ok(self.apply(edit)?.iter().map(key_of).collect())
    }

    fn kept(&self, key: &str) -> Option<String> {
        self.stored(&entry_keyed(key)?)
    }

    fn take(&mut self, key: &str, value: Option<&str>) -> bool {
        entry_keyed(key).is_none_or(|id| self.adopt(&id, value).is_ok())
    }
}

/// The program the launcher record's `key` names, `None` for a key outside it.
fn entry_keyed(key: &str) -> Option<EntryId> {
    key.strip_prefix(KEY_PREFIX)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|id| EntryId::new(id).ok())
}

/// What reading the record produced: the record, or none when the store could
/// not be read, and a ready-to-print line for each record left out.
pub struct LoadedPersonal {
    /// The record, or `None` for a store that could not be read: never a
    /// stand-in an edit could be stored over.
    pub personal: Option<Personal>,
    /// One `stderr` line per refused record or unreadable store.
    pub warnings: Vec<String>,
}

/// Read the record `R` out of this session's private scope: the record, and
/// the key of each item it left out.
///
/// # Errors
///
/// The app-data service's refusal, which leaves no record to store over.
pub fn load_kept<R: Kept>(host: &mut dyn AppDataHost) -> Result<(R, Vec<String>), Errno> {
    let store = SettingsStore::open_without_defaults(host);
    if let Some(err) = store.store_refusal() {
        return Err(err);
    }
    let settings = store.settings();
    Ok(R::read(
        settings.iter().map(|setting| (setting.key, setting.value)),
    ))
}

/// Read the account's record out of this session's private scope.
pub fn load_personal(host: &mut dyn AppDataHost) -> LoadedPersonal {
    match load_kept::<Personal>(host) {
        Ok((personal, refused)) => LoadedPersonal {
            personal: Some(personal),
            warnings: refused
                .into_iter()
                .map(|key| {
                    alloc::format!(
                        "desktop: the launcher record `{key}` is not one kept; left out\n"
                    )
                })
                .collect(),
        },
        Err(err) => LoadedPersonal {
            personal: None,
            warnings: alloc::vec![alloc::format!(
                "desktop: the launcher's record could not be read ({err:?}); favourites and \
                 frequent use wait until it can\n"
            )],
        },
    }
}

/// Write `changes` — each a key and the value to keep, `None` to keep nothing
/// — into this session's private scope, and answer what the store then holds
/// for each: the one writer of every record kept there.
///
/// A removal that leaves a lower layer's record showing — machine policy
/// seeding a favourite — is masked with [`FORGOTTEN`], so what was removed
/// stays removed; a mask the store refuses leaves the answer saying what does
/// show.
///
/// # Errors
///
/// The app-data service's refusal; the edits are withdrawn, so none of them
/// lands with a later commit either.
pub fn store_personal(
    host: &mut dyn AppDataHost,
    changes: &[(String, Option<String>)],
) -> Result<Vec<(String, Option<String>)>, Errno> {
    let mut store = SettingsStore::open_without_defaults(host);
    if let Some(err) = store.store_refusal() {
        return Err(err);
    }
    for (key, value) in changes {
        match value {
            Some(value) => store.set(key, value).map_err(|_| Errno::OutOfRange)?,
            None => store.unset(key),
        }
    }
    if let Err(err) = store.commit() {
        let _ = store.abandon();
        return Err(err);
    }
    let mut masked = false;
    for (key, value) in changes {
        if value.is_none() && store.get(key).is_some_and(|held| held != FORGOTTEN) {
            masked |= store.set(key, FORGOTTEN).is_ok();
        }
    }
    if masked && store.commit().is_err() {
        let _ = store.abandon();
    }
    Ok(changes
        .iter()
        .map(|(key, _)| (key.clone(), store.get(key).map(String::from)))
        .collect())
}

/// A write the worker is to make: which ask it is, and what to keep.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreAsk {
    /// Which ask it is, in the order asked.
    pub number: u64,
    /// Each key and the value to keep, `None` to keep nothing.
    pub changes: Vec<(String, Option<String>)>,
}

/// The write in flight: its number, and how many of the unconfirmed edits it
/// carries.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct InFlight {
    number: u64,
    edits: usize,
}

/// When the record is next read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Reading {
    asked: bool,
    next_ns: u64,
    wait_ns: u64,
}

/// A record kept between its surface and the store. See the
/// [module docs](self).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordMemory<R: Kept> {
    durable: Option<R>,
    live: R,
    unconfirmed: Vec<R::Edit>,
    in_flight: Option<InFlight>,
    asked: u64,
    reading: Reading,
    /// Whether the live record moved since its surface was last told.
    moved: bool,
}

/// The account's record of its program library, kept between the launcher
/// and the store.
pub type LauncherMemory = RecordMemory<Personal>;

impl<R: Kept> Default for RecordMemory<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Kept> RecordMemory<R> {
    /// A record not yet read, its first read due at once.
    #[must_use]
    pub fn new() -> Self {
        Self {
            durable: None,
            live: R::default(),
            unconfirmed: Vec::new(),
            in_flight: None,
            asked: 0,
            reading: Reading {
                asked: false,
                next_ns: 0,
                wait_ns: RETRY_NS,
            },
            moved: false,
        }
    }

    /// Whether the live record moved since this was last asked — the cue to
    /// hand its surface the record again.
    pub fn take_moved(&mut self) -> bool {
        core::mem::take(&mut self.moved)
    }

    /// Whether the record has been read.
    #[must_use]
    pub const fn is_read(&self) -> bool {
        self.durable.is_some()
    }

    /// What the surface shows: the durable record with every unconfirmed
    /// edit replayed over it.
    #[must_use]
    pub const fn live(&self) -> &R {
        &self.live
    }

    /// Whether a read is due at `now_ns`; answering `true` marks it asked.
    pub fn next_load(&mut self, now_ns: u64) -> bool {
        if self.durable.is_some() || self.reading.asked || now_ns < self.reading.next_ns {
            return false;
        }
        self.reading.asked = true;
        true
    }

    /// Adopt what a read at `now_ns` produced: the record, or none — a read
    /// tried again after a wait that doubles each time, up to its bound.
    pub fn loaded(&mut self, now_ns: u64, record: Option<R>) {
        self.reading.asked = false;
        if let Some(record) = record {
            self.durable = Some(record);
            self.replay();
        } else {
            self.reading.next_ns = now_ns.saturating_add(self.reading.wait_ns);
            self.reading.wait_ns = self.reading.wait_ns.saturating_mul(2).min(RETRY_MAX_NS);
        }
    }

    /// Make `edit`, shown at once and kept once the store confirms it.
    ///
    /// # Errors
    ///
    /// The record's refusal of an edit it has no room for, or of one past
    /// [`MAX_UNCONFIRMED`] held edits; nothing changed.
    pub fn apply(&mut self, edit: R::Edit) -> Result<(), R::Full> {
        if self.unconfirmed.len() >= MAX_UNCONFIRMED {
            return Err(R::OVERFULL);
        }
        self.live.make(&edit)?;
        self.unconfirmed.push(edit);
        self.moved = true;
        Ok(())
    }

    /// The write owed now, if one is: never before the record is read, never
    /// while one is in flight, and carrying every edit made so far, so edits
    /// made meanwhile gather into the next.
    pub fn next_store(&mut self) -> Option<StoreAsk> {
        if self.in_flight.is_some() || self.unconfirmed.is_empty() {
            return None;
        }
        let mut replayed = self.durable.clone()?;
        let mut touched = BTreeSet::new();
        for edit in &self.unconfirmed {
            if let Ok(changed) = replayed.make(edit) {
                touched.extend(changed);
            }
        }
        self.asked = self.asked.wrapping_add(1);
        self.in_flight = Some(InFlight {
            number: self.asked,
            edits: self.unconfirmed.len(),
        });
        Some(StoreAsk {
            number: self.asked,
            changes: touched
                .into_iter()
                .map(|key| {
                    let value = replayed.kept(&key);
                    (key, value)
                })
                .collect(),
        })
    }

    /// The write numbered `number` could not be handed to the worker: nothing
    /// was asked, so its edits stay unconfirmed and the next write carries
    /// them.
    pub fn unasked(&mut self, number: u64) {
        if self.in_flight.is_some_and(|flight| flight.number == number) {
            self.in_flight = None;
        }
    }

    /// Adopt what the write numbered `number` came to: what the store holds
    /// for each key it carried, or its refusal — which drops the edits it
    /// carried, so what is shown is what the next start restores. Answers the
    /// keys whose stored value the record could not read.
    pub fn stored(
        &mut self,
        number: u64,
        result: Result<Vec<(String, Option<String>)>, Errno>,
    ) -> Vec<String> {
        let Some(flight) = self.in_flight.filter(|flight| flight.number == number) else {
            return Vec::new();
        };
        self.in_flight = None;
        let carried = flight.edits.min(self.unconfirmed.len());
        self.unconfirmed.drain(..carried);
        let mut unreadable = Vec::new();
        if let (Ok(answers), Some(durable)) = (result, self.durable.as_mut()) {
            for (key, value) in answers {
                if !durable.take(&key, value.as_deref()) {
                    durable.take(&key, None);
                    unreadable.push(key);
                }
            }
        }
        self.replay();
        unreadable
    }

    /// Rebuild the live record: the durable one, or nothing before a read,
    /// with every unconfirmed edit replayed in order. An edit the record no
    /// longer has room for is dropped.
    fn replay(&mut self) {
        let mut live = self.durable.clone().unwrap_or_default();
        self.unconfirmed.retain(|edit| live.make(edit).is_ok());
        self.moved |= live != self.live;
        self.live = live;
    }

    /// Whether any edit is still unconfirmed by the store.
    #[must_use]
    pub fn holds_unconfirmed(&self) -> bool {
        !self.unconfirmed.is_empty()
    }

    /// Whether a write is in flight, or one is owed and could be made now.
    #[must_use]
    pub fn writing(&self) -> bool {
        self.in_flight.is_some() || (self.durable.is_some() && !self.unconfirmed.is_empty())
    }

    /// `park_ns` shortened to when the next read is due, while one is owed.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        if self.durable.is_some() || self.reading.asked {
            return park_ns;
        }
        park_ns.min(self.reading.next_ns.saturating_sub(now_ns))
    }
}

impl RecordMemory<Personal> {
    /// The shelf the launcher draws at the wall-clock second `now`.
    #[must_use]
    pub fn shelf(&self, now: Option<i64>) -> Shelf {
        self.live.shelf(now)
    }
}

#[cfg(test)]
#[path = "launcher_tests.rs"]
mod tests;
