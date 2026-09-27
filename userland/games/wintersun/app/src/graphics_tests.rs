//! A new install draws everything at its finest; the store keeps what is in
//! force and nothing else; a broken line costs only itself; and an answered
//! write never moves a control the player is still using.

use tairix_appconf::Document;
use tairix_appdata::fake::FakeService;

use super::*;

/// The command word the bundle is installed under.
const OWN_WORD: &str = "wintersun";

/// A custom choice with every knob moved off its finest.
const CUSTOM: Graphics = Graphics::Custom(Detail {
    lighting: Lighting::Medium,
    shadows: Shadows::Hard,
    ground: MaterialQuality::new(2),
    resolution: Resolution::TwoThirds,
});

fn document(text: &str) -> Document {
    Document::parse(text).expect("a well-formed document")
}

#[test]
fn a_new_install_draws_every_detail_at_its_finest() {
    assert_eq!(Graphics::DEFAULT, Graphics::Ultra);
    assert_eq!(Graphics::DEFAULT.fixed(), Some(Detail::FINEST));
    let stored = read(&document(""));
    assert_eq!(stored.graphics, Graphics::Ultra);
    assert!(stored.refused.is_empty());
}

#[test]
fn only_auto_leaves_the_detail_to_the_governor() {
    for mode in Mode::ALL {
        let graphics = Graphics::chosen(mode, Detail::PLAINEST);
        assert_eq!(graphics.mode(), mode);
        assert_eq!(graphics.fixed().is_none(), mode == Mode::Auto, "{mode:?}");
    }
    assert_eq!(
        Graphics::chosen(Mode::Custom, BASIC).fixed(),
        Some(BASIC),
        "custom starts from what is on screen"
    );
    assert_eq!(
        BASIC.resolution,
        Resolution::Full,
        "basic is not drawn smaller"
    );
}

#[test]
fn every_choice_survives_the_store() {
    for graphics in [Graphics::Auto, Graphics::Ultra, Graphics::Basic, CUSTOM] {
        let mut host = FakeService::for_word(OWN_WORD);
        let stored = publish(&mut host, graphics).expect("the fake accepts the write");
        assert_eq!(stored.graphics, graphics);
        assert!(stored.refused.is_empty());
        let (reread, refusal) = load(&mut host);
        assert_eq!(reread.graphics, graphics);
        assert_eq!(refusal, None);
    }
}

#[test]
fn the_store_keeps_the_knobs_only_while_the_choice_is_custom() {
    let mut host = FakeService::for_word(OWN_WORD);
    publish(&mut host, CUSTOM).expect("written");
    assert_eq!(host.committed().get(MODE_KEY), Some("custom"));
    assert_eq!(host.committed().get(LIGHTING_KEY), Some("medium"));
    assert_eq!(host.committed().get(SHADOWS_KEY), Some("hard"));
    assert_eq!(host.committed().get(GROUND_KEY), Some("2"));
    assert_eq!(host.committed().get(RESOLUTION_KEY), Some("two-thirds"));

    publish(&mut host, Graphics::Auto).expect("written");
    assert_eq!(host.committed().get(MODE_KEY), Some("auto"));
    for key in KNOB_KEYS {
        assert_eq!(host.committed().get(key), None, "{key} outlived its choice");
    }
}

#[test]
fn a_broken_line_costs_only_itself() {
    let stored = read(&document(
        "graphics.mode = custom\n\
         graphics.lighting = dazzling\n\
         graphics.shadows = flat\n\
         graphics.ground = 99\n\
         graphics.resolution = half\n",
    ));
    assert_eq!(
        stored.graphics,
        Graphics::Custom(Detail {
            lighting: Lighting::Fine,
            shadows: Shadows::Flat,
            ground: Detail::FINEST.ground,
            resolution: Resolution::Half,
        })
    );
    assert_eq!(stored.refused, alloc::vec![LIGHTING_KEY, GROUND_KEY]);

    let unknown = read(&document("graphics.mode = cinematic\n"));
    assert_eq!(unknown.graphics, Graphics::DEFAULT);
    assert_eq!(unknown.refused, alloc::vec![MODE_KEY]);
}

#[test]
fn a_store_that_cannot_be_reached_reads_as_the_default_and_says_why() {
    let mut host = FakeService::for_word(OWN_WORD);
    host.read_refusal().set(Some(Errno::NotFound));
    let (stored, refusal) = load(&mut host);
    assert_eq!(stored.graphics, Graphics::DEFAULT);
    assert_eq!(refusal, Some(Errno::NotFound));
}

#[test]
fn a_refused_write_is_an_error_not_a_silent_default() {
    let mut host = FakeService::for_word(OWN_WORD);
    host.refusal().set(Some(Errno::PermissionDenied));
    assert_eq!(publish(&mut host, CUSTOM), Err(Errno::PermissionDenied));
}

#[test]
fn an_answer_is_adopted_only_where_the_player_has_not_moved_on() {
    let mut choice = Choice::new(Graphics::Ultra);
    assert!(choice.preview(Graphics::Basic));
    assert!(
        !choice.preview(Graphics::Basic),
        "the same choice changes nothing"
    );

    // The player moved on before the write of Basic answered.
    assert!(choice.preview(CUSTOM));
    assert_eq!(
        choice.answered(Graphics::Basic, Ok(Graphics::Basic)),
        Adopted::Standing
    );
    assert_eq!(choice.live(), CUSTOM, "a control in use jumped back");
    assert_eq!(
        choice.adopted,
        Graphics::Basic,
        "the store's word is still recorded"
    );

    // Their own write answers, and the store holds something else — a
    // machine policy wins where the player is not editing.
    assert_eq!(choice.answered(CUSTOM, Ok(Graphics::Auto)), Adopted::Moved);
    assert_eq!(choice.live(), Graphics::Auto);
}

#[test]
fn a_refused_write_puts_the_stored_choice_back() {
    let mut choice = Choice::new(Graphics::Ultra);
    choice.preview(Graphics::Auto);
    assert_eq!(
        choice.answered(Graphics::Auto, Err(Errno::PermissionDenied)),
        Adopted::Moved
    );
    assert_eq!(choice.live(), Graphics::Ultra);
    assert_eq!(choice.adopted, Graphics::Ultra);
}

#[test]
fn every_key_is_one_the_store_accepts() {
    for key in [MODE_KEY].into_iter().chain(KNOB_KEYS) {
        assert!(tairix_appconf::validate_key(key).is_ok(), "{key}");
    }
}
