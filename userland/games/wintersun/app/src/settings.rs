//! The settings window: where the player chooses how the client draws.
//!
//! One window for every setting a player makes about the client, with a
//! category strip down its side. Graphics is the category there is; another
//! — key bindings, sound — is a row in [`Category::ALL`] and a pane beside it.
//!
//! Every pixel is a shared control, and the window holds no capability and
//! does no I/O. Input updates its state, reports the rectangles that moved into
//! the caller's sink, and answers at most one [`Request`]: a slider drag
//! previews its detail and writes nothing, and the one write is the caller's,
//! where the interaction settles.
//!
//! A slider moved while the choice is a preset or `auto` makes it custom,
//! starting from the detail on screen, so a player can watch what `auto`
//! settled on and pin it by touching it.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_controls::{
    stack, ComboBox, FieldAction, FieldControl, FieldGroup, FieldGroupAction, FieldLayout,
    FieldRow, Keystroke, Slider, Tab, Tabs, TabsAction, TabsOrientation,
};
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_icon::NoArtwork;
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;
use tairix_wintersun_art::material::{Quality as MaterialQuality, MAX_OCTAVES};

use crate::graphics::{Graphics, Mode};
use crate::quality::{Detail, Lighting, Resolution, Shadows};

/// The window's title.
pub const TITLE: &str = "WinterSun Settings";

/// The category strip's width, in logical pixels.
const STRIP_WIDTH: u32 = 150;

/// The pane's width, in logical pixels: room for a setting's name and what it
/// is set to beside a slider long enough to aim.
const PANE_WIDTH: u32 = 480;

/// A category of setting.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Category {
    /// How frames are drawn.
    Graphics,
}

impl Category {
    /// Every category, in the order the strip lists them.
    pub const ALL: [Self; 1] = [Self::Graphics];

    /// What the strip calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Graphics => "Graphics",
        }
    }
}

/// What the player's input asks of the client.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Request {
    /// Draw with this choice from the next frame on, and keep nothing.
    Preview(Graphics),
    /// The interaction settled on this choice: draw with it, and keep it.
    Settle(Graphics),
    /// Close the window.
    Close,
}

/// What the window shows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Shown {
    /// The player's choice.
    pub graphics: Graphics,
    /// The detail frames are drawn at: the choice's own, or what `auto` has
    /// settled on.
    pub detail: Detail,
    /// The coarsest render scale that still draws figures readably in the
    /// game's window at its zoom.
    pub readable: Resolution,
}

/// One of the four knobs, as a slider sets it: plainest at the left, finest
/// at the right.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Knob {
    Lighting,
    Shadows,
    Ground,
    Resolution,
}

