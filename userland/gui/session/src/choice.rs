//! The two-button question the desktop session puts up and answers itself — a
//! power action's confirmation, a resolution's trial — so every such prompt
//! opens, fits the screen, takes keys and clicks, and is drawn through one
//! definition.

use alloc::string::String;

use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode};
use tairix_controls::{damage, Dialog, DialogAction, FocusState, PointerState};
use tairix_geometry::Scale;
use tairix_wm::{
    Compositor, InputEvent, Key, NamedKey, Point, PointerButton, Rect, Surface, WindowId,
};

use crate::shell::{prompt_origin, DesktopShell};

/// The index of the button that declines: *Cancel*, *Revert*.
pub(crate) const DECLINE: usize = 0;

/// The index of the button that accepts.
pub(crate) const ACCEPT: usize = 1;

/// How a prompt was answered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Choice {
    /// The declining button, or `Escape`.
    Declined,
    /// The accepting button.
    Accepted,
}

/// A showing two-button prompt.
pub(crate) struct ChoicePrompt {
    wm: WindowId,
    dialog: Dialog,
    /// Its size in logical pixels.
    size: (u32, u32),
    /// The physical extent it was last drawn at, which a click is read
    /// against.
    extent: (u32, u32),
}

impl ChoicePrompt {
    /// Open `dialog`, whose button [`DECLINE`] declines and [`ACCEPT`]
    /// accepts, at `size` logical pixels and titled `title`, or `None` when
    /// its surface or its window cannot be had.
    pub(crate) fn open(
        dialog: Dialog,
        size: (u32, u32),
        title: &str,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Self> {
        let extent = extent(&dialog, size, compositor, shell);
        let surface = render(&dialog, extent, compositor.scale(), shell)?;
        let wm = shell.open_prompt(compositor, surface, title)?;
        Some(Self {
            wm,
            dialog,
            size,
            extent,
        })
    }

    /// The prompt's compositor window.
    pub(crate) const fn wm(&self) -> WindowId {
        self.wm
    }

    /// The sentence the prompt asks with.
    pub(crate) fn message(&self) -> Option<&str> {
        self.dialog.message()
    }

    /// Ask with `message` instead, redrawn at once.
    pub(crate) fn set_message(
        &mut self,
        message: String,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) {
        self.dialog.set_message(message);
        self.repaint(shell, compositor);
    }

    /// One key press: `Escape` declines, `Enter` presses the focused button,
    /// and `Tab` or an arrow moves the focus to the other.
    pub(crate) fn key(
        &mut self,
        key: &KeyInput,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Choice> {
        let KeyInput::Pressed { key, .. } = key else {
            return None;
        };
        match key {
            KeyValue::Named(NamedKeyCode::Escape) => Some(Choice::Declined),
            KeyValue::Named(NamedKeyCode::Tab | NamedKeyCode::Left | NamedKeyCode::Right) => {
                let next = if focused(&self.dialog) == Some(DECLINE) {
                    ACCEPT
                } else {
                    DECLINE
                };
                set_focus(&mut self.dialog, next);
                self.repaint(shell, compositor);
                None
            }
            KeyValue::Named(NamedKeyCode::Enter) => {
                choice(self.dialog.on_key(Key::Named(NamedKey::Enter)))
            }
            _ => None,
        }
    }

    /// One primary-button click at the prompt-local `local`. A button answers
    /// on the completed click, so the press and the release are both fed
    /// where the user pressed; a click beside the buttons answers nothing.
    pub(crate) fn click(
        &mut self,
        local: Point,
        shell: &DesktopShell,
        compositor: &Compositor,
    ) -> Option<Choice> {
        let scale = compositor.scale();
        let bounds = Rect::new(0, 0, self.extent.0, self.extent.1);
        let theme = shell.session().active_theme();
        let mut reported = damage::sink();
        let dialog = &mut self.dialog;
        let mut feed =
            |event: InputEvent| dialog.on_pointer(&event, bounds, scale, theme, &mut reported);
        let _ = feed(InputEvent::PointerMoved { to: local });
        let _ = feed(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        choice(feed(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        }))
    }

    /// Redraw the prompt in the look and at the scale now in use, fitted to
    /// the screen and placed on it again. A surface that cannot be built
    /// leaves the previous frame up.
    pub(crate) fn repaint(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        let extent = extent(&self.dialog, self.size, compositor, shell);
        if let Some(surface) = render(&self.dialog, extent, compositor.scale(), shell) {
            self.extent = extent;
            let _ = compositor.set_surface(self.wm, surface);
            let _ =
                compositor.move_window(self.wm, prompt_origin(compositor.screen_rect(), extent));
        }
    }

    /// Take the prompt down.
    pub(crate) fn close(self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        let _ = shell.close_window(compositor, self.wm);
    }
}

/// A dialog action as an answer. Any index but the two buttons a prompt built
/// answers nothing, leaving the question up rather than guessing which way
/// the user meant it.
fn choice(action: Option<DialogAction>) -> Option<Choice> {
    match action {
        Some(DialogAction::ActionActivated { index: ACCEPT }) => Some(Choice::Accepted),
        Some(DialogAction::ActionActivated { index: DECLINE }) => Some(Choice::Declined),
        _ => None,
    }
}

/// The button holding the focus, if one does.
fn focused(dialog: &Dialog) -> Option<usize> {
    dialog
        .actions()
        .iter()
        .position(|button| button.state().focus.focused)
}

/// Give the focus to the button at `index` alone.
fn set_focus(dialog: &mut Dialog, index: usize) {
    for (position, button) in dialog.actions_mut().iter_mut().enumerate() {
        let focus = if position == index {
            FocusState::FOCUSED
        } else {
            FocusState::default()
        };
        button.set_state(
            button
                .state()
                .with_focus(focus)
                .with_pointer(PointerState::None),
        );
    }
}

/// `dialog`'s physical extent for `size` logical pixels on `compositor`'s
/// screen: no wider than the screen, so a small desktop at a large scale
/// strands no button, and tall enough for its message wrapped at that width.
fn extent(
    dialog: &Dialog,
    (width, height): (u32, u32),
    compositor: &Compositor,
    shell: &DesktopShell,
) -> (u32, u32) {
    let scale = compositor.scale();
    let screen = compositor.screen_rect();
    let width = scale.scale_length(width).min(screen.width);
    let wrapped = dialog.height_for_content(0, width, scale, shell.session().active_theme());
    let height = scale.scale_length(height).max(wrapped).min(screen.height);
    (width, height)
}

/// Paint `dialog` at the physical `extent` through the active theme.
fn render(
    dialog: &Dialog,
    (width, height): (u32, u32),
    scale: Scale,
    shell: &DesktopShell,
) -> Option<Surface> {
    let mut surface = Surface::new(width, height)?;
    dialog.render(
        &mut surface,
        Rect::new(0, 0, width, height),
        scale,
        shell.session().active_theme(),
    );
    Some(surface)
}

#[cfg(test)]
#[path = "choice_tests.rs"]
mod tests;
