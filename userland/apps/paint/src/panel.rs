//! A pane's settings: a stack of parts down the pane's width — a choice, a
//! number with its slider or track, fields side by side, buttons, a switch, a
//! track of handles, a line of note, or a part its owner draws — laid out by
//! the one routine the paint, every hit test and the keyboard read.
//!
//! Every control sees every pointer event, so a hover leaves and a press held
//! on one control ends there wherever it is let go. A number typed applies as
//! it spells one in range; a step, Enter and the keyboard leaving settle it.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_controls::{
    owner_chord, Button, ButtonAction, Checkbox, ComboAction, ComboBox, Keystroke, NumberAction,
    NumberField, SelectorAction, Slider, SliderAction,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use crate::filter::Parameter;
use crate::layout::Faces;
use crate::tool_controls::{selection, spacing, words};
use crate::track::{Track, TRACK_HEIGHT};

/// The most controls a part sets side by side.
pub const MOST_CELLS: usize = 4;

/// The most parts a panel holds; past it a part is left off.
pub const MOST_PARTS: usize = 16;

/// The art of a panel with nothing of its owner's to draw.
pub struct Plain;

impl PanelArt for Plain {
    fn draw(&self, _: &mut Surface, _: usize, _: Rect) {}

    fn sweeps(&self, _: usize) -> bool {
        false
    }

    fn sweep(&self, _: usize, _: u32) -> Color {
        Color::rgba(0, 0, 0, 0)
    }
}

/// What a panel's owner draws for it.
pub trait PanelArt {
    /// Draw custom part `part` at `rect`.
    fn draw(&self, surface: &mut Surface, part: usize, rect: Rect);

    /// Whether track part `part`'s groove is swept with colours.
    fn sweeps(&self, part: usize) -> bool;

    /// The colour track part `part`'s groove shows `along` thousandths of
    /// the way.
    fn sweep(&self, part: usize, along: u32) -> Color;
}

/// How tall a part its owner draws stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Height {
    /// A fraction of the pane's width: `numerator / denominator` of it.
    Ratio(u32, u32),
    /// So many logical pixels.
    Fixed(u32),
}

/// One captioned field of a [`Part::Fields`] row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cell {
    /// The caption above the field.
    pub label: &'static str,
    /// The field.
    pub field: NumberField,
}

/// What a number part is dragged on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Slide {
    /// A slider.
    Slider(Slider),
    /// A track of one handle, its groove swept by the owner's colours.
    Track(Track),
}

impl Slide {
    /// Stand it at `value` of `parameter`.
    fn show(&mut self, parameter: &Parameter, value: i32) {
        match self {
            Self::Slider(slider) => slider.set_value(parameter.permille_of(value)),
            Self::Track(track) => {
                track.set(0, value);
            }
        }
    }

    fn set_focused(&mut self, focused: bool) {
        match self {
            Self::Slider(slider) => slider.set_focused(focused),
            Self::Track(track) => track.focus(focused.then_some(0)),
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        match self {
            Self::Slider(slider) => {
                let mut state = slider.state();
                state.enabled = enabled;
                slider.set_state(state);
            }
            Self::Track(track) => track.set_enabled(enabled),
        }
    }
}

/// One part of a panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Part {
    /// A caption and a list to choose from.
    Choice {
        /// What it chooses.
        label: &'static str,
        /// The list.
        combo: ComboBox,
    },
    /// A number captioned and typed in its field, with a slider or a track
    /// beneath to drag it on.
    Number {
        /// Its name and bounds.
        parameter: Parameter,
        /// The field it is typed in.
        field: NumberField,
        /// What it is dragged on.
        slide: Slide,
    },
    /// Fields side by side, each captioned above.
    Fields(Vec<Cell>),
    /// Buttons side by side.
    Buttons(Vec<Button>),
    /// A switch.
    Switch(Checkbox),
    /// A track of handles.
    Track(Track),
    /// A part its owner draws and takes input for.
    Custom {
        /// How tall it stands.
        height: Height,
        /// Whether it takes the keyboard.
        focusable: bool,
    },
    /// A line of quiet text.
    Note(String),
}

impl Part {
    /// A number part for `parameter`, holding `value`, on a slider.
    #[must_use]
    pub fn number(parameter: Parameter, value: i32) -> Self {
        let (line, page) = parameter.steps();
        Self::Number {
            parameter,
            field: NumberField::new(value, parameter.least, parameter.most),
            slide: Slide::Slider(Slider::new(parameter.permille_of(value)).with_steps(line, page)),
        }
    }

    /// A number part on a slider whose ends are named `ends`.
    #[must_use]
    pub fn between(parameter: Parameter, value: i32, (start, end): (&str, &str)) -> Self {
        let (line, page) = parameter.steps();
        let slider = Slider::new(parameter.permille_of(value))
            .with_steps(line, page)
            .with_ends(start, end);
        Self::Number {
            parameter,
            field: NumberField::new(value, parameter.least, parameter.most),
            slide: Slide::Slider(slider),
        }
    }

    /// A number part on a track of one handle filled `handle`.
    #[must_use]
    pub fn swept(parameter: Parameter, value: i32, handle: Color) -> Self {
        Self::Number {
            parameter,
            field: NumberField::new(value, parameter.least, parameter.most),
            slide: Slide::Track(Track::new(
                parameter.least,
                parameter.most,
                &[(value, handle)],
            )),
        }
    }

