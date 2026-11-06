//! The Adjustments and Effects panes: one type serving both, a permanent
//! selector of its family's operations, the chosen operation's settings, and
//! then Preview, Reset and Apply.
//!
//! The pane holds settings and nothing else; what the picture shows of them and
//! applying them are the window's. A setting moved opens the pane's operation,
//! and the window's one preview follows the pane moved last, leaving the
//! other's first. Leaving an operation applies it only where its settings were
//! moved and it changes something ([`OpPane::applying`]), and drops it
//! otherwise, so looking down the list applies nothing; the pane keeps an
//! operation's settings until the window has applied it.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_colour::{Fraction, Hsl, Hue, Rgb};
use tairix_controls::{fill_area, Button, ButtonContent, ControlRole, Keystroke, NumberField};
use tairix_geometry::{to_i32, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;
use tairix_util::mathf;

use crate::curve_graph::{CurveEdit, CurveGraph};
use crate::effect::{Distribution, Noise};
use crate::histogram::{Histogram, Plot};
use crate::layout::{BodyHeights, Faces, Fit};
use crate::op::{Family, Op, Parameter};
use crate::panel::{
    switch, Cell, Height, Panel, PanelArt, PanelEvent, PanelOutcome, Part, MOST_PARTS,
};
use crate::tone::{
    Channel, ChannelLevels, ChannelMixer, ColourBalance, Curves, HueRange, HueRanges, Tones,
    WhiteBalance,
};
use crate::track::Track;

/// What an eyedropper takes from the picture.
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
pub enum OpOutcome {
    /// None of the pane's.
    Ignored,
    /// The pane's, the settings unchanged.
    Taken,
    /// The settings moved and the operation is open; `settled` once the
    /// interaction is over.
    Changed {
        /// Whether the interaction is over.
        settled: bool,
    },
    /// Preview was turned on or off; turned on, the operation is open.
    Previewed,
    /// Apply was pressed.
    Apply,
    /// The selector chose operation `to` of the family, which the window
    /// opens ([`OpPane::choose`]) once it has left the one the pane holds.
    Switched {
        /// Where the operation chosen stands in the family's list.
        to: usize,
    },
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
    /// The selector of the family's operations.
    Choose,
    /// A number the operation is set by: an entry of its parameters.
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
    /// The output channel the channel mixer shows.
    Mixer,
    /// The shown channel's weight of input `0`, `1` or `2`, or the constant.
    Mix(usize),
    /// Whether the channel mixer makes one grey, or the speckle is grey.
    Monochrome,
    /// How noise is spread.
    Distribution,
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

/// The chosen operation's settings and what the pane shows of them.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Setting {
    start: Op,
    op: Op,
    channel: Channel,
    range: HueRange,
    tones: Tones,
    /// The output channel the channel mixer shows.
    output: usize,
    curve: CurveGraph,
    picking: Option<Pick>,
    preview: bool,
    /// Whether a histogram of the picture as it stands is at hand, which Auto
    /// reads.
    histogram_ready: bool,
}

/// The Adjustments pane or the Effects pane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpPane {
    family: Family,
    panel: Panel,
    roles: Vec<Role>,
    setting: Setting,
    open: bool,
}

impl OpPane {
    /// The pane of `family`'s operations, its first chosen at its starting
    /// settings and not yet open.
    #[must_use]
    pub fn new(family: Family) -> Self {
        let mut pane = Self {
            family,
            panel: Panel::new(Vec::new()),
            roles: Vec::new(),
            setting: Setting::of(family.first()),
            open: false,
        };
        pane.rebuild();
        pane
    }

    /// Which operations it offers.
    #[must_use]
    pub const fn family(&self) -> Family {
        self.family
    }

