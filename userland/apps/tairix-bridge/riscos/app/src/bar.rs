//! A screen window's bar and its menu (`plans/RISCOS-EMULATOR.md` §15.3): a
//! strip above the picture whose one button opens the window's menu — Full
//! Screen, sharp or smooth scaling, Pause and Reset — since the secondary
//! button is the guest's Menu.

use tairix_abi::window_ipc::{AppMenu, AppMenuItemId, AppMenuMark, AppMenuRow, AppMenuSymbol};
use tairix_abi::Errno;
use tairix_controls::{fill_area, ground_fill, Button, ButtonAction, ChromeLayer};
use tairix_geometry::{Rect, Region, Scale};
use tairix_input::InputEvent;
use tairix_machines::menu::{item, marked, Acts};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use crate::settings::Scaling;

/// What a screen's menu asks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    /// Full Screen, or the window again.
    FullScreen,
    /// Scale the picture so.
    Scaling(Scaling),
    /// Hold the context where it stands, or let it go on.
    Pause,
    /// End the context and launch its command line again here.
    Reset,
}

/// What a screen's menu shows of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shown {
    /// The window is full screen.
    pub fullscreen: bool,
    /// How its picture is scaled.
    pub scaling: Scaling,
    /// Its context is paused.
    pub paused: bool,
}

const FULL_SCREEN: u16 = 1;
const SHARP: u16 = 2;
const SMOOTH: u16 = 3;
const PAUSE: u16 = 4;
const RESET: u16 = 5;

/// The menu of a screen shown as `shown` says.
///
/// # Errors
///
/// The menu builder's refusal.
pub fn menu(shown: &Shown) -> Result<AppMenu, Errno> {
    let mut menu = AppMenu::EMPTY;
    let check = |on| (marked(on, AppMenuMark::Check), Acts::InPlace);
    let radio = |on| (marked(on, AppMenuMark::Radio), Acts::InPlace);
    menu.push(item(
        FULL_SCREEN,
        "Full Screen",
        AppMenuSymbol::Display,
        check(shown.fullscreen),
    )?)?;
    menu.push(AppMenuRow::Separator)?;
    let sharp = shown.scaling == Scaling::Sharp;
    menu.push(item(
        SHARP,
        "Sharp",
        AppMenuSymbol::PixelGrid,
        radio(sharp),
    )?)?;
    menu.push(item(
        SMOOTH,
        "Smooth",
        AppMenuSymbol::ZoomFit,
        radio(!sharp),
    )?)?;
    menu.push(AppMenuRow::Separator)?;
    menu.push(item(
        PAUSE,
        "Pause",
        AppMenuSymbol::Pause,
        check(shown.paused),
    )?)?;
    menu.push(item(
        RESET,
        "Reset",
        AppMenuSymbol::Refresh,
        (AppMenuMark::None, Acts::Elsewhere),
    )?)?;
    Ok(menu)
}

/// The command row `item` is.
#[must_use]
pub fn command(item: AppMenuItemId) -> Option<Command> {
    match item.get() {
        FULL_SCREEN => Some(Command::FullScreen),
        SHARP => Some(Command::Scaling(Scaling::Sharp)),
        SMOOTH => Some(Command::Scaling(Scaling::Smooth)),
        PAUSE => Some(Command::Pause),
        RESET => Some(Command::Reset),
        _ => None,
    }
}

/// The strip above a screen's picture.
pub struct Bar {
    button: Button,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            button: Button::labelled("Screen"),
        }
    }
}

impl Bar {
    /// Its height.
    #[must_use]
    pub fn height(scale: Scale, theme: &Theme) -> u32 {
        let metrics = theme.metrics();
        scale
            .scale_length(metrics.control_height)
            .saturating_add(2 * scale.scale_length(metrics.control_gap))
    }

    /// The strip painted across `width` of `surface`'s top.
    pub fn render(&self, surface: &mut Surface, width: u32, scale: Scale, theme: &Theme) {
        let ground = ground_fill(theme, theme.palette().surface, ChromeLayer::Ground);
        fill_area(
            surface,
            Rect::new(0, 0, width, Self::height(scale, theme)),
            Color::from(ground),
        );
        self.button
            .render(surface, self.button_rect(width, scale, theme), scale, theme);
    }

    /// `event` taken over a bar `width` across: where to open the menu, once
    /// its button is pressed.
    pub fn on_input(
        &mut self,
        event: &InputEvent,
        width: u32,
        (scale, theme): (Scale, &Theme),
        damage: &mut Region,
    ) -> Option<Rect> {
        let bounds = self.button_rect(width, scale, theme);
        (self.button.on_pointer(event, bounds, damage) == Some(ButtonAction::Activated))
            .then_some(bounds)
    }

    /// Whether `at` is on the bar.
    #[must_use]
    pub fn covers(at: (u32, u32), scale: Scale, theme: &Theme) -> bool {
        at.1 < Self::height(scale, theme)
    }

    fn button_rect(&self, width: u32, scale: Scale, theme: &Theme) -> Rect {
        let gap = scale.scale_length(theme.metrics().control_gap);
        let wide = self.button.measured_width(scale, theme);
        let x = width.saturating_sub(wide + gap);
        Rect::new(
            i32::try_from(x).unwrap_or(0),
            i32::try_from(gap).unwrap_or(0),
            wide,
            scale.scale_length(theme.metrics().control_height),
        )
    }
}

#[cfg(test)]
mod tests;
