//! The Date & Time window: where its parts are drawn, how they are painted,
//! and what each makes of the pointer and the keys.
//!
//! The window is a dialog window's client ([`Dialog::in_window`]) sized to
//! what it holds: the clock now; the date and time — network time on or off,
//! and beneath it the calendar and the time entry it is set by hand with;
//! then the zone; above Close. One layout serves the paint, the hit test and
//! the damage every change reports.

use alloc::string::String;
use alloc::vec;

use tairix_controls::{
    paint_run, Button, ButtonAction, ButtonContent, ControlRole, ControlState, Dialog,
    DialogAction, FieldAction, FieldControl, FieldGroup, FieldGroupAction, FieldLayout, FieldRow,
    Keystroke, Toggle,
};
use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};
use tairix_tz::{HourCycle, Weekday};

use crate::calendar::{CalendarAction, CalendarMonth, Date};
use crate::time_entry::{TimeAction, TimeEntry, TimeOfDay};
use crate::Readout;

/// The window title, which is also what the desktop lists it under.
pub const TITLE: &str = "Date & Time";

/// The window's width, in logical pixels.
const WIN_WIDTH: u32 = 560;

/// The narrowest and widest a calendar day's cell is, in logical pixels: it
/// takes whatever the time column leaves between the two.
const CELL_WIDTH: (u32, u32) = (40, 48);

/// A calendar day's cell's height, in logical pixels.
const CELL_HEIGHT: u32 = 32;

/// The network-time switch's label.
const AUTOMATIC: &str = "Set date and time automatically";

/// The line beneath the hand-set controls while network time keeps the clock.
const NETWORK_KEEPS_IT: &str =
    "The network keeps the clock. Turn network time off to set the date and time by hand.";

/// What the zone the machine keeps is for, beneath its next change.
const ZONE_FOR: &str =
    "The login screen, and every account that has not chosen a zone of its own, use this zone.";

/// What the window asks of its owner.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Request {
    /// Close the window.
    Close,
    /// Turn network time on or off.
    Network(bool),
    /// Open the time-zone chooser.
    ChooseZone,
    /// Set the clock to this date and time, local to the machine's zone.
    SetClock {
        /// The day.
        date: Date,
        /// The time of day.
        time: TimeOfDay,
    },
}

/// A part of the window the keyboard can hold, in the order Tab walks them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Focus {
    Automatic,
    Calendar,
    Time,
    Set,
    Zone,
    Close,
}

const ORDER: [Focus; 6] = [
    Focus::Automatic,
    Focus::Calendar,
    Focus::Time,
    Focus::Set,
    Focus::Zone,
    Focus::Close,
];

/// A part a press can land on and hold.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Part {
    Clock,
    Calendar,
    Time,
    Set,
    Zone,
    Dialog,
}

/// How the hand-set controls share their band.
#[derive(Copy, Clone)]
struct Hand {
    /// The calendar's width and height.
    calendar: (u32, u32),
    /// Whether the time column stands beside the calendar, rather than
    /// beneath it.
    beside: bool,
}

/// Where every part is drawn.
#[derive(Copy, Clone)]
struct Layout {
    bounds: Rect,
    hero: Rect,
    clock: FieldLayout,
    zone: FieldLayout,
    calendar: Rect,
    time_label: Rect,
    time: Rect,
    set: Rect,
}

/// The Date & Time window.
#[derive(Clone, Debug)]
pub struct Window {
    dialog: Dialog,
    readout: Readout,
    clock: FieldGroup,
    calendar: CalendarMonth,
    time: TimeEntry,
    set: Button,
    zone: FieldGroup,
    /// Whether network time keeps the clock, which takes the hand-set
    /// controls away.
    network: bool,
    /// Whether the date and time shown are the user's own rather than the
    /// clock's.
    edited: bool,
    focus: Option<Focus>,
    /// The part holding a press, which sees the stream until it ends.
    armed: Option<Part>,
    pointer: Point,
}

