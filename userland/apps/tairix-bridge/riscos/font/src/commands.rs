//! The Font Manager's `*` commands and configuration keywords, in its
//! command table's order as RISC OS 5.30's: `*FontList` and `*FontCat`, the
//! font cache's saving and loading, the three that change `Font$Path`, and
//! the six `FontMax` keywords.

use alloc::vec::Vec;

use tairix_riscos_kernel::module::{Asked, Command, CommandKind, Help, Refused};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::{os as kos, X};
use tairix_riscos_kernel::Decimal;

use crate::cache;
use crate::handles::Source;
use crate::module::{error, message, raise, with};
use crate::scan;
use crate::settings::{self, FONT_MAX_CMOS, FONT_MAX_STORED, FONT_MAX_UNIT};
use crate::units;

const fn command(
    name: &'static str,
    (min, max): (u8, u8),
    help: &'static str,
    syntax: &'static str,
    run: tairix_riscos_kernel::module::Code,
) -> Command {
    Command {
        name,
        kind: CommandKind::Command,
        min,
        max,
        translate: 0,
        help: Help::Text(help),
        syntax: Some(syntax),
        run: Some(run),
    }
}

const fn configure(
    name: &'static str,
    help: &'static str,
    run: tairix_riscos_kernel::module::Code,
) -> Command {
    Command {
        name,
        kind: CommandKind::Configure,
        min: 0,
        max: 1,
        translate: 0,
        help: Help::Text(help),
        syntax: None,
        run: Some(run),
    }
}

/// The commands, in RISC OS 5.30's order.
pub(crate) static COMMANDS: [Command; 13] = [
    command("FontList", (0, 0), "*FontList lists the fonts the Font Manager holds, each with its size, resolution, uses and memory, then the font cache's size and what of it is free.", "Syntax: *FontList", font_list),
    command("FontCat", (0, 0), "*FontCat lists every font Font$Path reaches.", "Syntax: *FontCat", font_cat),
    command("LoadFontCache", (1, 1), "*LoadFontCache gives the Font Manager back the fonts *SaveFontCache saved, while it holds none in use.", "Syntax: *LoadFontCache <filename>", load_font_cache),
    command("SaveFontCache", (1, 1), "*SaveFontCache saves the fonts the Font Manager holds to a file.", "Syntax: *SaveFontCache <filename>", save_font_cache),
    command("FontInstall", (0, 1), "*FontInstall puts a font directory at the front of Font$Path, or with none given reads every directory on it afresh.", "Syntax: *FontInstall [<prefix>]", font_install),
    command("FontLibrary", (1, 1), "*FontLibrary makes a directory the font library: Font$Prefix, which heads Font$Path.", "Syntax: *FontLibrary <directory>", font_library),
    command("FontRemove", (1, 1), "*FontRemove takes a font directory off Font$Path.", "Syntax: *FontRemove <prefix>", font_remove),
    configure("FontMax1", "*Configure FontMax1 sets the largest size in points scaled from a bitmap font rather than from outlines.", configure_font_max_1),
    configure("FontMax2", "*Configure FontMax2 sets the largest size in points scaled from outlines to anti-aliased bitmaps.", configure_font_max_2),
    configure("FontMax3", "*Configure FontMax3 sets the largest size in points whose bitmaps are kept in the cache.", configure_font_max_3),
    configure("FontMax4", "*Configure FontMax4 sets the largest width in points anti-aliased at quarter pixels across.", configure_font_max_4),
    configure("FontMax5", "*Configure FontMax5 sets the largest height in points anti-aliased at quarter pixels up.", configure_font_max_5),
    configure("FontMax", "*Configure FontMax sets the most memory the font cache should take, in 64K units or kilobytes.", configure_font_max),
];

/// `Service_FontsChanged`, issued once a directory is read.
const FONTS_CHANGED: u32 = 0x6E;
/// The longest command tail read.
const TAIL_MOST: usize = 256;
/// `Font$Path` and `Font$Prefix`, and the element `*FontLibrary` heads the
/// path with.
const PATH: &[u8] = b"Font$Path";
const PREFIX: &[u8] = b"Font$Prefix";
const LIBRARY: &[u8] = b"<Font$Prefix>.";

