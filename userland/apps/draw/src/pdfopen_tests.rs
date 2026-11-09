//! The PDF question window: the page asked for, the password typed, and
//! turning it down.

use alloc::vec::Vec;

use tairix_geometry::{Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
use tairix_theme::ThemeRegistry;
use tairix_window::docapp::{AppRequest, AppView, Relayout};

use super::{Answered, Asked, Given, OpenPdf, Unoffered};
use crate::layout::Faces;
use tairix_sandbox::drawedit::MAX_PASSWORD_BYTES;

struct Fixture {
    registry: ThemeRegistry,
    window: OpenPdf<u8>,
    /// The size it was last laid out at, as `(width, height)`.
    size: (u32, u32),
}

impl Fixture {
    fn asking(asked: Asked) -> Self {
        let mut fixture = Self {
            registry: ThemeRegistry::with_builtins(),
            window: OpenPdf::new("plans.pdf", asked, 7),
            size: (0, 0),
        };
        fixture.held_to_its_least();
        fixture
    }

    fn key(&mut self, key: Key) -> Option<Answered<u8>> {
        let event = InputEvent::KeyPressed {
            key,
            modifiers: Modifiers::default(),
        };
        let theme = self.registry.active();
        let answer = self
            .window
            .input(&event, 0, Scale::ONE, theme, &mut Region::new());
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
        self.size = (width.max(self.size.0), height.max(self.size.1));
        let (width, height) = self.size;
        self.window
            .lay_out(width, height, theme, Scale::ONE, faces, &mut Region::new());
    }

    /// Type `text`, answering the pages whose sizes typing asked for.
    fn typed(&mut self, text: &str) -> Vec<u32> {
        let mut asked = Vec::new();
        for c in text.chars() {
            match self.key(Key::Char(c)) {
                None => {}
                Some(Answered::Size(page)) => asked.push(page),
                Some(_) => panic!("typing answers nothing"),
            }
        }
        asked
    }

    /// The page chosen backed out and `page` typed, counting from 1,
    /// answering the pages whose sizes that asked for.
    fn choose(&mut self, page: &str) -> Vec<u32> {
        let mut asked = Vec::new();
        if let Some(Answered::Size(page)) = self.key(Key::Named(NamedKey::Backspace)) {
            asked.push(page);
        }
        asked.extend(self.typed(page));
        asked
    }

    /// Page `page`'s size come back as `size`, answering what that asks.
    fn sized(&mut self, page: u32, size: Option<(f64, f64)>) -> Option<Answered<u8>> {
        let answer = self.window.sized(page, size, &mut Region::new());
        if answer.relayout != Relayout::None {
            self.held_to_its_least();
        }
        match answer.request {
            Some(AppRequest::Own(answered)) => Some(answered),
            Some(AppRequest::Close) | None => None,
        }
    }

    /// The size the page row states beside its number.
    fn described(&self) -> Option<&str> {
        self.window.group.rows().first()?.description()
    }

    /// The page the window opens: its answer to Enter.
    fn opened(&mut self) -> u32 {
        match self.key(Key::Named(NamedKey::Enter)) {
            Some(Answered::Open {
                given: Given::Page(page),
                ..
            }) => page,
            _ => panic!("a page opened"),
        }
    }
}

fn pages(count: u32) -> Asked {
    Asked::Page {
        count,
        first: Some((595.0, 842.0)),
    }
}

#[test]
fn enter_opens_the_first_page_unless_another_is_chosen() {
    let mut fixture = Fixture::asking(pages(3));
    assert_eq!(fixture.described(), Some("595 \u{d7} 842 points"));
    let Some(Answered::Open {
        ticket,
        given: Given::Page(page),
    }) = fixture.key(Key::Named(NamedKey::Enter))
    else {
        panic!("an answer");
    };
    assert_eq!((ticket, page), (7, 0));
}

#[test]
fn a_pages_size_is_asked_for_as_its_number_comes_to_it_and_shown_once_known() {
    let mut fixture = Fixture::asking(pages(3));
    assert_eq!(fixture.choose("3"), [2]);
    assert_eq!(fixture.described(), None, "unknown until it comes back");
    assert!(fixture.sized(2, Some((842.0, 595.0))).is_none());
    assert_eq!(fixture.described(), Some("842 \u{d7} 595 points"));
    assert_eq!(fixture.opened(), 2);
}

#[test]
fn one_size_is_asked_for_at_a_time_and_the_page_chosen_last_wins() {
    let mut fixture = Fixture::asking(pages(5));
    assert_eq!(fixture.choose("2"), [1]);
    assert_eq!(
        fixture.choose("4"),
        [],
        "the first size is still being fetched"
    );
    let next = fixture.sized(1, Some((100.0, 200.0)));
    assert!(matches!(next, Some(Answered::Size(3))), "{next:?}");
    assert_eq!(fixture.described(), None, "page two is no longer chosen");
    assert!(
        fixture.sized(1, Some((1.0, 1.0))).is_none(),
        "an answer no one awaits"
    );
    assert!(fixture.sized(3, Some((300.0, 400.0))).is_none());
    assert_eq!(fixture.described(), Some("300 \u{d7} 400 points"));
}

#[test]
fn a_size_that_could_not_be_had_is_not_asked_for_again_until_another_is_chosen() {
    let mut fixture = Fixture::asking(pages(3));
    assert_eq!(fixture.choose("2"), [1]);
    assert!(fixture.sized(1, None).is_none());
    assert_eq!(fixture.described(), None);
    assert_eq!(fixture.choose("2"), [], "not asked again");
    assert_eq!(fixture.choose("3"), [2]);
    assert_eq!(fixture.opened(), 2, "a page of no known size still opens");
}

#[test]
fn a_first_page_of_no_size_known_is_shown_without_one_and_another_still_asked() {
    let mut fixture = Fixture::asking(Asked::Page {
        count: 3,
        first: None,
    });
    assert_eq!(fixture.described(), None);
    assert_eq!(fixture.choose("1"), [], "not asked again");
    assert_eq!(fixture.choose("2"), [1]);
    assert!(fixture.sized(1, Some((10.0, 20.0))).is_none());
    assert_eq!(fixture.described(), Some("10 \u{d7} 20 points"));
}

#[test]
fn the_name_asked_about_is_shown_with_what_hides_in_it_made_visible() {
    let fixture = Fixture {
        registry: ThemeRegistry::with_builtins(),
        window: OpenPdf::new("photo\u{202e}fdp.exe", pages(2), 7),
        size: (0, 0),
    };
    assert!(
        !fixture.window.title().contains('\u{202e}'),
        "{}",
        fixture.window.title()
    );
    let message = fixture.window.dialog.message().unwrap_or_default();
    assert!(!message.contains('\u{202e}'), "{message}");
}

#[test]
fn a_page_number_typed_is_the_page_opened() {
    let mut fixture = Fixture::asking(pages(3));
    let _ = fixture.choose("3");
    assert_eq!(fixture.opened(), 2);
}

/// A page out of range blocks Open: it is held to the range and shown, and
/// only what is shown opens.
#[test]
fn a_page_past_the_last_is_held_to_it_before_it_opens() {
    for (count, typed, held) in [(3, "9", 2), (200, "250", 199), (3, "0", 0)] {
        let mut fixture = Fixture::asking(pages(count));
        let _ = fixture.choose(typed);
        assert!(
            matches!(
                fixture.key(Key::Named(NamedKey::Enter)),
                None | Some(Answered::Size(_))
            ),
            "{typed} is shown corrected first"
        );
        assert_eq!(fixture.opened(), held, "{typed}");
    }
}

#[test]
fn a_page_typed_and_left_unfinished_still_opens_as_typed() {
    let mut fixture = Fixture::asking(pages(3));
    let _ = fixture.choose("2");
    assert_eq!(fixture.opened(), 1);
}

#[test]
fn the_password_typed_is_handed_over() {
    let mut fixture = Fixture::asking(Asked::Password { wrong: false });
    let _ = fixture.typed("s3cret");
    let Some(Answered::Open {
        ticket,
        given: Given::Password(password),
    }) = fixture.key(Key::Named(NamedKey::Enter))
    else {
        panic!("an answer");
    };
    assert_eq!(ticket, 7);
    assert_eq!(&*password, b"s3cret");
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
    assert_eq!(fixture.window.ticket(), Some(&7));
    assert_eq!(fixture.window.take_ticket(), Some(7));
    assert_eq!(fixture.window.ticket(), None);
}

#[test]
fn no_password_typed_answers_nothing_and_says_why() {
    let mut fixture = Fixture::asking(Asked::Password { wrong: false });
    assert!(fixture.key(Key::Named(NamedKey::Enter)).is_none());
    assert_eq!(fixture.window.dialog.reason(), Some(Unoffered::Empty.why()));
    let _ = fixture.typed("k");
    assert_eq!(fixture.window.dialog.reason(), None, "typing takes it down");
    let Some(Answered::Open {
        given: Given::Password(password),
        ..
    }) = fixture.key(Key::Named(NamedKey::Enter))
    else {
        panic!("an answer");
    };
    assert_eq!(&*password, b"k");
}

/// More than a password can be is never offered as its prefix: it is said,
/// and typed again whole.
#[test]
fn a_password_past_the_bound_is_refused_and_typed_again() {
    let mut fixture = Fixture::asking(Asked::Password { wrong: false });
    let _ = fixture.typed(&"x".repeat(MAX_PASSWORD_BYTES + 1));
    assert!(fixture.key(Key::Named(NamedKey::Enter)).is_none());
    assert_eq!(
        fixture.window.dialog.reason(),
        Some(Unoffered::TooLong.why())
    );
    let _ = fixture.typed("right");
    assert_eq!(fixture.window.dialog.reason(), None);
    let Some(Answered::Open {
        given: Given::Password(password),
        ..
    }) = fixture.key(Key::Named(NamedKey::Enter))
    else {
        panic!("an answer");
    };
    assert_eq!(&*password, b"right");
}

#[test]
fn a_password_as_long_as_the_bound_is_offered_whole() {
    let mut fixture = Fixture::asking(Asked::Password { wrong: false });
    let longest = "y".repeat(MAX_PASSWORD_BYTES);
    let _ = fixture.typed(&longest);
    let Some(Answered::Open {
        given: Given::Password(password),
        ..
    }) = fixture.key(Key::Named(NamedKey::Enter))
    else {
        panic!("an answer");
    };
    assert_eq!(&*password, longest.as_bytes());
}

/// The password field's marker moves only while the window keeps its time.
#[test]
fn the_window_keeps_its_fields_time() {
    let mut fixture = Fixture::asking(Asked::Password { wrong: false });
    assert_eq!(fixture.window.deadline_ns(0), None);
    let _ = fixture.typed("a");
    let due = fixture.window.deadline_ns(0).expect("the marker moves");
    let mut damage = Region::new();
    let theme = fixture.registry.active();
    fixture.window.advance(due, Scale::ONE, theme, &mut damage);
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
    let _ = fixture.typed("a");
    let due = fixture.window.deadline_ns(0).expect("the marker moves");
    let theme = fixture.registry.active();
    fixture
        .window
        .advance(due, Scale::ONE, theme, &mut Region::new());
    assert!(fixture
        .window
        .deadline_ns(due)
        .is_some_and(|next| next > due));
}
