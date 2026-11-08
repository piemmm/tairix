//! Reading a Draw file into a drawing.
//!
//! Two passes over the objects: the first finds the font table and the
//! options, which RISC OS's later Draw lets follow objects that do not need
//! them, and the second reads every object in order into one layer. A path
//! with a gap that is both filled and stroked is read once for each, since a
//! fill runs along a gap its stroke leaves undrawn.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::{Affine, FillRule, Point};
use tairix_collections::{try_arc, SharedSlice, SharedStr};
use tairix_colour::Rgba;
use tairix_fsmeta::preset::acorn::latin1_char;
use tairix_util::fallible::{self, try_push};

use super::{
    a_paper, colour_of, face_of, fpa_double, Face, CAP_BUTT, CAP_ROUND, CAP_SQUARE, CAP_TRIANGLE,
    CLOSE, CLOSE_WITH_GAP, CODE, CURVE, DASHED, END, END_CAP_SHIFT, FIXED_ONE, FONT_NUMBER,
    FONT_TABLE, GAP, GROUP, GROUP_NAME, HEADER, JOIN_BEVEL, JOIN_MITRE, JOIN_ROUND, JPEG,
    LANDSCAPE, LINE, MAJOR_VERSION, MATRIX, MITRE_LIMIT, MONOSPACE, MOVE, OBJECT_HEADER, OPTIONS,
    PATH, RIGHT_TO_LEFT, SPRITE, START_CAP_SHIFT, TAG, TAGGED, TEXT, TEXT_AREA, TEXT_COLUMN,
    TRANSFORMED_SPRITE, TRANSFORMED_TEXT, TRIANGLE_LENGTH_SHIFT, TRIANGLE_SIZE, TRIANGLE_UNIT,
    TRIANGLE_WIDTH_SHIFT, UNITS_PER_PIXEL, WINDING_EVEN_ODD,
};
use crate::children::Children;
use crate::drawing::{Drawing, Grid, Page, Unit, PAPER};
use crate::id::Ids;
use crate::image::{self, Encoding, Picture, Pictures};
use crate::limits::{self, MAX_DEPTH, MAX_SUBDIVISIONS};
use crate::node::{Kind, Node};
use crate::shape::{Contour, NodeJoin, PathNode, PathShape, Shape};
use crate::style::{ArrowShape, Arrowhead, Cap, Join, Paint, PaintOrder, Stroke, Style};
use crate::text::{Baseline, Text, TextAlign, DEFAULT_LINE_HEIGHT};
use crate::unkept::{Lost, Unkept};
use crate::{Read, ReadError};

/// The width the thinnest outline a device can draw is read as: a pixel at
/// full size.
const HAIRLINE: f64 = 1.0;

/// The font numbers a font table can name: a byte's worth.
const FONT_NUMBERS: usize = 256;

/// Read `bytes`, a Draw file, into a drawing of one layer, issuing its
/// objects' identities from `ids`.
///
/// # Errors
///
/// [`ReadError::Unrecognised`] for bytes that are no Draw file or one of a
/// later format, [`ReadError::Malformed`] for one broken anywhere,
/// [`ReadError::TooComplex`] past a bound and [`ReadError::OutOfMemory`]
/// when it cannot be held. Nothing is half-read.
pub fn read(bytes: &[u8], ids: &mut Ids) -> Result<Read, ReadError> {
    if !super::recognised(bytes) {
        return Err(ReadError::Unrecognised);
    }
    let header = bytes.get(..HEADER).ok_or(ReadError::Malformed)?;
    if word(header, 4) != Some(MAJOR_VERSION) {
        return Err(ReadError::Unrecognised);
    }
    let body = &bytes[HEADER..];
    let mut names = fallible::filled(FONT_NUMBERS, None).ok_or(ReadError::OutOfMemory)?;
    let mut setting = None;
    for object in Objects(body) {
        let object = object?;
        match object.kind {
            FONT_TABLE => font_table(object.bytes, &mut names)?,
            // Draw keeps the first options object a file holds and discards
            // the rest.
            OPTIONS if setting.is_none() => setting = Some(options(object.bytes)?),
            _ => {}
        }
    }
    // A file without options is on Draw's default paper, A4 as the
    // drawing's own is.
    let (page, grid) = setting.unwrap_or_else(|| (Page::of(&PAPER[0]), Grid::default()));
    let mut reader = Reader {
        ids,
        names,
        faces: fallible::filled(FONT_NUMBERS, None).ok_or(ReadError::OutOfMemory)?,
        height: page.size().y,
        unkept: Unkept::NONE,
        pictures: Pictures::new(),
    };
    let mut nodes = Vec::new();
    reader.objects(body, 0, &mut nodes)?;
    let layer = Node::new(
        reader.ids.issue(),
        Kind::Group {
            children: Children::from_nodes(nodes)?,
            clip: None,
        },
    )?;
    let mut layers = Vec::new();
    try_push(&mut layers, try_arc(layer)?)?;
    let drawing = Drawing::new(
        page,
        Children::from_nodes(layers)?,
        SharedSlice::new(),
        grid,
    )?;
    Ok(Read {
        drawing,
        unkept: reader.unkept,
    })
}

