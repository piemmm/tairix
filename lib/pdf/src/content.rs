//! Content streams (ISO 32000-2 §8): a page's operators interpreted into its
//! display list.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::offset::{Cap, Join, StrokeStyle};
use tairix_bezier::{Affine, FillRule, Path, PathEl, Point, Subpath};
use tairix_collections::{try_arc, OrderedMap, SharedSlice};
use tairix_colour::Rgba;
use tairix_util::fallible::try_push;

use crate::colour::{Space, Srgb};
use crate::display::{DisplayList, Gradient, GradientKind, Item, Losses, Lost, Paint, Pen, Stop};
use crate::document::Document;
use crate::error::tolerated;
use crate::filter::Decoded;
use crate::function::{values, Values, MAX_COMPONENTS};
use crate::image;
use crate::lex::{Lexer, Token};
use crate::limits::{MAX_CONTENT_BYTES, MAX_ITEMS};
use crate::object::{Dict, Name, ObjRef, Object, Resolved, Stream};
use crate::page::{Page, Rect};
use crate::parse;
use crate::shading::{self, level, Shading, MAX_STOPS};
use crate::PdfError;

/// The most operators a page's content runs, its forms' included.
const MAX_OPERATORS: usize = 4_000_000;

/// The most path points and gradient stops a page's display list holds, the
/// path being built counted with them.
const MAX_POINTS: usize = 2_000_000;

/// The deepest `q` saves graphics states: one past it is counted but saves
/// nothing, so unbalanced content cannot grow the stack without bound.
const MAX_SAVES: usize = 64;

/// The deepest forms paint forms.
const MAX_FORM_DEPTH: usize = 16;

/// The most operands one operator is given; past it the operands are dropped.
const MAX_OPERANDS: usize = 64;

/// What a resource was read to, damage among it: kept either way, so one
/// used again is not read again.
#[derive(Clone)]
enum Read<T> {
    Held(T),
    Damaged,
}

impl<T> Read<T> {
    /// What `read` came to, its damage kept as damage.
    fn of(read: Option<T>) -> Self {
        read.map_or(Self::Damaged, Self::Held)
    }

    /// What was read, to be added to, or the damage it was.
    fn held_mut(&mut self) -> Result<&mut T, PdfError> {
        match self {
            Self::Held(held) => Ok(held),
            Self::Damaged => Err(PdfError::Malformed),
        }
    }
}

impl<T: Clone> Read<T> {
    /// What was read, or the damage it was.
    fn held(&self) -> Result<T, PdfError> {
        match self {
            Self::Held(held) => Ok(held.clone()),
            Self::Damaged => Err(PdfError::Malformed),
        }
    }
}

/// A resources dictionary's categories, each read once: the dictionaries its
/// names are looked up in.
#[derive(Default)]
struct Categories {
    spaces: Dict,
    patterns: Dict,
    shadings: Dict,
    states: Dict,
    xobjects: Dict,
    properties: Dict,
}

impl Categories {
    /// `resources`' categories, a category that is a reference resolved and
    /// one that cannot be read held as empty.
    fn of(document: &Document, resources: Dict) -> Result<Self, PdfError> {
        let mut categories = Self::default();
        for (key, entry) in resources.into_entries() {
            let held = match key.as_bytes() {
                b"ColorSpace" => &mut categories.spaces,
                b"Pattern" => &mut categories.patterns,
                b"Shading" => &mut categories.shadings,
                b"ExtGState" => &mut categories.states,
                b"XObject" => &mut categories.xobjects,
                b"Properties" => &mut categories.properties,
                _ => continue,
            };
            let entry = match entry {
                Object::Ref(_) => {
                    tolerated(document.resolve(&entry).and_then(Resolved::into_owned))?
                }
                entry => Some(entry),
            };
            if let Some(Object::Dict(dict)) = entry {
                *held = dict;
            }
        }
        Ok(categories)
    }
}

/// A form's decoded content, its geometry, and its own resources with the
/// scope read through them.
struct Form {
    content: Vec<u8>,
    matrix: Affine,
    bbox: Option<Rect>,
    grouped: bool,
    resources: Option<(Categories, Scope)>,
}

/// A resources dictionary, by its place in the interpreter's [`Named`] list.
#[derive(Copy, Clone, Eq, Ord, PartialEq, PartialOrd)]
struct Scope(usize);

/// A content stream's resources, and the scope what it reads by name through
/// them is kept in.
#[derive(Copy, Clone)]
struct Resources<'r> {
    categories: &'r Categories,
    scope: Scope,
}

/// What one resources dictionary's names were read to, so a resource used
/// again is not read again. A name means what its own dictionary says, so
/// each keeps its own.
#[derive(Default)]
struct Named {
    spaces: OrderedMap<Name, Read<Arc<Space>>>,
    /// `None` for a shading pattern no gradient holds.
    patterns: OrderedMap<Name, Read<Option<Arc<Pattern>>>>,
    /// `None` for a shading no gradient holds.
    shadings: OrderedMap<Name, Read<Option<Arc<Shading>>>>,
    states: OrderedMap<Name, Read<Arc<Setting>>>,
    xobjects: OrderedMap<Name, Read<Arc<External>>>,
    /// Whether the content each optional content name governs is shown.
    properties: OrderedMap<Name, Read<bool>>,
}

/// What an external object's name came to.
struct External {
    stream: Stream,
    /// Whether the default configuration shows it.
    shown: bool,
}

/// What a fill or stroke colour is.
#[derive(Clone)]
enum Ink {
    Srgb(Srgb),
    Pattern {
        pattern: Arc<Pattern>,
        tint: Option<Srgb>,
    },
    /// The colorant `/None`, or a pattern that could not be read.
    Nothing,
}

/// A colour and the space it was set in.
#[derive(Clone)]
struct Colour {
    space: Arc<Space>,
    ink: Ink,
}

/// A pattern, as a fill or stroke paints with it.
enum Pattern {
    /// A shading, and its matrix into its parent content stream's space.
    Shading {
        shading: Arc<Shading>,
        matrix: Affine,
    },
    /// A tiling pattern, which no paint holds: an uncoloured one is painted
    /// with its colour, a coloured one not at all.
    Tiling { coloured: bool },
}

/// A soft mask that is one gradient's alpha: how a translucent gradient is
/// written, and so read back as one.
#[derive(Clone)]
struct Mask {
    gradient: Arc<Gradient>,
    /// The mask's shading space into the page's default space.
    space: Affine,
}

/// What a graphics state parameter dictionary sets, read once however often
/// it is set.
#[derive(Default)]
struct Setting {
    width: Option<f64>,
    cap: Option<Cap>,
    join: Option<Join>,
    miter_limit: Option<f64>,
    dash: Option<(SharedSlice<f64>, f64)>,
    fill_alpha: Option<f64>,
    stroke_alpha: Option<f64>,
    /// Whether it sets a blend mode other than normal, which no paint holds.
    blends: bool,
    mask: Option<SoftMask>,
}

/// The soft mask a graphics state parameter dictionary sets.
enum SoftMask {
    /// `/None`.
    Cleared,
    /// One no gradient's alpha holds.
    Lost,
    /// One luminosity shading's gradient, and the matrix of the group it is
    /// painted in.
    Gradient {
        gradient: Arc<Gradient>,
        matrix: Affine,
    },
}

/// The graphics state.
#[derive(Clone)]
struct State {
    /// User space into the page's default space.
    ctm: Affine,
    /// Clips begun since this state was saved, ended when it is restored.
    clips: usize,
    /// Whether a clip lets nothing through: one to no path, or one that
    /// could not be placed.
    clipped_out: bool,
    fill: Colour,
    stroke: Colour,
    pen: Pen,
    fill_alpha: f64,
    stroke_alpha: f64,
    mask: Option<Mask>,
}

impl State {
    fn new(gray: &Arc<Space>) -> Self {
        let black = Colour {
            space: Arc::clone(gray),
            ink: Ink::Srgb([0.0; 3]),
        };
        Self {
            ctm: Affine::IDENTITY,
            clips: 0,
            clipped_out: false,
            fill: black.clone(),
            stroke: black,
            pen: Pen {
                style: StrokeStyle {
                    width: 1.0,
                    join: Join::Miter,
                    miter_limit: 10.0,
                    start_cap: Cap::Butt,
                    end_cap: Cap::Butt,
                },
                dashes: SharedSlice::new(),
                dash_offset: 0.0,
            },
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            mask: None,
        }
    }
}

