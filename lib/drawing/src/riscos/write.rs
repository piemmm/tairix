//! Writing a drawing as a Draw file.
//!
//! Draw holds no transforms, gradients, translucency or clips, so every
//! object is written in the paper's coordinates, flat and opaque, and what
//! that loses is reported. A stroke Draw cannot draw as the drawing does —
//! one its object's transform stretches, or one whose corners Draw's fixed
//! mitre limit would cut otherwise — is written as the area it paints, and an
//! arrowhead Draw's caps cannot draw as a shape of its own, so the file draws
//! what the drawing draws. Text is written a line at a time in RISC OS's own
//! fonts. A picture read from a baseline JPEG is a JPEG object of those
//! bytes, one read from a sprite that sprite, and any other its pixels as a
//! 32-bit sprite, masked as they need.

use alloc::vec::Vec;

use tairix_bezier::{Affine, Bounds, FillRule, Path, PathEl, Point, Subpath};
use tairix_colour::Rgba;
use tairix_fsmeta::preset::acorn::latin1_byte;
use tairix_image::{
    DensityUnit, EncodeError, JpegFrame, SpriteInput, SpriteMode, SpriteName, SpritePalette,
    SurfacePicture,
};
use tairix_raster::Surface;
use tairix_util::fallible;
use tairix_util::mathf::{fabs, floor, fmax, fmin, round_i32, sqrt};

use super::{
    a_paper, fpa_words, word_of, Font, CAP_BUTT, CAP_ROUND, CAP_SQUARE, CAP_TRIANGLE, CLOSE, CURVE,
    DASHED, END, END_CAP_SHIFT, FIXED_ONE, FONT_TABLE, GROUP, GROUP_NAME, JOIN_BEVEL, JOIN_MITRE,
    JOIN_ROUND, JPEG, LANDSCAPE, LINE, MAGIC, MAJOR_VERSION, MITRE_LIMIT, MOST_A_PAPER, MOVE,
    NO_COLOUR, OBJECT_HEADER, OPTIONS, OPTIONS_SIZE, PAPER_LIMITS_SHOWN, PATH, SPRITE,
    START_CAP_SHIFT, TEXT, TRANSFORMED_SPRITE, TRANSFORMED_TEXT, TRIANGLE_LENGTH_SHIFT,
    TRIANGLE_SIZE, TRIANGLE_UNIT, TRIANGLE_WIDTH_SHIFT, UNITS_PER_PIXEL, WINDING_EVEN_ODD,
};
use crate::children::Children;
use crate::drawing::{Drawing, Grid, Page, Unit, PAPER_WHITE, PIXELS_PER_INCH};
use crate::image::{survey, Encoding, Image, Source};
use crate::node::{Kind, Node};
use crate::render::{arrowheads, stroke_area};
use crate::style::{ArrowShape, Arrowhead, Cap, Gradient, Join, Paint, PaintOrder, Stroke, Style};
use crate::text::{Baseline, Glyphs, RunPaint, Text, TAB_SPACES};
use crate::DrawingError;

/// The program a file names as its maker, padded with spaces.
const MAKER: &[u8; 12] = b"TAIRiX Draw ";

/// The undo memory RISC OS's Draw gives a new drawing, in bytes, which a
/// file's options ask for.
const UNDO_BUFFER: u32 = 5000;

/// The entry mode a drawing opens in: selecting.
const SELECT_MODE: u32 = 1 << 7;

/// How near an A-series paper's size a page must be to be that paper, in
/// pixels.
const PAPER_TOLERANCE: f64 = 1e-6;

/// How near untransformed a text's matrix must be to be written as plain
/// text, and how near a mitre limit a corner's mitre must reach to be one
/// the two limits may cut otherwise.
const NEAR: f64 = 1e-9;

/// A drawing as a Draw file.
#[derive(Clone, Debug)]
pub struct Written {
    /// The file.
    pub bytes: Vec<u8>,
    /// What it could not keep.
    pub dropped: Dropped,
}

/// What a Draw file could not keep of a drawing, each drawn as near as the
/// format allows.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is a loss of its own a copy reports, and any of them can be"
)]
pub struct Dropped {
    /// Gradients, painted in their average colour.
    pub gradients: bool,
    /// Translucency, painted opaque.
    pub transparency: bool,
    /// Clipping, what was clipped drawn whole.
    pub clips: bool,
    /// Outlines of text, which Draw only fills.
    pub text_outlines: bool,
    /// Space added between characters.
    pub letter_spacing: bool,
    /// Characters RISC OS's Latin-1 has no code for, written as `?`.
    pub characters: bool,
    /// Font families RISC OS has none of, set in its own.
    pub fonts: bool,
    /// The page's size, where it is no A-series paper's.
    pub page_size: bool,
}

impl Dropped {
    /// What was dropped, each as a list of what a copy could not keep names
    /// it.
    pub fn iter(self) -> impl Iterator<Item = &'static str> {
        [
            (self.gradients, "gradients"),
            (self.transparency, "transparency"),
            (self.clips, "clipping"),
            (self.text_outlines, "text outlines"),
            (self.letter_spacing, "letter spacing"),
            (self.characters, "characters outside RISC OS's Latin-1"),
            (self.fonts, "font families"),
            (self.page_size, "page size"),
        ]
        .into_iter()
        .filter_map(|(dropped, part)| dropped.then_some(part))
    }

    /// Whether everything was kept.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.iter().next().is_none()
    }
}

