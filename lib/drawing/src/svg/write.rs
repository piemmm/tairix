//! Writing a drawing as SVG.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use core::fmt::{self, Write};
use core::ops::Range;

use tairix_bezier::{Affine, FillRule, Path, PathEl, Point, Subpath};
use tairix_colour::Rgba;
use tairix_util::fallible;
use tairix_xml::Escaped;

use super::data_url;
use super::number::{alpha, Number};
use super::{DRAW_NAMESPACE, INKSCAPE_NAMESPACE};
use crate::drawing::{Drawing, GuideAxis, PIXELS_PER_INCH};
use crate::image::Image;
use crate::node::{Kind, Node};
use crate::numbering::Numbered;
use crate::shape::{ArcKind, NodeJoin, Shape};
use crate::style::{
    ArrowShape, Arrowhead, Cap, Gradient, GradientKind, Join, Paint, PaintOrder, Spread, Style,
};
use crate::text::{Baseline, RunPaint, RunStyle, Text, TextAlign, TextPath};
use crate::DrawingError;

/// `drawing` as an SVG document.
///
/// # Errors
///
/// [`DrawingError::OutOfMemory`] when the document cannot be held.
pub fn write(drawing: &Drawing) -> Result<String, DrawingError> {
    let plan = Plan::of(drawing)?;
    fallible::written(|out| plan.write(out)).ok_or(DrawingError::OutOfMemory)
}

/// Every definition the document's objects refer to, found before anything
/// is written so the two passes of the writer agree on every name, and every
/// picture no web format can carry as it came, encoded once.
struct Plan<'d> {
    drawing: &'d Drawing,
    gradients: Numbered<usize, Arc<Gradient>>,
    clips: Numbered<usize, Arc<Node>>,
    markers: Numbered<MarkerKey, Marker>,
    /// Each such picture's pixels as a PNG, by its pixels' address.
    encoded: Numbered<usize, Vec<u8>>,
    /// The paths texts are set along, by address.
    text_paths: Numbered<usize, Arc<TextPath>>,
    /// The pictures more than one object shows, each written once, in the
    /// order first shown.
    shared: Vec<Arc<Image>>,
    /// The number of the picture in `shared` each of them is, by key.
    shared_by_key: Vec<(PictureKey, usize)>,
    images: bool,
}

/// What tells pictures apart: the addresses of their pixels and of the
/// bytes they were read from.
type PictureKey = (usize, usize);

/// The pictures written once for every object showing them, and the number
/// each key takes among them, sorted by key.
type SharedPictures = (Vec<Arc<Image>>, Vec<(PictureKey, usize)>);

fn picture_key(image: &Image) -> PictureKey {
    let source = image
        .source()
        .map_or(0, |source| source.bytes.as_ptr().addr());
    (Arc::as_ptr(image.pyramid()).addr(), source)
}

/// The namespace SVG 1.1 links a picture and a text's path through,
/// `XLink`'s.
const XLINK_NAMESPACE: &str = "http://www.w3.org/1999/xlink";

/// A clip path still to write, called for by a member of a clip that a group
/// within the clip clips: that group's clip, placed in the member's space.
#[derive(Clone)]
struct Pending {
    id: usize,
    node: Arc<Node>,
    /// The clip's parent's space onto the member's.
    outer: Affine,
    /// The clip path the next group out within the clip makes, in the same
    /// space.
    then: Option<usize>,
}

/// The clips of the groups within a clip around one of its members,
/// innermost first, each with the map from its group's members' space onto
/// the clip path's.
#[derive(Copy, Clone)]
struct Within<'l> {
    clip: &'l Arc<Node>,
    members: Affine,
    outer: Option<&'l Within<'l>>,
}

/// The name the definition `key` stands for has in the document: its number,
/// counted from one.
fn name<K: Ord + Copy, T>(numbered: &Numbered<K, T>, key: K) -> Result<usize, fmt::Error> {
    numbered
        .number(key)
        .map(|number| number + 1)
        .ok_or(fmt::Error)
}

/// One arrowhead's marker: its shape, its size, its colour and which end it
/// is drawn at.
#[derive(Copy, Clone)]
struct Marker {
    head: Arrowhead,
    colour: Rgba,
    at_start: bool,
}

/// What tells two markers apart, in a total order.
type MarkerKey = (u8, u64, u64, u32, bool);

impl Marker {
    fn key(&self) -> MarkerKey {
        let Rgba { r, g, b, a } = self.colour;
        (
            self.head.shape as u8,
            self.head.width.to_bits(),
            self.head.length.to_bits(),
            u32::from_be_bytes([r, g, b, a]),
            self.at_start,
        )
    }
}

