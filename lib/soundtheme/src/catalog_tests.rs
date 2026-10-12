extern crate std;

use std::fs;
use std::path::Path;

use super::{asset_path, is_theme_name, DEFAULT_THEME, MAX_SOUND_BYTES};
use crate::SoundAsset;

#[test]
fn an_asset_lives_in_its_theme_under_the_store() {
    assert_eq!(
        asset_path(DEFAULT_THEME, SoundAsset::UsbConnected).as_deref(),
        Some("/System/Audio/Sounds/TAIRiX/usb_connected.flac")
    );
    for illegal in ["", ".", "..", "a/b", "Theme\u{1}"] {
        assert!(!is_theme_name(illegal), "{illegal:?}");
        assert_eq!(asset_path(illegal, SoundAsset::Login), None);
    }
    assert!(is_theme_name("High Contrast"));
}

#[test]
fn the_shipped_theme_has_every_asset_within_the_bound_with_headroom() {
    let theme = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join(DEFAULT_THEME);
    for asset in SoundAsset::ALL {
        let file = theme.join(alloc::format!("{}.flac", asset.name()));
        let bytes = fs::metadata(&file)
            .unwrap_or_else(|err| panic!("{}: {err}", file.display()))
            .len();
        let bytes = usize::try_from(bytes).expect("fits");
        assert!(
            bytes * 5 <= MAX_SOUND_BYTES * 4,
            "{} is {bytes} bytes, too close to the {MAX_SOUND_BYTES}-byte bound",
            file.display()
        );
    }
}

#[test]
fn a_listing_offers_each_legal_theme_once_in_name_order() {
    assert_eq!(
        super::themes(["TAIRiX", "..", "Glass", "", "a/b", "Glass", "Aurora"]),
        ["Aurora", "Glass", "TAIRiX"]
    );
    assert!(super::themes(core::iter::empty::<&str>()).is_empty());
}
