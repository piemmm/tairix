//! The calendar month: one month's days under the weekday initials, its week
//! beginning on the region's first day, and a day chosen by the pointer or
//! the keys.

use alloc::string::String;
use core::fmt::Write as _;

use tairix_abi::time::{civil_from_days, days_from_civil, days_in_month};
use tairix_controls::{
    paint_run, plate_border, withheld, Button, ButtonAction, ButtonContent, ControlRole,
    ControlState, NumberField, RenderInvariant,
};
use tairix_font::BitmapFont;
use tairix_fsmeta::calendar::{month_name, WEEKDAY_ABBREVIATIONS};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::IconKind;
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Ring, RingInk, Surface};
use tairix_theme::{TextRole, Theme};
use tairix_tz::Weekday;

use crate::{YEAR_MAX, YEAR_MIN};

/// A day of the proleptic Gregorian calendar, within the years a clock may be
/// set to.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Date {
    /// The year.
    pub year: i64,
    /// The month, `1..=12`.
    pub month: u32,
    /// The day of the month.
    pub day: u32,
}

impl Date {
    /// The date, or `None` for a day its month lacks or a year past
    /// [`YEAR_MIN`]..=[`YEAR_MAX`].
    #[must_use]
    pub fn new(year: i64, month: u32, day: u32) -> Option<Self> {
        ((YEAR_MIN..=YEAR_MAX).contains(&year) && (1..=days_in_month(year, month)).contains(&day))
            .then_some(Self { year, month, day })
    }

    /// Days after 1970-01-01.
    fn days(self) -> i64 {
        days_from_civil(self.year, self.month, self.day).unwrap_or(0)
    }

    fn from_days(days: i64) -> Option<Self> {
        let (year, month, day) = civil_from_days(days);
        Self::new(year, month, day)
    }

    /// The date `days` days on, or `None` past the calendar's years.
    #[must_use]
    pub fn plus_days(self, days: i64) -> Option<Self> {
        Self::from_days(self.days().checked_add(days)?)
    }

    /// The same day `months` months on, held to the last day of a shorter
    /// month, or `None` past the calendar's years.
    #[must_use]
    pub fn plus_months(self, months: i64) -> Option<Self> {
        let index = self
            .year
            .checked_mul(12)?
            .checked_add(i64::from(self.month) - 1)?;
        let moved = index.checked_add(months)?;
        let year = moved.div_euclid(12);
        let month = u32::try_from(moved.rem_euclid(12) + 1).ok()?;
        Self::new(year, month, self.day.min(days_in_month(year, month)))
    }

    /// The day of the week it falls on.
    #[must_use]
    pub fn weekday(self) -> Weekday {
        Weekday::of_days(self.days())
    }
}

/// What a [`CalendarMonth`] asks of its owner.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CalendarAction {
    /// This day was chosen.
    Chose(Date),
}

/// The month and year steppers in the header, in drawing order.
const STEPPERS: usize = 4;

/// What each stepper moves the month on show by, in months.
const STEPS: [i64; STEPPERS] = [-1, 1, -12, 12];

/// The weeks a month is drawn in: every month fits six, so the grid never
/// changes height as the month does.
const WEEKS: u32 = 6;

/// The calendar's layout within its bounds.
#[derive(Copy, Clone)]
struct Layout {
    steppers: [Rect; STEPPERS],
    /// Where the month's name and the year are written.
    month_label: Rect,
    year_label: Rect,
    initials: Rect,
    grid: Rect,
    cell_w: u32,
    cell_h: u32,
}

impl Layout {
    /// The rectangle of the cell `index` days into the grid.
    fn cell(&self, index: u32) -> Rect {
        let (row, column) = (index / 7, index % 7);
        Rect::new(
            self.grid
                .left()
                .saturating_add(to_i32(column * self.cell_w)),
            self.grid.top().saturating_add(to_i32(row * self.cell_h)),
            self.cell_w,
            self.cell_h,
        )
    }
}

