use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_browse::NetworkItem;
use tairix_discovery::DiscoveryError;

use super::{
    refusal, Browse, Need, NetworkAnswer, NetworkAsk, NetworkLanded, NetworkReading, PACE_NS,
};

const WANTED: Need = Need {
    wanted: true,
    waiting: false,
    rung: false,
};

const WAITING: Need = Need {
    wanted: true,
    waiting: true,
    rung: false,
};

const RUNG: Need = Need {
    wanted: true,
    waiting: false,
    rung: true,
};

const LEFT: Need = Need {
    wanted: false,
    waiting: false,
    rung: false,
};

fn items() -> Vec<NetworkItem> {
    alloc::vec![NetworkItem::Truncated]
}

fn read(unsettled: bool) -> NetworkAnswer {
    NetworkAnswer::Read(Ok(NetworkReading {
        items: items(),
        unsettled,
    }))
}

/// A browse whose first collect has answered `answer`.
fn answered(answer: NetworkAnswer) -> Browse {
    let mut browse = Browse::default();
    assert_eq!(browse.next(WAITING, 0), Some(NetworkAsk::Collect));
    let _ = browse.landed(answer, 0);
    browse
}

#[test]
fn nothing_is_asked_while_no_window_wants_the_network() {
    let mut browse = Browse::default();
    assert_eq!(browse.next(LEFT, 0), None);
    assert_eq!(
        browse.next(Need { rung: true, ..LEFT }, 0),
        None,
        "a stray ring opens nothing"
    );
}

#[test]
fn the_first_window_opens_the_session_and_the_last_to_leave_closes_it() {
    let mut browse = Browse::default();
    assert_eq!(browse.next(WAITING, 0), Some(NetworkAsk::Collect));
    assert_eq!(browse.landed(read(false), 0), NetworkLanded::Items);
    assert_eq!(browse.next(WANTED, 1), None, "nothing moved");
    assert_eq!(browse.next(LEFT, 2), Some(NetworkAsk::Close));
    assert_eq!(browse.latest(), None, "a closed session shows nothing");
    assert_eq!(browse.next(LEFT, 3), None, "closed once");
    assert_eq!(
        browse.landed(NetworkAnswer::Closed, 4),
        NetworkLanded::Nothing
    );
    assert_eq!(browse.next(WAITING, 5), Some(NetworkAsk::Collect));
}

#[test]
fn one_collect_is_out_at_a_time_and_a_ring_meanwhile_owes_one_more() {
    let mut browse = Browse::default();
    assert_eq!(browse.next(WAITING, 0), Some(NetworkAsk::Collect));
    assert_eq!(browse.next(WAITING, 1), None, "waiting asks nothing more");
    assert_eq!(browse.next(RUNG, 2), None, "one is out");
    assert_eq!(browse.landed(read(false), 3), NetworkLanded::Items);
    assert_eq!(browse.next(WANTED, 4), Some(NetworkAsk::Collect));
    assert_eq!(browse.landed(read(false), 5), NetworkLanded::Items);
    assert_eq!(browse.next(WANTED, 6), None, "owed once");
}

#[test]
fn a_ring_asks_again() {
    let mut browse = answered(read(false));
    assert_eq!(browse.next(RUNG, 1), Some(NetworkAsk::Collect));
}

#[test]
fn a_window_arriving_while_an_answer_is_held_is_shown_it_without_a_collect() {
    let mut browse = answered(read(false));
    assert_eq!(browse.latest(), Some((1, items().as_slice())));
    assert_eq!(browse.next(WAITING, 1), None);
}

#[test]
fn each_answer_is_told_from_the_last() {
    let mut browse = answered(read(false));
    assert_eq!(browse.next(RUNG, 1), Some(NetworkAsk::Collect));
    let _ = browse.landed(read(false), 2);
    assert_eq!(browse.latest().map(|(generation, _)| generation), Some(2));
}

#[test]
fn a_pass_that_left_work_is_asked_again_at_its_pace_and_not_before() {
    let mut browse = answered(read(true));
    assert_eq!(browse.retry_at(), Some(PACE_NS));
    assert_eq!(browse.next(WANTED, PACE_NS - 1), None);
    assert_eq!(browse.next(WANTED, PACE_NS), Some(NetworkAsk::Collect));
    assert_eq!(browse.retry_at(), None);
    let _ = browse.landed(read(false), PACE_NS + 1);
    assert_eq!(browse.retry_at(), None, "a settled pass owes nothing");
}

#[test]
fn a_catalogue_that_cannot_be_read_is_asked_again_only_when_a_window_asks() {
    let mut browse = Browse::default();
    assert_eq!(browse.next(WAITING, 0), Some(NetworkAsk::Collect));
    assert_eq!(
        browse.landed(NetworkAnswer::Read(Err(DiscoveryError::Unavailable)), 0),
        NetworkLanded::Failed(DiscoveryError::Unavailable)
    );
    assert_eq!(browse.latest(), None);
    assert_eq!(
        browse.next(WANTED, 1),
        None,
        "an absent service is not pressed"
    );
    assert_eq!(
        browse.next(WANTED, 10 * PACE_NS),
        None,
        "nor retried by time"
    );
    assert_eq!(
        browse.next(WAITING, 10 * PACE_NS),
        Some(NetworkAsk::Collect),
        "a refresh asks again"
    );
}

#[test]
fn an_answer_landing_after_every_window_left_shows_nothing() {
    let mut browse = Browse::default();
    assert_eq!(browse.next(WAITING, 0), Some(NetworkAsk::Collect));
    assert_eq!(browse.next(LEFT, 1), Some(NetworkAsk::Close));
    assert_eq!(browse.landed(read(false), 2), NetworkLanded::Nothing);
    assert_eq!(browse.latest(), None);
    assert_eq!(
        browse.landed(NetworkAnswer::Closed, 3),
        NetworkLanded::Nothing
    );
    assert_eq!(browse.next(LEFT, 4), None);
}

#[test]
fn returning_while_the_close_is_out_opens_the_session_again() {
    let mut browse = answered(read(false));
    assert_eq!(browse.next(LEFT, 1), Some(NetworkAsk::Close));
    assert_eq!(browse.next(WAITING, 2), Some(NetworkAsk::Collect));
    // The close's answer, had it landed, names nothing the windows show.
    assert_eq!(
        browse.landed(NetworkAnswer::Closed, 3),
        NetworkLanded::Nothing
    );
    assert_eq!(browse.next(RUNG, 4), None, "the collect is still out");
    assert_eq!(browse.landed(read(false), 5), NetworkLanded::Items);
}

#[test]
fn every_refusal_says_why_in_words() {
    for err in [
        DiscoveryError::Unavailable,
        DiscoveryError::Refused(Errno::PermissionDenied),
        DiscoveryError::Refused(Errno::LimitExceeded),
        DiscoveryError::Refused(Errno::OutOfRange),
        DiscoveryError::Transport(Errno::NotFound),
        DiscoveryError::Malformed,
    ] {
        assert!(!refusal(err).is_empty(), "{err:?}");
    }
}
