//! The tool-controls bar: the tool in use, named, then each of its settings
//! left to right — a number in a [`NumberField`], a choice, or a switch —
//! editing the tool's [`Options`] as it changes.
//!
//! A setting is the window's own and nothing is written anywhere, so a value
//! typed or stepped applies at once, its settle included.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_controls::{
    owner_chord, paint_run, Button, Checkbox, ComboAction, ComboBox, NumberAction, NumberField,
    SelectionState, SelectorAction,
};
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use crate::layout::Faces;
use crate::tool::{Options, Setting, Tool, MOST_SETTINGS, WHOLE_PIXELS};

/// What an input to the bar came to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BarOutcome {
    /// Not the bar's.
    Ignored,
    /// The bar's, setting nothing: a hover, a caret, a list opened or closed.
    Taken,
    /// A setting took a new value, which the options already hold.
    Changed,
    /// The keyboard walked off the bar: past its last setting when
    /// `forward`, before its first otherwise.
    Left {
        /// Which way it walked.
        forward: bool,
    },
}

impl BarOutcome {
    /// The weightier of two outcomes of one event: a change over a take over
    /// nothing.
    fn and(self, other: Self) -> Self {
        let weight = |outcome: Self| match outcome {
            Self::Ignored => 0,
            Self::Taken => 1,
            Self::Changed | Self::Left { .. } => 2,
        };
        if weight(other) > weight(self) {
            other
        } else {
            self
        }
    }
}

/// Where the bar's parts are drawn: the one geometry its paint, its hit
/// tests and its tips read, resolved when the window is laid out.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Placement {
    bounds: Rect,
    /// The window an open list must fit in.
    viewport: Rect,
    caption: Rect,
    /// Each setting's parts, in order; `None` for one the bar has no room
    /// for.
    seats: [Option<Seat>; MOST_SETTINGS],
}

/// Where one setting's parts are drawn.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Seat {
    /// The label, control and unit together.
    cell: Rect,
    label: Rect,
    control: Rect,
    unit: Rect,
}

impl Placement {
    /// The bar's whole band.
    #[must_use]
    pub const fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Where setting `index`'s control is drawn, if the bar seats it.
    #[must_use]
    pub fn control(&self, index: usize) -> Option<Rect> {
        self.seat(index).map(|seat| seat.control)
    }

    fn seat(&self, index: usize) -> Option<Seat> {
        self.seats.get(index).copied().flatten()
    }

    /// The setting whose label, control or unit `point` is over.
    fn setting_at(&self, point: Point) -> Option<usize> {
        self.seats
            .iter()
            .position(|seat| seat.is_some_and(|seat| seat.cell.contains(point)))
    }
}

/// One setting of the bar and the control that edits it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Item {
    setting: Setting,
    control: Control,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Control {
    Number(NumberField),
    Choice(ComboBox),
    Switch(Checkbox),
}

/// The widths a setting's parts take.
#[derive(Copy, Clone, Debug)]
struct Widths {
    label: u32,
    control: u32,
    unit: u32,
}

impl Widths {
    /// The whole cell: the parts, `near` apart.
    fn total(self, near: u32) -> u32 {
        let apart = |width: u32| if width == 0 { 0 } else { width + near };
        apart(self.label) + self.control + apart(self.unit)
    }
}

/// The tool-controls bar.
///
/// Equal bars draw the same pixels: the controls compare as they draw.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolControls {
    tool: Tool,
    items: Vec<Item>,
    /// The setting holding the keyboard.
    focus: Option<usize>,
}

impl ToolControls {
    /// The bar for `tool`, its settings showing `options`; what lays part
    /// of a pixel — smoothing, a tip's hardness, opacity and flow — is
    /// offered only where `partial_allowed`, a palette picture's pixels being
    /// one colour each.
    #[must_use]
    pub fn new(tool: Tool, options: Options, partial_allowed: bool) -> Self {
        let items = tool
            .settings()
            .iter()
            .map(|&setting| Item {
                setting,
                control: Control::of(tool, setting, options),
            })
            .collect();
        let mut bar = Self {
            tool,
            items,
            focus: None,
        };
        bar.allow_partial(partial_allowed, options);
        bar
    }