/// One month's days as a grid under the weekday initials.
///
/// The arrows move the chosen day by a day and a week, the Page keys by a
/// month and with Shift by a year, Home and End to either end of its week; a
/// click chooses the day under it, a day of the month before or after
/// included, which brings that month on show. The steppers above browse the
/// months without changing the choice. Today is ringed; the chosen day is
/// filled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalendarMonth {
    /// The month on show: its first day.
    shown: Date,
    chosen: Option<Date>,
    today: Option<Date>,
    first_weekday: Weekday,
    state: ControlState,
    /// The day under the pointer.
    hovered: Option<Date>,
    steppers: [Button; STEPPERS],
    pointer: RenderInvariant<Point>,
    /// The day a press began on.
    armed: RenderInvariant<Option<Date>>,
}

impl CalendarMonth {
    /// A calendar showing `month`'s month with weeks from `first_weekday`.
    #[must_use]
    pub fn new(month: Date, first_weekday: Weekday) -> Self {
        let stepper = |icon| Button::new(ButtonContent::Icon(icon), ControlRole::Navigation);
        Self {
            shown: first_of(month),
            chosen: None,
            today: None,
            first_weekday,
            state: ControlState::idle(),
            hovered: None,
            steppers: [
                stepper(IconKind::NavBack),
                stepper(IconKind::NavForward),
                stepper(IconKind::NavBack),
                stepper(IconKind::NavForward),
            ],
            pointer: RenderInvariant::new(Point::ORIGIN),
            armed: RenderInvariant::new(None),
        }
    }

    /// The chosen day.
    #[must_use]
    pub const fn chosen(&self) -> Option<Date> {
        self.chosen
    }

    /// The month on show, as its first day.
    #[must_use]
    pub const fn shown(&self) -> Date {
        self.shown
    }

    /// Choose `date`, or nothing, showing its month; the owner reports the
    /// repaint.
    pub fn set_chosen(&mut self, date: Option<Date>) {
        self.chosen = date;
        if let Some(date) = date {
            self.shown = first_of(date);
        }
    }

    /// Mark `date` as today; the owner reports the repaint.
    pub fn set_today(&mut self, date: Option<Date>) {
        self.today = date;
    }

    /// The day marked as today.
    #[must_use]
    pub const fn today(&self) -> Option<Date> {
        self.today
    }

    /// The composed state.
    #[must_use]
    pub const fn state(&self) -> ControlState {
        self.state
    }

    /// Replace the composed state; the steppers follow its enablement.
    pub fn set_state(&mut self, state: ControlState) {
        self.state = state;
        for stepper in &mut self.steppers {
            stepper.set_state(ControlState::idle().with_enabled(state.enabled));
        }
    }

    /// Set whether it holds the keyboard.
    pub fn set_focused(&mut self, focused: bool) {
        self.state.focus.focused = focused;
    }

    /// The height a calendar needs for a grid of cells `cell` pixels tall.
    #[must_use]
    pub fn height_for(cell: u32, scale: Scale, theme: &Theme) -> u32 {
        NumberField::height(scale, theme)
            .saturating_add(initials_height(scale, theme))
            .saturating_add(cell.saturating_mul(WEEKS))
    }

