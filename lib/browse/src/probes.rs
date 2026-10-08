//! The folder-cue probe desk: which folders a paint has asked about, which
//! batch a worker is reading, and what has come back.
//!
//! Every surface that draws folder cues off its event loop — the file
//! manager's windows and the desktop's icons — shares this one policy, so the
//! rules a paint relies on are host tests. It holds no lock, thread, or
//! syscall: the embedder supplies the exclusion and the blocking, and the read
//! itself is [`vfs::probe_batch`](crate::vfs::probe_batch).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::Errno;

use crate::source::Probe;
use crate::watch::EntryChange;

/// The most folders waiting for a worker at once.
///
/// A containment bound, not a capacity: a sweep drops every folder no pass
/// asked about, so this bounds only what one pass can ask, which a screenful
/// of folders never approaches. Past it a folder is left unrecorded and asked
/// again by the next paint.
pub const MAX_WANTED_PROBES: usize = 1024;

/// An answer waiting to be drawn.
#[derive(Debug)]
struct Held {
    answer: Result<Probe, Errno>,
    /// A resolve pass has been offered this answer, so the next one drops it.
    offered: bool,
}

/// What the folder cues have asked for and what has come back.
#[derive(Debug, Default)]
pub struct Probes {
    /// Folders asked about and not yet taken by a worker, each with whether
    /// a pass asked about it since the last [`sweep`](Self::sweep).
    wanted: BTreeMap<Vec<String>, bool>,
    /// The batch a worker is reading. One batch is in flight at a time, so a
    /// re-ask during it records no second probe.
    probing: BTreeSet<Vec<String>>,
    /// Each answer is served once: the entry latches it, so a later ask is a
    /// fresh question. A refusal is an answer too.
    answers: BTreeMap<Vec<String>, Held>,
    /// Folders that changed while being read: the batch in flight describes
    /// them as they were.
    superseded: BTreeSet<Vec<String>>,
    /// A batch has been delivered since the embedder last asked.
    landed: bool,
    /// A batch landed and the embedder has not yet swept: until it does, what
    /// is wanted may be folders gone from view, so no batch is handed out.
    awaiting_sweep: bool,
    stopping: bool,
}

