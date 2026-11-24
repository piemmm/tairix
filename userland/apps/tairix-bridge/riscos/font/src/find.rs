//! Finding fonts and reading their handles back: `Font_FindFont` and
//! `Font_LoseFont`, `Font_ReadDefn`, `Font_SetFont`, `Font_LookupFont`,
//! the prefix and encoding file a font was found with, and the identifier
//! fields calls.
//!
//! A font is found through the catalogue: the element of `Font$Path` that
//! lists it is where its files are. One of RISC OS's own families listed in
//! ResourceFS's font directory, where no metrics file stands for it, is the
//! TAIRiX face `lib/drawing` maps it to; any other font is the guest's
//! files, the first metrics file in its directory and the outlines or
//! bitmaps beside it. Its data is loaded as it is found, so a font whose
//! files do not hold together is refused there.

use alloc::rc::Rc;
use alloc::vec::Vec;

use tairix_riscos_kernel::error::BUFFER_OVERFLOW;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};

use crate::catalogue::Catalogue;
use crate::faces::Face;
use crate::fontdata::{self, FontData, Unloaded, UTF8};
use crate::grid::Axis;
use crate::handles::bitmap_directory;
use crate::handles::{Master, Size, Source, HANDLES};
use crate::identifier::{self, encoding_key, Asked, Matrix};
use crate::module::{error, raise, with};
use crate::scan::{self, Unreadable};
use crate::settings;

/// The longest identifier read from the guest: past it, the rest is not
/// read, as no font is named by so long an identifier.
pub(crate) const IDENTIFIER_MOST: usize = 1024;
/// What a plain `Font_ReadDefn` ends a name with, and what a full one does.
const PLAIN_END: u8 = b'\r';
const FULL_END: u8 = 0;
/// R3 asking `Font_ReadDefn` for the full identifier.
const FULL: u32 = u32::from_le_bytes(*b"FULL");
/// The resolution that means the screen's.
const DEFAULT_RESOLUTION: i32 = 0;
/// The size a master reads as.
const NO_SIZE: i32 = -1;
/// The encoding that is no encoding: a font's glyphs in its files' order.
const GLYPH: &[u8] = b"glyph";

/// `Font$Path` and `Font$Prefix` as the Font Manager starts them where they
/// are not set: the library `Font$Prefix` names, then ResourceFS's fonts.
const PATH: &[u8] = b"Font$Path";
const PATH_DEFAULT: &[u8] = b"<Font$Prefix>.,Resources:$.Fonts.";
const PREFIX: &[u8] = b"Font$Prefix";
const PREFIX_DEFAULT: &[u8] = b" ";

/// Sets `Font$Path` and `Font$Prefix` to their defaults where they are not
/// set.
pub(crate) fn set_path_defaults(os: &mut Os<'_>) -> Result<(), Fail> {
    if os.variable_text(PREFIX)?.is_none() {
        os.set_string_variable(PREFIX, PREFIX_DEFAULT)?;
    }
    if scan::read_variable(os, PATH)?.is_none() {
        scan::set_macro(os, PATH, PATH_DEFAULT)?;
    }
    Ok(())
}

/// The identifier at `at`, to its control character.
pub(crate) fn read_identifier(os: &mut Os<'_>, at: u32) -> Result<Vec<u8>, Fail> {
    Ok(os.read_until(at, IDENTIFIER_MOST, identifier::ends)?.0)
}