    /// What its band says.
    #[must_use]
    pub const fn title(&self) -> &'static str {
        self.family.label()
    }

    /// The chosen operation, as set.
    #[must_use]
    pub const fn op(&self) -> Op {
        self.setting.op
    }

    /// Whether its operation is open: shown over the picture while Preview is
    /// on, and applied or dropped when it is left.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the picture shows its operation.
    #[must_use]
    pub const fn previewing(&self) -> bool {
        self.open && self.setting.preview
    }

    /// Whether its settings were moved from where the operation started.
    #[must_use]
    pub fn moved(&self) -> bool {
        self.setting.op != self.setting.start
    }

    /// The eyedropper taken up, if one is: the next press on the picture is
    /// its.
    #[must_use]
    pub const fn picking(&self) -> Option<Pick> {
        self.setting.picking
    }

    /// Open operation `index` of the family at its starting settings, letting
    /// the settings of the one chosen go: leaving that one is the window's to
    /// settle first ([`applying`](Self::applying)). The operation already
    /// chosen is opened as it is set.
    pub fn choose(&mut self, index: usize) {
        let Some(&op) = self.family.ops().get(index) else {
            return;
        };
        if !op.same_kind(&self.setting.op) {
            let selector_focused = self.panel.focus().is_some_and(|(part, _)| part == 0);
            let preview = self.setting.preview;
            self.setting = Setting::of(op);
            self.setting.preview = preview;
            self.rebuild();
            if selector_focused {
                self.panel.focus_at((0, 0));
            }
        }
        self.open = true;
    }

    /// Drop its operation without applying it: back to the starting settings,
    /// closed.
    pub fn close(&mut self) {
        self.restart();
    }

    /// Its operation was applied: back to the starting settings, closed, and
    /// Add noise drawn afresh, so applying it again lays new speckle.
    pub fn applied(&mut self) {
        if let Op::AddNoise(noise) = &mut self.setting.start {
            noise.seed = tairix_mix::mix64(noise.seed);
        }
        self.restart();
    }

    /// Return the settings to where the operation started, keeping it open as
    /// it was; answers whether they moved.
    pub fn reset(&mut self) -> bool {
        let moved = self.moved();
        self.setting.op = self.setting.start;
        self.setting.picking = None;
        self.setting.curve.select(None);
        self.sync();
        moved
    }

    /// The operation as set, where leaving it applies it: its settings were
    /// moved and it changes something.
    #[must_use]
    pub fn applying(&self) -> Option<Op> {
        let op = self.setting.op;
        (self.moved() && !op.is_identity()).then_some(op)
    }

    /// Back to the starting settings, closed.
    fn restart(&mut self) {
        self.reset();
        self.open = false;
    }

    /// Whether the pane reads a histogram of the picture: to draw one, or for
    /// Auto.
    #[must_use]
    pub const fn reads_histogram(&self) -> bool {
        matches!(
            self.setting.op,
            Op::Levels(_) | Op::Curves(_) | Op::Threshold { .. } | Op::WhiteBalance(_)
        )
    }

    /// Say whether a histogram of the picture as it stands is at hand, which
    /// Auto needs; answers whether what the pane draws changed.
    pub fn set_histogram_ready(&mut self, ready: bool) -> bool {
        if self.setting.histogram_ready == ready {
            return false;
        }
        self.setting.histogram_ready = ready;
        self.sync_pickers();
        true
    }

    /// The plot the histogram shows.
    const fn plot(&self) -> Plot {
        match self.setting.channel {
            Channel::Composite => Plot::Luma,
            Channel::Red => Plot::Red,
            Channel::Green => Plot::Green,
            Channel::Blue => Plot::Blue,
        }
    }

    /// Withhold the pane while the picture cannot take its operation, or offer
    /// it again.
    pub fn set_withheld(&mut self, withheld: bool) {
        self.panel.set_withheld(withheld);
        self.setting.curve.set_enabled(!withheld);
        if withheld {
            self.setting.picking = None;
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
        self.panel.holding() || self.setting.curve.dragging()
    }

    /// Whether the pane has the keyboard.
    #[must_use]
    pub const fn has_focus(&self) -> bool {
        self.panel.focus().is_some()
    }

    /// Where the part holding the keyboard is laid out down `bounds`: what a
    /// dock scrolling the pane's body brings into view.
    #[must_use]
    pub fn focus_rect(
        &self,
        bounds: Rect,
        (faces, scale, theme): (Faces, Scale, &Theme),
    ) -> Option<Rect> {
        let (part, _) = self.panel.focus()?;
        self.panel
            .place_of(part, bounds, faces, scale, theme)
            .map(|placed| placed.rect)
    }

    /// How tall the pane's body stands across `width`: the least its dock
    /// seats it at is its selector, so the operation chosen always shows; what
    /// shows it whole is every part at full height; and the dock lays it out
    /// whole and scrolls what lies between.
    #[must_use]
    pub fn heights(&self, width: u32, faces: Faces, scale: Scale, theme: &Theme) -> BodyHeights {
        BodyHeights {
            least: self.panel.part_height(0, width, faces, scale, theme),
            natural: self.panel.measured_height(width, faces, scale, theme),
            fit: Fit::Scrolled,
        }
    }

    /// Put the eyedropper down, answering whether one was up.
    pub fn put_down_pick(&mut self) -> bool {
        if self.setting.picking.take().is_none() {
            return false;
        }
        self.sync_pickers();
        true
    }

    /// The eyedropper took `colour` from the picture: set from it, opening the
    /// operation, and put the eyedropper down. Answers whether the settings
    /// moved.
    pub fn picked(&mut self, colour: Rgb) -> bool {
        let Some(pick) = self.setting.picking.take() else {
            return false;
        };
        let before = self.setting.op;
        match (&mut self.setting.op, pick) {
            (Op::Levels(levels), Pick::Black) => levels.black_point(colour),
            (Op::Levels(levels), Pick::Grey) => levels.grey_point(colour),
            (Op::Levels(levels), Pick::White) => levels.white_point(colour),
            (Op::WhiteBalance(balance), Pick::Neutral) => {
                if let Some(neutral) = WhiteBalance::neutralising(colour) {
                    *balance = neutral;
                }
            }
            _ => {}
        }
        let moved = self.setting.op != before;
        self.open |= moved;
        self.sync();
        moved
    }

    /// Auto, reading `histogram`: levels stretched over what each channel
    /// holds, or the light balanced to the picture's mean, opening the
    /// operation. Answers whether the settings moved.
    pub fn auto(&mut self, histogram: &Histogram) -> bool {
        let before = self.setting.op;
        match &mut self.setting.op {
            Op::Levels(levels) => levels.auto(histogram.colours(), AUTO_CLIP),
            Op::WhiteBalance(balance) => {
                let mean = histogram.mean();
                if let Some(neutral) = mean.and_then(WhiteBalance::neutralising_linear) {
                    *balance = neutral;
                }
            }
            _ => {}
        }
        let moved = self.setting.op != before;
        self.open |= moved;
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
    ) -> OpOutcome {
        let outcome =
            self.panel
                .on_pointer(event, (bounds, viewport), (faces, scale, theme), damage);
        let answer = match outcome {
            PanelOutcome::Custom { part, rect } => {
                self.custom_pointer(part, rect, event, scale, damage)
            }
            other => self.answer(other, bounds, (faces, scale, theme), damage),
        };
        self.follow_focus();
        answer
    }

    /// Feed a key: Escape with nothing typed to take back returns an open
    /// operation's settings to where it started, and is the window's while
    /// the pane is closed and unmoved.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        (bounds, viewport): (Rect, Rect),
        (faces, scale, theme): (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> OpOutcome {
        let outcome = self
            .panel
            .on_key(stroke, (bounds, viewport), (faces, scale, theme), damage);
        let answer = match outcome {
            PanelOutcome::Custom { part, rect } => self.custom_key(part, rect, stroke, damage),
            PanelOutcome::Ignored
                if stroke.key == Key::Named(NamedKey::Escape) && (self.open || self.moved()) =>
            {
                damage.add(bounds);
                OpOutcome::Changed {
                    settled: self.reset(),
                }
            }
            other => self.answer(other, bounds, (faces, scale, theme), damage),
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
    ) -> OpOutcome {
        match self.panel.commit(bounds, context, damage) {
            Some(event) => self.answer(PanelOutcome::Event(event), bounds, context, damage),
            None => OpOutcome::Ignored,
        }
    }

    /// Settle the keyboard's typing and take the keyboard from the pane.
    pub fn blur(
        &mut self,
        bounds: Rect,
        context: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> OpOutcome {
        let answer = match self.panel.blur(bounds, context, damage) {
            Some(event) => self.answer(PanelOutcome::Event(event), bounds, context, damage),
            None => OpOutcome::Ignored,
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

    /// Paint the pane's body down `bounds`, drawing the histogram from
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

    /// Where an open list is drawn; empty while none is.
    #[must_use]
    pub fn popup_rect(
        &self,
        bounds: Rect,
        viewport: Rect,
        (faces, scale, theme): (Faces, Scale, &Theme),
    ) -> Rect {
        self.panel.popup_rect(bounds, viewport, faces, scale, theme)
    }

    /// The curve editor has the keyboard while its part does.
    fn follow_focus(&mut self) {
        let curve = |(part, _): (usize, usize)| self.roles.get(part) == Some(&Role::Curve);
        let focused = self.panel.focus().is_some_and(curve);
        self.setting.curve.focus(focused);
    }

    /// What the panel's outcome comes to for the settings.
    fn answer(
        &mut self,
        outcome: PanelOutcome,
        bounds: Rect,
        look: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> OpOutcome {
        match outcome {
            PanelOutcome::Ignored => OpOutcome::Ignored,
            PanelOutcome::Taken | PanelOutcome::Custom { .. } => OpOutcome::Taken,
            PanelOutcome::Left { forward } => OpOutcome::Left { forward },
            PanelOutcome::Event(event) => self.event(event, bounds, look, damage),
        }
    }

    fn event(
        &mut self,
        event: PanelEvent,
        bounds: Rect,
        look: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> OpOutcome {
        let part = match event {
            PanelEvent::Chosen { part, .. }
            | PanelEvent::Number { part, .. }
            | PanelEvent::Pressed { part, .. }
            | PanelEvent::Switched { part, .. }
            | PanelEvent::Moved { part, .. } => part,
        };
        let Some(&role) = self.roles.get(part) else {
            return OpOutcome::Ignored;
        };
        let before = self.setting.op;
        let settled = match event {
            PanelEvent::Chosen { index, .. } => {
                return self.chose(role, index, bounds, look, damage)
            }
            PanelEvent::Pressed { button, .. } => {
                return self.pressed(role, button, bounds, damage)
            }
            PanelEvent::Switched { on, .. } if role == Role::Preview => {
                self.setting.preview = on;
                self.open |= on;
                self.sync();
                damage.add(bounds);
                return OpOutcome::Previewed;
            }
            PanelEvent::Switched { on, .. } => {
                self.setting.set_switch(role, on);
                true
            }
            PanelEvent::Number {
                cell,
                value,
                settled,
                ..
            } => {
                self.setting.set_number(role, cell, value);
                settled
            }
            PanelEvent::Moved {
                handle,
                value,
                settled,
                ..
            } => {
                self.setting.set_handle(role, handle, value);
                settled
            }
        };
        self.landed(before, settled, bounds, look, damage)
    }

    /// The settings were `before` and are now as they stand: repaint the parts
    /// that now show something else, and open the operation where they moved
    /// or the interaction settled.
    fn landed(
        &mut self,
        before: Op,
        settled: bool,
        bounds: Rect,
        look: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> OpOutcome {
        let moved = self.setting.op != before;
        let changed = self.sync_changed();
        self.panel.report_parts(&changed, bounds, look, damage);
        if moved || settled {
            self.open = true;
            OpOutcome::Changed { settled }
        } else {
            OpOutcome::Taken
        }
    }

    /// A list chose `index`: another operation in the selector, the noise's
    /// spread, or which channel, range, band or output the settings show.
    fn chose(
        &mut self,
        role: Role,
        index: usize,
        bounds: Rect,
        look: (Faces, Scale, &Theme),
        damage: &mut Region,
    ) -> OpOutcome {
        match role {
            Role::Choose => {
                let chosen = self.family.ops().get(index);
                if chosen.is_none_or(|op| op.same_kind(&self.setting.op)) {
                    return OpOutcome::Taken;
                }
                // The selector shows what is set until the window has left it.
                self.sync();
                damage.add(bounds);
                OpOutcome::Switched { to: index }
            }
            Role::Distribution => {
                let before = self.setting.op;
                if let Op::AddNoise(noise) = &mut self.setting.op {
                    noise.distribution = Distribution::ALL.get(index).copied().unwrap_or_default();
                }
                self.landed(before, true, bounds, look, damage)
            }
            _ => {
                // What the settings show changed, not the settings.
                self.setting.choose(role, index);
                self.sync();
                damage.add(bounds);
                OpOutcome::Taken
            }
        }
    }

    /// Button `button` of a part playing `role` was pressed.
    fn pressed(
        &mut self,
        role: Role,
        button: usize,
        bounds: Rect,
        damage: &mut Region,
    ) -> OpOutcome {
        match role {
            Role::Pickers => {
                if !self.setting.toggle_pick(button) {
                    return OpOutcome::Auto;
                }
                self.sync_pickers();
                damage.add(bounds);
                OpOutcome::Picking
            }
            Role::Actions if button == 0 => {
                let moved = self.reset();
                damage.add(bounds);
                OpOutcome::Changed { settled: moved }
            }
            Role::Actions => OpOutcome::Apply,
            _ => OpOutcome::Taken,
        }
    }

    fn custom_pointer(
        &mut self,
        part: usize,
        rect: Rect,
        event: &InputEvent,
        scale: Scale,
        damage: &mut Region,
    ) -> OpOutcome {
        if self.roles.get(part) != Some(&Role::Curve) {
            let press = matches!(
                event,
                InputEvent::PointerPressed {
                    button: PointerButton::Primary
                }
            );
            return if press {
                OpOutcome::Taken
            } else {
                OpOutcome::Ignored
            };
        }
        let Op::Curves(curves) = self.setting.op else {
            return OpOutcome::Ignored;
        };
        let curve = *curves.of(self.setting.channel);
        let edit = self
            .setting
            .curve
            .on_pointer(event, rect, &curve, scale, damage);
        self.curve_edited(edit)
    }

    fn custom_key(
        &mut self,
        part: usize,
        rect: Rect,
        stroke: Keystroke,
        damage: &mut Region,
    ) -> OpOutcome {
        if self.roles.get(part) != Some(&Role::Curve) {
            return OpOutcome::Ignored;
        }
        let Op::Curves(curves) = self.setting.op else {
            return OpOutcome::Ignored;
        };
        let curve = *curves.of(self.setting.channel);
        let edit = self.setting.curve.on_key(stroke, rect, &curve, damage);
        self.curve_edited(edit)
    }

    fn curve_edited(&mut self, edit: Option<CurveEdit>) -> OpOutcome {
        let outcome = match edit {
            Some(CurveEdit::Changed { curve, settled }) => {
                let channel = self.setting.channel;
                if let Op::Curves(curves) = &mut self.setting.op {
                    *curves.of_mut(channel) = curve;
                }
                self.open = true;
                OpOutcome::Changed { settled }
            }
            Some(CurveEdit::Chose) => OpOutcome::Taken,
            None => return OpOutcome::Ignored,
        };
        self.sync();
        outcome
    }

    /// Declare the chosen operation's parts afresh, keeping the pane withheld
    /// if it was.
    fn rebuild(&mut self) {
        let withheld = self.panel.withheld();
        let (parts, roles) = parts_of(self.family, &self.setting);
        self.panel = Panel::new(parts);
        self.roles = roles;
        self.set_withheld(withheld);
        self.sync();
    }

    /// Show the settings in every part.
    fn sync(&mut self) {
        let _ = self.sync_changed();
    }

    /// Show the settings in every part, answering which parts now draw
    /// something else.
    fn sync_changed(&mut self) -> [bool; MOST_PARTS] {
        let mut changed = [false; MOST_PARTS];
        let withheld = self.panel.withheld();
        for ((index, &role), changed) in self.roles.iter().enumerate().zip(&mut changed) {
            if let Some(part) = self.panel.part_mut(index) {
                *changed = self.setting.show(role, part, withheld);
            }
        }
        self.sync_pickers();
        changed
    }

    /// Show which eyedropper is taken up, and offer Auto only with a histogram
    /// at hand.
    fn sync_pickers(&mut self) {
        let (picking, ready) = (self.setting.picking, self.setting.histogram_ready);
        let picks = picks_of(&self.setting.op);
        let withheld = self.panel.withheld();
        let Some(index) = self.roles.iter().position(|&role| role == Role::Pickers) else {
            return;
        };
        let Some(Part::Buttons(buttons)) = self.panel.part_mut(index) else {
            return;
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
            let mut kept = state;
            kept.enabled = enabled;
            let same = button.role() == role
                && matches!(button.content(), ButtonContent::Label(shown) if shown == label);
            if same {
                button.set_state(kept);
                continue;
            }
            let mut rebuilt = Button::new(ButtonContent::Label(String::from(label)), role);
            rebuilt.set_state(kept);
            *button = rebuilt;
        }
    }
}

/// A place on the pane a window's test presses.
#[cfg(test)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Spot {
    /// The selector.
    Selector,
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
impl OpPane {
    /// Where `spot` is, the pane laid out down `bounds`.
    pub(crate) fn spot(
        &self,
        spot: Spot,
        bounds: Rect,
        (faces, scale, theme): (Faces, Scale, &Theme),
    ) -> Option<tairix_geometry::Point> {
        let number = |role: &&Role| {
            matches!(
                role,
                Role::Parameter(_) | Role::Kelvin | Role::Hue | Role::Mix(_)
            )
        };
        let (role, cell) = match spot {
            Spot::Selector => (Role::Choose, 0),
            Spot::Number(_) => (*self.roles.iter().find(number)?, 1),
            Spot::Reset => (Role::Actions, 0),
            Spot::Apply => (Role::Actions, 1),
            Spot::Preview => (Role::Preview, 0),
            Spot::Picker(index) => (Role::Pickers, index),
        };
        let part = self.roles.iter().position(|&at| at == role)?;
        let placed = self.panel.place_of(part, bounds, faces, scale, theme)?;
        let control = *placed.controls.get(cell)?;
        Some(match spot {
            Spot::Number(along) => {
                let across = u64::from(control.width) * u64::from(along.min(1000)) / 1000;
                tairix_geometry::Point::new(
                    control.left() + i32::try_from(across).ok()?,
                    control.top() + i32::try_from(control.height / 2).ok()?,
                )
            }
            _ => control.center(),
        })
    }
}

impl Setting {
    /// The settings of `op`, as it starts.
    fn of(op: Op) -> Self {
        Self {
            start: op,
            op,
            channel: Channel::Composite,
            range: HueRange::Master,
            tones: Tones::Midtones,
            output: 0,
            curve: CurveGraph::default(),
            picking: None,
            preview: true,
            histogram_ready: false,
        }
    }

    /// Land entry `index` of the list playing `role`: which channel, hues,
    /// tones or output the settings show.
    fn choose(&mut self, role: Role, index: usize) {
        match role {
            Role::Channel => {
                self.channel = Channel::ALL.get(index).copied().unwrap_or_default();
                self.curve.select(None);
            }
            Role::Range => self.range = HueRange::ALL.get(index).copied().unwrap_or_default(),
            Role::Tones => self.tones = Tones::ALL.get(index).copied().unwrap_or_default(),
            Role::Mixer => self.output = index.min(ChannelMixer::OUTPUTS.len() - 1),
            _ => {}
        }
    }

    /// Take up picker `button` from the picture, or put it down again,
    /// answering `false` for the button past the pickers, which sets the
    /// operation automatically instead.
    fn toggle_pick(&mut self, button: usize) -> bool {
        let Some(&pick) = picks_of(&self.op).get(button) else {
            return false;
        };
        self.picking = (self.picking != Some(pick)).then_some(pick);
        true
    }

    /// Land switch `role` on the settings.
    fn set_switch(&mut self, role: Role, on: bool) {
        match (role, &mut self.op) {
            (Role::KeepLuminosity, Op::ColourBalance(balance)) => balance.keep_luminosity = on,
            (Role::Monochrome, Op::ChannelMixer(mixer)) => mixer.monochrome = on,
            (Role::Monochrome, Op::AddNoise(noise)) => noise.monochrome = on,
            _ => {}
        }
    }

    /// Land number `cell` of a part playing `role` on the settings.
    fn set_number(&mut self, role: Role, cell: usize, value: i32) {
        let (channel, range, tones, shown) = (self.channel, self.range, self.tones, self.output);
        let chosen = self.curve.selected();
        let narrow = |value: i32| i8::try_from(value.clamp(-100, 100)).unwrap_or(0);
        let level = |value: i32| u8::try_from(value.clamp(0, 255)).unwrap_or(0);
        match (role, &mut self.op) {
            (Role::Parameter(index), op) => op.set(index, value),
            (Role::InputFields, Op::Levels(levels)) => {
                let levels = levels.of_mut(channel);
                match cell {
                    0 => levels.set_black(level(value)),
                    1 => levels.set_gamma(f64::from(value)),
                    _ => levels.set_white(level(value)),
                }
            }
            (Role::OutputFields, Op::Levels(levels)) => {
                let levels = levels.of_mut(channel);
                if cell == 0 {
                    levels.out_black = level(value);
                } else {
                    levels.out_white = level(value);
                }
            }
            (Role::Point, Op::Curves(curves)) => {
                let curve = curves.of_mut(channel);
                if let Some(index) = chosen {
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
            (Role::Hue, Op::HueSaturation(ranges)) => {
                ranges.of_mut(range).hue =
                    i16::try_from(value.clamp(HUE.least, HUE.most)).unwrap_or(0);
            }
            (Role::Saturation, Op::HueSaturation(ranges)) => {
                ranges.of_mut(range).saturation = narrow(value);
            }
            (Role::Lightness, Op::HueSaturation(ranges)) => {
                ranges.of_mut(range).lightness = narrow(value);
            }
            (Role::Axis(axis), Op::ColourBalance(balance)) => {
                if let Some(slot) = balance.tones[tones.index()].get_mut(axis) {
                    *slot = narrow(value);
                }
            }
            (Role::Kelvin, Op::WhiteBalance(balance)) => {
                balance.kelvin =
                    u16::try_from(value.clamp(KELVIN.least, KELVIN.most)).unwrap_or(balance.kelvin);
            }
            (Role::Tint, Op::WhiteBalance(balance)) => {
                balance.tint = i16::try_from(value.clamp(TINT.least, TINT.most)).unwrap_or(0);
            }
            (Role::Mix(input), Op::ChannelMixer(mixer)) => mixer.set(shown, input, value),
            _ => {}
        }
    }

    /// Land handle `handle` of a track playing `role` on the settings.
    fn set_handle(&mut self, role: Role, handle: usize, value: i32) {
        let channel = self.channel;
        let level = u8::try_from(value.clamp(0, 255)).unwrap_or(0);
        match (role, &mut self.op) {
            (Role::Input, Op::Levels(levels)) => {
                let levels = levels.of_mut(channel);
                match handle {
                    0 => levels.set_black(level),
                    1 => levels.set_grey(f64::from(value)),
                    _ => levels.set_white(level),
                }
            }
            (Role::Output, Op::Levels(levels)) => {
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

    /// Show the settings in `part`, which plays `role`, answering whether that
    /// changed what it draws; a part held off while the pane is `withheld`
    /// stays so.
    fn show(&self, role: Role, part: &mut Part, withheld: bool) -> bool {
        match (role, &self.op) {
            (
                Role::Input | Role::InputFields | Role::Output | Role::OutputFields,
                Op::Levels(levels),
            ) => show_levels(role, *levels.of(self.channel), part),
            (Role::Point, Op::Curves(curves)) => self.show_point(curves, part, withheld),
            (role, op) => self.show_value(role, op, part),
        }
    }

    /// Show the chosen curve point's input and output, offering its fields
    /// only while a point is chosen.
    fn show_point(&self, curves: &Curves, part: &mut Part, withheld: bool) -> bool {
        let points = curves.of(self.channel).points();
        let chosen = self
            .curve
            .selected()
            .and_then(|index| points.get(index).copied());
        let (input, output) = chosen.unwrap_or((0, 0));
        let mut changed = shown(part, &[i32::from(input), i32::from(output)]);
        if let Part::Fields(cells) = part {
            for cell in cells {
                let mut state = cell.field.state();
                let enabled = chosen.is_some() && !withheld;
                changed |= state.enabled != enabled;
                state.enabled = enabled;
                cell.field.set_state(state);
            }
        }
        changed
    }

    /// Show the one value a part playing `role` shows.
    fn show_value(&self, role: Role, op: &Op, part: &mut Part) -> bool {
        let index = |at: usize| i32::try_from(at).unwrap_or(0);
        let value = match (role, op) {
            (Role::Choose, op) => index(op.place()),
            (Role::Parameter(at), op) => op.value(at),
            (Role::Channel, _) => index(self.channel.index()),
            (Role::Range, _) => index(self.range.index()),
            (Role::Tones, _) => index(self.tones.index()),
            (Role::Mixer, _) => index(self.output),
            (Role::Hue, Op::HueSaturation(ranges)) => i32::from(ranges.of(self.range).hue),
            (Role::Saturation, Op::HueSaturation(ranges)) => {
                i32::from(ranges.of(self.range).saturation)
            }
            (Role::Lightness, Op::HueSaturation(ranges)) => {
                i32::from(ranges.of(self.range).lightness)
            }
            (Role::Axis(axis), Op::ColourBalance(balance)) => {
                let band = balance.tones[self.tones.index()];
                i32::from(band.get(axis).copied().unwrap_or(0))
            }
            (Role::KeepLuminosity, Op::ColourBalance(balance)) => {
                i32::from(balance.keep_luminosity)
            }
            (Role::Kelvin, Op::WhiteBalance(balance)) => i32::from(balance.kelvin),
            (Role::Tint, Op::WhiteBalance(balance)) => i32::from(balance.tint),
            (Role::Mix(input), Op::ChannelMixer(mixer)) => {
                i32::from(mixer.row(self.output).get(input).copied().unwrap_or(0))
            }
            (Role::Monochrome, Op::ChannelMixer(mixer)) => i32::from(mixer.monochrome),
            (Role::Monochrome, Op::AddNoise(noise)) => i32::from(noise.monochrome),
            (Role::Distribution, Op::AddNoise(noise)) => index(noise.distribution.index()),
            (Role::Preview, _) => i32::from(self.preview),
            _ => return false,
        };
        part.show(0, value)
    }
}

/// Show levels `levels` in a part playing `role`: the input handles or their
/// fields, or the output's.
fn show_levels(role: Role, levels: ChannelLevels, part: &mut Part) -> bool {
    match role {
        Role::Input => shown(
            part,
            &[
                i32::from(levels.black),
                mathf::round_i32(levels.grey()),
                i32::from(levels.white),
            ],
        ),
        Role::InputFields => shown(
            part,
            &[
                i32::from(levels.black),
                i32::from(levels.gamma),
                i32::from(levels.white),
            ],
        ),
        _ => shown(
            part,
            &[i32::from(levels.out_black), i32::from(levels.out_white)],
        ),
    }
}

/// Show `values` in `part`'s cells, every one of them, answering whether any
/// changed what it draws.
fn shown(part: &mut Part, values: &[i32]) -> bool {
    values
        .iter()
        .enumerate()
        .fold(false, |changed, (cell, &value)| {
            part.show(cell, value) || changed
        })
}

/// The eyedroppers `op` offers, in its buttons' order; Auto follows them.
const fn picks_of(op: &Op) -> &'static [Pick] {
    match op {
        Op::Levels(_) => &LEVEL_PICKS,
        _ => &[Pick::Neutral],
    }
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

    fn selector(&mut self, family: Family, op: Op) {
        let labels: Vec<&str> = family.ops().iter().map(Op::label).collect();
        let selector = Part::choice(family.caption(), &labels, op.place());
        self.add(selector, Role::Choose);
    }

    fn channels(&mut self) {
        let channels: Vec<&str> = Channel::ALL.iter().map(|channel| channel.label()).collect();
        self.add(Part::choice("Channel", &channels, 0), Role::Channel);
    }

    fn levels(&mut self) {
        self.channels();
        self.add(
            Part::Custom {
                height: Height::Ratio(1, 2),
                focusable: false,
            },
            Role::Histogram,
        );
        self.add(
            Part::Track(Track::new(0, 255, &[(0, BLACK), (128, GREY), (255, WHITE)])),
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
            Part::Track(Track::new(0, 255, &[(0, BLACK), (255, WHITE)])),
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

    fn threshold(&mut self, op: Op, level: i32) {
        self.add(
            Part::Custom {
                height: Height::Ratio(1, 2),
                focusable: false,
            },
            Role::Histogram,
        );
        if let Some(&parameter) = op.parameters().first() {
            self.add(Part::swept(parameter, level, GREY), Role::Parameter(0));
        }
    }

    fn mixer(&mut self, mixer: ChannelMixer, output: usize) {
        let outputs = ChannelMixer::OUTPUTS;
        self.add(Part::choice("Output", &outputs, output), Role::Mixer);
        let row = mixer.row(output);
        for (input, (&label, &weight)) in ChannelMixer::INPUTS.iter().zip(&row).enumerate() {
            let (least, most) = if input == 3 {
                ChannelMixer::CONSTANT
            } else {
                ChannelMixer::WEIGHT
            };
            let parameter = Parameter { label, least, most };
            self.add(Part::number(parameter, i32::from(weight)), Role::Mix(input));
        }
        self.add(switch("Monochrome", mixer.monochrome), Role::Monochrome);
    }

    fn noise(&mut self, op: Op, noise: Noise) {
        self.numbers(op);
        let spreads: Vec<&str> = Distribution::ALL
            .iter()
            .map(|distribution| distribution.label())
            .collect();
        let chosen = noise.distribution.index();
        self.add(
            Part::choice("Distribution", &spreads, chosen),
            Role::Distribution,
        );
        self.add(switch("Monochrome", noise.monochrome), Role::Monochrome);
    }

    fn note(&mut self, text: &str) {
        self.add(Part::Note(String::from(text)), Role::Note);
    }

    fn numbers(&mut self, op: Op) {
        for (index, &parameter) in op.parameters().iter().enumerate() {
            self.add(
                Part::number(parameter, op.value(index)),
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

/// The panel `setting`'s operation declares beneath `family`'s selector, and
/// what each part plays.
fn parts_of(family: Family, setting: &Setting) -> (Vec<Part>, Vec<Role>) {
    let mut declared = Declared {
        parts: Vec::new(),
        roles: Vec::new(),
    };
    declared.selector(family, setting.op);
    match setting.op {
        Op::Levels(_) => declared.levels(),
        Op::Curves(_) => declared.curves(),
        Op::WhiteBalance(balance) => declared.white_balance(balance),
        Op::HueSaturation(_) => declared.hue_saturation(),
        Op::ColourBalance(balance) => declared.colour_balance(balance),
        Op::Threshold { level } => declared.threshold(setting.op, level),
        Op::ChannelMixer(mixer) => declared.mixer(mixer, setting.output),
        Op::AddNoise(noise) => declared.noise(setting.op, noise),
        Op::Desaturate => declared.note("Each colour becomes its grey."),
        Op::Invert => declared.note("Each colour becomes its opposite."),
        Op::FindEdges => declared.note("The edges between colours are lit."),
        op => declared.numbers(op),
    }
    declared.footing(setting.preview);
    (declared.parts, declared.roles)
}

/// What the pane's owner-drawn parts and swept tracks show.
struct Art<'a> {
    pane: &'a OpPane,
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
        let setting = &self.pane.setting;
        let plot = self.pane.plot();
        match (self.role(part), &setting.op) {
            (Some(Role::Histogram), op) => {
                let plot = if matches!(op, Op::Threshold { .. }) {
                    Plot::Luma
                } else {
                    plot
                };
                draw_histogram(surface, rect, self.histogram, plot, self.theme);
            }
            (Some(Role::Curve), Op::Curves(curves)) => setting.curve.render(
                surface,
                rect,
                curves.of(setting.channel),
                self.histogram.map(|histogram| (histogram, plot)),
                self.scale,
                self.theme,
            ),
            (Some(Role::Spectra), Op::HueSaturation(ranges)) => {
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
        let across = |parameter: Parameter| {
            let span = i64::from(parameter.most - parameter.least);
            i32::try_from(i64::from(parameter.least) + span * i64::from(along.min(1000)) / 1000)
                .unwrap_or(parameter.least)
        };
        let cast = |balance: WhiteBalance| {
            let [r, g, b] = balance.cast().to_array();
            Color::rgba(r, g, b, 255)
        };
        match (self.role(part), self.pane.setting.op) {
            (Some(Role::Kelvin), Op::WhiteBalance(balance)) => cast(WhiteBalance {
                kelvin: u16::try_from(across(KELVIN)).unwrap_or(balance.kelvin),
                ..balance
            }),
            (Some(Role::Tint), Op::WhiteBalance(balance)) => cast(WhiteBalance {
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
    for (column, tall) in (0..plate.width).zip(histogram.columns(plot, plate.width)) {
        let tall = u32::from(tall);
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
            let off = turned - 360.0 * mathf::floor(turned / 360.0) - 180.0;
            if mathf::fabs(off) <= 60.0 {
                let mark = Rect::new(x, top.saturating_add_unsigned(band), 1, gap);
                fill_area(surface, mark, Color::from(palette.accent));
            }
        }
    }
}

#[cfg(test)]
#[path = "op_pane_tests.rs"]
mod tests;
