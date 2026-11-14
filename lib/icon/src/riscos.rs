//! A RISC OS application pictured as RISC OS's Filer pictures it: by the sprite
//! of its own name that its `!Boot` loads with `IconSprites`, else the one its
//! own `!Sprites` holds.
//!
//! The Filer runs an application's `!Boot` on merely seeing it. Nothing here
//! runs one: the file is read as text and only its `IconSprites` commands are
//! taken from it, with the variables the lines before them set, so a browsed
//! volume costs reads and never code. A command whose file lies outside the
//! application, or depends on anything the file does not set, names nothing.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

use tairix_fsmeta::preset::acorn;
use tairix_image::SpriteName;
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

/// The longest command line, and the longest value, a scan reads: RISC OS's
/// command line holds 256 bytes with its terminator.
const MAX_LINE: usize = 255;

/// The deepest a sprite file may lie inside its application: the most
/// components a path [`boot_sprite_files`] names has.
pub const MAX_DEPTH: usize = 4;

/// How deep one macro may expand into another.
const MAX_EXPANSION: usize = 4;

/// The most variable references one scan expands, however its macros nest: a
/// bound on the work one file can ask for, which nesting alone is not.
const MAX_REFERENCES: usize = 1024;

/// The most bytes one application's picture streams to the sandbox, across
/// every sprite file it tries.
pub const MAX_APPLICATION_PICTURE_BYTES: u64 = 2 * MAX_APPLICATION_SPRITES_BYTES;

/// What `<Obey$Dir>` expands to: the application's own directory, marked by a
/// character no scanned line can hold, so a path starts there only when it
/// really does.
const APPLICATION: char = '\0';

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
/// The file is read, never run. Of its commands only `Set`, `SetMacro`,
/// `Unset`, an `If` comparing two quoted strings, and `IconSprites` are
/// understood, `<Obey$Dir>` standing for the application's directory; a path
/// variable (`App:`) is read as its `App$Path` says. A file longer than
/// [`MAX_BOOT_BYTES`] or holding a byte no command line can is not read at all.
#[must_use]
pub fn boot_sprite_files(boot: &[u8]) -> Vec<String> {
    if boot.len() > MAX_BOOT_BYTES || !is_text(boot) {
        return Vec::new();
    }
    let Some(mut scan) = Scan::new() else {
        return Vec::new();
    };
    for line in boot.split(|&byte| byte == b'\n') {
        if scan.files.len() == MAX_SPRITE_FILES {
            break;
        }
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        // A line RISC OS wrote in Latin-1 holds no command this reads.
        if let Ok(line) = core::str::from_utf8(line) {
            scan.line(line, true);
        }
    }
    scan.files
}

/// Whether every byte of `bytes` is one a command line holds.
fn is_text(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|&byte| (byte >= b' ' && byte != 0x7F) || matches!(byte, b'\t' | b'\n' | b'\r'))
}

/// What a scan of a `!Boot` has found so far.
struct Scan {
    variables: Vec<Variable>,
    files: Vec<String>,
    /// The variable references it may still expand.
    references: Cell<usize>,
}

/// A system variable a `!Boot` set.
struct Variable {
    /// Its name, lower-cased: RISC OS takes a variable's name without regard
    /// to case.
    name: String,
    value: String,
    /// Whether it was set with `SetMacro`, so it is expanded when read rather
    /// than when set.
    is_macro: bool,
}

impl Scan {
    /// A scan with room for all it keeps, so none of it allocates later;
    /// `None` where that room cannot be had.
    fn new() -> Option<Self> {
        let mut variables = Vec::new();
        variables.try_reserve_exact(MAX_VARIABLES).ok()?;
        let mut files = Vec::new();
        files.try_reserve_exact(MAX_SPRITE_FILES).ok()?;
        Some(Self {
            variables,
            files,
            references: Cell::new(MAX_REFERENCES),
        })
    }

