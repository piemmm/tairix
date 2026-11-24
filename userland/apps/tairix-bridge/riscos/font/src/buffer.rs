//! `Font_SwitchOutputToBuffer`, and the Draw path objects a paint writes
//! while output is switched, as RISC OS 5.30 writes them: a rub-out box
//! first, an underline's bar ahead of the first character it runs under,
//! and each character's filled outline in the foreground's RGB colour, all
//! in Draw's units from the screen's origin. Stroke paths are not written,
//! and a font with no outlines is refused.
//!
//! Each object is written as it comes, as RISC OS writes it. An outline is
//! copied into as many bytes as the pen's position across after the
//! character is in millipoints, so a character whose step ends at or near
//! the screen's left edge fails "Buffer overflow" with its object part
//! written; and the word after the terminator is the room that copy left.

use alloc::vec::Vec;

use tairix_bezier::{Path as BezierPath, PathEl, Point as BezierPoint};
use tairix_drawing::riscos::format::{self, FIXED_ONE, NO_COLOUR, PATH, WINDING_EVEN_ODD};

use tairix_riscos_kernel::error::BUFFER_OVERFLOW;
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use crate::colours::Colours;
use crate::fontdata::Outline;
use crate::glyphs::saturated;
use crate::grid::{self, clamped, exactly, Axis};
use crate::layout::{self, Placed, Visit};
use crate::module::{error, raise, with};
use crate::outlines::Element;
use crate::paint::Geometry;
use crate::resolve::Font;
use crate::transform::{wrap, Transform};
use crate::units;

/// `Font_SwitchOutputToBuffer`'s flags.
pub(crate) mod output_flag {
    /// The pointer counts what would be written, and nothing is.
    pub(crate) const COUNT: u32 = 1;
    /// Outlines take their hints at the size; TAIRiX's outlines have none
    /// to take.
    pub(crate) const HINTS: u32 = 1 << 1;
    /// A character only bitmaps hold is refused rather than left out.
    pub(crate) const BITMAPS_REFUSED: u32 = 1 << 4;
    pub(crate) const KNOWN: u32 = COUNT | HINTS | BITMAPS_REFUSED;
}

/// R1's value that asks for the output's state and changes nothing, and
/// the one that switches output back to the screen.
const ENQUIRE: u32 = u32::MAX;
const SCREEN: u32 = 0;

/// Where a switched paint writes: its flags, and the buffer's terminator,
/// where the next object goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) flags: u32,
    pub(crate) at: u32,
}

/// `Font_SwitchOutputToBuffer`: R1 a buffer and R0 its flags, nought for
/// the screen, or -1 to ask; R0 and R1 the flags and pointer before, the
/// pointer past what was written.
pub(crate) fn switch_output_to_buffer(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (flags, at) = (regs.r[0], regs.r[1]);
    let to_buffer = at != ENQUIRE && at != SCREEN;
    let allowed = if to_buffer { output_flag::KNOWN } else { 0 };
    if flags & !allowed != 0 {
        return Err(raise(os, error::RESERVED, &[]));
    }
    let before = with(os, |state| {
        let before = state.output;
        match at {
            ENQUIRE => {}
            SCREEN => state.output = None,
            _ => state.output = Some(Output { flags, at }),
        }
        before
    })?;
    let (flags, at) = before.map_or((0, 0), |output| (output.flags, output.at));
    regs.r[0] = flags;
    regs.r[1] = at;
    Ok(())
}

/// Where a path object's colours begin, and its path, which no dash pattern
/// comes before; and a word.
#[expect(clippy::cast_possible_truncation, reason = "within an object's header")]
const COLOURS_AT: u32 = format::PATH_FILL as u32;
#[expect(clippy::cast_possible_truncation, reason = "within an object's header")]
const HEADER_BYTES: u32 = format::PATH_DASHES as u32;
const WORD: u32 = 4;
/// A rectangle's object: its header, a move, three lines, a close and an
/// end.
const RECTANGLE_BYTES: u32 = HEADER_BYTES
    + element(tag::MOVE)
    + 3 * element(tag::LINE)
    + element(tag::CLOSE)
    + element(tag::END);
const RECTANGLE_WORDS: u32 = RECTANGLE_BYTES / WORD;
/// A path's style word filled by the non-zero rule: the even-odd bit clear.
const NON_ZERO: u32 = 0;
/// A path's element tags.
mod tag {
    pub(super) use tairix_drawing::riscos::format::{CLOSE, CURVE, END, LINE, MOVE};
}

