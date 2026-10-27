//! Asking the session authority about one offered secret, off the screen's
//! loop.
//!
//! The authority spends a deliberate key-derivation cost on every secret, and
//! the screen owes the user a frame, so an offer never waits on the loop: it
//! becomes a [`Check`], a [`Courier`] — a worker thread on a running system —
//! [`exchange`]s it with the authority, and its [`Checked`] answer is
//! collected once it has landed.
//!
//! [`Courier`]: tairix_greeter::Courier

use alloc::string::String;

use tairix_abi::secret::Secret;
use tairix_abi::session_ipc::{
    SessionRequest, SessionVerdict, SESSION_MAX_REPLY, SESSION_MAX_REQUEST,
};
use tairix_abi::time::Duration64;
use tairix_greeter::{Ticket, Verdict};
use tairix_util::secret::WipedBuf;

use crate::accounts::SessionTransport;

/// What the authority answered about one offered secret.
///
/// Kept for the embedder because the surface swallows the verdict: the screen
/// only needs to know whether it is still asking, while the service still has
/// to audit the answer and present the lockout that came with it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Answer {
    /// The answer itself.
    pub verdict: Verdict,
    /// How long the account must wait before offering again. Zero unless the
    /// authority refused with a lockout.
    pub retry_after: Duration64,
}

impl Answer {
    /// No answer was obtained, so there is no lockout to present either.
    const fn unreachable() -> Self {
        Self {
            verdict: Verdict::Unreachable,
            retry_after: Duration64::ZERO,
        }
    }
}

/// One offered secret on its way to the authority: the `session-v1` request
/// asking about it, encoded once into a buffer sized up front so it never
/// moves, and erased when the exchange ends and again when the check is
/// dropped — carried, displaced by a newer offer, or never sent at all.
pub struct Check {
    ticket: Ticket,
    account: String,
    request: WipedBuf,
    len: usize,
}

impl Check {
    /// The request asking whether `secret` authenticates `account`, for the
    /// offer `ticket` names.
    ///
    /// # Errors
    ///
    /// The unreachable answer to an offer that cannot be asked: no memory
    /// for the request, or a name or secret the protocol cannot carry.
    pub fn new(ticket: Ticket, account: &str, secret: &str) -> Result<Self, Checked> {
        Self::encoded(ticket, account, secret).ok_or_else(|| Checked {
            ticket,
            account: String::from(account),
            answer: Answer::unreachable(),
        })
    }

    /// The check, or `None` when its request cannot be held or encoded.
    fn encoded(ticket: Ticket, account: &str, secret: &str) -> Option<Self> {
        let mut request = WipedBuf::with_capacity(SESSION_MAX_REQUEST).ok()?;
        let len = SessionRequest::Authenticate {
            username: account,
            password: Secret::new(secret),
        }
        .encode(request.extend_zeroed(SESSION_MAX_REQUEST)?)
        .ok()?;
        Some(Self {
            ticket,
            account: String::from(account),
            request,
            len,
        })
    }

    /// The request's bytes, for the tests that check what was sent and what
    /// was erased.
    #[cfg(test)]
    pub(crate) fn request_bytes(&self) -> &[u8] {
        &self.request
    }
}

/// The authority's answer to one [`Check`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checked {
    /// The offer it answers.
    pub ticket: Ticket,
    /// The account it was asked about, whose lockout [`answer`](Self::answer)
    /// carries.
    pub account: String,
    /// The answer.
    pub answer: Answer,
}

/// Carry `check` to the authority over `transport`, erasing its request
/// before returning whatever came back.
///
/// It decides nothing. A reply that is not a verdict frame is
/// [`Verdict::Unreachable`], never a pass and never a refusal, because the
/// authority answers a request it will not honour with a well-formed frame
/// of the *other* shape rather than an error.
pub fn exchange<T: SessionTransport>(transport: &mut T, check: &mut Check) -> Checked {
    let mut reply = [0u8; SESSION_MAX_REPLY];
    let answer = match transport.call(&check.request[..check.len], &mut reply) {
        Ok(read) => decode(&reply[..read.min(reply.len())]),
        Err(_) => Answer::unreachable(),
    };
    check.request.wipe();
    Checked {
        ticket: check.ticket,
        account: core::mem::take(&mut check.account),
        answer,
    }
}

/// The answer a reply frame states.
fn decode(reply: &[u8]) -> Answer {
    match SessionVerdict::decode(reply) {
        Ok(SessionVerdict::Accepted) => Answer {
            verdict: Verdict::Verified,
            retry_after: Duration64::ZERO,
        },
        Ok(SessionVerdict::Refused { retry_after }) => Answer {
            verdict: Verdict::Refused,
            retry_after,
        },
        Err(_) => Answer::unreachable(),
    }
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
