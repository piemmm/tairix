//! Handing slow work off an interactive loop: one piece latest-wins
//! ([`JobDesk`]), or each of several in turn ([`JobQueue`]).

use alloc::collections::{TryReserveError, VecDeque};

/// A one-job-at-a-time hand-off between an interactive loop and a worker:
/// one request waiting, one in flight, one answer landed.
///
/// The loop *submits* and later *collects*; the worker *takes* and
/// *delivers*. Nothing here blocks, locks, or performs I/O — the embedder
/// supplies the exclusion and the parking — so every rule below is a host
/// test.
///
/// Two properties are the reason it exists rather than a queue:
///
/// - **Latest-wins.** A submission made while a job is in flight replaces any
///   earlier waiting one, so an interaction that settles repeatedly costs at
///   most one further job. A queue would make the loop's own responsiveness
///   the thing that generated the backlog.
/// - **At most one in flight.** Two concurrent writes to the same store would
///   race for what it ends up saying, so a job is handed out only once the
///   previous one has been answered — however many workers ask.
///
/// An answer that a newer submission has superseded is dropped rather than
/// delivered: adopting it would show a state the queued job is about to
/// replace. What a submission *displaced* is handed back to the submitter, so a
/// caller waiting on the displaced request can be told it was superseded rather
/// than left waiting for an answer that will never come.
pub struct JobDesk<Req, Ans> {
    /// The request waiting to be taken, replaced by each submission.
    pending: Option<Req>,
    /// Whether a worker has taken a job and not yet answered it.
    in_flight: bool,
    /// The answer, kept until the loop collects it.
    done: Option<Ans>,
    /// Set once the embedder is tearing down, so a parked worker leaves
    /// instead of looking for work.
    stopping: bool,
}

/// What submitting a request did.
#[derive(Debug, Eq, PartialEq)]
pub struct Submitted<Req> {
    /// Whether a worker should be woken: only when the request is takeable
    /// now. With one already in flight the worker looks again as soon as it
    /// has delivered, so a wake would buy nothing.
    pub wake: bool,
    /// The request this one replaced, if it displaced one that had not been
    /// taken. Nobody will ever answer it.
    pub displaced: Option<Req>,
}

impl<Req, Ans> Default for JobDesk<Req, Ans> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Req, Ans> JobDesk<Req, Ans> {
    /// A desk with nothing submitted, nothing in flight, and nothing answered.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending: None,
            in_flight: false,
            done: None,
            stopping: false,
        }
    }

    /// Submit `request`, replacing any submission not yet taken.
    ///
    /// A stopping desk accepts nothing and hands the request straight back as
    /// displaced, so a caller waiting on it is never left waiting.
    pub fn submit(&mut self, request: Req) -> Submitted<Req> {
        if self.stopping {
            return Submitted {
                wake: false,
                displaced: Some(request),
            };
        }
        Submitted {
            wake: !self.in_flight,
            displaced: self.pending.replace(request),
        }
    }

    /// Take the waiting request, or `None` when there is nothing to do.
    pub fn next_job(&mut self) -> Option<Req> {
        if self.stopping || self.in_flight {
            return None;
        }
        let request = self.pending.take()?;
        self.in_flight = true;
        Some(request)
    }

    /// Record `answer` for the job in flight.
    ///
    /// Answers `false` — and keeps nothing — when a newer request is already
    /// waiting, because that job's answer is the one the loop should adopt.
    /// The caller uses it to decide whether a wake is owed at all.
    pub fn deliver(&mut self, answer: Ans) -> bool {
        self.in_flight = false;
        if self.pending.is_some() {
            return false;
        }
        self.done = Some(answer);
        true
    }

    /// Take the landed answer, once.
    pub fn collect(&mut self) -> Option<Ans> {
        self.done.take()
    }

    /// Whether a worker holds a job it has not yet answered.
    #[must_use]
    pub const fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// Whether a request is waiting for a worker to take it.
    #[must_use]
    pub const fn has_work(&self) -> bool {
        !self.stopping && self.pending.is_some() && !self.in_flight
    }

    /// Stop handing out work, so a parked worker leaves its loop.
    ///
    /// A job already in flight is still deliverable, so a worker mid-write
    /// finishes rather than abandoning a half-published document.
    pub fn stop(&mut self) {
        self.stopping = true;
        self.pending = None;
    }

    /// Whether the embedder has asked workers to leave.
    #[must_use]
    pub const fn stopping(&self) -> bool {
        self.stopping
    }
}

/// Jobs handed off an interactive loop, each answered exactly once, in the
/// order they finish — which is the order they were asked while one worker
/// serves the queue.
///
/// For work where every request is its own — opening each document the user
/// asked for — so latest-wins would silently drop one. Workers take jobs in
/// turn and may carry several at once; answers are collected in the order
/// they land. Everything not yet collected counts against a fixed capacity,
/// reserved when the queue is made, so a burst of asks is refused rather than
/// grown without limit and no later step allocates — an answer, once carried
/// out, always has somewhere to land.
pub struct JobQueue<Req, Ans> {
    waiting: VecDeque<Req>,
    answered: VecDeque<Ans>,
    in_flight: usize,
    capacity: usize,
    stopping: bool,
}