/// The bytes element `code` takes, nought for a code no element has.
#[expect(clippy::cast_possible_truncation, reason = "an element's few bytes")]
const fn element(code: u32) -> u32 {
    match format::element_bytes(code) {
        Some(bytes) => bytes as u32,
        None => 0,
    }
}

/// A word of a glyph's path: an element's tag, or a coordinate across or up,
/// both in 512ths of a pixel from the glyph's origin and in Draw's units
/// where the glyph is put.
#[derive(Clone, Copy, Debug)]
enum Word {
    Tag(u32),
    Across(At),
    Up(At),
}

/// A coordinate: from the glyph's origin, and placed.
#[derive(Clone, Copy, Debug)]
struct At {
    relative: i32,
    placed: i32,
}

impl Word {
    /// The word as the copy writes it: a coordinate placed once the path is
    /// whole, else as the glyph holds it.
    const fn value(self, placed: bool) -> u32 {
        match (self, placed) {
            (Self::Tag(tag), _) => tag,
            (Self::Across(at) | Self::Up(at), true) => at.placed.cast_unsigned(),
            (Self::Across(at) | Self::Up(at), false) => at.relative.cast_unsigned(),
        }
    }
}

/// A glyph's path, its last end left for the copy to write, and the box
/// its points make where it is put.
#[derive(Debug, Default)]
struct Glyph {
    words: Vec<Word>,
    bounds: Option<[i32; 4]>,
}

impl Glyph {
    /// Emptied for the next character.
    fn clear(&mut self) {
        self.words.clear();
        self.bounds = None;
    }

    /// Element `tag` through `points`, each across and up.
    fn element(&mut self, tag: u32, points: &[(At, At)]) -> Result<(), Unrecorded> {
        self.words
            .try_reserve(1 + 2 * points.len())
            .map_err(|_| Unrecorded)?;
        self.words.push(Word::Tag(tag));
        for &(x, y) in points {
            self.words.extend([Word::Across(x), Word::Up(y)]);
            let (px, py) = (x.placed, y.placed);
            self.bounds = Some(self.bounds.map_or([px, py, px, py], |[x0, y0, x1, y1]| {
                [x0.min(px), y0.min(py), x1.max(px), y1.max(py)]
            }));
        }
        Ok(())
    }
}

/// The host refused the memory a path needs.
#[derive(Clone, Copy, Debug)]
struct Unrecorded;

/// An underline being set: its left, top and bottom in Draw's units, the
/// colour it was set in, and once a character it runs under has given it
/// one, its bar.
#[derive(Clone, Copy, Debug)]
struct Underline {
    left: i32,
    top: i32,
    bottom: i32,
    fill: u32,
    bar: Option<Bar>,
}

/// An underline's bar: where its object goes, and how far right it runs.
#[derive(Clone, Copy, Debug)]
struct Bar {
    at: u32,
    right: i32,
}

/// A rub-out box's corners, bottom left then top right, in millipoints from
/// the screen's origin.
pub(crate) type Corners = ((i64, i64), (i64, i64));

/// A paint into a buffer under way.
pub(crate) struct Recorder {
    /// Where its millipoints fall; none where the scale gives no pixels.
    geometry: Option<Geometry>,
    counting: bool,
    /// Where the next object goes.
    at: u32,
    /// Whether an object has been put past where the paint began, which
    /// then wants a terminator after the last.
    advanced: bool,
    /// Whether a copy failed, leaving the pointer where RISC OS leaves it.
    failed: bool,
    /// The foreground's RGB colour, `&BBGGRR00`, what is filled in.
    fill: u32,
    underline: Option<Underline>,
    /// The path being made and the bytes being written, kept between
    /// characters.
    glyph: Glyph,
    bytes: Vec<u8>,
}

impl Recorder {
    /// A paint through `geometry` in `colours`, their RGB colours set, into
    /// `output`, behind a rub-out box from `low` to `high` in millipoints
    /// from the screen's origin where there is one: the box's corners the
    /// nearest pixels'.
    pub(crate) fn new(
        os: &mut Os<'_>,
        geometry: Option<Geometry>,
        output: Output,
        colours: &Colours,
        rub_out: Option<Corners>,
    ) -> Result<Self, Fail> {
        let Some((background, foreground, _)) = colours.rgb else {
            return Err(raise(os, error::NO_RGB, &[]));
        };
        let mut recorder = Self {
            geometry,
            counting: output.flags & output_flag::COUNT != 0,
            at: output.at,
            advanced: false,
            failed: false,
            fill: foreground,
            underline: None,
            glyph: Glyph::default(),
            bytes: Vec::new(),
        };
        if let (Some((low, high)), Some(geometry)) = (rub_out, geometry) {
            let corner = |(x, y): (i64, i64)| {
                let pixel = |axis: Axis, at: i64| axis.pixel_draw_units(axis.nearest(at));
                (pixel(geometry.grid.across, x), pixel(geometry.grid.up, y))
            };
            let at = recorder.at;
            recorder.put(os, at, &rectangle(background, corner(low), corner(high)))?;
            recorder.advance(RECTANGLE_BYTES);
        }
        Ok(recorder)
    }

