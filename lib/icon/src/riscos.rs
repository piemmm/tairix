//! A RISC OS application pictured as RISC OS's Filer pictures it: by the sprite
//! of its own name that its `!Boot` loads with `IconSprites`, else the one its
//! own `!Sprites` holds.
//!
//! The Filer runs an application's `!Boot` on merely seeing it. Nothing here
//! runs one: the file is read as data through `lib/obey`'s read-only mode,
//! which takes only the variables it sets and the files its `IconSprites`
//! commands load, so a browsed volume costs reads and never code. A command
//! whose file lies outside the application, or depends on anything the file
//! does not set, names nothing.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_fsmeta::preset::acorn;
use tairix_image::SpriteName;
use tairix_obey::command::{read_only, Observer, MOST_LINE};
use tairix_obey::vars::read_expanded;
use tairix_obey::{Budget, Store};
use tairix_raster::Surface;
use tairix_util::fallible;

use crate::artwork::{ArtworkRasteriser, ArtworkReader};
use crate::picture::Artwork;
use crate::thumbnail::ArtworkDocument;

/// Largest `!Boot` scanned for its `IconSprites` commands, in bytes.
///
/// A fixed bound on untrusted input, not a capacity: a boot file is a handful
/// of command lines, so a longer one is not read as one.
pub const MAX_BOOT_BYTES: usize = 16 * 1024;

/// Largest sprite file an application is drawn from, in bytes.
///
/// A fixed bound on untrusted input, not a capacity: a sprite file holds an
/// application's whole set of icons, a few hundred kilobytes at the most, and
/// each one tried is streamed whole to the sandbox for one sprite of it.
pub const MAX_APPLICATION_SPRITES_BYTES: u64 = 2 << 20;

/// The file an application's `!Boot` is.
const BOOT: &str = "!Boot";

/// The sprite file an application keeps its icons in when its `!Boot` names
/// none.
const SPRITES: &str = "!Sprites";

/// What RISC OS puts after a sprite file's name for the copy drawn for a finer
/// screen — 180 dots to the inch, then 90 — each tried before the file itself,
/// so the finest an application ships is the one drawn.
const RESOLUTIONS: [&str; 3] = ["11", "22", ""];

/// The most sprite files an application is sought in: the most
/// [`boot_sprite_files`] names.
pub const MAX_SPRITE_FILES: usize = 12;

/// The most variables a scan keeps.
const MAX_VARIABLES: usize = 32;

/// The deepest a sprite file may lie inside its application: the most
/// components a path [`boot_sprite_files`] names has.
pub const MAX_DEPTH: usize = 4;

/// The most variable references one scan expands, however its macros nest: a
/// bound on the work one file can ask for, which nesting alone is not.
const MAX_REFERENCES: usize = 1024;

/// The most bytes one application's picture streams to the sandbox, across
/// every sprite file it tries.
pub const MAX_APPLICATION_PICTURE_BYTES: u64 = 2 * MAX_APPLICATION_SPRITES_BYTES;

/// What `<Obey$Dir>` expands to: the application's own directory, marked by a
/// byte no scanned line can hold, so a path starts there only when it really
/// does.
const APPLICATION: u8 = 0;

/// The sprites RISC OS's Filer shows an application by: one called what the
/// application is called, for a large icon, and one called that behind `sm`,
/// for a small one — each cut to a sprite name's length, and matched without
/// regard to case.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SpriteNames {
    large: SpriteName,
    small: SpriteName,
}

impl SpriteNames {
    /// The sprites the application called `name` is shown by, or `None` for a
    /// name no sprite can be called: one not wholly printable ASCII.
    #[must_use]
    pub fn of(name: &str) -> Option<Self> {
        Some(Self {
            large: cut(name)?,
            small: cut(&format!("sm{name}"))?,
        })
    }

    /// The large icon's sprite.
    #[must_use]
    pub fn large(&self) -> &[u8] {
        self.large.as_bytes()
    }

    /// The small icon's sprite.
    #[must_use]
    pub fn small(&self) -> &[u8] {
        self.small.as_bytes()
    }
}

/// The sprite name `name` cut to a sprite name's length makes.
fn cut(name: &str) -> Option<SpriteName> {
    SpriteName::new(name.get(..name.len().min(SpriteName::MAX_LEN))?)
}