impl Window {
    /// A window showing `readout` for a machine keeping network time when
    /// `network`, in the zone `place`, its calendar weeks from
    /// `first_weekday` and its hours on `cycle`; the controls start on
    /// `today` at `time`. It is fitted to `scale` and `theme`.
    #[must_use]
    pub fn new(
        readout: Readout,
        (network, place): (bool, &str),
        (first_weekday, cycle): (Weekday, HourCycle),
        (today, time): (Date, TimeOfDay),
        (scale, theme): (Scale, &Theme),
    ) -> Self {
        let clock = FieldGroup::new(
            "DATE AND TIME",
            vec![
                FieldRow::new(AUTOMATIC, FieldControl::Toggle(Toggle::new("", network)))
                    .with_description(readout.source.clone()),
            ],
        );
        let mut calendar = CalendarMonth::new(today, first_weekday);
        calendar.set_chosen(Some(today));
        calendar.set_today(Some(today));
        let mut window = Self {
            dialog: Dialog::new(TITLE)
                .in_window()
                .with_actions(vec![Button::labelled("Close")]),
            zone: zone_group(place, &readout.change),
            readout,
            clock,
            calendar,
            time: TimeEntry::new(time, cycle),
            set: Button::new(
                ButtonContent::Label(String::from("Set")),
                ControlRole::Recommended,
            ),
            network,
            edited: false,
            focus: None,
            armed: None,
            pointer: Point::ORIGIN,
        };
        window.enable_hand_controls();
        window.fit(scale, theme);
        window
    }