impl<'d> Plan<'d> {
    fn of(drawing: &'d Drawing) -> Result<Self, DrawingError> {
        let mut gradients = Vec::new();
        let mut clips = Vec::new();
        let mut markers = Vec::new();
        let mut text_paths = Vec::new();
        let mut shown = Vec::new();
        let mut unembeddable = Vec::new();
        let mut images = false;
        let mut failed = false;
        visit(drawing, &mut |node: &Node, within_clip: bool| {
            if failed {
                return;
            }
            // A group within a clip is written as its members, each calling
            // for its clip by a path of its own.
            if let (
                Kind::Group {
                    clip: Some(clip), ..
                },
                false,
            ) = (node.kind(), within_clip)
            {
                failed |= !fallible::push(&mut clips, (Arc::as_ptr(clip).addr(), clip.clone()));
            }
            // A picture within a clip is written as its frame, so it needs no
            // link and no encoding.
            if let (Kind::Image { image }, false) = (node.kind(), within_clip) {
                images = true;
                failed |= !fallible::push(&mut shown, (picture_key(image), image.clone()));
                if embedded(image).is_none() {
                    failed |= !fallible::push(
                        &mut unembeddable,
                        (Arc::as_ptr(image.pyramid()).addr(), image.clone()),
                    );
                }
            }
            if let Kind::Text { text, .. } = node.kind() {
                if let Baseline::Path(along) = &text.baseline {
                    failed |= !fallible::push(
                        &mut text_paths,
                        (Arc::as_ptr(along).addr(), along.clone()),
                    );
                }
            }
            let Some(style) = node.style() else {
                return;
            };
            for paint in [&style.fill, &style.stroke.paint] {
                if let Paint::Gradient(gradient) = paint {
                    failed |= !fallible::push(
                        &mut gradients,
                        (Arc::as_ptr(gradient).addr(), gradient.clone()),
                    );
                }
            }
            for (head, at_start) in [(style.stroke.start, true), (style.stroke.end, false)] {
                if style.stroke.draws_head(&head) {
                    let marker = Marker {
                        head,
                        colour: style.stroke.head_colour(),
                        at_start,
                    };
                    failed |= !fallible::push(&mut markers, (marker.key(), marker));
                }
            }
        });
        if failed {
            return Err(DrawingError::OutOfMemory);
        }
        let pictures = Numbered::of(unembeddable)?;
        let mut encoded = Vec::new();
        if !fallible::reserve(&mut encoded, pictures.items().len()) {
            return Err(DrawingError::OutOfMemory);
        }
        for image in pictures.items() {
            let png = tairix_image::encode_png(&tairix_image::SurfacePicture::new(image.pixels()))
                .map_err(|error| match error {
                    tairix_image::EncodeError::OutOfMemory => DrawingError::OutOfMemory,
                    _ => DrawingError::TooComplex,
                })?;
            encoded.push((Arc::as_ptr(image.pyramid()).addr(), png));
        }
        let encoded = Numbered::of(encoded)?;
        let (shared, shared_by_key) = shared_pictures(shown, &encoded)?;
        Ok(Self {
            drawing,
            gradients: Numbered::of(gradients)?,
            clips: Numbered::of(clips)?,
            markers: Numbered::of(markers)?,
            encoded,
            text_paths: Numbered::of(text_paths)?,
            shared,
            shared_by_key,
            images,
        })
    }

    fn gradient_id(&self, gradient: &Arc<Gradient>) -> Result<usize, fmt::Error> {
        name(&self.gradients, Arc::as_ptr(gradient).addr())
    }

    fn clip_id(&self, clip: &Arc<Node>) -> Result<usize, fmt::Error> {
        name(&self.clips, Arc::as_ptr(clip).addr())
    }

    fn text_path_id(&self, along: &Arc<TextPath>) -> Result<usize, fmt::Error> {
        name(&self.text_paths, Arc::as_ptr(along).addr())
    }

    /// The name of the picture `image` shows, where more than one object
    /// shows it.
    fn shared_id(&self, image: &Image) -> Option<usize> {
        let key = picture_key(image);
        let at = self.shared_by_key.partition_point(|(seen, _)| *seen < key);
        self.shared_by_key
            .get(at)
            .filter(|(seen, _)| *seen == key)
            .map(|(_, number)| number + 1)
    }

    fn write(&self, out: &mut dyn Write) -> fmt::Result {
        let page = self.drawing.page();
        let unit = page.unit.symbol();
        let size = page.size();
        // Spelled exactly, so the page reads back as itself and its map onto
        // the user space as the identity.
        write!(
            out,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" \
             xmlns:draw=\"{DRAW_NAMESPACE}\" xmlns:inkscape=\"{INKSCAPE_NAMESPACE}\""
        )?;
        if self.images || !self.text_paths.items().is_empty() {
            write!(out, " xmlns:xlink=\"{XLINK_NAMESPACE}\"")?;
        }
        write!(
            out,
            " version=\"1.1\" width=\"{}{unit}\" height=\"{}{unit}\" viewBox=\"0 0 {} {}\"",
            page.width, page.height, size.x, size.y,
        )?;
        let grid = self.drawing.grid();
        write!(
            out,
            " draw:grid=\"{} {}\"",
            Number(grid.spacing),
            grid.subdivisions
        )?;
        if let Some(background) = page.background {
            write!(out, " draw:background=\"{}\"", background.hex().hashed())?;
        }
        let resolution = self.drawing.resolution();
        if resolution != PIXELS_PER_INCH {
            write!(out, " draw:resolution=\"{resolution}\"")?;
        }
        out.write_str(">\n")?;
        let info = self.drawing.info();
        if let Some(title) = &info.title {
            writeln!(out, "<title>{}</title>", Escaped(title))?;
        }
        if let Some(description) = &info.description {
            writeln!(out, "<desc>{}</desc>", Escaped(description))?;
        }
        let guides = self.drawing.guides();
        if !guides.is_empty() {
            out.write_str("<draw:guides>\n")?;
            for guide in guides {
                let axis = match guide.axis {
                    GuideAxis::Horizontal => "horizontal",
                    GuideAxis::Vertical => "vertical",
                };
                writeln!(
                    out,
                    "<draw:guide axis=\"{axis}\" position=\"{}\"/>",
                    Number(guide.position)
                )?;
            }
            out.write_str("</draw:guides>\n")?;
        }
        let swatches = self.drawing.swatches();
        if !swatches.is_empty() {
            out.write_str("<draw:swatches>\n")?;
            for swatch in swatches {
                writeln!(out, "<draw:swatch colour=\"{}\"/>", swatch.hex().hashed())?;
            }
            out.write_str("</draw:swatches>\n")?;
        }
        if !(self.gradients.items().is_empty()
            && self.clips.items().is_empty()
            && self.markers.items().is_empty()
            && self.text_paths.items().is_empty()
            && self.shared.is_empty())
        {
            out.write_str("<defs>\n")?;
            self.write_gradients(out)?;
            self.write_markers(out)?;
            self.write_text_paths(out)?;
            self.write_shared_pictures(out)?;
            self.write_clips(out)?;
            out.write_str("</defs>\n")?;
        }
        if let Some(background) = page.background {
            write!(
                out,
                "<rect draw:role=\"background\" x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" fill=\"{}\"",
                size.x,
                size.y,
                background.without_alpha().hex().hashed(),
            )?;
            if !background.is_opaque() {
                write!(out, " fill-opacity=\"{}\"", alpha(background.a))?;
            }
            out.write_str("/>\n")?;
        }
        for layer in self.drawing.layers().iter() {
            out.write_str("<g draw:layer=\"true\" inkscape:groupmode=\"layer\"")?;
            if let Some(name) = layer.name() {
                write!(out, " inkscape:label=\"{}\"", Escaped(name))?;
            }
            self.write_group(out, layer)?;
        }
        out.write_str("</svg>\n")
    }

