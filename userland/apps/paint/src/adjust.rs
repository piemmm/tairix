//! The Adjustment pane: while no adjustment is open, the list it opens one
//! from; otherwise the open adjustment's settings — the panel each declares,
//! how an input to it moves the settings, and the graphs it draws.
//!
//! The pane holds settings and nothing else: what the picture shows of them,
//! and applying them, are the window's.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_colour::{Fraction, Hsl, Hue, Rgb};
use tairix_controls::{
    fill_area, Button, ButtonContent, ComboBox, ControlRole, Keystroke, NumberField,
};
use tairix_geometry::{to_i32, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use crate::curve_graph::{CurveEdit, CurveGraph};
use crate::filter::{Filter, Parameter};
use crate::histogram::{Histogram, Plot};
use crate::layout::Faces;
use crate::panel::{switch, Cell, Height, Panel, PanelArt, PanelEvent, PanelOutcome, Part};
use crate::tone::{Channel, ColourBalance, HueRange, HueRanges, Tones, WhiteBalance};
use crate::track::Track;

/// What an adjustment's eyedropper takes from the picture.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Pick {
    /// The colour that becomes black.
    Black,
    /// The colour that becomes a neutral grey.
    Grey,
    /// The colour that becomes white.
    White,
    /// The colour the light is balanced to grey by.
    Neutral,
}

/// What an input to the pane came to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AdjustOutcome {
    /// None of the pane's.
    Ignored,
    /// The pane's, the settings unchanged.
    Taken,
    /// The settings moved; `settled` once the interaction is over.
    Changed {
        /// Whether the interaction is over.
        settled: bool,
    },
    /// Preview was turned on or off.
    Previewed,
    /// Apply was pressed.
    Apply,
    /// The adjustment was chosen from the list, at its starting settings.
    Open(Filter),
    /// An eyedropper was taken up or put down.
    Picking,
    /// Auto was pressed, with the histogram at hand.
    Auto,
    /// The keyboard walked off the pane: past its last stop when `forward`.
    Left {
        /// Which way.
        forward: bool,
    },
}

/// What one part of the pane's panel does.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Role {
    /// The list of adjustments to open.
    Choose,
    /// A number the filter is set by: an entry of its parameters.
    Parameter(usize),
    /// The channel levels or curves shows.
    Channel,
    /// The range hue and saturation shows.
    Range,
    /// The band colour balance shows.
    Tones,
    /// The histogram of what the settings show.
    Histogram,
    /// Levels' input black, grey and white.
    Input,
    /// Their fields: black, gamma and white.
    InputFields,
    /// Levels' output black and white.
    Output,
    /// Their fields.
    OutputFields,
    /// The eyedroppers, and Auto.
    Pickers,
    /// The curve.
    Curve,
    /// The chosen point's input and output.
    Point,
    /// The range's hue, saturation and lightness.
    Hue,
    /// See [`Role::Hue`].
    Saturation,
    /// See [`Role::Hue`].
    Lightness,
    /// The input and output hue spectra.
    Spectra,
    /// The band's move along axis `0`, `1` or `2`.
    Axis(usize),
    /// Whether colour balance keeps each colour's lightness.
    KeepLuminosity,
    /// White balance's temperature.
    Kelvin,
    /// White balance's tint.
    Tint,
    /// Whether the picture shows the settings.
    Preview,
    /// Reset and Apply.
    Actions,
    /// A line of text.
    Note,
}

const HUE: Parameter = Parameter {
    label: "Hue",
    least: -180,
    most: 180,
};
const SATURATION: Parameter = Parameter {
    label: "Saturation",
    least: -100,
    most: 100,
};
const LIGHTNESS: Parameter = Parameter {
    label: "Lightness",
    least: -100,
    most: 100,
};
const KELVIN: Parameter = Parameter {
    label: "Temperature",
    least: WhiteBalance::KELVIN.0,
    most: WhiteBalance::KELVIN.1,
};
const TINT: Parameter = Parameter {
    label: "Tint",
    least: -WhiteBalance::TINT,
    most: WhiteBalance::TINT,
};
const AXIS_LABELS: [&str; 3] = ["Cyan and red", "Magenta and green", "Yellow and blue"];

/// What Auto sets aside at each end of a channel: 0.1% of it.
const AUTO_CLIP: f64 = 0.001;

/// The levels pickers' buttons, then Auto's.
const LEVEL_PICKS: [Pick; 3] = [Pick::Black, Pick::Grey, Pick::White];

/// The open adjustment's settings and what the panel shows of them.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Setting {
    start: Filter,
    filter: Filter,
    channel: Channel,
    range: HueRange,
    tones: Tones,
    curve: CurveGraph,
    picking: Option<Pick>,
    preview: bool,
    /// Whether a histogram of the picture as it stands is at hand, which Auto
    /// reads.
    histogram_ready: bool,
}

/// The Adjustment pane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdjustPane {
    panel: Panel,
    roles: Vec<Role>,
    setting: Option<Box<Setting>>,
}

impl Default for AdjustPane {
    fn default() -> Self {
        Self::choosing()
    }
}

impl AdjustPane {
    /// The pane with no adjustment open: the list to open one from.
    #[must_use]
    pub fn choosing() -> Self {
        let choose = Part::Choice {
            label: "Adjustment",
            combo: ComboBox::new(
                Filter::ALL
                    .iter()
                    .map(|filter| String::from(filter.label()))
                    .collect(),
            )
            .with_placeholder("Choose\u{2026}"),
        };
        Self {
            panel: Panel::new(vec![
                choose,
                Part::Note(String::from("Its settings are set here.")),
            ]),
            roles: vec![Role::Choose, Role::Note],
            setting: None,
        }
    }