/// Which of a path's paintings an operator makes.
#[derive(Copy, Clone)]
struct Painting {
    close: bool,
    fill: Option<FillRule>,
    stroke: bool,
}

/// Where the content stream being run began: what its own `Q` and `EMC`
/// may not reach past, so a form cannot close what its caller opened.
#[derive(Copy, Clone, Default)]
struct Floor {
    saves: usize,
    marked: usize,
}

/// Where an inline image was read: its scope, its content stream — the page's
/// or a form's — and its place there.
type Inline = (Scope, Option<u32>, usize);

/// Where an image was read from: an image object, or an inline image.
#[derive(Copy, Clone, Eq, Ord, PartialEq, PartialOrd)]
enum Origin {
    Object(u32),
    Inline(Inline),
}

/// What an image was read to: a picture, or a stencil's samples and the
/// picture they paint in each colour they were painted in.
enum Painted {
    Picture(Arc<image::Picture>),
    Stencil {
        stencil: image::Stencil,
        coloured: OrderedMap<[u8; 3], Arc<image::Picture>>,
    },
}

impl Painted {
    /// What image dictionary `dict` over samples `decoded` paints as `kind`.
    fn read(
        document: &Document,
        dict: &Dict,
        decoded: Decoded,
        kind: &image::Kind,
    ) -> Result<Self, PdfError> {
        Ok(match kind {
            image::Kind::Stencil => Self::Stencil {
                stencil: image::stencil(document, dict, &decoded)?,
                coloured: OrderedMap::new(),
            },
            image::Kind::Colour(space) => {
                Self::Picture(try_arc(image::picture(document, dict, decoded, space)?)?)
            }
        })
    }

    /// The picture it paints with the fill colour `fill`: a stencil's made
    /// once in each colour, and in black where the fill is a paint no picture
    /// holds, the loss recorded.
    fn picture(
        &mut self,
        document: &Document,
        fill: Option<Srgb>,
        lost: &mut Losses,
    ) -> Result<Arc<image::Picture>, PdfError> {
        match self {
            Self::Picture(picture) => Ok(Arc::clone(picture)),
            Self::Stencil { stencil, coloured } => {
                let colour = fill.unwrap_or_else(|| {
                    lost.insert(Lost::Images);
                    [0.0; 3]
                });
                let key = colour.map(level);
                if let Some(picture) = coloured.get(&key) {
                    return Ok(Arc::clone(picture));
                }
                let picture = try_arc(stencil.painted(document, colour)?)?;
                coloured.try_insert(key, Arc::clone(&picture))?;
                Ok(picture)
            }
        }
    }
}

/// The display list of `page`.
pub(crate) fn interpret(page: &Page<'_>) -> Result<DisplayList, PdfError> {
    let document = page.document();
    let mut interpreter = Interpreter::new(document, page)?;
    let content = page_content(document, &page.dict, &mut interpreter.lost)?;
    let categories = Categories::of(document, page.resources()?)?;
    let resources = Resources {
        categories: &categories,
        scope: interpreter.scope()?,
    };
    interpreter.run(&content, resources, Affine::IDENTITY)?;
    interpreter.finish()?;
    let annotated = document
        .get(&page.dict, b"Annots")?
        .as_array()
        .is_some_and(|annotations| !annotations.is_empty());
    if annotated {
        interpreter.lost.insert(Lost::Annotations);
    }
    Ok(DisplayList {
        size: page.size(),
        items: interpreter.items,
        lost: interpreter.lost,
    })
}

/// A page's content: its one stream, or its streams joined by white space. A
/// stream that cannot be read is left out, the loss recorded.
fn page_content(document: &Document, page: &Dict, lost: &mut Losses) -> Result<Vec<u8>, PdfError> {
    let contents = document.get(page, b"Contents")?;
    let streams: &[Object] = match &*contents {
        Object::Array(streams) => streams,
        Object::Stream(_) => core::slice::from_ref(&*contents),
        _ => &[],
    };
    let mut content: Vec<u8> = Vec::new();
    for stream in streams {
        let read = document
            .resolve(stream)
            .and_then(|stream| match stream.as_stream() {
                Some(stream) => document.decode(stream),
                None => Err(PdfError::Malformed),
            });
        let Some(decoded) = tolerated(read)? else {
            lost.insert(Lost::Damaged);
            continue;
        };
        if decoded.damaged {
            lost.insert(Lost::Damaged);
        }
        // The first stream is taken as it was decoded rather than copied.
        if content.is_empty() {
            content = decoded.bytes;
            content.try_reserve(1)?;
        } else {
            content.try_reserve(decoded.bytes.len() + 1)?;
            content.extend_from_slice(&decoded.bytes);
        }
        content.push(b'\n');
    }
    Ok(content)
}

struct Interpreter<'d> {
    document: &'d Document,
    /// The page's default space into its own.
    page: Affine,
    size: (f64, f64),
    items: Vec<Item>,
    lost: Losses,
    state: State,
    saved: Vec<State>,
    /// `q`s past the deepest kept, each restored by a `Q` that restores
    /// nothing.
    unsaved: usize,
    /// Where the content stream being run began.
    floor: Floor,
    /// The form whose content is being run, or none for the page's.
    content: Option<u32>,
    path: Path,
    subpath: Option<Subpath>,
    /// Where the next segment starts, once a path has begun.
    current: Option<Point>,
    clip: Option<FillRule>,
    operators: usize,
    /// Content bytes lexed: the page's own, and a form's each time it is
    /// painted.
    lexed: usize,
    points: usize,
    /// Points of the path being built.
    built: usize,
    /// The forms being painted, innermost last.
    forms: Vec<ObjRef>,
    /// Marked-content sections open, each whether it hides what it holds.
    marked: Vec<bool>,
    hiding: usize,
    /// What a form read to, by object, what an image read to, by where it
    /// was read, and where each inline image ends: so none painted again is
    /// read again.
    forms_read: OrderedMap<u32, Read<Arc<Form>>>,
    images: OrderedMap<Origin, Read<Painted>>,
    inline_ends: OrderedMap<Inline, usize>,
    /// What each resources dictionary's names were read to, by [`Scope`].
    named: Vec<Named>,
    /// The device spaces, shared by every colour set in one.
    gray: Arc<Space>,
    rgb: Arc<Space>,
    cmyk: Arc<Space>,
}

impl<'d> Interpreter<'d> {
    fn new(document: &'d Document, page: &Page<'_>) -> Result<Self, PdfError> {
        let gray = try_arc(Space::Gray)?;
        Ok(Self {
            document,
            page: page.transform(),
            size: page.size(),
            items: Vec::new(),
            lost: Losses::default(),
            state: State::new(&gray),
            saved: Vec::new(),
            unsaved: 0,
            floor: Floor::default(),
            content: None,
            path: Path::new(),
            subpath: None,
            current: None,
            clip: None,
            operators: 0,
            lexed: 0,
            points: 0,
            built: 0,
            forms: Vec::new(),
            marked: Vec::new(),
            hiding: 0,
            forms_read: OrderedMap::new(),
            images: OrderedMap::new(),
            inline_ends: OrderedMap::new(),
            named: Vec::new(),
            gray,
            rgb: try_arc(Space::Rgb)?,
            cmyk: try_arc(Space::Cmyk)?,
        })
    }

    /// A scope for a resources dictionary read for the first time.
    fn scope(&mut self) -> Result<Scope, PdfError> {
        try_push(&mut self.named, Named::default())?;
        Ok(Scope(self.named.len() - 1))
    }

    /// What `scope`'s names were read to.
    fn named(&mut self, scope: Scope) -> Result<&mut Named, PdfError> {
        // Only `scope` makes a scope, so every one is found.
        self.named.get_mut(scope.0).ok_or(PdfError::Malformed)
    }

    /// Interpret `content` with `resources`, `base` its default space into the
    /// page's. Damage is passed over and recorded; only a containment bound or
    /// the heap's refusal stops it.
    fn run(
        &mut self,
        content: &[u8],
        resources: Resources<'_>,
        base: Affine,
    ) -> Result<(), PdfError> {
        self.lexed = self.lexed.saturating_add(content.len());
        if self.lexed > MAX_CONTENT_BYTES {
            return Err(PdfError::TooComplex);
        }
        let outer = self.floor;
        self.floor = Floor {
            saves: self.saved.len() + self.unsaved,
            marked: self.marked.len(),
        };
        let ran = self.operators_of(content, resources, base);
        while self.saved.len() + self.unsaved > self.floor.saves {
            self.restore()?;
        }
        while self.marked.len() > self.floor.marked {
            self.end_marked();
        }
        self.floor = outer;
        ran
    }

