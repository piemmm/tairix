//! Reading an SVG document into a drawing.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_bezier::{Affine, Bounds, Path, PathEl, PathMeasure, Point, Subpath};
use tairix_collections::{try_arc, OrderedMap, SharedSlice, SharedStr};
use tairix_colour::Rgba;
use tairix_svg::css::{self, Declaration, SheetLimits, Stylesheet};
use tairix_svg::font::FontStyle;
use tairix_svg::number::{parse_length, parse_number, viewport_diagonal};
use tairix_svg::paint::{GradientDefinition, GradientGeometry, PaintServers};
use tairix_svg::pathdata::parse_path_curves;
use tairix_svg::shape::{shape_geometry, ShapeGeometry};
use tairix_svg::style::{BaselineShift, PaintSlot, PaintSpec, Style as SvgStyle, TextAnchor};
use tairix_svg::textpath;
use tairix_svg::transform::{
    parse_aspect_ratio, parse_transform, parse_view_box, viewport_transform, Align, AspectRatio,
};
use tairix_svg::xml::{self, Content, Element, Limits};
use tairix_svg::SvgError;
use tairix_util::fallible::{self, try_push};
use tairix_util::mathf::{fabs, fmax};

use super::{data_url, DRAW_NAMESPACE, INKSCAPE_NAMESPACE};
use crate::children::Children;
use crate::drawing::{Drawing, Grid, Guide, GuideAxis, Info, Page, Unit, PAPER, PIXELS_PER_INCH};
use crate::id::Ids;
use crate::image::{self, Pictures};
use crate::limits::{
    self, MAX_BASELINE_SHIFT, MAX_DEPTH, MAX_FAMILY_BYTES, MAX_GUIDES, MAX_IMAGE_SOURCE_BYTES,
    MAX_OBJECTS, MAX_PATH_NODES, MAX_RESOLUTION, MAX_SUBDIVISIONS, MAX_SWATCHES, MAX_TEXT_FAMILIES,
    MAX_TEXT_PATH_NODES,
};
use crate::node::{Kind, Node};
use crate::shape::{
    ArcKind, ArcPart, EllipseShape, NodeJoin, PathShape, RectShape, Shape, StarShape,
};
use crate::style::{
    ArrowShape, Arrowhead, Cap, Gradient, GradientKind, Join, Paint, PaintOrder, Spread, Stop,
    Stroke, Style,
};
use crate::text::{
    canonical, Baseline, Run, RunPaint, RunStyle, Text, TextAlign, TextPath, DEFAULT_FAMILY,
    DEFAULT_LINE_HEIGHT,
};
use crate::unkept::{Lost, Unkept};
use crate::{DrawingError, Read, ReadError};

/// How many elements a read may visit, every `<use>` expansion counted: a
/// containment bound on the work a document of references can ask for.
const MAX_VISITS: usize = 4 * MAX_OBJECTS;

/// How deeply `<use>` references may nest.
const MAX_USE_DEPTH: usize = 16;

impl From<SvgError> for ReadError {
    fn from(error: SvgError) -> Self {
        match error {
            SvgError::NotUtf8 | SvgError::MissingRoot => Self::Unrecognised,
            SvgError::TooComplex => Self::TooComplex,
            SvgError::OutOfMemory => Self::OutOfMemory,
            _ => Self::Malformed,
        }
    }
}

/// Read `bytes`, an SVG document, into a drawing, issuing its objects'
/// identities from `ids`.
///
/// # Errors
///
/// [`ReadError`] for a document that is not text, not SVG, malformed or past
/// a bound, or that cannot be held. Nothing is half-read.
pub fn read(bytes: &[u8], ids: &mut Ids) -> Result<Read, ReadError> {
    let text = core::str::from_utf8(bytes).map_err(|_| ReadError::Unrecognised)?;
    let root = xml::parse(text, Limits::DOCUMENT)?;
    if root.name != "svg" {
        return Err(ReadError::Unrecognised);
    }
    let sheets = css::sheet_texts(&root);
    let sheet = Stylesheet::collect(&sheets, SheetLimits::DOCUMENT)?;
    let names = Names::of(&root);
    let (page, to_page, viewport) = page_of(&root, &names)?;
    let (info, info_elements) = info_of(&root)?;
    let mut reader = Reader {
        root: &root,
        ids,
        names,
        sheet,
        servers: PaintServers::collect(&root)?,
        index: Vec::new(),
        path: Vec::new(),
        cascade: Vec::new(),
        viewport,
        unkept: Unkept::NONE,
        visits_left: MAX_VISITS,
        use_depth: 0,
        nesting: 0,
        expanding: Vec::new(),
        info_elements,
        background: page.background.is_some(),
        gradients: OrderedMap::new(),
        pictures: Pictures::new(),
        clipping: false,
    };
    reader.index(&root)?;
    reader.path.push(&root);
    let style = reader.style_of(&SvgStyle::default(), &root)?;
    let layers = reader.layers(&root, &style, to_page)?;
    reader.path.pop();
    let guides = reader.guides()?;
    let grid = reader.grid()?;
    let swatches = reader.swatches()?;
    let drawing = Drawing::new(page, layers, guides, grid)?
        .with_info(info)?
        .with_swatches(swatches)?
        .with_resolution(reader.resolution())?;
    Ok(Read {
        drawing,
        unkept: reader.unkept,
    })
}

/// The qualified names Draw's and Inkscape's attributes go by in this
/// document, whatever prefixes it bound their namespaces to.
struct Names<'a> {
    draw: &'a str,
    inkscape: &'a str,
}

impl<'a> Names<'a> {
    fn of(root: &'a Element<'a>) -> Self {
        let prefix_of = |namespace: &str, default: &'a str| {
            root.attrs
                .iter()
                .find_map(|(key, value)| {
                    key.strip_prefix("xmlns:")
                        .filter(|_| value.as_ref() == namespace)
                })
                .unwrap_or(default)
        };
        Self {
            draw: prefix_of(DRAW_NAMESPACE, "draw"),
            inkscape: prefix_of(INKSCAPE_NAMESPACE, "inkscape"),
        }
    }

    fn draw<'e>(&self, element: &'e Element<'_>, local: &str) -> Option<&'e str> {
        attribute(element, self.draw, local)
    }

    fn inkscape<'e>(&self, element: &'e Element<'_>, local: &str) -> Option<&'e str> {
        attribute(element, self.inkscape, local)
    }
}

/// `element`'s `prefix:local` attribute.
fn attribute<'e>(element: &'e Element<'_>, prefix: &str, local: &str) -> Option<&'e str> {
    element.attrs.iter().find_map(|(key, value)| {
        let (p, l) = key.split_once(':')?;
        (p == prefix && l == local).then_some(value.as_ref())
    })
}