impl<Req, Ans> Default for JobQueue<Req, Ans> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Req, Ans> JobQueue<Req, Ans> {
    /// A queue with no room, refusing every submission — what an embedder
    /// stops, so its callers carry their work out themselves, when the room
    /// for a real one is refused.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            waiting: VecDeque::new(),
            answered: VecDeque::new(),
            in_flight: 0,
            capacity: 0,
            stopping: false,
        }
    }

    /// An empty queue holding at most `capacity` jobs not yet collected.
    ///
    /// # Errors
    ///
    /// The refusal of the memory for `capacity` requests and answers.
    pub fn with_capacity(capacity: usize) -> Result<Self, TryReserveError> {
        let mut waiting = VecDeque::new();
        waiting.try_reserve_exact(capacity)?;
        let mut answered = VecDeque::new();
        answered.try_reserve_exact(capacity)?;
        Ok(Self {
            waiting,
            answered,
            in_flight: 0,
            capacity,
            stopping: false,
        })
    }

    /// Queue `request` for the next worker to look.
    ///
    /// # Errors
    ///
    /// `request` itself, back, when the queue is stopping or already holds
    /// its capacity: nobody will answer it.
    pub fn submit(&mut self, request: Req) -> Result<(), Req> {
        if self.stopping || !self.room() {
            return Err(request);
        }
        self.waiting.push_back(request);
        Ok(())
    }

    /// Take the oldest waiting request, or `None` when there is nothing to do.
    pub fn next_job(&mut self) -> Option<Req> {
        if self.stopping {
            return None;
        }
        let request = self.waiting.pop_front()?;
        self.in_flight += 1;
        Some(request)
    }

    /// Record `answer` for a job in flight, answering whether the loop needs
    /// a wake: only for the first answer it has not yet collected.
    ///
    /// An answer for no job in flight answers nothing a worker took, so it is
    /// dropped rather than landed past the room reserved for it.
    pub fn deliver(&mut self, answer: Ans) -> bool {
        let Some(in_flight) = self.in_flight.checked_sub(1) else {
            return false;
        };
        self.in_flight = in_flight;
        self.answered.push_back(answer);
        self.answered.len() == 1
    }

    /// Take the oldest landed answer, once.
    pub fn collect(&mut self) -> Option<Ans> {
        self.answered.pop_front()
    }

    /// How many answers have landed and wait to be collected.
    #[must_use]
    pub fn landed(&self) -> usize {
        self.answered.len()
    }

    /// Whether a request is waiting for a worker to take it.
    #[must_use]
    pub fn has_work(&self) -> bool {
        !self.stopping && !self.waiting.is_empty()
    }

    /// Whether one more job would fit the room the queue holds.
    #[must_use]
    pub fn room(&self) -> bool {
        self.held() < self.capacity
    }

    /// Whether a request is waiting or a worker holds one it has not yet
    /// answered: what a loop with nothing else left to do waits out.
    #[must_use]
    pub fn outstanding(&self) -> bool {
        !self.waiting.is_empty() || self.in_flight > 0
    }

    /// Carry `request` out with `run` here, when no worker will take it, and
    /// land its answer behind those already landed.
    ///
    /// # Errors
    ///
    /// `request`, back and not run, when the queue already holds its
    /// capacity.
    pub fn carry_out(&mut self, request: Req, run: impl FnOnce(Req) -> Ans) -> Result<(), Req> {
        if !self.room() {
            return Err(request);
        }
        self.answered.push_back(run(request));
        Ok(())
    }

    /// Withdraw every waiting request `keep` turns down. Nobody answers what
    /// is withdrawn, so a caller withdraws only what it no longer awaits;
    /// work in flight or answered is untouched.
    pub fn retain_waiting(&mut self, keep: impl FnMut(&Req) -> bool) {
        self.waiting.retain(keep);
    }

    /// Hold room for `more` further jobs, so a queue whose bound follows what
    /// it serves grows before it refuses.
    ///
    /// # Errors
    ///
    /// The refusal of the memory; the bound is as it was.
    pub fn grow(&mut self, more: usize) -> Result<(), TryReserveError> {
        let capacity = self.capacity.saturating_add(more);
        self.waiting
            .try_reserve_exact(capacity.saturating_sub(self.waiting.len()))?;
        self.answered
            .try_reserve_exact(capacity.saturating_sub(self.answered.len()))?;
        self.capacity = capacity;
        Ok(())
    }

    /// Give up room for `fewer` jobs. What the queue already holds stays and
    /// is still answered; it only refuses new requests until enough of it has
    /// been collected.
    pub fn shrink(&mut self, fewer: usize) {
        self.capacity = self.capacity.saturating_sub(fewer);
    }

    /// Stop handing out work, handing back what was still waiting so its
    /// asker can be answered another way. Jobs in flight stay deliverable.
    pub fn stop(&mut self) -> VecDeque<Req> {
        self.stopping = true;
        core::mem::take(&mut self.waiting)
    }

    /// Whether the embedder has asked workers to leave.
    #[must_use]
    pub const fn stopping(&self) -> bool {
        self.stopping
    }

    /// Jobs not yet collected: waiting, in flight, or answered.
    fn held(&self) -> usize {
        self.waiting.len() + self.in_flight + self.answered.len()
    }
}

#[cfg(test)]
#[path = "defer_tests.rs"]
mod tests;