    /// Where the next object goes moved past one of `bytes`.
    fn advance(&mut self, bytes: u32) {
        self.at = self.at.wrapping_add(bytes);
        self.advanced = true;
    }

    /// `words` written at `at`, unless the output only counts.
    fn put(&mut self, os: &mut Os<'_>, at: u32, words: &[u32]) -> Result<(), Fail> {
        self.put_each(os, at, words.iter().copied())
    }

    /// `words` written at `at` in one write, unless the output only counts.
    fn put_each(
        &mut self,
        os: &mut Os<'_>,
        at: u32,
        words: impl IntoIterator<Item = u32>,
    ) -> Result<(), Fail> {
        if self.counting {
            return Ok(());
        }
        self.bytes.clear();
        for word in words {
            if self.bytes.try_reserve(WORD as usize).is_err() {
                return Err(os.no_memory());
            }
            self.bytes.extend_from_slice(&word.to_le_bytes());
        }
        os.write_bytes(at, &self.bytes)
    }

    /// The underline's bar written where it goes, at the extent it reached.
    fn close_bar(&mut self, os: &mut Os<'_>) -> Result<(), Fail> {
        if let Some(Underline {
            left,
            top,
            bottom,
            fill,
            bar: Some(bar),
        }) = self.underline
        {
            let low = (left.min(bar.right), bottom);
            let high = (left.max(bar.right), top);
            self.put(os, bar.at, &rectangle(fill, low, high))?;
        }
        Ok(())
    }

    /// The underline, once a character it runs under steps, given its bar
    /// ahead of the character, and run on to `to`, millipoints from the
    /// pen's start.
    fn underline_to(&mut self, to: i32, character: bool) {
        let (Some(geometry), Some(underline)) = (self.geometry, self.underline.as_mut()) else {
            return;
        };
        if underline.bar.is_none() && character {
            underline.bar = Some(Bar {
                at: self.at,
                right: underline.left,
            });
            self.at = self.at.wrapping_add(RECTANGLE_BYTES);
            self.advanced = true;
        }
        if let Some(bar) = &mut underline.bar {
            bar.right = geometry
                .grid
                .across
                .draw_units_of(geometry.placed((i64::from(to), 0)).0);
        }
    }

    /// Every object finished: the underline's bar written, and where
    /// anything was put, the terminator after the last. The pointer past
    /// them, or where a failed copy left it.
    pub(crate) fn finish(mut self, os: &mut Os<'_>) -> Result<u32, Fail> {
        self.close_bar(os)?;
        if self.advanced && !self.failed {
            let at = self.at;
            self.put(os, at, &[tag::END])?;
        }
        Ok(self.at)
    }

    /// `glyph` written as RISC OS 5.30 writes a character's object, its
    /// outline copied into `room` bytes: the pen's position across after
    /// the character, in millipoints.
    fn write_glyph(
        &mut self,
        os: &mut Os<'_>,
        glyph: &Glyph,
        style: u32,
        room: i32,
    ) -> Result<(), Fail> {
        let object = self.at;
        let path = object.wrapping_add(HEADER_BYTES);
        let words = glyph.words.len();
        if self.counting {
            if words > 0 {
                self.advance(
                    path_end(path, words)
                        .wrapping_sub(object)
                        .wrapping_add(WORD),
                );
            }
            return Ok(());
        }
        let copied = usize::try_from((i64::from(room) / i64::from(WORD)).max(0))
            .map_or(words, |fits| fits.min(words));
        let left = i64::from(room) - i64::from(WORD) * i64::try_from(copied).unwrap_or(i64::MAX);
        let marker = path_end(path, copied);
        let whole = copied == words;
        let colours = [self.fill, NO_COLOUR, 0, style];
        if words > 0 && whole && left >= i64::from(WORD) {
            let size = marker.wrapping_add(WORD).wrapping_sub(object);
            let bounds = glyph.bounds.unwrap_or_default().map(i32::cast_unsigned);
            let header = [PATH, size].into_iter().chain(bounds).chain(colours);
            let path = glyph.words.iter().map(|word| word.value(true));
            let end = [
                tag::END,
                tag::END,
                wrap(left - i64::from(WORD)).cast_unsigned(),
            ];
            self.put_each(os, object, header.chain(path).chain(end))?;
            self.advance(size);
            return Ok(());
        }
        self.put(os, object, &[PATH])?;
        self.put(os, object.wrapping_add(COLOURS_AT), &colours)?;
        let copy = glyph.words[..copied].iter().map(|word| word.value(whole));
        self.put_each(os, path, copy)?;
        self.put(os, marker, &[tag::END, wrap(left).cast_unsigned()])?;
        if words == 0 {
            let discarded = wrap(i64::from(room) + i64::from(HEADER_BYTES)).cast_unsigned();
            return self.put(os, object, &[tag::END, discarded]);
        }
        self.failed = true;
        self.at = if whole {
            marker.wrapping_add(WORD)
        } else {
            path
        };
        Err(os.error(BUFFER_OVERFLOW))
    }
}

