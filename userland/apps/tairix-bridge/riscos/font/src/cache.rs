//! The font cache as the guest sees it: what each handle holds of it, and
//! its saving and loading. A saved cache is the fonts the handles hold —
//! each master and size, by handle — in TAIRiX Bridge's own format, so a
//! loaded one brings them back as cached fonts on the handles they had; the
//! data decoded for them is the session's, read again when next wanted. A
//! file is the guest's, so loading reads it as hostile and refuses it whole
//! on any fault.

use alloc::vec::Vec;

use tairix_riscos_kernel::os::{len32, Fail, Os};
use tairix_util::endian::{le_u16, le_u32};

use crate::handles::{Handles, Master, Size, Slot, Source, HANDLES};
use crate::identifier::Matrix;
use crate::module::{error, raise, with, State};
use crate::scan;
use crate::scan::DataFiles;

/// A saved cache's file type, and how it opens.
const FONT_CACHE: u32 = 0xFCF;
const MAGIC: &[u8; 8] = b"TBFCache";
const VERSION: u32 = 1;
/// The longest string a saved cache holds, and the most it is read to.
const STRING_MOST: usize = 1024;
const FILE_MOST: u32 = 4 * 1024 * 1024;
/// A string's length that stands for none.
const NONE: u16 = u16::MAX;
/// What each entry is.
const MASTER: u8 = 1;
const SIZED: u8 = 2;
/// Where a master's data comes from.
const FACE: u8 = 0;
const FILES: u8 = 1;

/// The bytes handle `handle`'s font holds of the cache: a master's, its
/// data's while the store holds it; a size holds none of its own.
pub(crate) fn bytes_of(state: &State, handle: u8) -> u32 {
    match state.handles.get(u32::from(handle)) {
        Some(Slot::Master(master)) => state
            .store
            .bytes_of(master.serial)
            .map_or(0, |bytes| u32::try_from(bytes).unwrap_or(u32::MAX)),
        _ => 0,
    }
}

/// `*SaveFontCache`: the handles' fonts to the file `name`.
pub(crate) fn save(os: &mut Os<'_>, name: &[u8]) -> Result<(), Fail> {
    let bytes = with(os, |state| encode(&state.handles))?;
    scan::save(os, name, FONT_CACHE, &bytes)
}

/// `*LoadFontCache`: the fonts the file `name` saved, as cached fonts, in
/// place of those held, while none is in use.
pub(crate) fn load(os: &mut Os<'_>, name: &[u8]) -> Result<(), Fail> {
    if with(os, |state| state.handles.claimed())? {
        return Err(raise(os, error::FONTS_CLAIMED, &[]));
    }
    let Some(bytes) = scan::load(os, name, FILE_MOST)? else {
        return Err(raise(os, error::NOT_CACHE, &[]));
    };
    let Some(handles) = decode(&bytes) else {
        return Err(raise(os, error::NOT_CACHE, &[]));
    };
    with(os, |state| {
        state.handles = handles;
        state.store.forget_all();
    })
}

fn encode(handles: &Handles) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&VERSION.to_le_bytes());
    let used: Vec<(u8, &Slot)> = handles
        .slots()
        .filter(|(_, slot)| !matches!(slot, Slot::Free))
        .collect();
    out.extend_from_slice(&len32(used.len()).to_le_bytes());
    for (handle, slot) in used {
        out.push(handle);
        match slot {
            Slot::Master(master) => {
                out.push(MASTER);
                for text in [
                    Some(&master.identifier),
                    master.name.as_ref(),
                    master.encoding.as_ref(),
                    Some(&master.prefix),
                    Some(&master.encoding_file),
                ] {
                    string(&mut out, text.map(Vec::as_slice));
                }
                out.push(u8::from(master.encoded));
                match &master.source {
                    Source::Face => out.push(FACE),
                    Source::Files(files) => {
                        out.push(FILES);
                        for text in [
                            Some(&files.metrics),
                            Some(&files.outlines),
                            files.base.as_ref(),
                        ] {
                            string(&mut out, text.map(Vec::as_slice));
                        }
                    }
                }
            }
            Slot::Sized(sized) => {
                out.push(SIZED);
                out.push(sized.master);
                let size = sized.size;
                for word in [
                    size.points.0,
                    size.points.1,
                    size.resolution.0,
                    size.resolution.1,
                ] {
                    out.extend_from_slice(&word.to_le_bytes());
                }
                match size.matrix {
                    Some(Matrix(words)) => {
                        out.push(1);
                        for word in words {
                            out.extend_from_slice(&word.to_le_bytes());
                        }
                    }
                    None => out.push(0),
                }
            }
            Slot::Free => {}
        }
    }
    out
}

