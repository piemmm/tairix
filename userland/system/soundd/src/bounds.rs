//! What stops a cue storm: one record per recent principal, holding its cue
//! rate and when it last sounded each event.
//!
//! Every figure here is a containment bound, not a capacity. A desktop makes a
//! handful of sounds a minute, so a bound well above that costs a legitimate
//! principal nothing and caps what a misbehaving one can do with the speakers.

use alloc::vec::Vec;

use tairix_abi::sound_cue::SoundEvent;

/// Cues that may sound at once, decoding or playing. Past it a cue is dropped
/// rather than queued: a sound arriving after the thing it announces is
/// noise.
pub const MAX_CONCURRENT_CUES: usize = 4;

/// Callers waiting on one sounding cue to play out, beyond the one that asked
/// for it. A repeat of a sound already sounding joins it rather than sounding
/// over it; past this a joiner is answered at once as coalesced.
pub const MAX_JOINED_CUES: usize = 16;

/// A repeat of one event by one principal sooner than this after it last
/// sounded is the same occurrence, not a second one: two identical
/// notification sounds 20 ms apart are one notification and one bug.
pub const MIN_RETRIGGER_NS: u64 = 200_000_000;

/// Distinct cues a principal may make in a burst.
pub const CUE_BURST: u32 = 8;

/// How often a principal earns back one cue: four a second, sustained.
pub const CUE_REFILL_NS: u64 = 250_000_000;

/// Principals tracked at once. A principal whose rate has recovered and whose
/// last cue is past the retrigger interval holds nothing worth keeping, so a
/// record is reclaimed rather than the table growing; one that finds every
/// record still active is refused, failing closed.
pub const MAX_PRINCIPALS: usize = 64;

const EVENTS: usize = SoundEvent::ALL.len();

/// What the bounds decided about one cue.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Admit {
    /// It may sound.
    Admitted,
    /// It repeats the same event too soon: one sound, not two.
    Coalesced,
    /// The principal is past its rate, or no record is free for it.
    Limited {
        /// Whether this is the first refusal since it was last admitted, so
        /// a flood is recorded once rather than once per request.
        first: bool,
    },
}

/// One principal's cue rate and recent cues.
struct Record {
    uid: u32,
    tokens: u32,
    refilled: u64,
    sounded: [Option<u64>; EVENTS],
    refused: bool,
}

impl Record {
    const fn new(uid: u32, now: u64) -> Self {
        Self {
            uid,
            tokens: CUE_BURST,
            refilled: now,
            sounded: [None; EVENTS],
            refused: false,
        }
    }

    fn refill(&mut self, now: u64) {
        let earned = now.saturating_sub(self.refilled) / CUE_REFILL_NS;
        if earned == 0 {
            return;
        }
        let tokens = u64::from(self.tokens).saturating_add(earned);
        self.tokens = u32::try_from(tokens.min(u64::from(CUE_BURST))).unwrap_or(CUE_BURST);
        self.refilled = if self.tokens == CUE_BURST {
            now
        } else {
            self.refilled + earned * CUE_REFILL_NS
        };
    }

    /// Whether the record says nothing a fresh one would not.
    fn idle(&self, now: u64) -> bool {
        self.tokens == CUE_BURST
            && self
                .sounded
                .iter()
                .flatten()
                .all(|&at| now.saturating_sub(at) >= MIN_RETRIGGER_NS)
    }
}

/// The records of every recent principal.
pub struct Principals {
    records: Vec<Record>,
}

impl Default for Principals {
    fn default() -> Self {
        Self::new()
    }
}

impl Principals {
    /// No principal has cued yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }

    /// Whether `uid` may sound `event` at `now`, charging it if so.
    ///
    /// A repeat is coalesced before the rate is consulted, so the same event
    /// asked for again too soon costs the principal nothing.
    pub fn admit(&mut self, uid: u32, event: SoundEvent, now: u64) -> Admit {
        let Some(record) = self.record(uid, now) else {
            return Admit::Limited { first: true };
        };
        record.refill(now);
        let slot = event.index();
        if record.sounded[slot].is_some_and(|at| now.saturating_sub(at) < MIN_RETRIGGER_NS) {
            return Admit::Coalesced;
        }
        if record.tokens == 0 {
            let first = !core::mem::replace(&mut record.refused, true);
            return Admit::Limited { first };
        }
        record.tokens -= 1;
        record.refused = false;
        record.sounded[slot] = Some(now);
        Admit::Admitted
    }

    /// `uid`'s record, reclaiming an idle one for it when it has none; `None`
    /// when every record is in use or no memory is left for one.
    fn record(&mut self, uid: u32, now: u64) -> Option<&mut Record> {
        if let Some(at) = self.records.iter().position(|record| record.uid == uid) {
            return self.records.get_mut(at);
        }
        if let Some(at) = self.records.iter_mut().position(|record| {
            record.refill(now);
            record.idle(now)
        }) {
            self.records[at] = Record::new(uid, now);
            return self.records.get_mut(at);
        }
        if self.records.len() >= MAX_PRINCIPALS || self.records.try_reserve(1).is_err() {
            return None;
        }
        self.records.push(Record::new(uid, now));
        self.records.last_mut()
    }
}

#[cfg(test)]
#[path = "bounds_tests.rs"]
mod tests;
