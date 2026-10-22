//! Host tests of the screensaver catalogue: what it lists and in what order,
//! and which savers it offers.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::saver::{OptionKey, OptionLabel, OptionNote, SaverOption, SaverOptionKind};
use tairix_abi::window_ipc::{SaverOrigin, SaverStanding};
use tairix_abi::{
    manifest_header, BundleId, ProgramKind, APPINFO_FLAG_FEED_MACHINE, SYSTEM_SCREENSAVER_STORE,
};
use tairix_wallpaper::ScreensaverChoice;

use super::{load_savers, SAVER_REFUSED};
use crate::saver::fixtures::Records;
use crate::tests::MemoryAssets;

const HOME: &str = "/Users/ada";

fn switch(key: &str) -> SaverOption {
    SaverOption::new(
        OptionKey::new(key).expect("a key"),
        (
            OptionLabel::new("A switch").expect("a label"),
            OptionNote::new("").expect("a note"),
        ),
        SaverOptionKind::Switch { default: true },
    )
    .expect("an option")
}

/// The signed manifest of a saver `id`, with `feeds` and `options`.
fn saver_manifest(id: &str, feeds: u32, options: &[SaverOption]) -> Vec<u8> {
    let mut header = manifest_header(id, "Waves");
    header.kind = ProgramKind::Screensaver.to_wire();
    header.flags = feeds;
    header.option_count = u8::try_from(options.len()).expect("few");
    let mut bytes = header.to_le_bytes().to_vec();
    for option in options {
        bytes.extend_from_slice(&option.to_entry());
    }
    bytes
}

fn system(name: &str) -> String {
    alloc::format!("{SYSTEM_SCREENSAVER_STORE}/{name}.app/AppInfo")
}

fn user(name: &str) -> String {
    alloc::format!("{HOME}/Library/Screensavers/{name}.app/AppInfo")
}

fn id(text: &str) -> BundleId {
    BundleId::new(text).expect("an identifier")
}

/// The desktop's own two come first, then the system's savers, then the
/// user's own, each offered with what it declares.
/// A listing with no store refused.
fn listed(
    (catalog, refused): (super::SaverCatalog, Option<tairix_appstore::WalkError>),
) -> super::SaverCatalog {
    assert_eq!(refused, None, "every store was read");
    catalog
}

#[test]
fn the_catalogue_lists_the_desktops_own_then_the_systems_then_the_users() {
    let reader = MemoryAssets::default()
        .with(
            &system("Waves"),
            &saver_manifest(
                "os.tairix.saver.waves",
                APPINFO_FLAG_FEED_MACHINE,
                &[switch("calm")],
            ),
        )
        .with(&user("Mine"), &saver_manifest("org.ada.mine", 0, &[]));
    let catalog = listed(load_savers(&reader, Some(HOME)));
    assert_eq!(catalog.refused().count(), 0);
    assert_eq!(catalog.total(), 4);
    let choices: Vec<String> = (0..catalog.total())
        .map(|index| String::from(catalog.entry(index).expect("an entry").0.choice))
        .collect();
    assert_eq!(
        choices,
        ["blank", "dim", "os.tairix.saver.waves", "org.ada.mine"]
    );
    let (waves, options) = catalog.entry(2).expect("the system's");
    assert_eq!(waves.origin, SaverOrigin::System);
    assert_eq!(waves.standing, SaverStanding::Offered);
    assert!(waves.feeds.machine());
    assert_eq!(options.len(), 1);
    assert_eq!(
        catalog.entry(3).expect("the user's").0.origin,
        SaverOrigin::User
    );
    assert!(catalog.entry(4).is_none());
    let offered = catalog
        .offered(&ScreensaverChoice::Saver(id("os.tairix.saver.waves")))
        .expect("offered");
    assert_eq!(
        offered.preview_path(),
        "/System/Screensavers/Waves.app/Resources/Preview.png"
    );
    assert!(catalog.offered(&ScreensaverChoice::Blank).is_none());
}

/// A saver of the user's own asking for a feed is listed but not offered:
/// the feeds are the system's savers' alone. The refusal is recorded by name
/// and reason.
#[test]
fn a_users_saver_asking_for_a_feed_is_not_offered() {
    let reader = MemoryAssets::default().with(
        &user("Peek"),
        &saver_manifest("org.ada.peek", APPINFO_FLAG_FEED_MACHINE, &[]),
    );
    let catalog = listed(load_savers(&reader, Some(HOME)));
    assert_eq!(
        catalog.entry(2).expect("listed").0.standing,
        SaverStanding::AsksForAFeed
    );
    assert!(catalog
        .offered(&ScreensaverChoice::Saver(id("org.ada.peek")))
        .is_none());
    let refused: Vec<_> = catalog.refused().collect();
    assert_eq!(refused.len(), 1);
    let log = Records::default();
    refused[0].log_refused(&log);
    assert_eq!(
        log.of(SAVER_REFUSED),
        [alloc::vec![
            (String::from("saver"), String::from("org.ada.peek")),
            (
                String::from("reason"),
                String::from(tairix_wallpaper::standing_reason(
                    SaverStanding::AsksForAFeed
                ))
            ),
        ]]
    );
}

