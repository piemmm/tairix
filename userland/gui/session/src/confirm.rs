//! The desktop session's **trusted confirmation prompt** for a system
//! command that cannot be undone.
//!
//! Restarting or powering the machine off ends every task of every principal
//! on it, so neither may happen on a single click. The session — not the
//! taskbar, which holds no authority at all — puts the choice to the user in
//! an alert of its own, and only a deliberate confirmation produces the
//! outcome the caller relays onward.
//!
//! The prompt is modelled on the session's trusted file picker: one slot (a
//! second request while one is showing is refused rather than stacking a
//! second prompt), a session-owned compositor window, and a typed conclusion
//! the embedder acts on once the window is already closed.
//!
//! Every path that is not an explicit confirmation — the safe button,
//! `Escape`, or a request the session had to abandon — concludes as a
//! cancellation, so nothing irreversible can follow from a prompt the user
//! did not answer.

use tairix_abi::input::KeyInput;
use tairix_abi::window_ipc::{AlertButtonRole, AlertSpec};
use tairix_abi::PowerAction;
use tairix_wm::{Compositor, Point, WindowId};

use crate::choice::{question, Choice, ChoicePrompt};
use crate::shell::DesktopShell;

/// How the user answered a confirmation prompt.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Answer {
    /// The user confirmed: the caller may now relay the transition.
    Confirmed(PowerAction),
    /// The user declined, dismissed the prompt, or it was abandoned.
    /// Nothing is relayed.
    Declined,
}

/// One showing prompt: what it is asking about, and the question itself.
struct ActivePrompt {
    action: PowerAction,
    prompt: ChoicePrompt,
}

/// The session's confirmation-prompt slot.
///
/// Idle until a destructive command is chosen, then holding exactly one
/// prompt until the user answers it.
#[derive(Default)]
pub struct ConfirmPrompt {
    active: Option<ActivePrompt>,
}

impl ConfirmPrompt {
    /// An idle prompt.
    #[must_use]
    pub const fn new() -> Self {
        Self { active: None }
    }

    /// The centre of the showing prompt's button at `index`, in its client.
    #[cfg(test)]
    pub(crate) fn button_centre(
        &self,
        index: usize,
        shell: &DesktopShell,
        compositor: &Compositor,
    ) -> Option<Point> {
        Some(
            self.active
                .as_ref()?
                .prompt
                .button_centre(index, shell, compositor),
        )
    }

    /// The compositor window of the showing prompt, if one is up.
    ///
    /// The embedder routes this window's key and click input into
    /// [`handle_key`](Self::handle_key) / [`handle_click`](Self::handle_click)
    /// rather than to any served window.
    #[must_use]
    pub fn wm_id(&self) -> Option<WindowId> {
        self.active.as_ref().map(|active| active.prompt.wm())
    }

    /// The transition the showing prompt is asking about, if one is up.
    #[must_use]
    pub fn pending(&self) -> Option<PowerAction> {
        self.active.as_ref().map(|active| active.action)
    }

    /// Ask the user to confirm `action`.
    ///
    /// Returns whether the prompt came up. A prompt already showing, or a
    /// window the compositor could not give, answers `false`: the caller
    /// relays nothing, so a prompt that cannot be shown can never be taken
    /// for an answer.
    pub fn ask(
        &mut self,
        action: PowerAction,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> bool {
        if self.active.is_some() {
            return false;
        }
        let Some(mut prompt) =
            build_spec(action).and_then(|spec| ChoicePrompt::open(&spec, None, shell, compositor))
        else {
            return false;
        };
        prompt.stamp(now_ns);
        self.active = Some(ActivePrompt { action, prompt });
        true
    }

    /// Apply one key press at `now_ns` to the showing prompt.
    ///
    /// `Escape` declines outright. `Enter` and `Space` press whichever button
    /// holds the focus — the safe one when the prompt opens, since the
    /// confirming one destroys — so confirming always takes a deliberate move
    /// to the other button.
    ///
    /// Returns the answer once the user has given one; the prompt window is
    /// already closed by then.
    pub fn handle_key(
        &mut self,
        key: &KeyInput,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Answer> {
        let choice = self
            .active
            .as_mut()?
            .prompt
            .key(key, now_ns, shell, compositor)?;
        self.conclude(shell, compositor, choice)
    }

    /// Apply one primary-button press at the prompt-window-local position
    /// `local`.
    ///
    /// A press-and-release on a button answers with it; a press anywhere
    /// else inside the prompt changes nothing and leaves the question up, so
    /// the transition follows only from pressing the confirming button.
    pub fn handle_click(
        &mut self,
        local: Point,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Answer> {
        let choice = self
            .active
            .as_mut()?
            .prompt
            .click(local, now_ns, shell, compositor)?;
        self.conclude(shell, compositor, choice)
    }

    /// Take the prompt down without an answer, declining the transition.
    ///
    /// Used when the session is tearing the desktop down or the theme it was
    /// drawn against is gone: an unanswered prompt is never a confirmation.
    pub fn abandon(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        let _ = self.conclude(shell, compositor, Choice::Declined);
    }

    /// Repaint the showing prompt, so a theme switch behind it redraws it in
    /// the appearance now in use. A surface that cannot be built leaves the
    /// previous frame up rather than failing.
    pub fn repaint(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        if let Some(active) = self.active.as_mut() {
            active.prompt.repaint(shell, compositor);
        }
    }

    /// Paint what input changed in the prompt since the last paint.
    pub fn paint_owed(&mut self, shell: &DesktopShell, compositor: &mut Compositor) {
        if let Some(active) = self.active.as_mut() {
            active.prompt.paint_owed(shell, compositor);
        }
    }

    fn conclude(
        &mut self,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        choice: Choice,
    ) -> Option<Answer> {
        let active = self.active.take()?;
        active.prompt.close(shell, compositor);
        Some(match choice {
            Choice::Accepted => Answer::Confirmed(active.action),
            Choice::Declined => Answer::Declined,
        })
    }
}

/// The question the prompt asks about `action`.
const fn headline(action: PowerAction) -> &'static str {
    match action {
        PowerAction::PowerOff => "Shut down the computer?",
        PowerAction::Restart => "Restart the computer?",
    }
}

/// What the prompt says will happen, in plain words, so the user is agreeing
/// to a consequence rather than to a verb.
const fn message(action: PowerAction) -> &'static str {
    match action {
        PowerAction::PowerOff => {
            "Every open application will be closed and the machine will switch off."
        }
        PowerAction::Restart => {
            "Every open application will be closed and the machine will start again."
        }
    }
}

/// The label on the confirming button: the same words as the row that asked,
/// so the user confirms the thing they chose.
const fn confirm_label(action: PowerAction) -> &'static str {
    match action {
        PowerAction::PowerOff => "Shut Down",
        PowerAction::Restart => "Restart",
    }
}

/// The prompt's alert: the safe button leading and holding the keyboard, the
/// destructive one trailing. `None` only for compiled-in text no alert can
/// carry, which the tests rule out.
pub(crate) fn build_spec(action: PowerAction) -> Option<AlertSpec> {
    question(
        headline(action),
        message(action),
        "Cancel",
        (confirm_label(action), AlertButtonRole::Destructive),
    )
}