struct Reader<'a, 'i> {
    root: &'a Element<'a>,
    ids: &'i mut Ids,
    names: Names<'a>,
    sheet: Stylesheet<'a>,
    servers: PaintServers<'a>,
    /// Every element carrying an `id`, sorted by it and then by document
    /// order, so a reference finds the first of a repeated id by a binary
    /// search.
    index: Vec<(&'a str, usize, &'a Element<'a>)>,
    /// The elements from the root down to the one being read, which the
    /// stylesheet's selectors are matched against.
    path: Vec<&'a Element<'a>>,
    cascade: Vec<Declaration<'a>>,
    /// What percentages resolve against.
    viewport: (f64, f64),
    unkept: Unkept,
    visits_left: usize,
    use_depth: usize,
    /// How deeply reading has descended, every expansion of a reference
    /// counted: bounded by the depth a drawing may hold, so no document can
    /// recurse the reader past its stack.
    nesting: usize,
    /// The `<use>` targets and clip paths being expanded, by address: a
    /// reference back to one of them is a cycle, which SVG makes an error.
    expanding: Vec<usize>,
    /// The root's `<title>` and `<desc>` the drawing's own were read from,
    /// by address.
    info_elements: [Option<usize>; 2],
    /// Whether the page has a background, which Draw writes a rectangle for.
    background: bool,
    /// Every gradient paint resolved so far, so every reference that comes
    /// to the same gradient shares it, as the document wrote it once.
    gradients: OrderedMap<GradientKey, Arc<Gradient>>,
    /// Every picture read so far, by its link, so the same picture shown
    /// again is decoded once and shared.
    pictures: Pictures<'a>,
    /// Whether a clip path's content is being read: its shapes clip by
    /// `clip-rule`, and a picture in one is no part of it.
    clipping: bool,
}

/// What a resolved gradient paint depends on: its server, the opacity and
/// current colour it was resolved at, and — in bounding-box units — the box.
type GradientKey = (usize, u64, u32, [u64; 4]);

impl<'a> Reader<'a, '_> {
    fn index(&mut self, root: &'a Element<'a>) -> Result<(), ReadError> {
        self.gather_ids(root)?;
        self.index
            .sort_unstable_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(&b.1)));
        Ok(())
    }

    fn gather_ids(&mut self, element: &'a Element<'a>) -> Result<(), ReadError> {
        if let Some(id) = element.attr("id") {
            let order = self.index.len();
            if !fallible::push(&mut self.index, (id, order, element)) {
                return Err(ReadError::OutOfMemory);
            }
        }
        for child in element.children() {
            self.gather_ids(child)?;
        }
        Ok(())
    }

    fn find(&self, id: &str) -> Option<&'a Element<'a>> {
        let at = self.index.partition_point(|(name, ..)| *name < id);
        self.index
            .get(at)
            .filter(|(name, ..)| *name == id)
            .map(|(.., element)| *element)
    }

    fn referenced(&self, element: &Element<'_>) -> Option<&'a Element<'a>> {
        xml::href(element)
            .and_then(|link| link.strip_prefix('#'))
            .and_then(|id| self.find(id))
    }

    /// `element`'s style, given what it inherits; `element` is the last of
    /// the selector path.
    fn style_of(
        &mut self,
        inherited: &SvgStyle,
        element: &Element<'_>,
    ) -> Result<SvgStyle, ReadError> {
        self.cascade.clear();
        if !self.sheet.is_empty() {
            self.sheet.cascade(&self.path, &mut self.cascade);
        }
        Ok(inherited.apply(element, viewport_diagonal(self.viewport), &self.cascade)?)
    }

    /// Note a filter on `element`, which the cascade just styled.
    fn note_filter(&mut self, element: &Element<'_>) {
        if self.declares(element, "filter") {
            self.unkept.insert(Lost::Filters);
        }
    }

    /// Whether the cascade set `name` on the element last styled, or the
    /// element carries it as an attribute.
    fn declares(&self, element: &Element<'_>, name: &str) -> bool {
        element
            .attr(name)
            .is_some_and(|value| value.trim() != "none")
            || self
                .cascade
                .iter()
                .any(|declaration| declaration.name == name && declaration.value.trim() != "none")
            || element.attr("style").is_some_and(|text| {
                css::declarations(text).any(|d| d.name == name && d.value.trim() != "none")
            })
    }

    /// `read`, one level deeper.
    fn deeper<T>(
        &mut self,
        read: impl FnOnce(&mut Self) -> Result<T, ReadError>,
    ) -> Result<T, ReadError> {
        if self.nesting >= MAX_DEPTH {
            return Err(ReadError::TooComplex);
        }
        self.nesting += 1;
        let result = read(self);
        self.nesting -= 1;
        result
    }

    /// `read`, expanding the reference to `definition`.
    fn expanding<T>(
        &mut self,
        definition: &Element<'_>,
        read: impl FnOnce(&mut Self) -> Result<T, ReadError>,
    ) -> Result<T, ReadError> {
        let address = core::ptr::from_ref(definition).addr();
        if self.expanding.contains(&address) {
            return Err(ReadError::Malformed);
        }
        if !fallible::push(&mut self.expanding, address) {
            return Err(ReadError::OutOfMemory);
        }
        let result = self.deeper(read);
        self.expanding.pop();
        result
    }

    fn visit(&mut self) -> Result<(), ReadError> {
        self.visits_left = self
            .visits_left
            .checked_sub(1)
            .ok_or(ReadError::TooComplex)?;
        Ok(())
    }

    /// The document's layers: each top-level group marked as one — by
    /// Draw or by Inkscape — and every run of other top-level content
    /// between them as a layer of its own, so their order is kept.
    fn layers(
        &mut self,
        root: &'a Element<'a>,
        style: &SvgStyle,
        to_page: Affine,
    ) -> Result<Children, ReadError> {
        let inherited = style.inherit();
        let mut layers: Vec<Arc<Node>> = Vec::new();
        let mut loose: Vec<Arc<Node>> = Vec::new();
        for child in root.children() {
            if self.is_layer(child) {
                self.flush_loose(&mut loose, &mut layers, to_page)?;
                self.path.push(child);
                let layer = self.layer(child, &inherited, to_page);
                self.path.pop();
                try_push(&mut layers, layer?)?;
            } else if !self.is_background(child) && !self.is_info(child) {
                self.read_into(child, &inherited, &mut loose)?;
            }
        }
        self.flush_loose(&mut loose, &mut layers, to_page)?;
        if layers.is_empty() {
            let empty = self.group_node(Vec::new(), None, to_page)?;
            try_push(&mut layers, try_arc(empty)?)?;
        }
        Ok(Children::from_nodes(layers)?)
    }

    fn is_layer(&self, element: &Element<'_>) -> bool {
        element.name == "g"
            && (self.names.draw(element, "layer") == Some("true")
                || self.names.inkscape(element, "groupmode") == Some("layer"))
    }

    /// Whether `element` is the rectangle Draw paints the page's background
    /// with, which the page itself stands for.
    fn is_background(&self, element: &Element<'_>) -> bool {
        self.background
            && element.name == "rect"
            && self.names.draw(element, "role") == Some("background")
    }

    fn is_info(&self, element: &Element<'_>) -> bool {
        self.info_elements
            .contains(&Some(core::ptr::from_ref(element).addr()))
    }

    /// Gather the loose top-level content read so far into a layer.
    fn flush_loose(
        &mut self,
        loose: &mut Vec<Arc<Node>>,
        layers: &mut Vec<Arc<Node>>,
        to_page: Affine,
    ) -> Result<(), ReadError> {
        if loose.is_empty() {
            return Ok(());
        }
        let content = core::mem::take(loose);
        let layer = self.group_node(content, None, to_page)?;
        Ok(try_push(layers, try_arc(layer)?)?)
    }

    /// One layer group: what a group reads as, placed on the page.
    fn layer(
        &mut self,
        element: &'a Element<'a>,
        inherited: &SvgStyle,
        to_page: Affine,
    ) -> Result<Arc<Node>, ReadError> {
        self.visit()?;
        let style = self.style_of(inherited, element)?;
        self.note_filter(element);
        let own = transform_of(element)?;
        let mut children = Vec::new();
        let child_style = style.inherit();
        for child in element.children() {
            self.read_into(child, &child_style, &mut children)?;
        }
        let clip = self.clip_of(&style, &children)?;
        let layer = self.group_node(children, clip, own.then(to_page))?;
        Ok(try_arc(self.dressed(&layer, element, &style)?)?)
    }

    /// Read `element` and push what it comes to onto `out`.
    fn read_into(
        &mut self,
        element: &'a Element<'a>,
        inherited: &SvgStyle,
        out: &mut Vec<Arc<Node>>,
    ) -> Result<(), ReadError> {
        self.path.push(element);
        let read = self.deeper(|reader| reader.element(element, inherited));
        self.path.pop();
        if let Some(node) = read? {
            try_push(out, node)?;
        }
        Ok(())
    }

    /// What `element`, the last of the selector path, reads as.
    fn element(
        &mut self,
        element: &'a Element<'a>,
        inherited: &SvgStyle,
    ) -> Result<Option<Arc<Node>>, ReadError> {
        self.note_unkept_element(element);
        if !is_drawable(element.name) {
            return Ok(None);
        }
        self.visit()?;
        let style = self.style_of(inherited, element)?;
        self.note_filter(element);
        let own = transform_of(element)?;
        let node = match element.name {
            "g" | "a" => {
                if element.name == "a" {
                    self.unkept.insert(Lost::Links);
                }
                let mut children = Vec::new();
                let child_style = style.inherit();
                for child in element.children() {
                    self.read_into(child, &child_style, &mut children)?;
                }
                let clip = self.clip_of(&style, &children)?;
                Some(self.group_node(children, clip, own)?)
            }
            "switch" => {
                let candidates = element
                    .children()
                    .filter(|child| is_drawable(child.name))
                    .count();
                if candidates > 1 {
                    self.unkept.insert(Lost::Alternatives);
                }
                let mut children = Vec::new();
                if let Some(chosen) = tairix_svg::document::switch_choice(element) {
                    self.read_into(chosen, &style.inherit(), &mut children)?;
                }
                Some(self.group_node(children, None, own)?)
            }
            "use" => self.use_of(element, &style, own)?,
            "svg" => self.nested_viewport(element, &style, own, None)?,
            "text" => self.text_node(element, &style, own)?,
            "image" if self.clipping => None,
            "image" => self.image_node(element, own)?,
            _ => self.shape_node(element, &style, own)?,
        };
        match node {
            Some(node) => {
                let node = self.dressed(&node, element, &style)?;
                let clipped = self.clipped(node, element, &style)?;
                Ok(Some(try_arc(clipped)?))
            }
            None => Ok(None),
        }
    }

    /// Note what `element` is that the drawing cannot keep.
    fn note_unkept_element(&mut self, element: &Element<'_>) {
        let lost = match element.name {
            "foreignObject" => Some(Lost::ForeignContent),
            "animate" | "animateMotion" | "animateTransform" | "set" | "animateColor" => {
                Some(Lost::Animation)
            }
            "script" => Some(Lost::Scripts),
            "title" | "desc" | "metadata" => Some(Lost::Annotations),
            _ => None,
        };
        if let Some(lost) = lost {
            self.unkept.insert(lost);
        }
    }

    /// `node` with the visibility, opacity, name and lock `element` states.
    fn dressed(
        &mut self,
        node: &Node,
        element: &Element<'_>,
        style: &SvgStyle,
    ) -> Result<Node, ReadError> {
        if style.mask.is_some() {
            self.unkept.insert(Lost::Masks);
        }
        let mut node = node.with_opacity(style.opacity.clamp(0.0, 1.0))?;
        if !style.display || !style.visible {
            node = node.with_visible(false);
        }
        if self.names.draw(element, "locked") == Some("true")
            || self.names.inkscape(element, "insensitive") == Some("true")
        {
            node = node.with_locked(true);
        }
        let name = self
            .names
            .draw(element, "name")
            .or_else(|| self.names.inkscape(element, "label"))
            .or_else(|| element.attr("id"))
            .filter(|name| crate::node::check_name(Some(name)).is_ok());
        if let Some(name) = name {
            node = node.with_name(Some(SharedStr::try_from_str(name)?))?;
        }
        Ok(node)
    }

    /// `node` inside a group clipped as `element`'s `clip-path` says, or
    /// `node` itself when it says nothing; a group's own clip is its own.
    /// The clip is in the element's own space, so the wrapping group takes
    /// over the element's transform and the element keeps none of its own.
    fn clipped(
        &mut self,
        node: Node,
        element: &'a Element<'a>,
        style: &SvgStyle,
    ) -> Result<Node, ReadError> {
        if element.name == "g" || element.name == "a" || style.clip_path.is_none() {
            return Ok(node);
        }
        let own = node.transform();
        let mut held = Vec::new();
        try_push(&mut held, try_arc(node.with_transform(Affine::IDENTITY)?)?)?;
        let Some(clip) = self.clip_of(style, &held)? else {
            return Ok(node);
        };
        let wrapped = self.group_node(Vec::new(), None, own)?;
        Ok(wrapped.with_kind(Kind::Group {
            children: Children::from_nodes(held)?,
            clip: Some(clip),
        })?)
    }

    /// The clip `style`'s `clip-path` names, in the space of `content`,
    /// built from the `<clipPath>`'s own shapes; `None` when it names
    /// nothing.
    fn clip_of(
        &mut self,
        style: &SvgStyle,
        content: &[Arc<Node>],
    ) -> Result<Option<Arc<Node>>, ReadError> {
        let Some(id) = style.clip_path.as_deref() else {
            return Ok(None);
        };
        let Some(definition) = self.find(id).filter(|found| found.name == "clipPath") else {
            return Ok(None);
        };
        let units = match definition.attr("clipPathUnits") {
            Some("objectBoundingBox") => {
                let bounds = content
                    .iter()
                    .fold(Bounds::EMPTY, |bounds, node| bounds.union(node.bounds()));
                if bounds.is_empty() {
                    return Ok(None);
                }
                Affine::scale(bounds.width(), bounds.height())
                    .then(Affine::translate(bounds.min.x, bounds.min.y))
            }
            _ => Affine::IDENTITY,
        };
        let own = transform_of(definition)?.then(units);
        let outer = core::mem::take(&mut self.path);
        self.path.push(self.root);
        self.path.push(definition);
        let mut shapes = Vec::new();
        let was_clipping = core::mem::replace(&mut self.clipping, true);
        let read = self.expanding(definition, |reader| {
            let inherited = reader.style_of(&SvgStyle::default(), definition)?.inherit();
            definition
                .children()
                .try_for_each(|child| reader.read_into(child, &inherited, &mut shapes))
        });
        self.clipping = was_clipping;
        self.path = outer;
        read?;
        // A clip of one shape is that shape, placed as the clip path places
        // it; only several need a group to hold them.
        if shapes.len() == 1 {
            if let Some(only) = shapes.pop() {
                let placed = only.with_transform(only.transform().then(own))?;
                return Ok(Some(try_arc(placed)?));
            }
        }
        let clip = self.group_node(shapes, None, own)?;
        Ok(Some(try_arc(clip)?))
    }

    /// A `<use>`: what it references, inside a group placed at its offset —
    /// or, of a picture Draw writes once for every object showing it, that
    /// object.
    fn use_of(
        &mut self,
        element: &'a Element<'a>,
        style: &SvgStyle,
        own: Affine,
    ) -> Result<Option<Node>, ReadError> {
        let Some(target) = self.referenced(element) else {
            return Ok(None);
        };
        if self.use_depth >= MAX_USE_DEPTH {
            return Err(ReadError::TooComplex);
        }
        let x = optional_length(element, "x", self.viewport.0)?;
        let y = optional_length(element, "y", self.viewport.1)?;
        if target.name == "symbol" && self.names.draw(target, "picture") == Some("true") {
            // A clip path holds no picture, as it holds no `<image>`.
            if self.clipping {
                return Ok(None);
            }
            let slot = (
                optional_attr_length(element, "width", self.viewport.0)?,
                optional_attr_length(element, "height", self.viewport.1)?,
            );
            if let (Some(width), Some(height)) = slot {
                let place = Bounds::new(Point::new(x, y), Point::new(x + width, y + height));
                return self.shown_picture(target, place, own);
            }
        }
        let placed = Affine::translate(x, y).then(own);
        self.use_depth += 1;
        let read = self.expanding(target, |reader| {
            if target.name == "symbol" {
                let slot = (
                    optional_attr_length(element, "width", reader.viewport.0)?,
                    optional_attr_length(element, "height", reader.viewport.1)?,
                );
                reader.path.push(target);
                let read = reader
                    .style_of(&style.inherit(), target)
                    .and_then(|symbol_style| {
                        reader.nested_viewport(target, &symbol_style, placed, Some(slot))
                    });
                reader.path.pop();
                read
            } else {
                let mut children = Vec::new();
                reader.read_into(target, &style.inherit(), &mut children)?;
                Ok(Some(reader.group_node(children, None, placed)?))
            }
        });
        self.use_depth -= 1;
        read
    }

    /// A nested `<svg>` or a `<symbol>`: its content placed in its own
    /// viewport, clipped to it unless its `overflow` shows what spills.
    fn nested_viewport(
        &mut self,
        element: &'a Element<'a>,
        style: &SvgStyle,
        own: Affine,
        slot: Option<(Option<f64>, Option<f64>)>,
    ) -> Result<Option<Node>, ReadError> {
        let x = optional_length(element, "x", self.viewport.0)?;
        let y = optional_length(element, "y", self.viewport.1)?;
        let (slot_width, slot_height) = slot.unwrap_or((None, None));
        let width = match slot_width {
            Some(width) => width,
            None => {
                optional_attr_length(element, "width", self.viewport.0)?.unwrap_or(self.viewport.0)
            }
        };
        let height = match slot_height {
            Some(height) => height,
            None => {
                optional_attr_length(element, "height", self.viewport.1)?.unwrap_or(self.viewport.1)
            }
        };
        if width <= 0.0 || height <= 0.0 {
            return Ok(None);
        }
        let (inner, viewport) = match element.attr("viewBox") {
            Some(text) => {
                let view_box = parse_view_box(text)?;
                let ratio = match element.attr("preserveAspectRatio") {
                    Some(spec) => parse_aspect_ratio(spec)?,
                    None => AspectRatio::default(),
                };
                (
                    viewport_transform(view_box, (width, height), ratio),
                    view_box.size,
                )
            }
            None => (Affine::IDENTITY, (width, height)),
        };
        let outer = core::mem::replace(&mut self.viewport, viewport);
        let mut children = Vec::new();
        let child_style = style.inherit();
        let read = element
            .children()
            .try_for_each(|child| self.read_into(child, &child_style, &mut children));
        self.viewport = outer;
        read?;
        let content = self.group_node(children, None, inner)?;
        let placed = Affine::translate(x, y).then(own);
        if style.overflow == tairix_svg::style::Overflow::Visible {
            return Ok(Some(content.with_transform(inner.then(placed))?));
        }
        let window = self.shape_from(
            Shape::Rect(RectShape {
                x: 0.0,
                y: 0.0,
                width,
                height,
                rx: 0.0,
                ry: 0.0,
            }),
            Style::default(),
        )?;
        let frame = self.group_node(Vec::new(), None, placed)?;
        Ok(Some(frame.with_kind(Kind::Group {
            children: Children::single(try_arc(content)?)?,
            clip: Some(try_arc(window)?),
        })?))
    }

    /// The picture the symbol `target` holds, Draw's for one shown by several
    /// objects, laid over `place`: its pixels decoded once however often it
    /// is shown.
    fn shown_picture(
        &mut self,
        target: &'a Element<'a>,
        place: Bounds,
        own: Affine,
    ) -> Result<Option<Node>, ReadError> {
        if !(place.width() > 0.0 && place.height() > 0.0) {
            return Ok(None);
        }
        let link = target
            .children()
            .find(|child| child.name == "image")
            .and_then(|image| xml::href(image));
        let Some(picture) = link.map(|link| self.decoded(link)).transpose()?.flatten() else {
            self.unkept.insert(Lost::Images);
            return Ok(None);
        };
        let node = Node::new(
            self.ids.issue(),
            Kind::Image {
                image: try_arc(picture.placed(place)?)?,
            },
        )?;
        Ok(Some(node.with_transform(own)?))
    }

    /// The picture `link` carries as a data URL — a PNG or JPEG known by its
    /// content — decoded once however many elements show it; `None` for one
    /// linked from elsewhere, of another kind, or broken.
    fn decoded(&mut self, link: &'a str) -> Result<Option<image::Picture>, ReadError> {
        self.pictures.picture(link.as_bytes(), |limits| {
            use tairix_image::DecodeError;
            match data_url::decode(link, MAX_IMAGE_SOURCE_BYTES) {
                Ok(Some(bytes)) => {
                    let bytes =
                        SharedSlice::try_from_vec(bytes).map_err(|_| DecodeError::OutOfMemory)?;
                    image::decode_encoded(bytes, limits)
                }
                Err(DrawingError::OutOfMemory) => Err(DecodeError::OutOfMemory),
                Ok(None) | Err(_) => Err(DecodeError::UnknownFormat),
            }
        })
    }

    /// An `<image>`: a PNG or JPEG its link carries as a data URL, laid in
    /// the rectangle it states as its `preserveAspectRatio` lays it and cut
    /// to that rectangle where it overflows. A picture linked from anywhere
    /// else, or of another kind, is lost.
    fn image_node(
        &mut self,
        element: &'a Element<'a>,
        own: Affine,
    ) -> Result<Option<Node>, ReadError> {
        let Some(link) = xml::href(element) else {
            return Ok(None);
        };
        let x = optional_length(element, "x", self.viewport.0)?;
        let y = optional_length(element, "y", self.viewport.1)?;
        // SVG 2's `auto` is the side left unstated.
        let side = |name, basis| match element.attr(name) {
            Some(text) if text.trim() == "auto" => Ok(None),
            _ => optional_attr_length(element, name, basis),
        };
        let stated = (
            side("width", self.viewport.0)?,
            side("height", self.viewport.1)?,
        );
        if stated.0.is_some_and(|width| width <= 0.0)
            || stated.1.is_some_and(|height| height <= 0.0)
        {
            return Ok(None);
        }
        let Some(picture) = self.decoded(link)? else {
            self.unkept.insert(Lost::Images);
            return Ok(None);
        };
        // A side the element leaves unstated is the picture's own, a pixel
        // to the unit, or what its ratio makes of the side it states.
        let (pixels_x, pixels_y) = picture.size();
        let natural = (f64::from(pixels_x), f64::from(pixels_y));
        let (width, height) = match stated {
            (Some(width), Some(height)) => (width, height),
            (Some(width), None) => (width, width * natural.1 / natural.0),
            (None, Some(height)) => (height * natural.0 / natural.1, height),
            (None, None) => natural,
        };
        let ratio = match element.attr("preserveAspectRatio") {
            Some(spec) => parse_aspect_ratio(spec)?,
            None => AspectRatio::default(),
        };
        let view_box = tairix_svg::transform::ViewBox {
            min: (0.0, 0.0),
            size: natural,
        };
        // Stretched, the rectangle is the one stated, spelled from it rather
        // than scaled to the pixels and back, which rounding would move.
        let place = if ratio.align == Align::None {
            Bounds::new(Point::new(x, y), Point::new(x + width, y + height))
        } else {
            let laid =
                viewport_transform(view_box, (width, height), ratio).then(Affine::translate(x, y));
            Bounds::from_points([
                laid.map(Point::ZERO),
                laid.map(Point::new(natural.0, natural.1)),
            ])
        };
        let shown = picture.placed(place)?;
        let node = Node::new(
            self.ids.issue(),
            Kind::Image {
                image: try_arc(shown)?,
            },
        )?;
        // Only a ratio kept by slicing lays a picture past its rectangle,
        // and not one whose shape is the rectangle's to within rounding.
        let (across, down) = (natural.0 * height, natural.1 * width);
        let overflows = ratio.slice
            && ratio.align != Align::None
            && fabs(across - down) > f64::EPSILON * fmax(fabs(across), fabs(down));
        if !overflows {
            return Ok(Some(node.with_transform(own)?));
        }
        let window = self.shape_from(
            Shape::Rect(RectShape {
                x,
                y,
                width,
                height,
                rx: 0.0,
                ry: 0.0,
            }),
            Style::default(),
        )?;
        let cut = self.group_node(Vec::new(), None, own)?;
        Ok(Some(cut.with_kind(Kind::Group {
            children: Children::single(try_arc(node)?)?,
            clip: Some(try_arc(window)?),
        })?))
    }

    /// A basic shape.
    fn shape_node(
        &mut self,
        element: &'a Element<'a>,
        style: &SvgStyle,
        own: Affine,
    ) -> Result<Option<Node>, ReadError> {
        let Some(geometry) = shape_geometry(element, self.viewport, MAX_PATH_NODES)? else {
            return Ok(None);
        };
        let shape = match geometry {
            ShapeGeometry::Path(data) => self.live_shape(element).map_or_else(
                || -> Result<Shape, ReadError> {
                    let mut path = PathShape::from_path(&parse_path_curves(data, MAX_PATH_NODES)?)?;
                    self.apply_node_types(element, &mut path);
                    Ok(Shape::Path(try_arc(path)?))
                },
                Ok,
            )?,
            ShapeGeometry::Rect {
                x,
                y,
                width,
                height,
                rx,
                ry,
            } => Shape::Rect(RectShape {
                x,
                y,
                width,
                height,
                rx,
                ry,
            }),
            ShapeGeometry::Ellipse { centre, radii } => Shape::Ellipse(EllipseShape {
                centre: centre.into(),
                radii: radii.into(),
                arc: None,
            }),
            ShapeGeometry::Line { from, to } => {
                let mut subpath = Subpath::new(from.into());
                try_push(&mut subpath.elements, PathEl::Line(to.into()))?;
                Shape::Path(try_arc(PathShape::from_path(&Path::single(subpath)?)?)?)
            }
            ShapeGeometry::Polyline { points, closed } => {
                let Some((&first, rest)) = points.split_first() else {
                    return Ok(None);
                };
                let mut subpath = Subpath::new(first.into());
                if !fallible::reserve(&mut subpath.elements, rest.len()) {
                    return Err(ReadError::OutOfMemory);
                }
                subpath
                    .elements
                    .extend(rest.iter().map(|&point| PathEl::Line(point.into())));
                subpath.closed = closed;
                Shape::Path(try_arc(PathShape::from_path(&Path::single(subpath)?)?)?)
            }
        };
        let bounds = shape.bounds()?;
        let drawn = self.style(element, style, bounds)?;
        let node = self.shape_from(shape, drawn)?;
        Ok(Some(node.with_transform(own)?))
    }

    /// The live shape Draw recorded on a path, if any and if it reads.
    fn live_shape(&self, element: &Element<'_>) -> Option<Shape> {
        let names = &self.names;
        let number = |local: &str| -> Option<f64> {
            names
                .draw(element, local)
                .and_then(|text| tairix_svg::number::parse_number(text).ok())
        };
        let shape = match names.draw(element, "shape") {
            Some("arc") => {
                let (Some(cx), Some(cy), Some(rx), Some(ry), Some(start), Some(end)) = (
                    number("cx"),
                    number("cy"),
                    number("rx"),
                    number("ry"),
                    number("start"),
                    number("end"),
                ) else {
                    return None;
                };
                let kind = match names.draw(element, "arc") {
                    Some("pie") => ArcKind::Pie,
                    Some("chord") => ArcKind::Chord,
                    _ => ArcKind::Open,
                };
                Shape::Ellipse(EllipseShape {
                    centre: Point::new(cx, cy),
                    radii: Point::new(rx, ry),
                    arc: Some(ArcPart { start, end, kind }),
                })
            }
            Some("star") => {
                let (
                    Some(cx),
                    Some(cy),
                    Some(points),
                    Some(outer),
                    Some(inner),
                    Some(rotation),
                    Some(rounding),
                ) = (
                    number("cx"),
                    number("cy"),
                    number("points"),
                    number("outer"),
                    number("inner"),
                    number("rotation"),
                    number("rounding"),
                )
                else {
                    return None;
                };
                if !(3.0..=f64::from(crate::limits::MAX_STAR_POINTS)).contains(&points) {
                    return None;
                }
                Shape::Star(StarShape {
                    centre: Point::new(cx, cy),
                    points: whole_points(points),
                    outer,
                    inner_ratio: inner,
                    rotation,
                    rounding,
                    polygon: names.draw(element, "polygon") == Some("true"),
                })
            }
            _ => return None,
        };
        shape.check().is_ok().then_some(shape)
    }

    /// Set the node types Draw recorded on a path, where they match its
    /// nodes one for one.
    fn apply_node_types(&self, element: &Element<'_>, path: &mut PathShape) {
        let Some(types) = self.names.draw(element, "nodes") else {
            return;
        };
        if types.chars().count() != path.node_count() {
            return;
        }
        let mut types = types.chars();
        for node in path
            .contours
            .iter_mut()
            .flat_map(|contour| contour.nodes.iter_mut())
        {
            node.join = match types.next() {
                Some('s') => NodeJoin::Smooth,
                Some('z') => NodeJoin::Symmetric,
                _ => NodeJoin::Corner,
            };
        }
    }

    /// A text object: its characters in lines, or along the one path a
    /// `<textPath>` holding all of them names; each span styled otherwise
    /// than the text a run.
    fn text_node(
        &mut self,
        element: &'a Element<'a>,
        style: &SvgStyle,
        own: Affine,
    ) -> Result<Option<Node>, ReadError> {
        let mut lines = TextLines::default();
        let origin = Point::new(
            first_coordinate(element, "x", self.viewport.0, &mut self.unkept)?,
            first_coordinate(element, "y", self.viewport.1, &mut self.unkept)?,
        );
        let preserve = element.attr("xml:space") == Some("preserve");
        lines.start(origin);
        let within = Within {
            base: style,
            preserve,
            rise: 0.0,
            on_path: false,
            style: 0,
        };
        self.gather_text(element, style, within, &mut lines)?;
        let Gathered {
            content,
            runs,
            on_path,
        } = lines.finish(preserve)?;
        let mut text = Text {
            content: SharedStr::try_from_string(content)?,
            family: SharedStr::try_from_str(
                style.font_family.as_deref().unwrap_or(DEFAULT_FAMILY),
            )?,
            size: style.font_size,
            weight: style.font_weight,
            italic: style.font_style != FontStyle::Normal,
            letter_spacing: style.letter_spacing,
            align: match style.text_anchor {
                TextAnchor::Start => TextAlign::Start,
                TextAnchor::Middle => TextAlign::Middle,
                TextAnchor::End => TextAlign::End,
            },
            baseline: Baseline::Lines {
                origin,
                line_height: DEFAULT_LINE_HEIGHT,
            },
            runs: SharedSlice::new(),
        };
        let paints = promote(&mut text, runs)?;
        let path = match (lines.path.take(), on_path) {
            (Some(path), OnPath::All) if !lines.paths_lost => Some(path),
            (path, on_path) => {
                if path.is_some() || lines.paths_lost || on_path != OnPath::None {
                    self.unkept.insert(Lost::TextPaths);
                }
                None
            }
        };
        text.baseline = match path {
            Some(path) => Baseline::Path(try_arc(path)?),
            None => Baseline::Lines {
                origin: lines.origin.unwrap_or(origin),
                line_height: self
                    .names
                    .draw(element, "line-height")
                    .and_then(|height| parse_number(height).ok())
                    .or_else(|| lines.measured_line_height(text.size))
                    .unwrap_or(DEFAULT_LINE_HEIGHT),
            },
        };
        let mut drawn = self.style(element, style, text.estimated_bounds())?;
        if let Some(fill) = paints.0 {
            drawn.fill = fill.paint();
        }
        if let Some(stroke) = paints.1 {
            drawn.stroke.paint = stroke.paint();
        }
        let node = Node::new(
            self.ids.issue(),
            Kind::Text {
                text: try_arc(text)?,
                style: try_arc(drawn)?,
            },
        )?;
        Ok(Some(node.with_transform(own)?))
    }

    /// Gather `element`'s characters into `lines`: a `<tspan>` placed at a
    /// height of its own beginning a line, and a `<textPath>` naming the path
    /// its characters are set along.
    fn gather_text(
        &mut self,
        element: &'a Element<'a>,
        style: &SvgStyle,
        within: Within<'_>,
        lines: &mut TextLines,
    ) -> Result<(), ReadError> {
        for content in &element.content {
            let child = match content {
                Content::Text(text) => {
                    lines.push(text, within)?;
                    continue;
                }
                Content::Element(child) if matches!(child.name, "tspan" | "textPath") => child,
                Content::Element(_) => continue,
            };
            self.visit()?;
            self.path.push(child);
            let child_style = self.style_of(&style.inherit(), child);
            self.path.pop();
            let child_style = child_style?;
            let shift = child_style.baseline_shift;
            let raised = shift.rise(style.font_size, child_style.font_size);
            let own_ems = match shift {
                BaselineShift::Ems(ems) => ems,
                _ => raised / child_style.font_size,
            };
            let mut inner = Within {
                preserve: within.preserve || child.attr("xml:space") == Some("preserve"),
                rise: within.rise + raised,
                ..within
            };
            if child.name == "textPath" {
                // SVG draws no `<textPath>` within another, nor one naming
                // nothing.
                if within.on_path {
                    continue;
                }
                match self.text_path(child, &child_style)? {
                    Along::Missing => continue,
                    Along::Unusable => lines.paths_lost = true,
                    Along::Path(path) => {
                        if lines.path.is_some() {
                            lines.paths_lost = true;
                        } else {
                            lines.path = Some(path);
                        }
                        inner.on_path = true;
                    }
                }
            } else if within.on_path {
                if ["x", "y", "dx", "dy"]
                    .iter()
                    .any(|name| child.attr(name).is_some())
                {
                    self.unkept.insert(Lost::TextPositioning);
                }
            } else if let Some(y) = child.attr("y") {
                let y = first_number(y, self.viewport.1, &mut self.unkept)?;
                let x = match child.attr("x") {
                    Some(x) => Some(first_number(x, self.viewport.0, &mut self.unkept)?),
                    None => None,
                };
                lines.place(x, y)?;
            } else if child.attr("dy").is_some()
                || child.attr("dx").is_some()
                || child.attr("x").is_some()
            {
                self.unkept.insert(Lost::TextPositioning);
            }
            let ems = within.rise / child_style.font_size + own_ems;
            let run = self.run_style(within.base, &child_style, ems, lines)?;
            inner.style = lines.style_index(run)?;
            self.deeper(|reader| reader.gather_text(child, &child_style, inner, lines))?;
        }
        Ok(())
    }

    /// What `style` overrides of the text's own `base`, its characters
    /// raised `ems` of its size; what no run can hold is noted as lost.
    #[allow(
        clippy::float_cmp,
        reason = "a span overrides a value only where it sets one other than its text's, exactly"
    )]
    fn run_style(
        &mut self,
        base: &SvgStyle,
        style: &SvgStyle,
        ems: f64,
        lines: &mut TextLines,
    ) -> Result<RunStyle, ReadError> {
        if style.stroke_style != base.stroke_style
            || style.font_stretch != base.font_stretch
            || style.word_spacing != base.word_spacing
        {
            self.unkept.insert(Lost::TextStyling);
        }
        let family = match &style.font_family {
            Some(family) if style.font_family != base.font_family => {
                let held = lines.family(family)?;
                if held.is_none() {
                    self.unkept.insert(Lost::TextStyling);
                }
                held
            }
            _ => None,
        };
        let italic = style.font_style != FontStyle::Normal;
        let baseline_shift = if ems == 0.0 {
            None
        } else if ems.is_finite() && fabs(ems) <= MAX_BASELINE_SHIFT {
            Some(ems)
        } else {
            self.unkept.insert(Lost::TextStyling);
            None
        };
        let fill = if paint_differs(
            (&base.fill, base.fill_opacity),
            (&style.fill, style.fill_opacity),
            base,
            style,
        ) {
            self.run_paint(&style.fill, style.fill_opacity, style)?
        } else {
            None
        };
        let stroke = if paint_differs(
            (&base.stroke, base.stroke_opacity),
            (&style.stroke, style.stroke_opacity),
            base,
            style,
        ) {
            self.run_paint(&style.stroke, style.stroke_opacity, style)?
        } else {
            None
        };
        // A size or spacing no text may hold is no run's either: it is lost,
        // not the whole document.
        let mut held = |differs: bool, value: f64, holds: bool| {
            if differs && !holds {
                self.unkept.insert(Lost::TextStyling);
            }
            (differs && holds).then_some(value)
        };
        let size = held(
            style.font_size != base.font_size,
            style.font_size,
            limits::length(style.font_size) && style.font_size > 0.0,
        );
        let letter_spacing = held(
            style.letter_spacing != base.letter_spacing,
            style.letter_spacing,
            limits::coordinate(style.letter_spacing),
        );
        Ok(RunStyle {
            family,
            size,
            weight: (style.font_weight != base.font_weight).then_some(style.font_weight),
            italic: (italic != (base.font_style != FontStyle::Normal)).then_some(italic),
            letter_spacing,
            baseline_shift,
            fill,
            stroke,
        })
    }

    /// The paint `spec` at `opacity` comes to as a run holds it; `None`, and
    /// the styling noted as lost, for a gradient, which paints a whole object.
    fn run_paint(
        &mut self,
        spec: &PaintSpec,
        opacity: f64,
        style: &SvgStyle,
    ) -> Result<Option<RunPaint>, ReadError> {
        if let PaintSpec::Reference(id, _) = spec {
            let gradient = self
                .servers
                .node(id)
                .is_some_and(|server| matches!(server.name, "linearGradient" | "radialGradient"));
            if gradient {
                self.unkept.insert(Lost::TextStyling);
                return Ok(None);
            }
        }
        Ok(RunPaint::of(&self.paint(
            spec,
            opacity,
            style,
            Bounds::EMPTY,
        )?))
    }

    /// The path `element`, a `<textPath>` set in `style`, names: in the
    /// text's own space, run backwards for `side="right"`, with how far along
    /// it its characters start.
    fn text_path(&mut self, element: &Element<'_>, style: &SvgStyle) -> Result<Along, ReadError> {
        let target = self.referenced(element);
        let geometry = textpath::geometry(element, target, self.viewport, MAX_TEXT_PATH_NODES);
        let (mut path, stated) = match geometry {
            Ok(Some(geometry)) => geometry,
            Ok(None) => return Ok(Along::Missing),
            Err(SvgError::OutOfMemory) => return Err(ReadError::OutOfMemory),
            Err(_) => {
                self.unkept.insert(Lost::TextPaths);
                return Ok(Along::Unusable);
            }
        };
        if element.attr("side") == Some("right") {
            path = path.reversed()?;
        }
        if element.attr("method") == Some("stretch") {
            self.unkept.insert(Lost::TextPaths);
        }
        let length = PathMeasure::new(&path)?.length();
        let start = textpath::start(element, style.font_size, length, stated, self.viewport)?;
        let shape = PathShape::from_path(&path)?;
        if shape.node_count() > MAX_TEXT_PATH_NODES || !limits::coordinate(start) {
            self.unkept.insert(Lost::TextPaths);
            return Ok(Along::Unusable);
        }
        Ok(Along::Path(TextPath {
            path: try_arc(shape)?,
            start,
        }))
    }

    /// The drawing style `style` comes to, gradients placed for a shape whose
    /// outline's bounds are `bounds`.
    fn style(
        &mut self,
        element: &Element<'_>,
        style: &SvgStyle,
        bounds: Bounds,
    ) -> Result<Style, ReadError> {
        if style.non_scaling_stroke {
            self.unkept.insert(Lost::NonScalingStrokes);
        }
        let fill = self.paint(&style.fill, style.fill_opacity, style, bounds)?;
        let stroke_paint = self.paint(&style.stroke, style.stroke_opacity, style, bounds)?;
        let stroke_style = &style.stroke_style;
        let join = match stroke_style.join {
            tairix_svg::geom::LineJoin::Round => Join::Round,
            tairix_svg::geom::LineJoin::Bevel => Join::Bevel,
            tairix_svg::geom::LineJoin::Miter | tairix_svg::geom::LineJoin::MiterOrRound => {
                Join::Miter
            }
        };
        let cap = match stroke_style.cap {
            tairix_svg::geom::LineCap::Butt => Cap::Butt,
            tairix_svg::geom::LineCap::Round => Cap::Round,
            tairix_svg::geom::LineCap::Square => Cap::Square,
        };
        let (start, end) = self.arrowheads(element, style);
        let fill_first = style
            .paint_order
            .slots()
            .iter()
            .position(|slot| *slot == PaintSlot::Fill)
            < style
                .paint_order
                .slots()
                .iter()
                .position(|slot| *slot == PaintSlot::Stroke);
        Ok(Style {
            fill,
            fill_rule: if self.clipping {
                style.clip_rule
            } else {
                style.fill_rule
            },
            stroke: Stroke {
                paint: stroke_paint,
                width: stroke_style.width,
                join,
                miter_limit: stroke_style.miter_limit.clamp(1.0, 1e4),
                cap,
                dashes: SharedSlice::try_from_slice(stroke_style.dashes.as_slice())?,
                dash_offset: stroke_style.dash_offset,
                start,
                end,
            },
            paint_order: if fill_first {
                PaintOrder::FillFirst
            } else {
                PaintOrder::StrokeFirst
            },
        })
    }

    /// The arrowheads Draw recorded on a shape; markers it did not are
    /// noted as lost.
    fn arrowheads(&mut self, element: &Element<'_>, style: &SvgStyle) -> (Arrowhead, Arrowhead) {
        let start = self.names.draw(element, "start-arrow").and_then(arrowhead);
        let end = self.names.draw(element, "end-arrow").and_then(arrowhead);
        let ours = |marker: &Option<String>, kept: bool| marker.is_none() || kept;
        if !ours(&style.marker_start, start.is_some())
            || !ours(&style.marker_end, end.is_some())
            || style.marker_mid.is_some()
        {
            self.unkept.insert(Lost::Markers);
        }
        (
            start.unwrap_or(Arrowhead::NONE),
            end.unwrap_or(Arrowhead::NONE),
        )
    }

    /// The paint `spec` comes to at `opacity`.
    fn paint(
        &mut self,
        spec: &PaintSpec,
        opacity: f64,
        style: &SvgStyle,
        bounds: Bounds,
    ) -> Result<Paint, ReadError> {
        Ok(match spec {
            PaintSpec::None => Paint::None,
            PaintSpec::Color(colour) => Paint::Colour(weakened(*colour, opacity)),
            PaintSpec::Current => Paint::Colour(weakened(style.color, opacity)),
            PaintSpec::Reference(id, fallback) => match self.servers.node(id) {
                Some(server) if matches!(server.name, "linearGradient" | "radialGradient") => {
                    let colour = self.definition_colour(server)?;
                    match self.servers.gradient_definition(
                        server,
                        self.viewport,
                        opacity,
                        colour,
                    )? {
                        Some(definition) => {
                            let placed = if definition.object_units {
                                [bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y]
                                    .map(f64::to_bits)
                            } else {
                                [0; 4]
                            };
                            let key = (
                                core::ptr::from_ref(server).addr(),
                                opacity.to_bits(),
                                u32::from_be_bytes([colour.r, colour.g, colour.b, colour.a]),
                                placed,
                            );
                            if let Some(shared) = self.gradients.get(&key) {
                                Paint::Gradient(shared.clone())
                            } else {
                                let paint = gradient_paint(&definition, bounds)?;
                                if let Paint::Gradient(gradient) = &paint {
                                    self.gradients.try_insert(key, gradient.clone())?;
                                }
                                paint
                            }
                        }
                        None => Paint::None,
                    }
                }
                Some(_) => {
                    self.unkept.insert(Lost::Patterns);
                    fallback.map_or(Paint::None, |colour| {
                        Paint::Colour(weakened(colour, opacity))
                    })
                }
                None => fallback.map_or(Paint::None, |colour| {
                    Paint::Colour(weakened(colour, opacity))
                }),
            },
        })
    }

    /// The `color` a referenced definition's own ancestry gives it, which a
    /// `currentColor` stop stands for.
    fn definition_colour(
        &mut self,
        node: &'a Element<'a>,
    ) -> Result<tairix_raster::Color, ReadError> {
        let mut chain = Vec::new();
        descend(self.root, node, &mut chain);
        let outer = core::mem::take(&mut self.path);
        let mut style = SvgStyle::default();
        let mut result = Ok(());
        for (index, element) in chain.iter().enumerate() {
            self.path.push(element);
            match self.style_of(&style, element) {
                Ok(resolved) => {
                    style = if index + 1 == chain.len() {
                        resolved
                    } else {
                        resolved.inherit()
                    };
                }
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        self.path = outer;
        result.map(|()| style.color)
    }

    /// The guides Draw recorded.
    fn guides(&self) -> Result<SharedSlice<Guide>, ReadError> {
        let mut guides = Vec::new();
        for element in self
            .root
            .children()
            .filter(|child| child.is(DRAW_NAMESPACE, "guides"))
        {
            for guide in element
                .children()
                .filter(|child| child.is(DRAW_NAMESPACE, "guide"))
            {
                if guides.len() >= MAX_GUIDES {
                    return Err(ReadError::TooComplex);
                }
                let axis = match guide.attr("axis") {
                    Some("horizontal") => GuideAxis::Horizontal,
                    Some("vertical") => GuideAxis::Vertical,
                    _ => continue,
                };
                let Some(position) = guide
                    .attr("position")
                    .and_then(|text| tairix_svg::number::parse_number(text).ok())
                else {
                    continue;
                };
                if !fallible::push(&mut guides, Guide { axis, position }) {
                    return Err(ReadError::OutOfMemory);
                }
            }
        }
        Ok(SharedSlice::try_from_vec(guides)?)
    }

    /// The swatches Draw kept with the drawing; one whose colour does not
    /// read is passed over, as a guide is.
    fn swatches(&self) -> Result<SharedSlice<Rgba>, ReadError> {
        let mut swatches = Vec::new();
        for element in self
            .root
            .children()
            .filter(|child| child.is(DRAW_NAMESPACE, "swatches"))
        {
            for swatch in element
                .children()
                .filter(|child| child.is(DRAW_NAMESPACE, "swatch"))
            {
                if swatches.len() >= MAX_SWATCHES {
                    return Err(ReadError::TooComplex);
                }
                let Some(colour) = swatch.attr("colour").and_then(hex_colour) else {
                    continue;
                };
                if !fallible::push(&mut swatches, colour) {
                    return Err(ReadError::OutOfMemory);
                }
            }
        }
        Ok(SharedSlice::try_from_vec(swatches)?)
    }

    /// The pixels to the inch Draw recorded the drawing is copied as raster
    /// at, or the drawing's own where it recorded none it may hold.
    fn resolution(&self) -> u32 {
        self.names
            .draw(self.root, "resolution")
            .and_then(|text| text.trim().parse::<u32>().ok())
            .filter(|resolution| (1..=MAX_RESOLUTION).contains(resolution))
            .unwrap_or(PIXELS_PER_INCH)
    }

    /// The grid Draw recorded, or the default.
    fn grid(&self) -> Result<Grid, ReadError> {
        let Some(text) = self.names.draw(self.root, "grid") else {
            return Ok(Grid::default());
        };
        let mut numbers = tairix_svg::number::Numbers::new(text);
        let (Some(spacing), Some(subdivisions)) = (numbers.take()?, numbers.take()?) else {
            return Ok(Grid::default());
        };
        let grid = Grid {
            spacing,
            subdivisions: whole_points(subdivisions.clamp(1.0, f64::from(MAX_SUBDIVISIONS))),
        };
        Ok(if spacing > 0.0 { grid } else { Grid::default() })
    }

    /// A group of `children`, clipped by `clip`, placed by `transform`.
    fn group_node(
        &mut self,
        children: Vec<Arc<Node>>,
        clip: Option<Arc<Node>>,
        transform: Affine,
    ) -> Result<Node, ReadError> {
        let node = Node::new(
            self.ids.issue(),
            Kind::Group {
                children: Children::from_nodes(children)?,
                clip,
            },
        )?;
        Ok(node.with_transform(transform)?)
    }

    fn shape_from(&mut self, shape: Shape, style: Style) -> Result<Node, ReadError> {
        Ok(Node::new(
            self.ids.issue(),
            Kind::Shape {
                shape,
                style: try_arc(style)?,
            },
        )?)
    }
}

/// What the characters an element holds are gathered with.
#[derive(Copy, Clone)]
struct Within<'s> {
    /// The text's own style, which a run's overrides are told from.
    base: &'s SvgStyle,
    preserve: bool,
    /// How far the enclosing spans raise their characters, in user units.
    rise: f64,
    /// Whether a `<textPath>` holds them.
    on_path: bool,
    /// The style they are set in, as [`Letter::style`] counts it.
    style: u32,
}