    /// The window's extent at `scale`: its width, and as tall as what it
    /// holds.
    #[must_use]
    pub fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        let width = scale.scale_length(WIN_WIDTH);
        let band = self.dialog.content_width(width, scale, theme);
        let gap = gap(scale, theme);
        let content = hero_height(scale, theme)
            .saturating_add(gap)
            .saturating_add(group_height(&self.clock, band, scale, theme))
            .saturating_add(gap)
            .saturating_add(group_height(&self.zone, band, scale, theme));
        (
            width,
            self.dialog.height_for_content(content, width, scale, theme),
        )
    }

    /// The height the hand-set controls take across a band `width` wide.
    fn band_height(&self, width: u32, scale: Scale, theme: &Theme) -> u32 {
        let hand = self.hand(width, scale, theme);
        let column = time_column_height(scale, theme);
        if hand.beside {
            hand.calendar.1.max(column)
        } else {
            hand.calendar
                .1
                .saturating_add(gap(scale, theme))
                .saturating_add(column)
        }
    }

    /// How a band `width` wide is shared: the time column beside the
    /// calendar where the calendar keeps its narrowest cells, beneath it
    /// otherwise, the calendar's cells taking the rest up to their widest.
    fn hand(&self, width: u32, scale: Scale, theme: &Theme) -> Hand {
        let column = self
            .time
            .width(scale, theme)
            .max(self.set.measured_width(scale, theme));
        let (least, most) = (
            scale.scale_length(CELL_WIDTH.0),
            scale.scale_length(CELL_WIDTH.1),
        );
        let beside_cell =
            width.saturating_sub(column.saturating_add(gap(scale, theme).saturating_mul(2))) / 7;
        let (cell, beside) = if beside_cell >= least {
            (beside_cell.min(most), true)
        } else {
            ((width / 7).min(most), false)
        };
        Hand {
            calendar: (
                cell.saturating_mul(7),
                CalendarMonth::height_for(scale.scale_length(CELL_HEIGHT), scale, theme),
            ),
            beside,
        }
    }

    /// Fit the hand-set controls' band to `scale` and `theme`: the owner
    /// does so whenever either changes, before it measures or draws.
    pub fn fit(&mut self, scale: Scale, theme: &Theme) {
        let width = scale.scale_length(WIN_WIDTH);
        let band = self.dialog.content_width(width, scale, theme);
        let height = self.band_height(band, scale, theme);
        let _ = self.clock.set_band(height);
    }

    fn layout(&self, scale: Scale, theme: &Theme) -> Layout {
        let bounds = whole(self.extent(scale, theme));
        let content = self
            .dialog
            .content_rect(bounds, scale, theme)
            .unwrap_or(Rect::new(0, 0, 0, 0));
        let gap = to_i32(gap(scale, theme));
        let mut top = content.top();
        let mut take = |height: u32| {
            let rect = Rect::new(content.left(), top, content.width, height);
            top = top.saturating_add(to_i32(height)).saturating_add(gap);
            rect
        };
        let hero = take(hero_height(scale, theme));
        let clock_rect = take(group_height(&self.clock, content.width, scale, theme));
        let zone_rect = take(group_height(&self.zone, content.width, scale, theme));
        let clock = FieldLayout::new(
            clock_rect,
            self.clock.slot_column(clock_rect.width, scale, theme),
        );
        let zone = FieldLayout::new(
            zone_rect,
            self.zone.slot_column(zone_rect.width, scale, theme),
        );
        let band = self
            .clock
            .band_rect(clock, scale, theme)
            .unwrap_or(Rect::new(0, 0, 0, 0));
        let hand = self.hand(band.width, scale, theme);
        let calendar = Rect::new(band.left(), band.top(), hand.calendar.0, hand.calendar.1);
        let (column_left, column_top) = if hand.beside {
            (
                calendar.right().saturating_add(gap.saturating_mul(2)),
                band.top(),
            )
        } else {
            (band.left(), calendar.bottom().saturating_add(gap))
        };
        let label_h = caption_font(scale, theme).line_height();
        let entry_h = TimeEntry::height(scale, theme);
        let time_label = Rect::new(
            column_left,
            column_top,
            self.time.width(scale, theme),
            label_h,
        );
        let time = Rect::new(
            column_left,
            time_label.bottom(),
            self.time.width(scale, theme),
            entry_h,
        );
        let set_w = self.set.measured_width(scale, theme);
        let set_h = Button::height(scale, theme);
        let set = Rect::new(column_left, time.bottom().saturating_add(gap), set_w, set_h);
        Layout {
            bounds,
            hero,
            clock,
            zone,
            calendar,
            time_label,
            time,
            set,
        }
    }

    /// Whether the date and time shown are the user's own.
    #[must_use]
    pub const fn edited(&self) -> bool {
        self.edited
    }

    /// Show `readout`, reporting what it changed.
    pub fn adopt_readout(
        &mut self,
        readout: Readout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        if readout == self.readout {
            return;
        }
        let moved = self.clock.rows_mut()[0].set_description(Some(readout.source.clone()))
            | self.zone.set_footnote(Some(zone_footnote(&readout.change)));
        if moved {
            damage.add(whole(self.extent(scale, theme)));
        } else {
            let hero = self.layout(scale, theme).hero;
            for (line, (old, new)) in hero_lines(&self.readout)
                .into_iter()
                .zip(hero_lines(&readout))
                .enumerate()
            {
                if old != new {
                    damage.add(hero_line_rect(hero, line, scale, theme));
                }
            }
        }
        self.readout = readout;
    }

    /// Follow the clock: unless the user has made the date and time their
    /// own, the controls show `now`, `today` marked; with no time held they
    /// keep what they show.
    pub fn follow(
        &mut self,
        now: Option<(Date, TimeOfDay)>,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let Some((today, time)) = now else {
            return;
        };
        let layout = self.layout(scale, theme);
        if self.calendar.chosen() != Some(today) && !self.edited {
            self.calendar.set_chosen(Some(today));
            damage.add(layout.calendar);
        }
        if self.calendar.today() != Some(today) {
            self.calendar.set_today(Some(today));
            damage.add(layout.calendar);
        }
        if !self.edited && self.time.time() != time {
            self.time.set_time(time);
            damage.add(layout.time);
        }
    }

    /// Give the controls back to the clock, as after the clock was set from
    /// them.
    pub fn release_edit(&mut self, scale: Scale, theme: &Theme, damage: &mut Region) {
        if core::mem::replace(&mut self.edited, false) {
            self.enable_hand_controls();
            damage.add(self.layout(scale, theme).set);
        }
    }

    /// Show network time on or off, taking the hand-set controls away while
    /// it is on.
    pub fn set_network(&mut self, on: bool, scale: Scale, theme: &Theme, damage: &mut Region) {
        let mut moved = false;
        if let Some(FieldControl::Toggle(toggle)) =
            self.clock.rows_mut().first_mut().map(FieldRow::control_mut)
        {
            moved = toggle.is_on() != on;
            toggle.set_on(on);
        }
        if self.network != on {
            moved = true;
            self.network = on;
            if on {
                self.edited = false;
            }
            self.enable_hand_controls();
            if !self.focusable(self.focus) {
                self.focus = None;
                self.apply_focus(scale, theme, damage);
            }
        }
        if moved {
            damage.add(whole(self.extent(scale, theme)));
        }
    }

    /// Show the zone the machine keeps as `place`, its next change in
    /// `change`.
    pub fn set_zone(&mut self, place: &str, scale: Scale, theme: &Theme, damage: &mut Region) {
        let focused = self.zone.focus();
        self.zone = zone_group(place, &self.readout.change);
        self.zone.adopt_focus(focused);
        damage.add(whole(self.extent(scale, theme)));
    }

    /// State `reason` above Close, or nothing; the window repaints whole, as
    /// its bands move.
    pub fn set_reason(
        &mut self,
        reason: Option<String>,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        if self.dialog.set_reason(reason) {
            damage.add(whole(self.extent(scale, theme)));
        }
    }

    /// Put the keyboard on the time entry, as a refused time asks.
    pub fn focus_time(&mut self, scale: Scale, theme: &Theme, damage: &mut Region) {
        if self.focusable(Some(Focus::Time)) {
            self.focus = Some(Focus::Time);
            self.apply_focus(scale, theme, damage);
        }
    }

    /// The hand-set controls are usable exactly while network time is off,
    /// and Set while they hold the user's own date and time.
    fn enable_hand_controls(&mut self) {
        let usable = ControlState::idle().with_enabled(!self.network);
        let calendar_focused = self.calendar.state().focus.focused;
        self.calendar.set_state(usable);
        self.calendar.set_focused(calendar_focused && !self.network);
        self.time.set_state(usable);
        let focused = self.set.state().focus.focused;
        self.set
            .set_state(ControlState::idle().with_enabled(self.edited && !self.network));
        self.set
            .set_focused(focused && self.edited && !self.network);
        let note = self.network.then(|| String::from(NETWORK_KEEPS_IT));
        let _ = self.clock.set_footnote(note);
    }

    fn focusable(&self, focus: Option<Focus>) -> bool {
        match focus {
            None | Some(Focus::Automatic | Focus::Zone | Focus::Close) => true,
            Some(Focus::Calendar | Focus::Time) => !self.network,
            Some(Focus::Set) => !self.network && self.edited,
        }
    }

    /// Make every part's focus match `self.focus`, reporting what moved.
    fn apply_focus(&mut self, scale: Scale, theme: &Theme, damage: &mut Region) {
        let layout = self.layout(scale, theme);
        let focus = self.focus;
        self.clock.set_focus(
            (focus == Some(Focus::Automatic)).then_some(0),
            layout.clock,
            scale,
            theme,
            damage,
        );
        self.zone.set_focus(
            (focus == Some(Focus::Zone)).then_some(0),
            layout.zone,
            scale,
            theme,
            damage,
        );
        let calendar = focus == Some(Focus::Calendar);
        if self.calendar.state().focus.focused != calendar {
            self.calendar.set_focused(calendar);
            damage.add(layout.calendar);
        }
        if focus != Some(Focus::Time) && self.time.focus().is_some() {
            let settled = self.time.set_focus(None, layout.time, scale, theme, damage);
            self.note_time(settled);
        }
        if focus == Some(Focus::Time) && self.time.focus().is_none() {
            let first = self.time.parts().first().copied();
            let _ = self
                .time
                .set_focus(first, layout.time, scale, theme, damage);
        }
        let set = focus == Some(Focus::Set);
        if self.set.state().focus.focused != set {
            self.set.set_focused(set);
            damage.add(layout.set);
        }
        let close = focus == Some(Focus::Close);
        self.focus_close(close, &layout, scale, theme, damage);
    }

    /// Give Close the keyboard, or take it away, reporting the button.
    fn focus_close(
        &mut self,
        focused: bool,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let rect = self
            .dialog
            .action_rects(layout.bounds, scale, theme)
            .first()
            .copied();
        if let Some(button) = self.dialog.actions_mut().first_mut() {
            if button.state().focus.focused != focused {
                button.set_focused(focused);
                if let Some(rect) = rect {
                    damage.add(rect);
                }
            }
        }
    }

    /// Take what the time entry settled on as the user's own.
    fn note_time(&mut self, action: Option<TimeAction>) {
        if action.is_some() && !self.edited {
            self.edited = true;
            self.enable_hand_controls();
        }
    }

    /// Move the keyboard on by one part, or back when `backward`, past
    /// whatever cannot take it.
    fn step_focus(&mut self, backward: bool, scale: Scale, theme: &Theme, damage: &mut Region) {
        let at = self
            .focus
            .and_then(|focus| ORDER.iter().position(|&f| f == focus));
        let count = ORDER.len();
        for step in 1..=count {
            let index = match (at, backward) {
                (None, false) => step - 1,
                (None, true) => count - step,
                (Some(at), false) => (at + step) % count,
                (Some(at), true) => (at + count - step % count) % count,
            };
            let candidate = ORDER[index];
            if self.focusable(Some(candidate)) {
                self.focus = Some(candidate);
                if candidate == Focus::Time {
                    let parts = self.time.parts();
                    let part = if backward {
                        parts.last()
                    } else {
                        parts.first()
                    };
                    let layout = self.layout(scale, theme);
                    let settled =
                        self.time
                            .set_focus(part.copied(), layout.time, scale, theme, damage);
                    self.note_time(settled);
                }
                self.apply_focus(scale, theme, damage);
                return;
            }
        }
    }

    /// The part under `point`.
    fn part_at(&self, layout: &Layout, point: Point, scale: Scale, theme: &Theme) -> Option<Part> {
        let bounds = layout.bounds;
        if layout.calendar.contains(point) {
            return Some(Part::Calendar);
        }
        if layout.time.contains(point) {
            return Some(Part::Time);
        }
        if layout.set.contains(point) {
            return Some(Part::Set);
        }
        if layout.clock.bounds.contains(point) {
            return Some(Part::Clock);
        }
        if layout.zone.bounds.contains(point) {
            return Some(Part::Zone);
        }
        self.dialog
            .action_rects(bounds, scale, theme)
            .iter()
            .any(|rect| rect.contains(point))
            .then_some(Part::Dialog)
    }

    /// Feed a pointer event, answering what it asks of the owner.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Request> {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        let layout = self.layout(scale, theme);
        let under = self.part_at(&layout, self.pointer, scale, theme);
        let pressed = matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        );
        if pressed {
            self.armed = under;
            let focus = match under {
                Some(Part::Calendar) => Some(Focus::Calendar),
                Some(Part::Time) => Some(Focus::Time),
                Some(Part::Set) => Some(Focus::Set),
                Some(Part::Clock) => Some(Focus::Automatic),
                Some(Part::Zone) => Some(Focus::Zone),
                Some(Part::Dialog) | None => self.focus,
            };
            if self.focusable(focus) && focus != self.focus {
                self.focus = focus;
                if focus == Some(Focus::Time) {
                    self.focus_others_away(scale, theme, damage);
                } else {
                    self.apply_focus(scale, theme, damage);
                }
            }
        }
        let armed = self.armed;
        let route = |part: Part| {
            matches!(event, InputEvent::PointerMoved { .. })
                || armed.map_or(under == Some(part), |armed| armed == part)
        };
        let mut request = None;
        if route(Part::Dialog)
            && self
                .dialog
                .on_pointer(event, layout.bounds, scale, theme, damage)
                == Some(DialogAction::ActionActivated { index: 0 })
        {
            request = Some(Request::Close);
        }
        if route(Part::Clock) {
            if let Some(FieldGroupAction {
                action: FieldAction::Set { on },
                ..
            }) = self
                .clock
                .on_pointer(event, layout.clock, scale, theme, damage)
            {
                request = Some(Request::Network(on));
            }
        }
        if route(Part::Zone)
            && matches!(
                self.zone
                    .on_pointer(event, layout.zone, scale, theme, damage),
                Some(FieldGroupAction {
                    action: FieldAction::Activated,
                    ..
                })
            )
        {
            request = Some(Request::ChooseZone);
        }
        if route(Part::Calendar) {
            if let Some(CalendarAction::Chose(_)) =
                self.calendar
                    .on_pointer(event, layout.calendar, scale, theme, damage)
            {
                self.take_edit(&layout, damage);
            }
        }
        if route(Part::Time) {
            let action = self
                .time
                .on_pointer(event, layout.time, scale, theme, damage);
            if action.is_some() {
                self.take_edit(&layout, damage);
            }
        }
        if route(Part::Set)
            && self.set.on_pointer(event, layout.set, damage) == Some(ButtonAction::Activated)
        {
            request = self.set_clock();
        }
        if matches!(
            event,
            InputEvent::PointerReleased { .. } | InputEvent::PointerCancelled
        ) {
            self.armed = None;
        }
        request
    }

    /// Unfocus every part but the time entry, which a press has just taken
    /// the keyboard into itself.
    fn focus_others_away(&mut self, scale: Scale, theme: &Theme, damage: &mut Region) {
        let layout = self.layout(scale, theme);
        self.clock
            .set_focus(None, layout.clock, scale, theme, damage);
        self.zone.set_focus(None, layout.zone, scale, theme, damage);
        if self.calendar.state().focus.focused {
            self.calendar.set_focused(false);
            damage.add(layout.calendar);
        }
        if self.set.state().focus.focused {
            self.set.set_focused(false);
            damage.add(layout.set);
        }
        self.focus_close(false, &layout, scale, theme, damage);
    }

    /// The date and time shown became the user's own.
    fn take_edit(&mut self, layout: &Layout, damage: &mut Region) {
        if !self.edited {
            self.edited = true;
            self.enable_hand_controls();
            damage.add(layout.set);
        }
    }

    /// The request Set makes, when there is a date and time of the user's own
    /// to set.
    fn set_clock(&self) -> Option<Request> {
        if self.network || !self.edited {
            return None;
        }
        Some(Request::SetClock {
            date: self.calendar.chosen()?,
            time: self.time.time(),
        })
    }

    /// The pointer left the window: whatever it lit goes out.
    pub fn pointer_left(&mut self, scale: Scale, theme: &Theme, damage: &mut Region) {
        let away = InputEvent::PointerMoved {
            to: Point::new(i32::MIN, i32::MIN),
        };
        let _ = self.on_pointer(&away, scale, theme, damage);
    }

    /// Feed a key press, answering what it asks of the owner.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Request> {
        let layout = self.layout(scale, theme);
        if key == Key::Named(NamedKey::Tab) {
            if self.focus == Some(Focus::Time) {
                let (moved, settled) =
                    self.time
                        .step_focus(modifiers.shift, layout.time, scale, theme, damage);
                self.note_time(settled);
                if moved {
                    return None;
                }
            }
            self.step_focus(modifiers.shift, scale, theme, damage);
            return None;
        }
        let stroke = Keystroke {
            key,
            modifiers,
            at_ns: 0,
        };
        match self.focus {
            Some(Focus::Automatic) => {
                if let Some(FieldGroupAction {
                    action: FieldAction::Set { on },
                    ..
                }) = self
                    .clock
                    .on_key(stroke, layout.clock, scale, theme, damage)
                {
                    return Some(Request::Network(on));
                }
            }
            Some(Focus::Zone) => {
                if let Some(FieldGroupAction {
                    action: FieldAction::Activated,
                    ..
                }) = self.zone.on_key(stroke, layout.zone, scale, theme, damage)
                {
                    return Some(Request::ChooseZone);
                }
            }
            Some(Focus::Calendar) => {
                if key == Key::Named(NamedKey::Enter) {
                    return self.set_clock();
                }
                if let Some(CalendarAction::Chose(_)) =
                    self.calendar
                        .on_key(key, modifiers, layout.calendar, scale, theme, damage)
                {
                    self.take_edit(&layout, damage);
                    return None;
                }
            }
            Some(Focus::Time) => {
                let action = self
                    .time
                    .on_key(key, modifiers, layout.time, scale, theme, damage);
                if action.is_some() {
                    self.take_edit(&layout, damage);
                }
                if key == Key::Named(NamedKey::Enter) {
                    return self.set_clock();
                }
                if action.is_some() {
                    return None;
                }
            }
            Some(Focus::Set) => {
                let activated = self.set.on_key(key) == Some(ButtonAction::Activated);
                if activated {
                    return self.set_clock();
                }
            }
            Some(Focus::Close) => {
                let activated =
                    self.dialog.on_key(key) == Some(DialogAction::ActionActivated { index: 0 });
                if activated {
                    return Some(Request::Close);
                }
            }
            None => {}
        }
        (key == Key::Named(NamedKey::Escape)).then_some(Request::Close)
    }

    /// Paint the window into `surface`, which spans its extent.
    pub fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        let layout = self.layout(scale, theme);
        self.dialog.render(surface, layout.bounds, scale, theme);
        self.render_hero(surface, layout.hero, scale, theme);
        self.clock.render(surface, layout.clock, scale, theme);
        self.calendar.render(surface, layout.calendar, scale, theme);
        let caption = caption_font(scale, theme);
        let ink = Color::from(if self.network {
            theme.palette().on_surface_muted
        } else {
            theme.palette().on_surface
        });
        paint_run(
            surface,
            caption,
            caption.elide_to_width("Time", layout.time_label.width),
            (layout.time_label.left(), layout.time_label.top()),
            ink,
            None,
        );
        self.time.render(surface, layout.time, scale, theme);
        self.set.render(surface, layout.set, scale, theme);
        self.zone.render(surface, layout.zone, scale, theme);
    }

    fn render_hero(&self, surface: &mut Surface, hero: Rect, scale: Scale, theme: &Theme) {
        if FieldGroup::paint_plate(surface, hero, scale, theme).is_none() {
            return;
        }
        let palette = theme.palette();
        let colours = [
            palette.on_surface,
            palette.on_surface,
            palette.on_surface_muted,
        ];
        for (line, (text, colour)) in hero_lines(&self.readout)
            .into_iter()
            .zip(colours)
            .enumerate()
        {
            let rect = hero_line_rect(hero, line, scale, theme);
            let font = hero_font(line, scale, theme);
            paint_run(
                surface,
                font,
                font.elide_to_width(text, rect.width),
                (rect.left(), rect.top()),
                Color::from(colour),
                None,
            );
        }
    }
}