    fn write_gradients(&self, out: &mut dyn Write) -> fmt::Result {
        for (index, gradient) in self.gradients.items().iter().enumerate() {
            let tag = match gradient.kind {
                GradientKind::Linear { .. } => "linearGradient",
                GradientKind::Radial { .. } => "radialGradient",
            };
            write!(
                out,
                "<{tag} id=\"gradient-{}\" gradientUnits=\"userSpaceOnUse\" ",
                index + 1
            )?;
            match gradient.kind {
                GradientKind::Linear { start, end } => write!(
                    out,
                    "x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"",
                    Number(start.x),
                    Number(start.y),
                    Number(end.x),
                    Number(end.y)
                )?,
                GradientKind::Radial {
                    centre,
                    radius,
                    focus,
                } => write!(
                    out,
                    "cx=\"{}\" cy=\"{}\" r=\"{}\" fx=\"{}\" fy=\"{}\"",
                    Number(centre.x),
                    Number(centre.y),
                    Number(radius),
                    Number(focus.x),
                    Number(focus.y)
                )?,
            }
            if gradient.transform != Affine::IDENTITY {
                write!(out, " gradientTransform=\"{}\"", Matrix(gradient.transform))?;
            }
            match gradient.spread {
                Spread::Pad => {}
                Spread::Reflect => out.write_str(" spreadMethod=\"reflect\"")?,
                Spread::Repeat => out.write_str(" spreadMethod=\"repeat\"")?,
            }
            out.write_str(">\n")?;
            for stop in &gradient.stops {
                write!(
                    out,
                    "<stop offset=\"{}\" stop-color=\"{}\"",
                    Number(stop.offset),
                    stop.colour.without_alpha().hex().hashed()
                )?;
                if stop.colour.a < u8::MAX {
                    write!(out, " stop-opacity=\"{}\"", alpha(stop.colour.a))?;
                }
                out.write_str("/>\n")?;
            }
            writeln!(out, "</{tag}>")?;
        }
        Ok(())
    }

    fn write_markers(&self, out: &mut dyn Write) -> fmt::Result {
        for (index, marker) in self.markers.items().iter().enumerate() {
            // Drawn in stroke widths, pointing along the path's end; a start
            // marker is mirrored so a viewer without SVG 2's
            // `auto-start-reverse` still points it outwards.
            let direction = if marker.at_start {
                Point::new(-1.0, 0.0)
            } else {
                Point::new(1.0, 0.0)
            };
            let outline = marker
                .head
                .outline(Point::ZERO, direction, 1.0)
                .map_err(|_| fmt::Error)?;
            let Some(outline) = outline else {
                continue;
            };
            write!(
                out,
                "<marker id=\"marker-{}\" draw:marker=\"true\" markerUnits=\"strokeWidth\" orient=\"auto\" overflow=\"visible\" refX=\"0\" refY=\"0\" markerWidth=\"1\" markerHeight=\"1\"><path d=\"",
                index + 1
            )?;
            write_subpath_data(out, &outline)?;
            write!(
                out,
                "\" fill=\"{}\"",
                marker.colour.without_alpha().hex().hashed()
            )?;
            if marker.colour.a < u8::MAX {
                write!(out, " fill-opacity=\"{}\"", alpha(marker.colour.a))?;
            }
            out.write_str("/></marker>\n")?;
        }
        Ok(())
    }

    /// Every path a text is set along, in its text's own space.
    fn write_text_paths(&self, out: &mut dyn Write) -> fmt::Result {
        for (index, along) in self.text_paths.items().iter().enumerate() {
            let path = along.path.to_path().map_err(|_| fmt::Error)?;
            write!(out, "<path id=\"text-path-{}\" d=\"", index + 1)?;
            write_path_data(out, &path)?;
            out.write_str("\"/>\n")?;
        }
        Ok(())
    }

