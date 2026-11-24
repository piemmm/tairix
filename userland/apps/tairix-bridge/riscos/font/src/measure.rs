//! The measuring calls: `Font_ScanString`, the calls RISC OS keeps from
//! before it — `StringWidth`, `FindCaret`, `FindCaretJ` and `StringBBox` —
//! made on the same walk, `CharBBox`, `ReadInfo` and `ReadFontMetrics`,
//! which read a font's measures, `EnumerateCharacters`, and the colours a
//! walk leaves for `FutureFont` and `FutureRGB`.

use alloc::vec::Vec;

use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};

use crate::colours::Colours;
use crate::fontdata::{Codes, FontData};
use crate::layout::{self, Form, Limit, Place, Request, Walked};
use crate::metrics;
use crate::module::{error, raise, with};
use crate::resolve::{self, Font};
use crate::settings;
use crate::transform::{scaled, scaled_box, Transform};
use crate::units;

/// `Font_ScanString`'s own flags, beside those it shares with `Font_Paint`.
mod flag {
    pub(super) use crate::layout::flag::{BLOCK, OS_UNITS};
    use crate::layout::flag::{HALFWORDS, HANDLE, KERN, LENGTH, MATRIX, RIGHT_TO_LEFT, WORDS};

    pub(super) const CARET: u32 = 1 << 17;
    pub(super) const BBOX: u32 = 1 << 18;
    pub(super) const MATRIX_OUT: u32 = 1 << 19;
    pub(super) const COUNT: u32 = 1 << 20;
    pub(super) const KNOWN: u32 = BLOCK
        | MATRIX
        | LENGTH
        | HANDLE
        | KERN
        | RIGHT_TO_LEFT
        | HALFWORDS
        | WORDS
        | CARET
        | BBOX
        | MATRIX_OUT
        | COUNT;
}

/// What a split character of none is.
const NO_SPLIT: u32 = u32::MAX;
/// The box `Font_ScanString` returns for a string it placed nothing of.
const NOTHING: [i32; 4] = [1 << 29, 1 << 29, -(1 << 29), -(1 << 29)];
/// The pixels round a character's box `Font_CharBBox` gives in OS units.
const MARGIN: i32 = 2;

/// The current font, as the old calls take it.
fn current_font(os: &mut Os<'_>) -> Result<Font, Fail> {
    let handle = resolve::current(os)?;
    resolve::font(os, handle)
}

/// The font and colours a walk starts from.
fn starting(os: &mut Os<'_>) -> Result<Colours, Fail> {
    with(os, |state| state.current)
}

/// The colours a walk left, for `Font_FutureFont` and `Font_FutureRGB`.
fn leave(os: &mut Os<'_>, place: &Place) -> Result<(), Fail> {
    with(os, |state| state.future = place.colours)
}

/// `Font_ScanString`.
pub(crate) fn scan_string(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let flags = regs.r[2];
    if flags & !flag::KNOWN != 0 || (flags & flag::BBOX != 0 && flags & flag::BLOCK == 0) {
        return Err(raise(os, error::RESERVED, &[]));
    }
    let (font, mut request) = layout::requested(os, regs)?;
    if flags & flag::BLOCK != 0 {
        let split = os.read_word(regs.r[5].wrapping_add(16))?;
        request.split = (split != NO_SPLIT).then_some(split);
    }
    let point = (regs.r[3].cast_signed(), regs.r[4].cast_signed());
    request.limit = if flags & flag::CARET != 0 {
        Limit::Caret(point.0, point.1)
    } else {
        Limit::Signed(point.0, point.1)
    };
    let colours = starting(os)?;
    let Walked { stop, .. } = layout::walk(os, &request, font, colours, &mut layout::Measuring)?;
    if flags & flag::BBOX != 0 {
        let bbox = stop.bbox.map_or(NOTHING, |b| [b.x0, b.y0, b.x1, b.y1]);
        write_words(
            os,
            regs.r[5].wrapping_add(20),
            &bbox.map(i32::cast_unsigned),
        )?;
    }
    if flags & flag::MATRIX_OUT != 0 {
        let words = stop
            .matrix
            .map_or(Transform::IDENTITY, Transform::of)
            .words();
        write_words(os, regs.r[6], &words)?;
    }
    leave(os, &stop)?;
    regs.r[1] = regs.r[1].wrapping_add(stop.offset);
    regs.r[3] = stop.pen.0.cast_unsigned();
    regs.r[4] = stop.pen.1.cast_unsigned();
    if flags & flag::COUNT != 0 {
        regs.r[7] = stop.count;
    }
    Ok(())
}

fn write_words(os: &mut Os<'_>, at: u32, words: &[u32]) -> Result<(), Fail> {
    for (i, &word) in (0u32..).zip(words) {
        os.write_word(at.wrapping_add(4 * i), word)?;
    }
    Ok(())
}

