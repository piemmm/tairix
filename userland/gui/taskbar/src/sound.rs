//! The bar's sound: the output the volume signal stands for, the panel it
//! opens, and the recording indicator (`plans/SOUND.md` §Desktop
//! integration).
//!
//! Both signals are the session's own pixels, drawn from what the audio
//! service reports, so no program can hide that the machine is recording.
//! The panel's level moves the device live as it is dragged; the session
//! coalesces those moves and remembers the level where the drag settles.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::audio::AudioGain;
use tairix_audio::volume::{level_at_permille, permille_of_level};
use tairix_controls::{
    paint_surface_plate, plate_border, AuthorityState, ChromeLayer, ControlState, SelectorAction,
    Slider, SliderAction, Toggle,
};
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};

use crate::edge::Edge;
use crate::layout::{local_rect, BarLayout};
use crate::library::panel_origin;
use crate::notifications::{IconId, StatusKind, StatusSignal};

/// The notification-area id the volume signal goes by.
pub const VOLUME_SIGNAL: IconId = IconId(0x736f_756e_6401);

/// The notification-area id the recording indicator goes by.
pub const RECORDING_SIGNAL: IconId = IconId(0x736f_756e_6402);

/// The panel's width in logical pixels: a device name and a slider long
/// enough to set a level with.
const PANEL_WIDTH: u32 = 240;

/// The default sink, as the bar shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputState {
    /// Its id this boot, the one its controls are changed through.
    pub device_id: u32,
    /// What it is called.
    pub name: String,
    /// Its own level.
    pub level: AudioGain,
    /// Whether it is muted.
    pub muted: bool,
    /// Whether this session may change its controls.
    pub may_change: bool,
}

/// What the bar shows of sound.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SoundState {
    /// The default sink, while there is one.
    pub output: Option<OutputState>,
    /// Whether any capture stream on the machine is moving frames.
    pub recording: bool,
}

impl SoundState {
    /// The signals this stands for, the volume's first.
    pub(crate) fn signals(&self) -> Vec<StatusSignal> {
        let mut signals = Vec::with_capacity(2);
        if let Some(output) = &self.output {
            let kind = if output.muted {
                StatusKind::Muted
            } else {
                StatusKind::Volume
            };
            signals.push(StatusSignal::new(VOLUME_SIGNAL, kind));
        }
        if self.recording {
            signals.push(StatusSignal::new(RECORDING_SIGNAL, StatusKind::Recording));
        }
        signals
    }
}

/// What a change in the panel asks of the session.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SoundAction {
    /// Set the output's level: live while it is dragged, and `settled` where
    /// the drag rests, which is where the session remembers it.
    Level {
        /// The output.
        device_id: u32,
        /// Its new level.
        level: AudioGain,
        /// Whether the interaction has ended.
        settled: bool,
    },
    /// Mute or unmute the output.
    Mute {
        /// The output.
        device_id: u32,
        /// Whether it is now muted.
        muted: bool,
    },
}

/// What one input did to the open panel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum PanelOutcome {
    /// Nothing of the panel's moved.
    Ignored,
    /// The panel's pixels moved and nothing is asked.
    Changed,
    /// The panel's pixels moved and the session is asked to act.
    Act(SoundAction),
    /// The panel is to close.
    Dismiss,
}

/// Where the open panel and its parts lie, in screen coordinates.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SoundPanelLayout {
    /// The whole panel.
    pub panel: Rect,
    /// The radius the window manager rounds it with, the plate's own.
    pub corner_radius: u32,
    /// The output's name.
    pub name: Rect,
    /// The level slider.
    pub slider: Rect,
    /// The mute toggle.
    pub mute: Rect,
}

impl SoundPanelLayout {
    /// The panel opened outward from `anchor`, the volume signal's slot on
    /// `bar` pinned to `edge`.
    pub(crate) fn compute(
        edge: Edge,
        bar: &BarLayout,
        anchor: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Self {
        let metrics = theme.metrics();
        let inset = scale.scale_length(metrics.control_inset);
        let gap = scale.scale_length(metrics.control_gap);
        let row = scale.scale_length(metrics.control_height);
        let line = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale).line_height();
        let width = scale.scale_length(PANEL_WIDTH);
        let height = inset * 2 + line + gap * 2 + row * 2;
        let origin = panel_origin(edge, bar.bar, anchor, width, height);
        let inner = width.saturating_sub(inset * 2);
        let left = origin.x.saturating_add(to_i32(inset));
        let mut top = origin.y.saturating_add(to_i32(inset));
        let mut next = |height: u32| {
            let rect = Rect::new(left, top, inner, height);
            top = top.saturating_add(to_i32(height + gap));
            rect
        };
        let name = next(line);
        let slider = next(row);
        let mute = next(row);
        Self {
            panel: Rect::new(origin.x, origin.y, width, height),
            corner_radius: scale.scale_length(metrics.popup_corner_radius),
            name,
            slider,
            mute,
        }
    }
}

/// The volume panel the signal opens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoundPanel {
    open: bool,
    output: Option<OutputState>,
    slider: Slider,
    mute: Toggle,
    /// A drag in progress owns the slider, so a report arriving mid-drag
    /// cannot pull it from under the pointer.
    dragging: bool,
}

impl Default for SoundPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl SoundPanel {
    /// A closed panel with no output.
    #[must_use]
    pub fn new() -> Self {
        Self {
            open: false,
            output: None,
            slider: Slider::new(0),
            mute: Toggle::new("Mute", false),
            dragging: false,
        }
    }