    /// Every picture more than one object shows, once: a symbol over its
    /// pixels, marked as Draw's, that each of them uses over its own
    /// rectangle.
    fn write_shared_pictures(&self, out: &mut dyn Write) -> fmt::Result {
        for (index, image) in self.shared.iter().enumerate() {
            let base = image.pixels();
            let (width, height) = (base.width(), base.height());
            write!(
                out,
                "<symbol id=\"picture-{}\" draw:picture=\"true\" viewBox=\"0 0 {width} {height}\" preserveAspectRatio=\"none\"><image width=\"{width}\" height=\"{height}\" preserveAspectRatio=\"none\" xlink:href=\"",
                index + 1
            )?;
            self.write_link(out, image)?;
            out.write_str("\"/></symbol>\n")?;
        }
        Ok(())
    }

    /// Every clip path: each clip's members, and after them the clip paths
    /// the groups within them call for.
    fn write_clips(&self, out: &mut dyn Write) -> fmt::Result {
        let mut pending = Vec::new();
        let mut next = self.clips.items().len() + 1;
        for (index, clip) in self.clips.items().iter().enumerate() {
            let at = (index + 1, None);
            self.write_clip(out, at, clip, Affine::IDENTITY, &mut pending, &mut next)?;
        }
        let mut written = 0;
        while let Some(clip) = pending.get(written).cloned() {
            let at = (clip.id, clip.then);
            self.write_clip(out, at, &clip.node, clip.outer, &mut pending, &mut next)?;
            written += 1;
        }
        Ok(())
    }

    /// One clip path, numbered `id` and clipped by `then`: `node` placed by
    /// `outer`. SVG's clip paths hold shapes and text but no groups, so a
    /// group is written as its members, each placed as the group places it.
    fn write_clip(
        &self,
        out: &mut dyn Write,
        (id, then): (usize, Option<usize>),
        node: &Node,
        outer: Affine,
        pending: &mut Vec<Pending>,
        next: &mut usize,
    ) -> fmt::Result {
        write!(
            out,
            "<clipPath id=\"clip-{id}\" clipPathUnits=\"userSpaceOnUse\""
        )?;
        if let Some(then) = then {
            write!(out, " clip-path=\"url(#clip-{then})\"")?;
        }
        out.write_str(">\n")?;
        self.write_clip_member(out, node, outer, None, (pending, next))?;
        out.write_str("</clipPath>\n")
    }

    /// `node` as the shapes a clip path holds, placed from its parent's space
    /// by `outer`, inside the clips of the groups `within` the clip around it.
    fn write_clip_member(
        &self,
        out: &mut dyn Write,
        node: &Node,
        outer: Affine,
        within: Option<&Within<'_>>,
        (pending, next): (&mut Vec<Pending>, &mut usize),
    ) -> fmt::Result {
        if !node.visible() {
            return Ok(());
        }
        let placed = node.transform().then(outer);
        let clipped = match node.kind() {
            Kind::Group { children, clip } => {
                let inner = clip.as_ref().map(|clip| Within {
                    clip,
                    members: placed,
                    outer: within,
                });
                for child in children.iter() {
                    let around = inner.as_ref().or(within);
                    self.write_clip_member(
                        out,
                        child,
                        placed,
                        around,
                        (&mut *pending, &mut *next),
                    )?;
                }
                return Ok(());
            }
            _ if within.is_some() => {
                // A member of no area clips nothing, so nothing calls for its
                // clip paths.
                let Some(back) = placed.invert() else {
                    return Ok(());
                };
                chain(within, back, pending, next)?
            }
            _ => None,
        };
        match node.kind() {
            Kind::Shape { shape, style } => {
                Self::write_shape(out, shape)?;
                Self::write_member_attributes(out, node, placed, clipped)?;
                self.write_style(out, style)?;
                Self::write_clip_rule(out, style)?;
                out.write_str("/>\n")
            }
            Kind::Text { text, style } => {
                Self::write_text_open(out, text)?;
                Self::write_member_attributes(out, node, placed, clipped)?;
                self.write_style(out, style)?;
                Self::write_clip_rule(out, style)?;
                out.write_str(">")?;
                self.write_text_content(out, text, style)?;
                out.write_str("</text>\n")
            }
            // A picture clips by its frame, which is a rectangle.
            Kind::Image { image } => {
                let place = image.place();
                write!(
                    out,
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
                    Number(place.min.x),
                    Number(place.min.y),
                    Number(place.width()),
                    Number(place.height())
                )?;
                Self::write_member_attributes(out, node, placed, clipped)?;
                out.write_str("/>\n")
            }
            Kind::Group { .. } => Ok(()),
        }
    }

    fn write_clip_rule(out: &mut dyn Write, style: &Style) -> fmt::Result {
        if style.fill_rule == FillRule::EvenOdd {
            out.write_str(" clip-rule=\"evenodd\"")?;
        }
        Ok(())
    }

    /// A clip path member's attributes: `placed` as its transform, and the
    /// clip path `clipped` that the groups within the clip around it make.
    fn write_member_attributes(
        out: &mut dyn Write,
        node: &Node,
        placed: Affine,
        clipped: Option<usize>,
    ) -> fmt::Result {
        if placed != Affine::IDENTITY {
            write!(out, " transform=\"{}\"", Matrix(placed))?;
        }
        if let Some(clipped) = clipped {
            write!(out, " clip-path=\"url(#clip-{clipped})\"")?;
        }
        if node.locked() {
            out.write_str(" draw:locked=\"true\"")?;
        }
        if let Some(name) = node.name() {
            write!(out, " draw:name=\"{}\"", Escaped(name))?;
        }
        Ok(())
    }