    /// The operators of `content`, interpreted.
    fn operators_of(
        &mut self,
        content: &[u8],
        resources: Resources<'_>,
        base: Affine,
    ) -> Result<(), PdfError> {
        let mut lexer = Lexer::new(content, 0);
        let mut operands: Vec<Object> = Vec::new();
        loop {
            let at = lexer.at();
            let token = match lexer.next() {
                Ok(Some(token)) => token,
                Ok(None) => return Ok(()),
                Err(PdfError::OutOfMemory) => return Err(PdfError::OutOfMemory),
                Err(_) => {
                    self.lost.insert(Lost::Damaged);
                    operands.clear();
                    continue;
                }
            };
            let Token::Keyword(operator) = token else {
                match parse::operand(&mut lexer, token) {
                    Ok(operand) if operands.len() < MAX_OPERANDS => {
                        try_push(&mut operands, operand)?;
                    }
                    Err(PdfError::OutOfMemory) => return Err(PdfError::OutOfMemory),
                    _ => {
                        self.lost.insert(Lost::Damaged);
                        operands.clear();
                    }
                }
                continue;
            };
            self.operators += 1;
            if self.operators > MAX_OPERATORS {
                return Err(PdfError::TooComplex);
            }
            if operator == b"BI" {
                let read = self.inline_image(&mut lexer, resources, at);
                self.image_outcome(read)?;
            } else if tolerated(self.operator(operator, &operands, resources, base))?.is_none() {
                self.lost.insert(Lost::Damaged);
            }
            operands.clear();
        }
    }

    /// End the page: the clips begun outside any save ended, so every clip
    /// and group in the list is closed.
    fn finish(&mut self) -> Result<(), PdfError> {
        while !self.saved.is_empty() || self.unsaved > 0 {
            self.restore()?;
        }
        for _ in 0..core::mem::take(&mut self.state.clips) {
            self.emit(Item::Unclip)?;
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm an operator: splitting the table would scatter the operator set"
    )]
    fn operator(
        &mut self,
        operator: &[u8],
        operands: &[Object],
        resources: Resources<'_>,
        base: Affine,
    ) -> Result<(), PdfError> {
        let number = |index: usize| {
            operands
                .get(index)
                .and_then(Object::as_number)
                .filter(|value| value.is_finite())
                .ok_or(PdfError::Malformed)
        };
        let integer = |index: usize| {
            operands
                .get(index)
                .and_then(Object::as_int)
                .ok_or(PdfError::Malformed)
        };
        let name = |index: usize| {
            operands
                .get(index)
                .and_then(Object::as_name)
                .ok_or(PdfError::Malformed)
        };
        let point =
            |index: usize| Ok::<_, PdfError>(Point::new(number(index)?, number(index + 1)?));
        match operator {
            b"q" => self.save()?,
            b"Q" => {
                if self.saved.len() + self.unsaved > self.floor.saves {
                    self.restore()?;
                }
            }
            b"cm" => {
                let matrix = matrix(operands)?;
                self.state.ctm = matrix.then(self.state.ctm);
            }
            b"w" => self.state.pen.style.width = number(0)?.abs(),
            b"J" => self.set_cap(cap_of(integer(0)?)),
            b"j" => self.state.pen.style.join = join_of(integer(0)?),
            b"M" => self.state.pen.style.miter_limit = number(0)?.max(1.0),
            b"d" => {
                let pattern = operands
                    .first()
                    .and_then(Object::as_array)
                    .ok_or(PdfError::Malformed)?;
                self.state.pen.dashes = dashes(pattern)?;
                self.state.pen.dash_offset = number(1)?;
            }
            b"gs" => self.graphics_state(name(0)?, resources)?,
            b"m" => self.move_to(point(0)?)?,
            b"l" => self.line_to(point(0)?)?,
            b"c" => self.curve_to(point(0)?, point(2)?, point(4)?)?,
            b"v" => {
                let from = self.current.ok_or(PdfError::Malformed)?;
                self.curve_to(from, point(0)?, point(2)?)?;
            }
            b"y" => {
                let to = point(2)?;
                self.curve_to(point(0)?, to, to)?;
            }
            b"h" => self.close_subpath()?,
            b"re" => self.rectangle(point(0)?, number(2)?, number(3)?)?,
            b"W" => self.clip = Some(FillRule::NonZero),
            b"W*" => self.clip = Some(FillRule::EvenOdd),
            b"n" => self.paint(
                Painting {
                    close: false,
                    fill: None,
                    stroke: false,
                },
                base,
            )?,
            b"S" => self.paint(
                Painting {
                    close: false,
                    fill: None,
                    stroke: true,
                },
                base,
            )?,
            b"s" => self.paint(
                Painting {
                    close: true,
                    fill: None,
                    stroke: true,
                },
                base,
            )?,
            b"f" | b"F" => self.paint(
                Painting {
                    close: false,
                    fill: Some(FillRule::NonZero),
                    stroke: false,
                },
                base,
            )?,
            b"f*" => self.paint(
                Painting {
                    close: false,
                    fill: Some(FillRule::EvenOdd),
                    stroke: false,
                },
                base,
            )?,
            b"B" => self.paint(
                Painting {
                    close: false,
                    fill: Some(FillRule::NonZero),
                    stroke: true,
                },
                base,
            )?,
            b"B*" => self.paint(
                Painting {
                    close: false,
                    fill: Some(FillRule::EvenOdd),
                    stroke: true,
                },
                base,
            )?,
            b"b" => self.paint(
                Painting {
                    close: true,
                    fill: Some(FillRule::NonZero),
                    stroke: true,
                },
                base,
            )?,
            b"b*" => self.paint(
                Painting {
                    close: true,
                    fill: Some(FillRule::EvenOdd),
                    stroke: true,
                },
                base,
            )?,
            b"CS" | b"cs" => {
                let space = self.space(operands.first().ok_or(PdfError::Malformed)?, resources)?;
                let colour = Colour {
                    ink: self.ink(&space, &space.initial(), None, resources)?,
                    space,
                };
                *self.colour_mut(operator == b"CS") = colour;
            }
            b"SC" | b"SCN" | b"sc" | b"scn" => {
                let stroking = operator[0] == b'S';
                let space = Arc::clone(&self.colour_mut(stroking).space);
                let pattern = operands.last().and_then(Object::as_name);
                let components = values(
                    operands
                        .iter()
                        .filter_map(Object::as_number)
                        .take(MAX_COMPONENTS),
                );
                let ink = self.ink(&space, &components, pattern, resources)?;
                self.colour_mut(stroking).ink = ink;
            }
            b"G" | b"g" => self.device(operator == b"G", Arc::clone(&self.gray), operands)?,
            b"RG" | b"rg" => self.device(operator == b"RG", Arc::clone(&self.rgb), operands)?,
            b"K" | b"k" => self.device(operator == b"K", Arc::clone(&self.cmyk), operands)?,
            b"sh" => self.shade(name(0)?, resources)?,
            b"Do" => self.xobject(name(0)?, resources)?,
            b"Tj" | b"TJ" | b"'" | b"\"" if self.shown() => self.lost.insert(Lost::Text),
            b"BMC" => try_push(&mut self.marked, false)?,
            b"BDC" => {
                // Pushed whatever its operands, so its `EMC` ends this section
                // and no other; a group that cannot be read hides nothing.
                let tag = name(0);
                if tag.is_err() {
                    self.lost.insert(Lost::Damaged);
                }
                let governed = tag.is_ok_and(|tag| tag == b"OC");
                let hides = governed
                    && tolerated(self.optional_shown(operands.get(1), resources))? == Some(false);
                try_push(&mut self.marked, hides)?;
                self.hiding += usize::from(hides);
            }
            b"EMC" if self.marked.len() > self.floor.marked => self.end_marked(),
            // What changes nothing a display list holds — a rendering intent,
            // flatness, a compatibility section's brackets, a marked point, an
            // unmarked section's end — and operators unknown here, passed over
            // as the specification asks within a compatibility section and
            // every reader does outside one.
            _ => {}
        }
        Ok(())
    }

    /// End the innermost marked-content section.
    fn end_marked(&mut self) {
        let hid = self.marked.pop() == Some(true);
        self.hiding -= usize::from(hid);
    }

    /// Whether what is painted now shows: neither optional content hides it
    /// nor a clip lets nothing through.
    fn shown(&self) -> bool {
        self.hiding == 0 && !self.state.clipped_out
    }

