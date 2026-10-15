//! The active-probing posture the user chooses, persisted in the app's own
//! app-data store and read at start.
//!
//! The posture is the only knob here: it chooses *how politely* the map probes,
//! and each posture carries a fixed preset of rate, concurrency and bounds
//! (`polite` and `thorough` probe the same target set and differ only in pace;
//! `passive` does not probe at all). The raw rate and concurrency knobs are a
//! later, fuller settings pane. The one key round-trips through the shared
//! config engine, and an unknown stored value keeps the default rather than
//! being silently rewritten.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_appconf::{overwrite, Live, Registry};
use tairix_controls::{
    stack, ComboBox, FieldAction, FieldControl, FieldGroup, FieldGroupAction, FieldLayout,
    FieldRow, Keystroke,
};
use tairix_geometry::{Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use crate::probe::ProbeProfile;

/// How politely — if at all — the map actively probes for reachability.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Posture {
    /// No active probing: the map keeps the liveness it already has.
    Passive,
    /// A gentle sweep, the default.
    #[default]
    Polite,
    /// A faster, wider sweep, still bounded.
    Thorough,
}

impl Posture {
    /// Every posture, in the order a chooser lists them.
    pub const ALL: [Self; 3] = [Self::Passive, Self::Polite, Self::Thorough];

    /// The probe preset this posture resolves to — the single source the
    /// engine reads.
    #[must_use]
    pub const fn profile(self) -> ProbeProfile {
        match self {
            Self::Passive => ProbeProfile::PASSIVE,
            Self::Polite => ProbeProfile::POLITE,
            Self::Thorough => ProbeProfile::THOROUGH,
        }
    }

    /// Its stored spelling.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Passive => "passive",
            Self::Polite => "polite",
            Self::Thorough => "thorough",
        }
    }

    /// Its chooser label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Passive => "Off (don't probe)",
            Self::Polite => "Polite",
            Self::Thorough => "Thorough",
        }
    }

    /// A one-line description of what it does.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Passive => "Draw only liveness already known; send no probes.",
            Self::Polite => "A gentle liveness sweep and a well-known-port check.",
            Self::Thorough => "A faster, wider sweep of the same hosts and ports.",
        }
    }

    /// The posture a stored token names, or `None` for an unknown one.
    #[must_use]
    pub fn from_token(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|posture| posture.token() == text)
    }
}

/// The app's persisted probe settings: just the posture, for now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProbeSettings {
    /// The chosen posture.
    pub posture: Posture,
}

/// The one setting [`ProbeSettings`] holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeKey {
    /// The probe posture.
    Posture,
}

/// The posture's key in the store.
const POSTURE_KEY: &str = "probe.posture";

impl Registry for ProbeSettings {
    type Key = ProbeKey;
    const KEYS: &'static [ProbeKey] = &[ProbeKey::Posture];

    fn name(key: ProbeKey) -> &'static str {
        match key {
            ProbeKey::Posture => POSTURE_KEY,
        }
    }

    fn read(&mut self, key: ProbeKey, text: &str) -> bool {
        match key {
            ProbeKey::Posture => match Posture::from_token(text) {
                Some(posture) => {
                    self.posture = posture;
                    true
                }
                // An unknown value is refused, not clamped, so the default stands.
                None => false,
            },
        }
    }

    fn spell(&self, key: ProbeKey, out: &mut String) -> bool {
        match key {
            ProbeKey::Posture => {
                out.push_str(self.posture.token());
                true
            }
        }
    }
}

impl Live for ProbeSettings {
    fn take(&mut self, other: &Self, key: ProbeKey) -> bool {
        match key {
            ProbeKey::Posture => overwrite(&mut self.posture, other.posture),
        }
    }
}

/// The settings window's title.
pub const TITLE: &str = "Probe Settings";

/// The window's width in logical pixels: room for the posture chooser and a
/// line saying what the postures do.
const PANE_WIDTH: u32 = 380;

/// The static line under the chooser.
const DESCRIPTION: &str =
    "Polite and Thorough probe the same hosts and differ only in pace; Off sends no probes.";

/// What the window's input asks of the app.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Request {
    /// The user chose this posture: adopt it, probe at it, and keep it.
    Settle(Posture),
    /// Close the window.
    Close,
}

/// The probe-settings window's content: a posture chooser and nothing else.
#[derive(Debug)]
pub struct ProbePane {
    group: FieldGroup,
    shown: Posture,
    extent: (u32, u32),
}

