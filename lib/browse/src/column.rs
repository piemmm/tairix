//! Where a scrolling column rests, and the bar that draws it.
//!
//! Every scrolling surface this engine draws — the listing, the places rail,
//! the *Open With…* chooser, a Properties window's sections — holds one
//! [`ScrollColumn`]: the
//! offset in pixels, and the [`ScrollBar`] carrying its own hover, drag, and
//! wheel carry. The offset is a request, clamped by the surface's own geometry
//! each time it is used, so a resize never leaves it past the end; the surface
//! hands that geometry in as the [`ScrollModel`] it implies.

use tairix_controls::scroll::{ScrollModel, ScrollOrientation, ScrollRange};
use tairix_controls::{ScrollAction, ScrollBar, ScrollPart};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, PointerButton};
use tairix_theme::Theme;

/// A column's scroll offset and the bar that draws it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScrollColumn {
    offset: u64,
    bar: ScrollBar,
}

impl Default for ScrollColumn {
    fn default() -> Self {
        Self::new()
    }
}

impl ScrollColumn {
    /// A column at rest at its start.
    #[must_use]
    pub fn new() -> Self {
        Self {
            offset: 0,
            bar: ScrollBar::new(
                ScrollOrientation::Vertical,
                ScrollModel::in_pixels(ScrollRange::EMPTY, 1),
            ),
        }
    }

    /// How far the column is scrolled, in pixels.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// Rest the column `offset` pixels down, answering whether it moved.
    pub fn set_offset(&mut self, offset: u64) -> bool {
        let moved = offset != self.offset;
        self.offset = offset;
        moved
    }

    /// The bar, carrying its live hover and drag state.
    #[must_use]
    pub const fn scrollbar(&self) -> &ScrollBar {
        &self.bar
    }

    /// Scroll by a wheel turn of `(dx, dy)`, in the seat's scroll units,
    /// through the bar drawn at `bar` once it holds `model`, answering whether
    /// the column moved.
    ///
    /// The bar carries what is short of a whole pixel into the next turn. A
    /// move reports the bar and `shown`, the column's on-screen area.
    pub fn wheel(
        &mut self,
        model: ScrollModel,
        (dx, dy): (i32, i32),
        scale: Scale,
        (bar, shown): (Rect, Rect),
        damage: &mut Region,
    ) -> bool {
        self.bar.set_model(model);
        let Some(ScrollAction::ScrollTo { offset }) = self.bar.wheel(dx, dy, scale, bar, damage)
        else {
            return false;
        };
        self.adopt(offset, shown, damage)
    }

    /// Route a pointer `event` at `point` to the bar drawn at `bar` once it
    /// holds `model`: `Some` when the bar took it, carrying whether that
    /// repainted anything; `None` when the pointer had nothing to do with the
    /// bar.
    ///
    /// The bar keeps what a press on it started: a press steps or pages, a
    /// press on the thumb captures a drag, and the moves and the release that
    /// follow are the bar's until it ends. A hover over the bar is taken, so it
    /// can brighten. A press and a release carry no position of their own, so
    /// the bar is moved to `point` first. The bar reports its own look, and a
    /// move reports `shown`, the column's on-screen area, beside it; a sample
    /// that changed neither reports nothing and answers `Some(false)`.
    pub fn route(
        &mut self,
        model: ScrollModel,
        (bar, shown): (Rect, Rect),
        scale: Scale,
        theme: &Theme,
        (point, event): (Point, &InputEvent),
        damage: &mut Region,
    ) -> Option<bool> {
        self.bar.set_model(model);
        let mut drew = tairix_controls::damage::sink();
        let moved = self.bar.on_pointer(
            &InputEvent::PointerMoved { to: point },
            bar,
            scale,
            theme,
            &mut drew,
        );
        let held = self.bar.is_pressing();
        let on_bar = self.bar.part_at(bar, point, scale, theme) != ScrollPart::Outside;
        let (taken, action) = match event {
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => (
                on_bar,
                self.bar.on_pointer(event, bar, scale, theme, &mut drew),
            ),
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => (
                held,
                self.bar.on_pointer(event, bar, scale, theme, &mut drew),
            ),
            InputEvent::PointerMoved { .. } => (held || on_bar, moved),
            _ => (false, None),
        };
        if let Some(ScrollAction::ScrollTo { offset }) = action {
            self.adopt(offset, shown, &mut drew);
        }
        for rect in drew.rects() {
            damage.add(*rect);
        }
        taken.then_some(!drew.is_empty())
    }

