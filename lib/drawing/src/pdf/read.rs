//! Reading one page of a PDF into a drawing.
//!
//! `lib/pdf` interprets the page into its display list; each item becomes an
//! object of one layer, in order: a fill or a stroke a shape in its style —
//! one shape for a fill and the stroke of the same path that follows it — a
//! clip a group clipped to its path, a transparency group a group at its
//! opacity, and a picture a picture. The page's crop box is the drawing's
//! page, in points. What the page holds that a drawing cannot is recorded.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::{Affine, Bounds, FillRule, Path, Point};
use tairix_collections::{try_arc, OrderedMap, SharedSlice, SharedStr};
use tairix_colour::Rgba;
use tairix_pdf::{About, DisplayList, Document, Item, Losses, Options, PdfError};
use tairix_raster::{Pyramid, Surface};
use tairix_util::fallible::try_push;

use crate::children::Children;
use crate::drawing::{Drawing, Grid, Info, Page, Unit};
use crate::id::Ids;
use crate::image::{Encoding, Image, Source};
use crate::limits::{MAX_DASH_ENTRIES, MAX_DEPTH, MAX_STOPS};
use crate::node::{Kind, Node};
use crate::shape::{PathShape, Shape};
use crate::style::{
    Arrowhead, Gradient, GradientKind, Paint, PaintOrder, Spread, Stop, Stroke, Style,
};
use crate::unkept::{Lost, Unkept};
use crate::{DrawingError, Read, ReadError};

/// The widest a stroke drawn as thin as a device can draw it is read as: a
/// pixel of the page.
const HAIRLINE: f64 = 1.0;

/// Which page of a PDF to read, and how.
#[derive(Clone, Copy)]
pub struct Request<'a> {
    /// The page, counting from 0.
    pub page: usize,
    /// The password, where the empty one does not open it. A secret: held
    /// zeroizing by the caller, and neither kept nor logged here.
    pub password: Option<&'a [u8]>,
    /// The most bytes its streams may decode to, all of them together.
    pub budget: u64,
}

/// Whether `bytes` are a PDF: its header near its start, unless they begin
/// as markup — past a byte order mark and white space, a `<` — whose text may
/// hold the header's five characters anywhere.
#[must_use]
pub fn recognised(bytes: &[u8]) -> bool {
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let markup = text.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'<');
    !markup && tairix_pdf::recognised(bytes)
}

const fn read_error(error: PdfError) -> ReadError {
    match error {
        PdfError::Malformed | PdfError::Unsupported => ReadError::Malformed,
        PdfError::NeedsPassword => ReadError::NeedsPassword,
        PdfError::WrongPassword => ReadError::WrongPassword,
        PdfError::TooComplex => ReadError::TooComplex,
        PdfError::OutOfMemory => ReadError::OutOfMemory,
        PdfError::NoSuchPage => ReadError::NoSuchPage,
    }
}

fn open<'f>(bytes: &'f [u8], request: &Request<'_>) -> Result<Document<'f>, ReadError> {
    let options = Options {
        password: request.password,
        budget: request.budget,
    };
    Document::open(bytes, &options).map_err(read_error)
}

/// The pages of `bytes`, a PDF, as a chooser lists them: how many it has,
/// and the width and height, in points, its first `most` are shown at.
/// `request`'s page is not consulted.
///
/// # Errors
///
/// As [`read`], short of reading any page's content.
pub fn pages(
    bytes: &[u8],
    request: &Request<'_>,
    most: usize,
) -> Result<(usize, Vec<(f64, f64)>), ReadError> {
    let document = open(bytes, request)?;
    let listed = document.page_count().min(most);
    let mut sizes = Vec::new();
    sizes.try_reserve_exact(listed)?;
    for index in 0..listed {
        sizes.push(document.page(index).map_err(read_error)?.size());
    }
    Ok((document.page_count(), sizes))
}

