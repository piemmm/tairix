use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use crate::machine::bbc::fdc::PhysicalDrive;
use crate::media::c1541::layout::{bam, DIRECTORY_TRACK};
use crate::media::disc::Disc;
use crate::media::tape::Tape;

const MODEL_B: (Model, [bool; 2]) = (Model::ModelB, [true; 2]);

#[test]
fn a_machine_s_folder_is_its_name_made_plain_and_unique() {
    assert_eq!(folder_for("Model B", |_| false).as_deref(), Some("Model B"));
    assert_eq!(
        folder_for("Model B", |name| name == "Model B").as_deref(),
        Some("Model B 2")
    );
    assert_eq!(
        folder_for(".hidden/odd\u{7}", |_| false).as_deref(),
        Some("hiddenodd")
    );
    assert_eq!(folder_for("...", |_| false).as_deref(), Some("Machine"));
    assert_eq!(folder_for("x", |_| true), None, "crowded out");
    let long = "y".repeat(200);
    let whole = "y".repeat(MAX_NAME);
    let named = folder_for(&long, |name| name == whole).expect("a name");
    assert!(named.len() <= MAX_NAME && named.ends_with(" 2"), "{named}");
}

#[test]
fn a_new_disc_or_tape_takes_the_first_free_name() {
    let taken = ["Disc.ssd", "Disc 2.ssd"];
    assert_eq!(
        fresh("Disc", ".ssd", |name| taken.contains(&name)).as_deref(),
        Some("Disc 3.ssd")
    );
    assert_eq!(
        fresh("Tape", ".uef", |_| false).as_deref(),
        Some("Tape.uef")
    );
}

/// A blank disc is an empty DFS catalogue of eighty tracks, and a blank tape
/// a UEF of nothing; each reads as an image of its kind.
#[test]
fn blank_media_read_back_as_empty_images() {
    let disc = blank_disc();
    assert_eq!(disc.len(), 80 * TRACK_BYTES);
    assert_eq!(disc[256 + 5], 0, "no files");
    assert_eq!(
        u16::from(disc[256 + 6] & 3) << 8 | u16::from(disc[256 + 7]),
        800
    );
    let decoded = Disc::decode(&disc, ImageFormat::Ssd, false).expect("a disc");
    assert_eq!(decoded.tracks(), 80);
    let image = blank_image(Purpose::Tape, Model::ModelB, "Tape.uef");
    assert!(Tape::decode(&image).expect("a tape").is_empty());
}

/// A C64's blank disk is formatted with its file's name and an ID of its
/// own, and its blank tape is a TAP of nothing.
#[test]
fn a_commodore_s_blank_media_are_its_own() {
    let disc = Purpose::Disc(PhysicalDrive::Zero);
    assert_eq!(blank_name(disc, Model::C64), Some(("Disk", "d64")));
    assert_eq!(blank_name(Purpose::Tape, Model::C64), Some(("Tape", "tap")));
    let first = blank_image(disc, Model::C64, "Disk.d64");
    let second = blank_image(disc, Model::C64, "Disk 2.d64");
    let bam = (1..DIRECTORY_TRACK)
        .map(|track| usize::from(d64::sectors(track)))
        .sum::<usize>()
        * d64::SECTOR_BYTES;
    let bam_at = |image: &[u8], field: u8| image[bam + usize::from(field)..][..2].to_vec();
    assert_eq!(bam_at(&first, bam::NAME), b"DI");
    assert_ne!(
        bam_at(&first, bam::ID),
        bam_at(&second, bam::ID),
        "IDs of their own"
    );
    assert!(d64::decode(&first).is_ok());
    let tape = blank_image(Purpose::Tape, Model::C64, "Tape.tap");
    assert!(tap::decode(&tape).expect("a TAP").pulses().is_empty());
}

#[test]
fn the_folder_offers_its_discs_and_tapes_in_name_order() {
    let names: Vec<String> = ["b.SSD", "notes.txt", "A.dsd", "Game.uef", "side.csw", "x"]
        .into_iter()
        .map(String::from)
        .collect();
    let offered = offered(&names, MODEL_B.0, MODEL_B.1);
    assert!(offered.reachable);
    assert_eq!(offered.discs, ["A.dsd", "b.SSD"]);
    assert_eq!(offered.tapes, ["Game.uef", "side.csw"]);
    let c64 = super::offered(&names, Model::C64, [true, false]);
    assert!(c64.discs.is_empty() && c64.tapes.is_empty(), "none its own");
    let many: Vec<String> = (0..FOLDER_ROWS + 5)
        .map(|n| alloc::format!("{n:03}.ssd"))
        .collect();
    assert_eq!(
        super::offered(&many, MODEL_B.0, MODEL_B.1).discs.len(),
        FOLDER_ROWS
    );
    let cased =
        |names: [&str; 2]| super::offered(&names.map(String::from), MODEL_B.0, MODEL_B.1).discs;
    assert_eq!(
        cased(["a.ssd", "A.ssd"]),
        cased(["A.ssd", "a.ssd"]),
        "names alike but for case keep one order whatever the listing's"
    );
}

#[test]
fn a_digest_changes_with_the_file() {
    assert_eq!(digest(b"same"), digest(b"same"));
    assert_ne!(digest(b"same"), digest(b"other"));
}