/// The sprite files `boot`, an application's `!Boot`, loads its icons from with
/// `IconSprites`, in the order it loads them: each a path inside the
/// application's directory, its components spelled as TAIRiX spells them and
/// joined with `/`.
///
/// The file is read as data, never run, `<Obey$Dir>` standing for the
/// application's directory; a path variable (`App:`) is read as its `App$Path`
/// says. A file longer than [`MAX_BOOT_BYTES`] or holding a byte no command
/// line can is not read at all.
#[must_use]
pub fn boot_sprite_files(boot: &[u8]) -> Vec<String> {
    if boot.len() > MAX_BOOT_BYTES || !is_text(boot) {
        return Vec::new();
    }
    let (Some(mut vars), Some(mut found)) =
        (Store::with_room(MAX_VARIABLES, MOST_LINE), Found::new())
    else {
        return Vec::new();
    };
    let mut budget = Budget::new(MAX_REFERENCES);
    if read_only(boot, &[APPLICATION], &mut vars, &mut found, &mut budget).is_err() {
        return Vec::new();
    }
    found.files
}

/// Whether every byte of `bytes` is one a command line holds.
fn is_text(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|&byte| (byte >= b' ' && byte != 0x7F) || matches!(byte, b'\t' | b'\n' | b'\r'))
}

/// The sprite files a scan has found.
struct Found {
    files: Vec<String>,
}

impl Found {
    /// Room for every file it keeps, so keeping one never allocates the list;
    /// `None` where that room is refused.
    fn new() -> Option<Self> {
        let mut files = Vec::new();
        files.try_reserve_exact(MAX_SPRITE_FILES).ok()?;
        Some(Self { files })
    }

    /// Keep the RISC OS `path` as a sprite file, where it lies inside the
    /// application and is not kept already.
    fn keep(&mut self, path: &[u8]) {
        if self.files.len() == MAX_SPRITE_FILES {
            return;
        }
        if let Some(file) = within_application(path) {
            // RISC OS takes a file's name without regard to case.
            if !self
                .files
                .iter()
                .any(|kept| kept.eq_ignore_ascii_case(&file))
            {
                self.files.push(file);
            }
        }
    }
}

impl Observer for Found {
    fn icon_sprites(&mut self, file: &[u8], vars: &mut Store, budget: &mut Budget) {
        let Some(colon) = file.iter().position(|&b| b == b':') else {
            self.keep(file);
            return;
        };
        let (variable, rest) = (&file[..colon], &file[colon + 1..]);
        // A filing system's own name, `ADFS::Disc.$`, is no path variable.
        if variable.is_empty() || rest.first() == Some(&b':') {
            return;
        }
        let mut name = Vec::new();
        if !fallible::reserve(&mut name, variable.len() + 5) {
            return;
        }
        name.extend_from_slice(variable);
        name.extend_from_slice(b"$Path");
        let Ok(Some(prefixes)) = read_expanded(vars, &name, budget, MOST_LINE) else {
            return;
        };
        let mut path = Vec::new();
        for prefix in prefixes.split(|&b| b == b',') {
            path.clear();
            if !fallible::reserve(&mut path, prefix.len() + rest.len()) {
                return;
            }
            path.extend_from_slice(prefix);
            path.extend_from_slice(rest);
            self.keep(&path);
        }
    }
}

/// The RISC OS `path`, expanded, as a path inside the application spelled as
/// TAIRiX spells it, or `None` where it starts anywhere but the application's
/// own directory, lies more than [`MAX_DEPTH`] deep, or holds a leaf no RISC
/// OS name can be.
///
/// Only an ASCII path is taken: its leaves become TAIRiX names unchanged.
fn within_application(path: &[u8]) -> Option<String> {
    let inside = path.strip_prefix(&[APPLICATION, b'.'])?;
    if !inside.is_ascii() {
        return None;
    }
    let mut out = String::new();
    out.try_reserve_exact(inside.len()).ok()?;
    for (depth, leaf) in acorn::path_from_riscos(inside).enumerate() {
        if depth == MAX_DEPTH {
            return None;
        }
        if depth > 0 {
            out.push('/');
        }
        out.push_str(core::str::from_utf8(leaf.ok()?.as_bytes()).ok()?);
    }
    Some(out)
}