/// Read page `request.page` of `bytes`, a PDF, into a drawing of one layer,
/// issuing its objects' identities from `ids`.
///
/// # Errors
///
/// [`ReadError::Unrecognised`] for bytes that are no PDF,
/// [`ReadError::Malformed`] for one damaged past repair,
/// [`ReadError::NeedsPassword`] and [`ReadError::WrongPassword`] for one the
/// password given does not open, [`ReadError::NoSuchPage`] for a page it does
/// not have, [`ReadError::TooComplex`] past a bound of the file's or the
/// drawing's, and [`ReadError::OutOfMemory`]. A page's damaged parts are
/// passed over and recorded, not refused.
pub fn read(bytes: &[u8], request: &Request<'_>, ids: &mut Ids) -> Result<Read, ReadError> {
    if !recognised(bytes) {
        return Err(ReadError::Unrecognised);
    }
    let document = open(bytes, request)?;
    let page = document.page(request.page).map_err(read_error)?;
    let list = page.display_list().map_err(read_error)?;
    drop(page);
    let DisplayList { size, items, lost } = list;
    let mut builder = Builder {
        ids,
        open: Vec::new(),
        pending: None,
        unkept: unkept_of(lost),
        pyramids: OrderedMap::new(),
    };
    try_push(&mut builder.open, Open::layer())?;
    for item in items {
        builder.item(item)?;
    }
    let (layer, mut unkept) = builder.finish()?;
    let page = Page {
        width: size.0,
        height: size.1,
        unit: Unit::Pt,
        background: None,
    };
    let drawing = Drawing::new(
        page,
        Children::single(try_arc(layer)?)?,
        SharedSlice::new(),
        Grid::default(),
    )?;
    if document.page_count() > 1 {
        unkept.insert(Lost::OtherPages);
    }
    let drawing = described(drawing, document.about().map_err(read_error)?, &mut unkept)?;
    Ok(Read { drawing, unkept })
}

/// `drawing` given the title and subject `about` states, and `unkept` what
/// else the document holds that it cannot: a title or subject a drawing's
/// own does not hold is kept from it, recorded as lost.
fn described(drawing: Drawing, about: About, unkept: &mut Unkept) -> Result<Drawing, ReadError> {
    if about.more_parts {
        unkept.insert(Lost::DocumentParts);
    }
    if about.more_metadata {
        unkept.insert(Lost::Annotations);
    }
    if about.title.is_none() && about.subject.is_none() {
        return Ok(drawing);
    }
    let text = |text: Option<String>| text.map(SharedStr::try_from_string).transpose();
    let info = Info {
        title: text(about.title)?,
        description: text(about.subject)?,
    };
    Ok(match drawing.with_info(info) {
        Ok(described) => described,
        Err(DrawingError::Invalid) => {
            unkept.insert(Lost::Annotations);
            drawing
        }
        Err(error) => return Err(error.into()),
    })
}

/// The drawing's losses for what `lib/pdf` could not put in the list.
fn unkept_of(lost: Losses) -> Unkept {
    let mut unkept = Unkept::NONE;
    for (pdf, drawing) in [
        (tairix_pdf::Lost::SoftMasks, Lost::Masks),
        (tairix_pdf::Lost::BlendModes, Lost::BlendModes),
        (tairix_pdf::Lost::TilingPatterns, Lost::Patterns),
        (tairix_pdf::Lost::Shadings, Lost::Shadings),
        (tairix_pdf::Lost::Images, Lost::Images),
        (tairix_pdf::Lost::Text, Lost::UnreadableText),
        (tairix_pdf::Lost::Annotations, Lost::Markup),
        (tairix_pdf::Lost::Damaged, Lost::Damage),
    ] {
        if lost.contains(pdf) {
            unkept.insert(drawing);
        }
    }
    unkept
}

/// A group being filled: the layer, a clip, or a transparency group.
struct Open {
    children: Vec<Arc<Node>>,
    clip: Option<Node>,
    opacity: Option<f64>,
}

