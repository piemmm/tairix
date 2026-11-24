//! What the Font Manager reads through FileSwitch and the system
//! variables: `Font$Path` and the catalogue built from it, each element's
//! Messages file or its tree, and whether a font's files are there.
//!
//! A catalogue is bounded — in the path's elements, in the entries it reads
//! from directories and Messages files alike, in a tree's depth and in an
//! identifier's length — so a hostile path costs no more than those bounds,
//! and what lies past them is not listed.

use alloc::vec::Vec;

use tairix_riscos_kernel::filing::fs::kind::{DIRECTORY, FILE};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::file::{CREATE_DIRECTORY, DELETE, LOAD, READ_INFO, SAVE_TYPED};
use tairix_riscos_kernel::swi::os::{fs_control, gbpb};
use tairix_riscos_kernel::swi::{chunk, os as kos, X};

use crate::bitmaps;
use crate::catalogue::{self, Catalogue, Element, Encoding, Font, Holding, ENCODINGS};
use crate::module::with;
use crate::outlines::Depth;

/// The file a symbol font's own encoding is in.
pub(crate) const PRIVATE_ENCODING: &[u8] = b"Encoding";
/// What every metrics file's name starts with: `IntMetrics`, `IntMetric0`,
/// or one shortened to ten characters before its number.
const METRICS: &[u8] = b"IntMet";
/// A Messages file's leaf, before its territory's number.
const MESSAGES: &[u8] = b"Messages";
/// ResourceFS's font directory, canonical.
const ROM_FONTS: &[u8] = b"Resources:$.Fonts";

/// The most elements of `Font$Path` a catalogue reads.
const PATH_ELEMENTS_MOST: usize = 64;
/// The deepest a tree is walked below its element.
const DEPTH_MOST: usize = 16;
/// The most entries one catalogue reads: a directory's entries, and the
/// fonts and encodings a Messages file lists.
const ENTRIES_MOST: usize = 16_384;
/// The longest identifier listed.
const IDENTIFIER_MOST: usize = 255;
/// The largest Messages file read whole; one larger is read this far.
const MESSAGES_MOST: u32 = 64 * 1024;
/// The room each read of a directory's entries is made into.
const ENTRIES_ROOM: u32 = 1024;
/// Where an `OS_GBPB` 10 record holds its object's type and its name, and
/// the least room a record takes.
const KIND_AT: u32 = 16;
const NAME_AT: u32 = 20;
const RECORD_LEAST: u32 = 24;
/// The longest name a variable or canonical name is read to.
const NAME_ROOM: u32 = 1024;

/// `OS_SetVarVal`'s and `OS_ReadVarVal`'s types.
const STRING: u32 = 0;
const MACRO: u32 = 2;
/// `OS_Byte`'s alphabet reason, and its argument that reads it.
const ALPHABET: u32 = 71;
const READ_ALPHABET: u32 = 127;
/// `Service_International` and its reason for an alphabet's name.
const SERVICE_INTERNATIONAL: u32 = 0x43;
const ALPHABET_NAME: u32 = 3;
/// `Territory_Number`.
const TERRITORY_NUMBER: u32 = chunk::TERRITORY;