    fn colour_mut(&mut self, stroking: bool) -> &mut Colour {
        if stroking {
            &mut self.state.stroke
        } else {
            &mut self.state.fill
        }
    }

    /// Set a device colour, its space with it, from `operands`.
    fn device(
        &mut self,
        stroking: bool,
        space: Arc<Space>,
        operands: &[Object],
    ) -> Result<(), PdfError> {
        let wanted = space.components();
        let components = values(operands.iter().filter_map(Object::as_number));
        if components.len() < wanted {
            return Err(PdfError::Malformed);
        }
        let rgb = space
            .srgb(&components, self.document.work())?
            .unwrap_or([0.0; 3]);
        *self.colour_mut(stroking) = Colour {
            space,
            ink: Ink::Srgb(rgb),
        };
        Ok(())
    }

    /// The ink `components` make in `space`, or the pattern `pattern` names.
    fn ink(
        &mut self,
        space: &Arc<Space>,
        components: &Values,
        pattern: Option<&[u8]>,
        resources: Resources<'_>,
    ) -> Result<Ink, PdfError> {
        let work = self.document.work();
        let Space::Pattern(underlying) = &**space else {
            return Ok(space
                .srgb(components, work)?
                .map_or(Ink::Nothing, Ink::Srgb));
        };
        let Some(name) = pattern else {
            return Ok(Ink::Nothing);
        };
        let tint = match underlying {
            Some(underlying) => underlying.srgb(components, work)?,
            None => None,
        };
        Ok(match self.pattern(name, resources)? {
            Some(pattern) => Ink::Pattern { pattern, tint },
            None => Ink::Nothing,
        })
    }

    /// The colour space `operand` names, read once per name.
    fn space(
        &mut self,
        operand: &Object,
        resources: Resources<'_>,
    ) -> Result<Arc<Space>, PdfError> {
        let spaces = Some(&resources.categories.spaces);
        let Object::Name(name) = operand else {
            return Space::load(self.document, operand, spaces);
        };
        if let Some(space) = self.named(resources.scope)?.spaces.get(name.as_bytes()) {
            return space.held();
        }
        let read = tolerated(Space::load(self.document, operand, spaces))?;
        let read = Read::of(read);
        self.named(resources.scope)?
            .spaces
            .try_insert(Name::of(name.as_bytes())?, read.clone())?;
        read.held()
    }

    /// The pattern `name` names, read once per name; `None` for a shading
    /// pattern no gradient holds, the loss recorded.
    fn pattern(
        &mut self,
        name: &[u8],
        resources: Resources<'_>,
    ) -> Result<Option<Arc<Pattern>>, PdfError> {
        if let Some(pattern) = self.named(resources.scope)?.patterns.get(name) {
            return pattern.held();
        }
        let read = Read::of(tolerated(self.read_pattern(name, resources.categories))?);
        self.named(resources.scope)?
            .patterns
            .try_insert(Name::of(name)?, read.clone())?;
        read.held()
    }

    fn read_pattern(
        &mut self,
        name: &[u8],
        categories: &Categories,
    ) -> Result<Option<Arc<Pattern>>, PdfError> {
        let object = categories.patterns.get(name).ok_or(PdfError::Malformed)?;
        let object = self.document.resolve(object)?;
        let dict = object.as_dict().ok_or(PdfError::Malformed)?;
        let pattern = match self.document.get(dict, b"PatternType")?.as_int() {
            Some(1) => Pattern::Tiling {
                coloured: self.document.get(dict, b"PaintType")?.as_int() != Some(2),
            },
            Some(2) => {
                let matrix = matrix_entry(self.document, dict)?;
                let shading = self.document.get(dict, b"Shading")?;
                let read = shading::shading(self.document, &shading, Some(&categories.spaces))?;
                let Some(shading) = read else {
                    self.lost.insert(Lost::Shadings);
                    return Ok(None);
                };
                Pattern::Shading {
                    shading: try_arc(shading)?,
                    matrix,
                }
            }
            _ => return Err(PdfError::Malformed),
        };
        Ok(Some(try_arc(pattern)?))
    }

    /// The shading `name` names, read once per name; `None` for one no
    /// gradient holds.
    fn shading(
        &mut self,
        name: &[u8],
        resources: Resources<'_>,
    ) -> Result<Option<Arc<Shading>>, PdfError> {
        if let Some(shading) = self.named(resources.scope)?.shadings.get(name) {
            return shading.held();
        }
        let categories = resources.categories;
        let read = categories
            .shadings
            .get(name)
            .ok_or(PdfError::Malformed)
            .and_then(|object| shading::shading(self.document, object, Some(&categories.spaces)))
            .and_then(|shading| shading.map(try_arc).transpose().map_err(PdfError::from));
        let read = Read::of(tolerated(read)?);
        self.named(resources.scope)?
            .shadings
            .try_insert(Name::of(name)?, read.clone())?;
        read.held()
    }

    /// The paint `colour` makes at opacity `alpha` for geometry in user
    /// space, `base` the content stream's default space; `None` where it
    /// paints nothing.
    fn paint_of(
        &mut self,
        colour: &Colour,
        alpha: f64,
        base: Affine,
    ) -> Result<Option<Paint>, PdfError> {
        Ok(match &colour.ink {
            Ink::Srgb(rgb) => Some(Paint::Colour(with_alpha(*rgb, alpha))),
            Ink::Nothing => None,
            Ink::Pattern { pattern, tint } => match &**pattern {
                Pattern::Shading { shading, matrix } => {
                    let Some(user) = self.state.ctm.invert() else {
                        return Ok(None);
                    };
                    let transform = matrix.then(base).then(user);
                    if !transform.is_finite() {
                        self.lost.insert(Lost::Damaged);
                        return Ok(None);
                    }
                    if shading.bbox.is_some() {
                        self.lost.insert(Lost::Shadings);
                    }
                    let gradient = Gradient {
                        stops: faded(&shading.gradient.stops, alpha)?,
                        transform,
                        ..shading.gradient.clone()
                    };
                    Some(Paint::Gradient(try_arc(gradient)?))
                }
                Pattern::Tiling { coloured } => {
                    self.lost.insert(Lost::TilingPatterns);
                    match (coloured, tint) {
                        (false, Some(rgb)) => Some(Paint::Colour(with_alpha(*rgb, alpha))),
                        _ => None,
                    }
                }
            },
        })
    }

    /// `q`: the graphics state saved, past the deepest kept only counted.
    fn save(&mut self) -> Result<(), PdfError> {
        if self.saved.len() >= MAX_SAVES {
            self.unsaved += 1;
            return Ok(());
        }
        self.push_state()
    }

    /// The graphics state saved, however deep the saves already are: a
    /// form's own save, which forms' depth bounds.
    fn push_state(&mut self) -> Result<(), PdfError> {
        let mut saved = self.state.clone();
        self.state.clips = 0;
        core::mem::swap(&mut saved, &mut self.state);
        try_push(&mut self.saved, saved)?;
        Ok(())
    }

    /// `Q`: the newest `q` undone, one that saved nothing only counted.
    fn restore(&mut self) -> Result<(), PdfError> {
        if self.unsaved > 0 {
            self.unsaved -= 1;
            return Ok(());
        }
        self.pop_state()
    }

    /// The newest state saved restored, its clips ended: a form's own save,
    /// which lies above every `q` its caller made, counted or not.
    fn pop_state(&mut self) -> Result<(), PdfError> {
        let Some(saved) = self.saved.pop() else {
            return Ok(());
        };
        for _ in 0..self.state.clips {
            self.emit(Item::Unclip)?;
        }
        self.state = saved;
        Ok(())
    }

    /// `gs`: what the graphics state parameter dictionary `name` names sets.
    fn graphics_state(&mut self, name: &[u8], resources: Resources<'_>) -> Result<(), PdfError> {
        let setting = if let Some(setting) = self.named(resources.scope)?.states.get(name) {
            setting.held()?
        } else {
            let read = tolerated(self.setting(name, resources.categories))?;
            let read = Read::of(read.map(try_arc).transpose()?);
            self.named(resources.scope)?
                .states
                .try_insert(Name::of(name)?, read.clone())?;
            read.held()?
        };
        self.apply(&setting);
        Ok(())
    }

