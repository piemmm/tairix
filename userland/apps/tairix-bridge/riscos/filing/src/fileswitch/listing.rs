//! The listings (`plans/RISCOS-EMULATOR.md` §6): `OS_FSControl` 5 to 9 and
//! 32 — `*Cat`, `*Ex`, `*LCat`, `*LEx`, `*Info` and `*FileInfo`. What they
//! read they read as RISC OS 5 does (`tests/corpus/filing-control.scenarios`):
//! a directory twice in batches, once for the widths and once to print, and
//! its disc's name and boot option by `FSEntry_Func` 0 and 11. The layout is
//! the PRM's: a header naming the disc, its boot option and the directories,
//! then each object's name and attributes, and for the long forms its type
//! and date-stamp, or addresses, and length.

use alloc::vec::Vec;

use tairix_riscos_kernel::filing::fs;
use tairix_riscos_kernel::filing::{IS_A_FILE, NOT_FOUND, NO_DIRECTORY};
use tairix_riscos_kernel::hex8;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::names::{CONVERT_DATE_AND_TIME, CONVERT_STANDARD_DATE_AND_TIME};
use tairix_riscos_kernel::swi::os::READ_MODE_VARIABLE;

use super::call::{
    self, attribute, Info, Named, Need, DIRECTORY, FILE as A_FILE, NOT_FOUND as NOTHING,
};
use super::directory::{batch, Entry, READS_MOST};
use super::name::{self, Dir, Resolved, Search};
use super::{image, types, with};

/// How much each object is given.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Detail {
    /// Its name and attributes, several to a line: `*Cat`.
    Names,
    /// Its information, a line each: `*Ex` and `*Info`.
    Information,
    /// Its full information, the date-stamp to the centisecond and the
    /// length in hexadecimal: `*FileInfo`.
    Full,
}

/// The columns of an object's line, as RISC OS 5 lays one out: its name,
/// its attributes, and its type and date-stamp or addresses, which for full
/// information has room for the centiseconds.
const NAME_COLUMN: usize = 13;
const ATTRIBUTES: usize = 8;
const TYPE_COLUMN: usize = 10;
const STAMP_FIELD: usize = 31;
const FULL_STAMP_FIELD: usize = 34;
/// The least a length shown in decimal takes.
const LENGTH_LEAST: usize = 4;
/// What a directory, or an image file, shows for its type.
const DIRECTORY_TYPE: &[u8] = b"Directory";
/// The columns of a message `*Opt 1` asks for.
const MESSAGE_NAME: usize = 12;
const MESSAGE_TYPE: usize = 9;
/// The widest a line of names is where the screen's width cannot be read.
const LINE_MOST: usize = 80;
/// The boot options' names.
const BOOT: [&[u8]; 4] = [b"Off", b"Load", b"Run", b"Exec"];
/// What a directory never set shows.
const UNSET: &[u8] = b"\"Unset\"";
/// A date-stamp to the centisecond, for `*FileInfo`.
const FULL_DATE: &[u8] = b"%24:%mi:%se.%cs %dy-%m3-%ce%yr\0";
/// The room a converted date-stamp is given.
const DATE_ROOM: u32 = 64;

/// `OS_FSControl` 5 to 8: the directory R1 names — within the library for
/// `library`, the current directory or the library itself for none —
/// listed with `detail`.
pub(crate) fn catalogue(
    os: &mut Os<'_>,
    regs: &Regs,
    library: bool,
    detail: Detail,
) -> Result<(), Fail> {
    let text = name::read(os, regs.r[1])?;
    let mut target = Vec::new();
    let base: &[u8] = if library { b"%" } else { b"@" };
    if target.try_reserve(base.len() + 1 + text.len()).is_err() {
        return Err(os.no_memory());
    }
    match (library, text.is_empty()) {
        (_, true) => target.extend_from_slice(base),
        (true, false) => {
            target.extend_from_slice(b"%.");
            target.extend_from_slice(&text);
        }
        (false, false) => target.extend_from_slice(&text),
    }
    let (mut resolved, found) = if library && text.is_empty() {
        let (resolved, found) = name::library(os)?;
        (
            resolved,
            found.unwrap_or(Info {
                kind: DIRECTORY,
                ..Info::default()
            }),
        )
    } else {
        name::find_named(os, &target, Search::Plain, Need::Kind)?
    };
    match found.kind {
        NOTHING => return Err(name::named_error(os, NO_DIRECTORY, &target)),
        A_FILE => return Err(name::named_error(os, IS_A_FILE, &target)),
        _ => {}
    }
    image::root(os, &mut resolved, &found)?;
    let (disc, boot) = disc_of(os, &resolved)?;
    let width = widest(os, &resolved, None)?;
    header(os, &disc, boot)?;
    let screen = screen_width(os);
    let mut line = Vec::new();
    each_batch(os, &resolved, |os, (leaf, info)| {
        if detail != Detail::Names {
            return entry_line(os, &leaf, &info, width, detail);
        }
        let mut cell = Vec::new();
        pad(&mut cell, &leaf, name_column(width));
        pad(&mut cell, &attributes(&info), ATTRIBUTES);
        if !line.is_empty() && line.len() + cell.len() > screen {
            os.write_line(&line)?;
            line.clear();
        }
        line.extend_from_slice(&cell);
        Ok(())
    })?;
    if !line.is_empty() {
        os.write_line(&line)?;
    }
    Ok(())
}