/// One object of a file: its type and all its bytes, header and all.
#[derive(Copy, Clone)]
struct Object<'a> {
    kind: u32,
    bytes: &'a [u8],
}

/// The objects one after another in a run of bytes, each held to the run.
struct Objects<'a>(&'a [u8]);

impl<'a> Iterator for Objects<'a> {
    type Item = Result<Object<'a>, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }
        let framed = word(self.0, 0)
            .zip(word(self.0, 4))
            .and_then(|(kind, size)| {
                let size = usize::try_from(size).ok()?;
                (size >= 8 && size % 4 == 0 && size <= self.0.len()).then_some((kind, size))
            });
        let Some((kind, size)) = framed else {
            self.0 = &[];
            return Some(Err(ReadError::Malformed));
        };
        let (bytes, rest) = self.0.split_at(size);
        self.0 = rest;
        Some(Ok(Object { kind, bytes }))
    }
}

/// The names `bytes`, a font table, gives its font numbers, set into
/// `names`; a number a later table names again takes the later name.
fn font_table<'a>(bytes: &'a [u8], names: &mut [Option<&'a [u8]>]) -> Result<(), ReadError> {
    let mut at = 8;
    while let Some(&number) = bytes.get(at) {
        if number == 0 {
            // What is left pads the table to a word.
            break;
        }
        let name = terminated(bytes, at + 1)?;
        if let Some(slot) = names.get_mut(usize::from(number)) {
            *slot = Some(name);
        }
        at += name.len() + 2;
    }
    Ok(())
}

/// The page and grid `bytes`, an options object, sets out: its paper, the
/// way it is turned, and the grid in the unit Draw shows it in, which the
/// page is shown in too.
fn options(bytes: &[u8]) -> Result<(Page, Grid), ReadError> {
    let field = |at| word(bytes, at).ok_or(ReadError::Malformed);
    // A paper Draw does not name is its default, A4.
    let paper = (field(24)? >> 8)
        .checked_sub(1)
        .and_then(a_paper)
        .unwrap_or(&PAPER[0]);
    let mut page = Page::of(paper);
    if field(28)? & LANDSCAPE != 0 {
        core::mem::swap(&mut page.width, &mut page.height);
    }
    let spacing = fpa_double([field(32)?, field(36)?]);
    let subdivisions = field(40)?;
    let shown = if field(60)? == 0 { Unit::In } else { Unit::Mm };
    if shown != page.unit {
        let to_shown = page.unit.pixels() / shown.pixels();
        page.width *= to_shown;
        page.height *= to_shown;
        page.unit = shown;
    }
    let grid_unit = if shown == Unit::In {
        Unit::In
    } else {
        Unit::Cm
    };
    let spacing = spacing * grid_unit.pixels();
    // The grid sets nothing drawn, so one that cannot be held is the
    // drawing's own default rather than a broken file.
    let grid = if limits::length(spacing) && spacing > 0.0 {
        Grid {
            spacing,
            subdivisions: subdivisions.clamp(1, MAX_SUBDIVISIONS),
        }
    } else {
        Grid::default()
    };
    Ok((page, grid))
}

/// What sets a text's characters: its colour word, its style word naming
/// its font, and its nominal sizes across and up.
struct Lettering {
    colour: u32,
    style: u32,
    sizes: (u32, u32),
}

/// The objects a file holds, read into nodes.
struct Reader<'a, 'i> {
    ids: &'i mut Ids,
    /// Each font number's name, as the font table gave it.
    names: Vec<Option<&'a [u8]>>,
    /// Each font number's face, once a text has been set in it.
    faces: Vec<Option<Face>>,
    /// The page's height in pixels: Draw measures up from its foot.
    height: f64,
    unkept: Unkept,
    /// Every picture read so far, by its bytes.
    pictures: Pictures<'a>,
}

