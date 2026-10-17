//! A TIFF-structured block's directories as a structure report reads them:
//! the EXIF block a JPEG, PNG or WEBP file carries, and a TIFF file's own
//! pages. Each tag is named, and its value is rendered as a person reads it —
//! an exposure as a fraction of a second, a position as degrees, an
//! enumeration by its meaning — with the raw value kept beside the meaning.
//!
//! Advisory metadata, walked tolerantly: an entry list cut short is read as
//! far as it goes, a value outside the block is said to be, and a directory
//! is never visited twice, so a block that loops ends rather than repeating.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::ifd::{
    self, Endian, Entry, ASCII, BYTE, DOUBLE, ENTRY_LEN, FLOAT, LONG, MAGIC, RATIONAL, SBYTE,
    SHORT, SLONG, SRATIONAL, SSHORT, UNDEFINED,
};
use crate::inspect::{clean, hex, Builder};

/// Which directory a tag is read in: the same number names different tags in
/// each.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Directory {
    /// A page of a TIFF file, or an EXIF block's main or thumbnail directory.
    Image,
    /// The camera's settings.
    Exif,
    /// Where the picture was taken.
    Gps,
    /// How the file interoperates.
    Interop,
}

impl Directory {
    const fn names(self) -> &'static [(u16, &'static str)] {
        match self {
            Self::Image => IMAGE_TAGS,
            Self::Exif => EXIF_TAGS,
            Self::Gps => GPS_TAGS,
            Self::Interop => INTEROP_TAGS,
        }
    }

    /// What the report calls a directory of this kind.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Image => "Image",
            Self::Exif => "Exif",
            Self::Gps => "GPS",
            Self::Interop => "Interoperability",
        }
    }
}

/// Directories one block may hold. A fixed containment bound: a directory
/// costs six bytes, so a small block can name many, or loop.
const MAX_DIRECTORIES: usize = 64;

/// Entries of one directory a report lists before saying how many it left.
const MAX_ENTRIES_SHOWN: usize = 512;

/// Values of one entry a line shows before `…`.
const MAX_VALUES_SHOWN: usize = 8;

/// Report the EXIF block `tiff` — its bytes from the TIFF header on —
/// `depth` levels in.
pub(crate) fn report(tiff: &[u8], depth: usize, out: &mut Builder) {
    let Some((endian, first)) = header(tiff) else {
        out.fact(depth, format_args!("no TIFF header: not an EXIF block"));
        return;
    };
    out.fact(
        depth,
        format_args!("{} byte order, {} bytes", order_name(endian), tiff.len()),
    );
    walk(
        tiff,
        endian,
        (Directory::Image, first),
        depth,
        &mut ChainNames::Exif,
        out,
    );
}

/// The byte order and first directory a TIFF header names.
pub(crate) fn header(tiff: &[u8]) -> Option<(Endian, usize)> {
    let endian = Endian::of(tiff)?;
    if endian.u16(tiff, 2)? != MAGIC {
        return None;
    }
    Some((endian, usize::try_from(endian.u32(tiff, 4)?).ok()?))
}

/// What a byte order is called.
pub(crate) const fn order_name(endian: Endian) -> &'static str {
    match endian {
        Endian::Little => "Little-endian",
        Endian::Big => "Big-endian",
    }
}

/// What the directories along the main chain are called: in an EXIF block
/// the first is the picture and the second its thumbnail; in a TIFF file each
/// is a page.
pub(crate) enum ChainNames {
    Exif,
    Pages(u32),
}

impl ChainNames {
    fn next(&mut self, index: usize) -> String {
        match self {
            Self::Exif => String::from(if index == 0 { "Image" } else { "Thumbnail" }),
            Self::Pages(count) => {
                *count += 1;
                alloc::format!("Page {count}")
            }
        }
    }
}