    /// What the graphics state parameter dictionary `name` names sets.
    fn setting(&self, name: &[u8], categories: &Categories) -> Result<Setting, PdfError> {
        let object = categories.states.get(name).ok_or(PdfError::Malformed)?;
        let object = self.document.resolve(object)?;
        let dict = object.as_dict().ok_or(PdfError::Malformed)?;
        let get = |key: &[u8]| self.document.get(dict, key);
        let dash = match get(b"D")?.as_array() {
            Some([pattern, offset]) => {
                let pattern = self.document.resolve(pattern)?;
                let offset = self.document.resolve(offset)?.as_number().unwrap_or(0.0);
                Some((
                    dashes(pattern.as_array().ok_or(PdfError::Malformed)?)?,
                    offset,
                ))
            }
            _ => None,
        };
        let blend = get(b"BM")?;
        let blend = match &*blend {
            Object::Array(modes) => modes.first().and_then(Object::as_name),
            other => other.as_name(),
        };
        let mask = match dict.get(b"SMask") {
            Some(mask) => Some(self.soft_mask(mask, categories)?),
            None => None,
        };
        Ok(Setting {
            width: get(b"LW")?.as_number().map(f64::abs),
            cap: get(b"LC")?.as_int().map(cap_of),
            join: get(b"LJ")?.as_int().map(join_of),
            miter_limit: get(b"ML")?.as_number().map(|limit| limit.max(1.0)),
            dash,
            fill_alpha: get(b"ca")?.as_number().map(|alpha| alpha.clamp(0.0, 1.0)),
            stroke_alpha: get(b"CA")?.as_number().map(|alpha| alpha.clamp(0.0, 1.0)),
            blends: blend.is_some_and(|mode| mode != b"Normal" && mode != b"Compatible"),
            mask,
        })
    }

    /// Set what `setting` sets.
    fn apply(&mut self, setting: &Setting) {
        let style = &mut self.state.pen.style;
        if let Some(width) = setting.width {
            style.width = width;
        }
        if let Some(join) = setting.join {
            style.join = join;
        }
        if let Some(limit) = setting.miter_limit {
            style.miter_limit = limit;
        }
        if let Some(cap) = setting.cap {
            self.set_cap(cap);
        }
        if let Some((dashes, offset)) = &setting.dash {
            self.state.pen.dashes = dashes.clone();
            self.state.pen.dash_offset = *offset;
        }
        if let Some(alpha) = setting.fill_alpha {
            self.state.fill_alpha = alpha;
        }
        if let Some(alpha) = setting.stroke_alpha {
            self.state.stroke_alpha = alpha;
        }
        if setting.blends {
            self.lost.insert(Lost::BlendModes);
        }
        match &setting.mask {
            None => {}
            Some(SoftMask::Cleared) => self.state.mask = None,
            Some(SoftMask::Lost) => {
                self.lost.insert(Lost::SoftMasks);
                self.state.mask = None;
            }
            Some(SoftMask::Gradient { gradient, matrix }) => {
                self.state.mask = Some(Mask {
                    gradient: Arc::clone(gradient),
                    space: matrix.then(self.state.ctm),
                });
            }
        }
    }

    fn set_cap(&mut self, cap: Cap) {
        self.state.pen.style.start_cap = cap;
        self.state.pen.style.end_cap = cap;
    }

    /// The soft mask `object` sets: cleared for `/None`, a gradient's alpha
    /// where it is one luminosity shading, and lost otherwise.
    fn soft_mask(&self, object: &Object, categories: &Categories) -> Result<SoftMask, PdfError> {
        let object = self.document.resolve(object)?;
        let Some(dict) = object.as_dict() else {
            return Ok(SoftMask::Cleared);
        };
        Ok(match tolerated(self.alpha_shading(dict, categories))? {
            Some(Some((gradient, matrix))) => SoftMask::Gradient {
                gradient: try_arc(gradient)?,
                matrix,
            },
            _ => SoftMask::Lost,
        })
    }

    /// The gradient a luminosity soft mask paints and the matrix of its
    /// group, where the group paints nothing but one axial or radial shading.
    fn alpha_shading(
        &self,
        mask: &Dict,
        categories: &Categories,
    ) -> Result<Option<(Gradient, Affine)>, PdfError> {
        if self.document.get(mask, b"S")?.as_name() != Some(b"Luminosity")
            || mask.get(b"TR").is_some()
        {
            return Ok(None);
        }
        let group = self.document.get(mask, b"G")?;
        let stream = group.as_stream().ok_or(PdfError::Malformed)?;
        let content = self.document.decode(stream)?.bytes;
        let mut lexer = Lexer::new(&content, 0);
        let (Some(Token::Name(name)), Some(Token::Keyword(b"sh")), None) =
            (lexer.next()?, lexer.next()?, lexer.next()?)
        else {
            return Ok(None);
        };
        let own = match self
            .document
            .get(&stream.dict, b"Resources")?
            .into_owned()?
        {
            Object::Dict(own) => Some(Categories::of(self.document, own)?),
            _ => None,
        };
        let categories = own.as_ref().unwrap_or(categories);
        let shading = categories.shadings.get(&name).ok_or(PdfError::Malformed)?;
        let Some(shading) = shading::shading(self.document, shading, Some(&categories.spaces))?
        else {
            return Ok(None);
        };
        Ok(Some((
            shading.gradient,
            matrix_entry(self.document, &stream.dict)?,
        )))
    }

    fn move_to(&mut self, to: Point) -> Result<(), PdfError> {
        self.grow(1)?;
        self.end_subpath()?;
        self.subpath = Some(Subpath::new(to));
        self.current = Some(to);
        Ok(())
    }

    fn line_to(&mut self, to: Point) -> Result<(), PdfError> {
        self.segment(PathEl::Line(to))
    }

    fn curve_to(&mut self, c1: Point, c2: Point, to: Point) -> Result<(), PdfError> {
        self.segment(PathEl::Cubic(c1, c2, to))
    }

    /// Add `element`, a segment from the current point; a path begun without
    /// one starts there, as every reader starts it.
    fn segment(&mut self, element: PathEl) -> Result<(), PdfError> {
        self.grow(element_points(&element) + usize::from(self.subpath.is_none()))?;
        if self.subpath.is_none() {
            let start = self.current.unwrap_or_else(|| element.end());
            self.subpath = Some(Subpath::new(start));
        }
        if let Some(subpath) = &mut self.subpath {
            try_push(&mut subpath.elements, element)?;
        }
        self.current = Some(element.end());
        Ok(())
    }

    fn close_subpath(&mut self) -> Result<(), PdfError> {
        if let Some(mut subpath) = self.subpath.take() {
            subpath.closed = true;
            self.current = Some(subpath.start);
            try_push(&mut self.path.subpaths, subpath)?;
        }
        Ok(())
    }

    fn end_subpath(&mut self) -> Result<(), PdfError> {
        if let Some(subpath) = self.subpath.take() {
            try_push(&mut self.path.subpaths, subpath)?;
        }
        Ok(())
    }

    /// Count `points` more in the path being built: painting it copies them
    /// into the list, so it is held to the list's bound as it grows.
    fn grow(&mut self, points: usize) -> Result<(), PdfError> {
        self.built += points;
        if self.points + self.built > MAX_POINTS {
            return Err(PdfError::TooComplex);
        }
        Ok(())
    }

    fn rectangle(&mut self, at: Point, width: f64, height: f64) -> Result<(), PdfError> {
        self.grow(4)?;
        self.end_subpath()?;
        let mut subpath = Subpath::new(at);
        subpath.elements.try_reserve_exact(3)?;
        subpath
            .elements
            .push(PathEl::Line(Point::new(at.x + width, at.y)));
        subpath
            .elements
            .push(PathEl::Line(Point::new(at.x + width, at.y + height)));
        subpath
            .elements
            .push(PathEl::Line(Point::new(at.x, at.y + height)));
        subpath.closed = true;
        try_push(&mut self.path.subpaths, subpath)?;
        self.current = Some(at);
        Ok(())
    }

    /// Paint the current path as `painting` says, then clip to it where a
    /// clip was asked for; either way the path is then done with, given to
    /// the last item that holds it and copied only for those before.
    fn paint(&mut self, painting: Painting, base: Affine) -> Result<(), PdfError> {
        if painting.close {
            self.close_subpath()?;
        } else {
            self.end_subpath()?;
        }
        let path = core::mem::take(&mut self.path);
        self.built = 0;
        self.current = None;
        let clip = self.clip.take();
        let transform = self.state.ctm.then(self.page);
        let paints = painting.fill.is_some() || painting.stroke;
        if paints && !path.is_empty() && self.shown() {
            if transform.is_finite() {
                self.draw(painting, &path, transform, base)?;
            } else {
                self.lost.insert(Lost::Damaged);
            }
        }
        if let Some(rule) = clip {
            self.clip_to(path, rule, transform)?;
        }
        Ok(())
    }

