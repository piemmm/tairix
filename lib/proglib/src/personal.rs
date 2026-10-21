//! The account's own record of its program library: the favourites it keeps,
//! in its own order, and how often and how lately it launches each program.
//!
//! [`Personal`] is what the launcher puts first. It is kept in the desktop's
//! **private** app-data scope, one `library.<entry-id>` key per program
//! ([`key_of`]), because what a user runs is theirs: the published scope would
//! hand it to every application they launch. This module is the one
//! definition of that record — its token grammar, the integer decay that ranks
//! it, and the bounded [`Edit`]s that change it — and performs no I/O.
//!
//! A stored record is untrusted input like every document: [`Personal::load`]
//! leaves out, and names, any record it does not fully understand, keeping the
//! rest, since one bad record is one program out of place rather than a reason
//! to forget the others.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::entry::EntryId;

/// The key segment every record of the account's library is stored under.
pub const KEY_PREFIX: &str = "library";

/// The value a removal stores where a lower layer of the scope — machine
/// policy — would otherwise show its own record through: the program is kept
/// for nothing.
pub const FORGOTTEN: &str = "none";

/// How long a launch takes to count for half as much, in seconds: two weeks,
/// so a program used daily outranks one used often a month ago, and one tried
/// once leaves the frequently used within a fortnight unless used again.
pub const HALF_LIFE_SECS: u64 = 14 * 24 * 60 * 60;

/// What one launch weighs, in thousandths of a launch: integer weights rank
/// alike on every target.
pub const LAUNCH_WEIGHT: u32 = 1_000;

/// The heaviest a record may grow — a thousand launches' worth, far past where
/// ranking stops changing — so no arithmetic on it can overflow.
pub const MAX_WEIGHT: u32 = 1_000 * LAUNCH_WEIGHT;

/// The least a program must weigh now to count as frequently used: half a
/// launch.
pub const FREQUENT_FLOOR: u32 = LAUNCH_WEIGHT / 2;

/// The most records an account keeps.
///
/// A containment bound on what one account may grow in a scope it shares with
/// the desktop's other settings, not a capacity: twice what a catalog can list,
/// so programs since removed have room to fade out on their own.
pub const MAX_RECORDS: usize = 2 * crate::MAX_ENTRIES;

const FAVOURITE: &str = "favourite";
const WEIGHT: &str = "weight";
const LAST: &str = "last";

/// How much, and how lately, one program has been used.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Use {
    /// In thousandths of a launch, as of [`last`](Self::last).
    weight: u32,
    /// The wall-clock second of the last launch.
    last: i64,
}

impl Use {
    /// What this use weighs at the wall-clock second `now`, or as last stored
    /// when the clock could not be read. A clock behind the last launch decays
    /// nothing.
    #[must_use]
    pub fn weight_at(self, now: Option<i64>) -> u32 {
        match now {
            Some(now) => decayed(self.weight, elapsed(self.last, now)),
            None => self.weight,
        }
    }

    /// The wall-clock second of the last launch.
    #[must_use]
    pub const fn last(self) -> i64 {
        self.last
    }
}

/// `weight` after `elapsed` seconds: halved every [`HALF_LIFE_SECS`], and
/// interpolated linearly within one, so the decay is monotone and continuous
/// at each halving.
#[must_use]
pub fn decayed(weight: u32, elapsed: u64) -> u32 {
    let halvings = elapsed / HALF_LIFE_SECS;
    if halvings >= u64::from(u32::BITS) {
        return 0;
    }
    let whole = weight >> halvings;
    let part = elapsed % HALF_LIFE_SECS;
    let lost = u64::from(whole) * part / (2 * HALF_LIFE_SECS);
    whole.saturating_sub(u32::try_from(lost).unwrap_or(whole))
}

/// Seconds from `last` to `now`, nothing for a clock behind it.
fn elapsed(last: i64, now: i64) -> u64 {
    u64::try_from(i128::from(now) - i128::from(last)).unwrap_or(0)
}

/// What the account keeps about one program.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Keep {
    favourite: Option<u16>,
    used: Option<Use>,
}

impl Keep {
    /// The program's place among the favourites, if it is one.
    #[must_use]
    pub const fn favourite(self) -> Option<u16> {
        self.favourite
    }

    /// How the program has been used, if it has.
    #[must_use]
    pub const fn used(self) -> Option<Use> {
        self.used
    }

    /// Whether there is anything to keep at all.
    const fn is_empty(self) -> bool {
        self.favourite.is_none() && self.used.is_none()
    }