    /// Take in one command `line`; one an `If` chose when `may_branch` is
    /// false, since RISC OS runs no `If` inside another's branch.
    fn line(&mut self, line: &str, may_branch: bool) {
        if line.len() > MAX_LINE {
            return;
        }
        let line = line.trim_start_matches([' ', '\t', '*']);
        let line = line.strip_prefix('%').unwrap_or(line);
        if line.starts_with('|') {
            return;
        }
        let (command, rest) = split_word(line);
        if command.eq_ignore_ascii_case("Set") {
            self.set(rest, false);
        } else if command.eq_ignore_ascii_case("SetMacro") {
            self.set(rest, true);
        } else if command.eq_ignore_ascii_case("Unset") {
            let name = split_word(rest).0;
            self.variables
                .retain(|variable| !variable.name.eq_ignore_ascii_case(name));
        } else if command.eq_ignore_ascii_case("IconSprites") {
            self.icon_sprites(split_word(rest).0);
        } else if may_branch && command.eq_ignore_ascii_case("If") {
            self.branch(rest);
        }
    }

    /// `Set` or, as `is_macro`, `SetMacro`: the variable `rest` names takes the
    /// value after it, expanded now unless it is a macro.
    fn set(&mut self, rest: &str, is_macro: bool) {
        let (name, value) = split_word(rest);
        if !is_variable_name(name) {
            return;
        }
        let value = if is_macro {
            fallible::concatenated(&[value])
        } else {
            self.expand(value, 0)
        };
        let (Some(value), Some(mut key)) = (value, fallible::concatenated(&[name])) else {
            return;
        };
        key.make_ascii_lowercase();
        if let Some(variable) = self
            .variables
            .iter_mut()
            .find(|variable| variable.name == key)
        {
            variable.value = value;
            variable.is_macro = is_macro;
        } else if self.variables.len() < MAX_VARIABLES {
            self.variables.push(Variable {
                name: key,
                value,
                is_macro,
            });
        }
    }

    /// `IconSprites` of `argument`: the sprite file it loads, where that lies
    /// inside the application and has not been found already.
    fn icon_sprites(&mut self, argument: &str) {
        let Some(path) = self.expand(argument, 0) else {
            return;
        };
        let Some((variable, rest)) = path.split_once(':') else {
            self.found(&path);
            return;
        };
        // A filing system's own name, `ADFS::Disc.$`, is no path variable.
        if variable.is_empty() || rest.starts_with(':') {
            return;
        }
        let Some(name) = fallible::concatenated(&[variable, "$Path"]) else {
            return;
        };
        let Some(prefixes) = self.value(&name, 0) else {
            return;
        };
        for prefix in prefixes.split(',') {
            if let Some(path) = fallible::concatenated(&[prefix, rest]) {
                self.found(&path);
            }
        }
    }

