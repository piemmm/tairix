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
    damage, stack, ColumnAction, ComboBox, FieldAction, FieldColumn, FieldControl, FieldGroup,
    FieldRow, Keystroke, SettingsAction, SettingsActions, SettingsFrame, SettingsLayout, Slider,
    Tab,
};
use tairix_geometry::{Rect, Region, Scale};
use tairix_icon::NoArtwork;
use tairix_input::InputEvent;
use tairix_raster::Surface;
use tairix_theme::Theme;
use tairix_wintersun_art::material::{Quality as MaterialQuality, MAX_OCTAVES};

use crate::graphics::{Graphics, Mode};
use crate::quality::{Detail, Resolution, Shadows};

/// The window's title.
pub const TITLE: &str = "WinterSun Settings";

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

/// One of the three knobs, as a slider sets it: plainest at the left,
/// finest at the right.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Knob {
    Shadows,
    Ground,
    Resolution,
}

impl Knob {
    /// The knobs, in the order the pane lists them.
    const ALL: [Self; 3] = [Self::Shadows, Self::Ground, Self::Resolution];

    const fn label(self) -> &'static str {
        match self {
            Self::Shadows => "Shadows",
            Self::Ground => "Ground texture",
            Self::Resolution => "Render scale",
        }
    }

    /// How many settings the knob has.
    const fn settings(self) -> usize {
        match self {
            Self::Shadows => Shadows::ALL.len(),
            Self::Ground => MAX_OCTAVES as usize + 1,
            Self::Resolution => Resolution::ALL.len(),
        }
    }

    /// Where `detail` sets this knob, counted from its plainest.
    fn position(self, detail: Detail) -> usize {
        let finest_first = |found: Option<usize>| self.settings() - 1 - found.unwrap_or(0);
        match self {
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

    /// A slider with a stop for each of the knob's settings, plainest at the
    /// left, at `position`.
    fn slider(self, position: usize) -> Slider {
        let mut slider = Slider::new(0).with_stops(u16::try_from(self.settings()).unwrap_or(0));
        slider.set_value(stop_value(&slider, position));
        slider
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

/// The value of `slider`'s stop at `position`, counted from its plainest.
fn stop_value(slider: &Slider, position: usize) -> u16 {
    u16::try_from(position)
        .ok()
        .and_then(|stop| slider.stop_value(stop))
        .unwrap_or(0)
}

/// The group holding the mode.
const QUALITY: usize = 0;

/// The group holding the three knobs.
const KNOBS: usize = 1;

/// The settings window's content: the shared settings frame, and what it
/// shows of the client's choice.
#[derive(Debug)]
pub struct SettingsWindow {
    frame: SettingsFrame,
    shown: Shown,
    /// The window's own extent, in its pixels.
    extent: (u32, u32),
    /// The frame laid out in that extent.
    layout: SettingsLayout,
}

impl SettingsWindow {
    /// The window showing `shown` at `scale`, the graphics category selected
    /// and the keyboard in its first control.
    #[must_use]
    pub fn new(shown: Shown, scale: Scale, theme: &Theme) -> Self {
        let tabs = Category::ALL.iter().map(|c| Tab::new(c.label())).collect();
        let groups = vec![
            quality_group(shown.graphics.mode()),
            knob_group(shown.detail, shown.readable),
        ];
        let mut frame = SettingsFrame::new(tabs, 0, groups, Vec::new());
        frame.focus_column();
        let mut window = Self {
            frame,
            shown,
            extent: (0, 0),
            layout: SettingsLayout::default(),
        };
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
        let pane_w = scale.scale_length(PANE_WIDTH);
        let plate = stack::plate_width(pane_w, scale, theme);
        let modes = [Mode::ALL
            .iter()
            .map(|m| String::from(m.description()))
            .collect()];
        let knobs: Vec<Vec<String>> = Knob::ALL.iter().map(|k| k.every_description()).collect();
        let tallest = vec![
            tallest(quality_group(Mode::Auto), &modes, plate, scale, theme),
            tallest(
                knob_group(Detail::FINEST, Resolution::Full),
                &knobs,
                plate,
                scale,
                theme,
            ),
        ];
        // Measured as the window's own column measures it: one slot column
        // across both plates.
        let pane_h = FieldColumn::new(tallest).measured_height(pane_w, scale, theme);
        let extent = self.frame.size_for((pane_w, pane_h), scale, theme);
        self.set_extent(extent, scale, theme);
        self.extent
    }

    /// Lay out in `extent`: the frame the window actually has, where the
    /// desktop would not re-map it to the one it wanted.
    pub fn set_extent(&mut self, extent: (u32, u32), scale: Scale, theme: &Theme) {
        self.extent = extent;
        self.layout = self.frame.layout(extent.0, extent.1, scale, theme);
        self.frame.settle(&self.layout, &mut damage::sink());
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
        let before = self.row_rects(scale, theme);
        let groups = self.frame.column_mut().groups_mut();
        let mode = shown.graphics.mode();
        let mut moved = Vec::new();
        if restate_row(
            &mut groups[QUALITY].rows_mut()[0],
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
        ) {
            moved.push((QUALITY, 0));
        }
        for (index, knob) in Knob::ALL.into_iter().enumerate() {
            let position = knob.position(shown.detail);
            if restate_row(
                &mut groups[KNOBS].rows_mut()[index],
                |control| match control {
                    FieldControl::Slider(slider) => {
                        let value = stop_value(slider, position);
                        let moved = slider.value() != value;
                        slider.set_value(value);
                        moved
                    }
                    _ => false,
                },
                &knob.describe(shown.detail, shown.readable),
            ) {
                moved.push((KNOBS, index));
            }
        }
        self.shown = shown;
        self.layout = self
            .frame
            .layout(self.extent.0, self.extent.1, scale, theme);
        // A description that wrapped onto another line moved every row below
        // it; the window is drawn again whole.
        if before != self.row_rects(scale, theme) {
            damage.add(self.bounds());
            return;
        }
        for at in moved {
            if let Some(rect) = self.frame.row_rect(at, &self.layout, scale, theme) {
                damage.add(rect);
            }
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
        let asked = self
            .frame
            .on_pointer(event, &self.layout, scale, theme, damage);
        self.answered(asked, scale, theme, damage)
    }

    /// Feed a key press, answering what it asked of the client.
    ///
    /// `Tab` and `Shift`+`Tab` walk the strip and the pane; `Escape` closes
    /// an open choice list, and otherwise the window.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Request> {
        let asked = self
            .frame
            .on_key(stroke, &self.layout, scale, theme, damage);
        self.answered(asked, scale, theme, damage)
    }

    /// When a control of the window next changes on its own — a held scroll
    /// arrow stepping — if one will.
    pub fn deadline_ns(&mut self, now_ns: u64) -> Option<u64> {
        self.frame.deadline_ns(now_ns)
    }

    /// Carry out what came due by `now_ns`, reporting what moved.
    pub fn advance(&mut self, now_ns: u64, scale: Scale, theme: &Theme, damage: &mut Region) {
        self.frame
            .advance(now_ns, &self.layout, scale, theme, damage);
    }

    /// Paint the whole window into `surface`, which is its extent; a caller
    /// repainting part of it clips the surface first.
    pub fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        self.frame
            .render(surface, &self.layout, (scale, theme), &mut NoArtwork);
    }

    /// What the frame's `actions` ask of the client: the last a row made.
    fn answered(
        &mut self,
        actions: SettingsActions,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Request> {
        let mut asked = None;
        for action in actions {
            asked = match action {
                SettingsAction::Field(acted) => self.act(&acted, scale, theme, damage).or(asked),
                SettingsAction::Close => Some(Request::Close),
                // One category, and no commands beneath it.
                SettingsAction::Shown { .. } | SettingsAction::Command { .. } => asked,
            };
        }
        asked
    }

    /// Act on what a group's row asked for.
    fn act(
        &mut self,
        acted: &ColumnAction,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Request> {
        let action = &acted.action;
        match (acted.group, &action.action) {
            (QUALITY, FieldAction::Selected { index }) => {
                let mode = *Mode::ALL.get(*index)?;
                Some(Request::Settle(Graphics::chosen(mode, self.shown.detail)))
            }
            (KNOBS, FieldAction::SetValue { permille }) => self
                .turn(action.row, *permille, scale, theme, damage)
                .map(Request::Preview),
            (KNOBS, FieldAction::Settled { permille }) => self
                .turn(action.row, *permille, scale, theme, damage)
                .map(Request::Settle),
            _ => None,
        }
    }

    /// The knob on `row` was moved to the stop at `permille`: answer the
    /// custom choice it makes.
    fn turn(
        &mut self,
        row: usize,
        permille: u16,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Graphics> {
        let knob = *Knob::ALL.get(row)?;
        let FieldControl::Slider(slider) = self.frame.column().groups()[KNOBS]
            .rows()
            .get(row)?
            .control()
        else {
            return None;
        };
        let position = usize::from(slider.stop_of(permille)?);
        let detail = knob.set(self.shown.detail, position);
        if let Some(rect) = self
            .frame
            .row_rect((KNOBS, row), &self.layout, scale, theme)
        {
            damage.add(rect);
        }
        Some(Graphics::Custom(detail))
    }

    /// Where each row of the two plates is drawn, the mode's first.
    fn row_rects(&self, scale: Scale, theme: &Theme) -> [Option<Rect>; 1 + Knob::ALL.len()] {
        core::array::from_fn(|index| {
            let at = index
                .checked_sub(1)
                .map_or((QUALITY, 0), |knob| (KNOBS, knob));
            self.frame.row_rect(at, &self.layout, scale, theme)
        })
    }

    /// The window's own rectangle.
    fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.extent.0, self.extent.1)
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
                FieldControl::Slider(knob.slider(knob.position(detail))),
            )
            .with_description(knob.describe(detail, readable))
        })
        .collect();
    FieldGroup::new("EACH DETAIL", rows)
        .with_footnote("Moving any of these makes your choice Custom.")
}

/// `group` with each row's description the tallest of that row's
/// `candidates` in a plate `width` pixels wide.
///
/// Rows stack, so each row's tallest is found on its own.
fn tallest(
    mut group: FieldGroup,
    candidates: &[Vec<String>],
    width: u32,
    scale: Scale,
    theme: &Theme,
) -> FieldGroup {
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
    group
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

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