impl Probes {
    /// A desk with nothing asked for and nothing answered.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            wanted: BTreeMap::new(),
            probing: BTreeSet::new(),
            answers: BTreeMap::new(),
            superseded: BTreeSet::new(),
            landed: false,
            awaiting_sweep: false,
            stopping: false,
        }
    }

    /// Answer the cue for `components`, recording a probe unless one is held,
    /// wanted, or in flight.
    ///
    /// A held answer is handed over. Anything else is [`Probe::Pending`], and
    /// the folder draws without its cue until the answer lands. A stopped desk
    /// answers [`Errno::NotImplemented`], as a source that does not probe
    /// does, so the folder draws plain and is not asked again.
    ///
    /// The `bool` is whether this ask recorded a probe, so the embedder wakes
    /// a worker once per folder rather than once per paint.
    pub fn ask(&mut self, components: &[String]) -> (Result<Probe, Errno>, bool) {
        if let Some(held) = self.answers.remove(components) {
            return (held.answer, false);
        }
        if self.stopping {
            return (Err(Errno::NotImplemented), false);
        }
        let in_flight = self.probing.contains(components) && !self.superseded.contains(components);
        if in_flight {
            return (Ok(Probe::Pending), false);
        }
        if let Some(asked) = self.wanted.get_mut(components) {
            *asked = true;
            return (Ok(Probe::Pending), false);
        }
        if self.wanted.len() >= MAX_WANTED_PROBES {
            return (Ok(Probe::Pending), false);
        }
        self.wanted.insert(components.to_vec(), true);
        (Ok(Probe::Pending), !self.awaiting_sweep)
    }

    /// Whether a worker would be handed a batch now.
    #[must_use]
    pub fn has_work(&self) -> bool {
        !self.stopping && !self.awaiting_sweep && !self.wanted.is_empty() && self.probing.is_empty()
    }

    /// Drop every wanted folder no pass asked about since the last sweep,
    /// answering whether a worker now has a batch to take.
    ///
    /// Run only after a pass over **every** surface drawing folder cues: one
    /// a pass skipped would lose its still-visible folders until it paints
    /// again. A delivered batch holds the next until this runs, so a worker
    /// never reads folders scrolled out of view while it was busy.
    pub fn sweep(&mut self) -> bool {
        self.wanted
            .retain(|_, asked| core::mem::replace(asked, false));
        self.awaiting_sweep = false;
        self.has_work()
    }

    /// The folders among `changes`, reported in the directory `dir`, changed:
    /// what is held or in flight for them is dropped, so each is asked afresh.
    ///
    /// Answers whether any change was to a folder, which owes the cues a
    /// resolve.
    pub fn invalidate(&mut self, dir: &[String], changes: &[EntryChange]) -> bool {
        let folders: BTreeSet<&str> = changes
            .iter()
            .filter_map(|change| match change {
                EntryChange::Upsert(entry) if entry.is_directory() => Some(entry.name()),
                _ => None,
            })
            .collect();
        if folders.is_empty() {
            return false;
        }
        self.drop_where(dir, |name| folders.contains(name));
        true
    }

    /// The directory `dir` is being listed afresh, so whatever is held or in
    /// flight for a folder in it describes the folder as it was.
    pub fn invalidate_listing(&mut self, dir: &[String]) {
        self.drop_where(dir, |_| true);
    }

    /// Drop what is held, and supersede what is in flight, for each folder in
    /// `dir` whose name `changed` admits.
    fn drop_where(&mut self, dir: &[String], changed: impl Fn(&str) -> bool) {
        let stale = |path: &[String]| {
            path.split_last()
                .is_some_and(|(name, parent)| parent == dir && changed(name))
        };
        self.answers.retain(|path, _| !stale(path));
        for path in &self.probing {
            if stale(path) {
                self.superseded.insert(path.clone());
            }
        }
    }

    /// Take every outstanding probe as one batch, or `None` when there is
    /// nothing to take or a batch is already in flight.
    ///
    /// A batch because the answers are drawn together: a screenful answered
    /// one wake at a time would be a screenful of repaints.
    pub fn next_batch(&mut self) -> Option<Vec<Vec<String>>> {
        if !self.has_work() {
            return None;
        }
        let batch: BTreeSet<Vec<String>> = core::mem::take(&mut self.wanted).into_keys().collect();
        self.probing.clone_from(&batch);
        Some(batch.into_iter().collect())
    }

    /// Record the batch in flight's answers, answering whether any is worth a
    /// repaint. An answer for a folder that changed while it was read is
    /// dropped: that folder is asked afresh.
    pub fn deliver(&mut self, answers: Vec<(Vec<String>, Result<Probe, Errno>)>) -> bool {
        self.probing.clear();
        let superseded = core::mem::take(&mut self.superseded);
        if self.stopping {
            return false;
        }
        let mut fresh = false;
        for (path, answer) in answers {
            if !superseded.contains(&path) {
                let held = Held {
                    answer,
                    offered: false,
                };
                self.answers.insert(path, held);
                fresh = true;
            }
        }
        // Only a delivery worth drawing wakes the loop that sweeps, so only one
        // holds the next batch for it.
        self.landed |= fresh;
        self.awaiting_sweep |= fresh;
        fresh
    }

    /// Whether a batch has been delivered since this was last asked, clearing
    /// the record.
    ///
    /// Call it before each resolve pass. It is also what bounds the held
    /// answers by the screen rather than by every folder ever scrolled past:
    /// an answer a whole pass was offered and did not take is for a folder out
    /// of view, and goes here. A folder that scrolls back is asked again.
    pub fn take_landed(&mut self) -> bool {
        self.answers.retain(|_, held| !held.offered);
        for held in self.answers.values_mut() {
            held.offered = true;
        }
        core::mem::take(&mut self.landed)
    }

    /// Stop recording, so a parked worker leaves and every later ask is
    /// answered as a source that does not probe.
    pub fn stop(&mut self) {
        self.stopping = true;
        self.wanted.clear();
        self.answers.clear();
        self.landed = false;
    }

    /// Whether the embedder has asked workers to leave.
    #[must_use]
    pub const fn stopping(&self) -> bool {
        self.stopping
    }
}

#[cfg(test)]
#[path = "probes_tests.rs"]
mod tests;