/// One gathered character.
#[derive(Copy, Clone)]
struct Letter {
    character: char,
    /// The style it is set in: an index from one into the styles gathered,
    /// zero for the text's own.
    style: u32,
    /// Whether a `<textPath>` holds it.
    on_path: bool,
}

/// What a `<textPath>` sets its characters along.
enum Along {
    /// Nothing: SVG draws none of its characters.
    Missing,
    /// A path past what a text holds, or broken, so its characters are set
    /// in lines.
    Unusable,
    /// A path, in the text's own space.
    Path(TextPath),
}

/// How many of a text's characters a `<textPath>` holds.
#[derive(Copy, Clone, Eq, PartialEq)]
enum OnPath {
    None,
    Some,
    All,
}

/// A text's content as gathered: its white space collapsed, its runs, and
/// how many of its characters a `<textPath>` holds.
struct Gathered {
    content: String,
    runs: Vec<Run>,
    on_path: OnPath,
}

/// The lines of a text object as its characters are gathered.
#[derive(Default)]
struct TextLines {
    lines: Vec<Vec<Letter>>,
    /// Each line's baseline height, where a `<tspan>` stated one.
    heights: Vec<f64>,
    origin: Option<Point>,
    /// The bytes gathered, which the content's bound holds.
    bytes: usize,
    /// The styles spans set otherwise than the text's own.
    styles: Vec<Arc<RunStyle>>,
    /// The families those styles name.
    families: Vec<SharedStr>,
    /// The path the first `<textPath>` names.
    path: Option<TextPath>,
    /// Whether a `<textPath>` named what a text cannot be set along, or a
    /// second path.
    paths_lost: bool,
}

