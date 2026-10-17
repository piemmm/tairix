//! The two-button question the desktop session puts up and answers itself — a
//! power action's confirmation, a resolution's trial, the picker's question
//! whether to replace a file — as an alert of the desktop's own, so it is
//! drawn, fitted, armed and answered exactly as every application's alert is.

use tairix_abi::input::KeyInput;
use tairix_abi::window_ipc::{AlertButton, AlertButtonRole, AlertSeverity, AlertSpec};
use tairix_wm::{Compositor, Point, WindowId};

use crate::alert::{alert_title, AlertWindow, Speaker};
use crate::shell::DesktopShell;

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

/// The question `headline`, explained by `detail`, declined by `decline` and
/// accepted by `accept` in its role. A headline quoting a name too long for an
/// alert is shortened, never refused; `None` only for a button label no alert
/// can carry — a compiled-in word, so a defect the tests catch.
pub(crate) fn question(
    headline: &str,
    detail: &str,
    decline: &str,
    (accept, role): (&str, AlertButtonRole),
) -> Option<AlertSpec> {
    let buttons = [
        AlertButton::new(decline, AlertButtonRole::Cancel).ok()?,
        AlertButton::new(accept, role).ok()?,
    ];
    AlertSpec::fitted(AlertSeverity::Question, headline, detail, &buttons).ok()
}

/// A showing two-button prompt.
pub(crate) struct ChoicePrompt {
    window: AlertWindow,
}

impl ChoicePrompt {
    /// Open `spec`, whose button [`DECLINE`] declines and [`ACCEPT`]
    /// accepts, holding window `owner` when it is some, or `None` when its
    /// surface or its window cannot be had. It takes the keyboard: the user
    /// has just asked for what it confirms. Its buttons arm once it is
    /// [`stamp`](Self::stamp)ed with the moment it is first on screen.
    pub(crate) fn open(
        spec: &AlertSpec,
        owner: Option<WindowId>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Self> {
        let title = alert_title(spec.severity(), Speaker::Desktop);
        AlertWindow::open(spec, &title, owner, (0, true, false), shell, compositor)
            .map(|window| Self { window })
    }

    /// Start the arm delay from `now_ns`, the moment the prompt is first on
    /// screen; a prompt already armed keeps its moment.
    pub(crate) fn stamp(&mut self, now_ns: u64) {
        self.window.stamp(now_ns);
    }

    /// The prompt's compositor window.
    pub(crate) const fn wm(&self) -> WindowId {
        self.window.wm()
    }

    /// The centre of the button at `index`, in its client: where a test
    /// presses it.
    #[cfg(test)]
    pub(crate) fn button_centre(
        &self,
        index: usize,
        shell: &DesktopShell,
        compositor: &Compositor,
    ) -> Point {
        let rect = self.window.button_rects(shell, compositor)[index];
        Point::new(
            rect.left() + i32::try_from(rect.width / 2).unwrap_or(0),
            rect.top() + i32::try_from(rect.height / 2).unwrap_or(0),
        )
    }

    /// What the prompt says beneath its question.
    pub(crate) fn detail(&self) -> &str {
        self.window.detail()
    }

    /// Say `detail` instead, redrawn at once.
    pub(crate) fn set_detail(
        &mut self,
        detail: &str,
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) {
        self.window.set_detail(detail, shell, compositor);
    }

    /// One key press at `now_ns`: `Escape` declines, `Enter` accepts when the
    /// accepting button is the default, and `Tab` or an arrow moves the focus.
    pub(crate) fn key(
        &mut self,
        key: &KeyInput,
        now_ns: u64,
        shell: &DesktopShell,
        compositor: &Compositor,
    ) -> Option<Choice> {
        choice(self.window.key(key, now_ns, shell, compositor))
    }

    /// One primary-button click at the prompt-local `local` at `now_ns`; a
    /// click beside the buttons answers nothing.
    pub(crate) fn click(
        &mut self,
        local: Point,
        now_ns: u64,
        shell: &DesktopShell,
        compositor: &Compositor,
    ) -> Option<Choice> {
        choice(self.window.click(local, now_ns, shell, compositor))
    }

    /// Redraw the prompt in the look and at the scale now in use.
    pub(crate) fn repaint(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        self.window.repaint(shell, compositor);
    }

    /// Paint what input changed since the last paint.
    pub(crate) fn paint_owed(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        self.window.paint_owed(shell, compositor);
    }

    /// Take the prompt down, the keyboard going back where it came from.
    pub(crate) fn close(self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        let next = self.window.hand_back();
        self.window.close(next, shell, compositor);
    }
}

/// A button index as an answer. Any index but the two buttons a prompt built
/// answers nothing, leaving the question up rather than guessing which way
/// the user meant it.
fn choice(index: Option<usize>) -> Option<Choice> {
    match index {
        Some(ACCEPT) => Some(Choice::Accepted),
        Some(DECLINE) => Some(Choice::Declined),
        _ => None,
    }
}

#[cfg(test)]
#[path = "choice_tests.rs"]
mod tests;