/// `Font_FindFont`. A negative size across finds the font's master, as
/// RISC OS 5.30 does.
pub(crate) fn find_font(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let text = read_identifier(os, regs.r[1])?;
    let asked = Asked::of(&text);
    let points = (regs.r[2].cast_signed(), regs.r[3].cast_signed());
    if points.0 == 0 || points.1 == 0 {
        return Err(raise(os, error::ZERO_SIZE, &[]));
    }
    let matrix = match asked.matrix {
        Some(Ok(matrix)) => Some(matrix),
        Some(Err(_)) => return Err(raise(os, error::BAD_MATRIX, &[])),
        None => None,
    };
    let territory = scan::territory(os)?;
    let resolution = resolved(os, (regs.r[4].cast_signed(), regs.r[5].cast_signed()))?;
    let sized = (points.0 > 0 && points.1 > 0).then_some((points, resolution));
    let master = scan::with_catalogue(os, false, |os, catalogue| {
        locate(os, catalogue, &asked, territory, sized)
    })?;
    let data = data_for(os, &master)?;
    let found = with(os, |state| {
        let found = if points.0 < 0 {
            state.handles.find_master(master)
        } else {
            state.handles.find(
                master,
                Size {
                    points,
                    resolution,
                    matrix,
                },
            )
        };
        for serial in state.handles.take_freed() {
            state.store.forget(serial);
        }
        if let (Ok(handle), Some(data)) = (found, &data) {
            if let Some(held) = state.handles.held(u32::from(handle)) {
                state.store.keep(held.master.serial, data);
            }
        }
        found
    })?;
    let Ok(handle) = found else {
        return Err(raise(os, error::NO_HANDLES, &[]));
    };
    let handle = u32::from(handle);
    with(os, |state| state.current.font = handle)?;
    regs.r[0] = handle;
    regs.r[4] = resolution.0.cast_unsigned();
    regs.r[5] = resolution.1.cast_unsigned();
    Ok(())
}

/// The data `master` needs loading, if it does: none where its master is
/// held already and the store still holds its data.
fn data_for(os: &mut Os<'_>, master: &Master) -> Result<Option<Rc<FontData>>, Fail> {
    let held = with(os, |state| {
        let serial = state
            .handles
            .master_of(&master.identifier, master.encoding.as_deref())
            .and_then(|handle| state.handles.held(u32::from(handle)))
            .map(|held| held.master.serial);
        serial.and_then(|serial| state.store.get(serial))
    })?;
    if held.is_some() {
        return Ok(None);
    }
    load_data(os, master).map(Some)
}

/// `master`'s data loaded afresh, a failure the Font Manager's error.
pub(crate) fn load_data(os: &mut Os<'_>, master: &Master) -> Result<Rc<FontData>, Fail> {
    let mut names = with(os, |state| state.names.clone())?;
    let had = names.is_some();
    let loaded = scan::with_catalogue(os, false, |os, catalogue| {
        Ok(fontdata::load(os, master, catalogue, &mut names))
    })?;
    if !had && names.is_some() {
        with(os, |state| state.names = names)?;
    }
    let shown = master
        .name
        .as_deref()
        .unwrap_or(&master.identifier)
        .to_vec();
    match loaded {
        Ok(data) => Ok(Rc::new(data)),
        Err(Unloaded::Failed(fail)) => Err(fail),
        Err(Unloaded::Illegal) => Err(raise(os, error::ILLEGAL, &[&shown])),
        Err(Unloaded::NotFound(target)) => Err(raise(os, error::NOT_FOUND, &[&target])),
        Err(Unloaded::NoData) => Err(raise(os, error::NO_DATA, &[&shown])),
    }
}

/// Resolutions as found: the screen's at the scale set for nought, else as
/// given; nought still where the scale gives the screen no pixels.
pub(crate) fn resolved(os: &mut Os<'_>, given: (i32, i32)) -> Result<(i32, i32), Fail> {
    let grid = settings::grid(os)?;
    let resolved = |given: i32, axis: Option<Axis>| match (given, axis) {
        (DEFAULT_RESOLUTION, Some(axis)) => axis.dots(),
        _ => given,
    };
    Ok((
        resolved(given.0, grid.map(|grid| grid.across)),
        resolved(given.1, grid.map(|grid| grid.up)),
    ))
}