/// The zone group: the place the machine keeps, the command that changes
/// it, and what the zone does beneath.
fn zone_group(place: &str, change: &str) -> FieldGroup {
    FieldGroup::new(
        "TIME ZONE",
        vec![FieldRow::new(
            place,
            FieldControl::Button(Button::labelled("Change\u{2026}")),
        )],
    )
    .with_footnote(zone_footnote(change))
}

/// The zone group's footnote: the next change, then what the zone is for.
fn zone_footnote(change: &str) -> String {
    let mut footnote = String::from(change);
    if !footnote.is_empty() {
        footnote.push('\n');
    }
    footnote.push_str(ZONE_FOR);
    footnote
}

/// The hero's three lines: the time, the date, and the zone.
fn hero_lines(readout: &Readout) -> [&str; 3] {
    [&readout.time, &readout.date, &readout.zone]
}

fn hero_font(line: usize, scale: Scale, theme: &Theme) -> BitmapFont {
    let role = match line {
        0 => TextRole::Display,
        1 => TextRole::ItemTitle,
        _ => TextRole::Body,
    };
    BitmapFont::for_role(theme.fonts(), role, scale)
}

/// The hero plate's inner padding.
fn hero_pad(scale: Scale, theme: &Theme) -> u32 {
    scale
        .scale_length(theme.metrics().control_inset)
        .max(1)
        .saturating_mul(2)
}

