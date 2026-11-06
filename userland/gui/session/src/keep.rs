//! Keeping a file the desktop made for its user — a screen capture, a
//! screensaver's finished picture — in a folder of the user's home, under a
//! name that says what it is and when it was kept, never over another
//! (`plans/CAPTURE.md`).
//!
//! One naming rule serves every kept file: what it is, then the moment it was
//! kept in the user's own zone — `Screenshot 2026-11-04 at 14.03.07.png` —
//! numbered ` 2` to ` 64` where that name is taken, and the moment left out
//! while the clock is not set rather than a date it was not. A file is written
//! whole to a hidden scratch file of its own first, numbered the same way and
//! made afresh, so no two keeps — nor a keep and a file an earlier session
//! left — ever share one, and only then renamed onto its name without
//! replacing: one cut short by a session torn down mid-write never stands
//! under a name.
//!
//! Some names come from outside the desktop — a saver's title and the name it
//! gave its picture — so every name under the home must be a plain file name
//! the filesystem takes and no listing hides, or nothing is kept. The work is
//! a worker's, behind [`KeptFiles`] — the VFS on a running system, a table in
//! tests — since writing a file has no place on the serve loop.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::home::{HOME_FILES_DIR, HOME_FILES_PICTURES_DIR};
use tairix_abi::time::{CivilTime, Time64, WallClockReading};
use tairix_abi::Errno;
use tairix_image::{Density, EncodeError, PictureKind, PictureSource};
use tairix_raster::Surface;
use tairix_tz::CivilClock;

/// The most names one file, or the scratch file it is written to, tries — its
/// own, then numbered from two — before it is refused as crowded out: a bound
/// on the probing, not on how many files are kept.
pub const MOST_NAMES: u32 = 64;

/// The ending of a kept picture.
pub const PNG_ENDING: &str = "png";

/// What every scratch file's name begins with: hidden, so no listing shows
/// one.
const SCRATCH: &str = ".keeping";

/// The widest moment a name is given while the year has four digits: what a
/// name is checked to fit with before anything is written under it.
const WIDEST_MOMENT: CivilTime = CivilTime {
    year: 9999,
    month: 12,
    day: 31,
    hour: 23,
    minute: 59,
    second: 59,
};

/// Where under a home a kept file goes, and what it is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Keeping<'a> {
    /// The folders under the home it goes in, outermost first, each made
    /// where it is absent.
    pub folders: &'a [&'a str],
    /// What it is: its name before the moment it was kept.
    pub what: &'a str,
    /// Its file ending, without the dot.
    pub ending: &'a str,
}

/// A whole picture a saver handed over to keep.
#[derive(Debug)]
pub struct Picture {
    /// The folder under the home's pictures it goes in: the saver's title.
    pub folder: String,
    /// What the saver called it.
    pub name: String,
    /// The picture, opaque, so each pixel's colour is as stored.
    pub surface: Surface,
}

/// What kept files are written through.
pub trait KeptFiles {
    /// Make the folder `path`; one already there is no failure.
    ///
    /// # Errors
    ///
    /// Why the folder could not be made.
    fn make_folder(&mut self, path: &str) -> Result<(), Errno>;

    /// Write `bytes` durably as the new scratch file `path`, leaving nothing
    /// behind if it fails. A file already there is never written over: it may
    /// be the only copy of another keep's.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] for a name already taken, or why the file could
    /// not be written.
    fn write_scratch(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno>;

    /// Give the scratch file `scratch` the name `path`, never replacing a
    /// file already there.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] for a name already taken, or why the file
    /// could not be renamed.
    fn place(&mut self, scratch: &str, path: &str) -> Result<(), Errno>;

    /// Remove the file `path`.
    fn discard(&mut self, path: &str);

    /// What the regular file at `path` is, or `None` where there is none.
    fn found(&mut self, path: &str) -> Option<Found>;
}

/// What [`KeptFiles::found`] reads of a regular file.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Found {
    /// When it last changed.
    pub changed: Time64,
    /// Its length in bytes.
    pub len: u64,
}

/// Why a file was not kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Unkept {
    /// The session knows no home to keep it in.
    NoHome,
    /// A name it was to go under is no plain file name.
    Unnamed,
    /// A folder on the way could not be made.
    Folder(String, Errno),
    /// The picture could not be written as a PNG.
    Encode(EncodeError),
    /// The file could not be written.
    Write(String, Errno),
    /// Every name it might take is taken.
    Crowded(String),
    /// Too many files were already waiting to be kept.
    Busy,
}