/// The master `asked` names: the font the catalogue lists, in the encoding
/// asked for, else the alphabet's, or none for a symbol font unless the
/// alphabet is UTF-8's; its data where it should be, for the size `sized`,
/// its sixteenths of a point and dots an inch, where one is asked.
fn locate(
    os: &mut Os<'_>,
    catalogue: &Catalogue,
    asked: &Asked<'_>,
    territory: u32,
    sized: Option<((i32, i32), (i32, i32))>,
) -> Result<Master, Fail> {
    let shown = asked.shown(territory);
    let Some(font) = asked.font.and_then(|identifier| catalogue.font(identifier)) else {
        return Err(raise(os, error::NOT_FOUND, &[shown]));
    };
    let element = &catalogue.elements[font.element];
    let mut prefix = element.text.clone();
    prefix.extend_from_slice(&font.identifier);
    prefix.push(b'.');
    let alphabet = encoding_key(&scan::alphabet(os)?);
    let default = (!font.symbol || alphabet == UTF8)
        .then_some(alphabet)
        .filter(|key| key != GLYPH);
    let encoding = asked
        .encoding
        .map_or_else(|| default.clone(), |given| Some(encoding_key(given)));
    let encoding = encoding.filter(|key| key != GLYPH);
    let encoded = encoding.is_some() && encoding != default;
    let encoding_file = if let Some(key) = &encoding {
        let Some(listed) = catalogue
            .listed_encodings()
            .find(|e| encoding_key(&e.identifier) == *key)
        else {
            return Err(raise(os, error::NO_ENCODING, &[key]));
        };
        catalogue.encoding_file(listed)
    } else {
        let mut file = prefix.clone();
        file.extend_from_slice(scan::PRIVATE_ENCODING);
        file
    };
    let face = element.rom && Face::exists(&font.identifier);
    let unreadable = if face {
        Unreadable::Empty
    } else {
        Unreadable::Fails
    };
    let bitmaps = bitmap_directory(&prefix, encoded, &encoding_file);
    let source = match scan::data_files(os, &prefix, unreadable)? {
        None if face => Source::Face,
        Some(files) if scan::has_drawing(os, &prefix, &bitmaps, &files, sized)? => {
            Source::Files(files)
        }
        _ => return Err(raise(os, error::NO_DATA, &[font.shown()])),
    };
    Ok(Master::new(
        font.identifier.clone(),
        font.name.clone(),
        encoding,
        encoded,
        prefix,
        encoding_file,
        source,
    ))
}

/// `Font_LoseFont`, which refuses no handle.
pub(crate) fn lose_font(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = regs.r[0];
    with(os, |state| state.handles.lose(handle))
}

/// "Font handle out of range" unless `handle` is 1 to 255.
pub(crate) fn check_range(os: &mut Os<'_>, handle: u32) -> Result<(), Fail> {
    if (1..=len32(HANDLES)).contains(&handle) {
        Ok(())
    } else {
        Err(raise(os, error::OUT_OF_RANGE, &[]))
    }
}

/// `Font_SetFont`, which takes any handle in range, defined or not.
pub(crate) fn set_font(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = regs.r[0];
    check_range(os, handle)?;
    with(os, |state| state.current.font = handle)
}

/// What a handle holds, read out: the identifier, full or plain, and the
/// registers `Font_ReadDefn` gives.
struct Defn {
    identifier: Vec<u8>,
    registers: [u32; 6],
}

/// What handle `handle` holds; "Undefined font handle" for one that holds
/// nothing.
fn defn(os: &mut Os<'_>, handle: u32, full: bool) -> Result<Defn, Fail> {
    let territory = scan::territory(os)?;
    let found = with(os, |state| {
        let held = state.handles.held(handle)?;
        let identifier = if full {
            full_identifier(held.master, held.size.and_then(|s| s.matrix), territory)
        } else {
            held.master.identifier.clone()
        };
        let (points, resolution) = held
            .size
            .map_or(((NO_SIZE, NO_SIZE), (NO_SIZE, NO_SIZE)), |s| {
                (s.points, s.resolution)
            });
        Some(Defn {
            identifier,
            registers: [
                points.0.cast_unsigned(),
                points.1.cast_unsigned(),
                resolution.0.cast_unsigned(),
                resolution.1.cast_unsigned(),
                0,
                held.uses,
            ],
        })
    })?;
    found.ok_or_else(|| raise(os, error::UNDEFINED, &[]))
}

/// A font's full identifier, as `Font_ReadDefn` gives it: its identifier,
/// its name where a Messages file gives one, its encoding and its matrix.
fn full_identifier(master: &Master, matrix: Option<Matrix>, territory: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\\F");
    out.extend_from_slice(&master.identifier);
    if let Some(name) = &master.name {
        out.extend_from_slice(b"\\f");
        out.extend_from_slice(
            tairix_riscos_kernel::Decimal::of(territory.cast_signed()).as_bytes(),
        );
        out.push(b' ');
        out.extend_from_slice(name);
    }
    if let Some(encoding) = &master.encoding {
        out.extend_from_slice(b"\\E");
        out.extend_from_slice(encoding);
    }
    if let Some(matrix) = matrix {
        out.extend_from_slice(b"\\M");
        out.extend_from_slice(&matrix.spelled());
    }
    out
}

