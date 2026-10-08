//! Keeping a reveal's whole pictures: each one a PNG in the user's
//! `UserFiles/Pictures/Raytracing/`, named for its setting and the moment it
//! was finished, and never written over another.
//!
//! The work is the tracing thread's, behind [`PictureFiles`] — the VFS on a
//! running system, a table in tests — since writing a file has no place on
//! the serve loop.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::home::{HOME_USER_FILES_DIR, USER_FILES_PICTURES_DIR};
use tairix_abi::time::CivilTime;
use tairix_abi::Errno;
use tairix_image::{EncodeError, PictureKind, PictureSource};
use tairix_raster::Pixel;
use tairix_raytrace::Setting;

/// The folders within a home kept pictures go into, outermost first.
pub const FOLDERS: [&str; 3] = [HOME_USER_FILES_DIR, USER_FILES_PICTURES_DIR, "Raytracing"];

/// The most names one picture tries — its own, then numbered from two —
/// before it is refused as crowded out: a bound on the probing, not on how
/// many pictures are kept.
const MOST_NAMES: u32 = 64;

/// A whole picture, as traced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Picture {
    /// The setting its scene was composed in.
    pub setting: Setting,
    /// The seed its scene was composed from, which names it when the clock
    /// is not set.
    pub seed: u64,
    /// Its width and height in pixels.
    pub size: (u32, u32),
    /// Its pixels, row by row; opaque, so each one's colour is as stored.
    pub pixels: Vec<Pixel>,
}

/// What kept pictures are written through.
pub trait PictureFiles {
    /// Make the folder `path`; one already there is no failure.
    ///
    /// # Errors
    ///
    /// Why the folder could not be made.
    fn make_folder(&mut self, path: &str) -> Result<(), Errno>;

    /// Write `bytes` as a new file at `path`, durably, never replacing one and
    /// leaving nothing behind if it fails.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] for a name already taken, or why the file
    /// could not be written.
    fn create(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno>;
}

/// Why a picture was not kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Unkept {
    /// The session knows no home to keep it in.
    NoHome,
    /// The heap would not hold a copy of the picture as it was traced.
    Unheld(Setting),
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
            Self::Unheld(setting) => write!(
                f,
                "the {} picture was too large to hold a copy of",
                setting.name()
            ),
            Self::Folder(path, errno) => write!(f, "{path} could not be made: {errno}"),
            Self::Encode(error) => write!(f, "it could not be written as a PNG: {error}"),
            Self::Write(path, errno) => write!(f, "{path} could not be written: {errno}"),
            Self::Crowded(path) => write!(f, "every name for it in {path} is taken"),
        }
    }
}

/// Keep `picture` among the pictures in `home`, finished at `when` if the
/// clock was set: the path it was written to.
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
    let mut folder = String::from(home);
    for name in FOLDERS {
        folder.push('/');
        folder.push_str(name);
        files
            .make_folder(&folder)
            .map_err(|errno| Unkept::Folder(folder.clone(), errno))?;
    }
    let png = tairix_image::encode_png(&Rgba(picture)).map_err(Unkept::Encode)?;
    let stem = stem(picture, when);
    for number in 1..=MOST_NAMES {
        let path = if number == 1 {
            format!("{folder}/{stem}.png")
        } else {
            format!("{folder}/{stem} {number}.png")
        };
        match files.create(&path, &png) {
            Ok(()) => return Ok(path),
            Err(Errno::AlreadyExists) => {}
            Err(errno) => return Err(Unkept::Write(path, errno)),
        }
    }
    Err(Unkept::Crowded(folder))
}

/// What `picture` is called before any number: its setting and the moment it
/// was finished, spelled with no character a file name may not hold, or its
/// scene's seed when the clock was not set rather than a date it was not.
fn stem(picture: &Picture, when: Option<CivilTime>) -> String {
    let name = picture.setting.name();
    match when {
        Some(at) => format!(
            "{name} {:04}-{:02}-{:02} {:02}.{:02}.{:02}",
            at.year, at.month, at.day, at.hour, at.minute, at.second
        ),
        None => format!("{name} {:016x}", picture.seed),
    }
}

/// A picture as the encoder reads it: straight-alpha RGBA rows, which its
/// opaque pixels already are.
struct Rgba<'a>(&'a Picture);

impl PictureSource for Rgba<'_> {
    fn width(&self) -> u32 {
        self.0.size.0
    }

    fn height(&self) -> u32 {
        self.0.size.1
    }

    fn kind(&self) -> PictureKind<'_> {
        PictureKind::Rgba
    }

    fn read_row(&self, y: u32, samples: &mut [u8], _mask: &mut [u8]) {
        let width = self.0.size.0 as usize;
        let start = (y as usize).saturating_mul(width);
        let Some(row) = self.0.pixels.get(start..start.saturating_add(width)) else {
            samples.fill(0);
            return;
        };
        for (out, pixel) in samples.as_chunks_mut::<4>().0.iter_mut().zip(row) {
            *out = [pixel.r, pixel.g, pixel.b, pixel.a];
        }
    }
}

#[cfg(test)]
#[path = "album_tests.rs"]
mod tests;
