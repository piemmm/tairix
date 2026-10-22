//! Host tests of the screensavers as Settings lists them: what an entry the
//! desktop answered becomes, the rows a saver's own group draws, what each
//! writes, and what the pane says of a saver it cannot offer.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::saver::{
    Choice, ChoiceValue, Choices, OptionKey, Range, SaverFeeds, SaverOption, SaverOptionKind,
    SaverValue,
};
use tairix_abi::window_ipc::{
    decode_screensaver_reply, encode_screensaver_reply, SaverOrigin, SaverStanding,
    ScreensaverEntry, WINDOW_SCREENSAVER_REPLY_MAX,
};
use tairix_abi::{BundleId, APPINFO_FLAG_FEED_MACHINE, APPINFO_FLAG_FEED_SLIDES};
use tairix_controls::FieldControl;
use tairix_wallpaper::{
    CatalogItem, DesktopSettings, ScreensaverChoice, SlideSource, TaskNames, WallpaperCategory,
};

use super::{missing_statement, unavailable_statement, SaverEntry, SaverRow};

const WAVES: &str = "os.tairix.saver.waves";

fn speed() -> SaverOption {
    let choice = |value: &str, label: &str| Choice {
        value: ChoiceValue::new(value).expect("a word"),
        label: BoundedText::new(label).expect("a label"),
    };
    SaverOption::new(
        OptionKey::new("speed").expect("a key"),
        (
            BoundedText::new("Speed").expect("a label"),
            BoundedText::EMPTY,
        ),
        SaverOptionKind::Choice {
            choices: Choices::new(&[choice("slow", "Slow"), choice("fast", "Fast")])
                .expect("choices"),
            default: 0,
        },
    )
    .expect("an option")
}

fn foam() -> SaverOption {
    SaverOption::new(
        OptionKey::new("foam").expect("a key"),
        (
            BoundedText::new("Foam").expect("a label"),
            BoundedText::new("Whether the waves break white.").expect("a note"),
        ),
        SaverOptionKind::Switch { default: true },
    )
    .expect("an option")
}

fn height() -> SaverOption {
    SaverOption::new(
        OptionKey::new("height").expect("a key"),
        (
            BoundedText::new("Wave height").expect("a label"),
            BoundedText::EMPTY,
        ),
        SaverOptionKind::Range {
            range: Range::new(1, 9, 2).expect("a range"),
            default: 5,
            unit: BoundedText::new("metres").expect("a unit"),
        },
    )
    .expect("an option")
}

fn waves(feeds: u32) -> SaverEntry {
    SaverEntry {
        index: 2,
        choice: ScreensaverChoice::from_value(WAVES).expect("a choice"),
        title: String::from("Waves"),
        purpose: String::from("The sea at night"),
        origin: SaverOrigin::System,
        standing: SaverStanding::Offered,
        feeds: SaverFeeds::from_bits(feeds).expect("feeds"),
        options: alloc::vec![speed(), foam(), height()],
    }
}

fn id() -> BundleId {
    BundleId::new(WAVES).expect("an identifier")
}

fn catalog() -> Vec<CatalogItem> {
    let item = |category: &str, file: &str| CatalogItem {
        category: String::from(category),
        file: String::from(file),
    };
    alloc::vec![
        item("Space", "orbit.jpg"),
        item("Nature", "fern.jpg"),
        item("Space", "nebula.jpg"),
    ]
}

/// The choice labels and the one held of a row's list.
fn listed(control: &FieldControl) -> (Vec<String>, usize) {
    let FieldControl::Combo(combo) = control else {
        panic!("a list");
    };
    (
        combo.choices().to_vec(),
        combo.selected().expect("one held"),
    )
}

/// An entry the desktop answered becomes the saver Settings lists, its
/// options with it; one whose choice no document could hold is dropped.
#[test]
fn a_catalogue_entry_becomes_the_saver_listed() {
    let options = [speed(), foam()];
    let entry = ScreensaverEntry::new(
        (WAVES, "Waves", "The sea at night"),
        SaverOrigin::User,
        SaverStanding::Offered,
        SaverFeeds::NONE,
    );
    let mut frame = [0u8; WINDOW_SCREENSAVER_REPLY_MAX];
    let len = encode_screensaver_reply(&mut frame, Ok((3, &entry, &options)));
    let (total, decoded) = decode_screensaver_reply(&frame[..len]).expect("an entry");
    assert_eq!(total, 3);
    let saver = SaverEntry::of(2, &decoded).expect("listed");
    assert_eq!(saver.index, 2);
    assert_eq!(saver.choice.as_str(), WAVES);
    assert_eq!(
        (saver.title.as_str(), saver.origin),
        ("Waves", SaverOrigin::User)
    );
    assert_eq!(saver.options, options);
    let unnamed = ScreensaverEntry::new(
        ("not a choice", "?", ""),
        SaverOrigin::User,
        SaverStanding::Offered,
        SaverFeeds::NONE,
    );
    assert_eq!(SaverEntry::of(3, &unnamed), None);
}