impl Knob {
    /// The knobs, in the order the pane lists them.
    const ALL: [Self; 4] = [
        Self::Lighting,
        Self::Shadows,
        Self::Ground,
        Self::Resolution,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Lighting => "Lighting",
            Self::Shadows => "Shadows",
            Self::Ground => "Ground texture",
            Self::Resolution => "Render scale",
        }
    }

    /// How many settings the knob has.
    const fn settings(self) -> usize {
        match self {
            Self::Lighting => Lighting::ALL.len(),
            Self::Shadows => Shadows::ALL.len(),
            Self::Ground => MAX_OCTAVES as usize + 1,
            Self::Resolution => Resolution::ALL.len(),
        }
    }

    /// Where `detail` sets this knob, counted from its plainest.
    fn position(self, detail: Detail) -> usize {
        let finest_first = |found: Option<usize>| self.settings() - 1 - found.unwrap_or(0);
        match self {
            Self::Lighting => {
                finest_first(Lighting::ALL.iter().position(|l| *l == detail.lighting))
            }
            Self::Shadows => finest_first(Shadows::ALL.iter().position(|s| *s == detail.shadows)),
            Self::Ground => usize::try_from(detail.ground.octaves()).unwrap_or(0),
            Self::Resolution => {
                finest_first(Resolution::ALL.iter().position(|r| *r == detail.resolution))
            }
        }
    }

    /// `detail` with this knob set to `position`, counted from its plainest.
    fn set(self, detail: Detail, position: usize) -> Detail {
        let last = self.settings() - 1;
        let finest_first = last - position.min(last);
        match self {
            Self::Lighting => Detail {
                lighting: Lighting::ALL[finest_first],
                ..detail
            },
            Self::Shadows => Detail {
                shadows: Shadows::ALL[finest_first],
                ..detail
            },
            Self::Ground => Detail {
                ground: MaterialQuality::new(u32::try_from(position.min(last)).unwrap_or(0)),
                ..detail
            },
            Self::Resolution => Detail {
                resolution: Resolution::ALL[finest_first],
                ..detail
            },
        }
    }

    /// What the knob is set to in `detail`, in a line.
    fn describe(self, detail: Detail, readable: Resolution) -> String {
        match self {
            Self::Lighting => String::from(match detail.lighting {
                Lighting::Fine => "Fine",
                Lighting::Medium => "Medium",
                Lighting::Coarse => "Coarse",
            }),
            Self::Shadows => String::from(match detail.shadows {
                Shadows::Soft => "Soft",
                Shadows::Hard => "Hard",
                Shadows::Flat => "Hard, and flat ground",
            }),
            Self::Ground => match detail.ground.octaves() {
                0 => String::from("Flat"),
                1 => String::from("1 octave"),
                octaves => format!("{octaves} octaves"),
            },
            Self::Resolution => {
                let scale = detail.resolution.scale();
                let percent = scale.numerator() * 100 / scale.denominator();
                if detail.resolution > readable {
                    format!("{percent}%: figures may not read clearly")
                } else {
                    format!("{percent}%")
                }
            }
        }
    }

    /// Every way [`Self::describe`] can put it, for sizing a window that
    /// must seat the longest.
    fn every_description(self) -> Vec<String> {
        let blocked = Resolution::Full;
        (0..self.settings())
            .map(|position| self.describe(self.set(Detail::FINEST, position), blocked))
            .collect()
    }
}

/// A slider position, counted from the plainest, as a slider's permille.
fn permille_of(position: usize, settings: usize) -> u16 {
    let last = settings.saturating_sub(1).max(1);
    u16::try_from(position.min(last) * 1000 / last).unwrap_or(1000)
}

/// The position nearest a slider's `permille`.
fn position_of(permille: u16, settings: usize) -> usize {
    let last = settings.saturating_sub(1).max(1);
    (usize::from(permille) * last + 500) / 1000
}

/// A detented slider over `settings` positions at `position`.
fn detented(position: usize, settings: usize) -> Slider {
    let step = permille_of(1, settings);
    Slider::new(permille_of(position, settings)).with_steps(step, step)
}

/// Which region of the window holds the keyboard.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Focus {
    /// The category strip.
    Strip,
    /// The group at this index.
    Group(usize),
}

/// The group holding the mode.
const QUALITY: usize = 0;

/// The group holding the four knobs.
const KNOBS: usize = 1;

/// Where every part of the window is drawn, resolved once per pass so a
/// paint and a hit test can never disagree.
struct Placed {
    strip: Rect,
    groups: [FieldLayout; 2],
}

/// The settings window's content.
#[derive(Debug)]
pub struct SettingsWindow {
    strip: Tabs,
    groups: [FieldGroup; 2],
    focus: Focus,
    shown: Shown,
    /// Where the pointer last was, which a press lands at.
    pointer: Option<Point>,
    /// The window's own extent, in its pixels.
    extent: (u32, u32),
}