impl<'a> Reader<'a, '_> {
    /// Each object of `bytes` that draws, read at `depth` onto `out`.
    fn objects(
        &mut self,
        bytes: &'a [u8],
        depth: usize,
        out: &mut Vec<Arc<Node>>,
    ) -> Result<(), ReadError> {
        for object in Objects(bytes) {
            if let Some(node) = self.object(object?, depth)? {
                try_push(out, try_arc(node)?)?;
            }
        }
        Ok(())
    }

    /// What `object` draws, if it draws anything the drawing can hold.
    fn object(&mut self, object: Object<'a>, depth: usize) -> Result<Option<Node>, ReadError> {
        let bytes = object.bytes;
        Ok(match object.kind {
            TEXT => Some(self.text(bytes)?),
            TRANSFORMED_TEXT => Some(self.transformed_text(bytes)?),
            PATH => Some(self.path(bytes)?),
            GROUP => Some(self.group(bytes, depth)?),
            TAGGED => self.tagged(bytes, depth)?,
            // These set the drawing up rather than draw in it, and a text
            // column stands only within a text area.
            FONT_TABLE | OPTIONS | TEXT_COLUMN => None,
            SPRITE => self.sprite(bytes)?,
            TRANSFORMED_SPRITE => self.transformed_sprite(bytes)?,
            JPEG => self.jpeg(bytes)?,
            TEXT_AREA => {
                self.unkept.insert(Lost::TextAreas);
                None
            }
            _ => {
                self.unkept.insert(Lost::ForeignContent);
                None
            }
        })
    }

    /// A sprite filling its object's bounds.
    fn sprite(&mut self, bytes: &'a [u8]) -> Result<Option<Node>, ReadError> {
        let sprite = bytes.get(OBJECT_HEADER..).ok_or(ReadError::Malformed)?;
        let Some(picture) = self.picture(sprite, image::decode_sprite)? else {
            return Ok(None);
        };
        let low = self.point(bytes, 8)?;
        let high = self.point(bytes, 16)?;
        let place = tairix_bezier::Bounds::from_points([low, high]);
        self.image_node(&picture, place, Affine::IDENTITY)
    }

    /// A sprite its matrix maps from the space its own pixels' size spans,
    /// its foot on the origin.
    fn transformed_sprite(&mut self, bytes: &'a [u8]) -> Result<Option<Node>, ReadError> {
        let matrix = matrix(bytes, OBJECT_HEADER)?;
        let sprite = bytes
            .get(OBJECT_HEADER + MATRIX..)
            .ok_or(ReadError::Malformed)?;
        let Some(picture) = self.picture(sprite, image::decode_sprite)? else {
            return Ok(None);
        };
        let Some(Encoding::Sprite { eig }) = picture.source.as_ref().map(|source| source.encoding)
        else {
            return Err(ReadError::Malformed);
        };
        let (width, height) = picture.size();
        // Each pixel is `2^eig` OS units of 256 draw units.
        let pitch = |eig: u8| f64::from(1_u32 << eig) * 256.0;
        let native = (
            f64::from(width) * pitch(eig.0),
            f64::from(height) * pitch(eig.1),
        );
        let to_native = Affine::scale(pitch(eig.0), pitch(eig.1));
        self.placed_raster(&picture, native, to_native, matrix)
    }

    /// A JPEG its matrix maps from the space of the width and height the
    /// object states, its foot on the origin. RISC OS draws its raster as
    /// stored, whatever turn the JPEG's own metadata asks for.
    fn jpeg(&mut self, bytes: &'a [u8]) -> Result<Option<Node>, ReadError> {
        let field = |at: usize| word(bytes, OBJECT_HEADER + at).ok_or(ReadError::Malformed);
        let native = (f64::from(field(0)?), f64::from(field(4)?));
        let matrix = matrix(bytes, OBJECT_HEADER + 16)?;
        let length = usize::try_from(field(40)?).map_err(|_| ReadError::Malformed)?;
        let start = OBJECT_HEADER + 44;
        let data = start
            .checked_add(length)
            .and_then(|end| bytes.get(start..end))
            .ok_or(ReadError::Malformed)?;
        if native.0 <= 0.0 || native.1 <= 0.0 {
            return Ok(None);
        }
        let Some(picture) = self.picture(data, |data, limits| {
            let bytes = SharedSlice::try_from_slice(data)
                .map_err(|_| tairix_image::DecodeError::OutOfMemory)?;
            image::decode_encoded(bytes, limits)
        })?
        else {
            return Ok(None);
        };
        let Some(Encoding::Jpeg(frame)) = picture.source.as_ref().map(|source| source.encoding)
        else {
            self.unkept.insert(Lost::Images);
            return Ok(None);
        };
        // The picture's pixels back onto the raster as it is stored.
        let unturned = frame
            .orientation
            .to_picture(frame.stored)
            .invert()
            .ok_or(ReadError::Malformed)?;
        let stored = (f64::from(frame.stored.0), f64::from(frame.stored.1));
        let to_stored = Affine::scale(native.0 / stored.0, native.1 / stored.1);
        self.placed_raster(&picture, native, unturned.then(to_stored), matrix)
    }

    /// The picture whose bytes are `encoded`, decoded by `decode` the first
    /// time they are met; `None`, the picture lost, where they will not read.
    fn picture(
        &mut self,
        encoded: &'a [u8],
        decode: impl FnOnce(
            &'a [u8],
            &tairix_image::DecodeLimits,
        ) -> Result<
            (tairix_raster::Surface, Option<image::Source>),
            tairix_image::DecodeError,
        >,
    ) -> Result<Option<Picture>, ReadError> {
        let picture = self
            .pictures
            .picture(encoded, |limits| decode(encoded, limits))?;
        if picture.is_none() {
            self.unkept.insert(Lost::Images);
        }
        Ok(picture)
    }

    /// A picture laid over its own pixels, each a unit, mapped onto the page
    /// by `matrix` from a `native` space of draw units whose foot is on the
    /// origin and whose top is the raster's first row; `to_native` takes the
    /// picture's pixels to that space, its rows still running down.
    fn placed_raster(
        &mut self,
        picture: &Picture,
        native: (f64, f64),
        to_native: Affine,
        matrix: Affine,
    ) -> Result<Option<Node>, ReadError> {
        let (width, height) = picture.size();
        let place = tairix_bezier::Bounds::from_points([
            Point::ZERO,
            Point::new(f64::from(width), f64::from(height)),
        ]);
        // Raster rows run down from the native space's top.
        let upright = Affine {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            e: 0.0,
            f: native.1,
        };
        let from_draw = Affine {
            a: 1.0 / UNITS_PER_PIXEL,
            b: 0.0,
            c: 0.0,
            d: -1.0 / UNITS_PER_PIXEL,
            e: 0.0,
            f: self.height,
        };
        let placed = to_native.then(upright).then(matrix).then(from_draw);
        self.image_node(picture, place, placed)
    }

    /// The node a picture laid over `place` and placed by `transform` is, or
    /// `None` where the drawing cannot hold that placing, the picture lost.
    fn image_node(
        &mut self,
        picture: &Picture,
        place: tairix_bezier::Bounds,
        transform: Affine,
    ) -> Result<Option<Node>, ReadError> {
        if place.width() <= 0.0 || place.height() <= 0.0 {
            return Ok(None);
        }
        if !limits::transform(transform) {
            self.unkept.insert(Lost::Images);
            return Ok(None);
        }
        let image = picture.placed(place).map_err(ReadError::from)?;
        let node = Node::new(
            self.ids.issue(),
            Kind::Image {
                image: try_arc(image)?,
            },
        )?;
        Ok(Some(node.with_transform(transform)?))
    }

    fn group(&mut self, bytes: &'a [u8], depth: usize) -> Result<Node, ReadError> {
        let content = OBJECT_HEADER + GROUP_NAME;
        let name = bytes
            .get(OBJECT_HEADER..content)
            .ok_or(ReadError::Malformed)?;
        let mut children = Vec::new();
        self.objects(&bytes[content..], deeper(depth)?, &mut children)?;
        let node = Node::new(
            self.ids.issue(),
            Kind::Group {
                children: Children::from_nodes(children)?,
                clip: None,
            },
        )?;
        Ok(node.with_name(group_name(name)?)?)
    }

    /// The object a tagged object wraps; its tag and the data after it say
    /// something of the object to the program that tagged it, which the
    /// drawing cannot keep.
    fn tagged(&mut self, bytes: &'a [u8], depth: usize) -> Result<Option<Node>, ReadError> {
        let wrapped = bytes.get(OBJECT_HEADER + 4..).ok_or(ReadError::Malformed)?;
        let object = Objects(wrapped).next().ok_or(ReadError::Malformed)??;
        self.unkept.insert(Lost::Annotations);
        self.object(object, deeper(depth)?)
    }

    fn text(&mut self, bytes: &[u8]) -> Result<Node, ReadError> {
        let field = |at| word(bytes, at).ok_or(ReadError::Malformed);
        let lettering = Lettering {
            colour: field(24)?,
            style: field(32)?,
            sizes: (field(36)?, field(40)?),
        };
        let start = self.point(bytes, 44)?;
        let content = latin1(terminated(bytes, 52)?)?;
        self.text_node(content, &lettering, start, None)
    }

    fn transformed_text(&mut self, bytes: &[u8]) -> Result<Node, ReadError> {
        let field = |at| word(bytes, at).ok_or(ReadError::Malformed);
        let matrix = matrix(bytes, OBJECT_HEADER)?;
        if field(48)? & RIGHT_TO_LEFT != 0 {
            self.unkept.insert(Lost::TextPositioning);
        }
        let lettering = Lettering {
            colour: field(52)?,
            style: field(60)?,
            sizes: (field(64)?, field(68)?),
        };
        let start = self.point(bytes, 72)?;
        let content = latin1(terminated(bytes, 80)?)?;
        // Draw's matrix maps a space whose y runs up; the drawing's runs
        // down, so the matrix is seen through that reflection.
        let placed = (matrix != Affine::IDENTITY).then_some(Affine {
            a: matrix.a,
            b: -matrix.b,
            c: -matrix.c,
            d: matrix.d,
            e: matrix.e / UNITS_PER_PIXEL,
            f: -matrix.f / UNITS_PER_PIXEL,
        });
        self.text_node(content, &lettering, start, placed)
    }

    /// A text of `content` set as `lettering` says from `start`, its
    /// baseline's start, mapped by `matrix` about it when it is transformed.
    /// Draw sets a font wider or narrower than its height as asked, so such
    /// text is the drawing's stretched across.
    fn text_node(
        &mut self,
        content: String,
        lettering: &Lettering,
        start: Point,
        matrix: Option<Affine>,
    ) -> Result<Node, ReadError> {
        let (across, up) = lettering.sizes;
        if up == 0 {
            return Err(ReadError::Malformed);
        }
        let face = self.face(lettering.style)?;
        let (origin, transform) = match matrix {
            None if across == up => (start, Affine::IDENTITY),
            _ => {
                let stretched = Affine::scale(f64::from(across) / f64::from(up), 1.0);
                let placed = stretched
                    .then(matrix.unwrap_or(Affine::IDENTITY))
                    .then(Affine::translate(start.x, start.y));
                (Point::ZERO, placed)
            }
        };
        let text = Text {
            content: SharedStr::try_from_string(content)?,
            family: face.family,
            size: f64::from(up) / UNITS_PER_PIXEL,
            weight: face.weight,
            italic: face.italic,
            letter_spacing: 0.0,
            align: TextAlign::Start,
            baseline: Baseline::Lines {
                origin,
                line_height: DEFAULT_LINE_HEIGHT,
            },
            runs: SharedSlice::new(),
        };
        let style = Style {
            fill: paint_of(colour_of(lettering.colour)),
            ..Style::default()
        };
        let node = Node::new(
            self.ids.issue(),
            Kind::Text {
                text: try_arc(text)?,
                style: try_arc(style)?,
            },
        )?;
        Ok(node.with_transform(transform)?)
    }

    /// The face font `number` of a text style sets: the font table's, or
    /// the system font's for number zero or one the table does not name, as
    /// Draw sets either.
    fn face(&mut self, style: u32) -> Result<Face, ReadError> {
        let number = usize::try_from(style & FONT_NUMBER).unwrap_or(0);
        if let Some(Some(face)) = self.faces.get(number) {
            return Ok(face.clone());
        }
        let face = match self.names.get(number).copied().flatten() {
            Some(name) => face_of(&latin1(name)?)?,
            None => Face {
                family: SharedStr::try_from_str(MONOSPACE)?,
                weight: 400,
                italic: false,
            },
        };
        if let Some(slot) = self.faces.get_mut(number) {
            *slot = Some(face.clone());
        }
        Ok(face)
    }

    fn path(&mut self, bytes: &[u8]) -> Result<Node, ReadError> {
        let field = |at| word(bytes, at).ok_or(ReadError::Malformed);
        let fill = colour_of(field(24)?);
        let outline = colour_of(field(28)?);
        let width = field(32)?;
        let style = field(36)?;
        let mut dashes = Vec::new();
        let (dash_offset, components) = if style & DASHED == 0 {
            (0, bytes.get(40..).ok_or(ReadError::Malformed)?)
        } else {
            let count = usize::try_from(field(44)?).map_err(|_| ReadError::Malformed)?;
            let end = count
                .checked_mul(4)
                .and_then(|length| length.checked_add(48))
                .ok_or(ReadError::Malformed)?;
            let pattern = bytes.get(48..end).ok_or(ReadError::Malformed)?;
            if !fallible::reserve(&mut dashes, count) {
                return Err(ReadError::OutOfMemory);
            }
            let (lengths, _) = pattern.as_chunks::<4>();
            dashes.extend(
                lengths
                    .iter()
                    .map(|&length| f64::from(u32::from_le_bytes(length)) / UNITS_PER_PIXEL),
            );
            (field(40)?, &bytes[end..])
        };
        let join = match style & CODE {
            JOIN_MITRE => Join::Miter,
            JOIN_ROUND => Join::Round,
            JOIN_BEVEL => Join::Bevel,
            _ => return Err(ReadError::Malformed),
        };
        let (cap, start, end) = caps(style);
        let width = if width == 0 {
            if outline.is_some() {
                self.unkept.insert(Lost::NonScalingStrokes);
            }
            HAIRLINE
        } else {
            f64::from(width) / UNITS_PER_PIXEL
        };
        let style = Style {
            fill: paint_of(fill),
            fill_rule: if style & WINDING_EVEN_ODD == 0 {
                FillRule::NonZero
            } else {
                FillRule::EvenOdd
            },
            stroke: Stroke {
                paint: paint_of(outline),
                width,
                join,
                miter_limit: MITRE_LIMIT,
                cap,
                dashes: SharedSlice::try_from_vec(dashes)?,
                dash_offset: f64::from(dash_offset) / UNITS_PER_PIXEL,
                start,
                end,
            },
            paint_order: PaintOrder::FillFirst,
        };
        let (for_stroke, gapped) = self.contours(components, Reading::Stroked)?;
        if !gapped || !style.fill.paints() {
            return self.shape(for_stroke, style);
        }
        let (for_fill, _) = self.contours(components, Reading::Filled)?;
        if !style.stroke.paints() {
            return self.shape(for_fill, style);
        }
        let fill_only = Style {
            stroke: Stroke {
                paint: Paint::None,
                ..style.stroke.clone()
            },
            ..style.clone()
        };
        let stroke_only = Style {
            fill: Paint::None,
            ..style
        };
        let mut parts = Vec::new();
        try_push(&mut parts, try_arc(self.shape(for_fill, fill_only)?)?)?;
        try_push(&mut parts, try_arc(self.shape(for_stroke, stroke_only)?)?)?;
        Ok(Node::new(
            self.ids.issue(),
            Kind::Group {
                children: Children::from_nodes(parts)?,
                clip: None,
            },
        )?)
    }

    fn shape(&mut self, contours: Vec<Contour>, style: Style) -> Result<Node, ReadError> {
        Ok(Node::new(
            self.ids.issue(),
            Kind::Shape {
                shape: Shape::Path(try_arc(PathShape { contours })?),
                style: try_arc(style)?,
            },
        )?)
    }

    /// The contours a path's `components` trace, read for `reading`, and
    /// whether they hold a gap.
    fn contours(
        &self,
        components: &[u8],
        reading: Reading,
    ) -> Result<(Vec<Contour>, bool), ReadError> {
        let mut trace = Trace {
            reading,
            contours: Vec::new(),
            drawing: false,
            first: 0,
            gapped: false,
        };
        let mut at = 0;
        loop {
            let tag = word(components, at).ok_or(ReadError::Malformed)? & TAG;
            let point = |index: usize| self.point(components, at + 4 + 8 * index);
            match tag {
                END => return Ok((trace.contours, trace.gapped)),
                MOVE => trace.move_to(point(0)?)?,
                LINE => trace.line_to(point(0)?)?,
                GAP => trace.gap_to(point(0)?)?,
                CURVE => trace.curve_to(point(0)?, point(1)?, point(2)?)?,
                CLOSE => trace.close(true)?,
                CLOSE_WITH_GAP => trace.close(false)?,
                _ => return Err(ReadError::Malformed),
            }
            at += match tag {
                MOVE | LINE | GAP => 12,
                CURVE => 28,
                _ => 4,
            };
        }
    }

    /// The point at `at` of `bytes`, in the drawing's space.
    fn point(&self, bytes: &[u8], at: usize) -> Result<Point, ReadError> {
        let (Some(x), Some(y)) = (int(bytes, at), int(bytes, at + 4)) else {
            return Err(ReadError::Malformed);
        };
        Ok(Point::new(
            f64::from(x) / UNITS_PER_PIXEL,
            self.height - f64::from(y) / UNITS_PER_PIXEL,
        ))
    }
}