    /// The settings the bar holds, in order.
    pub fn settings(&self) -> impl Iterator<Item = Setting> + '_ {
        self.items.iter().map(|item| item.setting)
    }

    /// Offer what lays part of a pixel, or hold it off, as the picture's
    /// pixels allow, answering whether any control changed. A control held
    /// off gives up the keyboard; its owner repaints the bar.
    pub fn allow_partial(&mut self, allowed: bool, options: Options) -> bool {
        let mut changed = false;
        for (index, item) in self.items.iter_mut().enumerate() {
            if !item.setting.lays_part() {
                continue;
            }
            if !allowed && self.focus == Some(index) {
                item.control.set_focused(false);
                self.focus = None;
            }
            match &mut item.control {
                Control::Switch(check) => {
                    let mut state = check.state();
                    state.enabled = allowed;
                    let on = options.switch(item.setting).unwrap_or(false);
                    let selection = selection(on && allowed);
                    if state != check.state() || selection != check.selection() {
                        check.set_state(state);
                        check.set_selection(selection);
                        changed = true;
                    }
                }
                Control::Number(field) => {
                    let mut state = field.state();
                    state.enabled = allowed;
                    if state != field.state() {
                        field.set_state(state);
                        changed = true;
                    }
                }
                Control::Choice(_) => {}
            }
        }
        changed
    }

    /// The width the bar needs to seat its name and every setting in one
    /// row.
    #[must_use]
    pub fn natural_width(&self, faces: Faces, scale: Scale, theme: &Theme) -> u32 {
        let (gap, near) = spacing(scale, theme);
        self.items
            .iter()
            .fold(faces.heading.text_width(self.tool.name()), |width, item| {
                width
                    .saturating_add(gap * 2)
                    .saturating_add(item.widths(faces.label, scale, theme).total(near))
            })
    }

    /// The least width every tool's bar seats each of its settings in,
    /// however many rows it takes: what a window is floored on.
    #[must_use]
    pub fn least_width(faces: Faces, scale: Scale, theme: &Theme) -> u32 {
        let (_, near) = spacing(scale, theme);
        Tool::ALL
            .iter()
            .map(|&tool| {
                let bar = Self::new(tool, Options::default(), true);
                bar.items
                    .iter()
                    .map(|item| item.widths(faces.label, scale, theme).total(near))
                    .fold(faces.heading.text_width(tool.name()), u32::max)
            })
            .max()
            .unwrap_or(0)
    }

    /// The most rows any tool's bar takes across `width`.
    #[must_use]
    pub fn most_rows(width: u32, faces: Faces, scale: Scale, theme: &Theme) -> u32 {
        Tool::ALL
            .iter()
            .map(|&tool| Self::new(tool, Options::default(), true).rows(width, faces, scale, theme))
            .max()
            .unwrap_or(1)
    }

    /// The rows the bar takes across `width`: its name, then its settings
    /// left to right, a setting with no room left in its row starting the
    /// next.
    #[must_use]
    pub fn rows(&self, width: u32, faces: Faces, scale: Scale, theme: &Theme) -> u32 {
        self.flow(width, faces, scale, theme, |_, _, _, _| {})
    }

    /// Walk the settings as they are set out across `width`, handing `seat`
    /// each one's index, row, first column and widths, and answer the rows
    /// taken; a setting wider than a whole row ends the walk.
    fn flow(
        &self,
        width: u32,
        faces: Faces,
        scale: Scale,
        theme: &Theme,
        mut seat: impl FnMut(usize, u32, u32, Widths),
    ) -> u32 {
        let (gap, near) = spacing(scale, theme);
        let mut row = 0;
        let mut used = faces.heading.text_width(self.tool.name());
        for (index, item) in self.items.iter().enumerate() {
            let widths = item.widths(faces.label, scale, theme);
            let total = widths.total(near);
            let mut start = used.saturating_add(gap * 2);
            if start.saturating_add(total) > width {
                if total > width {
                    break;
                }
                row += 1;
                start = 0;
            }
            seat(index, row, start, widths);
            used = start + total;
        }
        row + 1
    }

    /// Lay the bar out in `bounds`, rows of a control's height a gap apart:
    /// its name, then each setting whole, left to right, wrapping into the
    /// rows the bounds hold; an open list fits `viewport`.
    #[must_use]
    pub fn place(
        &self,
        bounds: Rect,
        viewport: Rect,
        faces: Faces,
        scale: Scale,
        theme: &Theme,
    ) -> Placement {
        let (gap, near) = spacing(scale, theme);
        let height = Button::height(scale, theme);
        let mut seats = [None; MOST_SETTINGS];
        self.flow(
            bounds.width,
            faces,
            scale,
            theme,
            |index, row, start, widths| {
                let top = bounds
                    .top()
                    .saturating_add_unsigned(row.saturating_mul(height + gap));
                if top.saturating_add_unsigned(height) > bounds.bottom() {
                    return;
                }
                let left = bounds.left().saturating_add_unsigned(start);
                let cell = Rect::new(left, top, widths.total(near), height);
                let mut parts = cell;
                let label = parts.take_left(widths.label);
                if widths.label > 0 {
                    let _ = parts.take_left(near);
                }
                let control = parts.take_left(widths.control);
                if widths.unit > 0 {
                    let _ = parts.take_left(near);
                }
                if let Some(slot) = seats.get_mut(index) {
                    *slot = Some(Seat {
                        cell,
                        label,
                        control,
                        unit: parts.take_left(widths.unit),
                    });
                }
            },
        );
        let caption = Rect::new(
            bounds.left(),
            bounds.top(),
            faces.heading.text_width(self.tool.name()).min(bounds.width),
            height.min(bounds.height),
        );
        Placement {
            bounds,
            viewport,
            caption,
            seats,
        }
    }

    /// Paint the bar's name, labels, controls and units.
    pub fn render(
        &self,
        surface: &mut Surface,
        placement: &Placement,
        faces: Faces,
        scale: Scale,
        theme: &Theme,
    ) {
        let palette = theme.palette();
        let (ink, quiet) = (
            Color::from(palette.on_surface),
            Color::from(palette.on_surface_muted),
        );
        words(
            surface,
            faces.heading,
            self.tool.name(),
            placement.caption,
            ink,
        );
        for (index, item) in self.items.iter().enumerate() {
            let Some(seat) = placement.seat(index) else {
                continue;
            };
            match &item.control {
                Control::Number(field) => field.render(surface, seat.control, scale, theme),
                Control::Choice(combo) => combo.render(surface, seat.control, scale, theme),
                Control::Switch(check) => check.render(surface, seat.control, scale, theme),
            }
            if !seat.label.is_empty() {
                words(surface, faces.label, item.setting.label(), seat.label, ink);
            }
            words(surface, faces.label, item.setting.unit(), seat.unit, quiet);
        }
    }

    /// Paint an open choice's list over everything else the window draws.
    pub fn render_popup(
        &self,
        surface: &mut Surface,
        placement: &Placement,
        scale: Scale,
        theme: &Theme,
    ) {
        if let Some((combo, popup)) = self.open_list(placement, scale, theme) {
            combo.render_popup(surface, popup, scale, theme);
        }
    }

    /// Where an open choice's list is drawn; empty while none is open.
    #[must_use]
    pub fn popup_rect(&self, placement: &Placement, scale: Scale, theme: &Theme) -> Rect {
        self.open_list(placement, scale, theme)
            .map_or(Rect::EMPTY, |(_, popup)| popup)
    }

    fn open_list(
        &self,
        placement: &Placement,
        scale: Scale,
        theme: &Theme,
    ) -> Option<(&ComboBox, Rect)> {
        self.items.iter().enumerate().find_map(|(index, item)| {
            let Control::Choice(combo) = &item.control else {
                return None;
            };
            let field = placement.control(index).filter(|_| combo.is_expanded())?;
            Some((
                combo,
                combo.popup_rect(field, placement.viewport, scale, theme),
            ))
        })
    }

    /// Whether a choice's list is open, owning the pointer and the keyboard
    /// until it closes.
    #[must_use]
    pub fn listing(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(&item.control, Control::Choice(combo) if combo.is_expanded()))
    }

    /// Whether `at` is over a number field, where the pointer shows text
    /// entry.
    #[must_use]
    pub fn text_at(&self, placement: &Placement, at: Point) -> bool {
        self.items.iter().enumerate().any(|(index, item)| {
            matches!(item.control, Control::Number(_))
                && placement
                    .control(index)
                    .is_some_and(|rect| rect.contains(at))
        })
    }

    /// The setting holding the keyboard, if one does.
    #[must_use]
    pub const fn focus(&self) -> Option<usize> {
        self.focus
    }

    /// The tip for the setting under `at`, and the cell it covers.
    #[must_use]
    pub fn tip(&self, placement: &Placement, at: Point) -> Option<(Rect, &'static str)> {
        let index = placement.setting_at(at)?;
        let item = self.items.get(index)?;
        let held_off = !item.control.enabled();
        let tip = if held_off {
            WHOLE_PIXELS
        } else {
            item.setting.tip()
        };
        Some((placement.seat(index)?.cell, tip))
    }

    /// Feed a pointer event, the pointer at `at`. An open list owns every
    /// event wherever it falls; otherwise each setting the bar seats sees it,
    /// the wheel reaching only the one under the pointer, and a press on a
    /// setting gives it the keyboard.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        at: Point,
        placement: &Placement,
        options: &mut Options,
        style: (Scale, &Theme),
        damage: &mut Region,
    ) -> BarOutcome {
        if let Some(index) = self.open_choice() {
            let outcome = self.feed(index, (event, at), placement, options, style, damage);
            return outcome.and(BarOutcome::Taken);
        }
        let over = placement.setting_at(at);
        let pressed = matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        );
        if pressed {
            if let Some(index) = over.filter(|&index| self.actionable(index)) {
                self.focus_on(index, placement, damage);
            }
        }
        let wheel = matches!(event, InputEvent::PointerScrolled { .. });
        let mut outcome = BarOutcome::Ignored;
        for index in 0..self.items.len() {
            if wheel && over != Some(index) {
                continue;
            }
            let fed = self.feed(index, (event, at), placement, options, style, damage);
            outcome = outcome.and(fed);
        }
        let on_bar = matches!(
            event,
            InputEvent::PointerPressed { .. } | InputEvent::PointerScrolled { .. }
        ) && placement.bounds.contains(at);
        if on_bar {
            outcome = outcome.and(BarOutcome::Taken);
        }
        outcome
    }

    /// Feed a key to the setting holding the keyboard. An open list takes
    /// every key; Tab walks the settings; a number field claims every key but
    /// the owner's chords and an Escape with nothing to take back; a choice
    /// and a switch take what opens or flips them.
    pub fn on_key(
        &mut self,
        (key, modifiers): (Key, Modifiers),
        placement: &Placement,
        options: &mut Options,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> BarOutcome {
        let Some(index) = self.focus else {
            return BarOutcome::Ignored;
        };
        let Some(rect) = placement.control(index) else {
            return BarOutcome::Ignored;
        };
        let viewport = placement.viewport;
        if self.open_choice() == Some(index) {
            let Some(Item {
                setting,
                control: Control::Choice(combo),
            }) = self.items.get_mut(index)
            else {
                return BarOutcome::Taken;
            };
            let popup = combo.popup_rect(rect, viewport, scale, theme);
            return match combo.on_key(key, rect, popup, scale, theme, damage) {
                Some(ComboAction::Selected { index }) => choose(options, *setting, index),
                _ => BarOutcome::Taken,
            };
        }
        if key == Key::Named(NamedKey::Tab) {
            return self.walk(!modifiers.shift, placement, options, damage);
        }
        let tool = self.tool;
        let Some(item) = self.items.get_mut(index) else {
            return BarOutcome::Ignored;
        };
        let setting = item.setting;
        match &mut item.control {
            Control::Number(field) => {
                if owner_chord(key, modifiers) {
                    return BarOutcome::Ignored;
                }
                match field.on_key(key, modifiers, rect, damage) {
                    Some(action) => apply(options, (tool, setting), action),
                    None if key == Key::Named(NamedKey::Escape) => BarOutcome::Ignored,
                    None => BarOutcome::Taken,
                }
            }
            Control::Choice(combo) => {
                let opens = matches!(
                    key,
                    Key::Named(NamedKey::Down | NamedKey::Up | NamedKey::Enter) | Key::Char(' ')
                );
                let popup = if opens {
                    combo.popup_rect(rect, viewport, scale, theme)
                } else {
                    Rect::EMPTY
                };
                match combo.on_key(key, rect, popup, scale, theme, damage) {
                    Some(ComboAction::Selected { index }) => choose(options, setting, index),
                    Some(ComboAction::Opened | ComboAction::Closed) => BarOutcome::Taken,
                    None => BarOutcome::Ignored,
                }
            }
            Control::Switch(check) => match check.on_key(key) {
                Some(SelectorAction::Set { on }) => {
                    switch((options, setting), check, on, rect, damage)
                }
                None => BarOutcome::Ignored,
            },
        }
    }

    /// Give the bar the keyboard on its first setting, or its last when not
    /// `forward`; `false` where it seats none.
    pub fn enter_focus(
        &mut self,
        forward: bool,
        placement: &Placement,
        damage: &mut Region,
    ) -> bool {
        match self.next_seated(None, forward, placement) {
            Some(index) => {
                self.focus_on(index, placement, damage);
                true
            }
            None => false,
        }
    }

    /// Settle what the setting holding the keyboard holds — a field's
    /// typing — keeping it there: done before anything else acts.
    pub fn commit(&mut self, placement: &Placement, options: &mut Options, damage: &mut Region) {
        let Some(index) = self.focus else {
            return;
        };
        let (Some(item), Some(rect)) = (self.items.get_mut(index), placement.control(index)) else {
            return;
        };
        if let Control::Number(field) = &mut item.control {
            if let Some(action) = field.commit(rect, damage) {
                apply(options, (self.tool, item.setting), action);
            }
        }
    }

    /// Settle the bar and give up the keyboard, as a press elsewhere in the
    /// window or a menu does.
    pub fn blur(&mut self, placement: &Placement, options: &mut Options, damage: &mut Region) {
        self.commit(placement, options, damage);
        self.unfocus(placement, damage);
    }

    /// Move the keyboard on from the setting holding it, settling it; off the
    /// bar past either end.
    fn walk(
        &mut self,
        forward: bool,
        placement: &Placement,
        options: &mut Options,
        damage: &mut Region,
    ) -> BarOutcome {
        self.commit(placement, options, damage);
        if let Some(index) = self.next_seated(self.focus, forward, placement) {
            self.focus_on(index, placement, damage);
            BarOutcome::Taken
        } else {
            self.unfocus(placement, damage);
            BarOutcome::Left { forward }
        }
    }

    /// The seated setting that can act after `from`, or before it when not
    /// `forward`; the first or last with nothing yet holding the keyboard.
    fn next_seated(
        &self,
        from: Option<usize>,
        forward: bool,
        placement: &Placement,
    ) -> Option<usize> {
        let seated = |index: &usize| placement.seat(*index).is_some() && self.actionable(*index);
        let count = self.items.len();
        match (from, forward) {
            (None, true) => (0..count).find(seated),
            (None, false) => (0..count).rev().find(seated),
            (Some(at), true) => (at + 1..count).find(seated),
            (Some(at), false) => (0..at).rev().find(seated),
        }
    }

    /// Whether setting `index` can act: smoothing held off cannot.
    fn actionable(&self, index: usize) -> bool {
        self.items.get(index).is_some_and(|item| {
            match &item.control {
                Control::Number(field) => field.state(),
                Control::Choice(combo) => combo.state(),
                Control::Switch(check) => check.state(),
            }
            .is_actionable()
        })
    }

    fn focus_on(&mut self, index: usize, placement: &Placement, damage: &mut Region) {
        if self.focus == Some(index) {
            return;
        }
        self.unfocus(placement, damage);
        if let Some(item) = self.items.get_mut(index) {
            item.control.set_focused(true);
            self.focus = Some(index);
            if let Some(seat) = placement.seat(index) {
                damage.add(seat.control);
            }
        }
    }

    fn unfocus(&mut self, placement: &Placement, damage: &mut Region) {
        let Some(index) = self.focus.take() else {
            return;
        };
        if let Some(item) = self.items.get_mut(index) {
            item.control.set_focused(false);
        }
        if let Some(seat) = placement.seat(index) {
            damage.add(seat.control);
        }
    }

    fn open_choice(&self) -> Option<usize> {
        self.items
            .iter()
            .position(|item| matches!(&item.control, Control::Choice(combo) if combo.is_expanded()))
    }

    /// Feed `event` to setting `index`'s control, landing what it asked for
    /// on the options.
    fn feed(
        &mut self,
        index: usize,
        (event, at): (&InputEvent, Point),
        placement: &Placement,
        options: &mut Options,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> BarOutcome {
        let viewport = placement.viewport;
        let tool = self.tool;
        let (Some(item), Some(rect)) = (self.items.get_mut(index), placement.control(index)) else {
            return BarOutcome::Ignored;
        };
        let setting = item.setting;
        match &mut item.control {
            Control::Number(field) => match field.on_pointer(event, rect, scale, theme, damage) {
                Some(action) => apply(options, (tool, setting), action),
                None => BarOutcome::Ignored,
            },
            Control::Choice(combo) => {
                // Placing the list measures every choice, so it is placed only
                // where it is open or a release on the field may open it.
                let opening =
                    matches!(event, InputEvent::PointerReleased { .. }) && rect.contains(at);
                let popup = if combo.is_expanded() || opening {
                    combo.popup_rect(rect, viewport, scale, theme)
                } else {
                    Rect::EMPTY
                };
                match combo.on_pointer(event, rect, popup, scale, theme, damage) {
                    Some(ComboAction::Selected { index }) => choose(options, setting, index),
                    Some(ComboAction::Opened | ComboAction::Closed) => BarOutcome::Taken,
                    None => BarOutcome::Ignored,
                }
            }
            Control::Switch(check) => match check.on_pointer(event, rect, damage) {
                Some(SelectorAction::Set { on }) => {
                    switch((options, setting), check, on, rect, damage)
                }
                None => BarOutcome::Ignored,
            },
        }
    }
}

impl Item {
    fn widths(&self, font: BitmapFont, scale: Scale, theme: &Theme) -> Widths {
        let measure = |text: &str| {
            if text.is_empty() {
                0
            } else {
                font.text_width(text)
            }
        };
        match &self.control {
            Control::Number(field) => Widths {
                label: measure(self.setting.label()),
                control: field.preferred_width(scale, theme),
                unit: measure(self.setting.unit()),
            },
            Control::Choice(combo) => Widths {
                label: measure(self.setting.label()),
                control: combo.measured_width(scale, theme),
                unit: 0,
            },
            // A switch names itself.
            Control::Switch(check) => Widths {
                label: 0,
                control: check.measured_width(scale, theme),
                unit: 0,
            },
        }
    }
}

impl Control {
    fn of(tool: Tool, setting: Setting, options: Options) -> Self {
        match setting {
            Setting::Style | Setting::Marquee | Setting::Combine | Setting::Gradient => {
                Self::Choice(
                    ComboBox::new(
                        setting
                            .choices()
                            .iter()
                            .map(|&choice| String::from(choice))
                            .collect(),
                    )
                    .with_selected(options.choice(setting).unwrap_or(0)),
                )
            }
            switch if switch.is_switch() => Self::Switch(Checkbox::new(
                switch.label(),
                selection(options.switch(switch).unwrap_or(false)),
            )),
            number => {
                let (least, most) = number.bounds().unwrap_or((0, 0));
                let (line, page) = number.steps();
                Self::Number(
                    NumberField::new(options.number(tool, number).unwrap_or(least), least, most)
                        .with_steps(line, page),
                )
            }
        }
    }

    fn set_focused(&mut self, focused: bool) {
        match self {
            Self::Number(field) => field.set_focused(focused),
            Self::Choice(combo) => combo.set_focused(focused),
            Self::Switch(check) => check.set_focused(focused),
        }
    }

    fn enabled(&self) -> bool {
        match self {
            Self::Number(field) => field.state().enabled,
            Self::Choice(combo) => combo.state().enabled,
            Self::Switch(check) => check.state().enabled,
        }
    }
}

/// The checkbox mark for `on`.
const fn selection(on: bool) -> SelectionState {
    if on {
        SelectionState::Selected
    } else {
        SelectionState::Unselected
    }
}

/// The gap between settings, and the nearer one between a setting's parts.
fn spacing(scale: Scale, theme: &Theme) -> (u32, u32) {
    let gap = scale.scale_length(theme.metrics().control_gap).max(1);
    (gap, (gap / 2).max(1))
}

/// Land a number field's value on `tool`'s options.
fn apply(
    options: &mut Options,
    (tool, setting): (Tool, Setting),
    action: NumberAction,
) -> BarOutcome {
    let (NumberAction::Edited { value } | NumberAction::Settled { value }) = action;
    let before = options.number(tool, setting);
    options.set_number(tool, setting, value);
    if options.number(tool, setting) == before {
        BarOutcome::Taken
    } else {
        BarOutcome::Changed
    }
}

/// Land choice `index` of `setting` on the options.
fn choose(options: &mut Options, setting: Setting, index: usize) -> BarOutcome {
    if options.set_choice(setting, index) {
        BarOutcome::Changed
    } else {
        BarOutcome::Taken
    }
}

/// Land a switch's flip on the options and show it.
fn switch(
    (options, setting): (&mut Options, Setting),
    check: &mut Checkbox,
    on: bool,
    rect: Rect,
    damage: &mut Region,
) -> BarOutcome {
    options.set_switch(setting, on);
    check.set_selection(selection(on));
    damage.add(rect);
    BarOutcome::Changed
}

/// Draw `text` at the start of `rect`, centred down it, cut short with the
/// shared mark where it is wider.
fn words(surface: &mut Surface, font: BitmapFont, text: &str, rect: Rect, colour: Color) {
    if text.is_empty() || rect.is_empty() {
        return;
    }
    let run = font.elide_to_width(text, rect.width);
    let y = font.centred_top(rect.top(), rect.height);
    paint_run(surface, font, run, (rect.left(), y), colour, None);
}

#[cfg(test)]
#[path = "tool_controls_tests.rs"]
mod tests;