/// `work` done with the catalogue of `Font$Path` as it reads now: the one
/// kept where the path has not changed and `rescan` is not asked, else one
/// built afresh, which is kept for the next call.
pub(crate) fn with_catalogue<T>(
    os: &mut Os<'_>,
    rescan: bool,
    work: impl FnOnce(&mut Os<'_>, &Catalogue) -> Result<T, Fail>,
) -> Result<T, Fail> {
    let path = os.variable_text(b"Font$Path")?.unwrap_or_default();
    let kept = with(os, |state| state.catalogue.take())?;
    let catalogue = match kept {
        Some(kept) if !rescan && kept.path == path => kept,
        _ => build(os, path)?,
    };
    let done = work(os, &catalogue);
    with(os, |state| state.catalogue = Some(catalogue))?;
    done
}

/// The catalogue of `path`.
fn build(os: &mut Os<'_>, path: Vec<u8>) -> Result<Catalogue, Fail> {
    let territory = territory(os)?;
    let mut elements = Vec::new();
    let mut holdings = Vec::new();
    let mut budget = ENTRIES_MOST;
    let texts = path.split(|&b| b == b',').filter(|t| !t.is_empty());
    for text in texts.take(PATH_ELEMENTS_MOST) {
        let index = elements.len();
        let rom = is_rom_fonts(os, text)?;
        let holding = match messages(os, text, territory)? {
            Some(file) => catalogue::from_messages(&file, index, &mut budget),
            None => walk(os, text, index, &mut budget)?,
        };
        elements.push(Element {
            text: text.to_vec(),
            rom,
        });
        holdings.push(holding);
    }
    Ok(Catalogue::of(path, elements, holdings))
}

/// Whether the element `text` is ResourceFS's font directory.
fn is_rom_fonts(os: &mut Os<'_>, text: &[u8]) -> Result<bool, Fail> {
    let directory = text.strip_suffix(b".").unwrap_or(text);
    let canonical = os.with_text(directory, 0, |os, name| {
        os.below_supervisor_stack(NAME_ROOM, |os, buffer| {
            let mut regs = Regs::default();
            regs.r[..6].copy_from_slice(&[fs_control::CANONICALISE, name, buffer, 0, 0, NAME_ROOM]);
            match os.perform(X | kos::FS_CONTROL, &mut regs) {
                Ok(()) => Ok(Some(os.read_zstring(buffer, NAME_ROOM as usize)?)),
                Err(Fail::Error(_)) => Ok(None),
                Err(other) => Err(other),
            }
        })
    })?;
    Ok(canonical.is_some_and(|name| name.eq_ignore_ascii_case(ROM_FONTS)))
}

/// The element `text`'s Messages file for `territory`, else its plain one.
fn messages(os: &mut Os<'_>, text: &[u8], territory: u32) -> Result<Option<Vec<u8>>, Fail> {
    let territory = tairix_riscos_kernel::Decimal::of(territory.cast_signed());
    for suffix in [territory.as_bytes(), b""] {
        let mut name = text.to_vec();
        name.extend_from_slice(MESSAGES);
        name.extend_from_slice(suffix);
        if let Some(file) = load(os, &name, MESSAGES_MOST)? {
            return Ok(Some(file));
        }
    }
    Ok(None)
}

/// The file `name`, read to `most` bytes; none where no file is called that.
pub(crate) fn load(os: &mut Os<'_>, name: &[u8], most: u32) -> Result<Option<Vec<u8>>, Fail> {
    let Some((kind, length)) = info(os, name)? else {
        return Ok(None);
    };
    if kind != FILE {
        return Ok(None);
    }
    let block = os.rma_claim(length.max(4))?;
    let loaded = os.with_text(name, 0, |os, at| {
        let mut regs = Regs::default();
        regs.r[..4].copy_from_slice(&[LOAD, at, block, 0]);
        os.perform(X | kos::FILE, &mut regs)?;
        os.read_vec(block, length.min(most) as usize)
    });
    os.free_rma_quietly(block);
    loaded.map(Some)
}

/// What `name` is and its length; none where nothing is called that, or
/// its name cannot be.
fn info(os: &mut Os<'_>, name: &[u8]) -> Result<Option<(u32, u32)>, Fail> {
    os.with_text(name, 0, |os, at| {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[READ_INFO, at]);
        match os.perform(X | kos::FILE, &mut regs) {
            Ok(()) if regs.r[0] == 0 => Ok(None),
            Ok(()) => Ok(Some((regs.r[0], regs.r[4]))),
            Err(Fail::Error(_)) => Ok(None),
            Err(other) => Err(other),
        }
    })
}

/// Whether `name` is a file.
pub(crate) fn is_file(os: &mut Os<'_>, name: &[u8]) -> Result<bool, Fail> {
    Ok(info(os, name)?.is_some_and(|(kind, _)| kind == FILE))
}

/// `bytes` saved as file `name` of type `file_type`, from a block of the
/// RMA held for the call.
pub(crate) fn save(os: &mut Os<'_>, name: &[u8], file_type: u32, bytes: &[u8]) -> Result<(), Fail> {
    let block = os.rma_claim_with(bytes)?;
    let saved = os.with_text(name, 0, |os, at| {
        let mut regs = Regs::default();
        let end = block.wrapping_add(len32(bytes.len()));
        regs.r[..6].copy_from_slice(&[SAVE_TYPED, at, file_type, 0, block, end]);
        os.perform(X | kos::FILE, &mut regs)
    });
    os.free_rma_quietly(block);
    saved
}