    /// Keep the RISC OS `path` as a sprite file, where it lies inside the
    /// application and is not kept already.
    fn found(&mut self, path: &str) {
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

    /// `If condition Then command [Else command]`: the command the condition
    /// chooses, where it is one this can judge.
    fn branch(&mut self, rest: &str) {
        let Some((condition, outcome)) = split_at_keyword(rest, "Then") else {
            return;
        };
        let (then, otherwise) = match split_at_keyword(outcome, "Else") {
            Some((then, otherwise)) => (then, Some(otherwise)),
            None => (outcome, None),
        };
        match (self.evaluate(condition), otherwise) {
            (Some(true), _) => self.line(then, false),
            (Some(false), Some(otherwise)) => self.line(otherwise, false),
            (Some(false), None) | (None, _) => {}
        }
    }

    /// A condition comparing two quoted strings, `"a"="b"` or `"a"<>"b"`, each
    /// expanded; `None` for any other, which this does not judge.
    fn evaluate(&self, condition: &str) -> Option<bool> {
        let (left, rest) = quoted(condition.trim_start_matches([' ', '\t']))?;
        let rest = rest.trim_start_matches([' ', '\t']);
        let (equal, rest) = match rest.strip_prefix("<>") {
            Some(rest) => (false, rest),
            None => (true, rest.strip_prefix('=')?),
        };
        let (right, rest) = quoted(rest.trim_start_matches([' ', '\t']))?;
        if !rest.trim_matches([' ', '\t']).is_empty() {
            return None;
        }
        let (left, right) = (self.expand(left, 0)?, self.expand(right, 0)?);
        Some((left == right) == equal)
    }

    /// `text` with each `<variable>` it names replaced by its value, and `|<`,
    /// `||` and `|"` by the character they escape, as RISC OS's `GSTrans`
    /// translates it; `None` where it holds an escape no path can, grows past
    /// [`MAX_LINE`], or nests macros too deep.
    fn expand(&self, text: &str, depth: usize) -> Option<String> {
        if depth > MAX_EXPANSION {
            return None;
        }
        let mut out = String::new();
        // Each step adds at most one character, so this room never grows.
        out.try_reserve_exact(MAX_LINE + 4).ok()?;
        let mut rest = text;
        while let Some(next) = rest.chars().next() {
            rest = &rest[next.len_utf8()..];
            match next {
                '|' => {
                    let escaped = rest.chars().next()?;
                    rest = &rest[escaped.len_utf8()..];
                    if !matches!(escaped, '<' | '|' | '"') {
                        return None;
                    }
                    out.push(escaped);
                }
                '<' => match rest.find('>') {
                    Some(end) if is_variable_name(&rest[..end]) => {
                        let value = self.value(&rest[..end], depth)?;
                        if out.len() + value.len() > MAX_LINE {
                            return None;
                        }
                        out.push_str(&value);
                        rest = &rest[end + 1..];
                    }
                    _ => out.push('<'),
                },
                other => out.push(other),
            }
            if out.len() > MAX_LINE {
                return None;
            }
        }
        Some(out)
    }

    /// The value of the variable called `name`: the application's directory
    /// for `Obey$Dir`, a macro's expanded, and nothing for one never set.
    fn value(&self, name: &str, depth: usize) -> Option<String> {
        let left = self.references.get().checked_sub(1)?;
        self.references.set(left);
        if name.eq_ignore_ascii_case("Obey$Dir") {
            let mut application = String::new();
            application.try_reserve_exact(1).ok()?;
            application.push(APPLICATION);
            return Some(application);
        }
        match self
            .variables
            .iter()
            .find(|variable| variable.name.eq_ignore_ascii_case(name))
        {
            Some(variable) if variable.is_macro => self.expand(&variable.value, depth + 1),
            Some(variable) => fallible::concatenated(&[&variable.value]),
            None => Some(String::new()),
        }
    }
}

/// `text`'s first word, and what follows the space after it.
fn split_word(text: &str) -> (&str, &str) {
    match text.find([' ', '\t']) {
        Some(at) => (&text[..at], text[at + 1..].trim_start_matches([' ', '\t'])),
        None => (text, ""),
    }
}

/// Whether `name` can name a variable: one to [`MAX_LINE`] printable ASCII
/// characters, none of them a bracket.
fn is_variable_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_LINE
        && name
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'<' && byte != b'>')
}

/// `text` split around the word `keyword`, matched without regard to case,
/// outside any quoted string.
fn split_at_keyword<'t>(text: &'t str, keyword: &str) -> Option<(&'t str, &'t str)> {
    let mut quoted = false;
    for (at, byte) in text.bytes().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b' ' | b'\t' if !quoted => {
                let after = &text[at + 1..];
                let (Some(word), Some(rest)) =
                    (after.get(..keyword.len()), after.get(keyword.len()..))
                else {
                    continue;
                };
                if word.eq_ignore_ascii_case(keyword)
                    && (rest.is_empty() || rest.starts_with([' ', '\t']))
                {
                    return Some((&text[..at], rest.trim_start_matches([' ', '\t'])));
                }
            }
            _ => {}
        }
    }
    None
}

/// The quoted string `text` opens with, and what follows it.
fn quoted(text: &str) -> Option<(&str, &str)> {
    let inner = text.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some((&inner[..end], &inner[end + 1..]))
}

/// The RISC OS `path`, expanded, as a path inside the application spelled as
/// TAIRiX spells it, or `None` where it starts anywhere but the application's
/// own directory, lies more than [`MAX_DEPTH`] deep, or holds a leaf no RISC
/// OS name can be.
///
/// Only an ASCII path is taken: a line read as UTF-8 says what RISC OS's
/// Latin-1 would only in ASCII.
fn within_application(path: &str) -> Option<String> {
    let inside = path.strip_prefix(APPLICATION)?.strip_prefix('.')?;
    if !inside.is_ascii() {
        return None;
    }
    let mut out = String::new();
    out.try_reserve_exact(inside.len()).ok()?;
    for (depth, leaf) in acorn::path_from_riscos(inside.as_bytes()).enumerate() {
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
