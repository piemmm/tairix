use alloc::format;
use alloc::vec::Vec;

use super::{SoundAsset, SOUND_FILE_EXTENSION};
use crate::SoundEvent;

#[test]
fn every_asset_is_played_by_an_event_and_named_once() {
    for asset in SoundAsset::ALL {
        assert!(
            SoundEvent::ALL
                .into_iter()
                .any(|event| SoundAsset::of(event) == asset),
            "{asset:?} is played by nothing"
        );
        let file = format!("{}{SOUND_FILE_EXTENSION}", asset.name());
        assert_eq!(SoundAsset::from_file_name(&file), Some(asset));
    }
    let mut names: Vec<&str> = SoundAsset::ALL.iter().map(|asset| asset.name()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), SoundAsset::ALL.len());
}

#[test]
fn the_bell_is_the_soft_notification() {
    assert_eq!(
        SoundAsset::of(SoundEvent::Bell),
        SoundAsset::of(SoundEvent::Notification)
    );
    assert_eq!(
        SoundAsset::of(SoundEvent::DeviceAttached).name(),
        "usb_connected"
    );
}

#[test]
fn a_file_no_event_plays_is_an_orphan() {
    for orphan in [
        "power_connected.flac",
        "startup.wav",
        "startup",
        ".DS_Store",
        "Startup.flac",
        "startup.flac.flac",
    ] {
        assert_eq!(SoundAsset::from_file_name(orphan), None, "{orphan}");
    }
}