impl TextLines {
    fn start(&mut self, origin: Point) {
        self.lines.push(Vec::new());
        self.heights.push(origin.y);
    }

    /// Begin a line at baseline `y`, unless the current one is still empty.
    fn place(&mut self, x: Option<f64>, y: f64) -> Result<(), ReadError> {
        let current_empty = self
            .lines
            .last()
            .is_none_or(|line| line.iter().all(|letter| letter.character == ' '));
        if current_empty {
            if let (Some(line), Some(height)) = (self.lines.last_mut(), self.heights.last_mut()) {
                line.clear();
                *height = y;
            }
        } else {
            try_push(&mut self.lines, Vec::new())?;
            try_push(&mut self.heights, y)?;
        }
        if self.lines.len() == 1 {
            self.origin = Some(Point::new(x.unwrap_or(0.0), y));
        }
        Ok(())
    }

    fn push(&mut self, text: &str, within: Within<'_>) -> Result<(), ReadError> {
        let Some(line) = self.lines.last_mut() else {
            return Ok(());
        };
        for c in text.chars() {
            let character = match c {
                '\n' | '\r' if !within.preserve => continue,
                '\n' | '\r' | '\t' => ' ',
                c if c.is_control() => continue,
                c => c,
            };
            self.bytes += character.len_utf8();
            if self.bytes > crate::limits::MAX_TEXT_BYTES {
                return Err(ReadError::TooComplex);
            }
            let letter = Letter {
                character,
                style: within.style,
                on_path: within.on_path,
            };
            try_push(line, letter)?;
        }
        Ok(())
    }

