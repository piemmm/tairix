//! A new install draws everything at its finest; the store keeps what is in
//! force and nothing else; a broken line costs only itself; and an answered
//! write never moves a control the player is still using.

use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_appconf::Document;
use tairix_appdata::fake::FakeService;
use tairix_appdata::{Publication, PublishJob, Published, Refusal, Settings};

use super::*;

/// The command word the bundle is installed under.
const OWN_WORD: &str = "wintersun";

/// The four knobs' keys, which a non-custom choice leaves unset.
const KNOB_KEYS: [&str; 4] = [LIGHTING_KEY, SHADOWS_KEY, GROUND_KEY, RESOLUTION_KEY];

/// Every knob moved off its finest.
const DETAIL: Detail = Detail {
    lighting: Lighting::Medium,
    shadows: Shadows::Hard,
    ground: MaterialQuality::new(2),
    resolution: Resolution::TwoThirds,
};

/// A custom choice of [`DETAIL`].
const CUSTOM: Graphics = Graphics::Custom(DETAIL);

fn document(text: &str) -> Document {
    Document::parse(text).expect("a well-formed document")
}

fn publish(host: &mut FakeService, graphics: Graphics) -> Result<Published<Graphics>, Errno> {
    tairix_appdata::publish(
        &mut Settings::open_without_defaults(host),
        &PublishJob::Save(graphics),
    )
}

fn load(host: &mut FakeService) -> (Graphics, Vec<Refusal>) {
    tairix_appdata::loaded(&Settings::open_without_defaults(host))
}

#[test]
fn a_new_install_draws_every_detail_at_its_finest() {
    assert_eq!(Graphics::DEFAULT, Graphics::Ultra);
    assert_eq!(Graphics::DEFAULT.fixed(), Some(Detail::FINEST));
    assert_eq!(Graphics::load(&document("")), (Graphics::Ultra, Vec::new()));
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
        assert_eq!(stored.record, graphics);
        assert!(stored.refused.is_empty());
        assert_eq!(load(&mut host), (graphics, Vec::new()));
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
    let (graphics, refused) = Graphics::load(&document(
        "graphics.mode = custom\n\
         graphics.lighting = dazzling\n\
         graphics.shadows = flat\n\
         graphics.ground = 99\n\
         graphics.resolution = half\n",
    ));
    assert_eq!(
        graphics,
        Graphics::Custom(Detail {
            lighting: Lighting::Fine,
            shadows: Shadows::Flat,
            ground: Detail::FINEST.ground,
            resolution: Resolution::Half,
        })
    );
    assert_eq!(refused, [GraphicsKey::Lighting, GraphicsKey::Ground]);

    let (unknown, refused) = Graphics::load(&document("graphics.mode = cinematic\n"));
    assert_eq!(unknown, Graphics::DEFAULT);
    assert_eq!(refused, [GraphicsKey::Mode]);
}

#[test]
fn a_knob_left_behind_by_another_choice_means_nothing() {
    let (graphics, refused) = Graphics::load(&document(
        "graphics.mode = basic\ngraphics.lighting = dazzling\n",
    ));
    assert_eq!(graphics, Graphics::Basic);
    assert!(refused.is_empty(), "a knob a preset ignores is not refused");
}

#[test]
fn a_store_that_cannot_be_reached_reads_as_the_default_and_says_why() {
    let mut host = FakeService::for_word(OWN_WORD);
    host.read_refusal().set(Some(Errno::NotFound));
    let (graphics, refusals) = load(&mut host);
    assert_eq!(graphics, Graphics::DEFAULT);
    assert_eq!(refusals, [Refusal::StoreUnreadable(Errno::NotFound)]);
}

#[test]
fn a_refused_write_is_an_error_not_a_silent_default() {
    let mut host = FakeService::for_word(OWN_WORD);
    host.refusal().set(Some(Errno::PermissionDenied));
    assert_eq!(publish(&mut host, CUSTOM), Err(Errno::PermissionDenied));
}

/// One edit by the settings window: a whole choice, as its controls make one.
fn choose(publication: &mut Publication<Graphics>, graphics: Graphics) {
    let was = *publication.live();
    publication.edit(&was, &graphics);
}

fn answered(
    publication: &mut Publication<Graphics>,
    record: Graphics,
) -> Option<PublishJob<Graphics>> {
    let answer = Published {
        record,
        refused: Vec::new(),
    };
    publication.adopt(Ok(answer), &mut Vec::new())
}

#[test]
fn an_answer_is_adopted_only_where_the_player_has_not_moved_on() {
    let mut publication = Publication::new(Graphics::Ultra);
    choose(&mut publication, Graphics::Basic);
    assert_eq!(
        publication.settle(),
        Some(PublishJob::Save(Graphics::Basic))
    );

    // The player moved on before the write of Basic answered.
    choose(&mut publication, CUSTOM);
    assert_eq!(answered(&mut publication, Graphics::Basic), None);
    assert_eq!(
        *publication.live(),
        CUSTOM,
        "a control in use did not jump back"
    );
    assert_eq!(
        *publication.adopted(),
        Graphics::Basic,
        "the store's word is recorded"
    );

    // Their own write answers, and the store holds something else — a machine
    // policy wins where the player is not editing.
    assert_eq!(publication.settle(), Some(PublishJob::Save(CUSTOM)));
    assert_eq!(answered(&mut publication, Graphics::Auto), None);
    assert_eq!(*publication.live(), Graphics::Auto);
}

#[test]
fn a_knob_moved_while_its_write_is_out_stays_where_the_player_put_it() {
    let mut publication = Publication::new(CUSTOM);
    let soft = Graphics::Custom(Detail {
        shadows: Shadows::Soft,
        ..DETAIL
    });
    choose(&mut publication, soft);
    let _ = publication.settle();
    let lit = Graphics::Custom(Detail {
        shadows: Shadows::Soft,
        lighting: Lighting::Coarse,
        ..DETAIL
    });
    choose(&mut publication, lit);
    let _ = answered(&mut publication, soft);
    assert_eq!(*publication.live(), lit);
    assert_eq!(publication.settle(), Some(PublishJob::Save(lit)));
}

#[test]
fn a_refused_write_puts_the_stored_choice_back() {
    let mut publication = Publication::new(Graphics::Ultra);
    choose(&mut publication, Graphics::Auto);
    let _ = publication.settle();
    let mut refusals = Vec::new();
    let _ = publication.adopt(Err(Errno::PermissionDenied), &mut refusals);
    assert_eq!(*publication.live(), Graphics::Ultra);
    assert_eq!(refusals, [Refusal::NotSaved(Errno::PermissionDenied)]);
}

#[test]
fn every_key_is_one_the_store_accepts() {
    for key in [MODE_KEY].into_iter().chain(KNOB_KEYS) {
        assert!(tairix_appconf::validate_key(key).is_ok(), "{key}");
    }
}