    /// A list part captioned `label`, of `choices`, `selected` chosen.
    #[must_use]
    pub fn choice(label: &'static str, choices: &[&str], selected: usize) -> Self {
        Self::Choice {
            label,
            combo: ComboBox::new(choices.iter().map(|choice| String::from(*choice)).collect())
                .with_selected(selected),
        }
    }

    /// Show `value` in control `cell` without reporting it: a number's field
    /// and what it is dragged on, a row's field, a track's handle, a list's
    /// choice, or a switch, on (non-zero) or off. The owner reports the
    /// repaint.
    pub fn show(&mut self, cell: usize, value: i32) {
        match self {
            Self::Number {
                parameter,
                field,
                slide,
            } => {
                field.set_value(value);
                slide.show(parameter, value);
            }
            Self::Fields(row) => {
                if let Some(entry) = row.get_mut(cell) {
                    entry.field.set_value(value);
                }
            }
            Self::Track(track) => {
                track.set(cell, value);
            }
            Self::Choice { combo, .. } => {
                if let Ok(index) = usize::try_from(value) {
                    combo.set_selected(index);
                }
            }
            Self::Switch(check) => check.set_selection(selection(value != 0)),
            Self::Buttons(_) | Self::Custom { .. } | Self::Note(_) => {}
        }
    }

    /// How many keyboard stops it holds.
    fn stops(&self) -> usize {
        match self {
            Self::Choice { .. } | Self::Switch(_) => 1,
            Self::Number { .. } => 2,
            Self::Fields(cells) => cells.len(),
            Self::Buttons(buttons) => buttons.len(),
            Self::Track(track) => track.count(),
            Self::Custom { focusable, .. } => usize::from(*focusable),
            Self::Note(_) => 0,
        }
    }
}

/// Where a part's pieces are drawn.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Placed {
    /// The whole part.
    pub rect: Rect,
    /// Each caption: a choice's or a number's first, a field row's each.
    pub labels: [Rect; MOST_CELLS],
    /// Each control: a number's field then its slider, a row's each field or
    /// button, any other part's one.
    pub controls: [Rect; MOST_CELLS],
}

/// What an input to the panel came to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PanelEvent {
    /// Choice `index` of part `part`'s list was chosen.
    Chosen {
        /// The part.
        part: usize,
        /// The choice.
        index: usize,
    },
    /// Number `cell` of part `part` became `value`: a number part's field or
    /// slider (cell `0`), or a field of a row.
    Number {
        /// The part.
        part: usize,
        /// The field of a row; `0` for a number part.
        cell: usize,
        /// The value.
        value: i32,
        /// Whether the interaction is over.
        settled: bool,
    },
    /// Button `button` of part `part` was pressed.
    Pressed {
        /// The part.
        part: usize,
        /// The button.
        button: usize,
    },
    /// Part `part`'s switch asks to be `on`.
    Switched {
        /// The part.
        part: usize,
        /// What it asks to be.
        on: bool,
    },
    /// Handle `handle` of part `part`'s track was asked to stand at `value`.
    Moved {
        /// The part.
        part: usize,
        /// The handle.
        handle: usize,
        /// Where.
        value: i32,
        /// Whether the move is over.
        settled: bool,
    },
}

/// What feeding the panel an input came to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PanelOutcome {
    /// None of the panel's.
    Ignored,
    /// The panel's, setting nothing: a hover, a caret, a list opened.
    Taken,
    /// A control asked for a value.
    Event(PanelEvent),
    /// The event is the owner's part `part`'s, drawn at `rect`.
    Custom {
        /// The part.
        part: usize,
        /// Where it is drawn.
        rect: Rect,
    },
    /// The keyboard walked off the panel: past its last stop when `forward`.
    Left {
        /// Which way.
        forward: bool,
    },
}

/// The lengths a panel is laid out by.
#[derive(Copy, Clone, Debug)]
struct Measures {
    gap: u32,
    near: u32,
    row: u32,
    caption: u32,
    button: u32,
    track: u32,
    scale: Scale,
}

impl Measures {
    fn of(faces: Faces, scale: Scale, theme: &Theme) -> Self {
        let (gap, near) = spacing(scale, theme);
        Self {
            gap,
            near,
            row: NumberField::height(scale, theme),
            caption: faces.status.line_height().max(1),
            button: Button::height(scale, theme),
            track: scale.scale_length(TRACK_HEIGHT),
            scale,
        }
    }

    fn height(self, part: &Part, width: u32) -> u32 {
        match part {
            Part::Choice { .. } | Part::Switch(_) => self.row,
            Part::Number { slide, .. } => {
                let under = match slide {
                    Slide::Slider(_) => self.row,
                    Slide::Track(_) => self.track,
                };
                self.row + self.near + under
            }
            Part::Fields(_) => self.caption + self.row,
            Part::Buttons(_) => self.button,
            Part::Track(_) => self.track,
            Part::Custom { height, .. } => match *height {
                Height::Ratio(numerator, denominator) => u32::try_from(
                    u64::from(width) * u64::from(numerator) / u64::from(denominator.max(1)),
                )
                .unwrap_or(width),
                Height::Fixed(length) => self.scale.scale_length(length),
            },
            Part::Note(_) => self.caption,
        }
    }
}