fn string(out: &mut Vec<u8>, text: Option<&[u8]>) {
    match text {
        Some(text) => {
            let length = u16::try_from(text.len().min(STRING_MOST)).unwrap_or(0);
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(&text[..usize::from(length)]);
        }
        None => out.extend_from_slice(&NONE.to_le_bytes()),
    }
}

/// A saved cache's bytes that are not one whole and sound.
struct Unsound;

/// A saved cache's bytes read in order.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Unsound> {
        if n > self.bytes.len() {
            return Err(Unsound);
        }
        let (taken, rest) = self.bytes.split_at(n);
        self.bytes = rest;
        Ok(taken)
    }

    fn byte(&mut self) -> Result<u8, Unsound> {
        self.take(1).map(|b| b[0])
    }

    fn word(&mut self) -> Result<u32, Unsound> {
        le_u32(self.take(4)?, 0).ok_or(Unsound)
    }

    /// A string, or none where one is written as none.
    fn maybe(&mut self) -> Result<Option<Vec<u8>>, Unsound> {
        let length = le_u16(self.take(2)?, 0).ok_or(Unsound)?;
        if length == NONE {
            return Ok(None);
        }
        let length = usize::from(length);
        if length > STRING_MOST {
            return Err(Unsound);
        }
        Ok(Some(self.take(length)?.to_vec()))
    }

    /// A string that must be there.
    fn string(&mut self) -> Result<Vec<u8>, Unsound> {
        self.maybe()?.ok_or(Unsound)
    }
}

/// The handles a saved cache holds, each font cached; none for bytes that
/// are not one whole and sound: a handle twice or out of range, a size whose
/// master is not a master, a string too long, bytes past its end.
fn decode(bytes: &[u8]) -> Option<Handles> {
    entries(bytes)
        .ok()
        .and_then(|(masters, sizes)| Handles::restored(masters, sizes))
}

/// A saved cache's masters by handle, and its sizes by handle and master.
type Masters = Vec<(u8, Master)>;
type Sizes = Vec<(u8, u8, Size)>;

/// The masters and sizes a saved cache's bytes hold.
fn entries(bytes: &[u8]) -> Result<(Masters, Sizes), Unsound> {
    let mut reader = Reader { bytes };
    if reader.take(MAGIC.len())? != MAGIC || reader.word()? != VERSION {
        return Err(Unsound);
    }
    let count = usize::try_from(reader.word()?).map_err(|_| Unsound)?;
    if count > HANDLES {
        return Err(Unsound);
    }
    let mut masters = Vec::new();
    let mut sizes = Vec::new();
    for _ in 0..count {
        let handle = reader.byte()?;
        match reader.byte()? {
            MASTER => {
                let identifier = reader.string()?;
                let name = reader.maybe()?;
                let encoding = reader.maybe()?;
                let prefix = reader.string()?;
                let encoding_file = reader.string()?;
                let source = match reader.byte()? {
                    FACE => Source::Face,
                    FILES => Source::Files(DataFiles {
                        metrics: reader.string()?,
                        outlines: reader.string()?,
                        base: reader.maybe()?,
                    }),
                    _ => return Err(Unsound),
                };
                let encoded = reader.byte()? != 0;
                masters.push((
                    handle,
                    Master::new(
                        identifier,
                        name,
                        encoding,
                        encoded,
                        prefix,
                        encoding_file,
                        source,
                    ),
                ));
            }
            SIZED => {
                let master = reader.byte()?;
                let mut words = [0i32; 4];
                for word in &mut words {
                    *word = reader.word()?.cast_signed();
                }
                let matrix = match reader.byte()? {
                    0 => None,
                    1 => {
                        let mut matrix = [0i32; 6];
                        for word in &mut matrix {
                            *word = reader.word()?.cast_signed();
                        }
                        Some(Matrix(matrix))
                    }
                    _ => return Err(Unsound),
                };
                let size = Size {
                    points: (words[0], words[1]),
                    resolution: (words[2], words[3]),
                    matrix,
                };
                sizes.push((handle, master, size));
            }
            _ => return Err(Unsound),
        }
    }
    if !reader.bytes.is_empty() {
        return Err(Unsound);
    }
    Ok((masters, sizes))
}

#[cfg(test)]
mod tests;