/// Walk the directories reachable from `start` — the chain it begins and
/// every directory an entry points to — reporting each: a directory's
/// children before the next in its chain, so what describes a picture reads
/// under it.
pub(crate) fn walk(
    tiff: &[u8],
    endian: Endian,
    start: (Directory, usize),
    depth: usize,
    chain: &mut ChainNames,
    out: &mut Builder,
) {
    let mut pending: VecDeque<(Directory, usize)> = VecDeque::new();
    let mut seen: Vec<usize> = Vec::new();
    let mut main = Some(start.1).filter(|at| *at != 0);
    let mut index = 0;
    while !out.full() {
        let (kind, at, title, on_chain) = if let Some((kind, at)) = pending.pop_front() {
            (kind, at, String::from(kind.title()), false)
        } else if let Some(at) = main.take() {
            let title = chain.next(index);
            index += 1;
            (start.0, at, title, true)
        } else {
            return;
        };
        if seen.contains(&at) {
            out.fact(
                depth,
                format_args!("{title}: the directory at {at:#x} again, not read twice"),
            );
            continue;
        }
        if seen.len() >= MAX_DIRECTORIES {
            out.fact(
                depth,
                format_args!("more than {MAX_DIRECTORIES} directories: the rest are not read"),
            );
            return;
        }
        seen.push(at);
        let next = directory(tiff, endian, (kind, at, &title), depth, out, &mut pending);
        if on_chain {
            main = next.filter(|next| *next != 0);
        }
    }
}

/// Report the directory `kind` at `at`, titled `title`, queueing the
/// directories its entries point to onto `pending`; answers the offset of the
/// next directory in its chain, when it states one.
fn directory(
    tiff: &[u8],
    endian: Endian,
    (kind, at, title): (Directory, usize, &str),
    depth: usize,
    out: &mut Builder,
    pending: &mut VecDeque<(Directory, usize)>,
) -> Option<usize> {
    let Some(count) = endian.u16(tiff, at) else {
        out.fact(
            depth,
            format_args!("{title}: directory at {at:#x} lies past the end"),
        );
        return None;
    };
    let entries = at + 2;
    let held = tiff.len().saturating_sub(entries) / ENTRY_LEN;
    let readable = usize::from(count).min(held);
    out.heading(format_args!(
        "{title} directory at {at:#x}: {count} {}",
        if count == 1 { "entry" } else { "entries" }
    ));
    for slot in 0..readable.min(MAX_ENTRIES_SHOWN) {
        let Some(entry) = ifd::entry(tiff, endian, entries + slot * ENTRY_LEN) else {
            break;
        };
        if let Some(child) = pointed_to(kind, &entry, tiff, endian) {
            pending.push_back(child);
        }
        let mut line = String::new();
        render(&mut line, (kind, &entry), tiff, endian);
        if !out.fact(
            depth + 1,
            format_args!("{}: {line}", tag_name(kind, entry.tag)),
        ) {
            return None;
        }
    }
    if readable > MAX_ENTRIES_SHOWN {
        out.fact(
            depth + 1,
            format_args!("{} more entries not listed", readable - MAX_ENTRIES_SHOWN),
        );
    }
    if readable < usize::from(count) {
        out.fact(
            depth + 1,
            format_args!("{} entries lie past the end", usize::from(count) - readable),
        );
        return None;
    }
    let link = entries + usize::from(count) * ENTRY_LEN;
    endian
        .u32(tiff, link)
        .and_then(|next| usize::try_from(next).ok())
}

/// The directory `entry`, read in a `kind` directory, points to, and where.
fn pointed_to(
    kind: Directory,
    entry: &Entry,
    tiff: &[u8],
    endian: Endian,
) -> Option<(Directory, usize)> {
    let child = match (kind, entry.tag) {
        (Directory::Image, TAG_EXIF_IFD) => Directory::Exif,
        (Directory::Image, TAG_GPS_IFD) => Directory::Gps,
        (Directory::Exif, TAG_INTEROP_IFD) => Directory::Interop,
        _ => return None,
    };
    let at = entry_offset(entry, tiff, endian).filter(|at| *at != 0)?;
    Some((child, at))
}

/// The offset a pointer entry holds.
fn entry_offset(entry: &Entry, tiff: &[u8], endian: Endian) -> Option<usize> {
    let values = entry.values?;
    let raw = match entry.kind {
        LONG | UNDEFINED => endian.u32(tiff, values.at)?,
        SHORT => u32::from(endian.u16(tiff, values.at)?),
        _ => return None,
    };
    usize::try_from(raw).ok()
}

/// What `kind` calls `tag`.
fn tag_name(kind: Directory, tag: u16) -> String {
    kind.names()
        .binary_search_by_key(&tag, |(number, _)| *number)
        .ok()
        .and_then(|at| kind.names().get(at))
        .map_or_else(
            || alloc::format!("Tag {tag:#06x}"),
            |(_, name)| String::from(*name),
        )
}