impl SettingsWindow {
    /// The window showing `shown` at `scale`, the graphics category selected
    /// and the keyboard in its first control.
    #[must_use]
    pub fn new(shown: Shown, scale: Scale, theme: &Theme) -> Self {
        let tabs = Category::ALL.iter().map(|c| Tab::new(c.label())).collect();
        let mut strip = Tabs::new(tabs).with_orientation(TabsOrientation::Vertical);
        strip.adopt_selected(0);
        let mut window = Self {
            strip,
            groups: [
                quality_group(shown.graphics.mode()),
                knob_group(shown.detail, shown.readable),
            ],
            focus: Focus::Group(QUALITY),
            shown,
            pointer: None,
            extent: (0, 0),
        };
        window.groups[QUALITY].adopt_focus(Some(0));
        window.fit(scale, theme);
        window
    }

    /// The window's extent.
    #[must_use]
    pub const fn extent(&self) -> (u32, u32) {
        self.extent
    }

    /// The extent the window wants at `scale`, which it takes as its own:
    /// the strip beside a pane tall enough for every row at the longest it
    /// can be put, so nothing the player does pushes a row out of a window
    /// whose size is fixed when it opens.
    pub fn fit(&mut self, scale: Scale, theme: &Theme) -> (u32, u32) {
        let strip_w = scale.scale_length(STRIP_WIDTH);
        let pane_w = scale.scale_length(PANE_WIDTH);
        let plate = stack::plate_width(pane_w, scale, theme);
        let modes = [Mode::ALL
            .iter()
            .map(|m| String::from(m.description()))
            .collect()];
        let knobs: Vec<Vec<String>> = Knob::ALL.iter().map(|k| k.every_description()).collect();
        let tallest = [
            tallest_height(quality_group(Mode::Auto), &modes, plate, scale, theme),
            tallest_height(
                knob_group(Detail::FINEST, Resolution::Full),
                &knobs,
                plate,
                scale,
                theme,
            ),
        ];
        let pane_h = stack::height(tallest, scale, theme);
        let gap = stack::gap(scale, theme);
        let strip_h = self
            .strip
            .measured_height(scale, theme)
            .saturating_add(gap.saturating_mul(2));
        self.extent = (strip_w.saturating_add(pane_w), pane_h.max(strip_h));
        self.extent
    }

    /// Lay out in `extent`: the frame the window actually has, where the
    /// desktop would not re-map it to the one it wanted.
    pub fn set_extent(&mut self, extent: (u32, u32)) {
        self.extent = extent;
    }

    /// Show `shown`, reporting into `damage` what that moved.
    ///
    /// Called whenever the choice, the detail `auto` has settled on, or the
    /// readable floor moves. A control already showing what it is asked to
    /// show reports nothing, so a player's own drag costs nothing twice.
    pub fn show(&mut self, shown: Shown, scale: Scale, theme: &Theme, damage: &mut Region) {
        if self.shown == shown {
            return;
        }
        let before = self.place(scale, theme);
        let mode = shown.graphics.mode();
        let mode_moved = restate_row(
            &mut self.groups[QUALITY].rows_mut()[0],
            |control| match control {
                FieldControl::Combo(combo) => {
                    let index = Mode::ALL.iter().position(|m| *m == mode).unwrap_or(0);
                    let moved = combo.selected() != Some(index);
                    combo.set_selected(index);
                    moved
                }
                _ => false,
            },
            mode.description(),
        );
        let mut knobs_moved = [false; Knob::ALL.len()];
        for (index, knob) in Knob::ALL.into_iter().enumerate() {
            let position = knob.position(shown.detail);
            let settings = knob.settings();
            knobs_moved[index] = restate_row(
                &mut self.groups[KNOBS].rows_mut()[index],
                |control| match control {
                    FieldControl::Slider(slider) => {
                        let value = permille_of(position, settings);
                        let moved = slider.value() != value;
                        slider.set_value(value);
                        moved
                    }
                    _ => false,
                },
                &knob.describe(shown.detail, shown.readable),
            );
        }
        self.shown = shown;
        let after = self.place(scale, theme);
        if before.groups.map(|g| g.bounds) != after.groups.map(|g| g.bounds) {
            damage.add(self.bounds());
            return;
        }
        if mode_moved {
            report_row(
                &self.groups[QUALITY],
                0,
                after.groups[QUALITY],
                scale,
                theme,
                damage,
            );
        }
        for (index, _) in knobs_moved.iter().enumerate().filter(|(_, moved)| **moved) {
            report_row(
                &self.groups[KNOBS],
                index,
                after.groups[KNOBS],
                scale,
                theme,
                damage,
            );
        }
    }

