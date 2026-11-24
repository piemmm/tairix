//! A font's data, loaded when its font is found: how its codes map to its
//! glyphs, and each glyph's metrics in thousandths of an em. A guest's font
//! is its metrics and outlines files and the base encoding the number their
//! names end in names; a ROM font is its TAIRiX face, whose glyphs are base
//! encoding 0's. Both map their codes through the same encoding files, as
//! the UCS Font Manager does.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::OnceCell;
use core::mem::size_of;

use tairix_riscos_kernel::os::{Fail, Os};

use crate::catalogue::Catalogue;
use crate::encoding::{CodeMap, EncodingFile};
use crate::faces::{Face, Unanswered};
use crate::glyph_names::GlyphNames;
use crate::handles::{bitmap_directory, Master, Source};
use crate::metrics::{self, Kern, Metrics, Misc};
use crate::outlines::{self, Outlines, CHUNK};
use crate::rom_encodings::BASE0_GLYPHS;
use crate::scan::{self, DataFiles, Unreadable};

/// The UTF-8 encoding's key, as the Font Manager keeps it.
pub(crate) const UTF8: &[u8] = b"utf8";
/// The base encodings a ROM font and a symbol font with no encoding of its
/// own are read through.
const ROM_BASE: &[u8] = b"Base0";
const SYMBOL_BASE: &[u8] = b"Default";
/// The largest metrics, outlines and encoding files read.
const METRICS_MOST: u32 = 1 << 20;
const OUTLINES_MOST: u32 = 16 << 20;
const ENCODING_MOST: u32 = 1 << 20;
/// How many outlines files a redirection chain may pass through.
const REDIRECTS_MOST: usize = 4;
/// What a ROM font's metrics file's flags say of its face: no y offsets, and
/// the miscellaneous measures there.
const FACE_FLAGS: u8 = 1 << 2 | 1 << 3;
/// The 256ths of an em a face's underline sits below its baseline and is
/// thick, which the font service does not give.
const FACE_UNDERLINE: (i32, i32) = (-26, 13);
/// The characters whose glyphs' tops are a face's cap height and x height.
const CAP: u32 = 0x48;
const EX: u32 = 0x78;

/// `Font_LookupFont`'s characteristics.
mod characteristic {
    pub(super) const BITMAPS: u32 = 1;
    pub(super) const ROM: u32 = 1 << 1;
    pub(super) const MONOCHROME: u32 = 1 << 8;
    pub(super) const NON_ZERO: u32 = 1 << 9;
}

/// How a font's codes map to its glyphs.
#[derive(Debug)]
pub(crate) enum Codes {
    /// It has no encoding: a code is its glyph.
    Glyph,
    /// Through its encoding.
    Mapped(Box<CodeMap>),
    /// Its base encoding, named here, is not on `Font$Path`: no code is a
    /// glyph.
    NoBase(Vec<u8>),
}

/// What a guest's font draws with.
#[derive(Debug)]
enum Drawn {
    /// Outlines, its own or those its own name.
    Outlines(Outlines),
    /// The face of the ROM font its outlines name.
    Face(Face),
    /// Bitmaps, which hold no outlines, among them an `x90y45` file or not.
    Bitmaps { old: bool },
}

/// A guest's font's files: its metrics, what it draws with, and where its
/// metrics give no boxes and its outlines do, each glyph's, made when first
/// asked for.
#[derive(Debug)]
struct Files {
    metrics: Metrics,
    drawn: Drawn,
    outline_boxes: Option<Vec<OnceCell<metrics::Box>>>,
}

impl Files {
    /// Glyph `glyph`'s box where its metrics give none: its outline's or its
    /// face's.
    fn drawn_box(&self, glyph: u32) -> Option<metrics::Box> {
        match &self.drawn {
            Drawn::Outlines(outlines) => {
                let cell = self
                    .outline_boxes
                    .as_ref()?
                    .get(usize::try_from(glyph).ok()?)?;
                Some(*cell.get_or_init(|| outline_box(outlines, glyph)))
            }
            Drawn::Face(face) => face.shape(glyph).map(|shape| shape.bbox),
            Drawn::Bitmaps { .. } => None,
        }
    }
}

/// Where a font's glyphs come from.
#[derive(Debug)]
enum Glyphs {
    /// A guest's files.
    Files(Box<Files>),
    /// A TAIRiX face.
    Face(Face),
}

