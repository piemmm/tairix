//! The one question a window asks at a time: what it says until the desktop
//! puts it up, then the alert asking it, and what answering yes carries out.
//! A second question while one is pending is refused rather than put in its
//! place, so no question is lost unanswered and unstated.

use alloc::boxed::Box;

use tairix_abi::window_ipc::{AlertOutcome, AlertSpec};

/// Why a question could not be asked: another is.
pub const ANSWERING_ANOTHER: &str = "the window is waiting for an answer to another question";

/// Why a question could not be asked: an alert could not carry it.
pub const UNASKABLE: &str = "the question asking first could not be made";

/// Where a question is: what it says until it is up, then the alert asking it.
enum Ask {
    Waiting(Box<AlertSpec>),
    Up(u64),
}

struct Pending<C> {
    ask: Ask,
    /// The index of the button that confirms.
    yes: usize,
    confirms: C,
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
            .map(|pending| matches!(pending.ask, Ask::Up(_)))
    }

    /// What the question says, while it waits to go up.
    #[must_use]
    pub fn waiting(&self) -> Option<&AlertSpec> {
        match &self.pending.as_ref()?.ask {
            Ask::Waiting(spec) => Some(spec),
            Ask::Up(_) => None,
        }
    }

    /// The question waiting went up as alert `alert`.
    pub fn raised(&mut self, alert: u64) {
        if let Some(pending) = self.pending.as_mut() {
            if matches!(pending.ask, Ask::Waiting(_)) {
                pending.ask = Ask::Up(alert);
            }
        }
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
            .is_some_and(|pending| matches!(pending.ask, Ask::Up(up) if up == alert));
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
