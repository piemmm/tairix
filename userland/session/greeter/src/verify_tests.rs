use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::session_ipc::{
    encode_account_page, SessionRequest, SessionVerdict, SESSION_LOGIN_NAME_MAX,
};
use tairix_abi::time::Duration64;
use tairix_abi::Errno;
use tairix_greeter::{Asking, Courier, Ticket, Tickets, Verdict, Verifier};

use super::{exchange, Check, Checked};
use crate::accounts::SessionTransport;
use crate::test_courier::Inline;

const SECRET: &str = "hunter2-and-then-some";

/// An authority answering with a fixed verdict, keeping the request bytes it
/// was handed so a test can check what actually went out.
struct Fixed {
    answer: Option<SessionVerdict>,
    seen: Vec<u8>,
}

impl Fixed {
    const fn answering(answer: SessionVerdict) -> Self {
        Self {
            answer: Some(answer),
            seen: Vec::new(),
        }
    }

    const fn silent() -> Self {
        Self {
            answer: None,
            seen: Vec::new(),
        }
    }

    /// The secret the last request carried, if it decoded as one.
    fn offered_secret(&self) -> Option<String> {
        match SessionRequest::decode(&self.seen) {
            Ok(SessionRequest::Authenticate { password, .. }) => Some(String::from(password)),
            _ => None,
        }
    }
}

impl SessionTransport for Fixed {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        self.seen.clear();
        self.seen.extend_from_slice(request);
        match self.answer {
            Some(verdict) => verdict.encode(reply),
            None => Err(Errno::TimedOut),
        }
    }
}

/// An authority answering with a well-formed frame of the wrong shape — what
/// the real one sends for a request it will not honour.
struct WrongShape;

impl SessionTransport for WrongShape {
    fn call(&mut self, _request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        encode_account_page(reply, 0, 0, &[])
    }
}

/// Whether `haystack` contains `needle` anywhere.
fn holds(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|slot| slot == needle)
}

/// A verifier asking an authority answering over `transport`, carried at
/// once.
fn over<T: SessionTransport>(transport: T) -> Asking<Inline<T>> {
    Asking::new(Inline::new(transport))
}

/// A courier that holds every check it is handed, carrying nothing: an
/// authority still deriving its key.
#[derive(Default)]
struct Holding {
    held: Vec<Check>,
}

impl Courier for Holding {
    type Answer = Checked;

    fn send(&mut self, ticket: Ticket, account: &str, secret: &str) -> Result<(), Checked> {
        self.held.push(Check::new(ticket, account, secret)?);
        Ok(())
    }

    fn receive(&mut self) -> Option<Checked> {
        None
    }
}

/// Ask `verifier` about `SECRET` for `ann` and take what lands.
fn verdict_of<C: Courier<Answer = Checked>>(verifier: &mut Asking<C>) -> Checked {
    let ticket = verifier.ask("ann", SECRET);
    let landed = verifier.landed().expect("an answer landed");
    assert_eq!(
        landed.ticket, ticket,
        "the answer names the offer it answers"
    );
    landed
}

#[test]
fn an_accepted_secret_verifies() {
    let landed = verdict_of(&mut over(Fixed::answering(SessionVerdict::Accepted)));
    assert_eq!(landed.answer.verdict, Verdict::Verified);
    assert_eq!(landed.answer.retry_after, Duration64::ZERO);
    assert_eq!(landed.account, "ann");
}

#[test]
fn a_refusal_carries_its_lockout() {
    let retry_after = Duration64::from_secs(30);
    let landed = verdict_of(&mut over(Fixed::answering(SessionVerdict::Refused {
        retry_after,
    })));
    assert_eq!(landed.answer.verdict, Verdict::Refused);
    assert_eq!(landed.answer.retry_after, retry_after);
}

#[test]
fn no_answer_is_unreachable_never_a_refusal() {
    let landed = verdict_of(&mut over(Fixed::silent()));
    assert_eq!(landed.answer.verdict, Verdict::Unreachable);
    assert_eq!(landed.answer.retry_after, Duration64::ZERO);
}

#[test]
fn a_reply_that_is_not_a_verdict_is_unreachable() {
    let landed = verdict_of(&mut over(WrongShape));
    assert_eq!(landed.answer.verdict, Verdict::Unreachable);
}

#[test]
fn an_answer_is_taken_exactly_once() {
    let mut verifier = over(Fixed::answering(SessionVerdict::Accepted));
    assert!(verifier.landed().is_none());
    verifier.ask("ann", SECRET);
    assert!(verifier.landed().is_some());
    assert!(verifier.landed().is_none());
}

/// Asking hands the check over and returns: nothing lands until the courier
/// brings an answer back, and every offer is told apart from the last.
#[test]
fn asking_never_waits_for_the_answer() {
    let mut verifier = Asking::new(Holding::default());
    let first = verifier.ask("ann", SECRET);
    let second = verifier.ask("ann", SECRET);
    assert_ne!(first, second);
    assert_eq!(verifier.courier().held.len(), 2);
    assert!(verifier.landed().is_none(), "nothing came back yet");
}

#[test]
fn a_name_too_long_for_the_wire_never_reaches_the_authority() {
    let mut verifier = over(Fixed::answering(SessionVerdict::Accepted));
    let overlong = "n".repeat(SESSION_LOGIN_NAME_MAX + 1);
    let ticket = verifier.ask(&overlong, SECRET);
    let landed = verifier.landed().expect("answered at once");
    assert_eq!(landed.ticket, ticket);
    assert_eq!(landed.answer.verdict, Verdict::Unreachable);
    assert!(verifier.courier().transport.seen.is_empty());
}

#[test]
fn the_account_and_the_secret_are_what_went_out() {
    let mut verifier = over(Fixed::answering(SessionVerdict::Accepted));
    verifier.ask("ann", SECRET);
    let sent = SessionRequest::decode(&verifier.courier().transport.seen);
    assert!(matches!(
        sent,
        Ok(SessionRequest::Authenticate {
            username: "ann",
            password: SECRET,
        })
    ));
    assert_eq!(
        verifier.courier().transport.offered_secret().as_deref(),
        Some(SECRET)
    );
}

/// The check carries the secret until the exchange ends, and nothing of the
/// request — the secret or any other byte — after it, whatever came back.
#[test]
fn the_request_holds_no_secret_once_the_exchange_ends() {
    let answers = [
        Some(SessionVerdict::Accepted),
        Some(SessionVerdict::Refused {
            retry_after: Duration64::from_secs(5),
        }),
        None,
    ];
    for answer in answers {
        let mut authority = match answer {
            Some(verdict) => Fixed::answering(verdict),
            None => Fixed::silent(),
        };
        let Ok(mut check) = Check::new(Tickets::default().issue(), "ann", SECRET) else {
            panic!("a request fits");
        };
        assert!(holds(check.request_bytes(), SECRET.as_bytes()));
        exchange(&mut authority, &mut check);
        assert!(
            check.request_bytes().iter().all(|byte| *byte == 0),
            "the whole request is erased, not just the secret"
        );
    }
}

#[test]
fn a_secret_the_protocol_cannot_carry_is_never_encoded() {
    let overlong = "n".repeat(SESSION_LOGIN_NAME_MAX + 1);
    let ticket = Tickets::default().issue();
    let refused = Check::new(ticket, &overlong, SECRET).err();
    assert_eq!(
        refused.map(|checked| (checked.ticket, checked.answer.verdict)),
        Some((ticket, Verdict::Unreachable))
    );
}