    fn layout(bounds: Rect, scale: Scale, theme: &Theme) -> Layout {
        let header = NumberField::height(scale, theme).min(bounds.height);
        let initials = initials_height(scale, theme).min(bounds.height - header);
        let grid_h = bounds.height - header - initials;
        let (left, top, right) = (bounds.left(), bounds.top(), bounds.right());
        let side = to_i32(header);
        let gap = to_i32(scale.scale_length(theme.metrics().control_gap));
        let year_w = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale).text_width("-9999");
        let year_back = right
            .saturating_sub(side.saturating_mul(2))
            .saturating_sub(to_i32(year_w));
        let month_forward = year_back.saturating_sub(gap).saturating_sub(side);
        let at = |x: i32| Rect::new(x, top, header, header);
        let span = |from: i32, to: i32| u32::try_from(to.saturating_sub(from)).unwrap_or(0);
        Layout {
            steppers: [
                at(left),
                at(month_forward),
                at(year_back),
                at(right.saturating_sub(side)),
            ],
            month_label: Rect::new(
                left.saturating_add(side),
                top,
                span(left.saturating_add(side), month_forward),
                header,
            ),
            year_label: Rect::new(year_back.saturating_add(side), top, year_w, header),
            initials: Rect::new(left, top.saturating_add(side), bounds.width, initials),
            grid: Rect::new(
                left,
                top.saturating_add(to_i32(header + initials)),
                bounds.width,
                grid_h,
            ),
            cell_w: bounds.width / 7,
            cell_h: grid_h / WEEKS,
        }
    }

    /// The day drawn first: the grid's top-left, in or before the month.
    fn grid_start(&self) -> Option<Date> {
        let lead = self.shown.weekday().days_after(self.first_weekday);
        self.shown.plus_days(-i64::from(lead))
    }

    /// The day drawn in cell `index`.
    fn day_at(&self, index: u32) -> Option<Date> {
        self.grid_start()?.plus_days(i64::from(index))
    }

    /// The cell `date` is drawn in, where it is on show.
    fn index_of(&self, date: Date) -> Option<u32> {
        let start = self.grid_start()?;
        let index = u32::try_from(date.days().checked_sub(start.days())?).ok()?;
        (index < WEEKS * 7).then_some(index)
    }

    /// The day under window `point`.
    fn day_under(&self, layout: &Layout, point: Point) -> Option<Date> {
        if !layout.grid.contains(point) || layout.cell_w == 0 || layout.cell_h == 0 {
            return None;
        }
        let column = u32::try_from(point.x - layout.grid.left()).ok()? / layout.cell_w;
        let row = u32::try_from(point.y - layout.grid.top()).ok()? / layout.cell_h;
        (column < 7 && row < WEEKS)
            .then(|| self.day_at(row * 7 + column))
            .flatten()
    }

    fn report_day(&self, layout: &Layout, date: Option<Date>, damage: &mut Region) {
        if let Some(index) = date.and_then(|date| self.index_of(date)) {
            damage.add(layout.cell(index));
        }
    }

    /// Choose `date`, bringing its month on show; answers the choice.
    fn choose(
        &mut self,
        date: Date,
        layout: &Layout,
        damage: &mut Region,
    ) -> Option<CalendarAction> {
        if self.chosen == Some(date) {
            return None;
        }
        if first_of(date) == self.shown {
            self.report_day(layout, self.chosen, damage);
            self.report_day(layout, Some(date), damage);
        } else {
            self.shown = first_of(date);
            damage.add(layout.grid);
            damage.add(layout.month_label);
            damage.add(layout.year_label);
        }
        self.chosen = Some(date);
        Some(CalendarAction::Chose(date))
    }

    /// Feed a pointer event.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<CalendarAction> {
        if let InputEvent::PointerMoved { to } = event {
            *self.pointer = *to;
        }
        let layout = Self::layout(bounds, scale, theme);
        let point = *self.pointer;
        for (index, rect) in layout.steppers.into_iter().enumerate() {
            if self.steppers[index].on_pointer(event, rect, damage) == Some(ButtonAction::Activated)
            {
                if let Some(shown) = self.shown.plus_months(STEPS[index]) {
                    self.shown = shown;
                    damage.add(layout.grid);
                    damage.add(layout.month_label);
                    damage.add(layout.year_label);
                }
            }
        }
        if !self.state.is_actionable() {
            return None;
        }
        let under = self.day_under(&layout, point);
        match event {
            InputEvent::PointerMoved { .. } => {
                if self.hovered != under {
                    self.report_day(&layout, self.hovered, damage);
                    self.report_day(&layout, under, damage);
                    self.hovered = under;
                }
                None
            }
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                *self.armed = under;
                None
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => {
                let armed = core::mem::take(&mut *self.armed);
                let date = under.filter(|date| armed == Some(*date))?;
                self.choose(date, &layout, damage)
            }
            InputEvent::PointerCancelled => {
                *self.armed = None;
                None
            }
            _ => None,
        }
    }

    /// The pointer left the calendar: whatever it lit goes out.
    pub fn pointer_left(&mut self, bounds: Rect, scale: Scale, theme: &Theme, damage: &mut Region) {
        let layout = Self::layout(bounds, scale, theme);
        let away = InputEvent::PointerMoved {
            to: Point::new(i32::MIN, i32::MIN),
        };
        for (index, rect) in layout.steppers.into_iter().enumerate() {
            let _ = self.steppers[index].on_pointer(&away, rect, damage);
        }
        let hovered = self.hovered.take();
        self.report_day(&layout, hovered, damage);
    }

    /// Feed a key to a focused calendar. With nothing chosen, the first move
    /// chooses today, or the first of the month on show.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<CalendarAction> {
        if !self.state.focus.focused || !self.state.is_actionable() {
            return None;
        }
        let from = self.chosen.or(self.today).unwrap_or(self.shown);
        let into_week = i64::from(from.weekday().days_after(self.first_weekday));
        let year = if modifiers.shift { 12 } else { 1 };
        let target = match key {
            Key::Named(NamedKey::Left) => from.plus_days(-1),
            Key::Named(NamedKey::Right) => from.plus_days(1),
            Key::Named(NamedKey::Up) => from.plus_days(-7),
            Key::Named(NamedKey::Down) => from.plus_days(7),
            Key::Named(NamedKey::Home) => from.plus_days(-into_week),
            Key::Named(NamedKey::End) => from.plus_days(6 - into_week),
            Key::Named(NamedKey::PageUp) => from.plus_months(-year),
            Key::Named(NamedKey::PageDown) => from.plus_months(year),
            _ => return None,
        };
        let target = if self.chosen.is_none() { from } else { target? };
        let layout = Self::layout(bounds, scale, theme);
        self.choose(target, &layout, damage)
    }

    /// Paint the calendar into `surface` at `bounds`.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        let layout = Self::layout(bounds, scale, theme);
        let palette = theme.palette();
        let body = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
        let caption = BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale);
        let enabled = self.state.enabled;
        let ink = Color::from(if enabled {
            palette.on_surface
        } else {
            palette.on_surface_muted
        });
        let muted = Color::from(palette.on_surface_muted);
        for (button, rect) in self.steppers.iter().zip(layout.steppers) {
            button.render(surface, rect, scale, theme);
        }
        centred(
            surface,
            body,
            month_name(self.shown.month),
            layout.month_label,
            ink,
        );
        let mut year = String::new();
        let _ = write!(year, "{}", self.shown.year);
        centred(surface, body, &year, layout.year_label, ink);
        for column in 0..7u32 {
            let weekday = (self.first_weekday.iso() - 1 + column) % 7;
            let initial = WEEKDAY_ABBREVIATIONS
                .get(usize::try_from(weekday).unwrap_or(0))
                .and_then(|name| name.get(..1))
                .unwrap_or("");
            let cell = Rect::new(
                layout
                    .initials
                    .left()
                    .saturating_add(to_i32(column * layout.cell_w)),
                layout.initials.top(),
                layout.cell_w,
                layout.initials.height,
            );
            centred(surface, caption, initial, cell, muted);
        }
        let border = plate_border(theme, scale).max(1);
        for index in 0..WEEKS * 7 {
            let Some(day) = self.day_at(index) else {
                continue;
            };
            let cell = layout.cell(index);
            self.paint_day(surface, day, cell, (body, border), theme, (ink, muted));
        }
    }

    fn paint_day(
        &self,
        surface: &mut Surface,
        day: Date,
        cell: Rect,
        (font, border): (BitmapFont, u32),
        theme: &Theme,
        (ink, muted): (Color, Color),
    ) {
        let Some((x, y, w, h)) = disc(cell, border) else {
            return;
        };
        let palette = theme.palette();
        let radius = w.min(h) / 2;
        let chosen = self.chosen == Some(day);
        let in_month = day.month == self.shown.month && day.year == self.shown.year;
        if chosen {
            let fill = if self.state.enabled {
                palette.accent
            } else {
                palette.surface_pressed
            };
            surface.fill_round_rect(x, y, w, h, radius, Color::from(fill));
        } else if self.hovered == Some(day) && self.state.is_actionable() {
            surface.fill_round_rect(x, y, w, h, radius, Color::from(palette.surface_hover));
        }
        if self.today == Some(day) && !chosen {
            surface.wash_ring(
                x,
                y,
                w,
                h,
                Ring::uniform(radius, border),
                RingInk::Solid(Color::from(palette.accent)),
            );
        }
        if chosen && self.state.focus.focused {
            if let Some((fx, fy, fw, fh)) = outset(cell, x, y, w, h, border) {
                surface.wash_ring(
                    fx,
                    fy,
                    fw,
                    fh,
                    Ring::uniform(fw.min(fh) / 2, border),
                    RingInk::Solid(Color::from(palette.rim_active)),
                );
            }
        }
        let color = if chosen && self.state.enabled {
            Color::from(palette.on_accent)
        } else if in_month {
            ink
        } else {
            muted
        };
        let mut number = String::new();
        let _ = write!(number, "{}", day.day);
        centred(surface, font, &number, cell, color);
    }
}