/// `drawing` as a Draw file, its text laid out with `glyphs`.
///
/// # Errors
///
/// [`DrawingError::Invalid`] for a coordinate or size past what a Draw file's
/// words hold, [`DrawingError::TooComplex`] for geometry past what a
/// stroke's outline may hold, and [`DrawingError::OutOfMemory`] when the
/// file cannot be held.
pub fn write(drawing: &Drawing, glyphs: &dyn Glyphs) -> Result<Written, DrawingError> {
    let page = drawing.page();
    let size = page.size();
    let paper = Paper::of(size);
    let mut writer = Writer {
        glyphs,
        out: Out::default(),
        fonts: Vec::new(),
        dropped: Dropped {
            page_size: !paper.exact,
            ..Dropped::default()
        },
        // The page's top-left corner is the paper's, so a file read back
        // holds every object where it stood.
        to_draw: Affine {
            a: UNITS_PER_PIXEL,
            b: 0.0,
            c: 0.0,
            d: -UNITS_PER_PIXEL,
            e: 0.0,
            f: paper.height * UNITS_PER_PIXEL,
        },
        behind: page
            .background
            .filter(|ground| ground.a == u8::MAX)
            .unwrap_or(PAPER_WHITE),
    };
    let mut extent = match page.background {
        Some(ground) => writer.ground(size, ground)?,
        None => None,
    };
    for layer in drawing.layers().iter() {
        extent = Extent::joined(extent, writer.layer(layer)?);
    }
    let Writer {
        out: objects,
        fonts,
        dropped,
        ..
    } = writer;
    let mut file = Out::default();
    file.reserve(objects.0.len() + 256)?;
    file.bytes(MAGIC)?;
    file.word(MAJOR_VERSION)?;
    file.word(0)?;
    file.bytes(MAKER)?;
    for corner in extent.map_or([0; 4], |extent| extent.0) {
        file.int(corner)?;
    }
    if !fonts.is_empty() {
        font_table(&mut file, &fonts)?;
    }
    options(&mut file, page, drawing.grid(), &paper)?;
    file.bytes(&objects.0)?;
    Ok(Written {
        bytes: file.0,
        dropped,
    })
}

/// The A-series paper a page is written on: the smallest that holds it,
/// turned as the page is, A0 where none does, and whether it is the page's
/// own size.
struct Paper {
    number: u32,
    landscape: bool,
    height: f64,
    exact: bool,
}

impl Paper {
    fn of(size: Point) -> Self {
        let landscape = size.x > size.y;
        let mut chosen = (0, size);
        for number in (0..=MOST_A_PAPER).rev() {
            let Some(paper) = a_paper(number) else {
                continue;
            };
            let portrait = Page::of(paper).size();
            let held = if landscape {
                Point::new(portrait.y, portrait.x)
            } else {
                portrait
            };
            chosen = (number, held);
            if size.x <= held.x + PAPER_TOLERANCE && size.y <= held.y + PAPER_TOLERANCE {
                break;
            }
        }
        let (number, held) = chosen;
        Self {
            number,
            landscape,
            height: held.y,
            exact: fabs(size.x - held.x) <= PAPER_TOLERANCE
                && fabs(size.y - held.y) <= PAPER_TOLERANCE,
        }
    }
}

/// The file being written; every byte's room is reserved before it is
/// written.
#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn reserve(&mut self, count: usize) -> Result<(), DrawingError> {
        self.0
            .try_reserve(count)
            .map_err(|_| DrawingError::OutOfMemory)
    }

    fn bytes(&mut self, bytes: &[u8]) -> Result<(), DrawingError> {
        self.reserve(bytes.len())?;
        self.0.extend_from_slice(bytes);
        Ok(())
    }

    fn word(&mut self, word: u32) -> Result<(), DrawingError> {
        self.bytes(&word.to_le_bytes())
    }

    fn int(&mut self, int: i32) -> Result<(), DrawingError> {
        self.bytes(&int.to_le_bytes())
    }

    /// Zeros up to the next word, as every object ends.
    fn pad(&mut self) -> Result<(), DrawingError> {
        let short = (4 - self.0.len() % 4) % 4;
        self.bytes(&[0; 3][..short])
    }

    /// The size of what was written from `at` on set in its header's second
    /// word.
    fn sized(&mut self, at: usize) -> Result<(), DrawingError> {
        let size = u32::try_from(self.0.len() - at).map_err(|_| DrawingError::TooComplex)?;
        self.0[at + 4..at + 8].copy_from_slice(&size.to_le_bytes());
        Ok(())
    }

    /// An object of `kind` begun, its size and bounds set as it is finished;
    /// where it begins.
    fn begin(&mut self, kind: u32) -> Result<usize, DrawingError> {
        let at = self.0.len();
        self.word(kind)?;
        self.bytes(&[0; OBJECT_HEADER - 4])?;
        Ok(at)
    }

    /// The object begun at `at` finished: padded to a word, and its size
    /// and `extent` set.
    fn finish(&mut self, at: usize, extent: Extent) -> Result<(), DrawingError> {
        self.pad()?;
        self.sized(at)?;
        for (index, corner) in extent.0.iter().enumerate() {
            let field = at + 8 + 4 * index;
            self.0[field..field + 4].copy_from_slice(&corner.to_le_bytes());
        }
        Ok(())
    }
}

/// An object's bounds in draw units, as its header holds them: the low
/// corner within them and the high one just beyond.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Extent([i32; 4]);

impl Extent {
    /// The extent that holds `bounds`, in draw units.
    fn of(bounds: Bounds) -> Result<Self, DrawingError> {
        Ok(Self([
            unit(floor(bounds.min.x))?,
            unit(floor(bounds.min.y))?,
            unit(floor(bounds.max.x) + 1.0)?,
            unit(floor(bounds.max.y) + 1.0)?,
        ]))
    }

    /// The extent that holds both, either absent.
    fn joined(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a, b) {
            (Some(Self([x0, y0, x1, y1])), Some(Self([u0, v0, u1, v1]))) => {
                Some(Self([x0.min(u0), y0.min(v0), x1.max(u1), y1.max(v1)]))
            }
            (held, None) | (None, held) => held,
        }
    }
}

/// How Draw strokes a path itself: its colour word, its width in draw
/// units, its join, its caps and its dash pattern.
struct Pen {
    colour: u32,
    width: u32,
    join: u32,
    caps: Caps,
    dashes: Option<Dashes>,
}

/// A Draw path's caps at each end, and a triangle cap's breadth on each side
/// and reach, in sixteenths of the line's width.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Caps {
    start: u32,
    end: u32,
    triangle: (u32, u32),
}

/// A dash pattern in draw units: how far into it a stroke begins, and its
/// lengths.
struct Dashes {
    offset: u32,
    lengths: Vec<u32>,
}

impl Pen {
    /// Its path style word, the winding rule apart.
    const fn style(&self) -> u32 {
        let dashed = if self.dashes.is_some() { DASHED } else { 0 };
        self.join
            | self.caps.end << END_CAP_SHIFT
            | self.caps.start << START_CAP_SHIFT
            | dashed
            | self.caps.triangle.0 << TRIANGLE_WIDTH_SHIFT
            | self.caps.triangle.1 << TRIANGLE_LENGTH_SHIFT
    }