/// Render `entry`'s value onto `line`.
fn render(line: &mut String, (kind, entry): (Directory, &Entry), tiff: &[u8], endian: Endian) {
    let Some(values) = entry.values else {
        if ifd::type_size(entry.kind).is_none() {
            let _ = write!(
                line,
                "{} values of unknown type {}",
                entry.count, entry.kind
            );
        } else {
            let _ = write!(line, "{} values lying past the end", entry.count);
        }
        return;
    };
    let bytes = tiff.get(values.at..).unwrap_or_default();
    match entry.kind {
        ASCII => {
            let text = bytes
                .get(..usize::try_from(entry.count).unwrap_or(0))
                .unwrap_or_default();
            let text = text.split(|byte| *byte == 0).next().unwrap_or_default();
            let _ = write!(line, "\u{201c}{}\u{201d}", clean(text));
        }
        UNDEFINED => undefined(line, (kind, entry), bytes),
        RATIONAL | SRATIONAL => rationals(line, (kind, entry), tiff, endian, values.at),
        FLOAT | DOUBLE => floats(line, entry, tiff, endian, values.at),
        _ => integers(line, (kind, entry), tiff, endian, values.at),
    }
}

/// An integer entry's values, each with its meaning where the tag gives one.
fn integers(
    line: &mut String,
    (kind, entry): (Directory, &Entry),
    tiff: &[u8],
    endian: Endian,
    at: usize,
) {
    let count = usize::try_from(entry.count).unwrap_or(usize::MAX);
    for index in 0..count.min(MAX_VALUES_SHOWN) {
        if index > 0 {
            line.push_str(", ");
        }
        let Some(value) = integer(entry.kind, tiff, endian, at, index) else {
            line.push('?');
            continue;
        };
        let _ = write!(line, "{value}");
        if count == 1 {
            if let Some(meaning) = meaning(kind, entry.tag, value) {
                let _ = write!(line, " ({meaning})");
            } else if let Some(unit) = integer_unit(kind, entry.tag) {
                let _ = write!(line, " {unit}");
            }
        }
    }
    if count > MAX_VALUES_SHOWN {
        let _ = write!(line, ", … {count} values");
    }
}

/// Element `index` of an integer field of type `kind` at `at`.
fn integer(kind: u16, tiff: &[u8], endian: Endian, at: usize, index: usize) -> Option<i64> {
    Some(match kind {
        BYTE => i64::from(*tiff.get(at + index)?),
        SBYTE => i64::from(i8::from_ne_bytes([*tiff.get(at + index)?])),
        SHORT => i64::from(endian.u16(tiff, at + index * 2)?),
        SSHORT => i64::from(i16::from_ne_bytes(
            endian.u16(tiff, at + index * 2)?.to_ne_bytes(),
        )),
        LONG => i64::from(endian.u32(tiff, at + index * 4)?),
        SLONG => i64::from(i32::from_ne_bytes(
            endian.u32(tiff, at + index * 4)?.to_ne_bytes(),
        )),
        _ => return None,
    })
}

/// Element `index` of a rational field at `at`, as numerator and
/// denominator.
fn rational(kind: u16, tiff: &[u8], endian: Endian, at: usize, index: usize) -> Option<(i64, i64)> {
    let at = at.checked_add(index.checked_mul(8)?)?;
    let (numerator, denominator) = (endian.u32(tiff, at)?, endian.u32(tiff, at + 4)?);
    Some(if kind == SRATIONAL {
        (
            i64::from(i32::from_ne_bytes(numerator.to_ne_bytes())),
            i64::from(i32::from_ne_bytes(denominator.to_ne_bytes())),
        )
    } else {
        (i64::from(numerator), i64::from(denominator))
    })
}

/// A rational's value, or `None` for a zero denominator.
#[allow(clippy::cast_precision_loss)] // A displayed approximation: the exact ratio is shown beside it.
fn ratio((numerator, denominator): (i64, i64)) -> Option<f64> {
    (denominator != 0).then(|| numerator as f64 / denominator as f64)
}