/// The first day of `date`'s month.
fn first_of(date: Date) -> Date {
    Date { day: 1, ..date }
}

/// The height of the weekday initials' row.
fn initials_height(scale: Scale, theme: &Theme) -> u32 {
    BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale)
        .line_height()
        .saturating_add(scale.scale_length(theme.metrics().control_inset))
}

/// The square a day's mark is drawn in, centred in its cell and clear of a
/// focus ring's room round it.
fn disc(cell: Rect, border: u32) -> Option<(u32, u32, u32, u32)> {
    let side = cell
        .width
        .min(cell.height)
        .checked_sub(border.saturating_mul(4))?;
    if side == 0 {
        return None;
    }
    let x = u32::try_from(cell.left()).ok()? + (cell.width - side) / 2;
    let y = u32::try_from(cell.top()).ok()? + (cell.height - side) / 2;
    Some((x, y, side, side))
}

/// The ring a focused day wears: one border outside its disc, inside its cell.
fn outset(cell: Rect, x: u32, y: u32, w: u32, h: u32, border: u32) -> Option<(u32, u32, u32, u32)> {
    let gap = border.saturating_mul(2);
    let fx = x.checked_sub(gap)?;
    let fy = y.checked_sub(gap)?;
    let (cx, cy) = (
        u32::try_from(cell.left()).ok()?,
        u32::try_from(cell.top()).ok()?,
    );
    (fx >= cx && fy >= cy).then_some((fx, fy, w + gap * 2, h + gap * 2))
}

/// Write `text` centred in `area`.
fn centred(surface: &mut Surface, font: BitmapFont, text: &str, area: Rect, color: Color) {
    let run = font.elide_to_width(text, area.width);
    let width = font.text_width(run.0);
    let x = area
        .left()
        .saturating_add(to_i32(area.width.saturating_sub(width) / 2));
    let y = font.centred_top(area.top(), area.height);
    paint_run(surface, font, run, (x, y), color, None);
}

/// `value` as a coordinate, saturating.
fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
#[path = "calendar_tests.rs"]
mod tests;