/// `*FontCat`: the identifier of each font the path reaches, in the order
/// of their shown names.
fn font_cat(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let identifiers: Vec<Vec<u8>> = scan::with_catalogue(os, false, |_, catalogue| {
        Ok(catalogue
            .fonts
            .iter()
            .map(|f| f.identifier.clone())
            .collect())
    })?;
    for identifier in identifiers {
        os.write_line(&identifier)?;
    }
    Ok(())
}

/// The columns `*FontList` lays its lines out by: where the name, size,
/// resolution, uses and memory start, a column already passed adding no
/// space.
const NAME_COLUMN: usize = 5;
const SIZE_COLUMN: usize = 28;
const RESOLUTION_COLUMN: usize = 49;
const USES_COLUMN: usize = 59;
const USES_WIDTH: usize = 4;
const MEMORY_COLUMN: usize = 64;
const MEMORY_WIDTH: usize = 6;
/// The fractions of a point the sizes are shown to: quarters, in Latin-1.
const QUARTERS: [&[u8]; 4] = [b"", b"\xBC", b"\xBD", b"\xBE"];

/// `*FontList`: each handle in use with its font, then the cache's size and
/// what of it is free.
fn font_list(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let lines = with(os, |state| {
        state
            .handles
            .slots()
            .filter_map(|(handle, _)| list_line(state, handle))
            .collect::<Vec<_>>()
    })?;
    if !lines.is_empty() {
        let master_face = message(os, "MasterFace")?;
        let master_files = message(os, "MasterFiles")?;
        for token in ["ListHeading", "ListRule"] {
            let line = message(os, token)?;
            os.write_line(&line)?;
        }
        for line in lines {
            os.write_line(&line.text(&master_face, &master_files))?;
        }
        os.write_line(b"")?;
    }
    let (size, used) = settings::cache_size(os)?;
    let kilobytes = |bytes: u32| Decimal::of((bytes / 1024).cast_signed());
    for (token, value) in [
        ("CacheSize", size),
        ("CacheFree", size.saturating_sub(used)),
    ] {
        let mut line = message(os, token)?;
        pad_number(&mut line, kilobytes(value).as_bytes(), 4);
        line.extend_from_slice(b" Kbytes");
        os.write_line(&line)?;
    }
    Ok(())
}

/// One of `*FontList`'s lines, before the masters' words are known.
struct ListLine {
    handle: u8,
    identifier: Vec<u8>,
    size: Option<Vec<u8>>,
    resolution: Option<Vec<u8>>,
    /// Whether it is a master of a TAIRiX face.
    face: bool,
    uses: u32,
    memory: u32,
}

impl ListLine {
    fn text(self, master_face: &[u8], master_files: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        pad_number(&mut out, Decimal::of(i32::from(self.handle)).as_bytes(), 3);
        out.extend_from_slice(b". ");
        to_column(&mut out, NAME_COLUMN);
        out.extend_from_slice(&self.identifier);
        to_column(&mut out, SIZE_COLUMN);
        match &self.size {
            Some(size) => out.extend_from_slice(size),
            None => out.extend_from_slice(if self.face { master_face } else { master_files }),
        }
        to_column(&mut out, RESOLUTION_COLUMN);
        if let Some(resolution) = &self.resolution {
            out.extend_from_slice(resolution);
        }
        to_column(&mut out, USES_COLUMN);
        pad_number(
            &mut out,
            Decimal::of(self.uses.cast_signed()).as_bytes(),
            USES_WIDTH,
        );
        to_column(&mut out, MEMORY_COLUMN);
        pad_number(
            &mut out,
            Decimal::of(self.memory.cast_signed()).as_bytes(),
            MEMORY_WIDTH,
        );
        out.extend_from_slice(b" bytes");
        out
    }
}

/// Handle `handle`'s line; none for a free handle.
fn list_line(state: &crate::module::State, handle: u8) -> Option<ListLine> {
    let held = state.handles.held(u32::from(handle))?;
    Some(ListLine {
        handle,
        identifier: held.master.identifier.clone(),
        size: held.size.map(|s| points_text(s.points)),
        resolution: held.size.map(|s| {
            let mut text = unsigned(s.resolution.0.cast_unsigned());
            text.push(b'x');
            text.extend_from_slice(&unsigned(s.resolution.1.cast_unsigned()));
            text
        }),
        face: matches!(held.master.source, Source::Face),
        uses: held.uses,
        memory: cache::bytes_of(state, handle),
    })
}