    /// How far beyond its path it can paint, in draw units: its widest
    /// mitre, a square cap's corner, or a triangle's point.
    fn reach(&self) -> f64 {
        let width = f64::from(self.width);
        let half = width / 2.0;
        let join = if self.join == JOIN_MITRE {
            MITRE_LIMIT
        } else {
            1.0
        };
        let (across, along) = self.caps.triangle;
        fmax(
            half * fmax(join, core::f64::consts::SQRT_2),
            f64::from(across + along) / TRIANGLE_UNIT * width + half,
        )
    }
}

/// How a shape's stroke is written.
enum Stroked {
    Nothing,
    /// By Draw itself.
    Pen(Pen),
    /// As the area it paints, filled with its colour word.
    Area(Path, u32),
}

/// One path object: a path in its own space, what fills it and what
/// strokes it.
#[derive(Copy, Clone)]
struct Piece<'p> {
    path: &'p Path,
    fill: u32,
    even_odd: bool,
    pen: Option<&'p Pen>,
}

/// What sets a line of text: its colour word, the colour behind it, its
/// font's number, its size in draw units and, when it is transformed, the
/// matrix Draw maps it by about its start.
struct Lettering {
    colour: u32,
    ground: u32,
    number: u32,
    size: u32,
    matrix: Option<Affine>,
}

struct Writer<'g> {
    glyphs: &'g dyn Glyphs,
    out: Out,
    /// The fonts written text is set in, numbered from one in this order.
    fonts: Vec<Font>,
    dropped: Dropped,
    /// The page's space onto the paper's, in draw units.
    to_draw: Affine,
    /// The colour behind text: the page's opaque ground, else white paper.
    behind: Rgba,
}

