//! The canvas's scrollbars: the span each scrolls over — the page and what
//! is drawn, half a canvas of pasteboard beyond them, and wherever the
//! canvas looks now — and scrolling the canvas by them.
//!
//! A bar's model is derived from the viewport every time it is wanted, so it
//! can never disagree with where the canvas looks; only what a bar is doing
//! with the pointer is kept.

use tairix_bezier::Point;
use tairix_controls::{ScrollAction, ScrollBar, ScrollModel, ScrollRange};
use tairix_geometry::{Rect, Region, Scale};
use tairix_input::InputEvent;
use tairix_raster::Surface;
use tairix_theme::Theme;
use tairix_util::mathf::{ceil, clamp, floor, fmax, fmin, round};

use crate::layout::Layout;
use crate::view::View;
use crate::viewport::within_reach;

/// How far a line step of a bar scrolls, in logical pixels.
const LINE: u32 = 48;

/// One of the canvas's two bars.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Axis {
    /// The bar along the canvas's foot, scrolling across.
    Across,
    /// The bar down the canvas's side, scrolling down.
    Down,
}

impl Axis {
    const BOTH: [Self; 2] = [Self::Across, Self::Down];

    fn of(self, point: Point) -> f64 {
        match self {
            Self::Across => point.x,
            Self::Down => point.y,
        }
    }

    fn bar(self, layout: &Layout) -> Rect {
        match self {
            Self::Across => layout.horizontal_bar,
            Self::Down => layout.vertical_bar,
        }
    }

    fn extent(self, canvas: Rect) -> u32 {
        match self {
            Self::Across => canvas.width,
            Self::Down => canvas.height,
        }
    }
}

/// `length` device pixels as a scroll distance: whole, and never negative.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the length is clamped into the range of whole numbers a u64 holds exactly first"
)]
fn distance(length: f64) -> u64 {
    clamp(round(length), 0.0, 9_007_199_254_740_992.0) as u64
}

/// What one bar scrolls over: where its span starts, in device pixels from
/// the drawing's origin, and its model over the span.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Span {
    start: f64,
    model: ScrollModel,
}

impl View {
    /// What the bar along `axis` scrolls over.
    pub(crate) fn span(&self, axis: Axis) -> Span {
        let drawing = self.shown();
        let content = drawing.page().bounds().union(drawing.bounds());
        let pixels = self.viewport.pixels();
        let seen = f64::from(axis.extent(self.layout.canvas));
        let at = -axis.of(self.viewport.origin());
        // Held as the viewport is, so a drawing whose transforms reach past
        // anything it can hold still scrolls over a span the bar can show.
        let low = within_reach(axis.of(content.min));
        let high = within_reach(axis.of(content.max));
        let start = fmin(floor(low * pixels - seen / 2.0), at);
        let end = fmax(ceil(high * pixels + seen / 2.0), at + seen);
        let range = ScrollRange::new(distance(end - start), distance(seen), distance(at - start));
        Span {
            start,
            model: ScrollModel::in_pixels(range, u64::from(self.layout.length(LINE))),
        }
    }

    fn bar_mut(&mut self, axis: Axis) -> &mut ScrollBar {
        match axis {
            Axis::Across => &mut self.across,
            Axis::Down => &mut self.down,
        }
    }

