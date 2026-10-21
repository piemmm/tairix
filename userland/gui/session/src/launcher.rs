//! The session's side of the program library (`plans/NEW-TASKBAR.md` T5,
//! T22): what a launch from it asks for and counts as, what it shows running,
//! and the account's own record of it, kept off the loop.
//!
//! [`LauncherMemory`] is pure. The record lives in this session's private
//! app-data scope, a store away, so the session's private-store worker reads
//! and writes it ([`load_personal`], [`store_personal`]) and hands back what
//! came of it. This holds what the store last answered — the durable record —
//! and the edits it has not yet confirmed, replays them in order over it for
//! what the launcher shows — the live record — and says what to read or write
//! next. Nothing is stored before the record is read.

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
        window: None,
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

/// What reading the record produced: the record, or none when the store could
/// not be read, and a ready-to-print line for each record left out.
pub struct LoadedPersonal {
    /// The record, or `None` for a store that could not be read: never a
    /// stand-in an edit could be stored over.
    pub personal: Option<Personal>,
    /// One `stderr` line per refused record or unreadable store.
    pub warnings: Vec<String>,
}

/// Read the account's record out of this session's private scope.
pub fn load_personal(host: &mut dyn AppDataHost) -> LoadedPersonal {
    let store = SettingsStore::open_without_defaults(host);
    if let Some(err) = store.store_refusal() {
        return LoadedPersonal {
            personal: None,
            warnings: alloc::vec![alloc::format!(
                "desktop: the launcher's record could not be read ({err:?}); favourites and \
                 frequent use wait until it can\n"
            )],
        };
    }
    let settings = store.settings();
    let (personal, refused) =
        Personal::load(settings.iter().map(|setting| (setting.key, setting.value)));
    LoadedPersonal {
        personal: Some(personal),
        warnings: refused
            .into_iter()
            .map(|key| {
                alloc::format!("desktop: the launcher record `{key}` is not one kept; left out\n")
            })
            .collect(),
    }
}

/// Write `changes` — each a key and the value to keep, `None` to keep nothing
/// — into this session's private scope, and answer what the store then holds
/// for each.
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

/// The account's record, kept between the launcher and the store. See the
/// [module docs](self).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LauncherMemory {
    durable: Option<Personal>,
    live: Personal,
    unconfirmed: Vec<Edit>,
    in_flight: Option<InFlight>,
    asked: u64,
    reading: Reading,
    /// Whether the live record moved since the launcher was last told.
    moved: bool,
}

impl Default for LauncherMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl LauncherMemory {
    /// A record not yet read, its first read due at once.
    #[must_use]
    pub fn new() -> Self {
        Self {
            durable: None,
            live: Personal::new(),
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
    /// hand the launcher its shelf again.
    pub fn take_moved(&mut self) -> bool {
        core::mem::take(&mut self.moved)
    }

    /// Whether the record has been read.
    #[must_use]
    pub const fn is_read(&self) -> bool {
        self.durable.is_some()
    }

    /// What the launcher shows: the durable record with every unconfirmed
    /// edit replayed over it.
    #[must_use]
    pub const fn live(&self) -> &Personal {
        &self.live
    }

    /// The shelf the launcher draws at the wall-clock second `now`.
    #[must_use]
    pub fn shelf(&self, now: Option<i64>) -> Shelf {
        self.live.shelf(now)
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
    pub fn loaded(&mut self, now_ns: u64, personal: Option<Personal>) {
        self.reading.asked = false;
        if let Some(personal) = personal {
            self.durable = Some(personal);
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
    /// [`RecordFull`] for an edit the record has no room for, or one past
    /// [`MAX_UNCONFIRMED`] held edits; nothing changed.
    pub fn apply(&mut self, edit: Edit) -> Result<(), RecordFull> {
        if self.unconfirmed.len() >= MAX_UNCONFIRMED {
            return Err(RecordFull);
        }
        self.live.apply(&edit)?;
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
        let mut touched: Vec<EntryId> = Vec::new();
        for edit in &self.unconfirmed {
            if let Ok(changed) = replayed.apply(edit) {
                for id in changed {
                    if !touched.contains(&id) {
                        touched.push(id);
                    }
                }
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
                .iter()
                .map(|id| (key_of(id), replayed.stored(id)))
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
                let Some(id) = key
                    .strip_prefix(KEY_PREFIX)
                    .and_then(|rest| rest.strip_prefix('.'))
                    .and_then(|id| EntryId::new(id).ok())
                else {
                    continue;
                };
                if durable.adopt(&id, value.as_deref()).is_err() {
                    let _ = durable.adopt(&id, None);
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
        self.unconfirmed.retain(|edit| live.apply(edit).is_ok());
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

#[cfg(test)]
#[path = "launcher_tests.rs"]
mod tests;