    /// Whether the panel is open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// The output the panel stands for.
    #[must_use]
    pub const fn output(&self) -> Option<&OutputState> {
        self.output.as_ref()
    }

    /// Open the panel, if there is an output to show. Answers whether it
    /// opened.
    pub(crate) fn open(&mut self) -> bool {
        self.open = self.output.is_some();
        self.dragging = false;
        self.open
    }

    /// Close the panel.
    pub(crate) fn close(&mut self) {
        self.open = false;
        self.dragging = false;
    }

    /// Adopt the latest report, answering whether the panel's pixels moved.
    /// An output that went away closes the panel.
    pub(crate) fn adopt(&mut self, output: Option<OutputState>) -> bool {
        if self.output == output {
            return false;
        }
        match &output {
            Some(shown) => {
                if !self.dragging {
                    self.slider.set_value(permille_of_level(shown.level));
                }
                self.mute.set_on(shown.muted);
                let state = if shown.may_change {
                    ControlState::idle()
                } else {
                    ControlState::idle().with_authority(AuthorityState::Denied)
                };
                self.slider.set_state(state.with_enabled(shown.may_change));
                self.mute.set_state(state.with_enabled(shown.may_change));
            }
            None => self.close(),
        }
        self.output = output;
        true
    }

    /// Feed a pointer event, in screen coordinates, to the open panel.
    pub(crate) fn on_pointer(
        &mut self,
        event: &InputEvent,
        pointer: Point,
        layout: &SoundPanelLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> PanelOutcome {
        let Some(output) = self.output.as_ref() else {
            return PanelOutcome::Dismiss;
        };
        let device_id = output.device_id;
        if matches!(event, InputEvent::PointerPressed { .. })
            && !layout.panel.contains(pointer)
            && !self.dragging
        {
            return PanelOutcome::Dismiss;
        }
        if let Some(action) = self
            .slider
            .on_pointer(event, layout.slider, scale, theme, damage)
        {
            let (SliderAction::SetValue { permille } | SliderAction::Settled { permille }) = action;
            let settled = matches!(action, SliderAction::Settled { .. });
            self.dragging = !settled;
            let level = level_at_permille(permille);
            if let Some(shown) = self.output.as_mut() {
                shown.level = level;
            }
            return PanelOutcome::Act(SoundAction::Level {
                device_id,
                level,
                settled,
            });
        }
        if matches!(
            event,
            InputEvent::PointerReleased {
                button: PointerButton::Primary
            }
        ) {
            self.dragging = false;
        }
        match self.mute.on_pointer(event, layout.mute, damage) {
            Some(SelectorAction::Set { on }) => self.muted(device_id, on),
            None if damage.is_empty() => PanelOutcome::Ignored,
            None => PanelOutcome::Changed,
        }
    }

    /// Feed a key to the open panel: Escape closes it, or ends a drag in
    /// progress where it stands, and the arrows step the level.
    pub(crate) fn on_key(
        &mut self,
        key: Key,
        layout: &SoundPanelLayout,
        damage: &mut Region,
    ) -> PanelOutcome {
        let Some((device_id, level)) = self
            .output
            .as_ref()
            .map(|shown| (shown.device_id, shown.level))
        else {
            return PanelOutcome::Dismiss;
        };
        if key == Key::Named(NamedKey::Escape) {
            if !core::mem::take(&mut self.dragging) {
                return PanelOutcome::Dismiss;
            }
            return PanelOutcome::Act(SoundAction::Level {
                device_id,
                level,
                settled: true,
            });
        }
        match self.slider.on_key(key, layout.slider, damage) {
            Some(SliderAction::SetValue { permille } | SliderAction::Settled { permille }) => {
                let level = level_at_permille(permille);
                if let Some(shown) = self.output.as_mut() {
                    shown.level = level;
                }
                PanelOutcome::Act(SoundAction::Level {
                    device_id,
                    level,
                    settled: true,
                })
            }
            None => PanelOutcome::Ignored,
        }
    }

    fn muted(&mut self, device_id: u32, muted: bool) -> PanelOutcome {
        if let Some(shown) = self.output.as_mut() {
            shown.muted = muted;
        }
        PanelOutcome::Act(SoundAction::Mute { device_id, muted })
    }

    /// Paint the open panel into its own surface.
    pub(crate) fn paint(
        &self,
        layout: &SoundPanelLayout,
        scale: Scale,
        theme: &Theme,
        surface: &mut Surface,
    ) {
        let Some(output) = self.output.as_ref() else {
            return;
        };
        let origin = layout.panel.origin;
        let _ = paint_surface_plate(
            surface,
            (0, 0, layout.panel.width, layout.panel.height),
            (layout.corner_radius, plate_border(theme, scale)),
            theme,
            (theme.palette().surface_raised, ChromeLayer::Ground),
        );
        let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
        let name = local_rect(layout.name, origin);
        let fitted = font.truncate_to_width(&output.name, name.width);
        font.draw_text(
            surface,
            name.left(),
            name.top(),
            fitted,
            Color::from(theme.palette().on_surface),
        );
        self.slider
            .render(surface, local_rect(layout.slider, origin), scale, theme);
        self.mute
            .render(surface, local_rect(layout.mute, origin), scale, theme);
    }
}

/// Saturating `u32` → `i32`.
fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
#[path = "sound_tests.rs"]
mod tests;
