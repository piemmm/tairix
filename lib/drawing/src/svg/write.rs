//! Writing a drawing as SVG.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use core::fmt::{self, Write};

use tairix_bezier::{Affine, FillRule, Path, PathEl, Point};
use tairix_colour::Rgba;
use tairix_util::fallible;
use tairix_xml::Escaped;

use super::number::{alpha, Number};
use super::{DRAW_NAMESPACE, INKSCAPE_NAMESPACE};
use crate::drawing::{Drawing, GuideAxis};
use crate::node::{Kind, Node};
use crate::numbering::Numbered;
use crate::shape::{ArcKind, NodeJoin, Shape};
use crate::style::{
    ArrowShape, Arrowhead, Cap, Gradient, GradientKind, Join, Paint, PaintOrder, Spread, Style,
};
use crate::text::{Text, TextAlign};
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
/// is written so the two passes of the writer agree on every name.
struct Plan<'d> {
    drawing: &'d Drawing,
    gradients: Numbered<usize, Arc<Gradient>>,
    clips: Numbered<usize, Arc<Node>>,
    markers: Numbered<MarkerKey, Marker>,
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
        let mut failed = false;
        visit(drawing, &mut |node: &Node| {
            if failed {
                return;
            }
            if let Kind::Group {
                clip: Some(clip), ..
            } = node.kind()
            {
                failed |= !fallible::push(&mut clips, (Arc::as_ptr(clip).addr(), clip.clone()));
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
        Ok(Self {
            drawing,
            gradients: Numbered::of(gradients)?,
            clips: Numbered::of(clips)?,
            markers: Numbered::of(markers)?,
        })
    }

    fn gradient_id(&self, gradient: &Arc<Gradient>) -> Result<usize, fmt::Error> {
        name(&self.gradients, Arc::as_ptr(gradient).addr())
    }

    fn clip_id(&self, clip: &Arc<Node>) -> Result<usize, fmt::Error> {
        name(&self.clips, Arc::as_ptr(clip).addr())
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
             xmlns:draw=\"{DRAW_NAMESPACE}\" xmlns:inkscape=\"{INKSCAPE_NAMESPACE}\" version=\"1.1\" \
             width=\"{}{unit}\" height=\"{}{unit}\" viewBox=\"0 0 {} {}\"",
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
            write!(
                out,
                " draw:background=\"{}\"",
                background.without_alpha().hex().hashed()
            )?;
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
        if !(self.gradients.items().is_empty()
            && self.clips.items().is_empty()
            && self.markers.items().is_empty())
        {
            out.write_str("<defs>\n")?;
            self.write_gradients(out)?;
            self.write_markers(out)?;
            self.write_clips(out)?;
            out.write_str("</defs>\n")?;
        }
        if let Some(background) = page.background {
            writeln!(
                out,
                "<rect draw:role=\"background\" x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" fill=\"{}\"/>",
                size.x,
                size.y,
                background.without_alpha().hex().hashed(),
            )?;
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
            let (tag, geometry) = match gradient.kind {
                GradientKind::Linear { start, end } => (
                    "linearGradient",
                    alloc::format!(
                        "x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"",
                        Number(start.x),
                        Number(start.y),
                        Number(end.x),
                        Number(end.y)
                    ),
                ),
                GradientKind::Radial {
                    centre,
                    radius,
                    focus,
                } => (
                    "radialGradient",
                    alloc::format!(
                        "cx=\"{}\" cy=\"{}\" r=\"{}\" fx=\"{}\" fy=\"{}\"",
                        Number(centre.x),
                        Number(centre.y),
                        Number(radius),
                        Number(focus.x),
                        Number(focus.y)
                    ),
                ),
            };
            write!(
                out,
                "<{tag} id=\"gradient-{}\" gradientUnits=\"userSpaceOnUse\" {geometry}",
                index + 1
            )?;
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
            let Some(outline) = marker.head.outline(Point::ZERO, direction, 1.0) else {
                continue;
            };
            write!(
                out,
                "<marker id=\"marker-{}\" draw:marker=\"true\" markerUnits=\"strokeWidth\" orient=\"auto\" overflow=\"visible\" refX=\"0\" refY=\"0\" markerWidth=\"1\" markerHeight=\"1\"><path d=\"",
                index + 1
            )?;
            write_path_data(
                out,
                &Path {
                    subpaths: alloc::vec![outline],
                },
            )?;
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

    fn write_clips(&self, out: &mut dyn Write) -> fmt::Result {
        for (index, clip) in self.clips.items().iter().enumerate() {
            writeln!(
                out,
                "<clipPath id=\"clip-{}\" clipPathUnits=\"userSpaceOnUse\">",
                index + 1
            )?;
            self.write_node(out, clip)?;
            out.write_str("</clipPath>\n")?;
        }
        Ok(())
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
                Self::write_text_lines(out, text)?;
                out.write_str("</text>\n")
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
        write!(
            out,
            "<text x=\"{}\" y=\"{}\" xml:space=\"preserve\" font-family=\"{}\" font-size=\"{}\"",
            Number(text.origin.x),
            Number(text.origin.y),
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
        write!(out, " draw:line-height=\"{}\"", Number(text.line_height))
    }

    fn write_text_lines(out: &mut dyn Write, text: &Text) -> fmt::Result {
        for (index, line) in text.lines().enumerate() {
            write!(
                out,
                "<tspan x=\"{}\" y=\"{}\">{}</tspan>",
                Number(text.origin.x),
                Number(text.origin.y + text.baseline_drop(index)),
                Escaped(line)
            )?;
        }
        Ok(())
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

/// Call `visit` on every node of `drawing`, clips included.
fn visit(drawing: &Drawing, visit: &mut dyn FnMut(&Node)) {
    fn walk(node: &Node, visit: &mut dyn FnMut(&Node)) {
        visit(node);
        if let Kind::Group { children, clip } = node.kind() {
            if let Some(clip) = clip {
                walk(clip, visit);
            }
            for child in children.iter() {
                walk(child, visit);
            }
        }
    }
    for layer in drawing.layers().iter() {
        walk(layer, visit);
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
    let mut first = true;
    let mut separate = |out: &mut dyn Write| -> fmt::Result {
        if first {
            first = false;
            Ok(())
        } else {
            out.write_char(' ')
        }
    };
    for subpath in &path.subpaths {
        separate(out)?;
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
    }
    Ok(())
}