/// Object `name` deleted, nothing where there is none.
pub(crate) fn delete(os: &mut Os<'_>, name: &[u8]) -> Result<(), Fail> {
    file_call(os, DELETE, name)
}

/// Directory `name` made, where it is not already.
pub(crate) fn create_directory(os: &mut Os<'_>, name: &[u8]) -> Result<(), Fail> {
    file_call(os, CREATE_DIRECTORY, name)
}

/// `OS_File` `reason` on the object `name`.
fn file_call(os: &mut Os<'_>, reason: u32, name: &[u8]) -> Result<(), Fail> {
    os.with_text(name, 0, |os, at| {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[reason, at]);
        os.perform(X | kos::FILE, &mut regs)
    })
}

/// A directory's entry: its name and object type.
struct Entry {
    name: Vec<u8>,
    kind: u32,
}

/// Whether a directory that cannot be read is an error or holds nothing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unreadable {
    Fails,
    Empty,
}

/// The entries of directory `directory`, at most `budget` more of them.
fn entries(os: &mut Os<'_>, directory: &[u8], budget: &mut usize) -> Result<Vec<Entry>, Fail> {
    listing(os, directory, budget, Unreadable::Empty)
}

/// The entries of directory `directory`, at most `budget` more of them,
/// where it cannot be read `unreadable` says. A read that finds nothing ends
/// the listing, since short of the end it means the next name does not fit
/// the room; so does one claiming a record it did not complete.
fn listing(
    os: &mut Os<'_>,
    directory: &[u8],
    budget: &mut usize,
    unreadable: Unreadable,
) -> Result<Vec<Entry>, Fail> {
    let mut found = Vec::new();
    let mut offset = 0u32;
    while *budget > 0 {
        let asked = len32(*budget).min(ENTRIES_ROOM / RECORD_LEAST);
        let (read, next) = os.with_text(directory, 0, |os, name| {
            os.below_supervisor_stack(ENTRIES_ROOM, |os, buffer| {
                let mut regs = Regs::default();
                regs.r[..7].copy_from_slice(&[
                    gbpb::READ_ENTRIES,
                    name,
                    buffer,
                    asked,
                    offset,
                    ENTRIES_ROOM,
                    0,
                ]);
                match os.perform(X | kos::GBPB, &mut regs) {
                    Ok(()) => {}
                    Err(Fail::Error(_)) if unreadable == Unreadable::Empty => {
                        return Ok((Vec::new(), u32::MAX));
                    }
                    Err(other) => return Err(other),
                }
                let count = regs.r[3].min(asked);
                let read = records(os, buffer, count)?;
                let whole = read.len() == count as usize;
                Ok((read, if whole { regs.r[4] } else { u32::MAX }))
            })
        })?;
        *budget = budget.saturating_sub(read.len());
        let finished = read.is_empty() || next == u32::MAX;
        found.extend(read);
        if finished {
            break;
        }
        offset = next;
    }
    Ok(found)
}

/// The first `count` records of an `OS_GBPB` 10 read into `buffer`, up to
/// the first that does not lie wholly within its room.
fn records(os: &mut Os<'_>, buffer: u32, count: u32) -> Result<Vec<Entry>, Fail> {
    let end = buffer + ENTRIES_ROOM;
    let mut read = Vec::new();
    let mut at = buffer;
    for _ in 0..count {
        let name_at = at + NAME_AT;
        if name_at >= end {
            break;
        }
        let (name, ended) = os.read_until(name_at, (end - name_at - 1) as usize, |b| b == 0)?;
        if !ended {
            break;
        }
        let kind = os.read_word(at + KIND_AT)?;
        at = (name_at + len32(name.len()) + 1).next_multiple_of(4);
        read.push(Entry { name, kind });
    }
    Ok(read)
}