    /// The index characters set in `style` carry: zero for one overriding
    /// nothing.
    fn style_index(&mut self, style: RunStyle) -> Result<u32, ReadError> {
        if style.is_empty() {
            return Ok(0);
        }
        try_push(&mut self.styles, try_arc(style)?)?;
        u32::try_from(self.styles.len()).map_err(|_| ReadError::TooComplex)
    }

    /// `family` as a run may name it, or `None` where it is past a family's
    /// bound or one family too many for one text.
    fn family(&mut self, family: &str) -> Result<Option<SharedStr>, ReadError> {
        if let Some(held) = self.families.iter().find(|held| held.as_str() == family) {
            return Ok(Some(held.clone()));
        }
        if family.len() > MAX_FAMILY_BYTES
            || family.chars().any(char::is_control)
            || self.families.len() == MAX_TEXT_FAMILIES
        {
            return Ok(None);
        }
        let held = SharedStr::try_from_str(family)?;
        try_push(&mut self.families, held.clone())?;
        Ok(Some(held))
    }

    /// The text, its lines' white space collapsed unless preserved, with the
    /// runs its spans make.
    fn finish(&self, preserve: bool) -> Result<Gathered, ReadError> {
        let mut content = String::new();
        // Every byte gathered and a line feed between each line and the next,
        // so nothing below grows it.
        content
            .try_reserve(self.bytes + self.lines.len())
            .map_err(|_| ReadError::OutOfMemory)?;
        let mut spans: Vec<(Range<usize>, u32)> = Vec::new();
        let (mut on, mut off) = (false, false);
        for (index, line) in self.lines.iter().enumerate() {
            if index > 0 {
                content.push('\n');
            }
            let kept = if preserve {
                line.as_slice()
            } else {
                let first = line.iter().position(|letter| letter.character != ' ');
                let last = line.iter().rposition(|letter| letter.character != ' ');
                match (first, last) {
                    (Some(first), Some(last)) => &line[first..=last],
                    _ => &[],
                }
            };
            let mut spaced = false;
            for letter in kept {
                let space = letter.character == ' ';
                if space && spaced && !preserve {
                    continue;
                }
                spaced = space;
                on |= letter.on_path;
                off |= !letter.on_path;
                let at = content.len();
                content.push(letter.character);
                if letter.style == 0 {
                    continue;
                }
                match spans.last_mut() {
                    Some((span, style)) if *style == letter.style && span.end == at => {
                        span.end = content.len();
                    }
                    _ => try_push(&mut spans, (at..content.len(), letter.style))?,
                }
            }
        }
        if content.len() > crate::limits::MAX_TEXT_BYTES {
            return Err(ReadError::TooComplex);
        }
        let mut runs = Vec::new();
        if !fallible::reserve(&mut runs, spans.len()) {
            return Err(ReadError::OutOfMemory);
        }
        for (span, style) in spans {
            let style = self
                .styles
                .get(style as usize - 1)
                .ok_or(ReadError::Malformed)?;
            runs.push(Run {
                start: span.start,
                end: span.end,
                style: Arc::clone(style),
            });
        }
        Ok(Gathered {
            content,
            runs,
            on_path: match (on, off) {
                (true, false) => OnPath::All,
                (true, true) => OnPath::Some,
                (false, _) => OnPath::None,
            },
        })
    }

