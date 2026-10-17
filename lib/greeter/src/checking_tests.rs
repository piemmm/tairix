//! Unit tests for the offer being checked: what the surface does between
//! asking the authority and adopting its answer, and which answers it adopts.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::Duration64;
use tairix_input::{InputEvent, Key, NamedKey};

use crate::asking::{Asking, Courier};
use crate::chooser::AccountTile;
use crate::surface::{
    AuthSurface, Ticket, Tickets, Verdict, Verifier, CHECKING, CHOOSE_HINT, REFUSED,
};
use crate::testkit::{ask_in, feed, key, named, theme, Scripted, PRESS, RELEASE};

/// Apply `event`, answering no question.
fn press(surface: &mut AuthSurface, event: &InputEvent, verifier: &mut Scripted) {
    ask_in(surface, event, verifier, 0, &theme());
}

/// Type `secret` and press Enter, leaving every question unanswered.
fn offer(surface: &mut AuthSurface, secret: &str, verifier: &mut Scripted) {
    for ch in secret.chars() {
        press(surface, &key(Key::Char(ch)), verifier);
    }
    press(surface, &named(NamedKey::Enter), verifier);
}

#[test]
fn an_offered_secret_is_checked_and_the_field_takes_nothing_meanwhile() {
    let theme = theme();
    let mut surface = AuthSurface::new("ann", "ann");
    let mut verifier = Scripted::new(vec![Verdict::Refused]);
    offer(&mut surface, "pw", &mut verifier);
    assert_eq!(surface.notice(), CHECKING);
    assert_eq!(
        surface.motion_due(0),
        None,
        "the erased secret has no marker"
    );

    for event in [
        key(Key::Char('x')),
        named(NamedKey::Backspace),
        named(NamedKey::Enter),
        PRESS,
        RELEASE,
    ] {
        let outcome = ask_in(&mut surface, &event, &mut verifier, 0, &theme);
        assert!(!outcome.redraw(), "{event:?} reached a field being checked");
    }
    assert_eq!(verifier.waiting.len(), 1, "one question, asked once");

    let answered = verifier.answer(&mut surface, 0, &theme);
    assert!(answered.is_some_and(|outcome| !outcome.verified()));
    assert_eq!(surface.notice(), REFUSED);
    feed(&mut surface, &named(NamedKey::Enter), &mut verifier);
    assert_eq!(
        verifier.offered,
        vec![String::from("pw"), String::new()],
        "nothing typed while checking reached the field"
    );
}

#[test]
fn only_the_answer_the_surface_waits_for_is_adopted() {
    let theme = theme();
    let mut surface = AuthSurface::with_accounts(vec![AccountTile::new("Ann Example", "ann")]);
    let mut verifier = Scripted::new(vec![Verdict::Verified, Verdict::Refused]);
    press(&mut surface, &named(NamedKey::Enter), &mut verifier);
    offer(&mut surface, "first", &mut verifier);

    press(&mut surface, &named(NamedKey::Escape), &mut verifier);
    assert_eq!(
        surface.notice(),
        CHOOSE_HINT,
        "Escape left the check behind"
    );
    press(&mut surface, &named(NamedKey::Enter), &mut verifier);
    offer(&mut surface, "second", &mut verifier);
    assert_eq!(verifier.waiting.len(), 2);

    let stale = verifier.answer(&mut surface, 0, &theme);
    assert!(
        stale.is_some_and(|outcome| !outcome.verified() && !outcome.redraw()),
        "the answer to the offer left behind verifies nothing"
    );
    assert_eq!(surface.notice(), CHECKING, "still waiting on the second");

    let current = verifier.answer(&mut surface, 0, &theme);
    assert!(current.is_some_and(|outcome| !outcome.verified() && outcome.redraw()));
    assert_eq!(surface.notice(), REFUSED);
    assert_eq!(
        verifier.offered,
        vec![String::from("first"), String::from("second")]
    );
}

#[test]
fn an_answer_reaching_a_leaving_screen_changes_nothing() {
    let theme = theme();
    let mut surface = AuthSurface::new("ann", "ann");
    let mut verifier = Scripted::new(vec![Verdict::Verified]);
    offer(&mut surface, "pw", &mut verifier);
    surface.begin_session_fade(0, &theme);

    let answered = verifier.answer(&mut surface, 0, &theme);
    assert!(answered.is_some_and(|outcome| !outcome.verified() && !outcome.redraw()));
}

#[test]
fn a_lockout_running_out_while_an_offer_is_checked_leaves_it_checking() {
    let mut surface = AuthSurface::new("ann", "ann");
    let mut verifier = Scripted::refusing();
    offer(&mut surface, "pw", &mut verifier);

    surface.set_cooldown(Duration64::from_secs(5));
    assert_ne!(surface.notice(), CHECKING);
    surface.set_cooldown(Duration64::ZERO);

    assert_eq!(surface.notice(), CHECKING);
}

#[test]
fn no_ticket_is_issued_twice() {
    let mut tickets = Tickets::default();
    let issued: Vec<_> = (0..64).map(|_| tickets.issue()).collect();
    for (at, ticket) in issued.iter().enumerate() {
        assert!(
            !issued[at + 1..].contains(ticket),
            "{ticket:?} issued twice"
        );
    }
}

/// A courier that will not carry any offer for `refused`, and carries every
/// other one — answering it refused at once when `answers`, else holding it.
#[derive(Default)]
struct Held {
    refused: &'static str,
    answers: bool,
    carried: Vec<(Ticket, String)>,
    back: Vec<(Ticket, Verdict)>,
}

impl Courier for Held {
    type Answer = (Ticket, Verdict);

    fn send(&mut self, ticket: Ticket, account: &str, secret: &str) -> Result<(), Self::Answer> {
        if account == self.refused {
            return Err((ticket, Verdict::Unreachable));
        }
        self.carried.push((ticket, String::from(secret)));
        if self.answers {
            self.back.push((ticket, Verdict::Refused));
        }
        Ok(())
    }

    fn receive(&mut self) -> Option<Self::Answer> {
        (!self.back.is_empty()).then(|| self.back.remove(0))
    }
}

#[test]
fn asking_carries_each_offer_under_its_own_ticket_and_waits_for_none() {
    let mut asking = Asking::new(Held::default());
    let first = asking.ask("ann", "one");
    let second = asking.ask("ann", "two");
    assert_ne!(first, second);
    assert_eq!(
        asking.courier().carried,
        vec![(first, String::from("one")), (second, String::from("two"))]
    );
    assert_eq!(asking.landed(), None, "nothing has come back");
}

#[test]
fn an_offer_that_cannot_be_carried_is_answered_before_anything_carried() {
    let mut asking = Asking::new(Held {
        refused: "bob",
        answers: true,
        ..Held::default()
    });
    let carried = asking.ask("ann", "pw");
    let refused = asking.ask("bob", "pw");
    assert_eq!(asking.landed(), Some((refused, Verdict::Unreachable)));
    assert_eq!(asking.landed(), Some((carried, Verdict::Refused)));
    assert_eq!(asking.landed(), None);
    assert_eq!(
        asking.courier().carried.len(),
        1,
        "bob's offer was never carried"
    );
}