    /// Fill and stroke `path` as `painting` says, each item taking a compact
    /// copy of it, so the slack the path grew with stays out of the list.
    fn draw(
        &mut self,
        painting: Painting,
        path: &Path,
        transform: Affine,
        base: Affine,
    ) -> Result<(), PdfError> {
        if self.state.mask.is_some() {
            self.lost.insert(Lost::SoftMasks);
        }
        if let Some(rule) = painting.fill {
            let colour = self.state.fill.clone();
            if let Some(paint) = self.paint_of(&colour, self.state.fill_alpha, base)? {
                let path = path.try_clone()?;
                self.emit(Item::Fill {
                    path,
                    rule,
                    paint,
                    transform,
                })?;
            }
        }
        if painting.stroke {
            let colour = self.state.stroke.clone();
            if let Some(paint) = self.paint_of(&colour, self.state.stroke_alpha, base)? {
                let pen = self.state.pen.clone();
                let path = path.try_clone()?;
                self.emit(Item::Stroke {
                    path,
                    pen,
                    paint,
                    transform,
                })?;
            }
        }
        Ok(())
    }

    /// Clip what follows, until the state is restored, to `path` placed by
    /// `transform`. A clip to no path lets nothing through, and so does one
    /// that cannot be placed, recorded as damage: nothing beneath a clip is
    /// drawn unclipped.
    fn clip_to(&mut self, path: Path, rule: FillRule, transform: Affine) -> Result<(), PdfError> {
        let placed = transform.is_finite();
        if !placed && !path.is_empty() {
            self.lost.insert(Lost::Damaged);
        }
        if self.state.clipped_out || path.is_empty() || !placed {
            self.state.clipped_out = true;
            return Ok(());
        }
        self.emit(Item::Clip {
            path,
            rule,
            transform,
        })?;
        self.state.clips += 1;
        Ok(())
    }

    /// Add `item` to the list, counted against the bound on its items, and
    /// its points and stops against the bound on those.
    fn emit(&mut self, item: Item) -> Result<(), PdfError> {
        if self.items.len() >= MAX_ITEMS {
            return Err(PdfError::TooComplex);
        }
        let points = |path: &Path| -> usize {
            path.subpaths
                .iter()
                .map(|subpath| 1 + subpath.elements.iter().map(element_points).sum::<usize>())
                .sum()
        };
        let stops = |paint: &Paint| match paint {
            Paint::Gradient(gradient) => gradient.stops.len(),
            Paint::Colour(_) => 0,
        };
        self.points += match &item {
            Item::Fill { path, paint, .. } | Item::Stroke { path, paint, .. } => {
                points(path) + stops(paint)
            }
            Item::Clip { path, .. } => points(path),
            _ => 0,
        };
        if self.points > MAX_POINTS {
            return Err(PdfError::TooComplex);
        }
        try_push(&mut self.items, item)?;
        Ok(())
    }

    /// `sh`: shading `name` painted over everything the clip lets through.
    fn shade(&mut self, name: &[u8], resources: Resources<'_>) -> Result<(), PdfError> {
        if !self.shown() {
            return Ok(());
        }
        let Some(shading) = self.shading(name, resources)? else {
            self.lost.insert(Lost::Shadings);
            return Ok(());
        };
        let transform = self.state.ctm.then(self.page);
        let Some(user) = transform.invert() else {
            if !transform.is_finite() {
                self.lost.insert(Lost::Damaged);
            }
            return Ok(());
        };
        let colours = &shading.gradient.stops;
        let stops = match self.state.mask.clone() {
            Some(mask)
                if mask.space == self.state.ctm
                    && same_geometry(&mask.gradient, &shading.gradient) =>
            {
                if let Some(masked) = masked(colours, &mask.gradient.stops)? {
                    faded(&masked, self.state.fill_alpha)?
                } else {
                    self.lost.insert(Lost::SoftMasks);
                    faded(colours, self.state.fill_alpha)?
                }
            }
            Some(_) => {
                self.lost.insert(Lost::SoftMasks);
                faded(colours, self.state.fill_alpha)?
            }
            None => faded(colours, self.state.fill_alpha)?,
        };
        let (width, height) = self.size;
        let mut cover = Subpath::new(user.map(Point::new(0.0, 0.0)));
        cover.elements.try_reserve_exact(3)?;
        for corner in [
            Point::new(width, 0.0),
            Point::new(width, height),
            Point::new(0.0, height),
        ] {
            cover.elements.push(PathEl::Line(user.map(corner)));
        }
        cover.closed = true;
        let cover = Path::single(cover)?;
        let paint = Paint::Gradient(try_arc(Gradient {
            stops,
            ..shading.gradient.clone()
        })?);
        if let Some(bbox) = shading.bbox {
            self.emit(Item::Clip {
                path: rect_path(bbox)?,
                rule: FillRule::NonZero,
                transform,
            })?;
            self.emit(Item::Fill {
                path: cover,
                rule: FillRule::NonZero,
                paint,
                transform,
            })?;
            return self.emit(Item::Unclip);
        }
        self.emit(Item::Fill {
            path: cover,
            rule: FillRule::NonZero,
            paint,
            transform,
        })
    }

    /// Whether content marked as governed by the optional content `name`
    /// names in `resources` is shown: found once for each name, however much
    /// content it governs.
    fn optional_shown(
        &mut self,
        name: Option<&Object>,
        resources: Resources<'_>,
    ) -> Result<bool, PdfError> {
        let name = match name {
            Some(Object::Name(name)) => name.as_bytes(),
            Some(inline @ Object::Dict(_)) => {
                return self.document.visibility().shows(self.document, inline);
            }
            _ => return Ok(true),
        };
        if let Some(shown) = self.named(resources.scope)?.properties.get(name) {
            return shown.held();
        }
        let shown = match resources.categories.properties.get(name) {
            Some(governing) => {
                tolerated(self.document.visibility().shows(self.document, governing))?
            }
            None => Some(true),
        };
        self.named(resources.scope)?
            .properties
            .try_insert(Name::of(name)?, Read::of(shown))?;
        shown.ok_or(PdfError::Malformed)
    }

    /// `Do`: external object `name` painted.
    fn xobject(&mut self, name: &[u8], resources: Resources<'_>) -> Result<(), PdfError> {
        if !self.shown() {
            return Ok(());
        }
        let external = if let Some(external) = self.named(resources.scope)?.xobjects.get(name) {
            external.held()?
        } else {
            let read = Read::of(tolerated(self.external(name, resources.categories))?);
            self.named(resources.scope)?
                .xobjects
                .try_insert(Name::of(name)?, read.clone())?;
            read.held()?
        };
        if !external.shown {
            return Ok(());
        }
        let stream = &external.stream;
        match self.document.get(&stream.dict, b"Subtype")?.as_name() {
            Some(b"Form") => self.form(stream, resources),
            Some(b"Image") => {
                let painted = self.image_xobject(stream);
                self.image_outcome(painted)
            }
            _ => Ok(()),
        }
    }

    /// The external object `name` names, and whether it is shown.
    fn external(&self, name: &[u8], categories: &Categories) -> Result<Arc<External>, PdfError> {
        let entry = categories.xobjects.get(name).ok_or(PdfError::Malformed)?;
        let Object::Stream(stream) = self.document.resolve(entry)?.into_owned()? else {
            return Err(PdfError::Malformed);
        };
        let shown = match stream.dict.get(b"OC") {
            Some(governing) => self.document.visibility().shows(self.document, governing)?,
            None => true,
        };
        Ok(try_arc(External { stream, shown })?)
    }

    /// Record an image that could not be read as lost; only the heap's
    /// refusal or a containment bound stops the page.
    fn image_outcome(&mut self, painted: Result<(), PdfError>) -> Result<(), PdfError> {
        if tolerated(painted)?.is_none() {
            self.lost.insert(Lost::Images);
        }
        Ok(())
    }

    /// An image object painted: read once however often it is painted, a
    /// stencil's samples once in every colour it is painted in.
    fn image_xobject(&mut self, stream: &Stream) -> Result<(), PdfError> {
        let origin = Origin::Object(stream.owner.num);
        if !self.images.contains_key(&origin) {
            let read = Read::of(tolerated(self.read_image(stream))?);
            self.images.try_insert(origin, read)?;
        }
        self.show(origin)
    }