    /// A picture: its pixels laid over its rectangle — a use of the symbol
    /// holding them where more than one object shows them, else an image of
    /// its own.
    fn write_image(&self, out: &mut dyn Write, image: &Image) -> fmt::Result {
        let place = image.place();
        let rectangle = format_args!(
            "x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
            Number(place.min.x),
            Number(place.min.y),
            Number(place.width()),
            Number(place.height())
        );
        if let Some(id) = self.shared_id(image) {
            return write!(out, "<use xlink:href=\"#picture-{id}\" {rectangle}");
        }
        write!(
            out,
            "<image {rectangle} preserveAspectRatio=\"none\" xlink:href=\""
        )?;
        self.write_link(out, image)?;
        out.write_char('"')
    }

    /// A picture's pixels as a data URL.
    fn write_link(&self, out: &mut dyn Write, image: &Image) -> fmt::Result {
        let (media, bytes) = link(image, &self.encoded).ok_or(fmt::Error)?;
        data_url::write(out, media, bytes)
    }

    fn write_node(&self, out: &mut dyn Write, node: &Node) -> fmt::Result {
        match node.kind() {
            Kind::Group { .. } => {
                out.write_str("<g")?;
                self.write_group(out, node)
            }
            Kind::Shape { shape, style } => {
                Self::write_shape(out, shape)?;
                Self::write_node_attributes(out, node)?;
                self.write_style(out, style)?;
                out.write_str("/>\n")
            }
            Kind::Text { text, style } => {
                Self::write_text_open(out, text)?;
                Self::write_node_attributes(out, node)?;
                self.write_style(out, style)?;
                out.write_str(">")?;
                self.write_text_content(out, text, style)?;
                out.write_str("</text>\n")
            }
            Kind::Image { image } => {
                self.write_image(out, image)?;
                Self::write_node_attributes(out, node)?;
                out.write_str("/>\n")
            }
        }
    }

    /// The rest of a group whose `<g` is written: its attributes and its
    /// content.
    fn write_group(&self, out: &mut dyn Write, group: &Node) -> fmt::Result {
        let Kind::Group { children, clip } = group.kind() else {
            return Err(fmt::Error);
        };
        if let Some(clip) = clip {
            write!(out, " clip-path=\"url(#clip-{})\"", self.clip_id(clip)?)?;
        }
        Self::write_node_attributes(out, group)?;
        out.write_str(">\n")?;
        for child in children.iter() {
            self.write_node(out, child)?;
        }
        out.write_str("</g>\n")
    }

    fn write_node_attributes(out: &mut dyn Write, node: &Node) -> fmt::Result {
        let transform = node.transform();
        if transform != Affine::IDENTITY {
            write!(out, " transform=\"{}\"", Matrix(transform))?;
        }
        if node.opacity() < 1.0 {
            write!(out, " opacity=\"{}\"", Number(node.opacity()))?;
        }
        if !node.visible() {
            out.write_str(" display=\"none\"")?;
        }
        if node.locked() {
            out.write_str(" draw:locked=\"true\"")?;
        }
        if let Some(name) = node.name() {
            write!(out, " draw:name=\"{}\"", Escaped(name))?;
        }
        Ok(())
    }

    fn write_shape(out: &mut dyn Write, shape: &Shape) -> fmt::Result {
        match shape {
            Shape::Rect(rect) => {
                write!(
                    out,
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
                    Number(rect.x),
                    Number(rect.y),
                    Number(rect.width),
                    Number(rect.height)
                )?;
                if rect.rx > 0.0 || rect.ry > 0.0 {
                    write!(
                        out,
                        " rx=\"{}\" ry=\"{}\"",
                        Number(rect.rx),
                        Number(rect.ry)
                    )?;
                }
                Ok(())
            }
            Shape::Ellipse(ellipse) => match ellipse.arc {
                None => write!(
                    out,
                    "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\"",
                    Number(ellipse.centre.x),
                    Number(ellipse.centre.y),
                    Number(ellipse.radii.x),
                    Number(ellipse.radii.y)
                ),
                Some(arc) => {
                    Self::write_traced(out, shape)?;
                    let kind = match arc.kind {
                        ArcKind::Open => "open",
                        ArcKind::Pie => "pie",
                        ArcKind::Chord => "chord",
                    };
                    write!(
                        out,
                        " draw:shape=\"arc\" draw:cx=\"{}\" draw:cy=\"{}\" draw:rx=\"{}\" draw:ry=\"{}\" draw:start=\"{}\" draw:end=\"{}\" draw:arc=\"{kind}\"",
                        Number(ellipse.centre.x),
                        Number(ellipse.centre.y),
                        Number(ellipse.radii.x),
                        Number(ellipse.radii.y),
                        Number(arc.start),
                        Number(arc.end)
                    )
                }
            },
            Shape::Star(star) => {
                Self::write_traced(out, shape)?;
                write!(
                    out,
                    " draw:shape=\"star\" draw:cx=\"{}\" draw:cy=\"{}\" draw:points=\"{}\" draw:outer=\"{}\" draw:inner=\"{}\" draw:rotation=\"{}\" draw:rounding=\"{}\"",
                    Number(star.centre.x),
                    Number(star.centre.y),
                    star.points,
                    Number(star.outer),
                    Number(star.inner_ratio),
                    Number(star.rotation),
                    Number(star.rounding)
                )?;
                if star.polygon {
                    out.write_str(" draw:polygon=\"true\"")?;
                }
                Ok(())
            }
            Shape::Path(path) => {
                Self::write_traced(out, shape)?;
                out.write_str(" draw:nodes=\"")?;
                for node in path.contours.iter().flat_map(|contour| &contour.nodes) {
                    out.write_char(match node.join {
                        NodeJoin::Corner => 'c',
                        NodeJoin::Smooth => 's',
                        NodeJoin::Symmetric => 'z',
                    })?;
                }
                out.write_char('"')
            }
        }
    }

