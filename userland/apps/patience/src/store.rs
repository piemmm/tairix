//! The game's own store: what one write carries, how it is carried out, and
//! the ledger that keeps the loop's side of it.
//!
//! Preferences, statistics and the kept game share one document, so one
//! [`Job`] carries whatever of the three is owed and reaches the document in
//! a single commit. The [`Ledger`] keeps at most one job in flight; what is
//! asked for meanwhile is owed behind it, so no write ever displaces another
//! and every game reaches the statistics exactly once.
//!
//! Nothing here performs I/O of its own: [`carry_out`] is handed the store,
//! which on the device is opened by the writer's worker thread.

use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_appconf::{Keys, Registry};
use tairix_appdata::{stage, Publication, PublishAnswer, PublishJob, Published, Refusal, Settings};

use crate::prefs::Preferences;
use crate::saved::Saved;
use crate::stats::{Stats, Tally};

/// Take `value` into `slot` when there is one, answering whether there was:
/// a registry's read of one stored setting.
pub(crate) fn kept<T>(slot: &mut T, value: Option<T>) -> bool {
    match value {
        Some(value) => {
            *slot = value;
            true
        }
        None => false,
    }
}

/// One write to the game's store.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Job {
    /// The preferences write the publication owes.
    pub prefs: Option<PublishJob<Preferences>>,
    /// Games to add to the statistics, and records to forget, in order.
    pub tallies: Vec<Tally>,
    /// The game to keep — [`Saved::default`] forgets the one kept.
    pub saved: Option<Saved>,
}

impl Job {
    /// Whether the job asks for nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prefs.is_none() && self.tallies.is_empty() && self.saved.is_none()
    }
}

/// What the store said after a [`Job`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Answer {
    /// The answer to the preferences part, when the job had one.
    pub prefs: Option<PublishAnswer<Preferences>>,
    /// The statistics the store now holds, or why the write failed.
    pub stats: Result<Stats, Errno>,
    /// Whether the kept game was written, or why not.
    pub saved: Result<(), Errno>,
}

/// Carry `job` out against `store` in one commit, answering what the store
/// then holds.
///
/// Statistics are added to what the store holds now, and preferences are
/// written only where the job names them, so a session that began without
/// its store writes over nothing it never read.
pub fn carry_out(job: &Job, store: &mut Settings<'_>) -> Answer {
    let written = write(job, store);
    if let Err(err) = written {
        return Answer {
            prefs: job.prefs.as_ref().map(|_| Err(err)),
            stats: Err(err),
            saved: Err(err),
        };
    }
    let prefs = job.prefs.as_ref().map(|_| {
        let (record, refused) = Preferences::load(store);
        Ok(Published { record, refused })
    });
    Answer {
        prefs,
        stats: Ok(Stats::load(store).0),
        saved: Ok(()),
    }
}

fn write(job: &Job, store: &mut Settings<'_>) -> Result<(), Errno> {
    if let Some(err) = store.store_refusal() {
        return Err(err);
    }
    match &job.prefs {
        Some(PublishJob::Save { record, keys }) => stage(record, *keys, store)?,
        Some(PublishJob::Restore) => {
            for &key in Preferences::KEYS {
                store.unset(Preferences::name(key));
            }
        }
        None => {}
    }
    if !job.tallies.is_empty() {
        let (mut stats, refused) = Stats::load(store);
        let mut changed = Keys::EMPTY;
        for &tally in &job.tallies {
            changed = changed.union(stats.apply(tally));
        }
        // A stored value the registry refused counts as nothing, so the
        // record written in its place is the one the added games make.
        for key in refused {
            changed = changed.union(Keys::of(key));
        }
        stage(&stats, changed, store)?;
    }
    if let Some(saved) = &job.saved {
        stage(saved, Keys::ALL, store)?;
    }
    store.commit()
}

/// The loop's side of the store: the preferences on screen, the statistics
/// last read, and every write owed.
#[derive(Clone, Debug)]
pub struct Ledger {
    prefs: Publication<Preferences>,
    stats: Stats,
    tallies: Vec<Tally>,
    saved: Option<Saved>,
    /// What the job in flight carried, owed again if it fails.
    outstanding: Option<(Vec<Tally>, Option<Saved>)>,
    /// Set when the last write failed: owed work then waits for new work
    /// rather than being retried at once, which would retry for ever.
    failed: bool,
}

impl Ledger {
    /// Start from what the store held at start.
    #[must_use]
    pub fn new(prefs: Preferences, stats: Stats) -> Self {
        Self {
            prefs: Publication::new(prefs),
            stats,
            tallies: Vec::new(),
            saved: None,
            outstanding: None,
            failed: false,
        }
    }

    /// The preferences in force.
    #[must_use]
    pub fn prefs(&self) -> &Preferences {
        self.prefs.live()
    }

    /// Make `now` the preferences in force, owing their write.
    pub fn choose(&mut self, now: Preferences) {
        let was = *self.prefs.live();
        self.prefs.edit(&was, &now);
        self.failed = false;
    }

    /// The statistics as they will stand once every owed game is added.
    #[must_use]
    pub fn stats(&self) -> Stats {
        let mut stats = self.stats;
        let owed = self.outstanding.iter().flat_map(|(tallies, _)| tallies);
        for &tally in owed.chain(&self.tallies) {
            let _ = stats.apply(tally);
        }
        stats
    }

    /// Owe `tally`.
    pub fn tally(&mut self, tally: Tally) {
        self.tallies.push(tally);
        self.failed = false;
    }

    /// Owe keeping `saved`, which replaces any keep owed before it.
    pub fn keep(&mut self, saved: Saved) {
        self.saved = Some(saved);
        self.failed = false;
    }

    /// Whether a write is in flight.
    #[must_use]
    pub const fn writing(&self) -> bool {
        self.outstanding.is_some()
    }

    /// The write to submit now, if one is owed and none is in flight.
    pub fn job(&mut self) -> Option<Job> {
        if self.outstanding.is_some() || self.failed {
            return None;
        }
        self.next()
    }

    /// The write to submit as the program ends: as [`job`](Self::job), but
    /// tried once more however the last one went.
    pub fn final_job(&mut self) -> Option<Job> {
        if self.outstanding.is_some() {
            return None;
        }
        self.next()
    }

    fn next(&mut self) -> Option<Job> {
        let job = Job {
            prefs: self.prefs.settle(),
            tallies: core::mem::take(&mut self.tallies),
            saved: self.saved.take(),
        };
        if job.is_empty() {
            return None;
        }
        self.outstanding = Some((job.tallies.clone(), job.saved.clone()));
        Some(job)
    }

    /// Adopt the store's `answer` to the job in flight, saying what is worth
    /// saying into `refusals`, and answer the write owed behind it.
    pub fn landed(&mut self, answer: Answer, refusals: &mut Vec<Refusal>) -> Option<Job> {
        let (tallies, saved) = self.outstanding.take().unwrap_or_default();
        let said = answer.prefs.is_some();
        if let Some(prefs) = answer.prefs {
            // A preferences write owed meanwhile is the publication's own,
            // taken by the next job; a refusal of this one it says itself.
            let _ = self.prefs.adopt(prefs, refusals);
        }
        match answer.stats {
            Ok(stats) => self.stats = stats,
            Err(err) => {
                if !said {
                    refusals.push(Refusal::NotSaved(err));
                }
                self.failed = true;
                // Owed first: the failed games were played before the newer.
                let newer = core::mem::replace(&mut self.tallies, tallies);
                self.tallies.extend(newer);
                if self.saved.is_none() {
                    self.saved = saved;
                }
            }
        }
        self.job()
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