    /// The line height the `<tspan>` baselines were set at, in ems.
    fn measured_line_height(&self, size: f64) -> Option<f64> {
        let pair = self.heights.get(0..2)?;
        let step = pair[1] - pair[0];
        (size > 0.0 && step > 0.0).then(|| step / size)
    }
}

/// Give `text` its runs, a run over the whole content folded into the text's
/// own style; what that run paints its glyphs with, which the object's style
/// takes.
fn promote(
    text: &mut Text,
    mut runs: Vec<Run>,
) -> Result<(Option<RunPaint>, Option<RunPaint>), ReadError> {
    let whole = match runs.as_slice() {
        [run] if run.start == 0 && run.end == text.content.len() => Some(Arc::clone(&run.style)),
        _ => None,
    };
    let mut paints = (None, None);
    if let Some(style) = whole {
        if let Some(family) = &style.family {
            text.family = family.clone();
        }
        text.size = style.size.unwrap_or(text.size);
        text.weight = style.weight.unwrap_or(text.weight);
        text.italic = style.italic.unwrap_or(text.italic);
        text.letter_spacing = style.letter_spacing.unwrap_or(text.letter_spacing);
        paints = (style.fill, style.stroke);
        let rest = RunStyle {
            baseline_shift: style.baseline_shift,
            ..RunStyle::default()
        };
        runs.clear();
        if !rest.is_empty() {
            runs.push(Run {
                start: 0,
                end: text.content.len(),
                style: try_arc(rest)?,
            });
        }
    }
    text.runs = canonical(runs)?;
    Ok(paints)
}

