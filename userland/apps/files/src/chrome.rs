//! Which of the file manager's own chrome bands a window is showing, the keys
//! that turn them on, and the keys that run the toolbar's commands directly.
//!
//! A window opens showing the listing alone. The places rail and the command
//! toolbar are surfaces the user asks for, not fixed parts of the layout, so
//! neither reserves any of the window until it does: a plain window is the
//! directory and nothing else. The desktop settings application will set the
//! same two fields from the user's stored preference
//! (`plans/NEW-FILEMANAGER.md`); the two toggles here are how they are
//! reached from the window itself, and are what keeps every command the
//! toolbar carries — the view toggle, the sort cycle, the Trash tools —
//! reachable while it is hidden. [`Accelerator`] is the keyboard's direct
//! reach to the rest.
//!
//! Each decision is a pure function of the key, so it is host-tested; the
//! program only applies the answer.

use tairix_abi::input::{KeyValue, Modifiers, NamedKeyCode};
use tairix_browse::{ManagerTool, Places, ToolbarBand, ToolbarCommand, MANAGER_TOOLBAR_BAND};

/// The chrome bands one window is showing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Chrome {
    /// Whether the places / devices rail is drawn down the leading edge.
    pub rail: bool,
    /// Whether the command toolbar strip is drawn across the top.
    pub toolbar: ToolbarBand,
}

impl Chrome {
    /// What a window opens with: the listing alone.
    pub const HIDDEN: Self = Self {
        rail: false,
        toolbar: MANAGER_TOOLBAR_BAND,
    };

    /// The rail model the renderer and every rail hit-test take, or `None`
    /// when this window draws no rail — the one place the flag becomes the
    /// `Option` the shared engine speaks, so a window can never hit-test a
    /// rail it did not draw.
    #[must_use]
    pub const fn rail(self, places: &Places) -> Option<&Places> {
        if self.rail {
            Some(places)
        } else {
            None
        }
    }

    /// This chrome with the band `key` names flipped, or `None` when the key
    /// names neither.
    ///
    /// `F9` is the rail and `Ctrl+F9` the toolbar: one mnemonic for "show me
    /// the chrome", and `F9` alone is what a file manager's sidebar has
    /// answered to for years.
    #[must_use]
    pub fn toggled_by(self, key: KeyValue, modifiers: Modifiers) -> Option<Self> {
        if key != KeyValue::Named(NamedKeyCode::F9) {
            return None;
        }
        Some(if modifiers.ctrl {
            Self {
                toolbar: self.toolbar.toggled(),
                ..self
            }
        } else {
            Self {
                rail: !self.rail,
                ..self
            }
        })
    }
}

/// A command a key runs on the window, whichever field holds the keyboard.
///
/// These act on where the window is and how it shows it, never on the focused
/// field's own selection, so a focused rail lets them through rather than
/// swallowing the only way to reach a hidden toolbar's commands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Accelerator {
    /// One of the toolbar's read-only commands.
    Command(ToolbarCommand),
    /// One of the manager's write tools.
    Tool(ManagerTool),
}

impl Accelerator {
    /// The window command `key` runs, or `None` for a key that belongs to
    /// whichever field holds the keyboard.
    ///
    /// `Alt+←`/`Alt+→` walk the history, `Alt+↑` climbs, `F5` refreshes, and
    /// `Ctrl+Shift+N` is the New Folder tool.
    #[must_use]
    pub fn of(key: KeyValue, modifiers: Modifiers) -> Option<Self> {
        match key {
            KeyValue::Named(NamedKeyCode::Left) if modifiers.alt => {
                Some(Self::Command(ToolbarCommand::Back))
            }
            KeyValue::Named(NamedKeyCode::Right) if modifiers.alt => {
                Some(Self::Command(ToolbarCommand::Forward))
            }
            KeyValue::Named(NamedKeyCode::Up) if modifiers.alt => {
                Some(Self::Command(ToolbarCommand::Up))
            }
            KeyValue::Named(NamedKeyCode::F5) => Some(Self::Command(ToolbarCommand::Refresh)),
            // Shift may deliver the letter in either case.
            KeyValue::Char(ch)
                if modifiers.ctrl && modifiers.shift && ch.eq_ignore_ascii_case(&'n') =>
            {
                Some(Self::Tool(ManagerTool::NewFolder))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "chrome_tests.rs"]
mod tests;
