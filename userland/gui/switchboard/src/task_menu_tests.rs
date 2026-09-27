//! Unit tests for a task's menu: its rows, their ids, and what a refused
//! command says.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::{
    AppMenu, AppMenuItemId, AppMenuItemView, AppMenuRole, AppMenuRowView, APP_MENU_LABEL_MAX,
};

use super::{task_control, task_menu, COMMANDS};
use crate::view::{TaskAuthority, TaskControl, TaskRefusal, TaskSummary};

/// A task named `name` that may take every command but the logs.
fn permitted(name: &str) -> TaskSummary {
    TaskSummary {
        name: String::from(name),
        authority: TaskAuthority {
            switch: Ok(()),
            pause: Ok(()),
            resume: Ok(()),
            lower_priority: Ok(()),
            force_quit: Ok(()),
        },
        ..TaskSummary::default()
    }
}

/// The menu's chooseable rows, in plate order.
fn items(menu: &AppMenu) -> Vec<AppMenuItemView<'_>> {
    menu.rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some(item),
            _ => None,
        })
        .collect()
}

/// The row that answers `control`.
fn row_for(menu: &AppMenu, control: TaskControl) -> AppMenuItemView<'_> {
    items(menu)
        .into_iter()
        .find(|item| task_control(item.id) == Some(control))
        .expect("every command has a row")
}

#[test]
fn every_command_has_one_row_whose_id_names_it_back() {
    let menu = task_menu(&permitted("editor")).expect("the rows fit the menu bounds");
    let chosen: Vec<Option<TaskControl>> = items(&menu)
        .iter()
        .map(|item| task_control(item.id))
        .collect();
    assert_eq!(
        chosen,
        COMMANDS.iter().copied().map(Some).collect::<Vec<_>>(),
        "one row per command, in the declared order, each naming its own"
    );
    assert!(
        items(&menu).iter().all(|item| !item.label.is_empty()),
        "every row reads as something"
    );
}

#[test]
fn an_id_the_menu_never_declared_names_no_command() {
    let past = AppMenuItemId::for_index(COMMANDS.len()).expect("a representable id");
    assert_eq!(task_control(past), None);
    let far = AppMenuItemId::new(u16::MAX).expect("a representable id");
    assert_eq!(task_control(far), None);
}

#[test]
fn the_rows_group_as_go_to_throttle_read_and_end() {
    let menu = task_menu(&permitted("editor")).expect("the rows fit the menu bounds");
    let mut groups: Vec<Vec<TaskControl>> = alloc::vec![Vec::new()];
    for (row, _) in menu.rows() {
        match row {
            AppMenuRowView::Separator => groups.push(Vec::new()),
            AppMenuRowView::Item(item) => {
                if let (Some(group), Some(control)) = (groups.last_mut(), task_control(item.id)) {
                    group.push(control);
                }
            }
            AppMenuRowView::Submenu { .. } | AppMenuRowView::Info => {
                panic!("a task's menu declares plain rows only")
            }
        }
    }
    assert_eq!(
        groups,
        alloc::vec![
            alloc::vec![TaskControl::Switch, TaskControl::Reveal],
            alloc::vec![
                TaskControl::Pause,
                TaskControl::Resume,
                TaskControl::LowerPriority
            ],
            alloc::vec![TaskControl::OpenLogs],
            alloc::vec![TaskControl::ForceQuit],
        ]
    );
}

#[test]
fn only_force_quit_wears_the_destructive_emphasis() {
    let menu = task_menu(&permitted("editor")).expect("the rows fit the menu bounds");
    for item in items(&menu) {
        let destructive = task_control(item.id) == Some(TaskControl::ForceQuit);
        assert_eq!(
            item.role == AppMenuRole::Destructive,
            destructive,
            "{}",
            item.label
        );
    }
}

#[test]
fn a_permitted_command_is_offered_and_states_nothing() {
    let menu = task_menu(&permitted("editor")).expect("the rows fit the menu bounds");
    for control in [
        TaskControl::Switch,
        TaskControl::Reveal,
        TaskControl::Pause,
        TaskControl::Resume,
        TaskControl::LowerPriority,
        TaskControl::ForceQuit,
    ] {
        let row = row_for(&menu, control);
        assert!(row.enabled, "{control:?}");
        assert!(row.reason.is_empty(), "{control:?}");
    }
}

#[test]
fn a_refused_command_keeps_its_row_disabled_with_its_reason() {
    // The wire cannot carry the Authority Mark, so the reason is what tells a
    // refusal of authority from one the task's own state makes.
    let mut task = permitted("editor");
    task.authority.pause = Err(TaskRefusal::Paused);
    task.authority.force_quit = Err(TaskRefusal::NotPermitted);
    let menu = task_menu(&task).expect("the rows fit the menu bounds");
    assert_eq!(items(&menu).len(), COMMANDS.len(), "no row is left out");
    for (control, refusal) in [
        (TaskControl::Pause, TaskRefusal::Paused),
        (TaskControl::ForceQuit, TaskRefusal::NotPermitted),
        (TaskControl::OpenLogs, TaskRefusal::NoLogReader),
    ] {
        let row = row_for(&menu, control);
        assert!(!row.enabled, "{control:?} must not be chooseable");
        assert_eq!(row.reason, refusal.reason(), "{control:?}");
    }
    assert!(row_for(&menu, TaskControl::Resume).enabled);
}

#[test]
fn a_task_that_permits_nothing_still_lists_every_command() {
    let menu = task_menu(&TaskSummary {
        name: String::from("locked"),
        ..TaskSummary::default()
    })
    .expect("the rows fit the menu bounds");
    let rows = items(&menu);
    assert_eq!(rows.len(), COMMANDS.len());
    assert!(rows
        .iter()
        .all(|row| !row.enabled && !row.reason.is_empty()));
}

#[test]
fn the_plate_is_titled_with_the_tasks_own_name() {
    let menu = task_menu(&permitted("terminal")).expect("the rows fit the menu bounds");
    assert_eq!(menu.title(), "terminal");
}

#[test]
fn a_name_no_label_may_carry_titles_the_plate_generically_and_keeps_the_menu() {
    // A process chooses its name's bytes, so one that is not admissible label
    // text must not make it unreachable from here.
    let long = "x".repeat(APP_MENU_LABEL_MAX + 1);
    for name in ["bell\u{7}", long.as_str()] {
        let menu = task_menu(&permitted(name)).expect("the rows fit the menu bounds");
        assert_eq!(menu.title(), super::UNNAMED_TITLE, "{name:?}");
        assert_eq!(items(&menu).len(), COMMANDS.len(), "{name:?}");
    }
}