    /// The canonical stored spelling.
    fn render(self) -> String {
        let mut out = String::new();
        if let Some(rank) = self.favourite {
            let _ = write!(out, "{FAVOURITE}={rank}");
        }
        if let Some(Use { weight, last }) = self.used {
            if !out.is_empty() {
                out.push(' ');
            }
            let _ = write!(out, "{WEIGHT}={weight} {LAST}={last}");
        }
        if out.is_empty() {
            out.push_str(FORGOTTEN);
        }
        out
    }

    /// Read a stored value, refusing anything outside the canonical grammar:
    /// each of `favourite=`, `weight=`, `last=` at most once, single spaces
    /// between, `weight` and `last` together, or [`FORGOTTEN`] alone.
    fn parse(value: &str) -> Option<Self> {
        if value == FORGOTTEN {
            return Some(Self::default());
        }
        let (mut favourite, mut weight, mut last) = (None, None, None);
        for token in value.split(' ') {
            let (name, number) = token.split_once('=')?;
            let slot = match name {
                FAVOURITE => &mut favourite,
                WEIGHT => &mut weight,
                LAST => &mut last,
                _ => return None,
            };
            if slot.replace(number).is_some() {
                return None;
            }
        }
        let favourite = match favourite {
            Some(rank) => Some(canonical::<u16>(rank).filter(|&rank| rank > 0)?),
            None => None,
        };
        let used = match (weight, last) {
            (Some(weight), Some(last)) => Some(Use {
                weight: canonical::<u32>(weight).filter(|w| (1..=MAX_WEIGHT).contains(w))?,
                last: canonical::<i64>(last)?,
            }),
            (None, None) => None,
            _ => return None,
        };
        let keep = Self { favourite, used };
        (!keep.is_empty()).then_some(keep)
    }
}

/// `text` as a number only when it is the number's own spelling, so a stored
/// value has exactly one form (no `+`, no leading zeros).
fn canonical<T: core::str::FromStr + core::fmt::Display>(text: &str) -> Option<T> {
    let number: T = text.parse().ok()?;
    let mut spelled = String::new();
    let _ = write!(spelled, "{number}");
    (spelled == text).then_some(number)
}

/// The key `id`'s record is stored under: `library.<id>`.
#[must_use]
pub fn key_of(id: &EntryId) -> String {
    let mut key = String::from(KEY_PREFIX);
    key.push('.');
    key.push_str(id.as_str());
    key
}

/// The entry a stored key names, `None` for a key outside the record, and
/// `Some(Err)` for one inside it naming no valid entry.
fn entry_of_key(key: &str) -> Option<Result<EntryId, ()>> {
    let id = key.strip_prefix(KEY_PREFIX)?.strip_prefix('.')?;
    Some(EntryId::new(id).map_err(|_| ()))
}

/// One change the account makes to its record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Edit {
    /// The program was launched at this wall-clock second, or at a moment the
    /// clock could not say.
    Launched(EntryId, Option<i64>),
    /// Keep the program among the favourites, after the others.
    Favourite(EntryId),
    /// Stop keeping the program among the favourites.
    Unfavourite(EntryId),
    /// Move a favourite one place earlier.
    MoveEarlier(EntryId),
    /// Move a favourite one place later.
    MoveLater(EntryId),
    /// Forget how the program has been used.
    Forget(EntryId),
    /// Forget how every program has been used.
    ForgetAll,
}

/// An edit refused because it would keep more than [`MAX_RECORDS`] records
/// and no record could make room: every one is a favourite.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RecordFull;

/// A stored record refused because its value is outside the record's grammar.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RecordRefused;

/// What the launcher draws first, ranked once at one moment.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Shelf {
    /// The favourites, in the account's own order.
    pub favourites: Vec<EntryId>,
    /// The frequently used that are not favourites, most used first.
    pub frequent: Vec<EntryId>,
    /// What every used program weighs now, for ranking among equals.
    pub weights: BTreeMap<EntryId, u32>,
}

/// The account's record of its program library. See the [module docs](self).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Personal {
    keeps: BTreeMap<EntryId, Keep>,
}