/// A listing holds each choice once, the first entry naming it, and nothing
/// no document could hold.
#[test]
fn a_listing_keeps_a_second_claim_so_the_pane_can_say_why() {
    let entry = |choice: &'static str, title: &'static str, standing| {
        ScreensaverEntry::new(
            (choice, title, ""),
            SaverOrigin::System,
            standing,
            SaverFeeds::NONE,
        )
    };
    let mut savers = Vec::new();
    SaverEntry::list(
        &mut savers,
        0,
        &entry(WAVES, "Waves", SaverStanding::Offered),
    );
    SaverEntry::list(
        &mut savers,
        1,
        &entry(WAVES, "Waves again", SaverStanding::Duplicate),
    );
    SaverEntry::list(
        &mut savers,
        2,
        &entry("not a choice", "?", SaverStanding::Offered),
    );
    assert_eq!(savers.len(), 2, "the unnameable one is left out");
    assert!(savers[0].offered() && !savers[1].offered());
    let said = unavailable_statement(&savers).expect("a saver cannot be chosen");
    assert!(said.contains("Waves again"), "{said}");
}

/// A saver draws a row for each option it declares, then one for each feed
/// whose setting the desktop holds.
#[test]
fn a_saver_draws_its_options_then_its_feeds() {
    assert_eq!(
        SaverRow::of(&waves(0)),
        [
            SaverRow::Option(0),
            SaverRow::Option(1),
            SaverRow::Option(2)
        ]
    );
    assert_eq!(
        SaverRow::of(&waves(APPINFO_FLAG_FEED_SLIDES | APPINFO_FLAG_FEED_MACHINE))[3..],
        [SaverRow::Slides, SaverRow::Tasks]
    );
}

/// A choice and a switch are lists holding the value the desktop keeps or
/// the default, and a range is a slider whose row says the value it holds.
#[test]
fn each_option_draws_the_control_of_its_kind() {
    let saver = waves(0);
    let mut settings = DesktopSettings::default();
    let row = |at: usize, settings: &DesktopSettings| {
        SaverRow::Option(at)
            .row(&saver, settings, &[])
            .expect("a row")
    };
    let choice = row(0, &settings);
    assert_eq!(choice.label(), "Speed");
    assert_eq!(
        listed(choice.control()),
        (alloc::vec![String::from("Slow"), String::from("Fast")], 0)
    );
    let switch = row(1, &settings);
    assert_eq!(listed(switch.control()).1, 0, "On, its default");
    assert_eq!(switch.description(), Some("Whether the waves break white."));
    let range = row(2, &settings);
    assert!(matches!(range.control(), FieldControl::Slider(_)));
    assert_eq!(range.description(), Some("Now 5 metres."));

    assert!(settings.saver_values.set(
        &id(),
        &speed(),
        SaverValue::Choice(ChoiceValue::new("fast").expect("a word"))
    ));
    assert_eq!(listed(row(0, &settings).control()).1, 1, "the value kept");
    assert!(SaverRow::Option(3).row(&saver, &settings, &[]).is_none());
}

/// A document with no room for another value makes it by forgetting the
/// values of savers no longer listed, and keeps the listed savers' own.
#[test]
fn a_full_document_forgets_the_values_of_savers_no_longer_listed() {
    let saver = waves(0);
    let mut settings = DesktopSettings::default();
    let off = SaverValue::Switch(false);
    assert!(settings.saver_values.set(&id(), &foam(), off));
    for gone in 1..tairix_wallpaper::SAVER_VALUES_MAX {
        let gone = BundleId::new(&alloc::format!("org.gone.s{gone}")).expect("an identifier");
        assert!(settings.saver_values.set(&gone, &foam(), off));
    }
    assert!(SaverRow::Option(0).adopt(
        (&saver, core::slice::from_ref(&saver)),
        1,
        &mut settings,
        &[]
    ));
    assert_eq!(
        settings.saver_values.value(&id(), &speed()),
        SaverValue::Choice(ChoiceValue::new("fast").expect("a word"))
    );
    assert_eq!(
        settings.saver_values.value(&id(), &foam()),
        SaverValue::Switch(false),
        "the listed saver's own value is kept"
    );
}