impl Writer<'_> {
    /// The page's ground, a rectangle beneath everything.
    fn ground(&mut self, size: Point, ground: Rgba) -> Result<Option<Extent>, DrawingError> {
        let Some(fill) = self.colour(&Paint::Colour(ground)) else {
            return Ok(None);
        };
        let mut sheet = Subpath::new(Point::ZERO);
        if !fallible::reserve(&mut sheet.elements, 3) {
            return Err(DrawingError::OutOfMemory);
        }
        sheet.elements.extend([
            PathEl::Line(Point::new(size.x, 0.0)),
            PathEl::Line(size),
            PathEl::Line(Point::new(0.0, size.y)),
        ]);
        sheet.closed = true;
        let path = Path::single(sheet)?;
        let piece = Piece {
            path: &path,
            fill,
            even_odd: false,
            pen: None,
        };
        self.path(&piece, self.to_draw)
    }

    /// A layer's objects, written in its place, since Draw has no layers.
    fn layer(&mut self, layer: &Node) -> Result<Option<Extent>, DrawingError> {
        let Some(placed) = self.placed(layer, Affine::IDENTITY) else {
            return Ok(None);
        };
        self.dropped.clips |= matches!(layer.kind(), Kind::Group { clip: Some(_), .. });
        let mut extent = None;
        for child in layer.children().into_iter().flat_map(Children::iter) {
            extent = Extent::joined(extent, self.node(child, placed)?);
        }
        Ok(extent)
    }

    /// The map from `node`'s space into the page's, given its parent's, or
    /// `None` where it draws nothing at all.
    fn placed(&mut self, node: &Node, outer: Affine) -> Option<Affine> {
        if !node.visible() || node.opacity() <= 0.0 {
            return None;
        }
        self.dropped.transparency |= node.opacity() < 1.0;
        Some(node.transform().then(outer))
    }

    fn node(&mut self, node: &Node, outer: Affine) -> Result<Option<Extent>, DrawingError> {
        let Some(placed) = self.placed(node, outer) else {
            return Ok(None);
        };
        match node.kind() {
            Kind::Shape { shape, style } => {
                self.shape(node.name(), &shape.to_path()?, style, placed)
            }
            Kind::Text { text, style } => self.text(node.name(), text, style, placed),
            Kind::Image { image } => self.image(image, placed),
            Kind::Group { children, clip } => {
                self.dropped.clips |= clip.is_some();
                let at = self.group(node.name())?;
                let mut extent = None;
                for child in children.iter() {
                    extent = Extent::joined(extent, self.node(child, placed)?);
                }
                self.close(at, extent)
            }
        }
    }

    /// A picture laid where the drawing lays it: RISC OS draws a JPEG's
    /// raster as stored and reads no progressive one.
    fn image(&mut self, image: &Image, placed: Affine) -> Result<Option<Extent>, DrawingError> {
        let to_paper = image.to_place().then(placed).then(self.to_draw);
        let pixels = image.pixels();
        let size = (pixels.width(), pixels.height());
        match image.source() {
            Some(Source {
                encoding: Encoding::Jpeg(frame),
                bytes,
            }) if !frame.progressive => self.jpeg(bytes, frame, to_paper),
            Some(Source {
                encoding: Encoding::Sprite { eig },
                bytes,
            }) => self.sprite(bytes, *eig, size, to_paper),
            _ => {
                let sprite = sprite_of(pixels)?;
                self.sprite(&sprite, SPRITE_EIG, size, to_paper)
            }
        }
    }

    /// `sprite`, `size` pixels each `2^eig` OS units, laid as `to_paper` lays
    /// its pixels: filling its bounds where it stands upright, through a
    /// matrix otherwise.
    fn sprite(
        &mut self,
        sprite: &[u8],
        eig: (u8, u8),
        (width, height): (u32, u32),
        to_paper: Affine,
    ) -> Result<Option<Extent>, DrawingError> {
        let pitch = |eig: u8| f64::from(1_u32 << eig) * 256.0;
        let native = Point::new(
            f64::from(width) * pitch(eig.0),
            f64::from(height) * pitch(eig.1),
        );
        let from_native = Affine {
            a: 1.0 / pitch(eig.0),
            b: 0.0,
            c: 0.0,
            d: -1.0 / pitch(eig.1),
            e: 0.0,
            f: f64::from(height),
        }
        .then(to_paper);
        let bounds = Bounds::from_points([Point::ZERO, native]).transformed(from_native);
        let upright = from_native.b == 0.0
            && from_native.c == 0.0
            && from_native.a > 0.0
            && from_native.d > 0.0;
        if upright {
            // A sprite object fills its bounds exactly, so they are rounded
            // rather than grown.
            let extent = Extent([
                unit(bounds.min.x)?,
                unit(bounds.min.y)?,
                unit(bounds.max.x)?,
                unit(bounds.max.y)?,
            ]);
            if extent.0[0] >= extent.0[2] || extent.0[1] >= extent.0[3] {
                return Ok(None);
            }
            let at = self.out.begin(SPRITE)?;
            self.out.bytes(sprite)?;
            self.out.finish(at, extent)?;
            return Ok(Some(extent));
        }
        let extent = Extent::of(bounds)?;
        let at = self.out.begin(TRANSFORMED_SPRITE)?;
        self.matrix(from_native)?;
        self.out.bytes(sprite)?;
        self.out.finish(at, extent)?;
        Ok(Some(extent))
    }

    /// A JPEG object of `bytes`, at the size its own density states — RISC
    /// OS's ninety to the inch where it states none — mapped by its matrix
    /// from that size onto where `to_paper` lays its pixels.
    fn jpeg(
        &mut self,
        bytes: &[u8],
        frame: &JpegFrame,
        to_paper: Affine,
    ) -> Result<Option<Extent>, DrawingError> {
        let (across, down) = frame
            .density
            .and_then(|density| density.whole_in(DensityUnit::Inch))
            .unwrap_or((DESKTOP_DPI, DESKTOP_DPI));
        let (stored_width, stored_height) = frame.stored;
        let units_per_inch = UNITS_PER_PIXEL * f64::from(PIXELS_PER_INCH);
        let span = |pixels: u32, dpi: u32| {
            size_unit(f64::from(pixels) * units_per_inch / f64::from(dpi)).map(|units| units.max(1))
        };
        let native = (span(stored_width, across)?, span(stored_height, down)?);
        let (width, height) = (f64::from(native.0), f64::from(native.1));
        let to_stored = Affine {
            a: f64::from(stored_width) / width,
            b: 0.0,
            c: 0.0,
            d: -f64::from(stored_height) / height,
            e: 0.0,
            f: f64::from(stored_height),
        };
        let from_native = to_stored
            .then(frame.orientation.to_picture(frame.stored))
            .then(to_paper);
        let extent = Extent::of(
            Bounds::from_points([Point::ZERO, Point::new(width, height)]).transformed(from_native),
        )?;
        let length = u32::try_from(bytes.len()).map_err(|_| DrawingError::TooComplex)?;
        let at = self.out.begin(JPEG)?;
        for field in [native.0, native.1, across, down] {
            self.out.word(field)?;
        }
        self.matrix(from_native)?;
        self.out.word(length)?;
        self.out.bytes(bytes)?;
        self.out.finish(at, extent)?;
        Ok(Some(extent))
    }

    /// `map`, from an object's own space onto the paper, as a transformed
    /// object's matrix holds it.
    fn matrix(&mut self, map: Affine) -> Result<(), DrawingError> {
        for entry in [map.a, map.b, map.c, map.d] {
            self.out.int(unit(entry * FIXED_ONE)?)?;
        }
        self.out.int(unit(map.e)?)?;
        self.out.int(unit(map.f)?)
    }

    /// A group named `name` begun; where it begins.
    fn group(&mut self, name: Option<&str>) -> Result<usize, DrawingError> {
        let at = self.out.begin(GROUP)?;
        let mut held = [b' '; GROUP_NAME];
        let spelled = name.unwrap_or("").chars().filter_map(latin1_byte);
        for (slot, byte) in held.iter_mut().zip(spelled) {
            *slot = byte;
        }
        self.out.bytes(&held)?;
        Ok(at)
    }

    /// The object begun at `at` finished with `extent`, or given up where
    /// nothing within it drew.
    fn close(&mut self, at: usize, extent: Option<Extent>) -> Result<Option<Extent>, DrawingError> {
        match extent {
            Some(extent) => self.out.finish(at, extent)?,
            None => self.out.0.truncate(at),
        }
        Ok(extent)
    }

    /// A shape: its fill and its stroke in the order it paints them, then
    /// its arrowheads, grouped under its name where they take more than one
    /// object.
    fn shape(
        &mut self,
        name: Option<&str>,
        path: &Path,
        style: &Style,
        placed: Affine,
    ) -> Result<Option<Extent>, DrawingError> {
        let map = placed.then(self.to_draw);
        let even_odd = style.fill_rule == FillRule::EvenOdd;
        let fill = self.colour(&style.fill).map(|fill| Piece {
            path,
            fill,
            even_odd,
            pen: None,
        });
        let (stroked, heads) = self.stroked(path, &style.stroke, map)?;
        let stroke = match &stroked {
            Stroked::Nothing => None,
            Stroked::Pen(pen) => Some(Piece {
                path,
                fill: NO_COLOUR,
                even_odd,
                pen: Some(pen),
            }),
            Stroked::Area(area, colour) => Some(Piece {
                path: area,
                fill: *colour,
                even_odd: false,
                pen: None,
            }),
        };
        let heads = heads.as_ref().map(|(heads, colour)| Piece {
            path: heads,
            fill: *colour,
            even_odd: false,
            pen: None,
        });
        let [first, second] = match (style.paint_order, fill, stroke) {
            (PaintOrder::FillFirst, Some(fill), Some(Piece { pen: Some(pen), .. })) => [
                Some(Piece {
                    pen: Some(pen),
                    ..fill
                }),
                None,
            ],
            (PaintOrder::FillFirst, fill, stroke) => [fill, stroke],
            (PaintOrder::StrokeFirst, fill, stroke) => [stroke, fill],
        };
        let mut pieces = [first, second, heads].into_iter().flatten();
        let Some(only) = pieces.next() else {
            return Ok(None);
        };
        let Some(next) = pieces.next() else {
            return self.path(&only, map);
        };
        let at = self.group(name)?;
        let mut extent = None;
        for piece in [only, next].into_iter().chain(pieces) {
            extent = Extent::joined(extent, self.path(&piece, map)?);
        }
        self.close(at, extent)
    }

    /// How `stroke` along `path`, mapped by `map`, is written, and the
    /// arrowheads written as shapes of their own with their colour word.
    fn stroked(
        &mut self,
        path: &Path,
        stroke: &Stroke,
        map: Affine,
    ) -> Result<(Stroked, Option<(Path, u32)>), DrawingError> {
        let Some(colour) = stroke
            .paints()
            .then(|| self.colour(&stroke.paint))
            .flatten()
        else {
            return Ok((Stroked::Nothing, None));
        };
        let native = uniform_scale(map).filter(|_| !mitres_differ(path, stroke));
        let (stroked, capped) = match native {
            Some(scale) => {
                let (pen, capped) = pen_for(path, stroke, colour, scale)?;
                (Stroked::Pen(pen), capped)
            }
            None => (Stroked::Area(stroke_area(path, stroke)?, colour), false),
        };
        let mut heads = Path::new();
        if !capped {
            for head in arrowheads(path, stroke)?.into_iter().flatten() {
                if !fallible::push(&mut heads.subpaths, head) {
                    return Err(DrawingError::OutOfMemory);
                }
            }
        }
        if heads.subpaths.is_empty() {
            return Ok((stroked, None));
        }
        let head = self.colour(&Paint::Colour(stroke.head_colour()));
        Ok((stroked, head.map(|head| (heads, head))))
    }

    /// A text: each run of a line an object, every glyph of a text along a
    /// path an object turned to the path, grouped under its name. Draw fills
    /// text in one colour, so an outlined one is filled in its outline's.
    fn text(
        &mut self,
        name: Option<&str>,
        text: &Text,
        style: &Style,
        placed: Affine,
    ) -> Result<Option<Extent>, DrawingError> {
        let outlined = style.stroke.paints()
            || text
                .runs
                .iter()
                .any(|run| matches!(run.style.stroke, Some(RunPaint::Colour(_))));
        self.dropped.text_outlines |= outlined;
        self.dropped.letter_spacing |= text.letter_spacing != 0.0
            || text.runs.iter().any(|run| {
                run.style
                    .letter_spacing
                    .is_some_and(|spacing| spacing != 0.0)
            });
        let map = placed.then(self.to_draw);
        match &text.baseline {
            Baseline::Lines { .. } => self.text_lines(name, text, style, map),
            Baseline::Path(_) => self.text_along(name, text, style, map),
        }
    }

    /// A text set in lines: one object for each run of each line, from the
    /// pen the line sets it at.
    fn text_lines(
        &mut self,
        name: Option<&str>,
        text: &Text,
        style: &Style,
        map: Affine,
    ) -> Result<Option<Extent>, DrawingError> {
        let parts = text
            .line_ranges()
            .map(|range| {
                text.faced(range)
                    .fold((0, None), |(count, last), (_, _, face)| {
                        if count > 0 && last == Some(face.run) {
                            (count, last)
                        } else {
                            (count + 1, Some(face.run))
                        }
                    })
            })
            .map(|(count, _)| count)
            .sum::<usize>();
        if parts == 0 {
            return Ok(None);
        }
        let group = if parts > 1 {
            Some(self.group(name)?)
        } else {
            None
        };
        let mut extent = None;
        for (index, range) in text.line_ranges().enumerate() {
            if range.is_empty() {
                continue;
            }
            let bounds = Extent::of(
                text.estimated_line_bounds(index, range.clone())
                    .transformed(map),
            )?;
            let pens = text.pens(self.glyphs, index, range.clone())?;
            let mut first = 0;
            while first + 1 < pens.len() {
                let run = pens[first].face.run;
                let last = pens[first..pens.len() - 1]
                    .iter()
                    .position(|pen| pen.face.run != run)
                    .map_or(pens.len() - 1, |offset| first + offset);
                let pen = pens[first];
                let part = &text.content[pen.offset..pens[last].offset];
                let rise = Point::new(pen.point.x, pen.point.y - pen.face.rise);
                if let Some(lettering) =
                    self.lettering(text, style, pen.face, Affine::IDENTITY, map)?
                {
                    self.line(part, &lettering, map.map(rise), bounds)?;
                    extent = Extent::joined(extent, Some(bounds));
                }
                first = last;
            }
        }
        match group {
            Some(at) => self.close(at, extent),
            None => Ok(extent),
        }
    }

    /// A text set along a path: one object for each glyph, turned as the
    /// path turns it.
    fn text_along(
        &mut self,
        name: Option<&str>,
        text: &Text,
        style: &Style,
        map: Affine,
    ) -> Result<Option<Extent>, DrawingError> {
        let setting = text.set(self.glyphs)?;
        let group = if setting.glyphs.len() > 1 {
            Some(self.group(name)?)
        } else {
            None
        };
        let mut extent = None;
        for placed in &setting.glyphs {
            let rest = &text.content[placed.offset..];
            let Some(character) = rest.chars().next() else {
                continue;
            };
            let at = placed.at;
            let scale = sqrt(fabs(at.determinant()));
            if character == ' ' || scale <= 0.0 {
                continue;
            }
            let turn = Affine {
                a: at.a / scale,
                b: at.b / scale,
                c: at.c / scale,
                d: at.d / scale,
                e: 0.0,
                f: 0.0,
            };
            let face = crate::text::TextFace {
                size: scale,
                ..text.face(placed.run)
            };
            let Some(lettering) = self.lettering(text, style, face, turn, map)? else {
                continue;
            };
            let bounds = Extent::of(placed.glyph.ink().transformed(at).transformed(map))?;
            let pen = at.map(Point::ZERO);
            self.line(
                &rest[..character.len_utf8()],
                &lettering,
                map.map(pen),
                bounds,
            )?;
            extent = Extent::joined(extent, Some(bounds));
        }
        match group {
            Some(at) => self.close(at, extent),
            None => Ok(extent),
        }
    }

    /// How characters of `face`, turned by `turn` in the text's own space,
    /// are set on the paper `map` takes the text to: their colour (`None`
    /// where they paint nothing), font, size and matrix.
    fn lettering(
        &mut self,
        text: &Text,
        style: &Style,
        face: crate::text::TextFace<'_>,
        turn: Affine,
        map: Affine,
    ) -> Result<Option<Lettering>, DrawingError> {
        let overrides = face
            .run
            .and_then(|run| text.runs.get(run as usize))
            .map(|run| (run.style.fill, run.style.stroke));
        let (fill, stroke) = overrides.unwrap_or((None, None));
        let fill = fill.map_or_else(|| style.fill.clone(), RunPaint::paint);
        let stroke = stroke.map_or_else(|| style.stroke.paint.clone(), RunPaint::paint);
        let colour = match self.colour(&fill) {
            Some(colour) => colour,
            None => match self.colour(&stroke) {
                Some(colour) => colour,
                None => return Ok(None),
            },
        };
        let linear = turn.then(Affine {
            e: 0.0,
            f: 0.0,
            ..map
        });
        // Draw's matrix maps a space whose y runs up, as the paper's does.
        let upward = Affine {
            a: linear.a / UNITS_PER_PIXEL,
            b: linear.b / UNITS_PER_PIXEL,
            c: -linear.c / UNITS_PER_PIXEL,
            d: -linear.d / UNITS_PER_PIXEL,
            e: 0.0,
            f: 0.0,
        };
        let scale = sqrt(fabs(upward.determinant()));
        let size = size_unit(face.size * UNITS_PER_PIXEL * scale)?;
        if size == 0 {
            return Ok(None);
        }
        let unscaled = Affine {
            a: upward.a / scale,
            b: upward.b / scale,
            c: upward.c / scale,
            d: upward.d / scale,
            ..upward
        };
        let untransformed = fabs(unscaled.a - 1.0) <= NEAR
            && fabs(unscaled.b) <= NEAR
            && fabs(unscaled.c) <= NEAR
            && fabs(unscaled.d - 1.0) <= NEAR;
        let (font, asked_first) = Font::of(face.family, face.weight, face.italic);
        self.dropped.fonts |= !asked_first;
        Ok(Some(Lettering {
            colour,
            ground: word_of(self.behind),
            number: self.font_number(font)?,
            size,
            matrix: (!untransformed).then_some(unscaled),
        }))
    }

    /// One line of text, its baseline starting at `start` on the paper.
    fn line(
        &mut self,
        line: &str,
        lettering: &Lettering,
        start: Point,
        extent: Extent,
    ) -> Result<(), DrawingError> {
        let kind = if lettering.matrix.is_some() {
            TRANSFORMED_TEXT
        } else {
            TEXT
        };
        let at = self.out.begin(kind)?;
        if let Some(matrix) = lettering.matrix {
            // The format holds the matrix's translation zero, and its text
            // unkerned and left to right.
            self.matrix(Affine {
                e: 0.0,
                f: 0.0,
                ..matrix
            })?;
            self.out.bytes(&[0; 4])?;
        }
        self.out.word(lettering.colour)?;
        self.out.word(lettering.ground)?;
        self.out.word(lettering.number)?;
        self.out.word(lettering.size)?;
        self.out.word(lettering.size)?;
        self.out.int(unit(start.x)?)?;
        self.out.int(unit(start.y)?)?;
        self.out.reserve(line.len() + 1)?;
        for character in line.chars() {
            if character == '\t' {
                for _ in 0..TAB_SPACES {
                    self.out.bytes(b" ")?;
                }
                continue;
            }
            let byte = latin1_byte(character);
            self.dropped.characters |= byte.is_none();
            self.out.bytes(&[byte.unwrap_or(b'?')])?;
        }
        self.out.bytes(&[0])?;
        self.out.finish(at, extent)
    }

    /// The number the font table gives `font`, numbering it if it is new.
    fn font_number(&mut self, font: Font) -> Result<u32, DrawingError> {
        let index = if let Some(index) = self.fonts.iter().position(|&held| held == font) {
            index
        } else {
            if !fallible::push(&mut self.fonts, font) {
                return Err(DrawingError::OutOfMemory);
            }
            self.fonts.len() - 1
        };
        u32::try_from(index + 1).map_err(|_| DrawingError::TooComplex)
    }

    /// The colour word `paint` is written in — a gradient its average
    /// colour, a translucent colour opaque, each noted — or `None` where it
    /// paints nothing.
    fn colour(&mut self, paint: &Paint) -> Option<u32> {
        if !paint.paints() {
            return None;
        }
        let colour = match paint {
            Paint::None => return None,
            Paint::Colour(colour) => *colour,
            Paint::Gradient(gradient) => {
                self.dropped.gradients = true;
                self.dropped.transparency |=
                    gradient.stops.iter().any(|stop| stop.colour.a < u8::MAX);
                average(gradient)
            }
        };
        self.dropped.transparency |= colour.a < u8::MAX;
        Some(word_of(colour))
    }

    /// `piece` as a path object, mapped onto the paper by `map`; nothing for
    /// a path with nothing in it.
    fn path(&mut self, piece: &Piece<'_>, map: Affine) -> Result<Option<Extent>, DrawingError> {
        if piece.path.subpaths.is_empty() {
            return Ok(None);
        }
        let at = self.out.begin(PATH)?;
        let pen = piece.pen;
        self.out.word(piece.fill)?;
        self.out.word(pen.map_or(NO_COLOUR, |pen| pen.colour))?;
        self.out.word(pen.map_or(0, |pen| pen.width))?;
        let winding = if piece.even_odd { WINDING_EVEN_ODD } else { 0 };
        self.out.word(pen.map_or(0, Pen::style) | winding)?;
        if let Some(dashes) = pen.and_then(|pen| pen.dashes.as_ref()) {
            self.out.word(dashes.offset)?;
            self.out
                .word(u32::try_from(dashes.lengths.len()).map_err(|_| DrawingError::TooComplex)?)?;
            for &length in &dashes.lengths {
                self.out.word(length)?;
            }
        }
        let hull = self.components(piece.path, map)?;
        let extent = Extent::of(hull.inflated(pen.map_or(0.0, Pen::reach)))?;
        self.out.finish(at, extent)?;
        Ok(Some(extent))
    }

    /// `path`'s components mapped by `map`, and the bounds of every point
    /// written, in draw units.
    fn components(&mut self, path: &Path, map: Affine) -> Result<Bounds, DrawingError> {
        let mut hull = Bounds::EMPTY;
        for subpath in &path.subpaths {
            let start = self.component(MOVE, &[subpath.start], map, &mut hull)?;
            let mut last = start;
            if subpath.elements.is_empty() {
                // Every sub-path of a Draw file draws a segment at least.
                last = self.component(LINE, &[subpath.start], map, &mut hull)?;
            }
            for element in &subpath.elements {
                last = match *element {
                    PathEl::Line(to) => self.component(LINE, &[to], map, &mut hull)?,
                    PathEl::Cubic(from, to, end) => {
                        self.component(CURVE, &[from, to, end], map, &mut hull)?
                    }
                };
            }
            if subpath.closed {
                // Draw closes a shape with its closing edge drawn to its
                // start, as its own files do.
                if last != start {
                    self.component(LINE, &[subpath.start], map, &mut hull)?;
                }
                self.out.word(CLOSE)?;
            }
        }
        self.out.word(END)?;
        Ok(hull)
    }

    /// One component of `tag` through `points`, mapped by `map`; where it
    /// ends, in draw units.
    fn component(
        &mut self,
        tag: u32,
        points: &[Point],
        map: Affine,
        hull: &mut Bounds,
    ) -> Result<(i32, i32), DrawingError> {
        self.out.word(tag)?;
        let mut last = (0, 0);
        for &point in points {
            let mapped = map.map(point);
            last = (unit(mapped.x)?, unit(mapped.y)?);
            self.out.int(last.0)?;
            self.out.int(last.1)?;
            *hull = hull.union_point(Point::new(f64::from(last.0), f64::from(last.1)));
        }
        Ok(last)
    }
}

