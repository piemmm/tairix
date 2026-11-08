//! The PDF question window: the page asked for, the password typed, and
//! turning it down.

use alloc::vec;

use tairix_geometry::{Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
use tairix_theme::ThemeRegistry;
use tairix_window::docapp::{AppRequest, AppView, Relayout};

use super::{Answered, Asked, OpenPdf, Unoffered};
use crate::layout::Faces;
use tairix_sandbox::drawedit::MAX_PASSWORD_BYTES;

struct Fixture {
    registry: ThemeRegistry,
    window: OpenPdf<u8>,
    layout: Rect,
}

impl Fixture {
    fn asking(asked: Asked) -> Self {
        let registry = ThemeRegistry::with_builtins();
        let window = OpenPdf::new("plans.pdf", asked, 7);
        let theme = registry.active();
        let (width, height) = window.min_size(theme, Scale::ONE, Faces::of(theme, Scale::ONE));
        let layout = window.layout(
            width,
            height,
            theme,
            Scale::ONE,
            Faces::of(theme, Scale::ONE),
        );
        Self {
            registry,
            window,
            layout,
        }
    }

    fn key(&mut self, key: Key) -> Option<Answered<u8>> {
        let event = InputEvent::KeyPressed {
            key,
            modifiers: Modifiers::default(),
        };
        let theme = self.registry.active();
        let answer = self.window.input(
            &event,
            0,
            &self.layout,
            Scale::ONE,
            theme,
            &mut Region::new(),
        );
        if answer.relayout != Relayout::None {
            self.held_to_its_least();
        }
        match answer.request {
            Some(AppRequest::Own(answered)) => Some(answered),
            Some(AppRequest::Close) | None => None,
        }
    }

    /// Laid out again, held to the least size it now states, as the session
    /// holds it.
    fn held_to_its_least(&mut self) {
        let theme = self.registry.active();
        let faces = Faces::of(theme, Scale::ONE);
        let (width, height) = self.window.min_size(theme, Scale::ONE, faces);
        self.layout = self.window.layout(
            width.max(self.layout.width),
            height.max(self.layout.height),
            theme,
            Scale::ONE,
            faces,
        );
    }

    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            assert!(self.key(Key::Char(c)).is_none(), "typing answers nothing");
        }
    }
}

fn pages(count: u32) -> Asked {
    Asked::Page {
        count,
        sizes: vec![(595.0, 842.0), (842.0, 595.0), (612.0, 792.0)],
    }
}

#[test]
fn enter_opens_the_first_page_unless_another_is_chosen() {
    let mut fixture = Fixture::asking(pages(3));
    let Some(Answered::Open {
        ticket,
        page,
        password,
    }) = fixture.key(Key::Named(NamedKey::Enter))
    else {
        panic!("an answer");
    };
    assert_eq!((ticket, page), (7, Some(0)));
    assert!(password.is_none());
}

#[test]
fn a_page_number_typed_is_the_page_opened() {
    let mut fixture = Fixture::asking(pages(3));
    let _ = fixture.key(Key::Named(NamedKey::Backspace));
    fixture.typed("3");
    let Some(Answered::Open { page, .. }) = fixture.key(Key::Named(NamedKey::Enter)) else {
        panic!("an answer");
    };
    assert_eq!(page, Some(2));
}

/// A page out of range blocks Open: it is held to the range and shown, and
/// only what is shown opens.
#[test]
fn a_page_past_the_last_is_held_to_it_before_it_opens() {
    for (count, typed, held) in [(3, "9", 2), (200, "250", 199), (3, "0", 0)] {
        let mut fixture = Fixture::asking(pages(count));
        let _ = fixture.key(Key::Named(NamedKey::Backspace));
        fixture.typed(typed);
        assert!(
            fixture.key(Key::Named(NamedKey::Enter)).is_none(),
            "{typed} is shown corrected first"
        );
        let Some(Answered::Open { page, .. }) = fixture.key(Key::Named(NamedKey::Enter)) else {
            panic!("an answer");
        };
        assert_eq!(page, Some(held), "{typed}");
    }
}

#[test]
fn a_page_typed_and_left_unfinished_still_opens_as_typed() {
    let mut fixture = Fixture::asking(pages(3));
    let _ = fixture.key(Key::Named(NamedKey::Backspace));
    fixture.typed("2");
    let Some(Answered::Open { page, .. }) = fixture.key(Key::Named(NamedKey::Enter)) else {
        panic!("an answer");
    };
    assert_eq!(page, Some(1));
}