/// Whether a span's paint — `spec` at its opacity, `currentColor` in its own
/// colour — differs from the text's own.
#[allow(
    clippy::float_cmp,
    reason = "a span overrides a paint only where its opacity differs from its text's, exactly"
)]
fn paint_differs(
    base: (&PaintSpec, f64),
    span: (&PaintSpec, f64),
    base_style: &SvgStyle,
    span_style: &SvgStyle,
) -> bool {
    base.0 != span.0
        || base.1 != span.1
        || (matches!(span.0, PaintSpec::Current) && base_style.color != span_style.color)
}

/// The drawing paint a gradient definition comes to for a shape whose
/// outline's bounds are `bounds`: in the shape's own space.
fn gradient_paint(definition: &GradientDefinition, bounds: Bounds) -> Result<Paint, ReadError> {
    let to_object = if definition.object_units {
        if !(bounds.width() > 0.0 && bounds.height() > 0.0) {
            return Ok(Paint::None);
        }
        definition.transform.then(
            Affine::scale(bounds.width(), bounds.height())
                .then(Affine::translate(bounds.min.x, bounds.min.y)),
        )
    } else {
        definition.transform
    };
    let kind = match definition.geometry {
        GradientGeometry::Linear { start, end } => GradientKind::Linear {
            start: start.into(),
            end: end.into(),
        },
        GradientGeometry::Radial {
            centre,
            radius,
            focus,
        } => GradientKind::Radial {
            centre: centre.into(),
            radius: fmax(radius, 0.0),
            focus: focus.into(),
        },
    };
    let mut stops = Vec::new();
    if !fallible::reserve(&mut stops, definition.stops.len()) {
        return Err(ReadError::OutOfMemory);
    }
    stops.extend(definition.stops.iter().map(|stop| Stop {
        offset: stop.offset.clamp(0.0, 1.0),
        colour: Rgba::new(stop.color.r, stop.color.g, stop.color.b, stop.color.a),
    }));
    let spread = match definition.spread {
        tairix_raster::SpreadMethod::Pad => Spread::Pad,
        tairix_raster::SpreadMethod::Reflect => Spread::Reflect,
        tairix_raster::SpreadMethod::Repeat => Spread::Repeat,
    };
    let gradient = Gradient {
        kind,
        stops,
        spread,
        transform: to_object,
    };
    gradient.check()?;
    Ok(Paint::Gradient(try_arc(gradient)?))
}