fn hero_height(scale: Scale, theme: &Theme) -> u32 {
    let pad = hero_pad(scale, theme);
    (0..3)
        .map(|line| hero_font(line, scale, theme).line_height())
        .fold(pad.saturating_mul(2), u32::saturating_add)
}

/// Where hero line `line` is drawn within the plate `hero`.
fn hero_line_rect(hero: Rect, line: usize, scale: Scale, theme: &Theme) -> Rect {
    let pad = hero_pad(scale, theme);
    let top = (0..line)
        .map(|above| hero_font(above, scale, theme).line_height())
        .fold(pad, u32::saturating_add);
    Rect::new(
        hero.left().saturating_add(to_i32(pad)),
        hero.top().saturating_add(to_i32(top)),
        hero.width.saturating_sub(pad.saturating_mul(2)),
        hero_font(line, scale, theme).line_height(),
    )
}

/// The time column's height: its label, the entry, and Set beneath.
fn time_column_height(scale: Scale, theme: &Theme) -> u32 {
    caption_font(scale, theme)
        .line_height()
        .saturating_add(TimeEntry::height(scale, theme))
        .saturating_add(gap(scale, theme))
        .saturating_add(Button::height(scale, theme))
}

fn caption_font(scale: Scale, theme: &Theme) -> BitmapFont {
    BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale)
}

fn gap(scale: Scale, theme: &Theme) -> u32 {
    scale.scale_length(theme.metrics().control_gap).max(1)
}

/// The whole window at `extent`.
fn whole((width, height): (u32, u32)) -> Rect {
    Rect::new(0, 0, width, height)
}

/// The height `group` takes in a plate `width` wide, in its own column.
fn group_height(group: &FieldGroup, width: u32, scale: Scale, theme: &Theme) -> u32 {
    group.measured_height(width, group.slot_column(width, scale, theme), scale, theme)
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