/// `n` in unsigned decimal.
fn unsigned(n: u32) -> Vec<u8> {
    let mut buffer = [0; 20];
    tairix_util::fmt::format_u64(u64::from(n), &mut buffer)
        .as_bytes()
        .to_vec()
}

/// `points`, in sixteenths across and up, as `*FontList` shows them: whole
/// points, then the quarter they reach and a plus where they are past it,
/// and both where they differ.
fn points_text((x, y): (i32, i32)) -> Vec<u8> {
    let one = |sixteenths: i32| {
        let (sixteenths, a_point) = (
            sixteenths.cast_unsigned(),
            units::SIXTEENTHS.cast_unsigned(),
        );
        let mut text = unsigned(sixteenths / a_point);
        text.extend_from_slice(QUARTERS[(sixteenths % a_point / 4) as usize]);
        if !sixteenths.is_multiple_of(4) {
            text.push(b'+');
        }
        text
    };
    let mut out = one(x);
    if x != y {
        out.push(b'x');
        out.extend_from_slice(&one(y));
    }
    out.extend_from_slice(b" point");
    out
}

/// Spaces to column `column`, none where it is passed.
fn to_column(out: &mut Vec<u8>, column: usize) {
    while out.len() < column {
        out.push(b' ');
    }
}

/// `digits` right-aligned in `width`.
fn pad_number(out: &mut Vec<u8>, digits: &[u8], width: usize) {
    for _ in digits.len()..width {
        out.push(b' ');
    }
    out.extend_from_slice(digits);
}

/// The first word of the command tail at `at`.
fn first_word(os: &mut Os<'_>, at: u32) -> Result<Vec<u8>, Fail> {
    let (_, start) = os.skip_spaces(at)?;
    Ok(os.read_until(start, TAIL_MOST, |b| b <= b' ')?.0)
}