/// Whether `path` names a file inside an application, as TAIRiX spells it: at
/// most [`MAX_DEPTH`] plain names joined with `/`, none `.` or `..`. Every
/// path [`boot_sprite_files`] names is one, and a path a sandboxed scan answers
/// is believed only when it is.
#[must_use]
pub fn is_application_path(path: &str) -> bool {
    let mut depth = 0;
    path.split('/').all(|component| {
        depth += 1;
        depth <= MAX_DEPTH
            && component != "."
            && component != ".."
            && tairix_path::validate_file_name(component).is_ok()
    })
}

/// The whole of `document`, where it is no longer than `limit` and reads in
/// full.
fn read_whole(document: &mut dyn ArtworkDocument, limit: usize) -> Option<Vec<u8>> {
    let size = usize::try_from(document.stamp().size)
        .ok()
        .filter(|&size| size <= limit)?;
    let mut bytes = fallible::filled(size, 0u8)?;
    let mut at = 0;
    while at < size {
        let read = document.read_at(u64::try_from(at).ok()?, &mut bytes[at..])?;
        if read == 0 {
            return None;
        }
        at += read;
    }
    Some(bytes)
}

/// The picture of the RISC OS application at `dir`, at `side` pixels: its
/// large or small sprite, whichever suits the side, from the first sprite
/// file that holds one — those its `!Boot` loads, then its own `!Sprites` —
/// each tried at the finest resolution RISC OS has a copy of it for. `None`
/// where none does, so the request falls to the folder it is.
///
/// Every file is opened inside `dir` without following a link
/// ([`ArtworkReader::open_inside`]), and the `!Boot` is read in the sandbox
/// ([`ArtworkRasteriser::boot_sprite_files`]), each file it answers believed
/// only as a path inside the application.
pub(crate) fn application_picture<R, D>(
    reader: &mut R,
    rasteriser: &mut D,
    dir: &str,
    side: u32,
) -> Option<Artwork>
where
    R: ArtworkReader + ?Sized,
    D: ArtworkRasteriser + ?Sized,
{
    if side == 0 {
        return None;
    }
    // A sprite is named as RISC OS spells the application.
    let leaf = tairix_path::leaf_name(dir);
    let mut spelled = String::new();
    spelled.try_reserve_exact(leaf.len()).ok()?;
    spelled.extend(
        leaf.bytes()
            .map(|byte| char::from(acorn::crossed_name_byte(byte))),
    );
    let names = SpriteNames::of(&spelled)?;
    let mut files = reader
        .open_inside(dir, BOOT)
        .and_then(|mut boot| read_whole(&mut *boot, MAX_BOOT_BYTES))
        .and_then(|boot| rasteriser.boot_sprite_files(&boot))
        .unwrap_or_default();
    files.retain(|file| is_application_path(file));
    files.truncate(MAX_SPRITE_FILES);
    if !files.iter().any(|file| file.eq_ignore_ascii_case(SPRITES)) {
        files.try_reserve(1).ok()?;
        files.push(fallible::concatenated(&[SPRITES])?);
    }
    let mut spent = 0u64;
    for file in &files {
        for resolution in RESOLUTIONS {
            let inside = fallible::concatenated(&[file, resolution])?;
            let Some(mut document) = reader.open_inside(dir, &inside) else {
                continue;
            };
            let length = document.stamp().size;
            if length > MAX_APPLICATION_SPRITES_BYTES {
                continue;
            }
            spent = spent.saturating_add(length);
            if spent > MAX_APPLICATION_PICTURE_BYTES {
                return None;
            }
            let Some(fitted) = rasteriser.application_sprite(side, &names, &mut *document) else {
                continue;
            };
            // The pixels are believed only in exactly the shape asked for.
            let expected = (side as usize)
                .checked_mul(side as usize)
                .and_then(|area| area.checked_mul(4))?;
            if fitted.pixels.len() != expected {
                return None;
            }
            // An icon, not a thumbnail of a picture: its transparent margin
            // is the background's, so it is never framed.
            return Some(Artwork::new(Surface::from_rgba8(
                side,
                side,
                &fitted.pixels,
            )?));
        }
    }
    None
}

#[cfg(test)]
#[path = "riscos_tests.rs"]
mod tests;