/// What draws a glyph: an outlines file's character in its design units,
/// so many to an em, or a face's outline in ems, y up.
#[derive(Debug)]
pub(crate) enum Outline<'a> {
    Design {
        glyph: outlines::Glyph,
        design_size: u16,
    },
    Face(&'a tairix_bezier::Path),
}

/// What a glyph measures, in thousandths of an em.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Glyph {
    pub(crate) advance: (i32, i32),
    pub(crate) bbox: metrics::Box,
}

/// A font's data.
#[derive(Debug)]
pub(crate) struct FontData {
    pub(crate) codes: Codes,
    /// Whether its strings are UTF-8, and UTF-16 in halfwords.
    pub(crate) utf8: bool,
    glyphs: Glyphs,
    /// The glyphs its files have room for: a code its encoding of none may
    /// reach.
    pub(crate) count: u32,
    /// Its box, in thousandths of an em.
    pub(crate) bbox: metrics::Box,
    /// Its other measures, where it has them.
    pub(crate) misc: Option<Misc>,
    /// Its metrics file's flags.
    pub(crate) flags: u8,
    /// What `Font_LookupFont` says of it.
    pub(crate) characteristics: u32,
    /// Its identifier, as the catalogue spells it.
    pub(crate) identifier: Vec<u8>,
    /// The directory its bitmap files are in, its trailing full stop
    /// included.
    pub(crate) bitmaps: Vec<u8>,
    bytes: usize,
}

impl FontData {
    /// The glyph code `code` shows; none for a code that shows none.
    pub(crate) fn code(&self, code: u32) -> Option<u32> {
        match &self.codes {
            Codes::Glyph => (code < self.count).then_some(code),
            Codes::Mapped(map) => map.glyph(code),
            Codes::NoBase(_) => None,
        }
    }

    /// Each code that shows a glyph, and the glyph, in code order.
    pub(crate) fn defined(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let (unmapped, mapped) = match &self.codes {
            Codes::Glyph => (Some((0..self.count).map(|glyph| (glyph, glyph))), None),
            Codes::Mapped(map) => (None, Some(map.below(u32::MAX))),
            Codes::NoBase(_) => (None, None),
        };
        unmapped
            .into_iter()
            .flatten()
            .chain(mapped.into_iter().flatten())
    }

    /// What glyph `glyph` measures; none where the font does not define it.
    pub(crate) fn glyph(&self, glyph: u32) -> Option<Glyph> {
        match &self.glyphs {
            Glyphs::Files(files) => {
                let measure = files.metrics.measure(glyph)?;
                let bbox = measure
                    .bbox
                    .or_else(|| files.drawn_box(glyph))
                    .unwrap_or_default();
                Some(Glyph {
                    advance: (measure.x_offset, measure.y_offset),
                    bbox,
                })
            }
            Glyphs::Face(face) => face.shape(glyph).map(|shape| Glyph {
                advance: (shape.advance, 0),
                bbox: shape.bbox,
            }),
        }
    }

    /// The kern after glyph `left` before glyph `right`, in thousandths of
    /// an em.
    pub(crate) fn kern(&self, left: u32, right: u32) -> (i32, i32) {
        let (Ok(left), Ok(right)) = (u16::try_from(left), u16::try_from(right)) else {
            return (0, 0);
        };
        match &self.glyphs {
            Glyphs::Files(files) => files
                .metrics
                .kern(left, right)
                .map_or((0, 0), |k| (k.x, k.y)),
            Glyphs::Face(_) => (0, 0),
        }
    }

    /// Its kern pairs, by left then right glyph.
    pub(crate) fn kerns(&self) -> &[Kern] {
        match &self.glyphs {
            Glyphs::Files(files) => &files.metrics.kerns,
            Glyphs::Face(_) => &[],
        }
    }