/// What the old calls walk: the current font, unkerned, no matrix, a byte
/// string, to `limit`, each space moved `space` besides.
fn old_walk(
    os: &mut Os<'_>,
    at: u32,
    limit: Limit,
    split: Option<u32>,
    length: Option<u32>,
    space: (i32, i32),
) -> Result<Walked, Fail> {
    let font = current_font(os)?;
    let request = Request {
        at,
        form: Form::Bytes,
        length,
        kern: false,
        right_to_left: false,
        limit,
        split,
        space,
        letter: (0, 0),
        matrix: None,
    };
    let colours = starting(os)?;
    let walked = layout::walk(os, &request, font, colours, &mut layout::Measuring)?;
    leave(os, &walked.stop)?;
    Ok(walked)
}

/// The old calls' answer: R1 at the stop, R2 and R3 its offset, R4 its
/// count and R5 its index.
fn answer(regs: &mut Regs, place: &Place) {
    regs.r[1] = regs.r[1].wrapping_add(place.offset);
    regs.r[2] = place.pen.0.cast_unsigned();
    regs.r[3] = place.pen.1.cast_unsigned();
    regs.r[4] = place.count;
    regs.r[5] = place.offset;
}

/// `Font_StringWidth`: R2 and R3 the most the offset may grow to, R4 the
/// split character, R5 the most characters.
pub(crate) fn string_width(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let limit = Limit::Greater(regs.r[2].cast_signed(), regs.r[3].cast_signed());
    let split = (regs.r[4] != NO_SPLIT).then_some(regs.r[4]);
    let walked = old_walk(os, regs.r[1], limit, split, Some(regs.r[5]), (0, 0))?;
    answer(regs, &walked.stop);
    Ok(())
}

/// `Font_FindCaret`, and `Font_FindCaretJ` with R4 what each space moves
/// besides: the string measured to the point R2 and R3 give, and the place
/// there or past the next step, whichever is nearer.
pub(crate) fn find_caret(os: &mut Os<'_>, regs: &mut Regs, justified: bool) -> Result<(), Fail> {
    let point = (regs.r[2].cast_signed(), regs.r[3].cast_signed());
    let space = if justified {
        (regs.r[4].cast_signed(), 0)
    } else {
        (0, 0)
    };
    let walked = old_walk(
        os,
        regs.r[1],
        Limit::Greater(point.0, point.1),
        None,
        None,
        space,
    )?;
    let nearer = match walked.past {
        Some(past) if layout::distance(point, &past) < layout::distance(point, &walked.stop) => {
            past
        }
        _ => walked.stop,
    };
    answer(regs, &nearer);
    Ok(())
}

/// `Font_StringBBox`: the box of everything the string places, nought for
/// a string that places nothing.
pub(crate) fn string_bbox(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let walked = old_walk(os, regs.r[1], Limit::None, None, None, (0, 0))?;
    let bbox = walked.stop.bbox.unwrap_or_default();
    regs.r[1..5].copy_from_slice(&[bbox.x0, bbox.y0, bbox.x1, bbox.y1].map(i32::cast_unsigned));
    Ok(())
}

/// `font`'s millipoint box `bbox` in OS units: in its pixels, each edge
/// rounded to the nearest with halves towards nought, `margin` pixels
/// further out, in the current mode's OS units.
fn in_os_units(
    os: &mut Os<'_>,
    font: &Font,
    bbox: metrics::Box,
    margin: i32,
) -> Result<[i32; 4], Fail> {
    let (eigen_x, eigen_y) = (settings::eigen_x(os)?, settings::eigen_y(os)?);
    let pixel = |millipoints: i32, resolution: i32| {
        let value = i64::from(millipoints) * i64::from(resolution);
        let n = value.unsigned_abs();
        let d = u64::from(units::MILLIPOINTS_AN_INCH.unsigned_abs());
        let rounded = (2 * n + d).saturating_sub(1) / (2 * d);
        let rounded = i64::try_from(rounded).unwrap_or(i64::MAX);
        i32::try_from(if value < 0 { -rounded } else { rounded }).unwrap_or(0)
    };
    let os_units = |pixels: i32, eigen: u32| pixels.wrapping_shl(eigen);
    Ok([
        os_units(
            pixel(bbox.x0, font.resolution.0).wrapping_sub(margin),
            eigen_x,
        ),
        os_units(
            pixel(bbox.y0, font.resolution.1).wrapping_sub(margin),
            eigen_y,
        ),
        os_units(
            pixel(bbox.x1, font.resolution.0).wrapping_add(margin),
            eigen_x,
        ),
        os_units(
            pixel(bbox.y1, font.resolution.1).wrapping_add(margin),
            eigen_y,
        ),
    ])
}