/// Where a path of `words` words written from `path` ends.
fn path_end(path: u32, words: usize) -> u32 {
    path.wrapping_add(WORD.wrapping_mul(u32::try_from(words).unwrap_or(u32::MAX)))
}

/// The rectangle object with corners `low` and `high`, bottom left first,
/// filled in `fill` by the non-zero rule.
fn rectangle(fill: u32, low: (i32, i32), high: (i32, i32)) -> [u32; RECTANGLE_WORDS as usize] {
    let [x0, y0, x1, y1] = [low.0, low.1, high.0, high.1].map(i32::cast_unsigned);
    let bounds = [
        low.0.min(high.0),
        low.1.min(high.1),
        low.0.max(high.0),
        low.1.max(high.1),
    ]
    .map(i32::cast_unsigned);
    [
        PATH,
        RECTANGLE_BYTES,
        bounds[0],
        bounds[1],
        bounds[2],
        bounds[3],
        fill,
        NO_COLOUR,
        0,
        NON_ZERO,
        tag::MOVE,
        x0,
        y0,
        tag::LINE,
        x1,
        y0,
        tag::LINE,
        x1,
        y1,
        tag::LINE,
        x0,
        y1,
        tag::CLOSE,
        tag::END,
    ]
}

/// What takes a glyph of `font`'s outline, in its own units, through
/// `transform` to 512ths of the pixels of `resolution` it is drawn in, a
/// pixel to a pixel of the screen: the matrix's multipliers times the size
/// and the resolution, each rounded to a 65536th, as RISC OS 5.30 rounds
/// them.
fn composite(
    font: &Font,
    transform: &Transform,
    outline: &Outline<'_>,
    resolution: (i32, i32),
) -> [i64; 4] {
    let design = match outline {
        Outline::Design { design_size, .. } => f64::from(*design_size),
        Outline::Face(_) => f64::from(units::MILLEMS),
    };
    let size = |sixteenths: i32| {
        f64::from(sixteenths) * f64::from(units::MILLIPOINTS)
            / f64::from(units::SIXTEENTHS)
            / design
    };
    let screen = (
        grid::parts_a_millipoint(resolution.0),
        grid::parts_a_millipoint(resolution.1),
    );
    transform.multiplied((size(font.points.0), size(font.points.1)), screen)
}

impl Visit for Recorder {
    fn character(
        &mut self,
        os: &mut Os<'_>,
        font: &Font,
        transform: &Transform,
        placed: &Placed,
        _: &Colours,
    ) -> Result<(), Fail> {
        self.underline_to(placed.to.0, true);
        let (Some(glyph), Some(geometry)) = (placed.glyph, self.geometry) else {
            return Ok(());
        };
        if !font.data.has_outlines() {
            return Err(raise(os, error::NO_DATA, &[&font.data.identifier]));
        }
        let Some(outline) = font.data.outline(glyph) else {
            return Ok(());
        };
        let grid = geometry.grid;
        let multipliers = composite(font, transform, &outline, grid.resolution(font.resolution));
        let quarters = geometry.quarters((i64::from(placed.origin.0), i64::from(placed.origin.1)));
        let translation = transform.translation();
        let origin = |axis: Axis, quarter: i64, by: i64| {
            Axis::quarter_parts(quarter).saturating_add(axis.parts(by))
        };
        let origin = (
            origin(grid.across, quarters.0, translation.0),
            origin(grid.up, quarters.1, translation.1),
        );
        let mut path = core::mem::take(&mut self.glyph);
        path.clear();
        let place = |(x, y): (f64, f64)| {
            let product = |by: i64, coordinate: f64| {
                i64::from(saturated(tairix_util::mathf::floor(
                    exactly(by) * coordinate / f64::from(FIXED_ONE),
                )))
            };
            let along = |by_x: i64, by_y: i64| product(by_x, x).saturating_add(product(by_y, y));
            let at = |axis: Axis, origin: i64, relative: i64| At {
                relative: clamped(relative),
                placed: axis.draw_units(origin.saturating_add(relative)),
            };
            let [xx, xy, yx, yy] = multipliers;
            (
                at(grid.across, origin.0, along(xx, yx)),
                at(grid.up, origin.1, along(xy, yy)),
            )
        };
        let style = if font.data.non_zero() {
            NON_ZERO
        } else {
            WINDING_EVEN_ODD
        };
        let traced = match &outline {
            Outline::Design { glyph, .. } => design_path(&mut path, &glyph.fill, &place),
            Outline::Face(face) => face_path(&mut path, face, &place),
        };
        let room = wrap(geometry.placed((i64::from(placed.to.0), 0)).0);
        let written = match traced {
            Ok(()) => self.write_glyph(os, &path, style, room),
            Err(Unrecorded) => Err(os.no_memory()),
        };
        self.glyph = path;
        written
    }