/// A choice writes the value it names under the saver's own key, and an
/// index the row never offered writes nothing.
#[test]
fn a_choice_writes_the_savers_own_value_and_fails_closed() {
    let saver = waves(0);
    let mut settings = DesktopSettings::default();
    assert!(SaverRow::Option(0).adopt(
        (&saver, core::slice::from_ref(&saver)),
        1,
        &mut settings,
        &[]
    ));
    assert_eq!(
        settings.saver_values.value(&id(), &speed()),
        SaverValue::Choice(ChoiceValue::new("fast").expect("a word"))
    );
    assert!(SaverRow::Option(1).adopt(
        (&saver, core::slice::from_ref(&saver)),
        1,
        &mut settings,
        &[]
    ));
    assert_eq!(
        settings.saver_values.value(&id(), &foam()),
        SaverValue::Switch(false)
    );
    assert!(SaverRow::Option(2).adopt(
        (&saver, core::slice::from_ref(&saver)),
        4,
        &mut settings,
        &[]
    ));
    assert_eq!(
        settings.saver_values.value(&id(), &height()),
        SaverValue::Range(9),
        "the slider's last stop is the range's greatest value"
    );
    let before = settings.clone();
    assert!(!SaverRow::Option(0).adopt(
        (&saver, core::slice::from_ref(&saver)),
        2,
        &mut settings,
        &[]
    ));
    assert!(!SaverRow::Option(2).adopt(
        (&saver, core::slice::from_ref(&saver)),
        5,
        &mut settings,
        &[]
    ));
    assert!(!SaverRow::Option(7).adopt(
        (&saver, core::slice::from_ref(&saver)),
        0,
        &mut settings,
        &[]
    ));
    assert_eq!(settings, before, "nothing written");
}

/// The slides feed offers every picture or one category's, the category held
/// listed even where the catalog no longer has it; the machine feed's task
/// names are the desktop's to choose.
#[test]
fn the_feed_rows_set_what_the_desktop_feeds() {
    let saver = waves(APPINFO_FLAG_FEED_SLIDES | APPINFO_FLAG_FEED_MACHINE);
    let mut settings = DesktopSettings::default();
    let slides = SaverRow::Slides
        .row(&saver, &settings, &catalog())
        .expect("a row");
    assert_eq!(
        listed(slides.control()),
        (
            alloc::vec![
                String::from("Every picture"),
                String::from("Space"),
                String::from("Nature")
            ],
            0
        )
    );
    assert!(SaverRow::Slides.adopt(
        (&saver, core::slice::from_ref(&saver)),
        2,
        &mut settings,
        &catalog()
    ));
    assert_eq!(
        settings.slides,
        SlideSource::Category(WallpaperCategory::new("Nature").expect("a category"))
    );
    settings.slides = SlideSource::Category(WallpaperCategory::new("Animals").expect("a category"));
    let held = SaverRow::Slides
        .row(&saver, &settings, &catalog())
        .expect("a row");
    assert_eq!(
        listed(held.control()).1,
        3,
        "the category held, listed last"
    );
    assert!(!SaverRow::Slides.adopt(
        (&saver, core::slice::from_ref(&saver)),
        4,
        &mut settings,
        &catalog()
    ));

    assert!(SaverRow::Tasks.adopt(
        (&saver, core::slice::from_ref(&saver)),
        2,
        &mut settings,
        &catalog()
    ));
    assert_eq!(settings.task_names, TaskNames::All);
    assert!(!SaverRow::Tasks.adopt(
        (&saver, core::slice::from_ref(&saver)),
        3,
        &mut settings,
        &catalog()
    ));
}

/// The pane says why each saver it lists cannot be chosen, and that the one
/// chosen is gone, once the catalogue has been answered and not before.
#[test]
fn what_cannot_be_chosen_is_said() {
    let mut mine = waves(APPINFO_FLAG_FEED_MACHINE);
    mine.origin = SaverOrigin::User;
    mine.standing = SaverStanding::AsksForAFeed;
    let said = unavailable_statement(&[waves(0), mine]).expect("a statement");
    assert!(
        said.starts_with("Waves (yours) cannot be chosen:"),
        "{said}"
    );
    assert_eq!(unavailable_statement(&[waves(0)]), None);

    let gone = ScreensaverChoice::from_value("os.tairix.saver.gone").expect("a choice");
    assert_eq!(missing_statement(&gone, &[]), None, "not yet answered");
    let missing = missing_statement(&gone, &[waves(0)]).expect("a statement");
    assert!(missing.contains("os.tairix.saver.gone"), "{missing}");
    assert_eq!(missing_statement(&waves(0).choice, &[waves(0)]), None);
}