/// `bbox` in thousandths of an em at `font`'s size, through its matrix.
fn placed(font: &Font, bbox: metrics::Box) -> metrics::Box {
    font.matrix.bbox(scaled_box(bbox, font.points))
}

/// `Font_CharBBox`: R1's character's box in R0's font, in millipoints or,
/// with R2's bit 4, OS units; nought for a character the font lacks.
pub(crate) fn char_bbox(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let font = resolve::font(os, regs.r[0])?;
    let measured = font.data.code(regs.r[1]).and_then(|g| font.data.glyph(g));
    let bbox = match measured {
        None => [0; 4],
        Some(glyph) => {
            let bbox = placed(&font, glyph.bbox);
            if regs.r[2] & flag::OS_UNITS != 0 {
                in_os_units(os, &font, bbox, MARGIN)?
            } else {
                [bbox.x0, bbox.y0, bbox.x1, bbox.y1]
            }
        }
    };
    regs.r[1..5].copy_from_slice(&bbox.map(i32::cast_unsigned));
    Ok(())
}

/// `Font_ReadInfo`: the box holding every character of R0's font, in OS
/// units.
pub(crate) fn read_info(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let font = resolve::font(os, regs.r[0])?;
    let bbox = placed(&font, font.data.bbox);
    let os_units = in_os_units(os, &font, bbox, 0)?;
    regs.r[1..5].copy_from_slice(&os_units.map(i32::cast_unsigned));
    Ok(())
}

/// `Font_EnumerateCharacters`: R1 the character after R1 that the font's
/// encoding maps, R2 the glyph R1's maps to.
pub(crate) fn enumerate_characters(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = match regs.r[0] {
        0 => resolve::current(os)?,
        handle => handle,
    };
    let font = resolve::font(os, handle)?;
    let code = regs.r[1];
    let none = u32::MAX;
    let (next, glyph) = match &font.data.codes {
        Codes::NoBase(missing) => return Err(raise(os, error::NO_ENCODING, &[missing])),
        Codes::Glyph if code < font.data.count => {
            let next = code + 1;
            (if next < font.data.count { next } else { none }, code)
        }
        Codes::Mapped(map) if code < map.len() => (
            map.next(code).unwrap_or(map.len()),
            map.glyph(code).unwrap_or(none),
        ),
        Codes::Glyph | Codes::Mapped(_) => (none, none),
    };
    regs.r[1] = next;
    regs.r[2] = glyph;
    Ok(())
}

/// `Font_ReadFontMetrics`.
pub(crate) fn read_font_metrics(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let font = resolve::font(os, regs.r[0])?;
    if font.transformed {
        return Err(raise(os, error::TRANSFORMED, &[]));
    }
    let tables = metric_tables(&font);
    for (register, table) in regs.r[1..7].iter_mut().zip(&tables) {
        let length = len32(table.len());
        if *register != 0 && length != 0 {
            os.write_bytes(*register, table)?;
        }
        *register = length;
    }
    regs.r[0] = u32::from(font.data.flags);
    Ok(())
}

/// The six tables `Font_ReadFontMetrics` gives, each empty where the font
/// has none: the boxes, the widths across and up, the miscellaneous
/// measures, and the kern pairs in both forms.
fn metric_tables(font: &Font) -> [Vec<u8>; 6] {
    let data = &font.data;
    let points = font.points;
    let measured = |code: u32| data.code(code).and_then(|g| data.glyph(g));
    let mut boxes = Vec::new();
    let mut across = Vec::new();
    let mut up = Vec::new();
    for code in 0..256 {
        let glyph = measured(code);
        let bbox = glyph.map_or_else(metrics::Box::default, |g| scaled_box(g.bbox, points));
        for value in [bbox.x0, bbox.y0, bbox.x1, bbox.y1] {
            boxes.extend_from_slice(&value.to_le_bytes());
        }
        let (x, y) = glyph.map_or((0, 0), |g| {
            (scaled(g.advance.0, points.0), scaled(g.advance.1, points.1))
        });
        across.extend_from_slice(&x.to_le_bytes());
        up.extend_from_slice(&y.to_le_bytes());
    }
    if data.flags & metrics::flag::NO_Y_OFFSETS != 0 {
        up.clear();
    }
    let misc = data
        .misc
        .map_or_else(Vec::new, |misc| misc_table(&misc, points));
    let (old, new) = kern_tables(data, points);
    [boxes, across, up, misc, old, new]
}