impl fmt::Display for Unkept {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHome => f.write_str("there is no home folder to keep it in"),
            Self::Unnamed => f.write_str("it was named with no plain file name"),
            Self::Folder(path, errno) => write!(f, "{path} could not be made: {errno}"),
            Self::Encode(error) => write!(f, "it could not be written as a PNG: {error}"),
            Self::Write(path, errno) => write!(f, "{path} could not be written: {errno}"),
            Self::Crowded(path) => write!(f, "every name for it in {path} is taken"),
            Self::Busy => f.write_str("too many files were already waiting to be kept"),
        }
    }
}

/// The absolute home `home` names, without a trailing slash, or `None`:
/// anything else is a malformed environment, refused rather than written
/// relative to wherever the session runs.
pub(crate) fn home_of(home: Option<&str>) -> Option<&str> {
    home.filter(|home| home.starts_with('/'))
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| !home.is_empty())
}

/// The path of `folders` under `home`.
pub(crate) fn folder_path(home: &str, folders: &[&str]) -> String {
    let mut path = String::from(home);
    for name in folders {
        path.push('/');
        path.push_str(name);
    }
    path
}

/// The folder `keeping` names under `home`, each folder on the way made.
///
/// # Errors
///
/// [`Unkept::NoHome`], [`Unkept::Unnamed`] for a name no plain file name — a
/// folder's, or the one the file would take with its moment and its highest
/// number — and [`Unkept::Folder`] for a folder that could not be made.
pub fn kept_folder(
    files: &mut dyn KeptFiles,
    home: Option<&str>,
    keeping: &Keeping<'_>,
) -> Result<String, Unkept> {
    let home = home_of(home).ok_or(Unkept::NoHome)?;
    let named = keeping.folders.iter().copied().all(plain_name)
        && plain_name(keeping.what)
        && plain_name(keeping.ending)
        && plain_name(&kept_name(keeping, Some(WIDEST_MOMENT), MOST_NAMES));
    if !named {
        return Err(Unkept::Unnamed);
    }
    let mut folder = String::from(home);
    for name in keeping.folders {
        folder.push('/');
        folder.push_str(name);
        files
            .make_folder(&folder)
            .map_err(|errno| Unkept::Folder(folder.clone(), errno))?;
    }
    Ok(folder)
}

/// Keep `bytes`, a whole file, as `keeping` says, kept at `when` if the clock
/// is set: the path it was written to.
///
/// # Errors
///
/// Why it was not kept; a scratch file it wrote is removed.
pub fn keep_bytes(
    files: &mut dyn KeptFiles,
    home: Option<&str>,
    keeping: &Keeping<'_>,
    bytes: &[u8],
    when: Option<CivilTime>,
) -> Result<String, Unkept> {
    let folder = kept_folder(files, home, keeping)?;
    write_kept(files, &folder, keeping, bytes, when)
}

/// Keep `picture` as a PNG recording `density` where one is given, as
/// `keeping` says, kept at `when` if the clock is set: the path it was written
/// to. The PNG's bytes are wiped once written.
///
/// # Errors
///
/// Why it was not kept.
pub fn keep_png(
    files: &mut dyn KeptFiles,
    home: Option<&str>,
    keeping: &Keeping<'_>,
    picture: &Surface,
    density: Option<Density>,
    when: Option<CivilTime>,
) -> Result<String, Unkept> {
    let folder = kept_folder(files, home, keeping)?;
    write_png(files, &folder, keeping, (picture, density), when)
}

/// [`keep_png`] into `folder`, already made.
fn write_png(
    files: &mut dyn KeptFiles,
    folder: &str,
    keeping: &Keeping<'_>,
    (picture, density): (&Surface, Option<Density>),
    when: Option<CivilTime>,
) -> Result<String, Unkept> {
    let mut png = encode_png(picture, density)?;
    let kept = write_kept(files, folder, keeping, &png, when);
    tairix_util::secret::wipe(&mut png);
    kept
}