    /// A `<path>` drawing what `shape` traces.
    fn write_traced(out: &mut dyn Write, shape: &Shape) -> fmt::Result {
        let path = shape.to_path().map_err(|_| fmt::Error)?;
        out.write_str("<path d=\"")?;
        write_path_data(out, &path)?;
        out.write_char('"')
    }

    fn write_text_open(out: &mut dyn Write, text: &Text) -> fmt::Result {
        out.write_str("<text")?;
        if let Baseline::Lines { origin, .. } = &text.baseline {
            write!(
                out,
                " x=\"{}\" y=\"{}\"",
                Number(origin.x),
                Number(origin.y)
            )?;
        }
        write!(
            out,
            " xml:space=\"preserve\" font-family=\"{}\" font-size=\"{}\"",
            Escaped(&text.family),
            Number(text.size)
        )?;
        if text.weight != 400 {
            write!(out, " font-weight=\"{}\"", text.weight)?;
        }
        if text.italic {
            out.write_str(" font-style=\"italic\"")?;
        }
        if text.letter_spacing != 0.0 {
            write!(out, " letter-spacing=\"{}\"", Number(text.letter_spacing))?;
        }
        match text.align {
            TextAlign::Start => {}
            TextAlign::Middle => out.write_str(" text-anchor=\"middle\"")?,
            TextAlign::End => out.write_str(" text-anchor=\"end\"")?,
        }
        if let Baseline::Lines { line_height, .. } = &text.baseline {
            write!(out, " draw:line-height=\"{}\"", Number(*line_height))?;
        }
        Ok(())
    }

    /// A text's characters: each line a `<tspan>` placed on its baseline, or
    /// its one line a `<textPath>` over its path, a run's characters within
    /// a `<tspan>` of what it overrides.
    fn write_text_content(&self, out: &mut dyn Write, text: &Text, style: &Style) -> fmt::Result {
        match &text.baseline {
            Baseline::Lines { origin, .. } => {
                for (index, range) in text.line_ranges().enumerate() {
                    write!(
                        out,
                        "<tspan x=\"{}\" y=\"{}\">",
                        Number(origin.x),
                        Number(origin.y + text.baseline_drop(index))
                    )?;
                    Self::write_runs(out, text, style, range)?;
                    out.write_str("</tspan>")?;
                }
                Ok(())
            }
            Baseline::Path(along) => {
                write!(
                    out,
                    "<textPath xlink:href=\"#text-path-{}\"",
                    self.text_path_id(along)?
                )?;
                if along.start != 0.0 {
                    write!(out, " startOffset=\"{}\"", Number(along.start))?;
                }
                out.write_char('>')?;
                Self::write_runs(out, text, style, 0..text.content.len())?;
                out.write_str("</textPath>")
            }
        }
    }

    /// `range` of a text's content, each run's span of it within a `<tspan>`.
    fn write_runs(
        out: &mut dyn Write,
        text: &Text,
        style: &Style,
        range: Range<usize>,
    ) -> fmt::Result {
        let mut at = range.start;
        let mut next = text.runs.partition_point(|run| run.end <= at);
        while at < range.end {
            if let Some(run) = text.runs.get(next).filter(|run| run.start <= at) {
                let end = run.end.min(range.end);
                Self::write_run_open(out, &run.style, style)?;
                write!(out, "{}</tspan>", Escaped(&text.content[at..end]))?;
                at = end;
                next += 1;
            } else {
                let end = text
                    .runs
                    .get(next)
                    .map_or(range.end, |run| run.start.min(range.end));
                write!(out, "{}", Escaped(&text.content[at..end]))?;
                at = end;
            }
        }
        Ok(())
    }

    /// A run's `<tspan>`, saying what it overrides of its text's own `style`.
    fn write_run_open(out: &mut dyn Write, run: &RunStyle, style: &Style) -> fmt::Result {
        out.write_str("<tspan")?;
        if let Some(family) = &run.family {
            write!(out, " font-family=\"{}\"", Escaped(family))?;
        }
        if let Some(size) = run.size {
            write!(out, " font-size=\"{}\"", Number(size))?;
        }
        if let Some(weight) = run.weight {
            write!(out, " font-weight=\"{weight}\"")?;
        }
        if let Some(italic) = run.italic {
            let posture = if italic { "italic" } else { "normal" };
            write!(out, " font-style=\"{posture}\"")?;
        }
        if let Some(spacing) = run.letter_spacing {
            write!(out, " letter-spacing=\"{}\"", Number(spacing))?;
        }
        if let Some(shift) = run.baseline_shift {
            write!(out, " baseline-shift=\"{}em\"", Number(shift))?;
        }
        if let Some(fill) = run.fill {
            write_run_paint(out, "fill", fill, &style.fill)?;
        }
        if let Some(stroke) = run.stroke {
            write_run_paint(out, "stroke", stroke, &style.stroke.paint)?;
        }
        out.write_char('>')
    }

