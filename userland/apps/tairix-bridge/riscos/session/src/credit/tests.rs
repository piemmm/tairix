//! Credited input: the window closing after its events and reopening as
//! the worker takes them in, and the inbox refusing what is beyond it.

use super::*;

#[test]
fn the_window_closes_after_its_events_and_opens_as_they_are_taken_in() {
    let mut credit = Credit::new();
    for _ in 0..INPUT_WINDOW {
        credit.send().expect("within the window");
    }
    assert!(!credit.open());
    assert_eq!(credit.send(), Err(CreditError::Beyond));
    credit.taken(3).expect("three taken in");
    assert_eq!(credit.outstanding(), INPUT_WINDOW - 3);
    assert!(credit.open());
    assert_eq!(
        credit.taken(2),
        Err(CreditError::Miscounted),
        "fewer than before"
    );
    assert_eq!(
        credit.taken(INPUT_WINDOW + 1),
        Err(CreditError::Miscounted),
        "more than sent"
    );
    credit.taken(INPUT_WINDOW).expect("all taken in");
    assert_eq!(credit.outstanding(), 0);
}

#[test]
fn counts_wrap_past_two_to_the_thirty_two() {
    let mut credit = Credit {
        sent: u32::MAX,
        applied: u32::MAX,
    };
    credit.send().expect("sent");
    credit.taken(0).expect("taken in, wrapped");
    assert_eq!(credit.outstanding(), 0);
}

#[test]
fn an_inbox_takes_in_its_events_in_order_and_refuses_one_beyond_the_window() {
    let mut inbox = Inbox::new().expect("room");
    let room = inbox.events.capacity();
    for code in 0..INPUT_WINDOW {
        let code = u8::try_from(code).expect("a byte");
        inbox
            .receive(InputEvent::Key(code))
            .expect("within the window");
    }
    assert_eq!(inbox.receive(InputEvent::Key(0)), Err(CreditError::Beyond));
    assert_eq!(
        inbox.events.capacity(),
        room,
        "a whole window held in the room it began with"
    );
    assert_eq!(inbox.take(), Some(InputEvent::Key(0)));
    assert_eq!(inbox.take(), Some(InputEvent::Key(1)));
    assert_eq!(inbox.applied(), 2);
    inbox.receive(InputEvent::Key(9)).expect("room again");
    while inbox.take().is_some() {}
    assert!(!inbox.waiting());
    assert_eq!(inbox.applied(), INPUT_WINDOW + 1);
}