/// The matrix at `at` of `bytes`, in draw units on the paper.
fn matrix(bytes: &[u8], at: usize) -> Result<Affine, ReadError> {
    let entry = |index: usize| {
        int(bytes, at + 4 * index)
            .map(f64::from)
            .ok_or(ReadError::Malformed)
    };
    Ok(Affine {
        a: entry(0)? / FIXED_ONE,
        b: entry(1)? / FIXED_ONE,
        c: entry(2)? / FIXED_ONE,
        d: entry(3)? / FIXED_ONE,
        e: entry(4)?,
        f: entry(5)?,
    })
}

/// How a path's components are read: for its stroke, which a gap breaks, or
/// for its fill, which runs along one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Reading {
    Stroked,
    Filled,
}

/// Contours as a path's components trace them.
struct Trace {
    reading: Reading,
    contours: Vec<Contour>,
    /// Whether a sub-path is open to draw on.
    drawing: bool,
    /// The contour the open sub-path began in.
    first: usize,
    gapped: bool,
}

impl Trace {
    fn move_to(&mut self, point: Point) -> Result<(), ReadError> {
        self.begin(point)?;
        self.first = self.contours.len() - 1;
        self.drawing = true;
        Ok(())
    }

    fn begin(&mut self, point: Point) -> Result<(), ReadError> {
        let mut nodes = Vec::new();
        try_push(&mut nodes, PathNode::corner(point))?;
        Ok(try_push(
            &mut self.contours,
            Contour {
                nodes,
                closed: false,
            },
        )?)
    }

