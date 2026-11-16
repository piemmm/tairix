//! A machine's folder (`plans/BEEB.md` §5): its name in the application's
//! documents folder, the blank discs and tapes made in it, what of it the
//! drives and the deck are offered, and the digest that says whether a file
//! is still what a suspended machine left it.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::media::{disc_ending, tape_ending, Purpose};
use super::menu::{Offered, FOLDER_ROWS};
use crate::machine::Model;
use crate::media::c1541::{d64, layout::NAME_BYTES};
use crate::media::disc::{ImageFormat, SECTORS_PER_TRACK, TRACK_BYTES};
use crate::media::tap;
use crate::worker::protocol::DiscFormat;

/// The longest name a folder or a file of it is given.
const MAX_NAME: usize = 64;

/// The most names a folder or a file is tried under before it is refused as
/// crowded out: a bound on the probing, not on how many are kept.
pub const MOST_NAMES: u32 = 64;

/// The tracks a blank disc is formatted with: an 80-track drive's.
const BLANK_TRACKS: usize = 80;

/// What a UEF image begins with: its magic and version 0.10.
const UEF_HEADER: &[u8] = b"UEF File!\0\x0A\x00";

/// `name` made a name a folder holds: no separator, no control character, no
/// leading dot or space, at most `MAX_NAME` bytes; `fallback` where nothing
/// is left of it.
#[must_use]
pub fn plain(name: &str, fallback: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != ':')
        .collect();
    let trimmed = kept.trim().trim_start_matches('.').trim_start();
    let at = trimmed.floor_char_boundary(MAX_NAME);
    let cut = trimmed[..at].trim_end();
    if cut.is_empty() || tairix_path::validate_visible_name(cut).is_err() {
        String::from(fallback)
    } else {
        String::from(cut)
    }
}

/// `stem` with `ending`, or the least number after it that `taken` does not
/// hold: what a new folder or file is named. None once [`MOST_NAMES`] are
/// tried.
#[must_use]
pub fn fresh(stem: &str, ending: &str, taken: impl Fn(&str) -> bool) -> Option<String> {
    (1..=MOST_NAMES).find_map(|n| {
        let suffix = if n == 1 {
            String::new()
        } else {
            format!(" {n}")
        };
        let room = MAX_NAME.saturating_sub(suffix.len() + ending.len());
        let base = &stem[..stem.floor_char_boundary(room)];
        let name = format!("{base}{suffix}{ending}");
        (!taken(&name)).then_some(name)
    })
}

/// The folder a machine called `machine` is given among those `taken`.
#[must_use]
pub fn folder_for(machine: &str, taken: impl Fn(&str) -> bool) -> Option<String> {
    fresh(&plain(machine, "Machine"), "", taken)
}

/// A blank single-sided disc, formatted as the DFS formats one: an empty
/// catalogue of `BLANK_TRACKS` tracks.
#[must_use]
pub fn blank_disc() -> Vec<u8> {
    let mut image = vec![0; BLANK_TRACKS * TRACK_BYTES];
    let sectors = BLANK_TRACKS * SECTORS_PER_TRACK;
    image[..8].fill(b' ');
    image[256..260].fill(b' ');
    image[256 + 6] = u8::try_from(sectors >> 8).unwrap_or(0) & 3;
    image[256 + 7] = u8::try_from(sectors & 0xFF).unwrap_or(0);
    image
}

/// What a blank medium for `purpose` on a `model`'s machine is named, before
/// its number, and its ending; `None` for what is not a disc or tape.
#[must_use]
pub const fn blank_name(purpose: Purpose, model: Model) -> Option<(&'static str, &'static str)> {
    match (purpose, model) {
        (Purpose::Disc(_), Model::C64) => Some(("Disk", disc_ending(DiscFormat::D64))),
        (Purpose::Disc(_), _) => Some(("Disc", disc_ending(DiscFormat::Acorn(ImageFormat::Ssd)))),
        (Purpose::Tape, model) => Some(("Tape", tape_ending(model))),
        _ => None,
    }
}

/// A blank medium for `purpose` on a `model`'s machine, made as the file
/// `name`: an empty DFS disc, a 1541 disk formatted with the file's name and
/// an ID of its own, or a tape of nothing.
#[must_use]
pub fn blank_image(purpose: Purpose, model: Model, name: &str) -> Vec<u8> {
    match (purpose, model) {
        (Purpose::Disc(_), Model::C64) => d64::formatted(&disk_name(name), disk_id(name)),
        (Purpose::Disc(_), _) => blank_disc(),
        (_, Model::C64) => tap::Tape::blank().encode(),
        _ => UEF_HEADER.to_vec(),
    }
}

/// A 1541 disk's name from the file `name`: its stem's letters, digits and
/// spaces, the letters capitals, as the upper-case set shows them.
fn disk_name(name: &str) -> Vec<u8> {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    stem.bytes()
        .filter(|byte| byte.is_ascii_alphanumeric() || *byte == b' ')
        .map(|byte| byte.to_ascii_uppercase())
        .take(usize::from(NAME_BYTES))
        .collect()
}

/// An ID for the disk made as the file `name`, two letters or digits from
/// its digest, so disks made in turn tell themselves apart to a DOS.
fn disk_id(name: &str) -> [u8; 2] {
    const SYMBOLS: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let digest = digest(name.as_bytes());
    digest[..2].try_into().map_or(*b"00", |pair: [u8; 2]| {
        pair.map(|byte| SYMBOLS[usize::from(byte) % SYMBOLS.len()])
    })
}

/// `name` as it sorts whatever its case.
fn folded(name: &str) -> impl Iterator<Item = char> + '_ {
    name.chars().flat_map(char::to_lowercase)
}

/// What a folder holding `names` offers a `model`'s machine fitted with
/// `drives`: the disc and tape images it takes, in name order, at most
/// [`FOLDER_ROWS`] of each.
#[must_use]
pub fn offered(names: &[String], model: Model, drives: [bool; 2]) -> Offered {
    let purpose = |name: &String| Purpose::of(name, model, drives);
    let mut discs: Vec<String> = names
        .iter()
        .filter(|name| matches!(purpose(name), Some(Purpose::Disc(_))))
        .cloned()
        .collect();
    let mut tapes: Vec<String> = names
        .iter()
        .filter(|name| purpose(name) == Some(Purpose::Tape))
        .cloned()
        .collect();
    for list in [&mut discs, &mut tapes] {
        list.sort_unstable_by(|a, b| folded(a).cmp(folded(b)).then_with(|| a.cmp(b)));
        list.truncate(FOLDER_ROWS);
    }
    Offered {
        reachable: true,
        discs,
        tapes,
    }
}

/// What a file held when it was digested.
#[must_use]
pub fn digest(bytes: &[u8]) -> [u8; 32] {
    tairix_crypto::sha256(bytes)
}

#[cfg(test)]
#[path = "folder_tests.rs"]
mod tests;