    fn moved(&mut self, _: &mut Os<'_>, _: (i32, i32), to: (i32, i32)) -> Result<(), Fail> {
        self.underline_to(to.0, false);
        Ok(())
    }

    fn underline(
        &mut self,
        os: &mut Os<'_>,
        font: &Font,
        at: (i32, i32),
        position: i32,
        thickness: u32,
    ) -> Result<(), Fail> {
        self.close_bar(os)?;
        self.underline = None;
        let Some(geometry) = self.geometry.filter(|_| thickness != 0) else {
            return Ok(());
        };
        let (top, bottom) = layout::underline_edges(font, at, position, thickness);
        let (left, top) = geometry.placed((i64::from(at.0), top));
        let bottom = geometry.placed((0, bottom)).1;
        let (across, up) = (geometry.grid.across, geometry.grid.up);
        self.underline = Some(Underline {
            left: across.draw_units_of(left),
            top: up.draw_units_of(top),
            bottom: up.draw_units_of(bottom),
            fill: self.fill,
            bar: None,
        });
        Ok(())
    }

    fn colours(&mut self, os: &mut Os<'_>, colours: &mut Colours) -> Result<(), Fail> {
        crate::colours::string_changed(os, colours)?;
        if let Some((_, foreground, _)) = colours.rgb {
            self.fill = foreground;
        }
        Ok(())
    }
}

/// An outlines file's fill path into `glyph`, each subpath closed.
fn design_path(
    glyph: &mut Glyph,
    elements: &[Element],
    place: &impl Fn((f64, f64)) -> (At, At),
) -> Result<(), Unrecorded> {
    let at = |p: crate::outlines::Point| place((f64::from(p.x), f64::from(p.y)));
    let mut open = false;
    for element in elements {
        match *element {
            Element::Move(p) => {
                if open {
                    glyph.element(tag::CLOSE, &[])?;
                }
                glyph.element(tag::MOVE, &[at(p)])?;
                open = true;
            }
            Element::Line(p) => glyph.element(tag::LINE, &[at(p)])?,
            Element::Curve(p, q, r) => glyph.element(tag::CURVE, &[at(p), at(q), at(r)])?,
        }
    }
    close(glyph, open)
}

/// A face's outline, in ems, into `glyph` as thousandths of an em.
fn face_path(
    glyph: &mut Glyph,
    path: &BezierPath,
    place: &impl Fn((f64, f64)) -> (At, At),
) -> Result<(), Unrecorded> {
    let millems = f64::from(units::MILLEMS);
    let at = |p: BezierPoint| place((p.x * millems, p.y * millems));
    let mut open = false;
    for subpath in &path.subpaths {
        if open {
            glyph.element(tag::CLOSE, &[])?;
        }
        glyph.element(tag::MOVE, &[at(subpath.start)])?;
        open = true;
        for element in &subpath.elements {
            match *element {
                PathEl::Line(p) => glyph.element(tag::LINE, &[at(p)])?,
                PathEl::Cubic(p, q, r) => glyph.element(tag::CURVE, &[at(p), at(q), at(r)])?,
            }
        }
    }
    close(glyph, open)
}

/// A path's last subpath closed.
fn close(glyph: &mut Glyph, open: bool) -> Result<(), Unrecorded> {
    if open {
        glyph.element(tag::CLOSE, &[])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
