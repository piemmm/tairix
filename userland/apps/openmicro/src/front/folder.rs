//! A machine's folder (`plans/BEEB.md` §5): its name in the application's
//! documents folder, the blank discs and tapes made in it, what of it the
//! drives and the deck are offered, and the digest that says whether a file
//! is still what a suspended machine left it.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::menu::{Offered, FOLDER_ROWS};
use crate::media::disc::{SECTORS_PER_TRACK, TRACK_BYTES};

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

/// A blank tape: a UEF image of nothing.
#[must_use]
pub fn blank_tape() -> Vec<u8> {
    UEF_HEADER.to_vec()
}

/// Whether `name` is a disc image, by its ending.
fn is_disc(name: &str) -> bool {
    ends_with(name, &[".ssd", ".dsd"])
}

/// Whether `name` is a tape image, by its ending.
fn is_tape(name: &str) -> bool {
    ends_with(name, &[".uef", ".csw"])
}

fn ends_with(name: &str, endings: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    endings.iter().any(|ending| lower.ends_with(ending))
}

/// `name` as it sorts whatever its case.
fn folded(name: &str) -> impl Iterator<Item = char> + '_ {
    name.chars().flat_map(char::to_lowercase)
}

/// What a folder holding `names` offers a machine: its disc and tape images
/// in name order, at most [`FOLDER_ROWS`] of each.
#[must_use]
pub fn offered(names: &[String]) -> Offered {
    let mut discs: Vec<String> = names.iter().filter(|n| is_disc(n)).cloned().collect();
    let mut tapes: Vec<String> = names.iter().filter(|n| is_tape(n)).cloned().collect();
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