    /// The contour the open sub-path draws on; a component that draws with
    /// none open breaks the file.
    fn current(&mut self) -> Result<&mut Contour, ReadError> {
        if !self.drawing {
            return Err(ReadError::Malformed);
        }
        self.contours.last_mut().ok_or(ReadError::Malformed)
    }

    fn line_to(&mut self, point: Point) -> Result<(), ReadError> {
        Ok(try_push(
            &mut self.current()?.nodes,
            PathNode::corner(point),
        )?)
    }

    fn curve_to(&mut self, from: Point, to: Point, point: Point) -> Result<(), ReadError> {
        let contour = self.current()?;
        if let Some(last) = contour.nodes.last_mut() {
            last.after = Some(from);
        }
        Ok(try_push(
            &mut contour.nodes,
            PathNode {
                point,
                before: Some(to),
                after: None,
                join: NodeJoin::Corner,
            },
        )?)
    }

    fn gap_to(&mut self, point: Point) -> Result<(), ReadError> {
        self.current()?;
        self.gapped = true;
        match self.reading {
            Reading::Filled => self.line_to(point),
            Reading::Stroked => self.begin(point),
        }
    }

    /// The open sub-path closed, by a line or a gap; a stroke leaves a gap
    /// open, and carries a line on from its last run into its first where a
    /// gap broke it.
    fn close(&mut self, drawn: bool) -> Result<(), ReadError> {
        self.current()?;
        self.drawing = false;
        if self.reading == Reading::Stroked && !drawn {
            return Ok(());
        }
        if self.contours.len() - 1 == self.first {
            if let Some(contour) = self.contours.last_mut() {
                closed(contour);
            }
            return Ok(());
        }
        let Some(Contour { mut nodes, .. }) = self.contours.pop() else {
            return Err(ReadError::Malformed);
        };
        let head = self
            .contours
            .get_mut(self.first)
            .ok_or(ReadError::Malformed)?;
        if !fallible::reserve(&mut nodes, head.nodes.len()) {
            return Err(ReadError::OutOfMemory);
        }
        let mut rest = head.nodes.iter();
        if let (Some(end), Some(start)) = (nodes.last_mut(), head.nodes.first()) {
            if end.point == start.point {
                end.after = start.after;
                rest.next();
            }
        }
        nodes.extend(rest);
        head.nodes = nodes;
        Ok(())
    }
}

