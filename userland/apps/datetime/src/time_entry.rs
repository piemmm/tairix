//! The time entry: hours, minutes and seconds as fields stepped by the arrows
//! and typed into, on the region's 12- or 24-hour clock.

use alloc::string::String;

use tairix_controls::{
    paint_run, Button, ButtonAction, ButtonContent, ControlRole, ControlState, NumberAction,
    NumberField,
};
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};
use tairix_tz::HourCycle;

/// A time of day on the 24-hour clock, to the second.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TimeOfDay {
    /// `0..=23`.
    pub hour: u32,
    /// `0..=59`.
    pub minute: u32,
    /// `0..=59`.
    pub second: u32,
}

impl TimeOfDay {
    /// The time, or `None` past the clock.
    #[must_use]
    pub const fn new(hour: u32, minute: u32, second: u32) -> Option<Self> {
        if hour < 24 && minute < 60 && second < 60 {
            Some(Self {
                hour,
                minute,
                second,
            })
        } else {
            None
        }
    }
}

/// One part of the entry the keyboard can hold.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TimePart {
    /// The hour.
    Hour,
    /// The minute.
    Minute,
    /// The second.
    Second,
    /// Before or after noon, on a twelve-hour clock.
    Meridiem,
}

/// What an entry asks of its owner.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TimeAction {
    /// The time shown became this while typing continues.
    Edited(TimeOfDay),
    /// The entry settled on this time: a step, Enter, Escape taking typing
    /// back, a part letting the keyboard go, or a change of half of the day.
    Settled(TimeOfDay),
}

/// The parts of a 24-hour entry, in the order the keyboard walks them.
const TWENTY_FOUR: [TimePart; 3] = [TimePart::Hour, TimePart::Minute, TimePart::Second];

/// The parts of a twelve-hour entry.
const TWELVE: [TimePart; 4] = [
    TimePart::Hour,
    TimePart::Minute,
    TimePart::Second,
    TimePart::Meridiem,
];

/// What separates the hour, minute and second.
const SEPARATOR: &str = ":";

/// The words the halves of a twelve-hour day are named by.
const MORNING: &str = "AM";
const AFTERNOON: &str = "PM";

/// Where each part is drawn.
struct Geom {
    hour: Rect,
    minute: Rect,
    second: Rect,
    meridiem: Option<Rect>,
    /// The first and second separators' rectangles.
    separators: [Rect; 2],
}

impl Geom {
    fn rect(&self, part: TimePart) -> Option<Rect> {
        match part {
            TimePart::Hour => Some(self.hour),
            TimePart::Minute => Some(self.minute),
            TimePart::Second => Some(self.second),
            TimePart::Meridiem => self.meridiem,
        }
    }
}

/// Hours, minutes and seconds, and on a twelve-hour clock the half of the
/// day, each a part the keyboard holds in turn.
///
/// Each number part is a [`NumberField`]: typed into, stepped by Up and Down,
/// carried round past either end as a clock's minutes run from 59 to 0, and
/// settled as the keyboard leaves it. The half of the day is a button a click
/// or Space turns over, and Up, Down, `A` and `P` set. Tab and `:` walk the
/// parts; Tab past the last answers nothing, so the owner carries the focus on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeEntry {
    cycle: HourCycle,
    hour: NumberField,
    minute: NumberField,
    second: NumberField,
    afternoon: bool,
    meridiem: Button,
    focus: Option<TimePart>,
    state: ControlState,
    pointer: Point,
}

impl TimeEntry {
    /// An entry showing `time` on `cycle`.
    #[must_use]
    pub fn new(time: TimeOfDay, cycle: HourCycle) -> Self {
        let (hour, digits, (low, high)) = match cycle {
            HourCycle::TwentyFour => (time.hour, 2, (0, 23)),
            HourCycle::Twelve => (twelve_hour(time.hour), 1, (1, 12)),
        };
        let field = |value: u32, low: i32, high: i32, digits: u8| {
            NumberField::new(i32::try_from(value).unwrap_or(low), low, high)
                .with_digits(digits)
                .with_wrap()
        };
        let afternoon = time.hour >= 12;
        Self {
            cycle,
            hour: field(hour, low, high, digits),
            minute: field(time.minute, 0, 59, 2),
            second: field(time.second, 0, 59, 2),
            afternoon,
            meridiem: meridiem_button(afternoon),
            focus: None,
            state: ControlState::idle(),
            pointer: Point::ORIGIN,
        }
    }