/// The pen Draw strokes `path` with as `stroke` does under a map that
/// stretches every length by `scale`, and whether its caps draw the
/// stroke's arrowheads.
fn pen_for(
    path: &Path,
    stroke: &Stroke,
    colour: u32,
    scale: f64,
) -> Result<(Pen, bool), DrawingError> {
    let dashes = dash_pattern(stroke, scale)?;
    let heads = head_caps(path, stroke, dashes.is_none());
    let plain = cap_code(stroke.cap);
    let pen = Pen {
        colour,
        width: size_unit(stroke.width * scale)?.max(1),
        join: match stroke.join {
            Join::Miter => JOIN_MITRE,
            Join::Round => JOIN_ROUND,
            Join::Bevel => JOIN_BEVEL,
        },
        caps: heads.unwrap_or(Caps {
            start: plain,
            end: plain,
            triangle: (0, 0),
        }),
        dashes,
    };
    Ok((pen, heads.is_some()))
}

/// The caps that draw `stroke`'s arrowheads on `path` as Draw's own do, or
/// `None` where they cannot: Draw caps every end of every sub-path and dash,
/// so only one open sub-path, `solid`; an arrowhead is drawn over its
/// stroke's own cap, so only butt ends; and only arrowheads a cap is — a
/// triangle Draw's sixteenths can size, or a disc or a square a stroke's
/// width across.
fn head_caps(path: &Path, stroke: &Stroke, solid: bool) -> Option<Caps> {
    let (start, end) = (
        stroke.draws_head(&stroke.start),
        stroke.draws_head(&stroke.end),
    );
    let one_open = matches!(path.subpaths.as_slice(), [only] if !only.closed);
    if !(start || end) || !one_open || !solid || stroke.cap != Cap::Butt {
        return None;
    }
    let mut triangle = None;
    let mut code = |head: &Arrowhead, drawn: bool| -> Option<u32> {
        if !drawn {
            return Some(CAP_BUTT);
        }
        let stroke_wide = fabs(head.width - 1.0) <= NEAR;
        match head.shape {
            ArrowShape::Circle if stroke_wide => Some(CAP_ROUND),
            ArrowShape::Square if stroke_wide => Some(CAP_SQUARE),
            ArrowShape::Triangle => {
                let sized = (sixteenths(head.width / 2.0)?, sixteenths(head.length)?);
                (*triangle.get_or_insert(sized) == sized).then_some(CAP_TRIANGLE)
            }
            _ => None,
        }
    };
    let start = code(&stroke.start, start)?;
    let end = code(&stroke.end, end)?;
    Some(Caps {
        start,
        end,
        triangle: triangle.unwrap_or((0, 0)),
    })
}

