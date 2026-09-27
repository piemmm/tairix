//! The client's icon-bar declaration: what its slot on the desktop's bar
//! offers.
//!
//! The desktop draws the slot and owns the menu's pixels; the client declares
//! what is on it. One row is its own — *Settings…*, which opens the settings
//! window — and the shared convention ([`tairix_window::declaration`]) puts the
//! session's information row above it and *Quit* below.
//!
//! A window cannot raise itself, so while the settings window is open the row
//! is declared disabled with its reason rather than left to do nothing: the
//! window is reached through the slot's own window picker.

use tairix_abi::window_ipc::{
    AppBar, AppBarClick, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuReason, AppMenuRow,
};
use tairix_abi::Errno;
use tairix_window::QUIT_ROW;

/// The *Settings…* row's id, derived from the convention's own [`QUIT_ROW`]
/// so the two can never name the same row.
pub const ROW_SETTINGS: u16 = QUIT_ROW + 1;

/// What a primary click on the slot does: raise the game's window. The game
/// ends with its window, so a click never finds none to raise.
pub const SLOT_CLICK: AppBarClick = AppBarClick::Raise;

/// Which command a chosen row names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BarCommand {
    /// Open the settings window.
    Settings,
    /// Leave.
    Quit,
}

impl BarCommand {
    /// The command the row `item` names, or `None` for an id this client
    /// never declared.
    #[must_use]
    pub fn from_item(item: AppMenuItemId) -> Option<Self> {
        if tairix_window::is_quit(item) {
            return Some(Self::Quit);
        }
        (item.get() == ROW_SETTINGS).then_some(Self::Settings)
    }
}

/// The client's declaration, addressed to its own `endpoint`, with the
/// *Settings…* row disabled while `settings_open`.
///
/// # Errors
///
/// Any [`Errno`] the shared convention or the row bounds refuse. The rows are
/// fixed, so a refusal means the shared bounds changed under them; the caller
/// states it and carries on without a declared slot.
pub fn declaration(endpoint: u64, settings_open: bool) -> Result<AppBar, Errno> {
    let item = AppMenuItem::new(
        AppMenuItemId::new(ROW_SETTINGS)?,
        AppMenuLabel::new("Settings\u{2026}")?,
    );
    let item = if settings_open {
        item.disabled()
            .with_reason(AppMenuReason::new("The settings window is already open")?)
    } else {
        item
    };
    tairix_window::declaration(endpoint, SLOT_CLICK, &[AppMenuRow::Item(item)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use tairix_abi::window_ipc::AppMenuRowView;

    #[test]
    fn the_declaration_reads_information_settings_then_quit() {
        let bar = declaration(7, false).expect("the fixed rows fit");
        assert_eq!(bar.event_endpoint, 7);
        assert_eq!(bar.click, AppBarClick::Raise);
        let rows: Vec<AppMenuRowView<'_>> = bar.menu.rows().map(|(row, _)| row).collect();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0], AppMenuRowView::Info);
        assert!(matches!(
            rows[1],
            AppMenuRowView::Item(item) if item.id.get() == ROW_SETTINGS && item.enabled
        ));
        assert_eq!(rows[2], AppMenuRowView::Separator);
        assert!(matches!(rows[3], AppMenuRowView::Item(item) if item.id.get() == QUIT_ROW));
    }

    #[test]
    fn an_open_settings_window_disables_its_row_and_says_why() {
        let bar = declaration(7, true).expect("the fixed rows fit");
        let settings = bar
            .menu
            .rows()
            .find_map(|(row, _)| match row {
                AppMenuRowView::Item(item) if item.id.get() == ROW_SETTINGS => Some(item),
                _ => None,
            })
            .expect("the row is declared");
        assert!(!settings.enabled);
        assert!(!settings.reason.is_empty(), "a disabled row states why");
    }

    #[test]
    fn a_chosen_row_maps_to_its_command_and_an_unknown_id_to_nothing() {
        let id = |raw| AppMenuItemId::new(raw).expect("non-zero");
        assert_eq!(
            BarCommand::from_item(id(ROW_SETTINGS)),
            Some(BarCommand::Settings)
        );
        assert_eq!(BarCommand::from_item(id(QUIT_ROW)), Some(BarCommand::Quit));
        assert_eq!(BarCommand::from_item(id(99)), None);
    }
}