impl ProbePane {
    /// The pane showing `posture` at `scale`, the chooser focused.
    #[must_use]
    pub fn new(posture: Posture, scale: Scale, theme: &Theme) -> Self {
        let mut group = posture_group(posture);
        group.adopt_focus(Some(0));
        let mut pane = Self {
            group,
            shown: posture,
            extent: (0, 0),
        };
        pane.fit(scale, theme);
        pane
    }

    /// The pane's extent in its own pixels.
    #[must_use]
    pub const fn extent(&self) -> (u32, u32) {
        self.extent
    }

    /// The extent the pane wants at `scale`, which it takes as its own.
    pub fn fit(&mut self, scale: Scale, theme: &Theme) -> (u32, u32) {
        let pane_w = scale.scale_length(PANE_WIDTH);
        let plate = stack::plate_width(pane_w, scale, theme);
        let pane_h = stack::height([height_of(&self.group, plate, scale, theme)], scale, theme);
        self.extent = (pane_w, pane_h);
        self.extent
    }

    /// Lay out in the frame the window actually has.
    pub fn set_extent(&mut self, extent: (u32, u32)) {
        self.extent = extent;
    }

    /// Draw the pane into `surface`.
    pub fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        surface.fill(Color::from(theme.palette().surface));
        let layout = self.place(scale, theme);
        self.group.render(surface, layout, scale, theme);
        // A choice list hangs over the row, so it is drawn last.
        if !layout.popup.is_empty() {
            self.group.render_popup(surface, layout.popup, scale, theme);
        }
    }

    /// Feed a pointer event, answering what it asked of the app.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Request> {
        let layout = self.place(scale, theme);
        let action = self.group.on_pointer(event, layout, scale, theme, damage)?;
        self.act(&action)
    }

    /// Feed a key press. `Escape` closes an open choice list, else the window.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Request> {
        let layout = self.place(scale, theme);
        let was_open = self.list_open();
        if let Some(action) = self.group.on_key(stroke, layout, scale, theme, damage) {
            return self.act(&action);
        }
        if matches!(stroke.key, Key::Named(NamedKey::Escape)) && !was_open {
            return Some(Request::Close);
        }
        None
    }

    /// Adopt `posture` as the durable choice, reporting what moved. A chooser
    /// already showing it reports nothing.
    pub fn adopt(&mut self, posture: Posture, damage: &mut Region) {
        if self.shown == posture {
            return;
        }
        let index = Posture::ALL.iter().position(|p| *p == posture).unwrap_or(0);
        if let FieldControl::Combo(combo) = self.group.rows_mut()[0].control_mut() {
            combo.set_selected(index);
        }
        self.shown = posture;
        damage.add(Rect::new(0, 0, self.extent.0, self.extent.1));
    }

    fn act(&mut self, action: &FieldGroupAction) -> Option<Request> {
        match action.action {
            FieldAction::Selected { index } => {
                let posture = *Posture::ALL.get(index)?;
                self.shown = posture;
                Some(Request::Settle(posture))
            }
            _ => None,
        }
    }

    fn list_open(&self) -> bool {
        matches!(self.group.rows().first().map(FieldRow::control), Some(FieldControl::Combo(combo)) if combo.is_expanded())
    }

    fn place(&self, scale: Scale, theme: &Theme) -> FieldLayout {
        let bounds = Rect::new(0, 0, self.extent.0, self.extent.1);
        let plate = stack::plate_width(self.extent.0, scale, theme);
        let height = height_of(&self.group, plate, scale, theme);
        let rect = stack::place(bounds, 1, scale, theme, |_| height)
            .first()
            .map_or(Rect::EMPTY, |(_, rect)| *rect);
        self.group.layout(rect, bounds, scale, theme)
    }
}

/// The one field group: the posture chooser.
fn posture_group(posture: Posture) -> FieldGroup {
    let choices = Posture::ALL
        .iter()
        .map(|p| String::from(p.label()))
        .collect();
    let index = Posture::ALL.iter().position(|p| *p == posture).unwrap_or(0);
    let row = FieldRow::new(
        "Probing",
        FieldControl::Combo(ComboBox::new(choices).with_selected(index)),
    )
    .with_description(DESCRIPTION);
    FieldGroup::new("REACHABILITY", Vec::from([row]))
}

fn height_of(group: &FieldGroup, width: u32, scale: Scale, theme: &Theme) -> u32 {
    group.measured_height(width, group.slot_column(width, scale, theme), scale, theme)
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
