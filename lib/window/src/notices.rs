//! A window's refusals and warnings, stated one alert at a time in the order
//! they arose.
//!
//! A notice that arrives while another's alert is up waits behind it rather
//! than replacing it or being lost. Each source keeps at most one waiting — a
//! newer notice replaces its own source's older one — so the queue is bounded
//! by the sources a window has, never by how often one of them speaks.

use alloc::boxed::Box;
use alloc::collections::VecDeque;

use tairix_abi::window_ipc::{AlertSeverity, AlertSpec};
use tairix_abi::Errno;

use crate::client::{notice_alert, WindowClient, WindowTransport};

/// What [`Notices::raise`] did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Raised {
    /// One is up already, or none is waiting.
    Nothing,
    /// The oldest waiting is up.
    Up,
    /// The application has as many alerts up as it may: the notice waits, to
    /// be raised again once one of its alerts is answered.
    Full,
    /// The desktop refused it for the reason given, so it is dropped: the
    /// caller states it on `stderr`, where it is at least seen.
    Refused(Errno, Box<AlertSpec>),
}

/// One window's refusal notices: the alert up, and those waiting behind it,
/// each keyed by the source `S` that raised it.
#[derive(Debug)]
pub struct Notices<S> {
    showing: Option<u64>,
    waiting: VecDeque<(S, AlertSpec)>,
}

impl<S> Default for Notices<S> {
    fn default() -> Self {
        Self {
            showing: None,
            waiting: VecDeque::new(),
        }
    }
}

impl<S: Copy + Eq> Notices<S> {
    /// Whether a notice is up or about to be: the window is answering one.
    #[must_use]
    pub fn is_showing(&self) -> bool {
        self.showing.is_some() || !self.waiting.is_empty()
    }

    /// Whether one of these notices is up now.
    #[must_use]
    pub const fn is_up(&self) -> bool {
        self.showing.is_some()
    }

    /// State `notice` from `source`: next if nothing is up, else after those
    /// already waiting, in the place of any of its own still waiting.
    pub fn state(&mut self, source: S, notice: AlertSpec) {
        if let Some(waiting) = self.waiting.iter_mut().find(|(from, _)| *from == source) {
            waiting.1 = notice;
        } else {
            self.waiting.push_back((source, notice));
        }
    }

    /// State, from `source`, that what `title` names was not done because of
    /// `reason`.
    pub fn refuse(&mut self, source: S, title: &str, reason: &str) {
        if let Ok(notice) = notice_alert(AlertSeverity::Error, title, reason) {
            self.state(source, notice);
        }
    }

    /// Warn, from `source`, that what `title` names was done short of the
    /// whole, as `reason` says.
    pub fn warn(&mut self, source: S, title: &str, reason: &str) {
        if let Ok(notice) = notice_alert(AlertSeverity::Warning, title, reason) {
            self.state(source, notice);
        }
    }

    /// Put the oldest waiting notice up as an alert holding `owner`, or none
    /// for a refusal no window asked for, its answer sent to
    /// `event_endpoint`, unless one is up already.
    pub fn raise<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
        event_endpoint: u64,
        owner: Option<u64>,
    ) -> Raised {
        if self.showing.is_some() {
            return Raised::Nothing;
        }
        let Some((source, notice)) = self.waiting.pop_front() else {
            return Raised::Nothing;
        };
        match client.open_alert(event_endpoint, owner, &notice) {
            Ok(alert) => {
                self.showing = Some(alert);
                Raised::Up
            }
            Err(Errno::LimitExceeded) => {
                self.waiting.push_front((source, notice));
                Raised::Full
            }
            Err(err) => Raised::Refused(err, Box::new(notice)),
        }
    }

    /// Alert `alert_id` was answered: whether it was this window's notice,
    /// which lets the next waiting one go up.
    pub fn answered(&mut self, alert_id: u64) -> bool {
        let shown = self.showing == Some(alert_id);
        if shown {
            self.showing = None;
        }
        shown
    }
}
