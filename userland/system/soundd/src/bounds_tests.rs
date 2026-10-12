use tairix_abi::sound_cue::SoundEvent;

use super::{Admit, Principals, CUE_BURST, CUE_REFILL_NS, MAX_PRINCIPALS, MIN_RETRIGGER_NS};

const ALICE: u32 = 1000;
const BOB: u32 = 1001;

#[test]
fn a_repeat_inside_the_retrigger_interval_is_one_sound_and_costs_nothing() {
    let mut bounds = Principals::new();
    assert_eq!(bounds.admit(ALICE, SoundEvent::Bell, 0), Admit::Admitted);
    for at in [1, MIN_RETRIGGER_NS / 2, MIN_RETRIGGER_NS - 1] {
        assert_eq!(bounds.admit(ALICE, SoundEvent::Bell, at), Admit::Coalesced);
    }
    // Another event, or another principal, is another occurrence.
    assert_eq!(bounds.admit(ALICE, SoundEvent::Error, 2), Admit::Admitted);
    assert_eq!(bounds.admit(BOB, SoundEvent::Bell, 3), Admit::Admitted);
    assert_eq!(
        bounds.admit(ALICE, SoundEvent::Bell, MIN_RETRIGGER_NS),
        Admit::Admitted,
        "measured from when it last sounded, not from the last repeat"
    );
    // The coalesced repeats charged nothing: a full burst is still left.
    for event in [
        SoundEvent::Info,
        SoundEvent::Warning,
        SoundEvent::Notification,
        SoundEvent::NotificationMessage,
        SoundEvent::NotificationReminder,
    ] {
        assert_eq!(
            bounds.admit(ALICE, event, MIN_RETRIGGER_NS),
            Admit::Admitted
        );
    }
}

/// Past its burst a principal is refused, and the refusal is reported once a
/// flood; it earns cues back at the sustained rate.
#[test]
fn a_principal_past_its_rate_is_refused_until_it_earns_a_cue_back() {
    let mut bounds = Principals::new();
    let mut events = SoundEvent::ALL.into_iter().cycle();
    let mut next = || events.next().expect("cycles");
    for _ in 0..CUE_BURST {
        assert_eq!(bounds.admit(ALICE, next(), 0), Admit::Admitted);
    }
    assert_eq!(
        bounds.admit(ALICE, next(), 1),
        Admit::Limited { first: true }
    );
    assert_eq!(
        bounds.admit(ALICE, next(), 2),
        Admit::Limited { first: false }
    );
    assert_eq!(
        bounds.admit(BOB, next(), 2),
        Admit::Admitted,
        "a rate is one principal's"
    );
    assert_eq!(bounds.admit(ALICE, next(), CUE_REFILL_NS), Admit::Admitted);
    assert_eq!(
        bounds.admit(ALICE, next(), CUE_REFILL_NS + 1),
        Admit::Limited { first: true },
        "a new flood is reported anew"
    );
}

/// A full table reclaims a principal that holds nothing, and refuses a new
/// one rather than forgetting a principal still being limited.
#[test]
fn the_table_reclaims_idle_principals_and_fails_closed_when_none_is() {
    let mut bounds = Principals::new();
    let fill = |bounds: &mut Principals, at: u64| {
        for uid in 0..u32::try_from(MAX_PRINCIPALS).expect("small") {
            assert_eq!(bounds.admit(uid, SoundEvent::Info, at), Admit::Admitted);
        }
    };
    fill(&mut bounds, 0);
    assert_eq!(
        bounds.admit(9_999, SoundEvent::Info, 1),
        Admit::Limited { first: true },
        "every record is still inside its retrigger interval"
    );
    // Once every record has recovered its rate and its interval has passed,
    // the newcomer takes one over.
    let later = CUE_REFILL_NS.max(MIN_RETRIGGER_NS);
    assert_eq!(
        bounds.admit(9_999, SoundEvent::Info, later),
        Admit::Admitted
    );
    fill(&mut bounds, later * 2);
}