    /// Image object `stream` read: only an inline image's colour space may be
    /// a resource's name.
    fn read_image(&self, stream: &Stream) -> Result<Painted, PdfError> {
        let dict = &stream.dict;
        let kind = if self.document.get(dict, b"ImageMask")?.as_bool() == Some(true) {
            image::Kind::Stencil
        } else {
            let space = self.document.get(dict, b"ColorSpace")?;
            image::Kind::Colour(Space::load(self.document, &space, None)?)
        };
        Painted::read(self.document, dict, self.document.decode(stream)?, &kind)
    }

    /// The image read from `origin` painted, a stencil in the fill colour.
    fn show(&mut self, origin: Origin) -> Result<(), PdfError> {
        let document = self.document;
        let fill = self.stencil_colour();
        let inked = !matches!(self.state.fill.ink, Ink::Nothing);
        let painted = self
            .images
            .get_mut(&origin)
            .ok_or(PdfError::Malformed)?
            .held_mut()?;
        // A stencil inked with nothing paints nothing, as a fill in it does.
        if !inked && matches!(painted, Painted::Stencil { .. }) {
            return Ok(());
        }
        let picture = painted.picture(document, fill, &mut self.lost)?;
        self.place(picture)
    }

    /// The colour a stencil is painted in: the fill's, or none where the fill
    /// is a paint a picture cannot hold.
    fn stencil_colour(&self) -> Option<Srgb> {
        match &self.state.fill.ink {
            Ink::Srgb(rgb)
            | Ink::Pattern {
                tint: Some(rgb), ..
            } => Some(*rgb),
            _ => None,
        }
    }

    /// `picture` painted over the unit square, its top row at the top.
    fn place(&mut self, picture: Arc<image::Picture>) -> Result<(), PdfError> {
        if !self.shown() {
            return Ok(());
        }
        if self.state.mask.is_some() {
            self.lost.insert(Lost::SoftMasks);
        }
        let flip = Affine {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            e: 0.0,
            f: 1.0,
        };
        let transform = flip.then(self.state.ctm).then(self.page);
        if !transform.is_finite() {
            self.lost.insert(Lost::Damaged);
            return Ok(());
        }
        self.emit(Item::Image {
            picture,
            transform,
            opacity: self.state.fill_alpha,
        })
    }

    /// `BI`, read at `at`: an inline image, its dictionary, its data after
    /// `ID` and its `EI`, the lexer left past it. One met again where it lies
    /// — a form painted again — is found and decoded only once.
    fn inline_image(
        &mut self,
        lexer: &mut Lexer<'_>,
        resources: Resources<'_>,
        at: usize,
    ) -> Result<(), PdfError> {
        let inline = (resources.scope, self.content, at);
        let origin = Origin::Inline(inline);
        let shown = self.shown();
        // One first met unseen is decoded only once it is painted.
        if let Some(&end) = self.inline_ends.get(&inline) {
            if !shown || self.images.contains_key(&origin) {
                lexer.seek(end);
                return if shown { self.show(origin) } else { Ok(()) };
            }
        }
        let mut entries = Vec::new();
        loop {
            match lexer.next()?.ok_or(PdfError::Malformed)? {
                Token::Keyword(b"ID") => break,
                Token::Name(key) => {
                    let token = lexer.next()?.ok_or(PdfError::Malformed)?;
                    let value = parse::operand(lexer, token)?;
                    try_push(&mut entries, (image::spelled_out(key)?, value))?;
                }
                _ => return Err(PdfError::Malformed),
            }
        }
        let dict = Dict::from_written(entries)?;
        let stencil = self.document.get(&dict, b"ImageMask")?.as_bool() == Some(true);
        // The space is read before the data is found: an unfiltered image's
        // data is as long as its samples are.
        let space = match dict.get(b"ColorSpace") {
            Some(space) if !stencil => tolerated(self.space(space, resources))?,
            _ => None,
        };
        // One white-space byte separates `ID` from the data.
        let start = lexer.at() + 1;
        let data = lexer.data().get(start..).unwrap_or_default();
        let length = image::inline_end(self.document, &dict, space.as_deref(), data)?;
        let after = parse::find(&data[length..], b"EI").map_or(data.len(), |at| length + at + 2);
        lexer.seek(start + after);
        self.inline_ends.try_insert(inline, lexer.at())?;
        if !shown {
            return Ok(());
        }
        let kind = match space {
            _ if stencil => Ok(image::Kind::Stencil),
            Some(space) => Ok(image::Kind::Colour(space)),
            None => Err(PdfError::Malformed),
        };
        let read = kind.and_then(|kind| {
            let chain = crate::filter::chain(dict.get(b"Filter"), dict.get(b"DecodeParms"))?;
            let decoded = crate::filter::decode(
                alloc::borrow::Cow::Borrowed(&data[..length]),
                &chain,
                self.document.budget(),
            )?;
            Painted::read(self.document, &dict, decoded, &kind)
        });
        self.images.try_insert(origin, Read::of(tolerated(read)?))?;
        self.show(origin)
    }

    /// A form painted: its content in its own space and resources, clipped
    /// to its box, and drawn as one where it is a transparency group.
    fn form(&mut self, stream: &Stream, resources: Resources<'_>) -> Result<(), PdfError> {
        if self.forms.len() >= MAX_FORM_DEPTH {
            return Err(PdfError::TooComplex);
        }
        if self.forms.contains(&stream.owner) {
            return Err(PdfError::Malformed);
        }
        let form = self.form_content(stream)?;
        let own = match &form.resources {
            Some((categories, scope)) => Resources {
                categories,
                scope: *scope,
            },
            None => resources,
        };
        self.push_state()?;
        self.state.ctm = form.matrix.then(self.state.ctm);
        let base = self.state.ctm;
        if form.grouped {
            // The group's own mask would apply to it whole, which no group
            // the list holds can take.
            if self.state.mask.is_some() {
                self.lost.insert(Lost::SoftMasks);
            }
            self.emit(Item::Group {
                opacity: self.state.fill_alpha,
            })?;
            self.state.fill_alpha = 1.0;
            self.state.stroke_alpha = 1.0;
            self.state.mask = None;
        }
        if let Some(bbox) = form.bbox {
            let transform = self.state.ctm.then(self.page);
            self.clip_to(rect_path(bbox)?, FillRule::NonZero, transform)?;
        }
        try_push(&mut self.forms, stream.owner)?;
        let content = self.content.replace(stream.owner.num);
        let ran = self.run(&form.content, own, base);
        self.content = content;
        self.forms.pop();
        self.pop_state()?;
        if form.grouped {
            self.emit(Item::Ungroup)?;
        }
        ran
    }

    /// A form's decoded content, geometry and own resources, read once
    /// however often it is painted.
    fn form_content(&mut self, stream: &Stream) -> Result<Arc<Form>, PdfError> {
        let num = stream.owner.num;
        if let Some(form) = self.forms_read.get(&num) {
            return form.held();
        }
        let read = tolerated(self.read_form(stream))?;
        let read = Read::of(read);
        self.forms_read.try_insert(num, read.clone())?;
        read.held()
    }

    fn read_form(&mut self, stream: &Stream) -> Result<Arc<Form>, PdfError> {
        let dict = &stream.dict;
        let matrix = matrix_entry(self.document, dict)?;
        let bbox = tolerated(crate::function::numbers(self.document, dict, b"BBox", 4))?;
        let bbox = match bbox.flatten().as_deref() {
            Some(&[x0, y0, x1, y1]) => Some(Rect::spanning(x0, y0, x1, y1)),
            _ => None,
        };
        let group = self.document.get(dict, b"Group")?;
        let grouped = group
            .as_dict()
            .is_some_and(|group| group.name(b"S") == Some(b"Transparency"));
        let decoded = self.document.decode(stream)?;
        if decoded.damaged {
            self.lost.insert(Lost::Damaged);
        }
        let resources = match self.document.get(dict, b"Resources")?.into_owned()? {
            Object::Dict(resources) => {
                Some((Categories::of(self.document, resources)?, self.scope()?))
            }
            _ => None,
        };
        Ok(try_arc(Form {
            content: decoded.bytes,
            matrix,
            bbox,
            grouped,
            resources,
        })?)
    }
}

/// The line cap code `code` names.
fn cap_of(code: i64) -> Cap {
    match code {
        1 => Cap::Round,
        2 => Cap::Square,
        _ => Cap::Butt,
    }
}