/// `widths` stroke widths in a triangle cap's sixteenths, where they fit.
fn sixteenths(widths: f64) -> Option<u32> {
    u32::try_from(round_i32(widths * TRIANGLE_UNIT))
        .ok()
        .filter(|&sixteenths| sixteenths <= TRIANGLE_SIZE)
}

/// The code Draw names `cap` by.
const fn cap_code(cap: Cap) -> u32 {
    match cap {
        Cap::Butt => CAP_BUTT,
        Cap::Round => CAP_ROUND,
        Cap::Square => CAP_SQUARE,
    }
}

/// `stroke`'s dash pattern in draw units, under a map stretching lengths by
/// `scale`; `None` for a solid stroke, or one whose pattern draws nothing
/// apart, which the drawing strokes solid.
fn dash_pattern(stroke: &Stroke, scale: f64) -> Result<Option<Dashes>, DrawingError> {
    let period: f64 = stroke.dashes.iter().sum();
    if period <= 0.0 {
        return Ok(None);
    }
    let mut lengths = Vec::new();
    if !fallible::reserve(&mut lengths, stroke.dashes.len()) {
        return Err(DrawingError::OutOfMemory);
    }
    for &dash in stroke.dashes.iter() {
        lengths.push(size_unit(dash * scale)?);
    }
    if lengths.iter().all(|&length| length == 0) {
        return Ok(None);
    }
    // An odd pattern runs twice over before it repeats, as both formats
    // dash it.
    let period = if stroke.dashes.len() % 2 == 1 {
        2.0 * period
    } else {
        period
    };
    let into = stroke.dash_offset - floor(stroke.dash_offset / period) * period;
    Ok(Some(Dashes {
        offset: size_unit(into * scale)?,
        lengths,
    }))
}