#[test]
fn the_password_typed_is_handed_over() {
    let mut fixture = Fixture::asking(Asked::Password { wrong: false });
    fixture.typed("s3cret");
    let Some(Answered::Open {
        ticket,
        page,
        password,
    }) = fixture.key(Key::Named(NamedKey::Enter))
    else {
        panic!("an answer");
    };
    assert_eq!((ticket, page), (7, None));
    assert_eq!(password.as_deref(), Some(&b"s3cret"[..]));
}

#[test]
fn escape_turns_the_document_down_and_answers_once() {
    let mut fixture = Fixture::asking(Asked::Password { wrong: true });
    let Some(Answered::Cancelled(ticket)) = fixture.key(Key::Named(NamedKey::Escape)) else {
        panic!("turned down");
    };
    assert_eq!(ticket, 7);
    assert!(
        fixture.key(Key::Named(NamedKey::Enter)).is_none(),
        "an answered window answers no more"
    );
    assert!(fixture.window.take_ticket().is_none());
}

#[test]
fn a_window_closed_unanswered_hands_its_ticket_back() {
    let mut fixture = Fixture::asking(pages(2));
    assert_eq!(fixture.window.take_ticket(), Some(7));
    assert_eq!(fixture.window.asked, pages(2));
}

#[test]
fn no_password_typed_answers_nothing_and_says_why() {
    let mut fixture = Fixture::asking(Asked::Password { wrong: false });
    assert!(fixture.key(Key::Named(NamedKey::Enter)).is_none());
    assert_eq!(fixture.window.dialog.reason(), Some(Unoffered::Empty.why()));
    fixture.typed("k");
    assert_eq!(fixture.window.dialog.reason(), None, "typing takes it down");
    let Some(Answered::Open { password, .. }) = fixture.key(Key::Named(NamedKey::Enter)) else {
        panic!("an answer");
    };
    assert_eq!(password.as_deref(), Some(&b"k"[..]));
}

/// More than a password can be is never offered as its prefix: it is said,
/// and typed again whole.
#[test]
fn a_password_past_the_bound_is_refused_and_typed_again() {
    let mut fixture = Fixture::asking(Asked::Password { wrong: false });
    fixture.typed(&"x".repeat(MAX_PASSWORD_BYTES + 1));
    assert!(fixture.key(Key::Named(NamedKey::Enter)).is_none());
    assert_eq!(
        fixture.window.dialog.reason(),
        Some(Unoffered::TooLong.why())
    );
    fixture.typed("right");
    assert_eq!(fixture.window.dialog.reason(), None);
    let Some(Answered::Open { password, .. }) = fixture.key(Key::Named(NamedKey::Enter)) else {
        panic!("an answer");
    };
    assert_eq!(password.as_deref(), Some(&b"right"[..]));
}

#[test]
fn a_password_as_long_as_the_bound_is_offered_whole() {
    let mut fixture = Fixture::asking(Asked::Password { wrong: false });
    let longest = "y".repeat(MAX_PASSWORD_BYTES);
    fixture.typed(&longest);
    let Some(Answered::Open { password, .. }) = fixture.key(Key::Named(NamedKey::Enter)) else {
        panic!("an answer");
    };
    assert_eq!(password.as_deref(), Some(longest.as_bytes()));
}

/// The password field's marker moves only while the window keeps its time.
#[test]
fn the_window_keeps_its_fields_time() {
    let mut fixture = Fixture::asking(Asked::Password { wrong: false });
    assert_eq!(fixture.window.deadline_ns(0), None);
    fixture.typed("a");
    let due = fixture.window.deadline_ns(0).expect("the marker moves");
    let mut damage = Region::new();
    let theme = fixture.registry.active();
    fixture
        .window
        .advance(due, &fixture.layout, Scale::ONE, theme, &mut damage);
    assert!(!damage.is_empty(), "the marker redrawn");
    assert!(fixture
        .window
        .deadline_ns(due)
        .is_some_and(|next| next > due));
}

/// Laid out where its form has no room, the window's clock still moves, so
/// a deadline never stays passed.
#[test]
fn a_window_too_small_for_its_form_still_keeps_time() {
    let mut fixture = Fixture::asking(Asked::Password { wrong: false });
    fixture.typed("a");
    let due = fixture.window.deadline_ns(0).expect("the marker moves");
    let theme = fixture.registry.active();
    fixture.window.advance(
        due,
        &Rect::new(0, 0, 2, 2),
        Scale::ONE,
        theme,
        &mut Region::new(),
    );
    assert!(fixture
        .window
        .deadline_ns(due)
        .is_some_and(|next| next > due));
}