/// `colour` with its alpha weakened by `opacity`.
fn weakened(colour: tairix_raster::Color, opacity: f64) -> Rgba {
    let alpha = tairix_svg::style::scale_alpha(colour, opacity).map_or(0, |scaled| scaled.a);
    Rgba::new(colour.r, colour.g, colour.b, alpha)
}

/// The arrowhead Draw spelled as `shape width length`.
fn arrowhead(text: &str) -> Option<Arrowhead> {
    let mut words = text.split_whitespace();
    let shape = match words.next()? {
        "triangle" => ArrowShape::Triangle,
        "arrow" => ArrowShape::Arrow,
        "circle" => ArrowShape::Circle,
        "square" => ArrowShape::Square,
        "diamond" => ArrowShape::Diamond,
        "bar" => ArrowShape::Bar,
        _ => return None,
    };
    let width = tairix_svg::number::parse_number(words.next()?).ok()?;
    let length = tairix_svg::number::parse_number(words.next()?).ok()?;
    let head = Arrowhead {
        shape,
        width,
        length,
    };
    (width.is_finite()
        && length.is_finite()
        && (0.0..=100.0).contains(&width)
        && (0.0..=100.0).contains(&length))
    .then_some(head)
}

/// The title and description `root` gives the drawing — its first `<title>`
/// and `<desc>` — and those elements' addresses.
fn info_of(root: &Element<'_>) -> Result<(Info, [Option<usize>; 2]), ReadError> {
    let mut info = Info::default();
    let mut used = [None; 2];
    for (slot, (name, one_line)) in [("title", true), ("desc", false)].into_iter().enumerate() {
        let Some(element) = root.children().find(|child| child.name == name) else {
            continue;
        };
        used[slot] = Some(core::ptr::from_ref(element).addr());
        let raw = fallible::written(|out| {
            element
                .content
                .iter()
                .try_for_each(|content| match content {
                    Content::Text(text) => out.write_str(text),
                    Content::Element(_) => Ok(()),
                })
        })
        .ok_or(ReadError::OutOfMemory)?;
        // A title is one line, its spaces collapsed; a description keeps its
        // lines. Other control characters are not text.
        let text = fallible::written(|out| {
            let mut space = false;
            for c in raw.chars() {
                if one_line && c.is_whitespace() {
                    space = true;
                } else if !c.is_control() || matches!(c, '\n' | '\t') {
                    if core::mem::take(&mut space) {
                        out.write_char(' ')?;
                    }
                    out.write_char(c)?;
                }
            }
            Ok(())
        })
        .ok_or(ReadError::OutOfMemory)?;
        let text = text.trim();
        if text.len() > crate::limits::MAX_TEXT_BYTES {
            return Err(ReadError::TooComplex);
        }
        let held = if text.is_empty() {
            None
        } else {
            Some(SharedStr::try_from_str(text)?)
        };
        if one_line {
            info.title = held;
        } else {
            info.description = held;
        }
    }
    Ok((info, used))
}

/// The page `root` states, the map from its user space onto the page, and
/// the viewport its percentages measure.
fn page_of(root: &Element<'_>, names: &Names<'_>) -> Result<(Page, Affine, (f64, f64)), ReadError> {
    let view_box = root.attr("viewBox").map(parse_view_box).transpose()?;
    let width = root.attr("width").map(page_extent).transpose()?.flatten();
    let height = root.attr("height").map(page_extent).transpose()?.flatten();
    let mut page = if let (None, None, None) = (width, height, view_box) {
        Page::of(&PAPER[0])
    } else {
        let unit = width.or(height).map_or(Unit::Px, |(_, unit)| unit);
        let (box_width, box_height) = view_box.map_or((0.0, 0.0), |view_box| view_box.size);
        let in_unit = |extent: Option<(f64, Unit)>, fallback: f64| match extent {
            Some((value, stated)) if stated == unit => value,
            Some((value, stated)) => value * stated.pixels() / unit.pixels(),
            None => fallback / unit.pixels(),
        };
        Page {
            width: in_unit(width, box_width),
            height: in_unit(height, box_height),
            unit,
            background: None,
        }
    };
    page.background = names.draw(root, "background").and_then(hex_colour);
    let size = page.size();
    if !(size.x > 0.0 && size.y > 0.0) {
        return Err(ReadError::Malformed);
    }
    Ok(match view_box {
        Some(view_box) if view_box.size.0 > 0.0 && view_box.size.1 > 0.0 => {
            let ratio = match root.attr("preserveAspectRatio") {
                Some(text) => parse_aspect_ratio(text)?,
                None => AspectRatio::default(),
            };
            (
                page,
                viewport_transform(view_box, (size.x, size.y), ratio),
                view_box.size,
            )
        }
        _ => (page, Affine::IDENTITY, (size.x, size.y)),
    })
}

/// A root `width` or `height`: its number and the unit it is stated in, or
/// nothing for a percentage, which states no size of its own. A unit the page
/// cannot be held in is read as the pixels it measures.
fn page_extent(text: &str) -> Result<Option<(f64, Unit)>, ReadError> {
    let text = text.trim();
    if text.ends_with('%') {
        return Ok(None);
    }
    let (number, symbol) = text.split_at(
        text.trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .len(),
    );
    let unit = if symbol.is_empty() {
        Some(Unit::Px)
    } else {
        Unit::from_symbol(symbol)
    };
    Ok(Some(match unit {
        Some(unit) => (parse_number(number)?, unit),
        None => (parse_length(text, 0.0)?, Unit::Px),
    }))
}

/// The first of a coordinate list, noting a list of more as positions lost.
fn first_number(text: &str, basis: f64, unkept: &mut Unkept) -> Result<f64, ReadError> {
    let mut parts = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty());
    let first = parts
        .next()
        .map_or(Ok(0.0), |part| parse_length(part, basis))?;
    if parts.next().is_some() {
        unkept.insert(Lost::TextPositioning);
    }
    Ok(first)
}

/// `element`'s coordinate `name`, the first of its list, zero when absent.
fn first_coordinate(
    element: &Element<'_>,
    name: &str,
    basis: f64,
    unkept: &mut Unkept,
) -> Result<f64, ReadError> {
    match element.attr(name) {
        Some(text) => first_number(text, basis, unkept),
        None => Ok(0.0),
    }
}

/// Whether `name` is an element the walk reads; definitions, metadata and
/// what the drawing cannot keep are not.
fn is_drawable(name: &str) -> bool {
    matches!(
        name,
        "g" | "a"
            | "switch"
            | "use"
            | "svg"
            | "text"
            | "image"
            | "path"
            | "rect"
            | "circle"
            | "ellipse"
            | "line"
            | "polyline"
            | "polygon"
    )
}

/// `element`'s own `transform`.
fn transform_of(element: &Element<'_>) -> Result<Affine, ReadError> {
    match element.attr("transform") {
        Some(text) => Ok(parse_transform(text)?),
        None => Ok(Affine::IDENTITY),
    }
}

fn optional_length(element: &Element<'_>, name: &str, basis: f64) -> Result<f64, ReadError> {
    Ok(optional_attr_length(element, name, basis)?.unwrap_or(0.0))
}

fn optional_attr_length(
    element: &Element<'_>,
    name: &str,
    basis: f64,
) -> Result<Option<f64>, ReadError> {
    match element.attr(name) {
        Some(text) => Ok(Some(parse_length(text, basis)?)),
        None => Ok(None),
    }
}

/// A `#rrggbb` colour.
fn hex_colour(text: &str) -> Option<Rgba> {
    let (colour, _) = tairix_colour::parse_hex(text.trim().strip_prefix('#')?)?;
    Some(colour)
}

/// The chain of elements from `root` down to `target`, pushed onto `chain`.
fn descend<'a>(
    root: &'a Element<'a>,
    target: &'a Element<'a>,
    chain: &mut Vec<&'a Element<'a>>,
) -> bool {
    chain.push(root);
    if core::ptr::eq(root, target) {
        return true;
    }
    for child in root.children() {
        if descend(child, target, chain) {
            return true;
        }
    }
    chain.pop();
    false
}

/// A count read as a float, held to the whole numbers it stands for.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "every caller holds the value to a small positive range first"
)]
fn whole_points(value: f64) -> u32 {
    tairix_util::mathf::round(value) as u32
}