    /// The canvas scrolled so the bar along `axis`, over `span`, stands at
    /// `offset`.
    fn scroll_bar_to(&mut self, axis: Axis, span: Span, offset: u64, damage: &mut Region) {
        let at = -axis.of(self.viewport.origin());
        #[allow(
            clippy::cast_precision_loss,
            reason = "a scroll offset is a device-pixel distance well within the range a float holds exactly"
        )]
        let wanted = span.start + offset as f64;
        let by = wanted - at;
        let step = match axis {
            Axis::Across => Point::new(by, 0.0),
            Axis::Down => Point::new(0.0, by),
        };
        self.viewport.scroll(step);
        damage.add(self.layout.window);
    }

    /// Whether a bar is holding the pointer, its thumb dragged or an end held.
    pub(crate) fn bar_held(&self) -> bool {
        self.across.is_pressing() || self.down.is_pressing()
    }

    /// Feed a pointer event to the bars, answering whether one took it.
    pub(crate) fn bars_input(
        &mut self,
        event: &InputEvent,
        over: tairix_geometry::Point,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> bool {
        let mut taken = false;
        for axis in Axis::BOTH {
            let span = self.span(axis);
            let bounds = axis.bar(&self.layout);
            let bar = self.bar_mut(axis);
            bar.set_model(span.model);
            let held = bar.is_pressing();
            let action = bar.on_pointer(event, bounds, scale, theme, damage);
            taken |= held || bar.is_pressing() || bounds.contains(over);
            if let Some(ScrollAction::ScrollTo { offset }) = action {
                self.scroll_bar_to(axis, span, offset, damage);
            }
        }
        taken
    }

    /// When a held end of either bar next steps.
    pub(crate) fn bars_deadline(&mut self, now_ns: u64) -> Option<u64> {
        let across = self.across.repeat_deadline(now_ns);
        let down = self.down.repeat_deadline(now_ns);
        across.into_iter().chain(down).min()
    }

    /// Step whichever held bar is due at `now_ns`.
    pub(crate) fn bars_repeat(
        &mut self,
        now_ns: u64,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) {
        for axis in Axis::BOTH {
            let span = self.span(axis);
            let bounds = axis.bar(&self.layout);
            let bar = self.bar_mut(axis);
            bar.set_model(span.model);
            if let Some(ScrollAction::ScrollTo { offset }) =
                bar.repeat(now_ns, bounds, scale, theme, damage)
            {
                self.scroll_bar_to(axis, span, offset, damage);
            }
        }
    }

    /// Paint both bars as the canvas stands.
    pub(crate) fn render_bars(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        for axis in Axis::BOTH {
            let mut bar = match axis {
                Axis::Across => self.across,
                Axis::Down => self.down,
            };
            bar.set_model(self.span(axis).model);
            bar.render(surface, axis.bar(&self.layout), scale, theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use tairix_bezier::Point;
    use tairix_drawing::unkept::Unkept;
    use tairix_drawing::Ids;
    use tairix_geometry::{Region, Scale};
    use tairix_theme::Theme;
    use tairix_window::docapp::DocumentView;
    use tairix_window::document::Access;

    use super::Axis;
    use crate::document::{blank, Document};
    use crate::layout::Faces;
    use crate::view::View;

    #[test]
    fn a_bar_spans_the_page_with_pasteboard_and_scrolling_by_it_moves_the_canvas() {
        let mut ids = Ids::new();
        let drawing =
            blank(&mut ids, &crate::preferences::Preferences::default()).expect("a drawing");
        let document = Document::new(drawing, ids, true);
        let mut view = View::new(
            document,
            String::from("Untitled"),
            Access::Untitled,
            Unkept::NONE,
            1.0,
        );
        let theme = Theme::light();
        let faces = Faces::of(&theme, Scale::ONE);
        view.lay_out(1080, 720, &theme, Scale::ONE, faces, &mut Region::new());
        let layout = view.layout;
        let span = view.span(Axis::Across);
        let range = span.model.range();
        let page = view.document().drawing().page().bounds().width() * view.viewport.pixels();
        let seen = f64::from(layout.canvas.width);
        let extent = u32::try_from(range.content_extent())
            .map(f64::from)
            .expect("a modest span");
        assert!(
            extent >= page + seen - 2.0,
            "the page and half a canvas either side"
        );
        assert!(
            range.offset() > 0 && range.offset() < range.max_offset(),
            "the fitted page sits within it"
        );
        let before = view.viewport.origin();
        view.scroll_bar_to(Axis::Across, span, range.offset() + 40, &mut Region::new());
        let moved = before.x - view.viewport.origin().x;
        assert!((moved - 40.0).abs() < 1.0, "forty pixels on: {moved}");
        let far = Point::new(1.0e6, 0.0);
        view.viewport.scroll(far);
        let wide = view.span(Axis::Across).model.range();
        assert_eq!(
            wide.offset(),
            wide.max_offset(),
            "wherever the canvas looks is within the span"
        );
    }
}
