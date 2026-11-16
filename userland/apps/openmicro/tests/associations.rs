//! The bundle's manifest associates exactly the media a machine takes: each
//! association a medium some model takes, and each medium a model takes
//! associated, so Open With and a drop are offered what a machine can use.

use tairix_browse::media::{media_for_name, named_extension};
use tairix_browse::MediaType;
use tairix_openmicro::front::media::Purpose;
use tairix_openmicro::machine::Model;

const MODELS: [Model; 3] = [Model::ModelB, Model::Electron, Model::C64];

/// The manifest's associations, each a registered media type.
fn associations() -> Vec<MediaType> {
    let manifest = include_str!("../AppInfo.toml");
    let start = manifest
        .find("associations = [")
        .expect("the manifest associates media");
    let list = &manifest[start..];
    let list = &list[list.find('[').expect("a list")..=list.find(']').expect("its end")];
    list.split(['[', ']', ','])
        .map(|item| item.trim().trim_matches('"'))
        .filter(|item| !item.is_empty())
        .map(|mime| {
            MediaType::from_media_str(mime)
                .unwrap_or_else(|| panic!("{mime} is a registered media type"))
        })
        .collect()
}

/// Whether some model, both its drives fitted, takes a file of `media`.
fn taken(media: MediaType) -> bool {
    named_extension(media).is_some_and(|extension| {
        MODELS
            .iter()
            .any(|&model| Purpose::of_extension(extension, model, [true; 2]).is_some())
    })
}

#[test]
fn every_association_is_a_medium_a_machine_takes() {
    let associated = associations();
    assert!(!associated.is_empty());
    for media in associated {
        assert!(taken(media), "{media:?}");
    }
}

#[test]
fn every_medium_a_machine_takes_is_associated() {
    let associated = associations();
    for name in [
        "a.ssd", "a.dsd", "a.uef", "a.csw", "a.d64", "a.g64", "a.tap", "a.crt", "a.prg", "a.t64",
    ] {
        let media = media_for_name(name).unwrap_or_else(|| panic!("{name} is a registered medium"));
        assert!(taken(media), "{name} is taken");
        assert!(associated.contains(&media), "{name} is associated");
    }
}
