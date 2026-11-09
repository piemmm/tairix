//! The one question a window asks at a time: what it says until the desktop
//! puts it up, then the alert asking it, and what answering yes carries out.
//! A second question while one is pending is refused rather than put in its
//! place, so no question is lost unanswered and unstated.

use alloc::boxed::Box;

use tairix_abi::window_ipc::{AlertOutcome, AlertSpec};
use tairix_abi::Errno;

/// Why a question could not be asked: another is.
pub const ANSWERING_ANOTHER: &str = "the window is waiting for an answer to another question";

/// Why a question could not be asked: an alert could not carry it.
pub const UNASKABLE: &str = "the question asking first could not be made";

/// Where a question is: what it says until it is up, then the alert asking
/// it, still saying it until answered, so a refusal can ask it again.
enum Ask {
    Waiting(Box<AlertSpec>),
    Up(u64, Box<AlertSpec>),
}

struct Pending<C> {
    ask: Ask,
    /// The index of the button that confirms.
    yes: usize,
    confirms: C,
}

/// What the desktop's refusal of an alert came to.
#[derive(Debug, Eq, PartialEq)]
pub enum Refusal<C> {
    /// The alert refused was not this window's question.
    Other,
    /// The application has every alert up it may: the question waits to go
    /// up again once one is answered.
    Waiting,
    /// The desktop will not ask it, so it is withdrawn, handing back what it
    /// would have confirmed.
    Withdrawn(C),
}

/// What an answer came to.
#[derive(Debug, Eq, PartialEq)]
pub enum Answer<C> {
    /// The alert answered was not this window's question.
    Other,
    /// The question was declined, or came down unanswered.
    Declined,
    /// It was answered yes: what it confirms is to be carried out.
    Confirmed(C),
}

/// A window's one question, if it asks one; `C` is what answering yes
/// carries out.
pub struct Question<C> {
    pending: Option<Pending<C>>,
}

impl<C> Default for Question<C> {
    fn default() -> Self {
        Self { pending: None }
    }
}

impl<C> Question<C> {
    /// Ask `spec`, whose button `yes` confirms `confirms`.
    ///
    /// # Errors
    ///
    /// `confirms` back, while another question is pending: the one already
    /// asked keeps its place and its answer.
    pub fn ask(&mut self, spec: AlertSpec, yes: usize, confirms: C) -> Result<(), C> {
        if self.pending.is_some() {
            return Err(confirms);
        }
        self.pending = Some(Pending {
            ask: Ask::Waiting(Box::new(spec)),
            yes,
            confirms,
        });
        Ok(())
    }

    /// Whether a question is asked and not yet answered.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// `Some(true)` for a question up, `Some(false)` for one waiting to go
    /// up, `None` for none: what [`next_alert`](crate::notice::next_alert)
    /// orders the window's alerts by.
    #[must_use]
    pub fn is_up(&self) -> Option<bool> {
        self.pending
            .as_ref()
            .map(|pending| matches!(pending.ask, Ask::Up(..)))
    }

    /// What the question says, while it waits to go up.
    #[must_use]
    pub fn waiting(&self) -> Option<&AlertSpec> {
        match &self.pending.as_ref()?.ask {
            Ask::Waiting(spec) => Some(spec),
            Ask::Up(..) => None,
        }
    }

    /// The question waiting was asked to go up as alert `alert`.
    pub fn raised(&mut self, alert: u64) {
        self.pending = self.pending.take().map(|pending| {
            let ask = match pending.ask {
                Ask::Waiting(spec) => Ask::Up(alert, spec),
                up @ Ask::Up(..) => up,
            };
            Pending { ask, ..pending }
        });
    }

    /// The desktop refused alert `alert` for `errno`.
    pub fn refused(&mut self, alert: u64, errno: Errno) -> Refusal<C> {
        let asked = self
            .pending
            .as_ref()
            .is_some_and(|pending| matches!(pending.ask, Ask::Up(up, _) if up == alert));
        if !asked {
            return Refusal::Other;
        }
        if errno != Errno::LimitExceeded {
            return self.withdraw().map_or(Refusal::Other, Refusal::Withdrawn);
        }
        self.pending = self.pending.take().map(|pending| {
            let ask = match pending.ask {
                Ask::Up(_, spec) => Ask::Waiting(spec),
                waiting @ Ask::Waiting(_) => waiting,
            };
            Pending { ask, ..pending }
        });
        Refusal::Waiting
    }

    /// Take the question back unanswered — the desktop would not ask it —
    /// handing back what it would have confirmed.
    pub fn withdraw(&mut self) -> Option<C> {
        self.pending.take().map(|pending| pending.confirms)
    }

    /// The answer `outcome` to alert `alert`, ending the question it asked.
    pub fn answer(&mut self, alert: u64, outcome: AlertOutcome) -> Answer<C> {
        let asked = self
            .pending
            .as_ref()
            .is_some_and(|pending| matches!(pending.ask, Ask::Up(up, _) if up == alert));
        let Some(pending) = self.pending.take_if(|_| asked) else {
            return Answer::Other;
        };
        match outcome {
            AlertOutcome::Chosen { index } if usize::from(index) == pending.yes => {
                Answer::Confirmed(pending.confirms)
            }
            AlertOutcome::Chosen { .. } | AlertOutcome::Dismissed => Answer::Declined,
        }
    }
}

#[cfg(test)]
#[path = "question_tests.rs"]
mod tests;