    /// The pane setting `filter`, from its settings as they stand.
    #[must_use]
    pub fn setting(filter: Filter) -> Self {
        let setting = Box::new(Setting {
            start: filter,
            filter,
            channel: Channel::Composite,
            range: HueRange::Master,
            tones: Tones::Midtones,
            curve: CurveGraph::default(),
            picking: None,
            preview: true,
            histogram_ready: false,
        });
        let (parts, roles) = parts_of(&setting);
        let mut pane = Self {
            panel: Panel::new(parts),
            roles,
            setting: Some(setting),
        };
        pane.sync();
        pane
    }

    /// The settings as they stand, while an adjustment is open.
    #[must_use]
    pub fn filter(&self) -> Option<Filter> {
        self.setting.as_ref().map(|setting| setting.filter)
    }

    /// What the band heading the pane says.
    #[must_use]
    pub fn title(&self) -> &'static str {
        self.setting
            .as_ref()
            .map_or("Adjustment", |setting| setting.filter.label())
    }

    /// Whether the picture shows the settings.
    #[must_use]
    pub fn previewing(&self) -> bool {
        self.setting.as_ref().is_some_and(|setting| setting.preview)
    }

    /// The eyedropper taken up, if one is: the next press on the picture is
    /// its.
    #[must_use]
    pub fn picking(&self) -> Option<Pick> {
        self.setting.as_ref().and_then(|setting| setting.picking)
    }

    /// Whether the pane reads a histogram of the picture: to show one, or for
    /// Auto.
    #[must_use]
    pub fn reads_histogram(&self) -> bool {
        self.setting.as_ref().is_some_and(|setting| {
            matches!(
                setting.filter,
                Filter::Levels(_)
                    | Filter::Curves(_)
                    | Filter::Threshold { .. }
                    | Filter::WhiteBalance(_)
            )
        })
    }

    /// Say whether a histogram of the picture as it stands is at hand, which
    /// Auto needs; answers whether what the pane draws changed.
    pub fn set_histogram_ready(&mut self, ready: bool) -> bool {
        let Some(setting) = &mut self.setting else {
            return false;
        };
        if setting.histogram_ready == ready {
            return false;
        }
        setting.histogram_ready = ready;
        self.sync_pickers();
        true
    }

    /// The plot the histogram shows.
    fn plot(&self) -> Plot {
        let Some(setting) = &self.setting else {
            return Plot::Luma;
        };
        match setting.channel {
            Channel::Composite => Plot::Luma,
            Channel::Red => Plot::Red,
            Channel::Green => Plot::Green,
            Channel::Blue => Plot::Blue,
        }
    }

    /// Withhold the pane while the picture cannot take its settings, or offer
    /// it again.
    pub fn set_withheld(&mut self, withheld: bool) {
        self.panel.set_withheld(withheld);
        if let Some(setting) = &mut self.setting {
            setting.curve.set_enabled(!withheld);
            if withheld {
                setting.picking = None;
            }
        }
    }

    /// Whether a list is open, owning the pointer and the keyboard.
    #[must_use]
    pub fn listing(&self) -> bool {
        self.panel.listing()
    }

    /// Whether a press is held on the pane.
    #[must_use]
    pub fn holding(&self) -> bool {
        self.panel.holding()
            || self
                .setting
                .as_ref()
                .is_some_and(|setting| setting.curve.dragging())
    }

    /// Whether the pane has the keyboard.
    #[must_use]
    pub fn has_focus(&self) -> bool {
        self.panel.focus().is_some()
    }

    /// How tall the pane's settings stand across `width`.
    #[must_use]
    pub fn measured_height(&self, width: u32, faces: Faces, scale: Scale, theme: &Theme) -> u32 {
        self.panel.measured_height(width, faces, scale, theme)
    }

    /// Put the eyedropper down, answering whether one was up.
    pub fn put_down_pick(&mut self) -> bool {
        let Some(setting) = &mut self.setting else {
            return false;
        };
        if setting.picking.take().is_none() {
            return false;
        }
        self.sync_pickers();
        true
    }

    /// Return the settings to where they started.
    pub fn reset(&mut self) -> bool {
        let Some(setting) = &mut self.setting else {
            return false;
        };
        let moved = setting.filter != setting.start;
        setting.filter = setting.start;
        setting.picking = None;
        setting.curve.select(None);
        self.sync();
        moved
    }

    /// The eyedropper took `colour` from the picture: set from it, and put
    /// the eyedropper down. Answers whether the settings moved.
    pub fn picked(&mut self, colour: Rgb) -> bool {
        let Some(setting) = &mut self.setting else {
            return false;
        };
        let Some(pick) = setting.picking.take() else {
            return false;
        };
        let before = setting.filter;
        match (&mut setting.filter, pick) {
            (Filter::Levels(levels), Pick::Black) => levels.black_point(colour),
            (Filter::Levels(levels), Pick::Grey) => levels.grey_point(colour),
            (Filter::Levels(levels), Pick::White) => levels.white_point(colour),
            (Filter::WhiteBalance(balance), Pick::Neutral) => {
                if let Some(neutral) = WhiteBalance::neutralising(colour) {
                    *balance = neutral;
                }
            }
            _ => {}
        }
        let moved = setting.filter != before;
        self.sync();
        moved
    }

    /// Auto, reading `histogram`: levels stretched over what each channel
    /// holds, or the light balanced to the picture's mean. Answers whether
    /// the settings moved.
    pub fn auto(&mut self, histogram: &Histogram) -> bool {
        let Some(setting) = &mut self.setting else {
            return false;
        };
        let before = setting.filter;
        match &mut setting.filter {
            Filter::Levels(levels) => levels.auto(histogram.colours(), AUTO_CLIP),
            Filter::WhiteBalance(balance) => {
                if let Some(neutral) = histogram.mean().and_then(WhiteBalance::neutralising_linear)
                {
                    *balance = neutral;
                }
            }
            _ => {}
        }
        let moved = setting.filter != before;
        self.sync();
        moved
    }

    /// Feed a pointer event within `bounds`, an open list fitting `viewport`.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        (bounds, viewport): (Rect, Rect),
        (faces, scale, theme): (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> AdjustOutcome {
        let outcome =
            self.panel
                .on_pointer(event, (bounds, viewport), (faces, scale, theme), damage);
        let answer = match outcome {
            PanelOutcome::Custom { part, rect } => {
                self.custom_pointer(part, rect, event, scale, damage)
            }
            other => self.answer(other, bounds, damage),
        };
        self.follow_focus();
        answer
    }

    /// Feed a key: Escape with nothing typed to take back returns the
    /// settings to where they started.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        (bounds, viewport): (Rect, Rect),
        (faces, scale, theme): (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> AdjustOutcome {
        let outcome = self
            .panel
            .on_key(stroke, (bounds, viewport), (faces, scale, theme), damage);
        let answer = match outcome {
            PanelOutcome::Custom { part, rect } => self.custom_key(part, rect, stroke, damage),
            PanelOutcome::Ignored
                if stroke.key == Key::Named(NamedKey::Escape) && self.setting.is_some() =>
            {
                damage.add(bounds);
                AdjustOutcome::Changed {
                    settled: self.reset(),
                }
            }
            other => self.answer(other, bounds, damage),
        };
        self.follow_focus();
        answer
    }

    /// Settle what the keyboard holds typed, keeping it there.
    pub fn commit(
        &mut self,
        bounds: Rect,
        context: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> AdjustOutcome {
        match self.panel.commit(bounds, context, damage) {
            Some(event) => self.answer(PanelOutcome::Event(event), bounds, damage),
            None => AdjustOutcome::Ignored,
        }
    }

    /// Settle the keyboard's typing and take the keyboard from the pane.
    pub fn blur(
        &mut self,
        bounds: Rect,
        context: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> AdjustOutcome {
        let answer = match self.panel.blur(bounds, context, damage) {
            Some(event) => self.answer(PanelOutcome::Event(event), bounds, damage),
            None => AdjustOutcome::Ignored,
        };
        self.follow_focus();
        answer
    }

    /// Give the pane the keyboard at its first stop, or its last.
    pub fn enter_focus(&mut self, forward: bool, bounds: Rect, damage: &mut Region) -> bool {
        let entered = self.panel.enter_focus(forward, bounds, damage);
        self.follow_focus();
        entered
    }

    /// The curve editor has the keyboard while its part does.
    fn follow_focus(&mut self) {
        let focused = self
            .panel
            .focus()
            .is_some_and(|(part, _)| self.roles.get(part) == Some(&Role::Curve));
        if let Some(setting) = &mut self.setting {
            setting.curve.focus(focused);
        }
    }

    /// What the panel's outcome comes to for the settings.
    fn answer(
        &mut self,
        outcome: PanelOutcome,
        bounds: Rect,
        damage: &mut Region,
    ) -> AdjustOutcome {
        match outcome {
            PanelOutcome::Ignored => AdjustOutcome::Ignored,
            PanelOutcome::Taken | PanelOutcome::Custom { .. } => AdjustOutcome::Taken,
            PanelOutcome::Left { forward } => AdjustOutcome::Left { forward },
            PanelOutcome::Event(event) => self.event(event, bounds, damage),
        }
    }

    fn event(&mut self, event: PanelEvent, bounds: Rect, damage: &mut Region) -> AdjustOutcome {
        let part = match event {
            PanelEvent::Chosen { part, .. }
            | PanelEvent::Number { part, .. }
            | PanelEvent::Pressed { part, .. }
            | PanelEvent::Switched { part, .. }
            | PanelEvent::Moved { part, .. } => part,
        };
        let Some(&role) = self.roles.get(part) else {
            return AdjustOutcome::Ignored;
        };
        if let (Role::Choose, PanelEvent::Chosen { index, .. }) = (role, event) {
            return Filter::ALL
                .get(index)
                .map_or(AdjustOutcome::Taken, |&filter| AdjustOutcome::Open(filter));
        }
        let Some(setting) = &mut self.setting else {
            return AdjustOutcome::Ignored;
        };
        let before = setting.filter;
        let mut settled = true;
        match (role, event) {
            (Role::Channel | Role::Range | Role::Tones, PanelEvent::Chosen { index, .. }) => {
                setting.choose(role, index);
                // What the settings show changed, not the settings.
                self.sync();
                damage.add(bounds);
                return AdjustOutcome::Taken;
            }
            (
                role,
                PanelEvent::Number {
                    cell,
                    value,
                    settled: done,
                    ..
                },
            ) => {
                settled = done;
                setting.set_number(role, cell, value);
            }
            (
                role,
                PanelEvent::Moved {
                    handle,
                    value,
                    settled: done,
                    ..
                },
            ) => {
                settled = done;
                setting.set_handle(role, handle, value);
            }
            (Role::Pickers, PanelEvent::Pressed { button, .. }) => {
                if !setting.toggle_pick(button) {
                    return AdjustOutcome::Auto;
                }
                self.sync_pickers();
                damage.add(bounds);
                return AdjustOutcome::Picking;
            }
            (Role::Actions, PanelEvent::Pressed { button: 0, .. }) => {
                let moved = self.reset();
                damage.add(bounds);
                return AdjustOutcome::Changed { settled: moved };
            }
            (Role::Actions, PanelEvent::Pressed { .. }) => return AdjustOutcome::Apply,
            (Role::Preview, PanelEvent::Switched { on, .. }) => {
                setting.preview = on;
                self.sync();
                damage.add(bounds);
                return AdjustOutcome::Previewed;
            }
            (Role::KeepLuminosity, PanelEvent::Switched { on, .. }) => {
                if let Filter::ColourBalance(balance) = &mut setting.filter {
                    balance.keep_luminosity = on;
                }
            }
            _ => return AdjustOutcome::Taken,
        }
        let moved = self
            .setting
            .as_ref()
            .is_some_and(|setting| setting.filter != before);
        self.sync();
        damage.add(bounds);
        if moved {
            AdjustOutcome::Changed { settled }
        } else if settled {
            AdjustOutcome::Changed { settled: true }
        } else {
            AdjustOutcome::Taken
        }
    }

    fn custom_pointer(
        &mut self,
        part: usize,
        rect: Rect,
        event: &InputEvent,
        scale: Scale,
        damage: &mut Region,
    ) -> AdjustOutcome {
        if self.roles.get(part) != Some(&Role::Curve) {
            let press = matches!(
                event,
                InputEvent::PointerPressed {
                    button: PointerButton::Primary
                }
            );
            return if press {
                AdjustOutcome::Taken
            } else {
                AdjustOutcome::Ignored
            };
        }
        let Some(setting) = &mut self.setting else {
            return AdjustOutcome::Ignored;
        };
        let channel = setting.channel;
        let Filter::Curves(curves) = &setting.filter else {
            return AdjustOutcome::Ignored;
        };
        let curve = *curves.of(channel);
        let edit = setting.curve.on_pointer(event, rect, &curve, scale, damage);
        self.curve_edited(edit)
    }

    fn custom_key(
        &mut self,
        part: usize,
        rect: Rect,
        stroke: Keystroke,
        damage: &mut Region,
    ) -> AdjustOutcome {
        if self.roles.get(part) != Some(&Role::Curve) {
            return AdjustOutcome::Ignored;
        }
        let Some(setting) = &mut self.setting else {
            return AdjustOutcome::Ignored;
        };
        let channel = setting.channel;
        let Filter::Curves(curves) = &setting.filter else {
            return AdjustOutcome::Ignored;
        };
        let curve = *curves.of(channel);
        let edit = setting.curve.on_key(stroke, rect, &curve, damage);
        self.curve_edited(edit)
    }

    fn curve_edited(&mut self, edit: Option<CurveEdit>) -> AdjustOutcome {
        let Some(setting) = &mut self.setting else {
            return AdjustOutcome::Ignored;
        };
        let outcome = match edit {
            Some(CurveEdit::Changed { curve, settled }) => {
                if let Filter::Curves(curves) = &mut setting.filter {
                    *curves.of_mut(setting.channel) = curve;
                }
                AdjustOutcome::Changed { settled }
            }
            Some(CurveEdit::Chose) => AdjustOutcome::Taken,
            None => return AdjustOutcome::Ignored,
        };
        self.sync();
        outcome
    }

    /// Show the settings in every part.
    fn sync(&mut self) {
        let Some(setting) = &self.setting else {
            return;
        };
        let withheld = self.panel.withheld();
        for (index, &role) in self.roles.iter().enumerate() {
            if let Some(part) = self.panel.part_mut(index) {
                setting.show(role, part, withheld);
            }
        }
        self.sync_pickers();
    }

    /// Show which eyedropper is taken up, and offer Auto only with a
    /// histogram at hand.
    fn sync_pickers(&mut self) {
        let Some(setting) = &self.setting else {
            return;
        };
        let (picking, ready, filter) = (setting.picking, setting.histogram_ready, setting.filter);
        let withheld = self.panel.withheld();
        let Some(index) = self.roles.iter().position(|&role| role == Role::Pickers) else {
            return;
        };
        let Some(Part::Buttons(buttons)) = self.panel.part_mut(index) else {
            return;
        };
        let picks: &[Pick] = match filter {
            Filter::Levels(_) => &LEVEL_PICKS,
            _ => &[Pick::Neutral],
        };
        for (at, button) in buttons.iter_mut().enumerate() {
            let state = button.state();
            let (label, role, enabled) = match picks.get(at) {
                Some(&pick) => (
                    pick_label(pick),
                    if picking == Some(pick) {
                        ControlRole::Recommended
                    } else {
                        ControlRole::Neutral
                    },
                    state.enabled,
                ),
                None => ("Auto", ControlRole::Neutral, ready && !withheld),
            };
            let mut rebuilt = Button::new(ButtonContent::Label(String::from(label)), role);
            let mut kept = state;
            kept.enabled = enabled;
            rebuilt.set_state(kept);
            *button = rebuilt;
        }
    }

    /// Paint the pane's settings down `bounds`, drawing the histogram from
    /// `histogram` where one is at hand.
    pub fn render(
        &self,
        surface: &mut Surface,
        bounds: Rect,
        (faces, scale, theme): (Faces, Scale, &Theme),
        histogram: Option<&Histogram>,
    ) {
        let art = Art {
            pane: self,
            histogram,
            scale,
            theme,
        };
        self.panel
            .render(surface, bounds, (faces, scale, theme), &art);
    }

    /// Paint an open list over everything else the window draws.
    pub fn render_popup(
        &self,
        surface: &mut Surface,
        bounds: Rect,
        viewport: Rect,
        context: (Faces, Scale, &Theme),
    ) {
        self.panel.render_popup(surface, bounds, viewport, context);
    }
}

/// A place on the pane a window's test presses.
#[cfg(test)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Spot {
    /// `along` thousandths of the way along the first number's slider or
    /// track.
    Number(u32),
    /// Reset.
    Reset,
    /// Apply.
    Apply,
    /// The Preview switch.
    Preview,
    /// Picker button `index`, Auto the last.
    Picker(usize),
}

#[cfg(test)]
impl AdjustPane {
    /// Where `spot` is, the settings laid out down `bounds`.
    pub(crate) fn spot(
        &self,
        spot: Spot,
        bounds: Rect,
        (faces, scale, theme): (Faces, Scale, &Theme),
    ) -> Option<tairix_geometry::Point> {
        let (role, cell) = match spot {
            Spot::Number(_) => (
                *self
                    .roles
                    .iter()
                    .find(|role| matches!(role, Role::Parameter(_) | Role::Kelvin | Role::Hue))?,
                1,
            ),
            Spot::Reset => (Role::Actions, 0),
            Spot::Apply => (Role::Actions, 1),
            Spot::Preview => (Role::Preview, 0),
            Spot::Picker(index) => (Role::Pickers, index),
        };
        let part = self.roles.iter().position(|&at| at == role)?;
        let control = self
            .panel
            .place_of(part, bounds, faces, scale, theme)?
            .controls[cell];
        Some(match spot {
            Spot::Number(along) => tairix_geometry::Point::new(
                control.left()
                    + i32::try_from(u64::from(control.width) * u64::from(along.min(1000)) / 1000)
                        .ok()?,
                control.top() + i32::try_from(control.height / 2).ok()?,
            ),
            _ => control.center(),
        })
    }
}

impl Setting {
    /// Land entry `index` of the choice playing `role`: which channel, which
    /// hues or which tones the settings show.
    fn choose(&mut self, role: Role, index: usize) {
        match role {
            Role::Channel => {
                self.channel = Channel::ALL.get(index).copied().unwrap_or_default();
                self.curve.select(None);
            }
            Role::Range => self.range = HueRange::ALL.get(index).copied().unwrap_or_default(),
            _ => self.tones = Tones::ALL.get(index).copied().unwrap_or_default(),
        }
    }

    /// Take up picker `button` from the picture, or put it down again,
    /// answering `false` for the button past the pickers, which sets the
    /// adjustment automatically instead.
    fn toggle_pick(&mut self, button: usize) -> bool {
        let picks: &[Pick] = match self.filter {
            Filter::Levels(_) => &LEVEL_PICKS,
            _ => &[Pick::Neutral],
        };
        let Some(&pick) = picks.get(button) else {
            return false;
        };
        self.picking = (self.picking != Some(pick)).then_some(pick);
        true
    }

    /// Land number `cell` of a part playing `role` on the settings.
    fn set_number(&mut self, role: Role, cell: usize, value: i32) {
        let channel = self.channel;
        let narrow = |value: i32| i8::try_from(value.clamp(-100, 100)).unwrap_or(0);
        let level = |value: i32| u8::try_from(value.clamp(0, 255)).unwrap_or(0);
        match (role, &mut self.filter) {
            (Role::Parameter(index), filter) => filter.set(index, value),
            (Role::InputFields, Filter::Levels(levels)) => {
                let levels = levels.of_mut(channel);
                match cell {
                    0 => levels.set_black(level(value)),
                    1 => levels.set_gamma(f64::from(value)),
                    _ => levels.set_white(level(value)),
                }
            }
            (Role::OutputFields, Filter::Levels(levels)) => {
                let levels = levels.of_mut(channel);
                if cell == 0 {
                    levels.out_black = level(value);
                } else {
                    levels.out_white = level(value);
                }
            }
            (Role::Point, Filter::Curves(curves)) => {
                if let Some(index) = self.curve.selected() {
                    let curve = curves.of_mut(channel);
                    if let Some(&(input, output)) = curve.points().get(index) {
                        let to = if cell == 0 {
                            (level(value), output)
                        } else {
                            (input, level(value))
                        };
                        curve.set(index, to);
                    }
                }
            }
            (Role::Hue, Filter::HueSaturation(ranges)) => {
                ranges.of_mut(self.range).hue =
                    i16::try_from(value.clamp(HUE.least, HUE.most)).unwrap_or(0);
            }
            (Role::Saturation, Filter::HueSaturation(ranges)) => {
                ranges.of_mut(self.range).saturation = narrow(value);
            }
            (Role::Lightness, Filter::HueSaturation(ranges)) => {
                ranges.of_mut(self.range).lightness = narrow(value);
            }
            (Role::Axis(axis), Filter::ColourBalance(balance)) => {
                if let Some(slot) = balance.tones[self.tones.index()].get_mut(axis) {
                    *slot = narrow(value);
                }
            }
            (Role::Kelvin, Filter::WhiteBalance(balance)) => {
                balance.kelvin =
                    u16::try_from(value.clamp(KELVIN.least, KELVIN.most)).unwrap_or(balance.kelvin);
            }
            (Role::Tint, Filter::WhiteBalance(balance)) => {
                balance.tint = i16::try_from(value.clamp(TINT.least, TINT.most)).unwrap_or(0);
            }
            _ => {}
        }
    }

    /// Land handle `handle` of a track playing `role` on the settings.
    fn set_handle(&mut self, role: Role, handle: usize, value: i32) {
        let channel = self.channel;
        let level = u8::try_from(value.clamp(0, 255)).unwrap_or(0);
        match (role, &mut self.filter) {
            (Role::Input, Filter::Levels(levels)) => {
                let levels = levels.of_mut(channel);
                match handle {
                    0 => levels.set_black(level),
                    1 => levels.set_grey(f64::from(value)),
                    _ => levels.set_white(level),
                }
            }
            (Role::Output, Filter::Levels(levels)) => {
                let levels = levels.of_mut(channel);
                if handle == 0 {
                    levels.out_black = level;
                } else {
                    levels.out_white = level;
                }
            }
            _ => {}
        }
    }

    /// Show the settings in `part`, which plays `role`; a part held off
    /// while the pane is `withheld` stays so.
    fn show(&self, role: Role, part: &mut Part, withheld: bool) {
        let channel = self.channel;
        match (role, &self.filter) {
            (Role::Parameter(index), filter) => part.show(0, filter.value(index)),
            (Role::Channel, _) => part.show(0, to_i32_index(channel.index())),
            (Role::Range, _) => part.show(0, to_i32_index(self.range.index())),
            (Role::Tones, _) => part.show(0, to_i32_index(self.tones.index())),
            (Role::Input, Filter::Levels(levels)) => {
                let levels = levels.of(channel);
                part.show(0, i32::from(levels.black));
                part.show(1, tairix_util::mathf::round_i32(levels.grey()));
                part.show(2, i32::from(levels.white));
            }
            (Role::InputFields, Filter::Levels(levels)) => {
                let levels = levels.of(channel);
                part.show(0, i32::from(levels.black));
                part.show(1, i32::from(levels.gamma));
                part.show(2, i32::from(levels.white));
            }
            (Role::Output | Role::OutputFields, Filter::Levels(levels)) => {
                let levels = levels.of(channel);
                part.show(0, i32::from(levels.out_black));
                part.show(1, i32::from(levels.out_white));
            }
            (Role::Point, Filter::Curves(curves)) => {
                let chosen = self
                    .curve
                    .selected()
                    .and_then(|index| curves.of(channel).points().get(index).copied());
                let (input, output) = chosen.unwrap_or((0, 0));
                part.show(0, i32::from(input));
                part.show(1, i32::from(output));
                if let Part::Fields(cells) = part {
                    for cell in cells {
                        let mut state = cell.field.state();
                        state.enabled = chosen.is_some() && !withheld;
                        cell.field.set_state(state);
                    }
                }
            }
            (Role::Hue, Filter::HueSaturation(ranges)) => {
                part.show(0, i32::from(ranges.of(self.range).hue));
            }
            (Role::Saturation, Filter::HueSaturation(ranges)) => {
                part.show(0, i32::from(ranges.of(self.range).saturation));
            }
            (Role::Lightness, Filter::HueSaturation(ranges)) => {
                part.show(0, i32::from(ranges.of(self.range).lightness));
            }
            (Role::Axis(axis), Filter::ColourBalance(balance)) => {
                let moved = balance.tones[self.tones.index()]
                    .get(axis)
                    .copied()
                    .unwrap_or(0);
                part.show(0, i32::from(moved));
            }
            (Role::KeepLuminosity, Filter::ColourBalance(balance)) => {
                part.show(0, i32::from(balance.keep_luminosity));
            }
            (Role::Kelvin, Filter::WhiteBalance(balance)) => {
                part.show(0, i32::from(balance.kelvin));
            }
            (Role::Tint, Filter::WhiteBalance(balance)) => part.show(0, i32::from(balance.tint)),
            (Role::Preview, _) => part.show(0, i32::from(self.preview)),
            _ => {}
        }
    }
}

/// A list's index as a part shows it.
fn to_i32_index(index: usize) -> i32 {
    i32::try_from(index).unwrap_or(0)
}

/// What a picker's button says.
const fn pick_label(pick: Pick) -> &'static str {
    match pick {
        Pick::Black => "Black",
        Pick::Grey => "Grey",
        Pick::White => "White",
        Pick::Neutral => "Neutral",
    }
}

/// The parts of a panel being declared, and what each plays.
struct Declared {
    parts: Vec<Part>,
    roles: Vec<Role>,
}

impl Declared {
    fn add(&mut self, part: Part, role: Role) {
        self.parts.push(part);
        self.roles.push(role);
    }

    fn channels(&mut self) {
        let channels: Vec<&str> = Channel::ALL.iter().map(|channel| channel.label()).collect();
        self.add(Part::choice("Channel", &channels, 0), Role::Channel);
    }

    fn levels(&mut self) {
        let (black, grey, white) = (BLACK, GREY, WHITE);
        self.channels();
        self.add(
            Part::Custom {
                height: Height::Ratio(1, 2),
                focusable: false,
            },
            Role::Histogram,
        );
        self.add(
            Part::Track(Track::new(0, 255, &[(0, black), (128, grey), (255, white)])),
            Role::Input,
        );
        self.add(
            Part::Fields(vec![
                Cell {
                    label: "Black",
                    field: NumberField::new(0, 0, 254),
                },
                Cell {
                    label: "Gamma",
                    field: NumberField::new(100, 10, 999).with_decimals(2),
                },
                Cell {
                    label: "White",
                    field: NumberField::new(255, 1, 255),
                },
            ]),
            Role::InputFields,
        );
        self.add(Part::Note(String::from("Output levels")), Role::Note);
        self.add(
            Part::Track(Track::new(0, 255, &[(0, black), (255, white)])),
            Role::Output,
        );
        self.add(
            Part::Fields(vec![
                Cell {
                    label: "Black",
                    field: NumberField::new(0, 0, 255),
                },
                Cell {
                    label: "White",
                    field: NumberField::new(255, 0, 255),
                },
            ]),
            Role::OutputFields,
        );
        let pickers = ["Black", "Grey", "White", "Auto"]
            .iter()
            .map(|&label| Button::labelled(label))
            .collect();
        self.add(Part::Buttons(pickers), Role::Pickers);
    }

    fn curves(&mut self) {
        self.channels();
        self.add(
            Part::Custom {
                height: Height::Ratio(1, 1),
                focusable: true,
            },
            Role::Curve,
        );
        self.add(
            Part::Fields(vec![
                Cell {
                    label: "Input",
                    field: NumberField::new(0, 0, 255),
                },
                Cell {
                    label: "Output",
                    field: NumberField::new(0, 0, 255),
                },
            ]),
            Role::Point,
        );
    }

    fn white_balance(&mut self, balance: WhiteBalance) {
        self.add(
            Part::swept(KELVIN, i32::from(balance.kelvin), WHITE),
            Role::Kelvin,
        );
        self.add(
            Part::swept(TINT, i32::from(balance.tint), WHITE),
            Role::Tint,
        );
        self.add(
            Part::Buttons(vec![Button::labelled("Neutral"), Button::labelled("Auto")]),
            Role::Pickers,
        );
    }

    fn hue_saturation(&mut self) {
        let ranges: Vec<&str> = HueRange::ALL.iter().map(|range| range.label()).collect();
        self.add(Part::choice("Range", &ranges, 0), Role::Range);
        self.add(Part::number(HUE, 0), Role::Hue);
        self.add(Part::number(SATURATION, 0), Role::Saturation);
        self.add(Part::number(LIGHTNESS, 0), Role::Lightness);
        self.add(
            Part::Custom {
                height: Height::Fixed(28),
                focusable: false,
            },
            Role::Spectra,
        );
    }

    fn colour_balance(&mut self, balance: ColourBalance) {
        let bands: Vec<&str> = Tones::ALL.iter().map(|tones| tones.label()).collect();
        self.add(
            Part::choice("Tones", &bands, Tones::Midtones.index()),
            Role::Tones,
        );
        for (axis, (&label, ends)) in AXIS_LABELS.iter().zip(ColourBalance::AXES).enumerate() {
            let parameter = Parameter {
                label,
                least: -100,
                most: 100,
            };
            self.add(Part::between(parameter, 0, ends), Role::Axis(axis));
        }
        self.add(
            switch("Keep luminosity", balance.keep_luminosity),
            Role::KeepLuminosity,
        );
    }

    fn threshold(&mut self, filter: Filter, level: i32) {
        self.add(
            Part::Custom {
                height: Height::Ratio(1, 2),
                focusable: false,
            },
            Role::Histogram,
        );
        if let Some(&parameter) = filter.parameters().first() {
            self.add(Part::swept(parameter, level, GREY), Role::Parameter(0));
        }
    }

    fn numbers(&mut self, filter: Filter) {
        for (index, &parameter) in filter.parameters().iter().enumerate() {
            self.add(
                Part::number(parameter, filter.value(index)),
                Role::Parameter(index),
            );
        }
    }

    fn footing(&mut self, preview: bool) {
        self.add(switch("Preview", preview), Role::Preview);
        self.add(
            Part::Buttons(vec![
                Button::labelled("Reset"),
                Button::new(
                    ButtonContent::Label(String::from("Apply")),
                    ControlRole::Recommended,
                ),
            ]),
            Role::Actions,
        );
    }
}

/// The handles' fills: black, grey and white, as the levels they stand at.
const BLACK: Color = Color::rgba(0, 0, 0, 255);
const GREY: Color = Color::rgba(128, 128, 128, 255);
const WHITE: Color = Color::rgba(255, 255, 255, 255);

/// The panel `setting`'s adjustment declares, and what each part plays.
fn parts_of(setting: &Setting) -> (Vec<Part>, Vec<Role>) {
    let mut declared = Declared {
        parts: Vec::new(),
        roles: Vec::new(),
    };
    match setting.filter {
        Filter::Levels(_) => declared.levels(),
        Filter::Curves(_) => declared.curves(),
        Filter::WhiteBalance(balance) => declared.white_balance(balance),
        Filter::HueSaturation(_) => declared.hue_saturation(),
        Filter::ColourBalance(balance) => declared.colour_balance(balance),
        Filter::Threshold { level } => declared.threshold(setting.filter, level),
        filter => declared.numbers(filter),
    }
    declared.footing(setting.preview);
    (declared.parts, declared.roles)
}

/// What the pane's owner-drawn parts and swept tracks show.
struct Art<'a> {
    pane: &'a AdjustPane,
    histogram: Option<&'a Histogram>,
    scale: Scale,
    theme: &'a Theme,
}