/// A rational entry's values, in the unit the tag measures in where it has
/// one.
fn rationals(
    line: &mut String,
    (kind, entry): (Directory, &Entry),
    tiff: &[u8],
    endian: Endian,
    at: usize,
) {
    let count = usize::try_from(entry.count).unwrap_or(usize::MAX);
    let read = |index| rational(entry.kind, tiff, endian, at, index);
    if count == 3
        && kind == Directory::Gps
        && matches!(entry.tag, 0x0002 | 0x0004 | 0x0014 | 0x0016)
    {
        if let (Some(d), Some(m), Some(s)) = (
            read(0).and_then(ratio),
            read(1).and_then(ratio),
            read(2).and_then(ratio),
        ) {
            let _ = write!(line, "{d:.0}\u{b0} {m:.0}\u{2032} {s:.2}\u{2033}");
            return;
        }
    }
    if count == 3 && kind == Directory::Gps && entry.tag == 0x0007 {
        if let (Some(h), Some(m), Some(s)) = (
            read(0).and_then(ratio),
            read(1).and_then(ratio),
            read(2).and_then(ratio),
        ) {
            let _ = write!(line, "{h:02.0}:{m:02.0}:{s:05.2} UTC");
            return;
        }
    }
    for index in 0..count.min(MAX_VALUES_SHOWN) {
        if index > 0 {
            line.push_str(", ");
        }
        let Some(value) = read(index) else {
            line.push('?');
            continue;
        };
        if count == 1 {
            if let Some(shown) = measured(kind, entry.tag, value) {
                line.push_str(&shown);
                continue;
            }
        }
        let _ = write!(line, "{}/{}", value.0, value.1);
    }
    if count > MAX_VALUES_SHOWN {
        let _ = write!(line, ", … {count} values");
    }
}

/// A single rational of `tag` in its own unit, or `None` for a tag with no
/// unit of its own.
fn measured(kind: Directory, tag: u16, value: (i64, i64)) -> Option<String> {
    let number = ratio(value)?;
    let raw = alloc::format!("{}/{}", value.0, value.1);
    Some(match (kind, tag) {
        (Directory::Exif, 0x829A) if value.0 == 1 => alloc::format!("1/{} s", value.1),
        (Directory::Exif, 0x829A) => alloc::format!("{number} s ({raw})"),
        (Directory::Exif, 0x829D) => alloc::format!("f/{number:.1} ({raw})"),
        (Directory::Exif, 0x920A) => alloc::format!("{number:.1} mm ({raw})"),
        (Directory::Exif, 0x9204) => alloc::format!("{number:+.2} EV ({raw})"),
        (Directory::Exif, 0x9206) => alloc::format!("{number:.2} m ({raw})"),
        (Directory::Exif, 0xA404) => alloc::format!("{number:.2}\u{d7} ({raw})"),
        (Directory::Gps, 0x0006) => alloc::format!("{number:.1} m ({raw})"),
        (Directory::Gps, 0x000D | 0x000F | 0x0011 | 0x0018 | 0x001A) => {
            alloc::format!("{number:.2} ({raw})")
        }
        (Directory::Image, 0x011A | 0x011B) | (Directory::Exif, 0xA20E | 0xA20F) => {
            alloc::format!("{number} ({raw})")
        }
        _ => return None,
    })
}

/// A floating-point entry's values.
fn floats(line: &mut String, entry: &Entry, tiff: &[u8], endian: Endian, at: usize) {
    let count = usize::try_from(entry.count).unwrap_or(usize::MAX);
    for index in 0..count.min(MAX_VALUES_SHOWN) {
        if index > 0 {
            line.push_str(", ");
        }
        let value = if entry.kind == FLOAT {
            endian
                .u32(tiff, at + index * 4)
                .map(|bits| f64::from(f32::from_bits(bits)))
        } else {
            endian.u64(tiff, at + index * 8).map(f64::from_bits)
        };
        match value {
            Some(value) => {
                let _ = write!(line, "{value}");
            }
            None => line.push('?'),
        }
    }
    if count > MAX_VALUES_SHOWN {
        let _ = write!(line, ", … {count} values");
    }
}

/// An `UNDEFINED` entry: a version or a text where the tag says it is one,
/// else its bytes.
fn undefined(line: &mut String, (kind, entry): (Directory, &Entry), bytes: &[u8]) {
    let held = bytes
        .get(..usize::try_from(entry.count).unwrap_or(usize::MAX))
        .unwrap_or(bytes);
    match (kind, entry.tag) {
        (Directory::Exif, 0x9000 | 0xA000) | (Directory::Interop, 0x0002) => {
            let _ = write!(line, "\u{201c}{}\u{201d}", clean(held));
        }
        (Directory::Exif, 0x9101) => {
            for (index, component) in held.iter().take(4).enumerate() {
                if index > 0 {
                    line.push(' ');
                }
                line.push_str(match component {
                    0 => "-",
                    1 => "Y",
                    2 => "Cb",
                    3 => "Cr",
                    4 => "R",
                    5 => "G",
                    6 => "B",
                    _ => "?",
                });
            }
        }
        (Directory::Exif, 0x9286) | (Directory::Gps, 0x001B | 0x001C) => {
            let (charset, text) = held.split_at(held.len().min(8));
            let charset = charset.split(|byte| *byte == 0).next().unwrap_or_default();
            let text = text.split(|byte| *byte == 0).next().unwrap_or_default();
            let _ = write!(line, "{} \u{201c}{}\u{201d}", clean(charset), clean(text));
        }
        (Directory::Exif, 0xA300) if held.first() == Some(&3) => {
            line.push_str("3 (digital camera)");
        }
        (Directory::Exif, 0xA301) if held.first() == Some(&1) => {
            line.push_str("1 (directly photographed)");
        }
        _ => {
            let _ = write!(line, "{} bytes: {}", held.len(), hex(held, 16));
        }
    }
}