    /// The time shown, on the 24-hour clock.
    #[must_use]
    pub fn time(&self) -> TimeOfDay {
        let number = |field: &NumberField| u32::try_from(field.value()).unwrap_or(0);
        let hour = number(&self.hour);
        let hour = match self.cycle {
            HourCycle::TwentyFour => hour,
            HourCycle::Twelve => hour % 12 + if self.afternoon { 12 } else { 0 },
        };
        TimeOfDay {
            hour,
            minute: number(&self.minute),
            second: number(&self.second),
        }
    }

    /// Show `time`, replacing whatever was typed; the owner reports the
    /// repaint.
    pub fn set_time(&mut self, time: TimeOfDay) {
        let hour = match self.cycle {
            HourCycle::TwentyFour => time.hour,
            HourCycle::Twelve => twelve_hour(time.hour),
        };
        let value = |number: u32| i32::try_from(number).unwrap_or(0);
        self.hour.set_value(value(hour));
        self.minute.set_value(value(time.minute));
        self.second.set_value(value(time.second));
        self.set_afternoon(time.hour >= 12);
    }

    /// The clock it counts hours on.
    #[must_use]
    pub const fn cycle(&self) -> HourCycle {
        self.cycle
    }

    /// The parts, in the order the keyboard walks them.
    #[must_use]
    pub const fn parts(&self) -> &'static [TimePart] {
        match self.cycle {
            HourCycle::TwentyFour => &TWENTY_FOUR,
            HourCycle::Twelve => &TWELVE,
        }
    }

    /// The composed state.
    #[must_use]
    pub const fn state(&self) -> ControlState {
        self.state
    }

    /// Replace the composed state; every part follows its enablement. An
    /// entry that can no longer act lets the keyboard go.
    pub fn set_state(&mut self, state: ControlState) {
        self.state = state;
        let part = ControlState::idle().with_enabled(state.enabled);
        for field in [&mut self.hour, &mut self.minute, &mut self.second] {
            let focused = field.state().focus.focused;
            field.set_state(part);
            field.set_focused(focused && state.is_actionable());
        }
        let focused = self.meridiem.state().focus.focused;
        self.meridiem.set_state(part);
        self.meridiem.set_focused(focused && state.is_actionable());
        if !state.is_actionable() {
            self.focus = None;
        }
    }

    /// The part holding the keyboard.
    #[must_use]
    pub const fn focus(&self) -> Option<TimePart> {
        self.focus
    }

    /// Give the keyboard to `part`, or let it go. The part it leaves is
    /// settled first, so typing left in it is kept.
    pub fn set_focus(
        &mut self,
        part: Option<TimePart>,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<TimeAction> {
        let part = part.filter(|part| self.parts().contains(part) && self.state.is_actionable());
        if part == self.focus {
            return None;
        }
        let geom = self.geom(bounds, scale, theme);
        let settled = self.release(&geom, damage);
        self.focus = part;
        if let Some(part) = part {
            self.hold(part, &geom, damage);
        }
        settled
    }

    /// Move the keyboard to the next part, or the previous one when
    /// `backward`, answering whether it moved and what the part it left
    /// settled on. Past either end it moves nowhere, so the owner carries the
    /// focus on.
    pub fn step_focus(
        &mut self,
        backward: bool,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> (bool, Option<TimeAction>) {
        let parts = self.parts();
        let at = self
            .focus
            .and_then(|focus| parts.iter().position(|&part| part == focus));
        let next = match (at, backward) {
            (None, false) => parts.first().copied(),
            (None, true) => parts.last().copied(),
            (Some(at), false) => parts.get(at + 1).copied(),
            (Some(at), true) => at.checked_sub(1).and_then(|at| parts.get(at)).copied(),
        };
        match next {
            Some(part) => (
                true,
                self.set_focus(Some(part), bounds, scale, theme, damage),
            ),
            None => (false, self.set_focus(None, bounds, scale, theme, damage)),
        }
    }

    /// The width the entry takes.
    #[must_use]
    pub fn width(&self, scale: Scale, theme: &Theme) -> u32 {
        let (gap, separator) = spacing(scale, theme);
        let numbers = self
            .hour
            .preferred_width(scale, theme)
            .saturating_add(self.minute.preferred_width(scale, theme))
            .saturating_add(self.second.preferred_width(scale, theme))
            .saturating_add(separator.saturating_mul(2));
        match self.cycle {
            HourCycle::TwentyFour => numbers,
            HourCycle::Twelve => numbers
                .saturating_add(gap)
                .saturating_add(meridiem_width(scale, theme)),
        }
    }

    /// The height the entry takes: one line of field.
    #[must_use]
    pub fn height(scale: Scale, theme: &Theme) -> u32 {
        NumberField::height(scale, theme)
    }

    fn geom(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Geom {
        let (gap, separator) = spacing(scale, theme);
        let height = Self::height(scale, theme).min(bounds.height);
        let top = bounds.top();
        let mut left = bounds.left();
        let mut take = |width: u32| {
            let rect = Rect::new(left, top, width, height);
            left = left.saturating_add(to_i32(width));
            rect
        };
        let hour = take(self.hour.preferred_width(scale, theme));
        let first = take(separator);
        let minute = take(self.minute.preferred_width(scale, theme));
        let second_separator = take(separator);
        let second = take(self.second.preferred_width(scale, theme));
        let meridiem = (self.cycle == HourCycle::Twelve).then(|| {
            let _ = take(gap);
            take(meridiem_width(scale, theme))
        });
        Geom {
            hour,
            minute,
            second,
            meridiem,
            separators: [first, second_separator],
        }
    }

    /// Where `part` is drawn within `bounds`.
    #[must_use]
    pub fn part_rect(
        &self,
        part: TimePart,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        self.geom(bounds, scale, theme).rect(part)
    }

    fn part_at(&self, geom: &Geom, point: Point) -> Option<TimePart> {
        self.parts()
            .iter()
            .copied()
            .find(|&part| geom.rect(part).is_some_and(|rect| rect.contains(point)))
    }

    fn field_mut(&mut self, part: TimePart) -> Option<&mut NumberField> {
        match part {
            TimePart::Hour => Some(&mut self.hour),
            TimePart::Minute => Some(&mut self.minute),
            TimePart::Second => Some(&mut self.second),
            TimePart::Meridiem => None,
        }
    }

    /// Settle and unfocus the part holding the keyboard.
    fn release(&mut self, geom: &Geom, damage: &mut Region) -> Option<TimeAction> {
        let part = self.focus.take()?;
        let rect = geom.rect(part)?;
        damage.add(rect);
        if part == TimePart::Meridiem {
            self.meridiem.set_focused(false);
            return None;
        }
        let field = self.field_mut(part)?;
        field.set_focused(false);
        let settled = field.commit(rect, damage);
        self.answer(settled)
    }

    fn hold(&mut self, part: TimePart, geom: &Geom, damage: &mut Region) {
        if let Some(rect) = geom.rect(part) {
            damage.add(rect);
        }
        match self.field_mut(part) {
            Some(field) => field.set_focused(true),
            None => self.meridiem.set_focused(true),
        }
    }

    /// A number part's action as the time it leaves the entry showing.
    fn answer(&self, action: Option<NumberAction>) -> Option<TimeAction> {
        match action? {
            NumberAction::Edited { .. } => Some(TimeAction::Edited(self.time())),
            NumberAction::Settled { .. } => Some(TimeAction::Settled(self.time())),
        }
    }

    fn set_afternoon(&mut self, afternoon: bool) {
        if self.afternoon == afternoon {
            return;
        }
        self.afternoon = afternoon;
        let state = self.meridiem.state();
        self.meridiem = meridiem_button(afternoon);
        self.meridiem.set_state(state);
    }

    /// Turn the half of the day to `afternoon`, settling there.
    fn turn(
        &mut self,
        afternoon: bool,
        rect: Option<Rect>,
        damage: &mut Region,
    ) -> Option<TimeAction> {
        if self.afternoon == afternoon {
            return None;
        }
        self.set_afternoon(afternoon);
        if let Some(rect) = rect {
            damage.add(rect);
        }
        Some(TimeAction::Settled(self.time()))
    }

    /// Feed a pointer event: a press gives its part the keyboard, the wheel
    /// steps the part holding it, and a click on the half of the day turns it
    /// over.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<TimeAction> {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        if !self.state.is_actionable() {
            return None;
        }
        let geom = self.geom(bounds, scale, theme);
        let mut answer = None;
        if let Some(rect) = geom.meridiem {
            if self.meridiem.on_pointer(event, rect, damage) == Some(ButtonAction::Activated) {
                answer = self.turn(!self.afternoon, Some(rect), damage);
            }
        }
        if matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        ) {
            if let Some(part) = self.part_at(&geom, self.pointer) {
                if self.focus != Some(part) {
                    let settled = self.release(&geom, damage);
                    self.focus = Some(part);
                    self.hold(part, &geom, damage);
                    answer = answer.or(settled);
                }
            }
        }
        if let Some((part, rect)) = self
            .focus
            .filter(|&part| part != TimePart::Meridiem)
            .and_then(|part| Some((part, geom.rect(part)?)))
        {
            let stepped = self
                .field_mut(part)
                .and_then(|field| field.on_pointer(event, rect, scale, theme, damage));
            return answer.or_else(|| self.answer(stepped));
        }
        if let (InputEvent::PointerScrolled { dy, .. }, Some(TimePart::Meridiem)) =
            (event, self.focus)
        {
            if *dy != 0 {
                return self.turn(!self.afternoon, geom.meridiem, damage);
            }
        }
        answer
    }

    /// The pointer left the entry: whatever it lit goes out.
    pub fn pointer_left(&mut self, bounds: Rect, scale: Scale, theme: &Theme, damage: &mut Region) {
        self.pointer = Point::new(i32::MIN, i32::MIN);
        if let Some(rect) = self.geom(bounds, scale, theme).meridiem {
            let away = InputEvent::PointerMoved { to: self.pointer };
            let _ = self.meridiem.on_pointer(&away, rect, damage);
        }
    }

    /// Feed a key to the part holding the keyboard. Tab answers nothing, so
    /// the owner walks the parts with [`step_focus`](Self::step_focus); `:`
    /// moves to the next part.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<TimeAction> {
        let part = self.focus?;
        if !self.state.is_actionable() {
            return None;
        }
        if key == Key::Char(':') {
            return self.step_focus(false, bounds, scale, theme, damage).1;
        }
        let geom = self.geom(bounds, scale, theme);
        let rect = geom.rect(part)?;
        if part == TimePart::Meridiem {
            let afternoon = match key {
                Key::Named(NamedKey::Up | NamedKey::Down) => !self.afternoon,
                Key::Char('a' | 'A') => false,
                Key::Char('p' | 'P') => true,
                Key::Named(NamedKey::Enter | NamedKey::Escape) => return None,
                _ => {
                    return (self.meridiem.on_key(key) == Some(ButtonAction::Activated))
                        .then(|| self.turn(!self.afternoon, Some(rect), damage))
                        .flatten();
                }
            };
            return self.turn(afternoon, Some(rect), damage);
        }
        let field = self.field_mut(part)?;
        let action = field.on_key(key, modifiers, rect, damage);
        self.answer(action)
    }

    /// Paint the entry into `surface` at `bounds`.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        let geom = self.geom(bounds, scale, theme);
        self.hour.render(surface, geom.hour, scale, theme);
        self.minute.render(surface, geom.minute, scale, theme);
        self.second.render(surface, geom.second, scale, theme);
        if let Some(rect) = geom.meridiem {
            self.meridiem.render(surface, rect, scale, theme);
        }
        let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
        let palette = theme.palette();
        let ink = Color::from(if self.state.enabled {
            palette.on_surface
        } else {
            palette.on_surface_muted
        });
        for rect in geom.separators {
            let width = font.text_width(SEPARATOR);
            let x = rect
                .left()
                .saturating_add(to_i32(rect.width.saturating_sub(width) / 2));
            let y = font.centred_top(rect.top(), rect.height);
            paint_run(
                surface,
                font,
                font.elide_to_width(SEPARATOR, rect.width),
                (x, y),
                ink,
                None,
            );
        }
    }
}

/// `hour` on the twelve-hour clock: 12, then 1 to 11.
const fn twelve_hour(hour: u32) -> u32 {
    match hour % 12 {
        0 => 12,
        hour => hour,
    }
}

/// The button naming the half of the day, `afternoon` or not.
fn meridiem_button(afternoon: bool) -> Button {
    let word = if afternoon { AFTERNOON } else { MORNING };
    Button::new(
        ButtonContent::Label(String::from(word)),
        ControlRole::Navigation,
    )
}

/// The width the half of the day's button takes: its wider word.
fn meridiem_width(scale: Scale, theme: &Theme) -> u32 {
    meridiem_button(false)
        .measured_width(scale, theme)
        .max(meridiem_button(true).measured_width(scale, theme))
}

/// The gap before the half of the day, and the width a separator stands in.
fn spacing(scale: Scale, theme: &Theme) -> (u32, u32) {
    let gap = scale.scale_length(theme.metrics().control_gap).max(1);
    let separator = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale)
        .text_width(SEPARATOR)
        .saturating_add(gap);
    (gap, separator)
}

/// `value` as a coordinate, saturating.
fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
#[path = "time_entry_tests.rs"]
mod tests;