/// `Font_ReadDefn`: with R3 "FULL", the full identifier — its size alone
/// where R1 is nought, else written whole, as RISC OS writes it, whatever
/// R2 says of the buffer.
pub(crate) fn read_defn(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let full = regs.r[3] == FULL;
    let defn = defn(os, regs.r[0], full)?;
    if full && regs.r[1] == 0 {
        regs.r[2] = len32(defn.identifier.len() + 1);
        return Ok(());
    }
    let end = if full { FULL_END } else { PLAIN_END };
    os.write_bytes(regs.r[1], &defn.identifier)?;
    os.write_byte(regs.r[1].wrapping_add(len32(defn.identifier.len())), end)?;
    regs.r[2..8].copy_from_slice(&defn.registers);
    Ok(())
}

/// The master at, or of the size at, `handle`, read by `read`.
fn of_master<T>(os: &mut Os<'_>, handle: u32, read: impl FnOnce(&Master) -> T) -> Result<T, Fail> {
    let read = with(os, |state| {
        state.handles.held(handle).map(|held| read(held.master))
    })?;
    read.ok_or_else(|| raise(os, error::UNDEFINED, &[]))
}

/// `text` written to the buffer at R1 of R2 bytes, terminated: R1 left at
/// the terminator and R2 counting what is left from it; "Buffer overflow",
/// with R2 nought, where it does not fit.
fn give(os: &mut Os<'_>, regs: &mut Regs, text: &[u8]) -> Result<(), Fail> {
    let length = len32(text.len());
    if length >= regs.r[2] {
        regs.r[2] = 0;
        return Err(os.error(BUFFER_OVERFLOW));
    }
    os.write_bytes(regs.r[1], text)?;
    os.write_byte(regs.r[1].wrapping_add(length), 0)?;
    regs.r[1] = regs.r[1].wrapping_add(length);
    regs.r[2] -= length;
    Ok(())
}

/// `Font_ReadFontPrefix`: the directory the font was found in.
pub(crate) fn read_font_prefix(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let prefix = of_master(os, regs.r[0], |master| master.prefix.clone())?;
    give(os, regs, &prefix)
}

/// `Font_ReadEncodingFilename`: the file the font's encoding is in, R0
/// left pointing at it.
pub(crate) fn read_encoding_filename(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let file = of_master(os, regs.r[0], |master| master.encoding_file.clone())?;
    let start = regs.r[1];
    give(os, regs, &file)?;
    regs.r[0] = start;
    Ok(())
}

/// `Font_LookupFont`: R0 the font, nought for the current one; R2 its
/// characteristics.
pub(crate) fn lookup_font(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = match regs.r[0] {
        0 => crate::resolve::current(os)?,
        handle => handle,
    };
    let font = crate::resolve::font(os, handle)?;
    regs.r[2] = font.data.characteristics;
    Ok(())
}

/// `Font_FindField`: R1 left at the value of R2's field, or R2 made nought
/// where the identifier has none.
pub(crate) fn find_field(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let text = read_identifier(os, regs.r[1])?;
    let wanted = regs.r[2].to_le_bytes()[0];
    match identifier::find(&text, wanted) {
        Some(field) if u32::from(wanted) == regs.r[2] => {
            regs.r[1] = regs.r[1].wrapping_add(len32(field.at));
        }
        _ => regs.r[2] = 0,
    }
    Ok(())
}

/// `Font_ApplyFields`: R0's identifier with R1's fields in it, into the
/// buffer at R2 of R3 bytes, R3 left counting what is left; with R2
/// nought, R3 grown by the length alone.
pub(crate) fn apply_fields(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let original = read_identifier(os, regs.r[0])?;
    let fields = read_identifier(os, regs.r[1])?;
    let applied = identifier::apply(&original, &fields);
    let length = len32(applied.len());
    if regs.r[2] == 0 {
        regs.r[3] = regs.r[3].wrapping_add(length);
        return Ok(());
    }
    if length >= regs.r[3] {
        regs.r[3] = 0;
        return Err(os.error(BUFFER_OVERFLOW));
    }
    os.write_bytes(regs.r[2], &applied)?;
    os.write_byte(regs.r[2].wrapping_add(length), 0)?;
    regs.r[3] -= length;
    Ok(())
}

#[cfg(test)]
mod tests;
