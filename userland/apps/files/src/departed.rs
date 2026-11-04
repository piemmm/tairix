//! What volumes leaving ask of a file manager: one alert for every volume
//! disconnected without being ejected, however many windows showed it, the
//! places its windows must leave, and one window left at each place the
//! windows that left converge on.
//!
//! Whether a volume was ejected first is the kernel's word
//! ([`VolumeRemovalRecord`]): a volume gone from the mount table reads the same
//! whether it was ejected or pulled out.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::sysinfo::{MountAvailability, RemovedWrites, VolumeRemovalRecord, VOLUME_ID_LEN};
use tairix_browse::{Place, Places, Volume};
use tairix_util::utf8::shown;

/// The kernel's removals this file manager has taken in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Removals {
    /// The newest removal taken in, once a read of them has landed: those up to
    /// it were stated, or happened before this file manager looked.
    seen: Option<u64>,
}

impl Removals {
    /// Take in `listed`, the kernel's removals oldest first as read — `None`
    /// where they could not be read — answering those not taken in before. The
    /// first read answers none, since what it lists happened before this file
    /// manager looked.
    pub fn take_in<'a>(
        &mut self,
        listed: Option<&'a [VolumeRemovalRecord]>,
    ) -> &'a [VolumeRemovalRecord] {
        let Some(listed) = listed else {
            return &[];
        };
        let newest = listed.last().map_or(0, VolumeRemovalRecord::sequence);
        let Some(seen) = self.seen else {
            self.seen = Some(newest);
            return &[];
        };
        self.seen = Some(seen.max(newest));
        &listed[listed.partition_point(|removal| removal.sequence() <= seen)..]
    }
}

/// What the user calls each of `removed` and what became of its writes: its
/// name in `before`, the places shown before the removal, or in `after`, the
/// volumes listed since, where one that kept its writes stays. One named in
/// neither was never shown, and nothing need be said of it.
#[must_use]
pub fn named<'a>(
    removed: &[VolumeRemovalRecord],
    before: &'a Places,
    after: &'a [Volume],
) -> Vec<(&'a str, RemovedWrites)> {
    let name_of = |id: [u8; VOLUME_ID_LEN]| {
        before
            .rows()
            .iter()
            .find(|row| row.facts().is_some_and(|facts| facts.volume_id == id))
            .map(Place::label)
            .or_else(|| {
                after
                    .iter()
                    .find(|volume| volume.facts.volume_id == id)
                    .map(|volume| volume.label.as_str())
            })
    };
    removed
        .iter()
        .filter_map(|removal| Some((name_of(removal.volume_id())?, removal.writes())))
        .collect()
}

/// The alert saying `removed` were disconnected without being ejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Disconnected {
    /// Which were.
    pub headline: String,
    /// What became of what was being written to them.
    pub reason: &'static str,
    /// Some of it was lost: an error, where the rest is a warning.
    pub lost: bool,
}

/// `names` quoted and listed as a sentence names them — "A", "A and B",
/// "A, B and C" — each as shown, and whether there are several; `None` for
/// none.
fn listed<'a>(names: impl ExactSizeIterator<Item = &'a str>) -> Option<(String, bool)> {
    let count = names.len();
    let mut list = String::new();
    for (at, name) in names.enumerate() {
        if at > 0 {
            list.push_str(if at + 1 == count { " and " } else { ", " });
        }
        list.push('\u{201c}');
        list.push_str(&shown(name));
        list.push('\u{201d}');
    }
    (count > 0).then_some((list, count > 1))
}

/// The one alert for every volume `removed` names, or `None` for none.
#[must_use]
pub fn disconnected(removed: &[(&str, RemovedWrites)]) -> Option<Disconnected> {
    let (mut headline, several) = listed(removed.iter().map(|(name, _)| *name))?;
    headline.push_str(if several {
        " were disconnected without being ejected"
    } else {
        " was disconnected without being ejected"
    });
    let worst = removed
        .iter()
        .map(|(_, writes)| *writes)
        .max_by_key(|writes| match writes {
            RemovedWrites::None => 0,
            RemovedWrites::Held => 1,
            RemovedWrites::Lost => 2,
        })
        .unwrap_or(RemovedWrites::None);
    let reason = match (worst, several) {
        (RemovedWrites::None, false) => "nothing that was being written to it was lost",
        (RemovedWrites::None, true) => "nothing that was being written to them was lost",
        (RemovedWrites::Held, false) => {
            "what was still being written to it is kept until it is connected again"
        }
        (RemovedWrites::Held, true) => {
            "what was still being written to them is kept until they are connected again"
        }
        (RemovedWrites::Lost, false) => "some of what was being written to it was lost",
        (RemovedWrites::Lost, true) => "some of what was being written to them was lost",
    };
    Some(Disconnected {
        headline,
        reason,
        lost: worst == RemovedWrites::Lost,
    })
}