    #[allow(
        clippy::float_cmp,
        reason = "a value is left unsaid only when it is exactly SVG's initial value"
    )]
    fn write_style(&self, out: &mut dyn Write, style: &Style) -> fmt::Result {
        self.write_paint(out, "fill", &style.fill)?;
        if style.fill_rule == FillRule::EvenOdd {
            out.write_str(" fill-rule=\"evenodd\"")?;
        }
        let stroke = &style.stroke;
        self.write_paint(out, "stroke", &stroke.paint)?;
        if stroke.width != 1.0 {
            write!(out, " stroke-width=\"{}\"", Number(stroke.width))?;
        }
        match stroke.join {
            Join::Miter => {}
            Join::Round => out.write_str(" stroke-linejoin=\"round\"")?,
            Join::Bevel => out.write_str(" stroke-linejoin=\"bevel\"")?,
        }
        if stroke.miter_limit != 4.0 {
            write!(out, " stroke-miterlimit=\"{}\"", Number(stroke.miter_limit))?;
        }
        match stroke.cap {
            Cap::Butt => {}
            Cap::Round => out.write_str(" stroke-linecap=\"round\"")?,
            Cap::Square => out.write_str(" stroke-linecap=\"square\"")?,
        }
        if !stroke.dashes.is_empty() {
            out.write_str(" stroke-dasharray=\"")?;
            for (index, dash) in stroke.dashes.iter().enumerate() {
                if index > 0 {
                    out.write_char(' ')?;
                }
                write!(out, "{}", Number(*dash))?;
            }
            out.write_char('"')?;
        }
        if stroke.dash_offset != 0.0 {
            write!(out, " stroke-dashoffset=\"{}\"", Number(stroke.dash_offset))?;
        }
        for (head, at_start, end) in [(stroke.start, true, "start"), (stroke.end, false, "end")] {
            if head.shape == ArrowShape::None {
                continue;
            }
            if stroke.draws_head(&head) {
                let marker = Marker {
                    head,
                    colour: stroke.head_colour(),
                    at_start,
                };
                write!(
                    out,
                    " marker-{end}=\"url(#marker-{})\"",
                    name(&self.markers, marker.key())?
                )?;
            }
            write!(
                out,
                " draw:{end}-arrow=\"{} {} {}\"",
                arrow_shape(head.shape),
                Number(head.width),
                Number(head.length)
            )?;
        }
        if style.paint_order == PaintOrder::StrokeFirst {
            out.write_str(" paint-order=\"stroke\"")?;
        }
        Ok(())
    }

    fn write_paint(&self, out: &mut dyn Write, name: &str, paint: &Paint) -> fmt::Result {
        match paint {
            Paint::None => write!(out, " {name}=\"none\""),
            Paint::Colour(colour) => {
                write!(out, " {name}=\"{}\"", colour.without_alpha().hex().hashed())?;
                if colour.a < u8::MAX {
                    write!(out, " {name}-opacity=\"{}\"", alpha(colour.a))?;
                }
                Ok(())
            }
            Paint::Gradient(gradient) => {
                write!(
                    out,
                    " {name}=\"url(#gradient-{})\"",
                    self.gradient_id(gradient)?
                )
            }
        }
    }
}

/// What a picture is written as: the bytes it was read from where a web
/// format carries them, else its pixels as the PNG `encoded` holds for them.
fn link<'i>(
    image: &'i Image,
    encoded: &'i Numbered<usize, Vec<u8>>,
) -> Option<(&'static str, &'i [u8])> {
    embedded(image).or_else(|| {
        let at = encoded.number(Arc::as_ptr(image.pyramid()).addr())?;
        Some(("image/png", encoded.items().get(at)?.as_slice()))
    })
}

/// Of `shown` — each picture an object shows, by its key — those shown more
/// than once, the same pixels or pixels written as the same bytes alike,
/// each once in the order first shown; and the number each key takes among
/// them, sorted by key.
fn shared_pictures(
    shown: Vec<(PictureKey, Arc<Image>)>,
    encoded: &Numbered<usize, Vec<u8>>,
) -> Result<SharedPictures, DrawingError> {
    let mut keys = fallible::collected(shown.len(), shown.iter().map(|(key, _)| *key))
        .ok_or(DrawingError::OutOfMemory)?;
    keys.sort_unstable();
    let distinct = Numbered::of(shown)?.into_items();
    let shown_times = |image: &Image| {
        let key = picture_key(image);
        let first = keys.partition_point(|seen| *seen < key);
        keys[first..]
            .iter()
            .take_while(|seen| **seen == key)
            .count()
    };
    let links = fallible::collected(
        distinct.len(),
        distinct.iter().map(|image| link(image, encoded)),
    )
    .ok_or(DrawingError::OutOfMemory)?;
    // Distinct pictures written alike are neighbours, each run in the order
    // first shown.
    let mut order =
        fallible::collected(distinct.len(), 0..distinct.len()).ok_or(DrawingError::OutOfMemory)?;
    order.sort_by(|&a, &b| links[a].cmp(&links[b]).then(a.cmp(&b)));
    let mut runs: Vec<&[usize]> = Vec::new();
    let mut rest = order.as_slice();
    while let Some(&first) = rest.first() {
        let written = links[first];
        let alike = rest
            .iter()
            .take_while(|&&other| links[other] == written)
            .count();
        let (run, later) = rest.split_at(alike);
        rest = later;
        let times: usize = run.iter().map(|&at| shown_times(&distinct[at])).sum();
        if written.is_some() && times > 1 && !fallible::push(&mut runs, run) {
            return Err(DrawingError::OutOfMemory);
        }
    }
    runs.sort_unstable_by_key(|run| run[0]);
    let mut shared = Vec::new();
    let mut by_key = Vec::new();
    if !fallible::reserve(&mut shared, runs.len())
        || !fallible::reserve(&mut by_key, runs.iter().map(|run| run.len()).sum())
    {
        return Err(DrawingError::OutOfMemory);
    }
    for (number, run) in runs.iter().enumerate() {
        shared.push(Arc::clone(&distinct[run[0]]));
        by_key.extend(run.iter().map(|&at| (picture_key(&distinct[at]), number)));
    }
    by_key.sort_unstable();
    Ok((shared, by_key))
}