/// The scale `map` stretches every length by, where it stretches every
/// length alike: a rotation, a reflection or a uniform scale, unskewed.
fn uniform_scale(map: Affine) -> Option<f64> {
    let scale = sqrt(fabs(map.determinant()));
    let near = |a: f64, b: f64| fabs(a - b) <= NEAR * scale;
    let turned = near(map.a, map.d) && near(map.b, -map.c);
    let reflected = near(map.a, -map.d) && near(map.b, map.c);
    (scale > 0.0 && (turned || reflected)).then_some(scale)
}

/// Whether Draw's fixed mitre limit would cut a corner of `path` otherwise
/// than `stroke`'s own: a corner whose mitre reaches between the two limits,
/// or so near either that the two strokers may round it apart.
fn mitres_differ(path: &Path, stroke: &Stroke) -> bool {
    if stroke.join != Join::Miter {
        return false;
    }
    let low = fmin(stroke.miter_limit, MITRE_LIMIT) * (1.0 - NEAR);
    let high = fmax(stroke.miter_limit, MITRE_LIMIT) * (1.0 + NEAR);
    // A corner's mitre, in half the stroke's width, as the stroker measures
    // it: the secant of half the angle the path turns through.
    let differs = |arriving: Point, leaving: Point| {
        let turn = arriving.dot(leaving);
        if turn <= -1.0 || turn >= 1.0 - NEAR {
            return false;
        }
        let mitre = sqrt(2.0 / (1.0 + turn));
        mitre > low && mitre <= high
    };
    path.subpaths.iter().any(|subpath| {
        let mut first = None;
        let mut arriving: Option<Point> = None;
        for segment in subpath.segments() {
            let (leaves, arrives) = segment.tangents();
            let (Some(leaves), Some(arrives)) = (leaves.normalised(), arrives.normalised()) else {
                continue;
            };
            if arriving.is_some_and(|arriving| differs(arriving, leaves)) {
                return true;
            }
            first.get_or_insert(leaves);
            arriving = Some(arrives);
        }
        subpath.closed
            && arriving
                .zip(first)
                .is_some_and(|(arriving, leaving)| differs(arriving, leaving))
    })
}