/// The system's savers claim their identifiers before the account's, however
/// deep the system files one and however near the top the account files a
/// saver claiming the same identifier.
#[test]
fn a_system_saver_filed_in_a_folder_still_claims_its_identifier_first() {
    let reader = MemoryAssets::default()
        .with(
            "/System/Screensavers/calm/Waves.app/AppInfo",
            &saver_manifest("os.tairix.saver.waves", 0, &[]),
        )
        .with(
            &user("Mine"),
            &saver_manifest("os.tairix.saver.waves", 0, &[]),
        );
    let catalog = listed(load_savers(&reader, Some(HOME)));
    let standing = |index| catalog.entry(index).expect("listed").0.standing;
    assert_eq!(standing(2), SaverStanding::Offered, "the system's");
    assert_eq!(standing(3), SaverStanding::Duplicate, "the account's");
}

/// An account's store that cannot be read leaves the system's savers offered,
/// and says why the account's are missing.
#[test]
fn an_unreadable_account_store_leaves_the_systems_savers() {
    let reader = MemoryAssets::default()
        .with(
            &system("Waves"),
            &saver_manifest("os.tairix.saver.waves", 0, &[]),
        )
        .with(&user("Mine"), &saver_manifest("org.ada.mine", 0, &[]))
        .with_unlistable("/Users/ada/Library/Screensavers");
    let (catalog, refused) = load_savers(&reader, Some(HOME));
    assert!(refused.is_some(), "why the account's savers are missing");
    assert_eq!(catalog.total(), 3, "the desktop's own two and the system's");
}

/// A change to the stores says again only what it newly refuses: a refusal
/// already said stands unrepeated, however many times the stores change.
#[test]
fn a_change_to_the_stores_says_only_what_it_newly_refuses() {
    let peek = || {
        MemoryAssets::default().with(
            &user("Peek"),
            &saver_manifest("org.ada.peek", APPINFO_FLAG_FEED_MACHINE, &[]),
        )
    };
    let before = listed(load_savers(&peek(), Some(HOME)));
    let reader = peek()
        .with(
            &user("Spy"),
            &saver_manifest("org.ada.spy", APPINFO_FLAG_FEED_MACHINE, &[]),
        )
        .with(&user("Calm"), &saver_manifest("org.ada.calm", 0, &[]));
    let after = listed(load_savers(&reader, Some(HOME)));
    let newly: Vec<_> = after
        .newly_refused(&before)
        .map(|saver| saver.choice.as_str())
        .collect();
    assert_eq!(newly, ["org.ada.spy"]);
    assert_eq!(after.newly_refused(&after).count(), 0);
}

/// A saver of the user's own claiming a system saver's identifier is listed
/// as the duplicate it is, and the system's is the one offered.
#[test]
fn a_second_claim_to_an_identifier_is_a_duplicate() {
    let reader = MemoryAssets::default()
        .with(
            &system("Waves"),
            &saver_manifest("os.tairix.saver.waves", 0, &[]),
        )
        .with(
            &user("Waves"),
            &saver_manifest("os.tairix.saver.waves", 0, &[]),
        );
    let catalog = listed(load_savers(&reader, Some(HOME)));
    assert_eq!(
        catalog.entry(3).expect("the user's").0.standing,
        SaverStanding::Duplicate
    );
    let offered = catalog
        .offered(&ScreensaverChoice::Saver(id("os.tairix.saver.waves")))
        .expect("offered");
    assert_eq!(offered.origin, SaverOrigin::System);
}

/// A bundle of another kind in a screensaver store, or one whose options do
/// not read, is listed as damaged.
#[test]
fn a_bundle_that_is_no_whole_saver_is_damaged() {
    let mut application = manifest_header("org.ada.app", "App");
    application.kind = ProgramKind::Application.to_wire();
    let mut torn = saver_manifest("org.ada.torn", 0, &[switch("calm")]);
    torn.truncate(torn.len() - 1);
    let reader = MemoryAssets::default()
        .with(&user("App"), &application.to_le_bytes())
        .with(&user("Torn"), &torn);
    let catalog = listed(load_savers(&reader, Some(HOME)));
    let standings: Vec<SaverStanding> = (2..catalog.total())
        .map(|index| catalog.entry(index).expect("listed").0.standing)
        .collect();
    assert!(
        standings
            .iter()
            .all(|standing| *standing == SaverStanding::Damaged),
        "{standings:?}"
    );
    assert_eq!(catalog.refused().count(), standings.len());
}

/// With no home only the system's store is walked.
#[test]
fn with_no_home_only_the_systems_savers_are_listed() {
    let reader = MemoryAssets::default()
        .with(
            &system("Waves"),
            &saver_manifest("os.tairix.saver.waves", 0, &[]),
        )
        .with(&user("Mine"), &saver_manifest("org.ada.mine", 0, &[]));
    let catalog = listed(load_savers(&reader, None));
    assert_eq!(
        catalog.total(),
        3,
        "the desktop's own two and the system's one"
    );
}