/// The unit an integer `tag` counts in, if it has one.
const fn integer_unit(kind: Directory, tag: u16) -> Option<&'static str> {
    match (kind, tag) {
        (Directory::Image, 0x0100 | 0x0101 | 0x0142 | 0x0143)
        | (Directory::Exif, 0xA002 | 0xA003) => Some("pixels"),
        (Directory::Exif, 0xA405) => Some("mm (35 mm equivalent)"),
        _ => None,
    }
}

/// What the single value `value` of `tag` means, where the tag enumerates
/// its values.
fn meaning(kind: Directory, tag: u16, value: i64) -> Option<String> {
    if kind == Directory::Exif && tag == 0x9209 {
        return flash(value);
    }
    if kind == Directory::Exif && tag == 0x8827 {
        return Some(alloc::format!("ISO {value}"));
    }
    let table = MEANINGS
        .iter()
        .find(|(of, number, _)| *of == kind && *number == tag)
        .map(|(_, _, table)| *table)?;
    let value = u32::try_from(value).ok()?;
    table
        .iter()
        .find(|(number, _)| *number == value)
        .map(|(_, meaning)| String::from(*meaning))
}

/// What a `Flash` value's bits say happened.
fn flash(value: i64) -> Option<String> {
    let value = u32::try_from(value).ok()?;
    let mut said = String::from(if value & 1 != 0 {
        "fired"
    } else {
        "did not fire"
    });
    match (value >> 3) & 3 {
        1 => said.push_str(", compulsory"),
        2 => said.push_str(", suppressed"),
        3 => said.push_str(", auto"),
        _ => {}
    }
    match (value >> 1) & 3 {
        2 => said.push_str(", no return detected"),
        3 => said.push_str(", return detected"),
        _ => {}
    }
    if value & 0x20 != 0 {
        said.push_str(", no flash function");
    }
    if value & 0x40 != 0 {
        said.push_str(", red-eye reduction");
    }
    Some(said)
}

/// The tags that point to another directory.
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
const TAG_INTEROP_IFD: u16 = 0xA005;