    /// Feed a pointer event, answering what it asked of the client.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Request> {
        let placed = self.place(scale, theme);
        match event {
            InputEvent::PointerMoved { to } => self.pointer = Some(*to),
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => self.focus_under_press(&placed, scale, theme, damage),
            _ => {}
        }
        // A choice list that is open is modal: it hangs over the rows beneath
        // it, so only its own group may see the stream.
        let open = self.open_group();
        if open.is_none() {
            if let Some(TabsAction::Selected { index }) =
                self.strip
                    .on_pointer(event, placed.strip, scale, theme, damage)
            {
                self.strip
                    .set_selected(index, placed.strip, scale, theme, damage);
            }
        }
        let mut asked = None;
        for group in 0..self.groups.len() {
            if open.is_some_and(|open| open != group) {
                continue;
            }
            let layout = placed.groups[group];
            if let Some(action) = self.groups[group].on_pointer(event, layout, scale, theme, damage)
            {
                asked = self.act(group, action, &placed, scale, theme, damage);
            }
        }
        asked
    }

    /// Feed a key press, answering what it asked of the client.
    ///
    /// `Tab` and `Shift`+`Tab` walk the strip and the two groups; `Escape`
    /// closes an open choice list, and otherwise the window.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Request> {
        let placed = self.place(scale, theme);
        let listing = self.open_group().is_some();
        let key = stroke.key;
        match key {
            Key::Named(NamedKey::Escape) if !listing => return Some(Request::Close),
            Key::Named(NamedKey::Tab) if !listing => {
                let order = [Focus::Strip, Focus::Group(QUALITY), Focus::Group(KNOBS)];
                let at = order.iter().position(|f| *f == self.focus).unwrap_or(0);
                let next = if stroke.modifiers.shift {
                    order[(at + order.len() - 1) % order.len()]
                } else {
                    order[(at + 1) % order.len()]
                };
                self.move_focus(next, &placed, scale, theme, damage);
                return None;
            }
            _ => {}
        }
        match self.focus {
            Focus::Strip => {
                if let Some(TabsAction::Selected { index }) =
                    self.strip.on_key(key, placed.strip, scale, theme, damage)
                {
                    self.strip
                        .set_selected(index, placed.strip, scale, theme, damage);
                }
                None
            }
            Focus::Group(group) => {
                let layout = placed.groups[group];
                let action = self.groups[group].on_key(stroke, layout, scale, theme, damage)?;
                self.act(group, action, &placed, scale, theme, damage)
            }
        }
    }

    /// Paint the whole window into `surface`, which is its extent; a caller
    /// repainting part of it clips the surface first.
    pub fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        surface.fill(Color::from(theme.palette().surface));
        let placed = self.place(scale, theme);
        self.strip
            .render(surface, placed.strip, scale, theme, &mut NoArtwork);
        for (group, layout) in self.groups.iter().zip(placed.groups) {
            group.render(surface, layout, scale, theme);
        }
        // A choice list draws over every group, so it goes last.
        for (group, layout) in self.groups.iter().zip(placed.groups) {
            if !layout.popup.is_empty() {
                group.render_popup(surface, layout.popup, scale, theme);
            }
        }
    }

    /// Act on what a group's row asked for.
    fn act(
        &mut self,
        group: usize,
        action: FieldGroupAction,
        before: &Placed,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Request> {
        match (group, action.action) {
            (QUALITY, FieldAction::Selected { index }) => {
                let mode = *Mode::ALL.get(index)?;
                Some(Request::Settle(Graphics::chosen(mode, self.shown.detail)))
            }
            (QUALITY, FieldAction::Choices { .. }) => {
                // The list is drawn outside its row, so the row cannot report
                // it: the rectangle it covered and the one it now covers.
                damage.add(before.groups[QUALITY].popup);
                damage.add(self.place(scale, theme).groups[QUALITY].popup);
                None
            }
            (KNOBS, FieldAction::SetValue { permille }) => self
                .turn(action.row, permille, before, scale, theme, damage)
                .map(Request::Preview),
            (KNOBS, FieldAction::Settled { permille }) => self
                .turn(action.row, permille, before, scale, theme, damage)
                .map(Request::Settle),
            _ => None,
        }
    }

    /// The knob on `row` was moved to `permille`: seat its slider on the
    /// nearest setting and answer the custom choice it makes.
    fn turn(
        &mut self,
        row: usize,
        permille: u16,
        placed: &Placed,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Graphics> {
        let knob = *Knob::ALL.get(row)?;
        let settings = knob.settings();
        let position = position_of(permille, settings);
        let detail = knob.set(self.shown.detail, position);
        let seated = permille_of(position, settings);
        if let Some(FieldControl::Slider(slider)) = self.groups[KNOBS]
            .rows_mut()
            .get_mut(row)
            .map(FieldRow::control_mut)
        {
            slider.set_value(seated);
        }
        report_row(
            &self.groups[KNOBS],
            row,
            placed.groups[KNOBS],
            scale,
            theme,
            damage,
        );
        Some(Graphics::Custom(detail))
    }

    /// Put the keyboard in the region under a primary press.
    fn focus_under_press(
        &mut self,
        placed: &Placed,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        if self.open_group().is_some() {
            return;
        }
        let Some(pointer) = self.pointer else {
            return;
        };
        if placed.strip.contains(pointer) {
            self.move_focus(Focus::Strip, placed, scale, theme, damage);
            return;
        }
        for group in 0..self.groups.len() {
            let layout = placed.groups[group];
            if let Some(row) = self.groups[group].row_at(layout, scale, theme, pointer) {
                self.move_focus(Focus::Group(group), placed, scale, theme, damage);
                self.groups[group].set_focus(Some(row), layout, scale, theme, damage);
                return;
            }
        }
    }

    /// Move the keyboard to `next`, reporting the marks it moves between.
    fn move_focus(
        &mut self,
        next: Focus,
        placed: &Placed,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        if self.focus == next {
            return;
        }
        match self.focus {
            Focus::Strip => self
                .strip
                .set_current(None, placed.strip, scale, theme, damage),
            Focus::Group(group) => {
                self.groups[group].set_focus(None, placed.groups[group], scale, theme, damage);
            }
        }
        match next {
            Focus::Strip => {
                let current = self.strip.selected();
                self.strip
                    .set_current(current, placed.strip, scale, theme, damage);
            }
            Focus::Group(group) => {
                self.groups[group].set_focus(Some(0), placed.groups[group], scale, theme, damage);
            }
        }
        self.focus = next;
    }

    /// The group whose choice list is open, if one is.
    fn open_group(&self) -> Option<usize> {
        self.groups
            .iter()
            .position(|group| group.rows().iter().any(FieldRow::popup_open))
    }

    /// The window's own rectangle.
    fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.extent.0, self.extent.1)
    }

    /// Where every part of the window is drawn.
    fn place(&self, scale: Scale, theme: &Theme) -> Placed {
        let bounds = self.bounds();
        let strip_w = scale.scale_length(STRIP_WIDTH);
        let pane_w = scale.scale_length(PANE_WIDTH);
        let gap = stack::gap(scale, theme);
        let strip = Rect::new(
            to_i32(gap),
            to_i32(gap),
            strip_w.saturating_sub(gap.saturating_mul(2)),
            self.strip.measured_height(scale, theme),
        );
        let pane = Rect::new(to_i32(strip_w), 0, pane_w, bounds.height);
        let plate = stack::plate_width(pane_w, scale, theme);
        let heights = [
            height_of(&self.groups[QUALITY], plate, scale, theme),
            height_of(&self.groups[KNOBS], plate, scale, theme),
        ];
        let plates = stack::place(pane, heights.len(), scale, theme, |i| heights[i]);
        let layout = |index: usize| {
            let rect = plates.get(index).map_or(Rect::EMPTY, |(_, rect)| *rect);
            self.groups[index].layout(rect, bounds, scale, theme)
        };
        Placed {
            strip,
            groups: [layout(QUALITY), layout(KNOBS)],
        }
    }
}