impl Art<'_> {
    fn role(&self, part: usize) -> Option<Role> {
        self.pane.roles.get(part).copied()
    }
}

impl PanelArt for Art<'_> {
    fn draw(&self, surface: &mut Surface, part: usize, rect: Rect) {
        let Some(setting) = &self.pane.setting else {
            return;
        };
        let plot = self.pane.plot();
        match (self.role(part), &setting.filter) {
            (Some(Role::Histogram), filter) => {
                let plot = if matches!(filter, Filter::Threshold { .. }) {
                    Plot::Luma
                } else {
                    plot
                };
                draw_histogram(surface, rect, self.histogram, plot, self.theme);
            }
            (Some(Role::Curve), Filter::Curves(curves)) => setting.curve.render(
                surface,
                rect,
                curves.of(setting.channel),
                self.histogram.map(|histogram| (histogram, plot)),
                self.scale,
                self.theme,
            ),
            (Some(Role::Spectra), Filter::HueSaturation(ranges)) => {
                draw_spectra(
                    surface,
                    rect,
                    (ranges, setting.range),
                    self.scale,
                    self.theme,
                );
            }
            _ => {}
        }
    }

    fn sweeps(&self, part: usize) -> bool {
        matches!(
            self.role(part),
            Some(Role::Input | Role::Output | Role::Kelvin | Role::Tint | Role::Parameter(_))
        )
    }

    fn sweep(&self, part: usize, along: u32) -> Color {
        let grey = |along: u32| {
            let level = u8::try_from(u64::from(along.min(1000)) * 255 / 1000).unwrap_or(u8::MAX);
            Color::rgba(level, level, level, 255)
        };
        let Some(setting) = &self.pane.setting else {
            return grey(along);
        };
        let across = |parameter: Parameter| {
            let span = i64::from(parameter.most - parameter.least);
            i32::try_from(i64::from(parameter.least) + span * i64::from(along.min(1000)) / 1000)
                .unwrap_or(parameter.least)
        };
        let cast = |balance: WhiteBalance| {
            let [r, g, b] = balance.cast().to_array();
            Color::rgba(r, g, b, 255)
        };
        match (self.role(part), setting.filter) {
            (Some(Role::Kelvin), Filter::WhiteBalance(balance)) => cast(WhiteBalance {
                kelvin: u16::try_from(across(KELVIN)).unwrap_or(balance.kelvin),
                ..balance
            }),
            (Some(Role::Tint), Filter::WhiteBalance(balance)) => cast(WhiteBalance {
                tint: i16::try_from(across(TINT)).unwrap_or(0),
                ..balance
            }),
            _ => grey(along),
        }
    }
}