/// What a scan of element `text` finds: each directory holding a metrics
/// file is a font, and its `Encodings` directory holds its encodings.
fn walk(os: &mut Os<'_>, text: &[u8], index: usize, budget: &mut usize) -> Result<Holding, Fail> {
    let mut holding = Holding::default();
    let root = text.strip_suffix(b".").unwrap_or(text);
    let mut pending = alloc::vec![(root.to_vec(), Vec::new(), 0usize)];
    while let Some((directory, identifier, depth)) = pending.pop() {
        let entries = entries(os, &directory, budget)?;
        if let Some(metrics) = first_metrics(&entries).filter(|_| !identifier.is_empty()) {
            holding.fonts.push(Font {
                identifier: identifier.clone(),
                name: None,
                default: false,
                symbol: number_of(metrics).is_empty(),
                element: index,
            });
        }
        for entry in entries.iter().rev().filter(|entry| entry.kind == DIRECTORY) {
            let mut path = directory.clone();
            path.push(b'.');
            path.extend_from_slice(&entry.name);
            if depth == 0 && entry.name.eq_ignore_ascii_case(ENCODINGS) {
                for encoding in entries_of(os, &path, budget)? {
                    let base = encoding.name.first() == Some(&b'/');
                    holding.encodings.push(Encoding {
                        identifier: encoding.name[usize::from(base)..].to_vec(),
                        name: None,
                        base,
                        element: index,
                    });
                }
                continue;
            }
            let mut deeper = identifier.clone();
            if !deeper.is_empty() {
                deeper.push(b'.');
            }
            deeper.extend_from_slice(&entry.name);
            if depth < DEPTH_MOST && deeper.len() <= IDENTIFIER_MOST {
                pending.push((path, deeper, depth + 1));
            }
        }
    }
    Ok(holding)
}

/// The files of directory `path`.
fn entries_of(os: &mut Os<'_>, path: &[u8], budget: &mut usize) -> Result<Vec<Entry>, Fail> {
    let mut files = entries(os, path, budget)?;
    files.retain(|entry| entry.kind == FILE);
    Ok(files)
}

/// The leaf a font's outlines file's name starts with, and the longest leaf
/// a font's data file has.
const OUTLINES: &[u8] = b"Outlines";
const LEAF_MOST: usize = 10;
/// The old format's bitmaps, which stand in for a font's outlines.
pub(crate) const OLD_BITMAPS: &[u8] = b"x90y45";

/// A guest font's data files, as the UCS Font Manager finds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DataFiles {
    /// Its metrics file and its outlines file, by their full names.
    pub(crate) metrics: Vec<u8>,
    pub(crate) outlines: Vec<u8>,
    /// The number its files' names end in, which numbers its base encoding;
    /// none for a symbol font.
    pub(crate) base: Option<Vec<u8>>,
}

/// The data files of the font whose directory is `prefix`: the metrics
/// file whose name comes first, and the outlines file its name's number
/// names; none where it has no metrics file, and where there is no
/// directory to read, FileSwitch's error or none as `unreadable` says.
pub(crate) fn data_files(
    os: &mut Os<'_>,
    prefix: &[u8],
    unreadable: Unreadable,
) -> Result<Option<DataFiles>, Fail> {
    let directory = prefix.strip_suffix(b".").unwrap_or(prefix);
    let mut budget = ENTRIES_MOST;
    let entries = listing(os, directory, &mut budget, unreadable)?;
    let Some(metrics) = first_metrics(&entries) else {
        return Ok(None);
    };
    let number = number_of(metrics);
    let mut leaf = OUTLINES[..OUTLINES.len().min(LEAF_MOST.saturating_sub(number.len()))].to_vec();
    leaf.extend_from_slice(number);
    let file = |leaf: &[u8]| {
        let mut name = prefix.to_vec();
        name.extend_from_slice(leaf);
        name
    };
    Ok(Some(DataFiles {
        metrics: file(metrics),
        outlines: file(&leaf),
        base: (!number.is_empty()).then(|| number.to_vec()),
    }))
}

/// The name of the metrics file among `entries` that comes first, without
/// regard to case.
fn first_metrics(entries: &[Entry]) -> Option<&[u8]> {
    entries
        .iter()
        .filter(|entry| entry.kind == FILE && starts_caseless(&entry.name, METRICS))
        .map(|entry| entry.name.as_slice())
        .min_by(|a, b| catalogue::caseless(a, b))
}

