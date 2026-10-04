//! Unit tests for what only the desktop answers ([`super::DesktopAsks`]).

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::window_ipc::{
    decode_notify_sources_reply, encode_notify_sources_reply, WINDOW_NOTIFY_SOURCES_REPLY_MAX,
};

use super::*;
use crate::test_support::showing;

fn preview() -> DesktopAsk {
    DesktopAsk::Preview(PinboardDocument::new("screensaver.kind = clock\n").expect("a document"))
}

/// One of each kind is outstanding at a time: a second asked while the
/// first is in flight is not submitted, and an answer or a withdrawal frees
/// its kind alone.
#[test]
fn a_second_ask_of_a_kind_waits_for_the_first_to_be_answered() {
    let mut asks = DesktopAsks::new();
    assert!(asks.ask(&DesktopAsk::Lock));
    assert!(!asks.ask(&DesktopAsk::Lock), "the lock already asked");
    assert!(asks.ask(&preview()), "a preview beside it");
    assert!(asks.ask(&DesktopAsk::NotifySources));
    assert!(!asks.ask(&DesktopAsk::NotifySources));
    asks.answered(&DesktopAnswer::Lock(Ok(())));
    assert!(asks.ask(&DesktopAsk::Lock), "answered, so free again");
    assert!(!asks.ask(&preview()), "the preview still outstanding");
    asks.withdraw(&preview());
    assert!(asks.ask(&preview()), "withdrawn, so free again");
    let mut fresh = DesktopAsks::new();
    let kinds = [DesktopAsk::Lock, preview(), DesktopAsk::NotifySources];
    let submitted = kinds.iter().filter(|ask| fresh.ask(ask)).count();
    assert_eq!(submitted, MOST_OUTSTANDING, "one of each kind at most");
    assert!(kinds.iter().all(|ask| !fresh.ask(ask)));
}

/// A preview of another document asked while one is outstanding is held, the
/// newest replacing any held before it, and handed back to be asked once the
/// first is answered; asking for the one outstanding again holds nothing.
#[test]
fn a_preview_of_another_document_is_asked_once_the_first_is_answered() {
    let of = |kind: &str| {
        DesktopAsk::Preview(
            PinboardDocument::new(&alloc::format!("screensaver.kind = {kind}\n"))
                .expect("a document"),
        )
    };
    let document = |ask: Option<DesktopAsk>| match ask {
        Some(DesktopAsk::Preview(document)) => Some(document),
        _ => None,
    };
    let mut asks = DesktopAsks::new();
    assert!(asks.ask(&of("clock")));
    assert!(!asks.ask(&of("ribbon")));
    assert!(!asks.ask(&of("blank")), "the newest replaces the one held");
    let answered = DesktopAnswer::Preview(Err(Errno::SeatBusy));
    assert_eq!(
        document(asks.answered(&answered)),
        document(Some(of("blank")))
    );
    assert!(
        !asks.ask(&of("blank")),
        "the one handed back is outstanding"
    );
    assert_eq!(asks.answered(&answered).map(|_| ()), None, "nothing held");
    assert!(asks.ask(&of("clock")));
    assert!(!asks.ask(&of("ribbon")));
    assert!(!asks.ask(&of("clock")), "back to the one outstanding");
    assert_eq!(asks.answered(&answered).map(|_| ()), None);
}

/// An answer lands in the pane: the sources listed, a refusal stated.
#[test]
fn an_answer_is_adopted_into_the_pane_that_asked() {
    let mut shell = showing("notifications");
    assert!(shell.notify_sources_wanted());
    let chat = BundleId::new("com.example.chat").expect("an identity");
    DesktopAnswer::NotifySources(Ok(vec![chat])).adopt(&mut shell);
    assert!(!shell.notify_sources_wanted(), "the answer landed");
    let refused = DesktopAnswer::NotifySources(Err(Errno::PermissionDenied));
    assert_eq!(
        refused.refusal(),
        Some(("say which programs have notified", Errno::PermissionDenied))
    );
    assert_eq!(DesktopAnswer::Lock(Ok(())).refusal(), None);
    assert_eq!(
        DesktopAnswer::Preview(Err(Errno::SeatBusy)).refusal(),
        Some(("show the screensaver", Errno::SeatBusy))
    );
}

/// A name no bundle could carry is dropped, the rest kept in the order the
/// desktop named them.
#[test]
fn only_names_a_bundle_could_carry_are_offered() {
    let names: [&[u8]; 4] = [
        b"com.example.chat",
        b"not a bundle!",
        b"\xff\xfe",
        b"org.example.mail",
    ];
    let mut frame = [0u8; WINDOW_NOTIFY_SOURCES_REPLY_MAX];
    let n = encode_notify_sources_reply(&mut frame, Ok(names));
    let answered = decode_notify_sources_reply(&frame[..n]).expect("a reply");
    let offered: Vec<_> = notified(&answered)
        .iter()
        .map(|id| alloc::string::String::from(id.as_str()))
        .collect();
    assert_eq!(offered, ["com.example.chat", "org.example.mail"]);
}