/// Keep `picture`, which a saver handed over, among the pictures in `home`,
/// in the folder its saver is titled, kept at `when` if the clock is set: the
/// path it was written to.
///
/// What an earlier keep left in that folder is settled first, each named for
/// its saver, since a scratch file does not say what its picture was called,
/// and for the moment it last changed in `civil`'s zone.
///
/// # Errors
///
/// Why it was not kept.
pub fn keep_picture(
    files: &mut dyn KeptFiles,
    home: Option<&str>,
    picture: &Picture,
    (civil, when): (&CivilClock<'_>, Option<CivilTime>),
) -> Result<String, Unkept> {
    let folders = [
        HOME_FILES_DIR,
        HOME_FILES_PICTURES_DIR,
        picture.folder.as_str(),
    ];
    let keeping = Keeping {
        folders: &folders,
        what: &picture.name,
        ending: PNG_ENDING,
    };
    let folder = kept_folder(files, home, &keeping)?;
    let leftover = Keeping {
        what: &picture.folder,
        ..keeping
    };
    settle_leftovers(files, &folder, &leftover, civil, drop);
    write_png(files, &folder, &keeping, (&picture.surface, None), when)
}

/// Write `bytes` aside in a scratch file of their own in `folder` and give
/// them their name, the scratch file removed if no name could be given.
fn write_kept(
    files: &mut dyn KeptFiles,
    folder: &str,
    keeping: &Keeping<'_>,
    bytes: &[u8],
    when: Option<CivilTime>,
) -> Result<String, Unkept> {
    let (scratch, ()) = claim_scratch(folder, keeping.ending, |scratch| {
        files.write_scratch(scratch, bytes)
    })?;
    let placed = place_kept(files, folder, &scratch, keeping, when);
    if placed.is_err() {
        files.discard(&scratch);
    }
    placed
}

/// Give `scratch`, a file already whole in `folder`, the first free name the
/// rule gives what `keeping` keeps at `when`, never replacing a file there:
/// the path it took.
///
/// The scratch file is left where it is if no name can be given, since it may
/// be the only copy of what it holds.
///
/// # Errors
///
/// [`Unkept::Unnamed`] for a name no plain file name, [`Unkept::Write`] for a
/// rename refused other than for a taken name, and [`Unkept::Crowded`] once
/// every name is taken.
pub fn place_kept(
    files: &mut dyn KeptFiles,
    folder: &str,
    scratch: &str,
    keeping: &Keeping<'_>,
    when: Option<CivilTime>,
) -> Result<String, Unkept> {
    for number in 1..=MOST_NAMES {
        let name = kept_name(keeping, when, number);
        if !plain_name(&name) {
            return Err(Unkept::Unnamed);
        }
        let path = format!("{folder}/{name}");
        match files.place(scratch, &path) {
            Ok(()) => return Ok(path),
            Err(Errno::AlreadyExists) => {}
            Err(errno) => return Err(Unkept::Write(path, errno)),
        }
    }
    Err(Unkept::Crowded(String::from(folder)))
}

/// The name the rule gives the `number`th try at a file of `keeping` kept at
/// `when`: what it is, the moment, the number from two, then its ending.
#[must_use]
pub fn kept_name(keeping: &Keeping<'_>, when: Option<CivilTime>, number: u32) -> String {
    let moment = when.map_or_else(String::new, |at| {
        format!(
            " {:04}-{:02}-{:02} at {:02}.{:02}.{:02}",
            at.year, at.month, at.day, at.hour, at.minute, at.second
        )
    });
    let numbered = if number > 1 {
        format!(" {number}")
    } else {
        String::new()
    };
    format!("{}{moment}{numbered}.{}", keeping.what, keeping.ending)
}

/// The `number`th hidden scratch file a file ending in `ending` may be written
/// to in `folder` before it is given its name: numbered from two as a kept
/// name is.
#[must_use]
pub fn scratch_path(folder: &str, ending: &str, number: u32) -> String {
    if number > 1 {
        format!("{folder}/{SCRATCH} {number}.{ending}")
    } else {
        format!("{folder}/{SCRATCH}.{ending}")
    }
}

/// Claim a scratch file of its own for a file ending in `ending` in `folder`:
/// the first of [`scratch_path`]'s names `create` makes afresh, `create`
/// refusing one already taken with [`Errno::AlreadyExists`] rather than
/// opening it. The path claimed, and what `create` answered.
///
/// # Errors
///
/// [`Unkept::Write`] for a creation refused other than for a taken name, and
/// [`Unkept::Crowded`] once every name is taken.
pub fn claim_scratch<T>(
    folder: &str,
    ending: &str,
    mut create: impl FnMut(&str) -> Result<T, Errno>,
) -> Result<(String, T), Unkept> {
    for number in 1..=MOST_NAMES {
        let scratch = scratch_path(folder, ending, number);
        match create(&scratch) {
            Ok(made) => return Ok((scratch, made)),
            Err(Errno::AlreadyExists) => {}
            Err(errno) => return Err(Unkept::Write(scratch, errno)),
        }
    }
    Err(Unkept::Crowded(String::from(folder)))
}

/// Settle `scratch`, a scratch file in `folder`: give it the name `keeping`
/// gives it at the moment `when` makes of what [`KeptFiles::found`] read of
/// it, or remove it where it holds nothing. The path it took, if it took one;
/// one no longer there takes none.
///
/// # Errors
///
/// As [`place_kept`], the scratch file left where it is.
pub fn settle_scratch(
    files: &mut dyn KeptFiles,
    (folder, scratch): (&str, &str),
    keeping: &Keeping<'_>,
    when: impl FnOnce(Found) -> Option<CivilTime>,
) -> Result<Option<String>, Unkept> {
    let Some(found) = files.found(scratch) else {
        return Ok(None);
    };
    if found.len == 0 {
        files.discard(scratch);
        return Ok(None);
    }
    place_kept(files, folder, scratch, keeping, when(found)).map(Some)
}

/// Settle every scratch file of `keeping` an earlier keep left in `folder`,
/// each named for the moment it last changed in `civil`'s zone, or removed
/// where it holds nothing: each path one took is handed to `settled`.
pub fn settle_leftovers(
    files: &mut dyn KeptFiles,
    folder: &str,
    keeping: &Keeping<'_>,
    civil: &CivilClock<'_>,
    mut settled: impl FnMut(String),
) {
    let when = |found: Found| {
        (found.changed != Time64::UNIX_EPOCH).then(|| civil.local(found.changed).civil)
    };
    for number in 1..=MOST_NAMES {
        let scratch = scratch_path(folder, keeping.ending, number);
        if let Ok(Some(kept)) = settle_scratch(files, (folder, &scratch), keeping, when) {
            settled(kept);
        }
    }
}

/// `picture`, opaque, as a PNG recording `density` where one is given.
///
/// # Errors
///
/// [`Unkept::Encode`] for a picture the encoder refused.
pub fn encode_png(picture: &Surface, density: Option<Density>) -> Result<Vec<u8>, Unkept> {
    tairix_image::encode_png(&Opaque { picture, density }).map_err(Unkept::Encode)
}

/// The moment a file kept at `reading` is named for, in `civil`'s zone, as the
/// bar would tell it: `None` while the clock is not set, rather than a date it
/// was not.
#[must_use]
pub fn kept_at(reading: Option<WallClockReading>, civil: &CivilClock<'_>) -> Option<CivilTime> {
    let reading = reading.filter(|reading| reading.state().is_set())?;
    Some(civil.local(reading.time()).civil)
}

/// Whether `name` is a file name the filesystem takes and no listing hides.
fn plain_name(name: &str) -> bool {
    !name.starts_with('.') && tairix_path::validate_file_name(name).is_ok()
}

/// A picture as the encoder reads it: straight-alpha RGBA rows, which an
/// opaque surface's premultiplied pixels already are.
struct Opaque<'a> {
    picture: &'a Surface,
    density: Option<Density>,
}

impl PictureSource for Opaque<'_> {
    fn width(&self) -> u32 {
        self.picture.width()
    }

    fn height(&self) -> u32 {
        self.picture.height()
    }

    fn kind(&self) -> PictureKind<'_> {
        PictureKind::Rgba
    }

    fn read_row(&self, y: u32, samples: &mut [u8], _mask: &mut [u8]) {
        let width = self.picture.width() as usize;
        let start = (y as usize).saturating_mul(width);
        let Some(row) = self
            .picture
            .pixels()
            .get(start..start.saturating_add(width))
        else {
            samples.fill(0);
            return;
        };
        for (out, pixel) in samples.as_chunks_mut::<4>().0.iter_mut().zip(row) {
            *out = [pixel.r, pixel.g, pixel.b, pixel.a];
        }
    }

    fn density(&self) -> Option<Density> {
        self.density
    }
}

#[cfg(test)]
#[path = "keep_tests.rs"]
pub(crate) mod tests;
