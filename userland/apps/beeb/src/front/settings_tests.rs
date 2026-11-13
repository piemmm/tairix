use alloc::string::String;

use tairix_appconf::{Document, Registry};
use tairix_crt::screen::Scaling;

use super::*;

#[test]
fn the_factory_machine_is_mode_7_booting_on_shift_break_with_basic_and_the_dfs() {
    let preferences = Preferences::default();
    assert_eq!(preferences.links(), 0xFF, "no link fitted");
    assert_eq!(preferences.slots[15], Image::Shipped(Shipped::Basic));
    assert_eq!(preferences.slots[14], Image::Shipped(Shipped::Dfs));
    assert_eq!(preferences.os, Image::Shipped(Shipped::Mos));
    assert_eq!(
        Preferences::load(&Document::new()).0,
        preferences,
        "an empty store"
    );
}

#[test]
fn every_setting_round_trips_through_the_store() {
    let mut preferences = Preferences {
        keyboard: Mapping::Physical,
        scaling: Scaling::Smooth,
        scan_lines: true,
        volume: 350,
        fast_tape: false,
        mode: 3,
        shift_boots: false,
        disc_timing: 1,
        os: Image::Installed(String::from("rom.os-1-00")),
        ..Preferences::default()
    };
    preferences.slots[0] = Image::Ram;
    preferences.slots[7] = Image::Installed(String::from("rom.viewsheet"));
    preferences.slots[14] = Image::Empty;
    assert_eq!(preferences.links(), 0xC0 | 0x10 | 3);
    let document = preferences.document_of(Preferences::KEYS);
    let (read, refused) = Preferences::load(&document);
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(read, preferences);
}

#[test]
fn a_value_out_of_bounds_is_refused_and_leaves_the_default() {
    let mut document = Document::new();
    for (key, value) in [
        ("mode", "8"),
        ("disc-timing", "4"),
        ("os", "basic"),
        ("slot.3", "rom.Upper"),
        ("slot.4", "viewsheet"),
        ("keyboard", "dvorak"),
        ("volume", "1001"),
    ] {
        document.set(key, value).expect("a setting");
    }
    let (read, refused) = Preferences::load(&document);
    assert_eq!(read, Preferences::default());
    assert_eq!(refused.len(), 7, "{refused:?}");
}

#[test]
fn only_a_change_to_the_roms_or_links_builds_another_machine() {
    let preferences = Preferences::default();
    let shown = Preferences {
        scaling: Scaling::Smooth,
        volume: 0,
        ..preferences.clone()
    };
    assert!(!preferences.builds_other_than(&shown));
    let booted = Preferences {
        shift_boots: false,
        ..preferences.clone()
    };
    assert!(preferences.builds_other_than(&booted));
    let mut fitted = preferences.clone();
    fitted.slots[1] = Image::Ram;
    assert!(preferences.builds_other_than(&fitted));
}