/// `contour` closed, its last node merged into its first where the two
/// stand together, as a Draw file closing a shape at its start draws it.
fn closed(contour: &mut Contour) {
    if let (Some(&last), true) = (contour.nodes.last(), contour.nodes.len() > 1) {
        if let Some(first) = contour
            .nodes
            .first_mut()
            .filter(|first| first.point == last.point)
        {
            first.before = last.before;
            contour.nodes.pop();
        }
    }
    contour.closed = true;
}

/// The cap a path is stroked with and the arrowheads it ends in, for the
/// caps its style gives its two ends: their one cap where both ends share a
/// plain one, else butt ends each bearing its own cap as an arrowhead — a
/// round cap as a disc and a square one as a square, each the stroke's
/// width, and a triangle as the triangle its style sizes.
fn caps(style: u32) -> (Cap, Arrowhead, Arrowhead) {
    let start = (style >> START_CAP_SHIFT) & CODE;
    let end = (style >> END_CAP_SHIFT) & CODE;
    let plain = match start {
        CAP_BUTT => Some(Cap::Butt),
        CAP_ROUND => Some(Cap::Round),
        CAP_SQUARE => Some(Cap::Square),
        _ => None,
    };
    if let Some(cap) = plain.filter(|_| start == end) {
        return (cap, Arrowhead::NONE, Arrowhead::NONE);
    }
    let triangle = Arrowhead {
        shape: ArrowShape::Triangle,
        width: 2.0 * f64::from((style >> TRIANGLE_WIDTH_SHIFT) & TRIANGLE_SIZE) / TRIANGLE_UNIT,
        length: f64::from((style >> TRIANGLE_LENGTH_SHIFT) & TRIANGLE_SIZE) / TRIANGLE_UNIT,
    };
    let head = |code| match code {
        CAP_ROUND => Arrowhead {
            shape: ArrowShape::Circle,
            width: 1.0,
            length: 1.0,
        },
        CAP_SQUARE => Arrowhead {
            shape: ArrowShape::Square,
            width: 1.0,
            length: 1.0,
        },
        CAP_TRIANGLE => triangle,
        _ => Arrowhead::NONE,
    };
    (Cap::Butt, head(start), head(end))
}