/// The one alert for volumes `names`, still attached, whose folders could not
/// be read again for a fault of their devices: its headline and its reason,
/// or `None` for none.
#[must_use]
pub fn unreadable(names: &[&str]) -> Option<(String, &'static str)> {
    let (mut headline, several) = listed(names.iter().copied())?;
    headline.push_str(" could not be read");
    Some((
        headline,
        if several {
            "their devices failed"
        } else {
            "its device failed"
        },
    ))
}

/// The rail row of the volume `places` offers that holds the folder `at`: the
/// innermost, where one is shown within another.
#[must_use]
pub fn holding<'a>(places: &'a Places, at: &[String]) -> Option<&'a Place> {
    places
        .rows()
        .iter()
        .filter(|row| row.facts().is_some() && at.starts_with(row.components()))
        .max_by_key(|row| row.components().len())
}

/// Whether the volume `place` stands for is being ridden out — its device or
/// its driver coming back — so what it refuses meanwhile is no failure yet.
#[must_use]
pub fn recovering(place: &Place) -> bool {
    place
        .facts()
        .is_some_and(|facts| facts.availability == MountAvailability::Recovering)
}

/// The places of every volume `before` offered that `after` no longer
/// offers — gone from the mount table, or there but unavailable — which a
/// window within one leaves for the place above it.
#[must_use]
pub fn departed(before: &Places, after: &[Volume]) -> Vec<Vec<String>> {
    let offered: BTreeSet<[u8; VOLUME_ID_LEN]> = after
        .iter()
        .filter(|volume| volume.offered)
        .map(|volume| volume.facts.volume_id)
        .collect();
    before
        .rows()
        .iter()
        .filter(|row| {
            row.facts()
                .is_some_and(|facts| !offered.contains(&facts.volume_id))
        })
        .map(|row| row.components().to_vec())
        .collect()
}

/// One browser window as converging duplicates weighs it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Standing<'a> {
    /// Where it is: a folder's components, or `None` for the network.
    pub at: Option<&'a [String]>,
    /// Closing it would end something it holds: a gesture, a question, an
    /// operation, a clipboard, a navigation under way.
    pub busy: bool,
    /// It holds the keyboard.
    pub focused: bool,
    /// It came to where it is by the user's own move, not by climbing out of a
    /// place that went.
    pub chosen: bool,
    /// When it came to where it is, in an order every window shares.
    pub since: u64,
}

/// What converging windows on one place each came to.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Converged {
    /// The windows to close, by index, ascending.
    pub closing: Vec<usize>,
    /// A set still holds two windows, one of them busy, so it is to be weighed
    /// again once that one is idle.
    pub waiting: bool,
}

/// The windows of `windows` to close so no two show one place. Of each set
/// showing the same place, the one holding the keyboard stays; else the one
/// the user came there in last, which is in front; else, where every one
/// climbed there, the one there longest. A busy window is never closed, so a
/// set it shares waits for it to finish.
#[must_use]
pub fn duplicates(windows: &[Standing<'_>]) -> Converged {
    let mut sets: BTreeMap<Option<&[String]>, Vec<usize>> = BTreeMap::new();
    for (index, window) in windows.iter().enumerate() {
        sets.entry(window.at).or_default().push(index);
    }
    let mut converged = Converged::default();
    for set in sets.values().filter(|set| set.len() > 1) {
        let keep = set.iter().copied().min_by_key(|&index| {
            let window = &windows[index];
            let order = if window.chosen {
                u64::MAX - window.since
            } else {
                window.since
            };
            (!window.focused, !window.chosen, order, index)
        });
        for &index in set {
            if Some(index) == keep {
                continue;
            }
            if windows[index].busy {
                converged.waiting = true;
            } else {
                converged.closing.push(index);
            }
        }
    }
    converged.closing.sort_unstable();
    converged
}

#[cfg(test)]
#[path = "departed_tests.rs"]
mod tests;