/// The histogram of `plot` drawn up from the foot of `rect`; an empty plate
/// while none is at hand.
fn draw_histogram(
    surface: &mut Surface,
    rect: Rect,
    histogram: Option<&Histogram>,
    plot: Plot,
    theme: &Theme,
) {
    let palette = theme.palette();
    fill_area(surface, rect, Color::from(palette.rim));
    let plate = rect.inset(1);
    fill_area(surface, plate, Color::from(palette.document));
    let Some(histogram) = histogram else {
        return;
    };
    let ink = match plot {
        Plot::Red => Color::rgba(200, 40, 40, 255),
        Plot::Green => Color::rgba(40, 160, 60, 255),
        Plot::Blue => Color::rgba(50, 90, 210, 255),
        Plot::Luma => Color::from(palette.on_surface_muted),
    };
    for column in 0..plate.width {
        let tall = u32::from(histogram.column(plot, column, plate.width));
        let height = u32::try_from(u64::from(plate.height) * u64::from(tall) / 1000).unwrap_or(0);
        let x = plate.left().saturating_add_unsigned(column);
        fill_area(
            surface,
            Rect::new(x, plate.bottom() - to_i32(height), 1, height),
            ink,
        );
    }
}

/// The input hues across the top half of `rect` and what each becomes across
/// the foot, the chosen range's reach marked between them.
fn draw_spectra(
    surface: &mut Surface,
    rect: Rect,
    (ranges, range): (&HueRanges, HueRange),
    scale: Scale,
    theme: &Theme,
) {
    let palette = theme.palette();
    let gap = scale.scale_length(4).max(2);
    let band = rect.height.saturating_sub(gap) / 2;
    let (top, foot) = (rect.top(), rect.bottom() - to_i32(band));
    let width = rect.width.max(1);
    for column in 0..rect.width {
        let degrees = f64::from(column) * 360.0 / f64::from(width);
        let pure = Hsl::new(
            Hue::from_degrees_f64(degrees),
            Fraction::ALL,
            Fraction::from_f64(0.5),
        )
        .to_rgb();
        let moved = ranges.map(pure);
        let x = rect.left().saturating_add_unsigned(column);
        let colour = |rgb: Rgb| Color::rgba(rgb.r, rgb.g, rgb.b, 255);
        fill_area(surface, Rect::new(x, top, 1, band), colour(pure));
        fill_area(surface, Rect::new(x, foot, 1, band), colour(moved));
        if let Some(centre) = range.centre() {
            let turned = degrees - f64::from(centre) + 180.0;
            let off = turned - 360.0 * tairix_util::mathf::floor(turned / 360.0) - 180.0;
            if tairix_util::mathf::fabs(off) <= 60.0 {
                let mark = Rect::new(x, top.saturating_add_unsigned(band), 1, gap);
                fill_area(surface, mark, Color::from(palette.accent));
            }
        }
    }
}

#[cfg(test)]
#[path = "adjust_tests.rs"]
mod tests;