    /// What draws glyph `glyph`; none for a glyph it draws no outline of.
    pub(crate) fn outline(&self, glyph: u32) -> Option<Outline<'_>> {
        let face = match &self.glyphs {
            Glyphs::Files(files) => match &files.drawn {
                Drawn::Outlines(outlines) => {
                    return outlines
                        .glyph(glyph)
                        .ok()
                        .flatten()
                        .map(|drawn| Outline::Design {
                            glyph: drawn,
                            design_size: outlines.design_size,
                        });
                }
                Drawn::Face(face) => face,
                Drawn::Bitmaps { .. } => return None,
            },
            Glyphs::Face(face) => face,
        };
        face.shape(glyph).map(|shape| Outline::Face(&shape.path))
    }

    /// Whether its outlines fill by the non-zero rule rather than even-odd.
    pub(crate) const fn non_zero(&self) -> bool {
        self.characteristics & characteristic::NON_ZERO != 0
    }

    /// Whether it is never anti-aliased.
    pub(crate) const fn monochrome(&self) -> bool {
        self.characteristics & characteristic::MONOCHROME != 0
    }

    /// Whether it has outlines: a guest's that draws with bitmaps alone has
    /// none.
    pub(crate) fn has_outlines(&self) -> bool {
        !matches!(&self.glyphs, Glyphs::Files(files) if matches!(files.drawn, Drawn::Bitmaps { .. }))
    }

    /// Roughly what it holds in memory.
    pub(crate) const fn bytes(&self) -> usize {
        self.bytes
    }
}

/// Why a font's data could not be loaded.
#[derive(Debug)]
pub(crate) enum Unloaded {
    /// A call failed: the error to give.
    Failed(Fail),
    /// A file of the font's does not hold together.
    Illegal,
    /// The font its outlines name is not on `Font$Path`.
    NotFound(Vec<u8>),
    /// Nothing draws it: no outlines and no bitmaps, or a face the font
    /// service would not give.
    NoData,
}

impl From<Fail> for Unloaded {
    fn from(fail: Fail) -> Self {
        Self::Failed(fail)
    }
}

/// The data of the font `master` is, its files found through `catalogue`
/// and a ROM font's face asked of the font service; `names` the glyph lists,
/// read once a UTF-8 font needs them.
pub(crate) fn load(
    os: &mut Os<'_>,
    master: &Master,
    catalogue: &Catalogue,
    names: &mut Option<Rc<GlyphNames>>,
) -> Result<FontData, Unloaded> {
    let found = match &master.source {
        Source::Face => base_file(catalogue, ROM_BASE),
        Source::Files(files) => {
            if let Some(number) = &files.base {
                let mut identifier = b"Base".to_vec();
                identifier.extend_from_slice(number);
                base_file(catalogue, &identifier)
            } else {
                let mut own = master.prefix.clone();
                own.extend_from_slice(scan::PRIVATE_ENCODING);
                if scan::is_file(os, &own)? {
                    Ok(own)
                } else {
                    base_file(catalogue, SYMBOL_BASE)
                }
            }
        }
    };
    let base = match found {
        Ok(file) => scan::load(os, &file, ENCODING_MOST)?
            .map(EncodingFile::read)
            .ok_or(file),
        Err(missing) => Err(missing),
    };
    let utf8 = master.encoding.as_deref() == Some(UTF8);
    let codes = match (&master.encoding, base) {
        (None, _) => Codes::Glyph,
        (Some(_), Err(missing)) => Codes::NoBase(missing),
        (Some(_), Ok(base)) if utf8 => {
            let names = names.get_or_insert_with(|| Rc::new(GlyphNames::read()));
            Codes::Mapped(Box::new(CodeMap::utf8(&base, names)))
        }
        (Some(_), Ok(base)) => {
            let target = scan::load(os, &master.encoding_file, ENCODING_MOST)?
                .map(EncodingFile::read)
                .unwrap_or_default();
            Codes::Mapped(Box::new(CodeMap::based(&target, &base)))
        }
    };
    let glyphs = match &master.source {
        Source::Face => {
            let face = face(os, &master.identifier, &codes)?;
            Glyphs::Face(face)
        }
        Source::Files(files) => files_glyphs(os, master, files, catalogue, &codes)?,
    };
    let bitmaps = bitmap_directory(&master.prefix, master.encoded, &master.encoding_file);
    Ok(assemble(
        codes,
        utf8,
        glyphs,
        master.identifier.clone(),
        bitmaps,
    ))
}

/// The name of base encoding `identifier`'s file on `Font$Path`; the name a
/// missing one goes by where none is.
fn base_file(catalogue: &Catalogue, identifier: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
    catalogue
        .base(identifier)
        .map(|e| catalogue.encoding_file(e))
        .ok_or_else(|| {
            let mut missing = b"/".to_vec();
            missing.extend_from_slice(identifier);
            missing
        })
}

