//! The network signal's menu: the way to the network's settings and to what
//! the network is doing.
//!
//! [`ROWS`] is the single definition of the menu's shape; the rendered rows
//! and the row → command mapping are both derived from it. Neither command
//! needs authority the bar lacks: each opens a surface that states and checks
//! its own.

use alloc::vec::Vec;

use tairix_abi::switchboard_ipc::CommandSection;
use tairix_controls::{ControlState, MenuItem};

use crate::input::TaskbarResponse;
use crate::menu::BarMenuRow;
use crate::system::REASON_NOT_INSTALLED;

/// One row of the network menu.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NetworkRow {
    /// Open Settings where the interfaces are configured.
    Settings,
    /// Open the Switchboard on what the machine's resources, the network
    /// among them, are doing.
    Activity,
}

impl NetworkRow {
    /// What choosing this row asks the embedder for.
    #[must_use]
    pub(crate) const fn response(self) -> TaskbarResponse {
        match self {
            Self::Settings => TaskbarResponse::OpenNetworkSettings,
            Self::Activity => TaskbarResponse::OpenSwitchboard {
                section: CommandSection::Resources,
            },
        }
    }

    /// The row's label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Settings => "Network Settings…",
            Self::Activity => "Network Activity…",
        }
    }
}

/// The network menu, in order: the single definition of its shape.
pub const ROWS: &[NetworkRow] = &[NetworkRow::Settings, NetworkRow::Activity];

/// The menu's rows, each carrying its position in [`ROWS`] as its id. The
/// settings row is shown disabled, with its reason, when the desktop found no
/// settings bundle to open.
#[must_use]
pub(crate) fn rows(settings_installed: bool) -> Vec<BarMenuRow> {
    ROWS.iter()
        .enumerate()
        .map(|(index, row)| {
            let item = MenuItem::new(row.label());
            match row {
                NetworkRow::Settings if !settings_installed => (
                    index,
                    item.with_state(ControlState::disabled()),
                    Some(REASON_NOT_INSTALLED),
                ),
                _ => (index, item, None),
            }
        })
        .collect()
}

/// What the row at `index` asks the embedder for, or `None` past the end
/// (fail closed, never a guessed command).
#[must_use]
pub(crate) fn response_at(index: usize) -> Option<TaskbarResponse> {
    ROWS.get(index).copied().map(NetworkRow::response)
}

#[cfg(test)]
mod tests {
    use super::{response_at, rows, NetworkRow, ROWS};
    use crate::input::TaskbarResponse;
    use crate::system::REASON_NOT_INSTALLED;
    use tairix_abi::switchboard_ipc::CommandSection;

    #[test]
    fn each_row_draws_its_label_and_answers_its_own_command() {
        let drawn = rows(true);
        assert_eq!(drawn.len(), ROWS.len());
        for ((index, item, why), row) in drawn.into_iter().zip(ROWS) {
            assert_eq!(item.label(), row.label());
            assert!(why.is_none());
            assert!(item.state().is_actionable());
            assert_eq!(response_at(index), Some(row.response()));
        }
        assert_eq!(response_at(0), Some(TaskbarResponse::OpenNetworkSettings));
        assert_eq!(
            NetworkRow::Activity.response(),
            TaskbarResponse::OpenSwitchboard {
                section: CommandSection::Resources
            }
        );
    }

    #[test]
    fn an_index_past_the_table_answers_nothing() {
        assert_eq!(response_at(ROWS.len()), None);
    }

    #[test]
    fn with_no_settings_bundle_the_settings_row_says_so_and_cannot_act() {
        for (index, item, why) in rows(false) {
            let settings = ROWS[index] == NetworkRow::Settings;
            assert_eq!(item.state().is_actionable(), !settings);
            assert_eq!(why, settings.then_some(REASON_NOT_INSTALLED));
        }
    }
}