/// `width` split into `count` cells `near` apart, the remainder spread from
/// the left.
fn cells(area: Rect, count: usize, near: u32) -> [Rect; MOST_CELLS] {
    let mut out = [Rect::EMPTY; MOST_CELLS];
    let count = count.clamp(1, MOST_CELLS);
    let count_u32 = u32::try_from(count).unwrap_or(1);
    let room = area.width.saturating_sub(near * (count_u32 - 1));
    let (each, spare) = (room / count_u32, room % count_u32);
    let mut left = area.left();
    for (index, slot) in out.iter_mut().take(count).enumerate() {
        let width = each + u32::from(u32::try_from(index).unwrap_or(0) < spare);
        *slot = Rect::new(left, area.top(), width, area.height);
        left = left.saturating_add_unsigned(width + near);
    }
    out
}

/// A pane's stack of settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Panel {
    parts: Vec<Part>,
    /// The part and stop holding the keyboard.
    focus: Option<(usize, usize)>,
    /// The part a press is held on.
    held: Option<usize>,
    /// Whether the panel is withheld.
    withheld: bool,
    pointer: Point,
}

impl Panel {
    /// A panel of `parts`, top down, the first [`MOST_PARTS`] of them.
    #[must_use]
    pub fn new(mut parts: Vec<Part>) -> Self {
        parts.truncate(MOST_PARTS);
        Self {
            parts,
            focus: None,
            held: None,
            withheld: false,
            pointer: Point::ORIGIN,
        }
    }

    /// The parts, top down.
    #[must_use]
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// Part `index`, to change; the owner reports the repaint.
    pub fn part_mut(&mut self, index: usize) -> Option<&mut Part> {
        self.parts.get_mut(index)
    }

    /// How tall the panel stands across `width`.
    #[must_use]
    pub fn measured_height(&self, width: u32, faces: Faces, scale: Scale, theme: &Theme) -> u32 {
        let measures = Measures::of(faces, scale, theme);
        let count = u32::try_from(self.parts.len()).unwrap_or(0);
        self.parts
            .iter()
            .map(|part| measures.height(part, width))
            .sum::<u32>()
            .saturating_add(measures.gap * count.saturating_sub(1))
    }