/// The face of ROM font `identifier`, its glyphs those `codes` reach and the
/// two its cap and x heights are read from.
fn face(os: &mut Os<'_>, identifier: &[u8], codes: &Codes) -> Result<Face, Unloaded> {
    let mut wanted: Vec<u32> = match codes {
        Codes::Glyph => (0..u32::try_from(BASE0_GLYPHS).unwrap_or(0)).collect(),
        Codes::Mapped(map) => map.below(u32::MAX).map(|(_, glyph)| glyph).collect(),
        Codes::NoBase(_) => Vec::new(),
    };
    wanted.extend(base_glyph_of(CAP).into_iter().chain(base_glyph_of(EX)));
    let desktop = os.desktop_font().family;
    Face::load(
        identifier,
        desktop,
        &mut |request, reply| os.font_relay(request, reply),
        wanted.into_iter(),
    )
    .map_err(|Unanswered| Unloaded::NoData)
}

/// The base glyph whose first character is `character`.
fn base_glyph_of(character: u32) -> Option<u32> {
    crate::rom_encodings::BASE0
        .iter()
        .position(|characters| characters.first() == Some(&character))
        .and_then(|at| u32::try_from(at).ok())
}

/// A guest font's glyphs: its metrics file and what it draws with.
fn files_glyphs(
    os: &mut Os<'_>,
    master: &Master,
    files: &DataFiles,
    catalogue: &Catalogue,
    codes: &Codes,
) -> Result<Glyphs, Unloaded> {
    let bytes = scan::load(os, &files.metrics, METRICS_MOST)?.ok_or(Unloaded::Illegal)?;
    let metrics = Metrics::read(&bytes).map_err(|_| Unloaded::Illegal)?;
    let drawn = drawn(os, &master.prefix, files, catalogue, codes)?;
    let outline_boxes = match &drawn {
        Drawn::Outlines(outlines) if !metrics.has_boxes() => {
            let mut cells = Vec::new();
            cells
                .try_reserve_exact(outlines.slots())
                .map_err(|_| Unloaded::Illegal)?;
            cells.resize_with(outlines.slots(), OnceCell::new);
            Some(cells)
        }
        _ => None,
    };
    Ok(Glyphs::Files(Box::new(Files {
        metrics,
        drawn,
        outline_boxes,
    })))
}

/// What a guest font in `prefix` draws with: its outlines file, following
/// one short enough to name a font to that font's file of the same number,
/// or its face where it is a ROM font's; its bitmaps where it has no
/// outlines.
fn drawn(
    os: &mut Os<'_>,
    prefix: &[u8],
    files: &DataFiles,
    catalogue: &Catalogue,
    codes: &Codes,
) -> Result<Drawn, Unloaded> {
    let leaf = files
        .outlines
        .strip_prefix(prefix)
        .unwrap_or(&files.outlines)
        .to_vec();
    let mut file = files.outlines.clone();
    for _ in 0..REDIRECTS_MOST {
        let Some(bytes) = scan::load(os, &file, OUTLINES_MOST)? else {
            return if file == files.outlines {
                Ok(Drawn::Bitmaps {
                    old: scan::has_old_bitmaps(os, prefix)?,
                })
            } else {
                Err(Unloaded::NoData)
            };
        };
        let named = match outlines::File::read(bytes).map_err(|_| Unloaded::Illegal)? {
            outlines::File::Outlines(outlines) => return Ok(Drawn::Outlines(outlines)),
            outlines::File::Bitmaps(_) => return Err(Unloaded::Illegal),
            outlines::File::Redirect(named) => named,
        };
        let identifier = crate::identifier::Asked::of(&named)
            .font
            .map(<[u8]>::to_vec)
            .ok_or(Unloaded::Illegal)?;
        let Some(target) = catalogue.font(&identifier) else {
            return Err(Unloaded::NotFound(identifier));
        };
        let element = &catalogue.elements[target.element];
        let mut target_prefix = element.text.clone();
        target_prefix.extend_from_slice(&target.identifier);
        target_prefix.push(b'.');
        if element.rom
            && Face::exists(&target.identifier)
            && scan::data_files(os, &target_prefix, Unreadable::Empty)?.is_none()
        {
            return face(os, &target.identifier, codes).map(Drawn::Face);
        }
        file = target_prefix;
        file.extend_from_slice(&leaf);
    }
    Err(Unloaded::Illegal)
}

/// Glyph `glyph`'s box its outline gives, scaled from its design units to
/// thousandths of an em, outward: nought for one not defined or not holding
/// together, which draws nothing.
fn outline_box(outlines: &Outlines, glyph: u32) -> metrics::Box {
    outlines
        .glyph(glyph)
        .ok()
        .flatten()
        .and_then(|g| g.bbox)
        .map_or_else(metrics::Box::default, |b| outlines.millems(b))
}