/// The digits a data file's name ends in.
fn number_of(leaf: &[u8]) -> &[u8] {
    let digits = leaf.iter().rev().take_while(|b| b.is_ascii_digit()).count();
    &leaf[leaf.len() - digits..]
}

fn starts_caseless(name: &[u8], start: &[u8]) -> bool {
    name.len() >= start.len() && name[..start.len()].eq_ignore_ascii_case(start)
}

/// Whether a guest font's files draw it: its outlines file, its old
/// bitmaps beside it, or, for a size, a bitmap file of that size in
/// `bitmaps`, its sixteenths of a point and dots an inch.
pub(crate) fn has_drawing(
    os: &mut Os<'_>,
    prefix: &[u8],
    bitmaps: &[u8],
    files: &DataFiles,
    size: Option<((i32, i32), (i32, i32))>,
) -> Result<bool, Fail> {
    if is_file(os, &files.outlines)? || has_old_bitmaps(os, prefix)? {
        return Ok(true);
    }
    let Some((points, resolution)) = size else {
        return Ok(false);
    };
    for depth in [Depth::Four, Depth::One] {
        if let Some(leaf) = bitmaps::leaf(depth, points, resolution) {
            let mut name = bitmaps.to_vec();
            name.extend_from_slice(&leaf);
            if is_file(os, &name)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Whether the font in `prefix` has old bitmaps, an `x90y45` file.
pub(crate) fn has_old_bitmaps(os: &mut Os<'_>, prefix: &[u8]) -> Result<bool, Fail> {
    let mut index = prefix.to_vec();
    index.extend_from_slice(OLD_BITMAPS);
    is_file(os, &index)
}

/// The current territory's number.
pub(crate) fn territory(os: &mut Os<'_>) -> Result<u32, Fail> {
    let mut regs = Regs::default();
    os.perform(X | TERRITORY_NUMBER, &mut regs)?;
    Ok(regs.r[0])
}

/// The current alphabet's name, which names the encoding a font follows it
/// in.
pub(crate) fn alphabet(os: &mut Os<'_>) -> Result<Vec<u8>, Fail> {
    let mut byte = Regs::default();
    byte.r[..2].copy_from_slice(&[ALPHABET, READ_ALPHABET]);
    os.perform(X | kos::BYTE, &mut byte)?;
    let number = byte.r[1];
    os.below_supervisor_stack(NAME_ROOM, |os, buffer| {
        let mut regs = Regs::default();
        regs.r[1..6].copy_from_slice(&[
            SERVICE_INTERNATIONAL,
            ALPHABET_NAME,
            number,
            buffer,
            NAME_ROOM,
        ]);
        os.perform(X | kos::SERVICE_CALL, &mut regs)?;
        if regs.r[1] != 0 {
            return Ok(Vec::new());
        }
        os.read_vec(buffer, regs.r[5].min(NAME_ROOM) as usize)
    })
}

/// A variable's value unexpanded; none where it is not set.
pub(crate) fn read_variable(os: &mut Os<'_>, name: &[u8]) -> Result<Option<Vec<u8>>, Fail> {
    os.with_text(name, 0, |os, at| {
        os.below_supervisor_stack(NAME_ROOM, |os, buffer| {
            let mut regs = Regs::default();
            regs.r[..5].copy_from_slice(&[at, buffer, NAME_ROOM, 0, STRING]);
            match os.perform(X | kos::READ_VAR_VAL, &mut regs) {
                Ok(()) => Ok(Some(
                    os.read_vec(buffer, regs.r[2].min(NAME_ROOM) as usize)?,
                )),
                Err(Fail::Error(_)) => Ok(None),
                Err(other) => Err(other),
            }
        })
    })
}

/// Sets variable `name` to the macro `value`.
pub(crate) fn set_macro(os: &mut Os<'_>, name: &[u8], value: &[u8]) -> Result<(), Fail> {
    os.with_text(name, 0, |os, at| {
        os.with_text(value, 0, |os, text| {
            let mut regs = Regs::default();
            regs.r[..5].copy_from_slice(&[at, text, len32(value.len()), 0, MACRO]);
            os.perform(X | kos::SET_VAR_VAL, &mut regs)
        })
    })
}

#[cfg(test)]
mod tests;