/// The dash pattern `pattern` states: one of only gaps draws a solid line,
/// as every reader draws it.
fn dashes(pattern: &[Object]) -> Result<SharedSlice<f64>, PdfError> {
    let mut dashes = Vec::new();
    dashes.try_reserve_exact(pattern.len())?;
    for dash in pattern {
        let dash = dash
            .as_number()
            .filter(|dash| dash.is_finite() && *dash >= 0.0)
            .ok_or(PdfError::Malformed)?;
        dashes.push(dash);
    }
    if dashes.iter().all(|&dash| dash == 0.0) {
        dashes.clear();
    }
    Ok(SharedSlice::try_from_vec(dashes)?)
}

/// The join line join code `code` names.
fn join_of(code: i64) -> Join {
    match code {
        1 => Join::Round,
        2 => Join::Bevel,
        _ => Join::Miter,
    }
}

/// The matrix six operands state.
fn matrix(operands: &[Object]) -> Result<Affine, PdfError> {
    affine(operands.iter().map(finite))
}

/// The matrix an array object states.
fn matrix_of(document: &Document, object: &Object) -> Result<Affine, PdfError> {
    let object = document.resolve(object)?;
    let items = object.as_array().ok_or(PdfError::Malformed)?;
    affine(
        items
            .iter()
            .map(|item| document.resolve(item).and_then(|item| finite(&item))),
    )
}

/// The matrix `dict`'s `/Matrix` states, the identity where it states none.
fn matrix_entry(document: &Document, dict: &Dict) -> Result<Affine, PdfError> {
    match dict.get(b"Matrix") {
        Some(matrix) => matrix_of(document, matrix),
        None => Ok(Affine::IDENTITY),
    }
}

/// The matrix the first six of `numbers` state.
fn affine(numbers: impl Iterator<Item = Result<f64, PdfError>>) -> Result<Affine, PdfError> {
    let mut parts = [0.0; 6];
    let mut stated = 0;
    for (part, number) in parts.iter_mut().zip(numbers) {
        *part = number?;
        stated += 1;
    }
    if stated < parts.len() {
        return Err(PdfError::Malformed);
    }
    Ok(Affine {
        a: parts[0],
        b: parts[1],
        c: parts[2],
        d: parts[3],
        e: parts[4],
        f: parts[5],
    })
}

/// The finite number `object` is.
fn finite(object: &Object) -> Result<f64, PdfError> {
    object
        .as_number()
        .filter(|value| value.is_finite())
        .ok_or(PdfError::Malformed)
}

/// The points a path element adds: its end, and a curve's two controls.
fn element_points(element: &PathEl) -> usize {
    match element {
        PathEl::Line(_) => 1,
        PathEl::Cubic(..) => 3,
    }
}

/// The closed path round `rect`.
fn rect_path(rect: Rect) -> Result<Path, PdfError> {
    let mut subpath = Subpath::new(Point::new(rect.x0, rect.y0));
    subpath.elements.try_reserve_exact(3)?;
    subpath
        .elements
        .push(PathEl::Line(Point::new(rect.x1, rect.y0)));
    subpath
        .elements
        .push(PathEl::Line(Point::new(rect.x1, rect.y1)));
    subpath
        .elements
        .push(PathEl::Line(Point::new(rect.x0, rect.y1)));
    subpath.closed = true;
    Ok(Path::single(subpath)?)
}

/// `rgb` at opacity `alpha`.
fn with_alpha(rgb: Srgb, alpha: f64) -> Rgba {
    let [r, g, b] = rgb.map(level);
    Rgba::new(r, g, b, level(alpha))
}

/// `stops` laid down at opacity `alpha`: shared as they are where it is
/// whole, a copy faded where not.
fn faded(stops: &SharedSlice<Stop>, alpha: f64) -> Result<SharedSlice<Stop>, PdfError> {
    if alpha >= 1.0 {
        return Ok(stops.clone());
    }
    let mut out = Vec::new();
    out.try_reserve_exact(stops.len())?;
    out.extend(stops.iter().map(|&stop| Stop {
        colour: Rgba {
            a: level(f64::from(stop.colour.a) / 255.0 * alpha),
            ..stop.colour
        },
        ..stop
    }));
    Ok(SharedSlice::try_from_vec(out)?)
}

/// Whether two gradients run over the same geometry, so one's luminance can
/// be the other's alpha.
fn same_geometry(mask: &Gradient, painted: &Gradient) -> bool {
    const CLOSE: f64 = 1e-6;
    let near = |a: Point, b: Point| (a.x - b.x).abs() <= CLOSE && (a.y - b.y).abs() <= CLOSE;
    let kinds = match (mask.kind, painted.kind) {
        (
            GradientKind::Axial { start: s0, end: e0 },
            GradientKind::Axial { start: s1, end: e1 },
        ) => near(s0, s1) && near(e0, e1),
        (
            GradientKind::Radial {
                start: s0,
                start_radius: r0,
                end: e0,
                end_radius: q0,
            },
            GradientKind::Radial {
                start: s1,
                start_radius: r1,
                end: e1,
                end_radius: q1,
            },
        ) => near(s0, s1) && near(e0, e1) && (r0 - r1).abs() <= CLOSE && (q0 - q1).abs() <= CLOSE,
        _ => false,
    };
    kinds && mask.extend == painted.extend
}

/// `colours` with the alpha of `mask`'s luminosity at each offset either
/// names, walked together once: each colour and alpha interpolated where its
/// own stops have none. `None` where that takes more stops than a gradient
/// holds.
fn masked(
    colours: &SharedSlice<Stop>,
    mask: &[Stop],
) -> Result<Option<SharedSlice<Stop>>, PdfError> {
    if colours.len() + mask.len() > MAX_STOPS {
        return Ok(None);
    }
    let mut out = Vec::new();
    out.try_reserve_exact(colours.len() + mask.len())?;
    let (mut c, mut m) = (0, 0);
    while c < colours.len() || m < mask.len() {
        let colour_next = match (colours.get(c), mask.get(m)) {
            (Some(colour), Some(alpha)) if colour.offset.total_cmp(&alpha.offset).is_eq() => {
                // Both place a stop here: one stop takes the two together.
                out.push(Stop {
                    colour: Rgba {
                        a: luminosity(alpha.colour),
                        ..colour.colour
                    },
                    ..*colour
                });
                (c, m) = (c + 1, m + 1);
                continue;
            }
            (Some(colour), Some(alpha)) => colour.offset < alpha.offset,
            (colour, _) => colour.is_some(),
        };
        out.push(if colour_next {
            let stop = colours[c];
            c += 1;
            let alpha = luminosity(colour_at(mask, m, stop.offset));
            Stop {
                colour: Rgba {
                    a: alpha,
                    ..stop.colour
                },
                ..stop
            }
        } else {
            let stop = mask[m];
            m += 1;
            let colour = colour_at(colours, c, stop.offset);
            Stop {
                colour: Rgba {
                    a: luminosity(stop.colour),
                    ..colour
                },
                ..stop
            }
        });
    }
    Ok(Some(SharedSlice::try_from_vec(out)?))
}

/// `colour`'s luminosity as a soft mask's alpha: its grey, as the
/// specification and every reader weigh the channels.
fn luminosity(colour: Rgba) -> u8 {
    let grey = 0.3 * f64::from(colour.r) + 0.59 * f64::from(colour.g) + 0.11 * f64::from(colour.b);
    level(grey / 255.0)
}

/// The colour `stops` give at `offset`, `next` the first of them not before
/// it: interpolated between it and the one before, and held at the ends.
fn colour_at(stops: &[Stop], next: usize, offset: f64) -> Rgba {
    let (Some(low), Some(high)) = (
        next.checked_sub(1).and_then(|at| stops.get(at)),
        stops.get(next),
    ) else {
        return stops
            .get(next)
            .or_else(|| stops.last())
            .map_or(Rgba::new(0, 0, 0, 0), |stop| stop.colour);
    };
    let share = if high.offset > low.offset {
        (offset - low.offset) / (high.offset - low.offset)
    } else {
        0.0
    };
    let blend =
        |a: u8, b: u8| level((f64::from(a) + (f64::from(b) - f64::from(a)) * share) / 255.0);
    Rgba::new(
        blend(low.colour.r, high.colour.r),
        blend(low.colour.g, high.colour.g),
        blend(low.colour.b, high.colour.b),
        blend(low.colour.a, high.colour.a),
    )
}

#[cfg(test)]
#[path = "content_tests.rs"]
mod tests;