/// What a colour word paints with.
fn paint_of(colour: Option<Rgba>) -> Paint {
    colour.map_or(Paint::None, Paint::Colour)
}

/// How much deeper than `depth` a group's or a tagged object's content
/// lies, within the drawing's bound on nesting.
fn deeper(depth: usize) -> Result<usize, ReadError> {
    if depth < MAX_DEPTH {
        Ok(depth + 1)
    } else {
        Err(ReadError::TooComplex)
    }
}

/// The name a group's twelve bytes spell, padded with spaces or zeros, or
/// none where they are all padding.
fn group_name(bytes: &[u8]) -> Result<Option<SharedStr>, ReadError> {
    let end = bytes
        .iter()
        .rposition(|&byte| byte != b' ' && byte != 0)
        .map_or(0, |last| last + 1);
    if end == 0 {
        return Ok(None);
    }
    Ok(Some(SharedStr::try_from_string(latin1(&bytes[..end])?)?))
}

/// The text `bytes` spell in RISC OS's Latin-1, a byte that spells no
/// character read as the replacement character.
fn latin1(bytes: &[u8]) -> Result<String, ReadError> {
    let mut text = String::new();
    // No character the alphabet holds takes more than three bytes.
    if text.try_reserve(bytes.len().saturating_mul(3)).is_err() {
        return Err(ReadError::OutOfMemory);
    }
    text.extend(
        bytes
            .iter()
            .map(|&byte| latin1_char(byte).unwrap_or(char::REPLACEMENT_CHARACTER)),
    );
    Ok(text)
}

/// The string at `from` of `bytes`, up to the zero that ends it.
fn terminated(bytes: &[u8], from: usize) -> Result<&[u8], ReadError> {
    let rest = bytes.get(from..).ok_or(ReadError::Malformed)?;
    let end = rest
        .iter()
        .position(|&byte| byte == 0)
        .ok_or(ReadError::Malformed)?;
    Ok(&rest[..end])
}

/// The little-endian word at `at` of `bytes`.
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let quad = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(quad.try_into().ok()?))
}

/// The little-endian signed word at `at` of `bytes`.
fn int(bytes: &[u8], at: usize) -> Option<i32> {
    word(bytes, at).map(|word| i32::from_le_bytes(word.to_le_bytes()))
}
