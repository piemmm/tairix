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
    let kinds = [
        DesktopAsk::Lock,
        preview(),
        DesktopAsk::NotifySources,
        DesktopAsk::Authorise {
            window_id: 3,
            task: SystemTask::SetClock,
        },
        DesktopAsk::TakeOutput {
            window_id: 3,
            exit_code: 0,
        },
    ];
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

/// A task asked while the last one's acceptance is still outstanding — its
/// outcome event read before the worker handed the acceptance back — is held,
/// the newest replacing any held before it, and handed back once that
/// acceptance lands; a withdrawal drops it.
#[test]
fn a_task_asked_behind_an_outstanding_acceptance_is_held_not_dropped() {
    let task = |window_id| DesktopAsk::Authorise {
        window_id,
        task: SystemTask::ShowAccounts,
    };
    let held = |ask: Option<DesktopAsk>| match ask {
        Some(DesktopAsk::Authorise { window_id, .. }) => Some(window_id),
        _ => None,
    };
    let mut asks = DesktopAsks::new();
    assert!(asks.ask(&task(1)));
    assert!(!asks.ask(&task(2)), "held behind the acceptance");
    assert!(!asks.ask(&task(3)), "the newest replaces it");
    assert_eq!(
        held(asks.answered(&DesktopAnswer::Authorise(Ok(())))),
        Some(3)
    );
    assert!(!asks.ask(&task(4)), "the one handed back is outstanding");
    asks.withdraw(&task(4));
    assert!(asks.ask(&task(5)), "a withdrawal frees the kind");
    assert_eq!(held(asks.answered(&DesktopAnswer::Authorise(Ok(())))), None);
}

/// A refused acceptance ends the task with the reason in the band, and a
/// fetched output is read as what the task printed.
#[test]
fn a_task_answer_is_adopted_into_the_pane_that_asked() {
    let mut shell = showing("ethernet");
    assert!(matches!(
        crate::test_support::asked_task(&mut shell, 0),
        SystemTask::ShowAddressing
    ));
    let refused = DesktopAnswer::Authorise(Err(Errno::SeatBusy));
    assert_eq!(
        refused.refusal(),
        Some(("ask for authorisation", Errno::SeatBusy))
    );
    refused.adopt(&mut shell);
    assert!(!shell.authorising());
    assert!(crate::test_support::band_line(&shell).contains("Something else holds the screen"));

    let _ = crate::test_support::asked_task(&mut shell, 0);
    DesktopAnswer::Authorise(Ok(())).adopt(&mut shell);
    assert!(shell.authorising(), "accepted, and still being performed");
    DesktopAnswer::Output {
        exit_code: 0,
        output: Ok(b"wan.ipv4.method dhcp\nwan.match.mac 52:54:00:12:34:56\n".to_vec()),
    }
    .adopt(&mut shell);
    assert!(!shell.authorising());
    assert!(
        shell.addressing_for_test().document().is_some(),
        "the listing landed"
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
