//! Keeping the pictures a saver hands over through its keeping feed: each one
//! a PNG in the user's `Home/Pictures/<saver>/`, named for what the saver
//! called it and the moment it was kept, and never written over another.
//!
//! A picture is written whole to a hidden scratch file first and only then
//! renamed onto its name, so one cut short by a session torn down mid-write
//! never stands under a name: the next keep replaces the scratch file.
//!
//! Both names come from outside the desktop — the folder from the saver's
//! manifest, the picture's from the saver itself — so each must be a plain
//! file name the filesystem takes and no listing hides, or nothing is kept.
//! The work is a worker's, behind [`PictureFiles`] — the VFS on a running
//! system, a table in tests — since writing a file has no place on the serve
//! loop.

use alloc::format;
use alloc::string::String;
use core::fmt;

use tairix_abi::home::{HOME_FILES_DIR, HOME_FILES_PICTURES_DIR};
use tairix_abi::time::{CivilTime, WallClockReading};
use tairix_abi::Errno;
use tairix_image::{EncodeError, PictureKind, PictureSource};
use tairix_raster::Surface;
use tairix_tz::CivilClock;

/// The most names one picture tries — its own, then numbered from two —
/// before it is refused as crowded out: a bound on the probing, not on how
/// many pictures are kept.
pub const MOST_NAMES: u32 = 64;

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

/// The hidden file a picture is written to before it is given its name.
pub const SCRATCH_NAME: &str = ".keeping.png";

/// What kept pictures are written through.
pub trait PictureFiles {
    /// Make the folder `path`; one already there is no failure.
    ///
    /// # Errors
    ///
    /// Why the folder could not be made.
    fn make_folder(&mut self, path: &str) -> Result<(), Errno>;

    /// Write `bytes` durably as the scratch file `path`, in place of one an
    /// earlier keep left there, leaving nothing behind if it fails.
    ///
    /// # Errors
    ///
    /// Why the file could not be written.
    fn write_scratch(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno>;

    /// Give the scratch file `scratch` the name `path`, never replacing a
    /// file already there.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] for a name already taken, or why the file
    /// could not be renamed.
    fn place(&mut self, scratch: &str, path: &str) -> Result<(), Errno>;

    /// Remove the scratch file `path`.
    fn discard(&mut self, path: &str);
}

/// Why a picture was not kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Unkept {
    /// The session knows no home to keep it in.
    NoHome,
    /// The saver's title or the picture's name is no plain file name.
    Unnamed,
    /// A folder on the way could not be made.
    Folder(String, Errno),
    /// The picture could not be written as a PNG.
    Encode(EncodeError),
    /// The file could not be written.
    Write(String, Errno),
    /// Every name it might take is taken.
    Crowded(String),
}

impl fmt::Display for Unkept {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHome => f.write_str("there is no home folder to keep it in"),
            Self::Unnamed => f.write_str("the saver named it with no plain file name"),
            Self::Folder(path, errno) => write!(f, "{path} could not be made: {errno}"),
            Self::Encode(error) => write!(f, "it could not be written as a PNG: {error}"),
            Self::Write(path, errno) => write!(f, "{path} could not be written: {errno}"),
            Self::Crowded(path) => write!(f, "every name for it in {path} is taken"),
        }
    }
}

/// Keep `picture` among the pictures in `home`, kept at `when` if the clock
/// is set: the path it was written to.
///
/// # Errors
///
/// Why it was not kept.
pub fn keep(
    files: &mut dyn PictureFiles,
    home: Option<&str>,
    picture: &Picture,
    when: Option<CivilTime>,
) -> Result<String, Unkept> {
    // Only an absolute home is one: anything else is a malformed environment,
    // refused rather than written relative to wherever the session runs.
    let home = home
        .filter(|home| home.starts_with('/'))
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| !home.is_empty())
        .ok_or(Unkept::NoHome)?;
    if !plain_name(&picture.folder) || !plain_name(&picture.name) {
        return Err(Unkept::Unnamed);
    }
    let mut folder = String::from(home);
    for name in [
        HOME_FILES_DIR,
        HOME_FILES_PICTURES_DIR,
        picture.folder.as_str(),
    ] {
        folder.push('/');
        folder.push_str(name);
        files
            .make_folder(&folder)
            .map_err(|errno| Unkept::Folder(folder.clone(), errno))?;
    }
    let png = tairix_image::encode_png(&Rgba(picture)).map_err(Unkept::Encode)?;
    let scratch = format!("{folder}/{SCRATCH_NAME}");
    files
        .write_scratch(&scratch, &png)
        .map_err(|errno| Unkept::Write(scratch.clone(), errno))?;
    let stem = stem(&picture.name, when);
    for number in 1..=MOST_NAMES {
        let path = if number == 1 {
            format!("{folder}/{stem}.png")
        } else {
            format!("{folder}/{stem} {number}.png")
        };
        match files.place(&scratch, &path) {
            Ok(()) => return Ok(path),
            Err(Errno::AlreadyExists) => {}
            Err(errno) => {
                files.discard(&scratch);
                return Err(Unkept::Write(path, errno));
            }
        }
    }
    files.discard(&scratch);
    Err(Unkept::Crowded(folder))
}

/// The moment a picture kept at `reading` is named for, in `civil`'s zone,
/// as the bar would tell it: `None` while the clock is not set, rather than a
/// date it was not.
#[must_use]
pub fn kept_at(reading: Option<WallClockReading>, civil: &CivilClock<'_>) -> Option<CivilTime> {
    let reading = reading.filter(|reading| reading.state().is_set())?;
    Some(civil.local(reading.time()).civil)
}

/// Whether `name` is a file name the filesystem takes and no listing hides.
fn plain_name(name: &str) -> bool {
    !name.starts_with('.') && tairix_path::validate_file_name(name).is_ok()
}

/// What a picture called `name` is called before any number: the name and
/// the moment it was kept, spelled with no character a file name may not
/// hold, or the name alone when the clock is not set rather than a date it
/// was not.
fn stem(name: &str, when: Option<CivilTime>) -> String {
    match when {
        Some(at) => format!(
            "{name} {:04}-{:02}-{:02} {:02}.{:02}.{:02}",
            at.year, at.month, at.day, at.hour, at.minute, at.second
        ),
        None => String::from(name),
    }
}

/// A picture as the encoder reads it: straight-alpha RGBA rows, which its
/// opaque pixels already are.
struct Rgba<'a>(&'a Picture);

impl PictureSource for Rgba<'_> {
    fn width(&self) -> u32 {
        self.0.surface.width()
    }

    fn height(&self) -> u32 {
        self.0.surface.height()
    }

    fn kind(&self) -> PictureKind<'_> {
        PictureKind::Rgba
    }

    fn read_row(&self, y: u32, samples: &mut [u8], _mask: &mut [u8]) {
        let width = self.0.surface.width() as usize;
        let start = (y as usize).saturating_mul(width);
        let Some(row) = self
            .0
            .surface
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
}

#[cfg(test)]
#[path = "keep_tests.rs"]
mod tests;