impl Open {
    const fn layer() -> Self {
        Self {
            children: Vec::new(),
            clip: None,
            opacity: None,
        }
    }
}

/// A fill waiting to see whether a stroke of the same path follows it.
struct Pending {
    path: Path,
    rule: FillRule,
    paint: Paint,
    transform: Affine,
}

struct Builder<'a> {
    ids: &'a mut Ids,
    open: Vec<Open>,
    pending: Option<Pending>,
    unkept: Unkept,
    /// The pixels of each picture made, by the picture's address, so a
    /// picture shown many times is held once; the picture is held with them,
    /// so no other takes its address while it is a key.
    pyramids: OrderedMap<usize, (Arc<tairix_pdf::Picture>, Arc<Pyramid>)>,
}

impl Builder<'_> {
    fn item(&mut self, item: Item) -> Result<(), ReadError> {
        if let Item::Stroke {
            path,
            pen,
            paint,
            transform,
        } = item
        {
            let filled = self
                .pending
                .take_if(|pending| pending.path == path && pending.transform == transform);
            let stroke = self.stroke(&pen, &paint, transform)?;
            let style = match filled {
                Some(fill) => Style {
                    fill: fill.paint,
                    fill_rule: fill.rule,
                    stroke,
                    paint_order: PaintOrder::FillFirst,
                },
                None => Style {
                    fill: Paint::None,
                    fill_rule: FillRule::NonZero,
                    stroke,
                    paint_order: PaintOrder::FillFirst,
                },
            };
            self.flush()?;
            return self.shape(&path, style, transform);
        }
        self.flush()?;
        match item {
            Item::Fill {
                path,
                rule,
                paint,
                transform,
            } => {
                let paint = self.paint(&paint)?;
                self.pending = Some(Pending {
                    path,
                    rule,
                    paint,
                    transform,
                });
                Ok(())
            }
            Item::Clip {
                path,
                rule,
                transform,
            } => {
                if self.open.len() >= MAX_DEPTH - 1 {
                    return Err(ReadError::TooComplex);
                }
                let style = Style {
                    fill_rule: rule,
                    ..Style::default()
                };
                let clip = match self.shape_node(&path, style, transform) {
                    Ok(node) => Some(node),
                    Err(DrawingError::Invalid) => {
                        self.unkept.insert(Lost::Damage);
                        None
                    }
                    Err(error) => return Err(error.into()),
                };
                try_push(
                    &mut self.open,
                    Open {
                        children: Vec::new(),
                        clip,
                        opacity: None,
                    },
                )?;
                Ok(())
            }
            Item::Group { opacity } => {
                if self.open.len() >= MAX_DEPTH - 1 {
                    return Err(ReadError::TooComplex);
                }
                try_push(
                    &mut self.open,
                    Open {
                        children: Vec::new(),
                        clip: None,
                        opacity: Some(opacity),
                    },
                )?;
                Ok(())
            }
            Item::Unclip | Item::Ungroup => self.close(),
            Item::Image {
                picture,
                transform,
                opacity,
            } => self.image(&picture, transform, opacity),
            Item::Stroke { .. } => Ok(()),
        }
    }

    /// The pending fill made a shape of its own.
    fn flush(&mut self) -> Result<(), ReadError> {
        let Some(fill) = self.pending.take() else {
            return Ok(());
        };
        let style = Style {
            fill: fill.paint,
            fill_rule: fill.rule,
            stroke: Stroke::default(),
            paint_order: PaintOrder::FillFirst,
        };
        self.shape(&fill.path, style, fill.transform)
    }

    /// The innermost open group closed and added to the one around it.
    fn close(&mut self) -> Result<(), ReadError> {
        if self.open.len() < 2 {
            return Ok(());
        }
        let Some(Open {
            children,
            clip,
            opacity,
        }) = self.open.pop()
        else {
            return Ok(());
        };
        if children.is_empty() {
            return Ok(());
        }
        let group = Node::new(
            self.ids.issue(),
            Kind::Group {
                children: Children::from_nodes(children)?,
                clip: clip.map(try_arc).transpose()?,
            },
        )?;
        let group = match opacity {
            Some(opacity) => group.with_opacity(opacity.clamp(0.0, 1.0))?,
            None => group,
        };
        self.add(group)
    }

    /// The layer, every group still open closed into it, and what was lost.
    fn finish(mut self) -> Result<(Node, Unkept), ReadError> {
        self.flush()?;
        while self.open.len() > 1 {
            self.close()?;
        }
        let layer = self.open.pop().unwrap_or_else(Open::layer);
        let node = Node::new(
            self.ids.issue(),
            Kind::Group {
                children: Children::from_nodes(layer.children)?,
                clip: None,
            },
        )?;
        Ok((node, self.unkept))
    }

    fn add(&mut self, node: Node) -> Result<(), ReadError> {
        let node = try_arc(node)?;
        if let Some(open) = self.open.last_mut() {
            try_push(&mut open.children, node)?;
        }
        Ok(())
    }

    /// `path` in `style`, its space into the page's `transform`; a path a
    /// drawing cannot hold is passed over, recorded as damage.
    fn shape(&mut self, path: &Path, style: Style, transform: Affine) -> Result<(), ReadError> {
        match self.shape_node(path, style, transform) {
            Ok(node) => self.add(node),
            Err(DrawingError::Invalid) => {
                self.unkept.insert(Lost::Damage);
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn shape_node(
        &mut self,
        path: &Path,
        style: Style,
        transform: Affine,
    ) -> Result<Node, DrawingError> {
        let shape = Shape::Path(try_arc(PathShape::from_path(path)?)?);
        let node = Node::new(
            self.ids.issue(),
            Kind::Shape {
                shape,
                style: try_arc(style)?,
            },
        )?;
        node.with_transform(in_pixels(transform))
    }

    /// A stroke of `pen` painted with `paint`: the thinnest a device draws
    /// read as a pixel of the page.
    fn stroke(
        &mut self,
        pen: &tairix_pdf::Pen,
        paint: &tairix_pdf::Paint,
        transform: Affine,
    ) -> Result<Stroke, DrawingError> {
        let scale = in_pixels(transform).max_scale();
        let width = if pen.style.width > 0.0 {
            pen.style.width
        } else if scale > 0.0 {
            HAIRLINE / scale
        } else {
            0.0
        };
        let dashes = if pen.dashes.len() <= MAX_DASH_ENTRIES {
            pen.dashes.clone()
        } else {
            self.unkept.insert(Lost::Damage);
            SharedSlice::new()
        };
        Ok(Stroke {
            paint: self.paint(paint)?,
            width,
            join: pen.style.join,
            miter_limit: pen.style.miter_limit.clamp(1.0, 1e4),
            cap: pen.style.end_cap,
            dashes,
            dash_offset: pen.dash_offset,
            start: Arrowhead::NONE,
            end: Arrowhead::NONE,
        })
    }

    fn paint(&mut self, paint: &tairix_pdf::Paint) -> Result<Paint, DrawingError> {
        Ok(match paint {
            tairix_pdf::Paint::Colour(colour) => Paint::Colour(*colour),
            tairix_pdf::Paint::Gradient(gradient) => match self.gradient(gradient)? {
                Some(gradient) => Paint::Gradient(try_arc(gradient)?),
                None => Paint::None,
            },
        })
    }

    /// A shading as a drawing's gradient: its ends' colours carried on past
    /// them where it extends there, and nothing painted past an end where it
    /// does not.
    fn gradient(
        &mut self,
        gradient: &tairix_pdf::Gradient,
    ) -> Result<Option<Gradient>, DrawingError> {
        let mut stops = Vec::new();
        stops.try_reserve_exact(gradient.stops.len() + 2)?;
        stops.extend(gradient.stops.iter().map(|stop| Stop {
            offset: stop.offset,
            colour: stop.colour,
        }));
        let kind = match gradient.kind {
            tairix_pdf::GradientKind::Axial { start, end } => GradientKind::Linear { start, end },
            tairix_pdf::GradientKind::Radial {
                start,
                start_radius,
                end,
                end_radius,
            } => {
                if start_radius > 0.0 {
                    if end_radius <= start_radius || start != end {
                        self.unkept.insert(Lost::Shadings);
                    }
                    let inner = (start_radius / end_radius).clamp(0.0, 1.0);
                    for stop in &mut stops {
                        stop.offset = inner + stop.offset * (1.0 - inner);
                    }
                }
                GradientKind::Radial {
                    centre: end,
                    radius: end_radius,
                    focus: start,
                }
            }
        };
        let clear = Rgba::new(0, 0, 0, 0);
        let (Some(first), Some(last)) = (stops.first().copied(), stops.last().copied()) else {
            return Ok(None);
        };
        thin(
            &mut stops,
            usize::from(!gradient.extend.0) + usize::from(!gradient.extend.1),
        );
        if !gradient.extend.0 {
            stops.insert(
                0,
                Stop {
                    offset: first.offset,
                    colour: clear,
                },
            );
        }
        if !gradient.extend.1 {
            stops.push(Stop {
                offset: last.offset,
                colour: clear,
            });
        }
        Ok(Some(Gradient {
            kind,
            stops,
            spread: Spread::Pad,
            transform: gradient.transform,
        }))
    }

    /// A picture over the unit square `transform` maps onto the page, its
    /// pixels held once however often it is shown.
    fn image(
        &mut self,
        picture: &Arc<tairix_pdf::Picture>,
        transform: Affine,
        opacity: f64,
    ) -> Result<(), ReadError> {
        let (width, height) = (f64::from(picture.width), f64::from(picture.height));
        let key = Arc::as_ptr(picture).addr();
        let pyramid = if let Some((_, pyramid)) = self.pyramids.get(&key) {
            Arc::clone(pyramid)
        } else {
            let surface = Surface::from_rgba8(picture.width, picture.height, &picture.pixels)
                .ok_or(ReadError::OutOfMemory)?;
            let pyramid = try_arc(Pyramid::new(surface).ok_or(ReadError::OutOfMemory)?)?;
            self.pyramids
                .try_insert(key, (Arc::clone(picture), Arc::clone(&pyramid)))?;
            pyramid
        };
        let source = picture.jpeg.as_ref().and_then(|bytes| {
            let frame = tairix_image::jpeg_frame(bytes).ok()?;
            Some(Source {
                encoding: Encoding::Jpeg(frame),
                bytes: bytes.clone(),
            })
        });
        let place = Bounds::new(Point::ZERO, Point::new(width, height));
        let made = Image::shared(pyramid, place, source).and_then(|image| {
            let node = Node::new(
                self.ids.issue(),
                Kind::Image {
                    image: try_arc(image)?,
                },
            )?;
            let unit = Affine::scale(1.0 / width, 1.0 / height);
            node.with_transform(in_pixels(unit.then(transform)))?
                .with_opacity(opacity.clamp(0.0, 1.0))
        });
        match made {
            Ok(node) => self.add(node),
            Err(DrawingError::Invalid) => {
                self.unkept.insert(Lost::Damage);
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// `transform`, into the page's points, carried on into the drawing's pixels.
fn in_pixels(transform: Affine) -> Affine {
    let pixels = Unit::Pt.pixels();
    transform.then(Affine::scale(pixels, pixels))
}

/// Thin `stops` so `room` more fit within a drawing's bound: every other
/// stop between the ends dropped until they do.
fn thin(stops: &mut Vec<Stop>, room: usize) {
    while stops.len() + room > MAX_STOPS && stops.len() > 2 {
        let last = stops.len() - 1;
        let mut index = 0;
        stops.retain(|_| {
            let keep = index == 0 || index == last || index % 2 == 0;
            index += 1;
            keep
        });
    }
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