impl Personal {
    /// An account that keeps nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the record out of a scope's effective settings, ignoring keys
    /// outside it and leaving out every record it refuses — a key naming no
    /// valid entry, a value outside the grammar, or one past
    /// [`MAX_RECORDS`] — answered by key.
    pub fn load<'a>(settings: impl IntoIterator<Item = (&'a str, &'a str)>) -> (Self, Vec<String>) {
        let mut personal = Self::new();
        let mut refused = Vec::new();
        for (key, value) in settings {
            let Some(id) = entry_of_key(key) else {
                continue;
            };
            let (Ok(id), Some(keep)) = (id, Keep::parse(value)) else {
                refused.push(String::from(key));
                continue;
            };
            if keep.is_empty() {
                continue;
            }
            if personal.keeps.len() >= MAX_RECORDS {
                refused.push(String::from(key));
                continue;
            }
            personal.keeps.insert(id, keep);
        }
        (personal, refused)
    }

    /// Take what the store answers for `id`, `None` or [`FORGOTTEN`] for
    /// nothing, refusing a value outside the grammar and leaving the record as
    /// it was.
    ///
    /// # Errors
    ///
    /// [`RecordRefused`] for a value [`load`](Self::load) would refuse.
    pub fn adopt(&mut self, id: &EntryId, value: Option<&str>) -> Result<(), RecordRefused> {
        let keep = match value {
            None => Keep::default(),
            Some(value) => Keep::parse(value).ok_or(RecordRefused)?,
        };
        self.put(id, keep);
        Ok(())
    }

    /// The value `id`'s record is stored as, or `None` when there is nothing
    /// to keep and the key is to be unset.
    #[must_use]
    pub fn stored(&self, id: &EntryId) -> Option<String> {
        self.keeps.get(id).map(|keep| keep.render())
    }

    /// What the account keeps about `id`.
    #[must_use]
    pub fn keep(&self, id: &EntryId) -> Keep {
        self.keeps.get(id).copied().unwrap_or_default()
    }

    /// Every record, by entry.
    pub fn keeps(&self) -> impl Iterator<Item = (&EntryId, Keep)> {
        self.keeps.iter().map(|(id, keep)| (id, *keep))
    }

    /// How many records there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keeps.len()
    }

    /// Whether nothing is kept.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keeps.is_empty()
    }

    /// Apply `edit`, answering the entries whose stored record changed.
    ///
    /// # Errors
    ///
    /// [`RecordFull`] for an edit that needs a new record when every one of
    /// the [`MAX_RECORDS`] kept is a favourite; nothing changed.
    pub fn apply(&mut self, edit: &Edit) -> Result<Vec<EntryId>, RecordFull> {
        match edit {
            Edit::Launched(id, now) => self.launched(id, *now),
            Edit::Favourite(id) => {
                if self.keep(id).favourite.is_some() {
                    return Ok(Vec::new());
                }
                let rank = u16::try_from(self.favourite_count().saturating_add(1))
                    .map_err(|_| RecordFull)?;
                let mut changed: Vec<EntryId> = self.make_room(id, None)?.into_iter().collect();
                for renumbered in self.renumber() {
                    push_once(&mut changed, &renumbered);
                }
                let mut keep = self.keep(id);
                keep.favourite = Some(rank);
                self.put(id, keep);
                push_once(&mut changed, id);
                Ok(changed)
            }
            Edit::Unfavourite(id) => {
                let mut keep = self.keep(id);
                if keep.favourite.take().is_none() {
                    return Ok(Vec::new());
                }
                self.put(id, keep);
                Ok(alloc::vec![id.clone()])
            }
            Edit::MoveEarlier(id) => Ok(self.move_favourite(id, false)),
            Edit::MoveLater(id) => Ok(self.move_favourite(id, true)),
            Edit::Forget(id) => {
                let mut keep = self.keep(id);
                if keep.used.take().is_none() {
                    return Ok(Vec::new());
                }
                self.put(id, keep);
                Ok(alloc::vec![id.clone()])
            }
            Edit::ForgetAll => {
                let used: Vec<EntryId> = self
                    .keeps
                    .iter()
                    .filter(|(_, keep)| keep.used.is_some())
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in &used {
                    let mut keep = self.keep(id);
                    keep.used = None;
                    self.put(id, keep);
                }
                Ok(used)
            }
        }
    }

    /// What the launcher draws first at the wall-clock second `now`.
    #[must_use]
    pub fn shelf(&self, now: Option<i64>) -> Shelf {
        let favourites = self.favourites();
        let weights: BTreeMap<EntryId, u32> = self
            .keeps
            .iter()
            .filter_map(|(id, keep)| Some((id.clone(), keep.used?.weight_at(now))))
            .collect();
        let mut frequent: Vec<(&EntryId, u32, i64)> = self
            .keeps
            .iter()
            .filter(|(_, keep)| keep.favourite.is_none())
            .filter_map(|(id, keep)| {
                let used = keep.used?;
                let weight = used.weight_at(now);
                (weight >= FREQUENT_FLOOR).then_some((id, weight, used.last))
            })
            .collect();
        frequent.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(b.0)));
        Shelf {
            favourites,
            frequent: frequent.into_iter().map(|(id, _, _)| id.clone()).collect(),
            weights,
        }
    }

    /// The favourites in the account's order: by rank, then by entry, so a
    /// store holding two of one rank still reads one way.
    fn favourites(&self) -> Vec<EntryId> {
        let mut ranked: Vec<(u16, &EntryId)> = self
            .keeps
            .iter()
            .filter_map(|(id, keep)| Some((keep.favourite?, id)))
            .collect();
        ranked.sort();
        ranked.into_iter().map(|(_, id)| id.clone()).collect()
    }

    fn favourite_count(&self) -> usize {
        self.keeps
            .values()
            .filter(|keep| keep.favourite.is_some())
            .count()
    }

    /// Number the favourites `1..` in their order, answering those whose rank
    /// changed: a gap an unfavourite left, or a tie a hand-written store held.
    fn renumber(&mut self) -> Vec<EntryId> {
        let mut changed = Vec::new();
        for (index, id) in self.favourites().into_iter().enumerate() {
            let rank = u16::try_from(index.saturating_add(1)).ok();
            if let Some(keep) = self.keeps.get_mut(&id) {
                if keep.favourite != rank {
                    keep.favourite = rank;
                    changed.push(id);
                }
            }
        }
        changed
    }

    fn move_favourite(&mut self, id: &EntryId, later: bool) -> Vec<EntryId> {
        if self.keep(id).favourite.is_none() {
            return Vec::new();
        }
        let mut changed = self.renumber();
        let order = self.favourites();
        let Some(at) = order.iter().position(|kept| kept == id) else {
            return changed;
        };
        let other = if later {
            order.get(at.saturating_add(1))
        } else {
            at.checked_sub(1).and_then(|before| order.get(before))
        };
        let Some(other) = other.cloned() else {
            return changed;
        };
        let (mine, theirs) = (self.keep(id).favourite, self.keep(&other).favourite);
        if let Some(keep) = self.keeps.get_mut(id) {
            keep.favourite = theirs;
        }
        if let Some(keep) = self.keeps.get_mut(&other) {
            keep.favourite = mine;
        }
        push_once(&mut changed, id);
        push_once(&mut changed, &other);
        changed
    }

    fn launched(&mut self, id: &EntryId, now: Option<i64>) -> Result<Vec<EntryId>, RecordFull> {
        let mut keep = self.keep(id);
        keep.used = match (keep.used, now) {
            (Some(used), Some(now)) => Some(Use {
                weight: used
                    .weight_at(Some(now))
                    .saturating_add(LAUNCH_WEIGHT)
                    .min(MAX_WEIGHT),
                last: now,
            }),
            // A launch the clock cannot place still counts, but cannot move
            // when the program was last used.
            (Some(used), None) => Some(Use {
                weight: used.weight.saturating_add(LAUNCH_WEIGHT).min(MAX_WEIGHT),
                last: used.last,
            }),
            (None, Some(now)) => Some(Use {
                weight: LAUNCH_WEIGHT,
                last: now,
            }),
            // A record with no time would decay from the epoch: nothing new is
            // started until the clock can be read.
            (None, None) => return Ok(Vec::new()),
        };
        let mut changed: Vec<EntryId> = self.make_room(id, now)?.into_iter().collect();
        self.put(id, keep);
        changed.push(id.clone());
        Ok(changed)
    }

    /// Make room for a record of `id` when it has none and the bound is
    /// reached, by evicting the least used non-favourite at `now` — the
    /// longest unused where the clock cannot say — answering the entry
    /// evicted, whose stored record is gone too.
    fn make_room(&mut self, id: &EntryId, now: Option<i64>) -> Result<Option<EntryId>, RecordFull> {
        if self.keeps.contains_key(id) || self.keeps.len() < MAX_RECORDS {
            return Ok(None);
        }
        let weight = |keep: &Keep| match (keep.used, now) {
            (Some(used), Some(_)) => used.weight_at(now),
            _ => 0,
        };
        let last = |keep: &Keep| keep.used.map_or(i64::MIN, Use::last);
        let evicted = self
            .keeps
            .iter()
            .filter(|(_, keep)| keep.favourite.is_none())
            .min_by(|(a_id, a), (b_id, b)| {
                weight(a)
                    .cmp(&weight(b))
                    .then(last(a).cmp(&last(b)))
                    .then(a_id.cmp(b_id))
            })
            .map(|(id, _)| id.clone())
            .ok_or(RecordFull)?;
        self.keeps.remove(&evicted);
        Ok(Some(evicted))
    }

    /// Keep `keep` for `id`, or nothing at all when it is empty.
    fn put(&mut self, id: &EntryId, keep: Keep) {
        if keep.is_empty() {
            self.keeps.remove(id);
        } else {
            self.keeps.insert(id.clone(), keep);
        }
    }
}

fn push_once(changed: &mut Vec<EntryId>, id: &EntryId) {
    if !changed.contains(id) {
        changed.push(id.clone());
    }
}

#[cfg(test)]
#[path = "personal_tests.rs"]
mod tests;