/// `OS_FSControl` 9 and 32: the object R1 names, or each its wildcarded
/// leaf matches, given its information; a leaf that matches nothing is not
/// found.
pub(crate) fn info(os: &mut Os<'_>, regs: &Regs, detail: Detail) -> Result<(), Fail> {
    let text = name::read(os, regs.r[1])?;
    let (resolved, found) = name::find_named(os, &text, Search::Plain, Need::Length)?;
    if found.kind == NOTHING {
        return Err(name::named_error(os, NOT_FOUND, &text));
    }
    let leaf = name::leaf(&text);
    if !name::wild(leaf) {
        let shown = name::leaf(&resolved.path).to_vec();
        return entry_line(os, &shown, &found, shown.len(), detail);
    }
    let pattern = leaf.to_vec();
    let mut directory = resolved;
    let end = directory.path.iter().rposition(|&b| b == b'.').unwrap_or(0);
    directory.path.truncate(end);
    let width = widest(os, &directory, Some(&pattern))?;
    each_batch(os, &directory, |os, (leaf, info)| {
        if name::matches(&pattern, &leaf) {
            entry_line(os, &leaf, &info, width, detail)?;
        }
        Ok(())
    })
}

/// Each entry of `directory`, read in batches, handed to `each`.
fn each_batch(
    os: &mut Os<'_>,
    directory: &Resolved,
    mut each: impl FnMut(&mut Os<'_>, Entry) -> Result<(), Fail>,
) -> Result<(), Fail> {
    let mut offset = 0;
    for _ in 0..READS_MOST {
        let (entries, next) = batch(os, directory, offset)?;
        for entry in entries {
            each(os, entry)?;
        }
        if next == u32::MAX || next == offset {
            break;
        }
        offset = next;
    }
    Ok(())
}

/// The longest name among `directory`'s entries `pattern` matches, read in
/// a first pass.
fn widest(os: &mut Os<'_>, directory: &Resolved, pattern: Option<&[u8]>) -> Result<usize, Fail> {
    let mut width = 0;
    each_batch(os, directory, |_, (leaf, _)| {
        if pattern.is_none_or(|p| name::matches(p, &leaf)) {
            width = width.max(leaf.len());
        }
        Ok(())
    })?;
    Ok(width)
}

/// The name and boot option of the disc `resolved` is on, as the header
/// gives them and RISC OS 5 asks for them: of an image filing system or its
/// host by `FSEntry_Func` 27 on the directory listed, the name its path's
/// disc; of any other by `FSEntry_Func` 0 and 11 on its disc's root.
fn disc_of(os: &mut Os<'_>, resolved: &Resolved) -> Result<(Vec<u8>, u8), Fail> {
    let named = resolved
        .path
        .strip_prefix(b":")
        .map(|p| p.split(|&b| b == b'.').next().unwrap_or(p).to_vec())
        .unwrap_or_default();
    if resolved.inside.is_some() || resolved.calls.has(super::system::info::IMAGES) {
        let mut regs = Regs::default();
        call::func(
            os,
            &resolved.calls,
            fs::func::READ_BOOT_OPTION,
            resolved.named(),
            0,
            &mut regs,
            |_, _, _| Ok(()),
        )?;
        return Ok((named, regs.byte(2)));
    }
    let root = name::disc_root_of(&resolved.path).to_vec();
    let (name, boot) = name::read_disc(os, &resolved.calls, Named::new(&root, resolved.special()))?;
    Ok((if name.is_empty() { named } else { name }, boot))
}

/// The header: the disc's name and boot option, then the current
/// directory, the library and the user root directory as they were set.
fn header(os: &mut Os<'_>, disc: &[u8], boot: u8) -> Result<(), Fail> {
    let [current, library, user_root] = with(os, |state| {
        let directories = state
            .current
            .and_then(|n| state.directories.iter().find(|(m, _)| *m == n))
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        [Dir::Current, Dir::Library, Dir::UserRoot]
            .map(|which| directories.get(which).map(|s| s.path.clone()))
    })?;
    let shown = |set: &Option<Vec<u8>>| set.clone().unwrap_or_else(|| UNSET.to_vec());
    let mut line = Vec::new();
    pad(&mut line, disc, 20);
    line.extend_from_slice(b"Option ");
    line.extend_from_slice(&two_digits(boot));
    line.extend_from_slice(b" (");
    line.extend_from_slice(BOOT[usize::from(boot & 3)]);
    line.push(b')');
    os.write_line(&line)?;
    line.clear();
    line.extend_from_slice(b"Dir. ");
    pad(&mut line, &shown(&current), 15);
    line.extend_from_slice(b"Lib. ");
    line.extend_from_slice(&shown(&library));
    os.write_line(&line)?;
    line.clear();
    line.extend_from_slice(b"URD  ");
    line.extend_from_slice(&shown(&user_root));
    os.write_line(&line)?;
    os.write_line(b"")
}

/// `value`'s two decimal digits.
const fn two_digits(value: u8) -> [u8; 2] {
    [b'0' + (value / 10) % 10, b'0' + value % 10]
}

/// `text` onto `line`, padded with spaces to `width`.
fn pad(line: &mut Vec<u8>, text: &[u8], width: usize) {
    line.extend_from_slice(text);
    line.resize(line.len() + width.saturating_sub(text.len()), b' ');
}

/// The width of a name column for names up to `longest` long: room for
/// the longest and a space.
fn name_column(longest: usize) -> usize {
    NAME_COLUMN.max(longest + 1)
}

/// Whether an object shows as a directory: an image file does.
const fn shown_as_directory(info: &Info) -> bool {
    info.kind == DIRECTORY || info.kind == call::IMAGE
}

/// An object's attributes as RISC OS shows them: `D` for a directory, `L`
/// locked, then the owner's `W` and `R`, a `/` and the public's.
fn attributes(info: &Info) -> Vec<u8> {
    let bits = info.attributes;
    let mut text = Vec::new();
    for (shown, letter) in [
        (shown_as_directory(info), b'D'),
        (bits & attribute::LOCKED != 0, b'L'),
        (bits & attribute::OWNER_WRITE != 0, b'W'),
        (bits & attribute::OWNER_READ != 0, b'R'),
    ] {
        if shown {
            text.push(letter);
        }
    }
    text.push(b'/');
    for (shown, letter) in [
        (bits & attribute::PUBLIC_WRITE != 0, b'W'),
        (bits & attribute::PUBLIC_READ != 0, b'R'),
    ] {
        if shown {
            text.push(letter);
        }
    }
    text
}

/// Whether an object's load address holds a type and a date-stamp.
const fn typed(info: &Info) -> bool {
    fs::stamp::type_of(info.load).is_some()
}

/// What an object's type shows as, in `width`: its type's name, or for a
/// directory `Directory`.
fn type_text(os: &mut Os<'_>, info: &Info, width: usize) -> Result<Vec<u8>, Fail> {
    let mut text = Vec::new();
    if shown_as_directory(info) {
        pad(&mut text, DIRECTORY_TYPE, width);
    } else {
        let name = types::name_of(os, (info.load & fs::stamp::TYPE) >> 8)?;
        let trimmed = name
            .iter()
            .rposition(|&b| b != b' ')
            .map_or(&name[..0], |end| &name[..=end]);
        pad(&mut text, trimmed, width);
    }
    Ok(text)
}

/// `info`'s addresses, load then execution, in hex.
fn addresses(info: &Info) -> Vec<u8> {
    let mut text = Vec::new();
    text.extend_from_slice(&hex8(info.load));
    text.push(b' ');
    text.extend_from_slice(&hex8(info.exec));
    text
}

/// One object's line: its name in a column `longest` sets, its attributes,
/// then in one field its type and date-stamp or, right-aligned, its
/// addresses, and its length — in hex for full information, else in
/// decimal and bytes.
fn entry_line(
    os: &mut Os<'_>,
    leaf: &[u8],
    info: &Info,
    longest: usize,
    detail: Detail,
) -> Result<(), Fail> {
    let full = detail == Detail::Full;
    let mut line = Vec::new();
    pad(&mut line, leaf, name_column(longest));
    pad(&mut line, &attributes(info), ATTRIBUTES);
    let field = if full { FULL_STAMP_FIELD } else { STAMP_FIELD };
    let mut stamp = Vec::new();
    if typed(info) {
        stamp.extend_from_slice(&type_text(os, info, TYPE_COLUMN)?);
        stamp.extend_from_slice(&date(os, info, full)?);
        stamp.push(b' ');
        pad(&mut line, &stamp, field);
    } else {
        stamp.extend_from_slice(&addresses(info));
        stamp.push(b' ');
        line.resize(line.len() + field.saturating_sub(stamp.len()), b' ');
        line.extend_from_slice(&stamp);
    }
    if full {
        line.extend_from_slice(&hex8(info.length));
    } else {
        let mut digits = Vec::new();
        decimal(&mut digits, info.length);
        line.resize(line.len() + LENGTH_LEAST.saturating_sub(digits.len()), b' ');
        line.extend_from_slice(&digits);
        line.extend_from_slice(b"  bytes");
    }
    os.write_line(&line)
}

/// The message the filing system `number`'s `*Opt 1` level asks a load,
/// save or creation of the object `leaf` names to give: its name; then its
/// addresses and length; or its type, date-stamp and length where it is
/// typed. RISC OS 5 names the object of a save or creation from a null
/// pointer; this names it.
pub(crate) fn announce(os: &mut Os<'_>, number: u8, leaf: &[u8], info: &Info) -> Result<(), Fail> {
    let level = with(os, |state| state.system(number).map_or(0, |s| s.messages))?;
    if level == 0 {
        return Ok(());
    }
    let mut line = Vec::new();
    if level == 1 {
        line.extend_from_slice(leaf);
        return os.write_line(&line);
    }
    pad(&mut line, leaf, MESSAGE_NAME.max(leaf.len() + 2));
    if level >= 3 && typed(info) {
        line.extend_from_slice(&type_text(os, info, MESSAGE_TYPE)?);
        line.extend_from_slice(&date(os, info, false)?);
    } else {
        line.extend_from_slice(&addresses(info));
    }
    line.push(b' ');
    line.extend_from_slice(&hex8(info.length));
    os.write_line(&line)
}

/// `value`'s decimal digits onto `out`.
pub(crate) fn decimal(out: &mut Vec<u8>, value: u32) {
    if value >= 10 {
        decimal(out, value / 10);
    }
    out.push(b'0' + (value % 10) as u8);
}

/// An object's date-stamp as text: `Sys$DateFormat`'s, or to the
/// centisecond for full information.
fn date(os: &mut Os<'_>, info: &Info, full: bool) -> Result<Vec<u8>, Fail> {
    let stamp = [
        info.exec.to_le_bytes(),
        [info.load.to_le_bytes()[0], 0, 0, 0],
    ];
    let format_room = len32(FULL_DATE.len()).next_multiple_of(4);
    os.below_supervisor_stack(8 + format_room + DATE_ROOM, |os, frame| {
        os.write_bytes(frame, &stamp[0])?;
        os.write_bytes(frame + 4, &stamp[1])?;
        os.write_bytes(frame + 8, FULL_DATE)?;
        let out = frame + 8 + format_room;
        let mut regs = Regs::default();
        regs.r[0] = frame;
        regs.r[1] = out;
        regs.r[2] = DATE_ROOM;
        let swi = if full {
            regs.r[3] = frame + 8;
            CONVERT_DATE_AND_TIME
        } else {
            CONVERT_STANDARD_DATE_AND_TIME
        };
        os.perform(tairix_riscos_kernel::swi::X | swi, &mut regs)?;
        Ok(os.read_until(out, DATE_ROOM as usize, |b| b == 0)?.0)
    })
}

/// The text window's width in characters, or a line's widest where it
/// cannot be read.
fn screen_width(os: &mut Os<'_>) -> usize {
    let mut regs = Regs::default();
    regs.r[0] = u32::MAX;
    regs.r[1] = tairix_vdu::mode::var::SCR_R_COL;
    match os.perform(tairix_riscos_kernel::swi::X | READ_MODE_VARIABLE, &mut regs) {
        Ok(()) if regs.r[2] > 0 => regs.r[2] as usize,
        _ => LINE_MOST,
    }
}

#[cfg(test)]
mod tests;