/// The group holding the mode chooser, set to `mode`.
fn quality_group(mode: Mode) -> FieldGroup {
    let choices = Mode::ALL.iter().map(|m| String::from(m.label())).collect();
    let index = Mode::ALL.iter().position(|m| *m == mode).unwrap_or(0);
    let row = FieldRow::new(
        "Quality",
        FieldControl::Combo(ComboBox::new(choices).with_selected(index)),
    )
    .with_description(mode.description());
    FieldGroup::new("DETAIL", vec![row])
}

/// The group holding one slider per knob, set to `detail`.
fn knob_group(detail: Detail, readable: Resolution) -> FieldGroup {
    let rows = Knob::ALL
        .into_iter()
        .map(|knob| {
            FieldRow::new(
                knob.label(),
                FieldControl::Slider(detented(knob.position(detail), knob.settings())),
            )
            .with_description(knob.describe(detail, readable))
        })
        .collect();
    FieldGroup::new("EACH DETAIL", rows)
        .with_footnote("Moving any of these makes your choice Custom.")
}

/// The height `group` needs as a plate `width` pixels wide with each row's
/// description the tallest of that row's `candidates`.
///
/// Rows stack, so each row's tallest is found on its own.
fn tallest_height(
    mut group: FieldGroup,
    candidates: &[Vec<String>],
    width: u32,
    scale: Scale,
    theme: &Theme,
) -> u32 {
    for (row, texts) in candidates.iter().enumerate() {
        let mut tallest: Option<(u32, &String)> = None;
        for text in texts {
            if let Some(field) = group.rows_mut().get_mut(row) {
                field.set_description(Some(text.clone()));
            }
            let height = height_of(&group, width, scale, theme);
            if tallest.is_none_or(|(held, _)| height > held) {
                tallest = Some((height, text));
            }
        }
        if let (Some(field), Some((_, text))) = (group.rows_mut().get_mut(row), tallest) {
            field.set_description(Some(text.clone()));
        }
    }
    height_of(&group, width, scale, theme)
}

/// The height `group` needs as a plate `width` pixels wide.
fn height_of(group: &FieldGroup, width: u32, scale: Scale, theme: &Theme) -> u32 {
    group.measured_height(width, group.slot_column(width, scale, theme), scale, theme)
}

/// Restate a row's control through `restate` and its description, answering
/// whether either moved.
fn restate_row(
    row: &mut FieldRow,
    restate: impl FnOnce(&mut FieldControl) -> bool,
    description: &str,
) -> bool {
    let control = restate(row.control_mut());
    let words = row.set_description(Some(String::from(description)));
    control || words
}

/// Report row `index` of `group`, as laid out in `layout`.
fn report_row(
    group: &FieldGroup,
    index: usize,
    layout: FieldLayout,
    scale: Scale,
    theme: &Theme,
    damage: &mut Region,
) {
    if let Some(rect) = group.row_rect(index, layout, scale, theme) {
        damage.add(rect);
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