/// `Font$Path` as written, its elements apart.
fn path_elements(os: &mut Os<'_>) -> Result<Vec<Vec<u8>>, Fail> {
    let path = scan::read_variable(os, PATH)?.unwrap_or_default();
    Ok(path
        .split(|&b| b == b',')
        .filter(|e| !e.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

/// `Font$Path` made of `elements`, as a macro.
fn set_path(os: &mut Os<'_>, elements: &[Vec<u8>]) -> Result<(), Fail> {
    let path = elements.join(&b',');
    scan::set_macro(os, PATH, &path)
}

/// Every directory on the path read afresh, and the change announced.
fn rescan(os: &mut Os<'_>) -> Result<(), Fail> {
    scan::with_catalogue(os, true, |_, _| Ok(()))?;
    let mut regs = Regs::default();
    regs.r[1] = FONTS_CHANGED;
    os.perform(X | kos::SERVICE_CALL, &mut regs)
}

/// `*FontInstall`: the prefix given put at the path's front, once; with
/// none, the path read afresh.
fn font_install(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[1] != 0 {
        let prefix = first_word(os, regs.r[0])?;
        if !matches!(prefix.last(), Some(b'.' | b':')) {
            return Err(raise(os, error::NOT_PATH, &[]));
        }
        let mut elements = path_elements(os)?;
        elements.retain(|e| !e.eq_ignore_ascii_case(&prefix));
        elements.insert(0, prefix);
        set_path(os, &elements)?;
    }
    rescan(os)
}

/// `*FontRemove`: the prefix given taken off the path.
fn font_remove(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let prefix = first_word(os, regs.r[0])?;
    let mut elements = path_elements(os)?;
    elements.retain(|e| !e.eq_ignore_ascii_case(&prefix));
    set_path(os, &elements)?;
    rescan(os)
}

/// `*FontLibrary`: `Font$Prefix` set to the tail as given, and
/// `<Font$Prefix>.` put at the path's front.
fn font_library(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (_, start) = os.skip_spaces(regs.r[0])?;
    let directory = os.read_until(start, TAIL_MOST, |b| b < b' ')?.0;
    os.set_string_variable(PREFIX, &directory)?;
    let mut elements = path_elements(os)?;
    elements.retain(|e| !e.eq_ignore_ascii_case(LIBRARY));
    elements.insert(0, LIBRARY.to_vec());
    set_path(os, &elements)?;
    rescan(os)
}

/// `*SaveFontCache`.
fn save_font_cache(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let name = first_word(os, regs.r[0])?;
    cache::save(os, &name)
}

/// `*LoadFontCache`.
fn load_font_cache(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let name = first_word(os, regs.r[0])?;
    cache::load(os, &name)
}

/// The value of a configuration keyword: a number, which ends the line.
fn configured_number(os: &mut Os<'_>, at: u32) -> Result<(u32, Vec<u8>), Fail> {
    let (_, start) = os.skip_spaces(at)?;
    let (word, _) = os.read_until(start, TAIL_MOST, |b| b <= b' ')?;
    let digits = word.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return Err(Refused::NumberNeeded.fail());
    }
    let end = start.wrapping_add(tairix_riscos_kernel::os::len32(word.len()));
    if os.skip_spaces(end)?.0 > b' ' {
        return Err(Refused::TooMany.fail());
    }
    let value = word[..digits]
        .iter()
        .try_fold(0u32, |n, &d| {
            n.checked_mul(10)?.checked_add(u32::from(d - b'0'))
        })
        .unwrap_or(u32::MAX);
    Ok((value, word[digits..].to_vec()))
}

/// `FontMax1` to `FontMax5`: `index` 1 to 5 among the `FontMax` values.
fn configure_points(os: &mut Os<'_>, regs: &mut Regs, index: usize) -> Result<(), Fail> {
    let keyword = COMMANDS[6 + index].name.as_bytes();
    let location = FONT_MAX_CMOS + u8::try_from(index).unwrap_or(0);
    let stored = FONT_MAX_STORED[index];
    match Asked::of(regs.r[0]) {
        Asked::Syntax => {
            let mut line = keyword.to_vec();
            line.extend_from_slice(b"   <D>");
            os.write_line(&line)
        }
        Asked::Status => {
            let points = os.read_cmos(location)? ^ stored;
            let mut value = Decimal::of(i32::from(points)).as_bytes().to_vec();
            value.extend_from_slice(b" points");
            os.write_status(keyword, &value, b"\n\r")
        }
        Asked::Set(at) => {
            let (points, suffix) = configured_number(os, at)?;
            if !suffix.is_empty() {
                return Err(Refused::NumberNeeded.fail());
            }
            let Ok(points) = u8::try_from(points) else {
                return Err(Refused::TooBig.fail());
            };
            os.write_cmos(location, points ^ stored)?;
            with(os, |state| {
                state.settings.font_max[index] = u32::from(points);
            })
        }
    }
}

fn configure_font_max_1(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    configure_points(os, regs, 1)
}

fn configure_font_max_2(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    configure_points(os, regs, 2)
}

fn configure_font_max_3(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    configure_points(os, regs, 3)
}

fn configure_font_max_4(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    configure_points(os, regs, 4)
}

fn configure_font_max_5(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    configure_points(os, regs, 5)
}

/// `FontMax`: in 64K units, or in kilobytes rounded down to them.
fn configure_font_max(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    const KEYWORD: &[u8] = b"FontMax";
    const KILOBYTES_A_UNIT: u32 = FONT_MAX_UNIT / 1024;
    match Asked::of(regs.r[0]) {
        Asked::Syntax => os.write_line(b"FontMax    <D>[K]"),
        Asked::Status => {
            let units = os.read_cmos(FONT_MAX_CMOS)?;
            let mut value = Decimal::of(i32::from(units) * KILOBYTES_A_UNIT.cast_signed())
                .as_bytes()
                .to_vec();
            value.push(b'k');
            os.write_status(KEYWORD, &value, b"\n\r")
        }
        Asked::Set(at) => {
            let (number, suffix) = configured_number(os, at)?;
            let units = match suffix.as_slice() {
                [] => number,
                [b'K' | b'k'] => number / KILOBYTES_A_UNIT,
                _ => return Err(Refused::BadOption.fail()),
            };
            let Ok(units) = u8::try_from(units) else {
                return Err(Refused::TooBig.fail());
            };
            os.write_cmos(FONT_MAX_CMOS, units)?;
            with(os, |state| {
                state.settings.font_max[0] = u32::from(units) * FONT_MAX_UNIT;
            })
        }
    }
}

#[cfg(test)]
mod tests;