/// The miscellaneous measures in millipoints, a word each, the underline's
/// two bytes in one.
fn misc_table(misc: &metrics::Misc, points: (i32, i32)) -> Vec<u8> {
    let across = |v: i32| scaled(v, points.0);
    let up = |v: i32| scaled(v, points.1);
    let underline = u32::from(misc.underline_position.to_le_bytes()[0])
        | u32::from(misc.underline_thickness.to_le_bytes()[0]) << 8;
    let words = [
        across(misc.bbox.x0).cast_unsigned(),
        up(misc.bbox.y0).cast_unsigned(),
        across(misc.bbox.x1).cast_unsigned(),
        up(misc.bbox.y1).cast_unsigned(),
        across(misc.x_offset).cast_unsigned(),
        up(misc.y_offset).cast_unsigned(),
        misc.italic.cast_unsigned(),
        underline,
        up(misc.cap_height).cast_unsigned(),
        up(misc.x_height).cast_unsigned(),
        up(misc.descender).cast_unsigned(),
        up(misc.ascender).cast_unsigned(),
        0,
    ];
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// Words a kern table's hash index has, its end and its flags, and the
/// bytes RISC OS leaves before its pairs.
const HASH_WORDS: usize = 256;
const GAP: usize = 8;
/// The flag of a table whose pairs are a word each.
const SHORT_PAIRS: u32 = 1 << 31;

/// Where kern pair `(first, second)` hashes to: the first EOR the second
/// rotated four bits within its byte.
fn hash(first: u32, second: u32) -> usize {
    let (first, second) = (first.to_le_bytes()[0], second.to_le_bytes()[0]);
    usize::from(first ^ second.rotate_right(4))
}

/// A hash-indexed kern table of `pairs`, each `(first, second, words)`.
fn kern_table(flags: u32, mut pairs: Vec<(u32, u32, Vec<u32>)>) -> Vec<u8> {
    if pairs.is_empty() {
        return Vec::new();
    }
    pairs.sort_by_key(|&(first, second, _)| hash(first, second));
    let header = 4 * (HASH_WORDS + 2) + GAP;
    let mut offsets = [0u32; HASH_WORDS + 1];
    let mut at = header;
    let mut pair = 0;
    for (bucket, offset) in offsets.iter_mut().enumerate().take(HASH_WORDS) {
        *offset = len32(at);
        while pair < pairs.len() && hash(pairs[pair].0, pairs[pair].1) == bucket {
            at += 4 * pairs[pair].2.len();
            pair += 1;
        }
    }
    offsets[HASH_WORDS] = len32(at);
    let mut out: Vec<u8> = offsets.iter().flat_map(|o| o.to_le_bytes()).collect();
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&[0; GAP]);
    for (_, _, words) in &pairs {
        out.extend(words.iter().flat_map(|w| w.to_le_bytes()));
    }
    out
}

/// The kern tables: RISC OS's first form, by each code of the first 256 a
/// pair's glyphs show, in millipoints; and its second, by glyph, in
/// thousandths of an em, a word a pair where they fit one.
fn kern_tables(data: &FontData, points: (i32, i32)) -> (Vec<u8>, Vec<u8>) {
    let kerns = data.kerns();
    let file_flags = u32::from(data.flags);
    let x = data.flags & metrics::flag::NO_X_OFFSETS == 0;
    let y = data.flags & metrics::flag::NO_Y_OFFSETS == 0;
    let mut codes_of: Vec<(u32, u32)> = (0..256)
        .filter_map(|c| data.code(c).map(|g| (g, c)))
        .collect();
    codes_of.sort_unstable();
    let codes = |glyph: u32| {
        let start = codes_of.partition_point(|&(g, _)| g < glyph);
        codes_of[start..]
            .iter()
            .take_while(move |&&(g, _)| g == glyph)
            .map(|&(_, c)| c)
    };
    let mut old = Vec::new();
    for kern in kerns {
        for first in codes(u32::from(kern.left)) {
            for second in codes(u32::from(kern.right)) {
                let mut words = alloc::vec![first];
                if x {
                    words.push(scaled(kern.x, points.0).cast_unsigned());
                }
                if y {
                    words.push(scaled(kern.y, points.1).cast_unsigned());
                }
                old.push((first, second, words));
            }
        }
    }
    let short = (x != y) && kerns.iter().all(|k| k.left < 256 && k.right < 256);
    let new = kerns
        .iter()
        .map(|kern| {
            let (first, second) = (u32::from(kern.left), u32::from(kern.right));
            let low16 = |v: i32| v.cast_unsigned() & 0xFFFF;
            let words = if short {
                let offset = if x { kern.x } else { kern.y };
                alloc::vec![first | second << 8 | low16(offset) << 16]
            } else {
                alloc::vec![first | second << 16, low16(kern.x) | low16(kern.y) << 16]
            };
            (first, second, words)
        })
        .collect();
    let new_flags = file_flags | if short { SHORT_PAIRS } else { 0 };
    (kern_table(file_flags, old), kern_table(new_flags, new))
}

#[cfg(test)]
mod tests;
