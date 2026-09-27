//! One task's menu: the rows a secondary press on its Tasks row offers, and
//! the command a chosen row names (`plans/NEW-MENUS.md`).
//!
//! The service describes and the desktop decides: the plate, its placement,
//! the grab and the dismissal are the session's. A row's id is its command's
//! position in the menu's own command list, so the menu's shape never moves
//! with the task's state — a command the task cannot take now is declared
//! disabled with its reason rather than left out.

use tairix_abi::window_ipc::{
    AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuReason, AppMenuRole, AppMenuRow,
};
use tairix_abi::Errno;

use crate::view::{TaskControl, TaskSummary};

/// The commands the menu lists, in the order it lists them: go to the task,
/// throttle it, read about it, and only last end it.
const COMMANDS: [TaskControl; 7] = [
    TaskControl::Switch,
    TaskControl::Reveal,
    TaskControl::Pause,
    TaskControl::Resume,
    TaskControl::LowerPriority,
    TaskControl::OpenLogs,
    TaskControl::ForceQuit,
];

/// The plate's title where a task's own name is not admissible label text.
const UNNAMED_TITLE: &str = "Task";

/// The row label `control` reads as.
const fn label(control: TaskControl) -> &'static str {
    match control {
        TaskControl::Switch => "Switch to",
        TaskControl::Reveal => "Reveal window",
        TaskControl::Pause => "Pause",
        TaskControl::Resume => "Resume",
        TaskControl::LowerPriority => "Lower priority",
        TaskControl::OpenLogs => "Open logs",
        TaskControl::ForceQuit => "Force quit",
    }
}

/// Whether `control` begins a new group, drawing a divider above it.
const fn opens_group(control: TaskControl) -> bool {
    matches!(
        control,
        TaskControl::Pause | TaskControl::OpenLogs | TaskControl::ForceQuit
    )
}

/// The menu a secondary press on `task`'s row asks the desktop to open,
/// titled with the task's name.
///
/// A name that is not admissible label text — the kernel attests a name's
/// origin, not its bytes — titles the plate generically rather than refusing
/// the menu, so no process can make itself unreachable here by its choice of
/// name.
///
/// # Errors
///
/// Any [`Errno`] the shared menu bounds refuse. The rows are fixed, so a
/// refusal can only mean those bounds changed under this menu; the caller
/// states it and opens nothing.
pub fn task_menu(task: &TaskSummary) -> Result<AppMenu, Errno> {
    let title = AppMenuLabel::new(&task.name).or_else(|_| AppMenuLabel::new(UNNAMED_TITLE))?;
    let mut menu = AppMenu::titled(title);
    for (index, control) in COMMANDS.into_iter().enumerate() {
        if opens_group(control) {
            menu.push(AppMenuRow::Separator)?;
        }
        let id = AppMenuItemId::for_index(index).ok_or(Errno::OutOfRange)?;
        let mut item = AppMenuItem::new(id, AppMenuLabel::new(label(control))?);
        if control == TaskControl::ForceQuit {
            item = item.with_role(AppMenuRole::Destructive);
        }
        if let Err(refusal) = task.authority.check(control) {
            item = item
                .disabled()
                .with_reason(AppMenuReason::new(refusal.reason())?);
        }
        menu.push(AppMenuRow::Item(item))?;
    }
    Ok(menu)
}

/// The command the chosen row `item` names, or `None` for an id this menu
/// never declared (fail closed — an outcome is never guessed at).
#[must_use]
pub fn task_control(item: AppMenuItemId) -> Option<TaskControl> {
    COMMANDS.get(item.index()).copied()
}

#[cfg(test)]
#[path = "task_menu_tests.rs"]
mod tests;