/// The tags of a TIFF page and an EXIF block's image directories, by number.
const IMAGE_TAGS: &[(u16, &str)] = &[
    (0x000B, "ProcessingSoftware"),
    (0x00FE, "NewSubfileType"),
    (0x00FF, "SubfileType"),
    (0x0100, "ImageWidth"),
    (0x0101, "ImageLength"),
    (0x0102, "BitsPerSample"),
    (0x0103, "Compression"),
    (0x0106, "PhotometricInterpretation"),
    (0x0107, "Thresholding"),
    (0x010A, "FillOrder"),
    (0x010D, "DocumentName"),
    (0x010E, "ImageDescription"),
    (0x010F, "Make"),
    (0x0110, "Model"),
    (0x0111, "StripOffsets"),
    (0x0112, "Orientation"),
    (0x0115, "SamplesPerPixel"),
    (0x0116, "RowsPerStrip"),
    (0x0117, "StripByteCounts"),
    (0x0118, "MinSampleValue"),
    (0x0119, "MaxSampleValue"),
    (0x011A, "XResolution"),
    (0x011B, "YResolution"),
    (0x011C, "PlanarConfiguration"),
    (0x011D, "PageName"),
    (0x011E, "XPosition"),
    (0x011F, "YPosition"),
    (0x0122, "GrayResponseUnit"),
    (0x0123, "GrayResponseCurve"),
    (0x0124, "T4Options"),
    (0x0125, "T6Options"),
    (0x0128, "ResolutionUnit"),
    (0x0129, "PageNumber"),
    (0x012D, "TransferFunction"),
    (0x0131, "Software"),
    (0x0132, "DateTime"),
    (0x013B, "Artist"),
    (0x013C, "HostComputer"),
    (0x013D, "Predictor"),
    (0x013E, "WhitePoint"),
    (0x013F, "PrimaryChromaticities"),
    (0x0140, "ColorMap"),
    (0x0141, "HalftoneHints"),
    (0x0142, "TileWidth"),
    (0x0143, "TileLength"),
    (0x0144, "TileOffsets"),
    (0x0145, "TileByteCounts"),
    (0x014A, "SubIFDs"),
    (0x014C, "InkSet"),
    (0x0151, "TargetPrinter"),
    (0x0152, "ExtraSamples"),
    (0x0153, "SampleFormat"),
    (0x0154, "SMinSampleValue"),
    (0x0155, "SMaxSampleValue"),
    (0x015B, "JPEGTables"),
    (0x0201, "JPEGInterchangeFormat"),
    (0x0202, "JPEGInterchangeFormatLength"),
    (0x0211, "YCbCrCoefficients"),
    (0x0212, "YCbCrSubSampling"),
    (0x0213, "YCbCrPositioning"),
    (0x0214, "ReferenceBlackWhite"),
    (0x02BC, "XMLPacket"),
    (0x4746, "Rating"),
    (0x4749, "RatingPercent"),
    (0x8298, "Copyright"),
    (0x83BB, "IPTC-NAA"),
    (0x8649, "PhotoshopSettings"),
    (TAG_EXIF_IFD, "ExifIFDPointer"),
    (0x8773, "InterColorProfile"),
    (TAG_GPS_IFD, "GPSInfoIFDPointer"),
    (0x9C9B, "XPTitle"),
    (0x9C9C, "XPComment"),
    (0x9C9D, "XPAuthor"),
    (0x9C9E, "XPKeywords"),
    (0x9C9F, "XPSubject"),
    (0xA480, "GDALMetadata"),
    (0xC4A5, "PrintImageMatching"),
    (0xC612, "DNGVersion"),
    (0xEA1C, "Padding"),
];

/// The tags of an EXIF block's camera-settings directory, by number.
const EXIF_TAGS: &[(u16, &str)] = &[
    (0x829A, "ExposureTime"),
    (0x829D, "FNumber"),
    (0x8822, "ExposureProgram"),
    (0x8824, "SpectralSensitivity"),
    (0x8827, "PhotographicSensitivity"),
    (0x8828, "OECF"),
    (0x8830, "SensitivityType"),
    (0x8831, "StandardOutputSensitivity"),
    (0x8832, "RecommendedExposureIndex"),
    (0x8833, "ISOSpeed"),
    (0x9000, "ExifVersion"),
    (0x9003, "DateTimeOriginal"),
    (0x9004, "DateTimeDigitized"),
    (0x9010, "OffsetTime"),
    (0x9011, "OffsetTimeOriginal"),
    (0x9012, "OffsetTimeDigitized"),
    (0x9101, "ComponentsConfiguration"),
    (0x9102, "CompressedBitsPerPixel"),
    (0x9201, "ShutterSpeedValue"),
    (0x9202, "ApertureValue"),
    (0x9203, "BrightnessValue"),
    (0x9204, "ExposureBiasValue"),
    (0x9205, "MaxApertureValue"),
    (0x9206, "SubjectDistance"),
    (0x9207, "MeteringMode"),
    (0x9208, "LightSource"),
    (0x9209, "Flash"),
    (0x920A, "FocalLength"),
    (0x9214, "SubjectArea"),
    (0x927C, "MakerNote"),
    (0x9286, "UserComment"),
    (0x9290, "SubSecTime"),
    (0x9291, "SubSecTimeOriginal"),
    (0x9292, "SubSecTimeDigitized"),
    (0x9400, "Temperature"),
    (0x9401, "Humidity"),
    (0x9402, "Pressure"),
    (0x9403, "WaterDepth"),
    (0x9404, "Acceleration"),
    (0x9405, "CameraElevationAngle"),
    (0xA000, "FlashpixVersion"),
    (0xA001, "ColorSpace"),
    (0xA002, "PixelXDimension"),
    (0xA003, "PixelYDimension"),
    (0xA004, "RelatedSoundFile"),
    (TAG_INTEROP_IFD, "InteroperabilityIFDPointer"),
    (0xA20B, "FlashEnergy"),
    (0xA20C, "SpatialFrequencyResponse"),
    (0xA20E, "FocalPlaneXResolution"),
    (0xA20F, "FocalPlaneYResolution"),
    (0xA210, "FocalPlaneResolutionUnit"),
    (0xA214, "SubjectLocation"),
    (0xA215, "ExposureIndex"),
    (0xA217, "SensingMethod"),
    (0xA300, "FileSource"),
    (0xA301, "SceneType"),
    (0xA302, "CFAPattern"),
    (0xA401, "CustomRendered"),
    (0xA402, "ExposureMode"),
    (0xA403, "WhiteBalance"),
    (0xA404, "DigitalZoomRatio"),
    (0xA405, "FocalLengthIn35mmFilm"),
    (0xA406, "SceneCaptureType"),
    (0xA407, "GainControl"),
    (0xA408, "Contrast"),
    (0xA409, "Saturation"),
    (0xA40A, "Sharpness"),
    (0xA40B, "DeviceSettingDescription"),
    (0xA40C, "SubjectDistanceRange"),
    (0xA420, "ImageUniqueID"),
    (0xA430, "CameraOwnerName"),
    (0xA431, "BodySerialNumber"),
    (0xA432, "LensSpecification"),
    (0xA433, "LensMake"),
    (0xA434, "LensModel"),
    (0xA435, "LensSerialNumber"),
    (0xA460, "CompositeImage"),
    (0xA500, "Gamma"),
];