/// A run's paint as attribute `name`, its opacity said wherever it differs
/// from what the text's own `paint` leaves its `<tspan>` to inherit.
fn write_run_paint(out: &mut dyn Write, name: &str, paint: RunPaint, own: &Paint) -> fmt::Result {
    let inherited = match own {
        Paint::Colour(colour) => colour.a,
        Paint::None | Paint::Gradient(_) => u8::MAX,
    };
    match paint {
        RunPaint::None => write!(out, " {name}=\"none\""),
        RunPaint::Colour(colour) => {
            write!(out, " {name}=\"{}\"", colour.without_alpha().hex().hashed())?;
            if colour.a != inherited {
                write!(out, " {name}-opacity=\"{}\"", alpha(colour.a))?;
            }
            Ok(())
        }
    }
}

/// The bytes `image` was read from and their media type, where a web format
/// carries them as they came.
fn embedded(image: &Image) -> Option<(&'static str, &[u8])> {
    let source = image.source()?;
    Some((source.encoding.media_type()?, &source.bytes))
}

/// The clip paths the groups `within` a clip around a member make, written
/// in the member's space — `back` maps the clip path's onto it — outermost
/// first and each clipped by the one outside it; the innermost's number.
fn chain(
    within: Option<&Within<'_>>,
    back: Affine,
    pending: &mut Vec<Pending>,
    next: &mut usize,
) -> Result<Option<usize>, fmt::Error> {
    let Some(level) = within else {
        return Ok(None);
    };
    let then = chain(level.outer, back, pending, next)?;
    let id = *next;
    *next += 1;
    let clip = Pending {
        id,
        node: level.clip.clone(),
        outer: level.members.then(back),
        then,
    };
    if !fallible::push(pending, clip) {
        return Err(fmt::Error);
    }
    Ok(Some(id))
}

/// Call `visit` on every node of `drawing`, clips included, saying whether
/// the node is within a clip.
fn visit(drawing: &Drawing, visit: &mut dyn FnMut(&Node, bool)) {
    fn walk(node: &Node, within_clip: bool, visit: &mut dyn FnMut(&Node, bool)) {
        visit(node, within_clip);
        if let Kind::Group { children, clip } = node.kind() {
            if let Some(clip) = clip {
                walk(clip, true, visit);
            }
            for child in children.iter() {
                walk(child, within_clip, visit);
            }
        }
    }
    for layer in drawing.layers().iter() {
        walk(layer, false, visit);
    }
}

/// An arrowhead shape's spelling in Draw's attributes.
const fn arrow_shape(shape: ArrowShape) -> &'static str {
    match shape {
        ArrowShape::None => "none",
        ArrowShape::Triangle => "triangle",
        ArrowShape::Arrow => "arrow",
        ArrowShape::Circle => "circle",
        ArrowShape::Square => "square",
        ArrowShape::Diamond => "diamond",
        ArrowShape::Bar => "bar",
    }
}

/// A transform as an SVG `transform` value.
struct Matrix(Affine);

impl fmt::Display for Matrix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.0;
        // Only an exactly unscaled, unturned map may be spelled as a move.
        #[allow(clippy::float_cmp, reason = "the exact identity, and nothing near it")]
        let linear_identity = m.a == 1.0 && m.b == 0.0 && m.c == 0.0 && m.d == 1.0;
        if linear_identity {
            write!(f, "translate({} {})", Number(m.e), Number(m.f))
        } else {
            write!(
                f,
                "matrix({} {} {} {} {} {})",
                Number(m.a),
                Number(m.b),
                Number(m.c),
                Number(m.d),
                Number(m.e),
                Number(m.f)
            )
        }
    }
}

/// `path` as SVG path data.
fn write_path_data(out: &mut dyn Write, path: &Path) -> fmt::Result {
    for (index, subpath) in path.subpaths.iter().enumerate() {
        if index > 0 {
            out.write_char(' ')?;
        }
        write_subpath_data(out, subpath)?;
    }
    Ok(())
}

/// `subpath` as SVG path data.
fn write_subpath_data(out: &mut dyn Write, subpath: &Subpath) -> fmt::Result {
    write!(
        out,
        "M{} {}",
        Number(subpath.start.x),
        Number(subpath.start.y)
    )?;
    for element in &subpath.elements {
        match *element {
            PathEl::Line(to) => write!(out, " L{} {}", Number(to.x), Number(to.y))?,
            PathEl::Cubic(c1, c2, to) => write!(
                out,
                " C{} {} {} {} {} {}",
                Number(c1.x),
                Number(c1.y),
                Number(c2.x),
                Number(c2.y),
                Number(to.x),
                Number(to.y)
            )?,
        }
    }
    if subpath.closed {
        out.write_str(" Z")?;
    }
    Ok(())
}