/// The data its parts make: its count, box, measures and size, its
/// identifier and its bitmap files in `bitmaps`.
fn assemble(
    codes: Codes,
    utf8: bool,
    glyphs: Glyphs,
    identifier: Vec<u8>,
    bitmaps: Vec<u8>,
) -> FontData {
    let Measures {
        count,
        bbox,
        misc,
        flags,
        characteristics,
    } = measures(&glyphs);
    let bytes = held_bytes(&codes, &glyphs) + identifier.len() + bitmaps.len();
    FontData {
        codes,
        utf8,
        glyphs,
        count: u32::try_from(count).unwrap_or(u32::MAX),
        bbox,
        misc,
        flags,
        characteristics,
        identifier,
        bitmaps,
        bytes,
    }
}

/// What a font's glyphs give its data: how many its files have room for,
/// its box, its measures, its flags and its characteristics.
struct Measures {
    count: usize,
    bbox: metrics::Box,
    misc: Option<Misc>,
    flags: u8,
    characteristics: u32,
}

/// `glyphs`' measures.
fn measures(glyphs: &Glyphs) -> Measures {
    let (count, bbox, misc, flags, characteristics) = match glyphs {
        Glyphs::Files(files) => {
            let Files { metrics, drawn, .. } = files.as_ref();
            let (count, drawn_box, characteristics) = match drawn {
                Drawn::Outlines(outlines) => (
                    outlines.slots(),
                    Some(outlines.millems(outlines.bbox)),
                    if outlines.monochrome {
                        characteristic::MONOCHROME
                    } else {
                        0
                    },
                ),
                Drawn::Face(face) => (
                    chunked(BASE0_GLYPHS),
                    Some(face.bbox()),
                    characteristic::ROM | characteristic::NON_ZERO,
                ),
                Drawn::Bitmaps { old } => (
                    metrics.map_len(),
                    None,
                    if *old { characteristic::BITMAPS } else { 0 },
                ),
            };
            let bbox = drawn_box
                .or_else(|| metrics.misc.map(|m| m.bbox))
                .unwrap_or_default();
            (count, bbox, metrics.misc, metrics.flags, characteristics)
        }
        Glyphs::Face(face) => {
            let (ascender, descender) = face.extent();
            let top = |character| {
                base_glyph_of(character)
                    .and_then(|g| face.shape(g))
                    .map_or(0, |s| s.bbox.y1)
            };
            let bbox = face.bbox();
            let misc = Misc {
                bbox,
                x_offset: 0,
                y_offset: 0,
                italic: face.italic(),
                underline_position: FACE_UNDERLINE.0,
                underline_thickness: FACE_UNDERLINE.1,
                cap_height: top(CAP),
                x_height: top(EX),
                descender,
                ascender,
            };
            (
                chunked(BASE0_GLYPHS),
                bbox,
                Some(misc),
                FACE_FLAGS,
                characteristic::ROM | characteristic::NON_ZERO,
            )
        }
    };
    Measures {
        count,
        bbox,
        misc,
        flags,
        characteristics,
    }
}

/// The bytes a font's data holds beside its identifier and bitmap files:
/// itself, its code map and its glyphs.
fn held_bytes(codes: &Codes, glyphs: &Glyphs) -> usize {
    let map = match codes {
        Codes::Mapped(map) => map.bytes(),
        Codes::Glyph | Codes::NoBase(_) => 0,
    };
    let glyphs = match glyphs {
        Glyphs::Files(files) => {
            let Files {
                metrics,
                drawn,
                outline_boxes,
            } = files.as_ref();
            metrics.bytes()
                + outline_boxes
                    .as_ref()
                    .map_or(0, |b| b.len() * size_of::<OnceCell<metrics::Box>>())
                + match drawn {
                    Drawn::Outlines(outlines) => outlines.bytes(),
                    Drawn::Face(face) => face.bytes(),
                    Drawn::Bitmaps { .. } => 0,
                }
        }
        Glyphs::Face(face) => face.bytes(),
    };
    size_of::<FontData>() + map + glyphs
}

/// `glyphs` rounded up to whole chunks of 32.
const fn chunked(glyphs: usize) -> usize {
    glyphs.div_ceil(CHUNK) * CHUNK
}

#[cfg(test)]
mod tests;