/// The tags of an EXIF block's position directory, by number.
const GPS_TAGS: &[(u16, &str)] = &[
    (0x0000, "GPSVersionID"),
    (0x0001, "GPSLatitudeRef"),
    (0x0002, "GPSLatitude"),
    (0x0003, "GPSLongitudeRef"),
    (0x0004, "GPSLongitude"),
    (0x0005, "GPSAltitudeRef"),
    (0x0006, "GPSAltitude"),
    (0x0007, "GPSTimeStamp"),
    (0x0008, "GPSSatellites"),
    (0x0009, "GPSStatus"),
    (0x000A, "GPSMeasureMode"),
    (0x000B, "GPSDOP"),
    (0x000C, "GPSSpeedRef"),
    (0x000D, "GPSSpeed"),
    (0x000E, "GPSTrackRef"),
    (0x000F, "GPSTrack"),
    (0x0010, "GPSImgDirectionRef"),
    (0x0011, "GPSImgDirection"),
    (0x0012, "GPSMapDatum"),
    (0x0013, "GPSDestLatitudeRef"),
    (0x0014, "GPSDestLatitude"),
    (0x0015, "GPSDestLongitudeRef"),
    (0x0016, "GPSDestLongitude"),
    (0x0017, "GPSDestBearingRef"),
    (0x0018, "GPSDestBearing"),
    (0x0019, "GPSDestDistanceRef"),
    (0x001A, "GPSDestDistance"),
    (0x001B, "GPSProcessingMethod"),
    (0x001C, "GPSAreaInformation"),
    (0x001D, "GPSDateStamp"),
    (0x001E, "GPSDifferential"),
    (0x001F, "GPSHPositioningError"),
];

/// The tags of an EXIF block's interoperability directory, by number.
const INTEROP_TAGS: &[(u16, &str)] = &[
    (0x0001, "InteroperabilityIndex"),
    (0x0002, "InteroperabilityVersion"),
    (0x1000, "RelatedImageFileFormat"),
    (0x1001, "RelatedImageWidth"),
    (0x1002, "RelatedImageLength"),
];

/// What each value of an enumerating tag means.
type Meanings = &'static [(u32, &'static str)];

/// The tags whose values enumerate meanings, and what each value means.
const MEANINGS: &[(Directory, u16, Meanings)] = &[
    (Directory::Image, 0x0103, COMPRESSIONS),
    (Directory::Image, 0x0106, PHOTOMETRICS),
    (Directory::Image, 0x0112, ORIENTATIONS),
    (Directory::Image, 0x011C, &[(1, "chunky"), (2, "planar")]),
    (Directory::Image, 0x0128, RESOLUTION_UNITS),
    (Directory::Image, 0x0213, &[(1, "centred"), (2, "co-sited")]),
    (Directory::Exif, 0x8822, EXPOSURE_PROGRAMS),
    (Directory::Exif, 0x9207, METERING_MODES),
    (Directory::Exif, 0x9208, LIGHT_SOURCES),
    (
        Directory::Exif,
        0xA001,
        &[(1, "sRGB"), (0xFFFF, "uncalibrated")],
    ),
    (Directory::Exif, 0xA210, RESOLUTION_UNITS),
    (Directory::Exif, 0xA217, SENSING_METHODS),
    (Directory::Exif, 0xA401, &[(0, "normal"), (1, "custom")]),
    (
        Directory::Exif,
        0xA402,
        &[(0, "auto"), (1, "manual"), (2, "auto bracket")],
    ),
    (Directory::Exif, 0xA403, &[(0, "auto"), (1, "manual")]),
    (
        Directory::Exif,
        0xA406,
        &[
            (0, "standard"),
            (1, "landscape"),
            (2, "portrait"),
            (3, "night"),
        ],
    ),
    (Directory::Exif, 0xA407, GAIN_CONTROLS),
    (Directory::Exif, 0xA408, SOFT_HARD),
    (
        Directory::Exif,
        0xA409,
        &[(0, "normal"), (1, "low"), (2, "high")],
    ),
    (Directory::Exif, 0xA40A, SOFT_HARD),
    (
        Directory::Exif,
        0xA40C,
        &[(0, "unknown"), (1, "macro"), (2, "close"), (3, "distant")],
    ),
    (
        Directory::Gps,
        0x0005,
        &[(0, "above sea level"), (1, "below sea level")],
    ),
];