    /// Each part as laid out down `bounds`, top first. Bounds shorter than
    /// the panel take the room from its owner-drawn parts first, each giving
    /// up its share down to a quarter of its height, so a graph shrinks
    /// before a control is lost; a part still past the foot is left out.
    pub fn placed<'a>(
        &'a self,
        bounds: Rect,
        faces: Faces,
        scale: Scale,
        theme: &'a Theme,
    ) -> impl Iterator<Item = (usize, &'a Part, Placed)> + 'a {
        let measures = Measures::of(faces, scale, theme);
        let width = bounds.width;
        let flexible: u32 = self
            .parts
            .iter()
            .filter(|part| matches!(part, Part::Custom { .. }))
            .map(|part| measures.height(part, width))
            .sum();
        let deficit = self
            .measured_height(width, faces, scale, theme)
            .saturating_sub(bounds.height);
        let mut top = bounds.top();
        self.parts
            .iter()
            .enumerate()
            .map_while(move |(index, part)| {
                let mut height = measures.height(part, width);
                if matches!(part, Part::Custom { .. }) && flexible > 0 {
                    let share = u64::from(deficit) * u64::from(height) / u64::from(flexible);
                    let given_up = u32::try_from(share)
                        .unwrap_or(u32::MAX)
                        .min(height - height / 4);
                    height -= given_up;
                }
                if top.saturating_add_unsigned(height) > bounds.bottom() {
                    return None;
                }
                let rect = Rect::new(bounds.left(), top, bounds.width, height);
                top = top.saturating_add_unsigned(height + measures.gap);
                Some((
                    index,
                    part,
                    Self::place(part, rect, measures, (faces, theme)),
                ))
            })
    }

    /// Each part laid out down `bounds`, into a list on the stack: what an
    /// input reads while it changes the parts.
    fn placements(&self, bounds: Rect, faces: Faces, scale: Scale, theme: &Theme) -> Placements {
        let mut placements = Placements {
            slots: [(0, Placed::default()); MOST_PARTS],
            count: 0,
        };
        for (index, _, placed) in self.placed(bounds, faces, scale, theme) {
            if let Some(slot) = placements.slots.get_mut(placements.count) {
                *slot = (index, placed);
                placements.count += 1;
            }
        }
        placements
    }

    fn place(
        part: &Part,
        rect: Rect,
        measures: Measures,
        (faces, theme): (Faces, &Theme),
    ) -> Placed {
        let mut placed = Placed {
            rect,
            ..Placed::default()
        };
        match part {
            Part::Choice { label, .. } => {
                let mut remaining = rect;
                let caption = faces.label.text_width(label).min(remaining.width / 2);
                placed.labels[0] = remaining.take_left(caption);
                let _ = remaining.take_left(measures.near);
                placed.controls[0] = remaining;
            }
            Part::Number { field, .. } => {
                let mut remaining = rect;
                let mut line = remaining.take_top(measures.row);
                let _ = remaining.take_top(measures.near);
                let wanted = field
                    .preferred_width(measures.scale, theme)
                    .max(measures.row * 2);
                placed.controls[0] = line.take_right(wanted.min(line.width / 2));
                let _ = line.take_right(measures.near);
                placed.labels[0] = line;
                placed.controls[1] = remaining;
            }
            Part::Fields(row) => {
                for (index, cell) in cells(rect, row.len(), measures.near)
                    .iter()
                    .enumerate()
                    .take(row.len())
                {
                    let mut remaining = *cell;
                    placed.labels[index] = remaining.take_top(measures.caption);
                    placed.controls[index] = remaining;
                }
            }
            Part::Buttons(row) => {
                placed.controls = cells(rect, row.len(), measures.near);
            }
            Part::Switch(_) | Part::Track(_) | Part::Custom { .. } | Part::Note(_) => {
                placed.controls[0] = rect;
            }
        }
        placed
    }

    /// Where part `index` is laid out down `bounds`, if it is.
    #[must_use]
    pub fn place_of(
        &self,
        index: usize,
        bounds: Rect,
        faces: Faces,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Placed> {
        self.placed(bounds, faces, scale, theme)
            .find(|&(at, _, _)| at == index)
            .map(|(_, _, placed)| placed)
    }

    /// Paint the panel down `bounds`; `art` draws its custom parts and its
    /// tracks' sweeps.
    pub fn render(
        &self,
        surface: &mut Surface,
        bounds: Rect,
        (faces, scale, theme): (Faces, Scale, &Theme),
        art: &dyn PanelArt,
    ) {
        let palette = theme.palette();
        let (ink, quiet) = (
            Color::from(palette.on_surface),
            Color::from(palette.on_surface_muted),
        );
        let caption_ink = if self.withheld { quiet } else { ink };
        for (index, part, placed) in self.placed(bounds, faces, scale, theme) {
            match part {
                Part::Choice { label, combo } => {
                    words(surface, faces.label, label, placed.labels[0], caption_ink);
                    combo.render(surface, placed.controls[0], scale, theme);
                }
                Part::Number {
                    parameter,
                    field,
                    slide,
                } => {
                    words(
                        surface,
                        faces.label,
                        parameter.label,
                        placed.labels[0],
                        caption_ink,
                    );
                    field.render(surface, placed.controls[0], scale, theme);
                    match slide {
                        Slide::Slider(slider) => {
                            slider.render(surface, placed.controls[1], scale, theme);
                        }
                        Slide::Track(track) => {
                            let sweep = |along| art.sweep(index, along);
                            let sweep: Option<&dyn Fn(u32) -> Color> = if art.sweeps(index) {
                                Some(&sweep)
                            } else {
                                None
                            };
                            track.render(surface, placed.controls[1], scale, theme, sweep);
                        }
                    }
                }
                Part::Fields(row) => {
                    for (cell, (label, control)) in
                        row.iter().zip(placed.labels.iter().zip(&placed.controls))
                    {
                        words(surface, faces.status, cell.label, *label, quiet);
                        cell.field.render(surface, *control, scale, theme);
                    }
                }
                Part::Buttons(row) => {
                    for (button, control) in row.iter().zip(&placed.controls) {
                        button.render(surface, *control, scale, theme);
                    }
                }
                Part::Switch(check) => check.render(surface, placed.controls[0], scale, theme),
                Part::Track(track) => {
                    let sweep = |along| art.sweep(index, along);
                    let sweep: Option<&dyn Fn(u32) -> Color> = if art.sweeps(index) {
                        Some(&sweep)
                    } else {
                        None
                    };
                    track.render(surface, placed.controls[0], scale, theme, sweep);
                }
                Part::Custom { .. } => art.draw(surface, index, placed.controls[0]),
                Part::Note(text) => words(surface, faces.status, text, placed.controls[0], quiet),
            }
        }
    }

    /// Paint an open list over everything else the window draws.
    pub fn render_popup(
        &self,
        surface: &mut Surface,
        bounds: Rect,
        viewport: Rect,
        (faces, scale, theme): (Faces, Scale, &Theme),
    ) {
        if let Some((combo, popup)) = self.open_list(bounds, viewport, faces, scale, theme) {
            combo.render_popup(surface, popup, scale, theme);
        }
    }

    /// The open list and where it is drawn.
    fn open_list<'a>(
        &'a self,
        bounds: Rect,
        viewport: Rect,
        faces: Faces,
        scale: Scale,
        theme: &'a Theme,
    ) -> Option<(&'a ComboBox, Rect)> {
        self.placed(bounds, faces, scale, theme)
            .find_map(|(_, part, placed)| match part {
                Part::Choice { combo, .. } if combo.is_expanded() => Some((
                    combo,
                    combo.popup_rect(placed.controls[0], viewport, scale, theme),
                )),
                _ => None,
            })
    }

    /// Where an open list is drawn; empty while none is.
    #[must_use]
    pub fn popup_rect(
        &self,
        bounds: Rect,
        viewport: Rect,
        faces: Faces,
        scale: Scale,
        theme: &Theme,
    ) -> Rect {
        self.open_list(bounds, viewport, faces, scale, theme)
            .map_or(Rect::EMPTY, |(_, popup)| popup)
    }

    /// Whether a list is open, owning the pointer and the keyboard until it
    /// closes.
    #[must_use]
    pub fn listing(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, Part::Choice { combo, .. } if combo.is_expanded()))
    }

    /// Whether a press is held on a part.
    #[must_use]
    pub const fn holding(&self) -> bool {
        self.held.is_some()
    }

    /// The part and stop holding the keyboard.
    #[must_use]
    pub const fn focus(&self) -> Option<(usize, usize)> {
        self.focus
    }

    /// Withhold every control, or offer them again; withholding settles the
    /// keyboard's typing and gives it up.
    pub fn set_withheld(&mut self, withheld: bool) {
        if withheld == self.withheld {
            return;
        }
        self.withheld = withheld;
        if withheld {
            self.focus_stop(None);
            self.held = None;
        }
        for part in &mut self.parts {
            set_enabled(part, !withheld);
        }
    }

    /// Whether the panel is withheld.
    #[must_use]
    pub const fn withheld(&self) -> bool {
        self.withheld
    }

    /// Feed a pointer event within `bounds`, an open list fitting `viewport`.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        (bounds, viewport): (Rect, Rect),
        (faces, scale, theme): (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> PanelOutcome {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        if self.withheld {
            return PanelOutcome::Ignored;
        }
        let pressed = matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        );
        let released = matches!(
            event,
            InputEvent::PointerReleased {
                button: PointerButton::Primary
            }
        );
        let placements = self.placements(bounds, faces, scale, theme);
        if self.listing() {
            return self.listing_pointer(event, placements.list(), viewport, scale, theme, damage);
        }
        if pressed {
            let landed = placements
                .list()
                .iter()
                .find(|(_, placed)| placed.rect.contains(self.pointer))
                .copied();
            if let Some((index, placed)) = landed {
                self.held = Some(index);
                if let Some(stop) = self.stop_at(index, &placed, scale) {
                    self.focus_stop(Some((index, stop)));
                    damage.add(placed.rect);
                }
            } else if self.focus.is_some() {
                self.focus_stop(None);
                damage.add(bounds);
            }
        }
        let held = self.held;
        if released {
            self.held = None;
        }
        let mut outcome = PanelOutcome::Ignored;
        for &(index, placed) in placements.list() {
            if let Part::Custom { .. } = self.parts[index] {
                let ours = held == Some(index) || placed.rect.contains(self.pointer);
                if ours && outcome == PanelOutcome::Ignored {
                    outcome = PanelOutcome::Custom {
                        part: index,
                        rect: placed.controls[0],
                    };
                }
                continue;
            }
            let found = self.part_pointer(index, &placed, event, viewport, scale, theme, damage);
            if let PanelOutcome::Event(_) = found {
                outcome = found;
            } else if found == PanelOutcome::Taken && outcome == PanelOutcome::Ignored {
                outcome = found;
            }
        }
        if outcome == PanelOutcome::Ignored && (pressed || released) && held.is_some() {
            return PanelOutcome::Taken;
        }
        outcome
    }

    /// A pointer event while a list is open, which takes them all.
    fn listing_pointer(
        &mut self,
        event: &InputEvent,
        placements: &[(usize, Placed)],
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> PanelOutcome {
        self.open_combo(placements, viewport, scale, theme).map_or(
            PanelOutcome::Taken,
            |(part, combo, field, popup)| {
                listed(
                    part,
                    combo.on_pointer(event, field, popup, scale, theme, damage),
                )
            },
        )
    }

    /// A key while a list is open, which takes them all.
    fn listing_key(
        &mut self,
        key: Key,
        placements: &[(usize, Placed)],
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> PanelOutcome {
        self.open_combo(placements, viewport, scale, theme).map_or(
            PanelOutcome::Taken,
            |(part, combo, field, popup)| {
                listed(part, combo.on_key(key, field, popup, scale, theme, damage))
            },
        )
    }

    /// The open list: its part, its field and where its list is drawn.
    fn open_combo(
        &mut self,
        placements: &[(usize, Placed)],
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<(usize, &mut ComboBox, Rect, Rect)> {
        let (part, placed) = placements.iter().copied().find(|&(index, _)| {
            matches!(&self.parts[index], Part::Choice { combo, .. } if combo.is_expanded())
        })?;
        let Part::Choice { combo, .. } = &mut self.parts[part] else {
            return None;
        };
        let field = placed.controls[0];
        let popup = combo.popup_rect(field, viewport, scale, theme);
        Some((part, combo, field, popup))
    }

    /// The keyboard stop of part `index` a press at the pointer gives the
    /// keyboard to, if any: a button or a switch acts without taking it.
    fn stop_at(&self, index: usize, placed: &Placed, scale: Scale) -> Option<usize> {
        let part = &self.parts[index];
        if part.stops() == 0 {
            return None;
        }
        match part {
            Part::Track(track) => {
                Some(track.handle_near(self.pointer.x, placed.controls[0], scale))
            }
            Part::Buttons(_) | Part::Switch(_) => None,
            _ => placed
                .controls
                .iter()
                .take(part.stops())
                .position(|control| control.contains(self.pointer)),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "a control's rectangles, the list's viewport and the paint context all reach the one control routed to"
    )]
    fn part_pointer(
        &mut self,
        index: usize,
        placed: &Placed,
        event: &InputEvent,
        viewport: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> PanelOutcome {
        match &mut self.parts[index] {
            Part::Choice { combo, .. } => {
                let field = placed.controls[0];
                let popup = combo.popup_rect(field, viewport, scale, theme);
                match combo.on_pointer(event, field, popup, scale, theme, damage) {
                    Some(ComboAction::Selected { index: chosen }) => {
                        PanelOutcome::Event(PanelEvent::Chosen {
                            part: index,
                            index: chosen,
                        })
                    }
                    Some(ComboAction::Opened | ComboAction::Closed) => PanelOutcome::Taken,
                    None => PanelOutcome::Ignored,
                }
            }
            Part::Number {
                parameter,
                field,
                slide,
            } => number_pointer(
                index,
                (parameter, field, slide),
                placed,
                event,
                (scale, theme),
                damage,
            ),
            Part::Fields(row) => {
                for (cell, (entry, control)) in row.iter_mut().zip(&placed.controls).enumerate() {
                    if let Some(action) = entry
                        .field
                        .on_pointer(event, *control, scale, theme, damage)
                    {
                        let (value, settled) = number_value(action);
                        return PanelOutcome::Event(PanelEvent::Number {
                            part: index,
                            cell,
                            value,
                            settled,
                        });
                    }
                }
                PanelOutcome::Ignored
            }
            Part::Buttons(row) => {
                let mut outcome = PanelOutcome::Ignored;
                for (button_index, (button, control)) in
                    row.iter_mut().zip(&placed.controls).enumerate()
                {
                    if let Some(ButtonAction::Activated) =
                        button.on_pointer(event, *control, damage)
                    {
                        outcome = PanelOutcome::Event(PanelEvent::Pressed {
                            part: index,
                            button: button_index,
                        });
                    }
                }
                outcome
            }
            Part::Switch(check) => match check.on_pointer(event, placed.controls[0], damage) {
                Some(SelectorAction::Set { on }) => {
                    PanelOutcome::Event(PanelEvent::Switched { part: index, on })
                }
                None => PanelOutcome::Ignored,
            },
            Part::Track(track) => {
                let was = track.dragging();
                match track.on_pointer(event, placed.controls[0], scale, damage) {
                    Some(moved) => PanelOutcome::Event(PanelEvent::Moved {
                        part: index,
                        handle: moved.handle,
                        value: moved.value,
                        settled: moved.settled,
                    }),
                    None if was || track.dragging() => PanelOutcome::Taken,
                    None => PanelOutcome::Ignored,
                }
            }
            Part::Custom { .. } | Part::Note(_) => PanelOutcome::Ignored,
        }
    }

    /// Feed a key: Tab walks the stops, settling a field it leaves, and walks
    /// off the panel past either end; a field claims every other key but the
    /// owner's chords; Escape, with nothing typed to take back, is the
    /// owner's.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        (bounds, viewport): (Rect, Rect),
        (faces, scale, theme): (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> PanelOutcome {
        if self.withheld {
            return PanelOutcome::Ignored;
        }
        let placements = self.placements(bounds, faces, scale, theme);
        if self.listing() {
            return self.listing_key(
                stroke.key,
                placements.list(),
                viewport,
                scale,
                theme,
                damage,
            );
        }
        let Some((part, stop)) = self.focus else {
            return PanelOutcome::Ignored;
        };
        let Some(placed) = placements.of(part) else {
            return PanelOutcome::Ignored;
        };
        if stroke.key == Key::Named(NamedKey::Tab) {
            let committed = self.commit_stop(part, stop, &placed, damage);
            damage.add(placed.rect);
            let next = self.next_stop((part, stop), !stroke.modifiers.shift);
            self.focus_stop(next);
            if let Some(landed) = next.and_then(|(to, _)| placements.of(to)) {
                damage.add(landed.rect);
            }
            return match (committed, next) {
                (Some(event), _) => PanelOutcome::Event(event),
                (None, Some(_)) => PanelOutcome::Taken,
                (None, None) => PanelOutcome::Left {
                    forward: !stroke.modifiers.shift,
                },
            };
        }
        if owner_chord(stroke.key, stroke.modifiers) {
            return PanelOutcome::Ignored;
        }
        self.stop_key(
            part,
            stop,
            stroke,
            &placed,
            (viewport, scale, theme),
            damage,
        )
    }

    fn stop_key(
        &mut self,
        part: usize,
        stop: usize,
        stroke: Keystroke,
        placed: &Placed,
        (viewport, scale, theme): (Rect, Scale, &Theme),
        damage: &mut Region,
    ) -> PanelOutcome {
        let number = |action: Option<NumberAction>, cell: usize| match action {
            Some(action) => {
                let (value, settled) = number_value(action);
                PanelOutcome::Event(PanelEvent::Number {
                    part,
                    cell,
                    value,
                    settled,
                })
            }
            None => PanelOutcome::Ignored,
        };
        match &mut self.parts[part] {
            Part::Choice { combo, .. } => {
                let field = placed.controls[0];
                let popup = combo.popup_rect(field, viewport, scale, theme);
                match combo.on_key(stroke.key, field, popup, scale, theme, damage) {
                    Some(ComboAction::Selected { index }) => {
                        PanelOutcome::Event(PanelEvent::Chosen { part, index })
                    }
                    Some(_) => PanelOutcome::Taken,
                    None => PanelOutcome::Ignored,
                }
            }
            Part::Number {
                parameter,
                field,
                slide,
            } => {
                if stop == 0 {
                    let action =
                        field.on_key(stroke.key, stroke.modifiers, placed.controls[0], damage);
                    if let Some(action) = action {
                        let (value, _) = number_value(action);
                        slide.show(parameter, value);
                        damage.add(placed.controls[1]);
                    }
                    return number(action, 0);
                }
                let stepped = match slide {
                    Slide::Slider(slider) => slider
                        .on_key(stroke.key, placed.controls[1], damage)
                        .map(|action| slid(parameter, action)),
                    Slide::Track(track) => track
                        .on_key(stroke, placed.controls[1], damage)
                        .map(|moved| (moved.value, moved.settled)),
                };
                match stepped {
                    Some((value, settled)) => {
                        field.set_value(value);
                        damage.add(placed.controls[0]);
                        PanelOutcome::Event(PanelEvent::Number {
                            part,
                            cell: 0,
                            value,
                            settled,
                        })
                    }
                    None => PanelOutcome::Ignored,
                }
            }
            Part::Fields(row) => match row.get_mut(stop) {
                Some(cell) => number(
                    cell.field
                        .on_key(stroke.key, stroke.modifiers, placed.controls[stop], damage),
                    stop,
                ),
                None => PanelOutcome::Ignored,
            },
            Part::Buttons(row) => match row
                .get_mut(stop)
                .and_then(|button| button.on_key(stroke.key))
            {
                Some(ButtonAction::Activated) => {
                    PanelOutcome::Event(PanelEvent::Pressed { part, button: stop })
                }
                None => PanelOutcome::Ignored,
            },
            Part::Switch(check) => match check.on_key(stroke.key) {
                Some(SelectorAction::Set { on }) => {
                    PanelOutcome::Event(PanelEvent::Switched { part, on })
                }
                None => PanelOutcome::Ignored,
            },
            Part::Track(track) => match track.on_key(stroke, placed.controls[0], damage) {
                Some(moved) => PanelOutcome::Event(PanelEvent::Moved {
                    part,
                    handle: moved.handle,
                    value: moved.value,
                    settled: moved.settled,
                }),
                None => PanelOutcome::Ignored,
            },
            Part::Custom { .. } => PanelOutcome::Custom {
                part,
                rect: placed.controls[0],
            },
            Part::Note(_) => PanelOutcome::Ignored,
        }
    }

    /// Settle what stop `stop` of part `part` holds typed.
    fn commit_stop(
        &mut self,
        part: usize,
        stop: usize,
        placed: &Placed,
        damage: &mut Region,
    ) -> Option<PanelEvent> {
        let (field, rect) = match &mut self.parts[part] {
            Part::Number { field, .. } if stop == 0 => (field, placed.controls[0]),
            Part::Fields(row) => {
                let cell = row.get_mut(stop)?;
                (&mut cell.field, placed.controls[stop])
            }
            _ => return None,
        };
        let action = field.commit(rect, damage)?;
        let (value, settled) = number_value(action);
        if let Part::Number {
            parameter, slide, ..
        } = &mut self.parts[part]
        {
            slide.show(parameter, value);
            damage.add(placed.controls[1]);
        }
        Some(PanelEvent::Number {
            part,
            cell: stop,
            value,
            settled,
        })
    }

    /// Settle what the keyboard's stop holds typed, keeping the keyboard
    /// there: done before anything else acts on the settings.
    pub fn commit(
        &mut self,
        bounds: Rect,
        (faces, scale, theme): (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> Option<PanelEvent> {
        let (part, stop) = self.focus?;
        let placed = self.place_of(part, bounds, faces, scale, theme)?;
        self.commit_stop(part, stop, &placed, damage)
    }

    /// Settle the keyboard's stop and take the keyboard from the panel.
    pub fn blur(
        &mut self,
        bounds: Rect,
        context: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> Option<PanelEvent> {
        let committed = self.commit(bounds, context, damage);
        if self.focus.is_some() {
            self.focus_stop(None);
            damage.add(bounds);
        }
        committed
    }

    /// Give the keyboard to the panel's first stop, or its last when not
    /// `forward`: `false` where it has none.
    pub fn enter_focus(&mut self, forward: bool, bounds: Rect, damage: &mut Region) -> bool {
        if self.withheld {
            return false;
        }
        let first = if forward {
            self.next_stop_from(None, true)
        } else {
            self.next_stop_from(None, false)
        };
        if first.is_none() {
            return false;
        }
        self.focus_stop(first);
        damage.add(bounds);
        true
    }

    /// Give the keyboard to `stop`, as a panel built again over the same
    /// parts had it: `false` where this one has no such stop.
    pub fn focus_at(&mut self, stop: (usize, usize)) -> bool {
        let (part, at) = stop;
        if self.withheld || self.parts.get(part).is_none_or(|part| at >= part.stops()) {
            return false;
        }
        self.focus_stop(Some(stop));
        true
    }

    /// The stop after (or before) `from`; `None` past either end.
    fn next_stop(&self, from: (usize, usize), forward: bool) -> Option<(usize, usize)> {
        self.next_stop_from(Some(from), forward)
    }

    fn next_stop_from(
        &self,
        from: Option<(usize, usize)>,
        forward: bool,
    ) -> Option<(usize, usize)> {
        let mut stops = self
            .parts
            .iter()
            .enumerate()
            .flat_map(|(index, part)| (0..part.stops()).map(move |stop| (index, stop)));
        match (from, forward) {
            (None, true) => stops.next(),
            (None, false) => stops.last(),
            (Some(from), true) => stops.skip_while(|&stop| stop != from).nth(1),
            (Some(from), false) => stops.take_while(|&stop| stop != from).last(),
        }
    }

    /// Move the keyboard to `to`, or off the panel.
    fn focus_stop(&mut self, to: Option<(usize, usize)>) {
        if let Some((part, stop)) = self.focus {
            if let Some(part) = self.parts.get_mut(part) {
                set_stop_focus(part, stop, false);
            }
        }
        self.focus = to;
        if let Some((part, stop)) = to {
            if let Some(part) = self.parts.get_mut(part) {
                set_stop_focus(part, stop, true);
            }
        }
    }
}

/// Each part as laid out, on the stack.
struct Placements {
    slots: [(usize, Placed); MOST_PARTS],
    count: usize,
}

impl Placements {
    fn list(&self) -> &[(usize, Placed)] {
        &self.slots[..self.count]
    }

    fn of(&self, part: usize) -> Option<Placed> {
        self.list()
            .iter()
            .find(|&&(index, _)| index == part)
            .map(|&(_, placed)| placed)
    }
}

/// A pointer event on number part `part`: its field's typing, or its slider
/// or track dragged, each shown in the other.
fn number_pointer(
    part: usize,
    (parameter, field, slide): (&Parameter, &mut NumberField, &mut Slide),
    placed: &Placed,
    event: &InputEvent,
    (scale, theme): (Scale, &Theme),
    damage: &mut Region,
) -> PanelOutcome {
    if let Some(action) = field.on_pointer(event, placed.controls[0], scale, theme, damage) {
        let (value, settled) = number_value(action);
        slide.show(parameter, value);
        damage.add(placed.controls[1]);
        return PanelOutcome::Event(PanelEvent::Number {
            part,
            cell: 0,
            value,
            settled,
        });
    }
    let dragged = match slide {
        Slide::Slider(slider) => slider
            .on_pointer(event, placed.controls[1], scale, theme, damage)
            .map(|action| slid(parameter, action)),
        Slide::Track(track) => track
            .on_pointer(event, placed.controls[1], scale, damage)
            .map(|moved| (moved.value, moved.settled)),
    };
    let Some((value, settled)) = dragged else {
        return PanelOutcome::Ignored;
    };
    field.set_value(value);
    damage.add(placed.controls[0]);
    PanelOutcome::Event(PanelEvent::Number {
        part,
        cell: 0,
        value,
        settled,
    })
}

/// What a list's action comes to: the choice made, or the list's own
/// business.
fn listed(part: usize, action: Option<ComboAction>) -> PanelOutcome {
    match action {
        Some(ComboAction::Selected { index }) => {
            PanelOutcome::Event(PanelEvent::Chosen { part, index })
        }
        _ => PanelOutcome::Taken,
    }
}

/// What a slider's action asks of `parameter`: the value and whether it is
/// settled.
fn slid(parameter: &Parameter, action: SliderAction) -> (i32, bool) {
    match action {
        SliderAction::SetValue { permille } => (parameter.value_of(permille), false),
        SliderAction::Settled { permille } => (parameter.value_of(permille), true),
    }
}

fn number_value(action: NumberAction) -> (i32, bool) {
    match action {
        NumberAction::Edited { value } => (value, false),
        NumberAction::Settled { value } => (value, true),
    }
}

fn set_stop_focus(part: &mut Part, stop: usize, focused: bool) {
    match part {
        Part::Choice { combo, .. } => combo.set_focused(focused),
        Part::Number { field, slide, .. } => {
            if stop == 0 {
                field.set_focused(focused);
            } else {
                slide.set_focused(focused);
            }
        }
        Part::Fields(row) => {
            if let Some(cell) = row.get_mut(stop) {
                cell.field.set_focused(focused);
            }
        }
        Part::Buttons(row) => {
            if let Some(button) = row.get_mut(stop) {
                button.set_focused(focused);
            }
        }
        Part::Switch(check) => check.set_focused(focused),
        Part::Track(track) => track.focus(focused.then_some(stop)),
        Part::Custom { .. } | Part::Note(_) => {}
    }
}

fn set_enabled(part: &mut Part, enabled: bool) {
    fn enable<T>(
        get: impl Fn(&T) -> tairix_controls::ControlState,
        put: impl Fn(&mut T, tairix_controls::ControlState),
        control: &mut T,
        enabled: bool,
    ) {
        let mut state = get(control);
        state.enabled = enabled;
        put(control, state);
    }
    match part {
        Part::Choice { combo, .. } => enable(ComboBox::state, ComboBox::set_state, combo, enabled),
        Part::Number { field, slide, .. } => {
            enable(NumberField::state, NumberField::set_state, field, enabled);
            slide.set_enabled(enabled);
        }
        Part::Fields(row) => {
            for cell in row {
                enable(
                    NumberField::state,
                    NumberField::set_state,
                    &mut cell.field,
                    enabled,
                );
            }
        }
        Part::Buttons(row) => {
            for button in row {
                enable(Button::state, Button::set_state, button, enabled);
            }
        }
        Part::Switch(check) => enable(Checkbox::state, Checkbox::set_state, check, enabled),
        Part::Track(track) => track.set_enabled(enabled),
        Part::Custom { .. } | Part::Note(_) => {}
    }
}

/// A switch part's mark for `on`.
#[must_use]
pub fn switch(label: &str, on: bool) -> Part {
    Part::Switch(Checkbox::new(label, selection(on)))
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