/// The colour a gradient averages along its length: each end's stop held
/// from that end, and the colours between two stops running evenly between
/// them.
fn average(gradient: &Gradient) -> Rgba {
    let mut sums = [0.0; 4];
    let mut add = |colour: Rgba, weight: f64| {
        for (sum, channel) in sums
            .iter_mut()
            .zip([colour.r, colour.g, colour.b, colour.a])
        {
            *sum += weight * f64::from(channel);
        }
    };
    if let (Some(first), Some(last)) = (gradient.stops.first(), gradient.stops.last()) {
        add(first.colour, first.offset);
        add(last.colour, 1.0 - last.offset);
    }
    for pair in gradient.stops.windows(2) {
        let half = (pair[1].offset - pair[0].offset) / 2.0;
        add(pair[0].colour, half);
        add(pair[1].colour, half);
    }
    let [r, g, b, a] =
        sums.map(|sum| u8::try_from(round_i32(sum).clamp(0, 255)).unwrap_or(u8::MAX));
    Rgba::new(r, g, b, a)
}

/// The eigen factors a sprite made of a picture's pixels is drawn at: two OS
/// units a pixel, the desktop's ninety to the inch.
const SPRITE_EIG: (u8, u8) = (1, 1);

/// The dots to the inch RISC OS takes a picture stating none to be at.
const DESKTOP_DPI: u32 = 90;

/// `pixels` as a 32-bit sprite, masked as they need: a mask bit where each
/// pixel is wholly shown or hidden, an eight-bit mask where any is partly.
fn sprite_of(pixels: &Surface) -> Result<Vec<u8>, DrawingError> {
    let survey = survey(pixels);
    let name = SpriteName::new("picture").ok_or(DrawingError::Invalid)?;
    tairix_image::encode_sprite(&SpriteInput::Picture {
        name,
        mode: SpriteMode::truecolour(SPRITE_EIG, !survey.binary),
        palette: &SpritePalette::Implied,
        masked: !survey.opaque,
        source: &SurfacePicture::new(pixels),
    })
    .map_err(|error| match error {
        EncodeError::OutOfMemory => DrawingError::OutOfMemory,
        EncodeError::TooLarge => DrawingError::TooComplex,
        _ => DrawingError::Invalid,
    })
}

/// `value`, in draw units, as the signed word a Draw file holds it in.
fn unit(value: f64) -> Result<i32, DrawingError> {
    if value.is_finite() && fabs(value) <= f64::from(i32::MAX) {
        Ok(round_i32(value))
    } else {
        Err(DrawingError::Invalid)
    }
}

/// `value`, a length in draw units, as the unsigned word a Draw file holds
/// it in.
fn size_unit(value: f64) -> Result<u32, DrawingError> {
    u32::try_from(unit(value)?).map_err(|_| DrawingError::Invalid)
}

/// The font table: each font numbered from one, in the order `fonts` holds
/// them.
fn font_table(file: &mut Out, fonts: &[Font]) -> Result<(), DrawingError> {
    let at = file.0.len();
    file.word(FONT_TABLE)?;
    file.word(0)?;
    for (index, font) in fonts.iter().enumerate() {
        file.bytes(&[u8::try_from(index + 1).map_err(|_| DrawingError::TooComplex)?])?;
        for piece in font.spelling() {
            file.bytes(piece.as_bytes())?;
        }
        file.bytes(&[0])?;
    }
    file.pad()?;
    file.sized(at)
}

/// The options: the paper, the grid in the unit the page is shown in, and
/// a drawing opened at full size to select in.
fn options(file: &mut Out, page: &Page, grid: Grid, paper: &Paper) -> Result<(), DrawingError> {
    let inches = matches!(page.unit, Unit::In | Unit::Pt | Unit::Pc | Unit::Px);
    let grid_unit = if inches { Unit::In } else { Unit::Cm };
    let limits = if paper.landscape {
        PAPER_LIMITS_SHOWN | LANDSCAPE
    } else {
        PAPER_LIMITS_SHOWN
    };
    let [spacing_high, spacing_low] = fpa_words(grid.spacing / grid_unit.pixels());
    let words = [
        OPTIONS,
        OPTIONS_SIZE,
        // The bounds an options object has no use for.
        0,
        0,
        0,
        0,
        (paper.number + 1) << 8,
        limits,
        spacing_high,
        spacing_low,
        grid.subdivisions,
        // A rectangular grid, adjusted to the zoom, neither shown nor locked.
        0,
        1,
        0,
        0,
        u32::from(!inches),
        // At full size, unlocked, the toolbox shown.
        1,
        1,
        0,
        1,
        SELECT_MODE,
        UNDO_BUFFER,
    ];
    for word in words {
        file.word(word)?;
    }
    Ok(())
}
