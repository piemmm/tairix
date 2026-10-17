//! What a worker answers of a window's document as it was at one generation:
//! asked once a generation, taken only when it answers the ask still waited
//! for. The shape the Code and Data displays share.

use alloc::string::String;

/// Why a document has no listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Unlisted {
    /// Why, in words.
    pub reason: String,
    /// Whether asking again would be refused alike. A passing refusal, for
    /// want of memory or of a worker, is asked again once the display is
    /// shown afresh.
    pub lasting: bool,
}

/// Where a worker's answer stands.
#[derive(Debug)]
pub(crate) struct Answered<T> {
    state: State<T>,
    /// The id the next ask is made by.
    next_id: u64,
}

#[derive(Debug)]
enum State<T> {
    /// Nothing is asked for: the display has not shown, or what it showed is
    /// out of date.
    Unasked,
    /// The worker is answering for the document as it was at `generation`.
    Asking { id: u64, generation: u64 },
    /// The answer for the document as it was at `generation`.
    Answered { answer: T, generation: u64 },
    /// Why there is none, for the document as it was at `generation`.
    Refused { unlisted: Unlisted, generation: u64 },
}

impl<T> Answered<T> {
    /// Nothing asked yet.
    pub(crate) const fn new() -> Self {
        Self {
            state: State::Unasked,
            next_id: 1,
        }
    }

    /// Forget what is held, so the next want asks afresh.
    pub(crate) fn forget(&mut self) {
        self.state = State::Unasked;
    }

    /// Forget a refusal that said nothing of the document, so it is asked
    /// again.
    pub(crate) fn forget_passing(&mut self) {
        if matches!(&self.state, State::Refused { unlisted, .. } if !unlisted.lasting) {
            self.state = State::Unasked;
        }
    }

    /// The id to ask by for the document at `generation`, when what is held
    /// is of another generation or nothing; records that it was asked.
    pub(crate) fn want(&mut self, generation: u64) -> Option<u64> {
        let current = match &self.state {
            State::Unasked => false,
            State::Asking { generation: of, .. }
            | State::Answered { generation: of, .. }
            | State::Refused { generation: of, .. } => *of == generation,
        };
        if current {
            return None;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.state = State::Asking { id, generation };
        Some(id)
    }

    /// Take the answer to the ask made by `id`, answering whether it was the
    /// one waited for: an answer to an ask since replaced is let go.
    pub(crate) fn take(&mut self, id: u64, answer: Result<T, Unlisted>) -> bool {
        let State::Asking {
            id: asked,
            generation,
        } = self.state
        else {
            return false;
        };
        if asked != id {
            return false;
        }
        self.state = match answer {
            Ok(answer) => State::Answered { answer, generation },
            Err(unlisted) => State::Refused {
                unlisted,
                generation,
            },
        };
        true
    }

    /// Refuse the document at `generation` without asking the worker.
    pub(crate) fn refuse(&mut self, unlisted: Unlisted, generation: u64) {
        self.state = State::Refused {
            unlisted,
            generation,
        };
    }

    /// The answer, once it has landed.
    pub(crate) const fn answer(&self) -> Option<&T> {
        match &self.state {
            State::Answered { answer, .. } => Some(answer),
            State::Unasked | State::Asking { .. } | State::Refused { .. } => None,
        }
    }

    /// What a display says in place of the answer while it has none:
    /// `asking` until it lands, else why there is none.
    pub(crate) fn note<'a>(&'a self, asking: &'a str) -> Option<&'a str> {
        match &self.state {
            State::Answered { .. } => None,
            State::Unasked | State::Asking { .. } => Some(asking),
            State::Refused { unlisted, .. } => Some(&unlisted.reason),
        }
    }
}