const COMPRESSIONS: &[(u32, &str)] = &[
    (1, "none"),
    (2, "CCITT modified Huffman"),
    (3, "CCITT group 3"),
    (4, "CCITT group 4"),
    (5, "LZW"),
    (6, "old-style JPEG"),
    (7, "JPEG"),
    (8, "Adobe deflate"),
    (32773, "PackBits"),
    (32946, "deflate"),
];

const PHOTOMETRICS: &[(u32, &str)] = &[
    (0, "white is zero"),
    (1, "black is zero"),
    (2, "RGB"),
    (3, "palette"),
    (4, "transparency mask"),
    (5, "CMYK"),
    (6, "YCbCr"),
    (8, "CIE L*a*b*"),
];

const ORIENTATIONS: &[(u32, &str)] = &[
    (1, "upright"),
    (2, "mirrored left to right"),
    (3, "upside down"),
    (4, "mirrored top to bottom"),
    (5, "mirrored, turned a quarter anticlockwise"),
    (6, "turned a quarter clockwise"),
    (7, "mirrored, turned a quarter clockwise"),
    (8, "turned a quarter anticlockwise"),
];

const RESOLUTION_UNITS: &[(u32, &str)] = &[(1, "no unit"), (2, "inches"), (3, "centimetres")];

const EXPOSURE_PROGRAMS: &[(u32, &str)] = &[
    (0, "not defined"),
    (1, "manual"),
    (2, "normal program"),
    (3, "aperture priority"),
    (4, "shutter priority"),
    (5, "creative program"),
    (6, "action program"),
    (7, "portrait mode"),
    (8, "landscape mode"),
];

const METERING_MODES: &[(u32, &str)] = &[
    (0, "unknown"),
    (1, "average"),
    (2, "centre-weighted average"),
    (3, "spot"),
    (4, "multi-spot"),
    (5, "pattern"),
    (6, "partial"),
    (255, "other"),
];

const LIGHT_SOURCES: &[(u32, &str)] = &[
    (0, "unknown"),
    (1, "daylight"),
    (2, "fluorescent"),
    (3, "tungsten"),
    (4, "flash"),
    (9, "fine weather"),
    (10, "cloudy weather"),
    (11, "shade"),
    (12, "daylight fluorescent"),
    (13, "day white fluorescent"),
    (14, "cool white fluorescent"),
    (15, "white fluorescent"),
    (16, "warm white fluorescent"),
    (17, "standard light A"),
    (18, "standard light B"),
    (19, "standard light C"),
    (20, "D55"),
    (21, "D65"),
    (22, "D75"),
    (23, "D50"),
    (24, "ISO studio tungsten"),
    (255, "other"),
];

const SENSING_METHODS: &[(u32, &str)] = &[
    (1, "not defined"),
    (2, "one-chip colour area"),
    (3, "two-chip colour area"),
    (4, "three-chip colour area"),
    (5, "colour sequential area"),
    (7, "trilinear"),
    (8, "colour sequential linear"),
];

const GAIN_CONTROLS: &[(u32, &str)] = &[
    (0, "none"),
    (1, "low gain up"),
    (2, "high gain up"),
    (3, "low gain down"),
    (4, "high gain down"),
];

const SOFT_HARD: &[(u32, &str)] = &[(0, "normal"), (1, "soft"), (2, "hard")];

#[cfg(test)]
#[path = "exif_tests.rs"]
pub(crate) mod tests;