    /// Take the offset the bar asked for, reporting `shown` when it moved.
    fn adopt(&mut self, offset: u64, shown: Rect, damage: &mut Region) -> bool {
        let moved = self.set_offset(offset);
        if moved {
            damage.add(shown);
        }
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::ScrollColumn;
    use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
    use tairix_controls::damage::sink;
    use tairix_controls::scroll::{ScrollModel, ScrollRange, WHEEL_STEP};
    use tairix_controls::ScrollPart;
    use tairix_geometry::{Point, Rect, Scale};
    use tairix_input::{InputEvent, PointerButton};
    use tairix_theme::Theme;

    const BAR: Rect = Rect::new(90, 0, 10, 100);
    const SHOWN: Rect = Rect::new(0, 0, 90, 100);

    /// A thousand pixels of content through a hundred-pixel column.
    fn geometry(column: &ScrollColumn) -> ScrollModel {
        ScrollModel::in_pixels(ScrollRange::new(1000, 100, column.offset()), 20)
    }

    #[test]
    fn a_detent_moves_the_wheel_step_and_reports_the_bar_and_the_column() {
        let mut column = ScrollColumn::new();
        let mut damage = sink();
        let turned = column.wheel(
            geometry(&column),
            (0, SCROLL_UNITS_PER_DETENT),
            Scale::ONE,
            (BAR, SHOWN),
            &mut damage,
        );
        assert!(turned);
        assert_eq!(column.offset(), u64::from(WHEEL_STEP));
        let covered = damage.bounds();
        assert_eq!(covered.intersection(&BAR), BAR, "the thumb moved");
        assert_eq!(covered.intersection(&SHOWN), SHOWN, "and every row with it");
    }

    /// A turn worth less than a pixel moves nothing, and is not lost: the bar
    /// carries it until the turns add up to one.
    #[test]
    fn a_turn_short_of_a_pixel_carries_to_the_next() {
        let mut column = ScrollColumn::new();
        let per_pixel = SCROLL_UNITS_PER_DETENT.unsigned_abs().div_ceil(WHEEL_STEP);
        let turns = |column: &mut ScrollColumn, units: i32| {
            let model = geometry(column);
            column.wheel(model, (0, units), Scale::ONE, (BAR, SHOWN), &mut sink())
        };
        assert!(!turns(&mut column, 1), "a unit is a fraction of a pixel");
        assert_eq!(column.offset(), 0);
        let rest = i32::try_from(per_pixel - 1).expect("a small count");
        assert!(
            turns(&mut column, rest),
            "the carried unit completes the pixel"
        );
        assert_eq!(column.offset(), 1);
    }

    #[test]
    fn a_wheel_at_the_end_moves_nothing_and_reports_nothing() {
        let mut column = ScrollColumn::new();
        let mut damage = sink();
        let model = geometry(&column);
        assert!(!column.wheel(
            model,
            (0, -SCROLL_UNITS_PER_DETENT),
            Scale::ONE,
            (BAR, SHOWN),
            &mut damage
        ));
        assert_eq!(column.offset(), 0);
        assert!(damage.is_empty());
    }

    #[test]
    fn a_press_on_the_bar_is_taken_and_one_beside_it_is_not() {
        let theme = Theme::dark();
        let mut column = ScrollColumn::new();
        let press = InputEvent::PointerPressed {
            button: PointerButton::Primary,
        };
        let increment = Point::new(BAR.left() + 5, BAR.bottom() - 1);
        let mut damage = sink();
        let model = geometry(&column);
        assert_eq!(
            column.route(
                model,
                (BAR, SHOWN),
                Scale::ONE,
                &theme,
                (increment, &press),
                &mut damage
            ),
            Some(true)
        );
        assert_eq!(column.offset(), 20, "one line, the model's own step");
        assert_eq!(damage.bounds().intersection(&SHOWN), SHOWN);
        let release = InputEvent::PointerReleased {
            button: PointerButton::Primary,
        };
        let model = geometry(&column);
        assert_eq!(
            column.route(
                model,
                (BAR, SHOWN),
                Scale::ONE,
                &theme,
                (increment, &release),
                &mut sink()
            ),
            Some(true),
            "the release ends the press it started, and the end button lets go"
        );
        let model = geometry(&column);
        assert_eq!(
            column.route(
                model,
                (BAR, SHOWN),
                Scale::ONE,
                &theme,
                (Point::new(10, 10), &press),
                &mut sink()
            ),
            None
        );
        assert_eq!(column.offset(), 20);
    }

    #[test]
    fn a_thumb_drag_keeps_the_pointer_until_the_release() {
        let theme = Theme::dark();
        let mut column = ScrollColumn::new();
        let x = BAR.left() + 5;
        let mut probe = *column.scrollbar();
        probe.set_model(geometry(&column));
        let thumb = (BAR.top()..BAR.bottom())
            .find(|&y| {
                probe.part_at(BAR, Point::new(x, y), Scale::ONE, &theme) == ScrollPart::Thumb
            })
            .expect("a draggable thumb");
        let route = |column: &mut ScrollColumn, at: Point, event: &InputEvent| {
            let model = geometry(column);
            column.route(
                model,
                (BAR, SHOWN),
                Scale::ONE,
                &theme,
                (at, event),
                &mut sink(),
            )
        };
        let grab = Point::new(x, thumb);
        let press = InputEvent::PointerPressed {
            button: PointerButton::Primary,
        };
        assert_eq!(
            route(&mut column, grab, &press),
            Some(true),
            "the grabbed thumb brightens"
        );
        let away = Point::new(10, BAR.bottom() - 1);
        assert_eq!(
            route(&mut column, away, &InputEvent::PointerMoved { to: away }),
            Some(true),
            "a drag off the bar is still the bar's"
        );
        assert!(column.offset() > 0);
        let release = InputEvent::PointerReleased {
            button: PointerButton::Primary,
        };
        assert_eq!(route(&mut column, away, &release), Some(true));
        assert_eq!(
            route(&mut column, away, &InputEvent::PointerMoved { to: away }),
            None,
            "and a move after the release is not"
        );
    }

    /// A motion over the bar that changes nothing it draws is still the bar's,
    /// and repaints nothing.
    #[test]
    fn an_idle_motion_over_the_bar_repaints_nothing() {
        let theme = Theme::dark();
        let mut column = ScrollColumn::new();
        let over = Point::new(BAR.left() + 5, BAR.top() + 40);
        let hover = InputEvent::PointerMoved { to: over };
        let model = geometry(&column);
        assert_eq!(
            column.route(
                model,
                (BAR, SHOWN),
                Scale::ONE,
                &theme,
                (over, &hover),
                &mut sink()
            ),
            Some(true),
            "arriving over the bar wakes it"
        );
        let nudged = Point::new(over.x, over.y + 1);
        let mut damage = sink();
        let model = geometry(&column);
        assert_eq!(
            column.route(
                model,
                (BAR, SHOWN),
                Scale::ONE,
                &theme,
                (nudged, &InputEvent::PointerMoved { to: nudged }),
                &mut damage
            ),
            Some(false)
        );
        assert!(damage.is_empty());
    }
}
