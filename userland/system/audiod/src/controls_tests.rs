//! Host tests for the tenancy model: each tenant's controls stand apart, the
//! baseline lies beneath them, and a departed session's are forgotten.

use tairix_abi::audio::{AudioBaseline, AudioGain, AudioLocation};
use tairix_abi::driver::audio::StreamDirection;
use tairix_abi::ProcId;
use tairix_audio::route::Room;

use super::{Controls, Tenant};

fn place(index: u16) -> AudioLocation {
    AudioLocation::new(0x5eed, index).expect("a place")
}

fn level(millibel: i32) -> AudioGain {
    AudioGain::new(millibel).expect("attenuation")
}

const ALICE: Tenant = Tenant::Session(ProcId::from_raw([0xA1; 16]));
const BOB: Tenant = Tenant::Session(ProcId::from_raw([0xB0; 16]));

#[test]
fn a_withheld_room_has_no_tenant_and_the_others_have_theirs() {
    assert_eq!(Tenant::of(Room::Withheld), None);
    assert_eq!(Tenant::of(Room::Unclaimed), Some(Tenant::Unclaimed));
    let session = ProcId::from_raw([7; 16]);
    assert_eq!(
        Tenant::of(Room::Session(session)),
        Some(Tenant::Session(session))
    );
}

#[test]
fn each_tenant_hears_its_own_level_over_the_baseline() {
    let mut controls = Controls::new();
    assert!(controls.set_baseline(AudioBaseline {
        level: level(-600),
        ..AudioBaseline::DEFAULT
    }));
    assert_eq!(controls.set_level(ALICE, place(0), level(-1_200)), Ok(true));
    assert_eq!(controls.level(Some(ALICE), place(0)), level(-1_200));
    assert_eq!(controls.level(Some(BOB), place(0)), level(-600));
    assert_eq!(controls.level(None, place(0)), level(-600));
    assert_eq!(controls.level(Some(ALICE), place(1)), level(-600));
    // Setting what is already in force moves nothing.
    assert_eq!(
        controls.set_level(ALICE, place(0), level(-1_200)),
        Ok(false)
    );
    assert_eq!(controls.set_level(BOB, place(0), level(-600)), Ok(false));
}

#[test]
fn a_mute_is_its_tenants_and_unmuting_returns_to_the_level() {
    let mut controls = Controls::new();
    assert_eq!(controls.set_level(ALICE, place(0), level(-300)), Ok(true));
    assert_eq!(controls.set_muted(ALICE, place(0), true), Ok(true));
    assert!(controls.muted(Some(ALICE), place(0)));
    assert!(!controls.muted(Some(BOB), place(0)));
    assert_eq!(controls.set_muted(ALICE, place(0), true), Ok(false));
    assert_eq!(controls.set_muted(ALICE, place(0), false), Ok(true));
    assert!(!controls.muted(Some(ALICE), place(0)));
    assert_eq!(controls.level(Some(ALICE), place(0)), level(-300));
}

#[test]
fn a_tenants_preference_comes_before_the_machines() {
    let mut controls = Controls::new();
    controls.set_baseline(AudioBaseline {
        output: Some(place(4)),
        ..AudioBaseline::DEFAULT
    });
    assert_eq!(
        controls.preferences(Some(ALICE), StreamDirection::Playback),
        [None, Some(place(4))]
    );
    assert_eq!(
        controls.prefer(ALICE, StreamDirection::Playback, place(2)),
        Ok(true)
    );
    assert_eq!(
        controls.preferences(Some(ALICE), StreamDirection::Playback),
        [Some(place(2)), Some(place(4))]
    );
    assert_eq!(
        controls.preferences(Some(ALICE), StreamDirection::Capture),
        [None, None]
    );
    assert_eq!(
        controls.preferences(None, StreamDirection::Playback),
        [None, Some(place(4))]
    );
    assert_eq!(
        controls.prefer(ALICE, StreamDirection::Playback, place(2)),
        Ok(false)
    );
}

#[test]
fn a_departed_sessions_controls_are_forgotten_and_the_unclaimed_rooms_are_not() {
    let mut controls = Controls::new();
    for tenant in [ALICE, BOB, Tenant::Unclaimed] {
        assert_eq!(controls.set_level(tenant, place(0), level(-900)), Ok(true));
    }
    let Tenant::Session(alice) = ALICE else {
        unreachable!("a session")
    };
    controls.retain(|session| session == alice);
    assert_eq!(controls.level(Some(ALICE), place(0)), level(-900));
    assert_eq!(controls.level(Some(BOB), place(0)), AudioGain::UNITY);
    assert_eq!(
        controls.level(Some(Tenant::Unclaimed), place(0)),
        level(-900)
    );
}
