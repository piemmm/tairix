use super::*;
use alloc::vec::Vec;
use tairix_abi::window_ipc::AppMenuRowView;
use tairix_machines::menu::{card_command, card_menu, CardCommand};
use tairix_machines::Standing;

fn shown() -> Shown {
    Shown {
        paused: false,
        speed: CpuClock::PC,
        tube: Tube::default(),
        clipboard: Clipboard {
            copyable: true,
            pasting: false,
        },
        can_share: true,
        windows: Windows::default(),
    }
}

/// Every chooseable row: its command, its label and its mark.
fn rows(shown: &Shown) -> Vec<(Command, &'static str, AppMenuMark)> {
    let menu = menu(shown).unwrap();
    menu.rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some((
                command(item.id).expect("every row is a command"),
                // The labels are this module's own constants.
                Effect::ALL
                    .iter()
                    .map(|effect| effect.label())
                    .chain(SPEEDS.iter().map(|(_, label)| *label))
                    .chain([
                        "Pause",
                        "Reset",
                        "Send Ctrl+Alt+Delete",
                        "Take Screenshot\u{2026}",
                        "Full Screen",
                        "Shut Down",
                        "Copy Screen Text",
                        "Paste",
                        "Stop Pasting",
                        "Share a Folder\u{2026}",
                        "Debugger",
                    ])
                    .find(|label| *label == item.label)
                    .expect("a label of the menu's"),
                item.mark,
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn every_row_is_its_own_command_and_the_ids_never_meet() {
    let commands: Vec<Command> = rows(&shown()).into_iter().map(|(c, ..)| c).collect();
    assert_eq!(commands.len(), 10 + SPEEDS.len() + Effect::ALL.len());
    assert!(commands.contains(&Command::ShutDown));
    for (n, command) in commands.iter().enumerate() {
        assert!(!commands[n + 1..].contains(command), "{command:?} twice");
    }
    assert!(commands.contains(&Command::Speed(CpuClock::TURBO_XT)));
    assert!(commands.contains(&Command::Effect(Effect::Wobble)));
    assert_eq!(command(AppMenuItemId::new(99).unwrap()), None);
    assert_eq!(
        command(AppMenuItemId::new(15).unwrap()),
        None,
        "past the speeds"
    );
}

#[test]
fn the_marks_say_how_the_machine_stands() {
    let standing = Shown {
        paused: true,
        speed: CpuClock::TURBO_XT,
        tube: Effect::Glow.toggled(Tube::default()),
        clipboard: Clipboard {
            copyable: true,
            pasting: true,
        },
        can_share: false,
        windows: Windows {
            fullscreen: true,
            debugging: true,
        },
    };
    for (command, label, mark) in rows(&standing) {
        let expected = match command {
            Command::Pause
            | Command::FullScreen
            | Command::Debugger
            | Command::Effect(Effect::Glow) => AppMenuMark::Check,
            Command::Speed(rate) if rate == CpuClock::TURBO_XT => AppMenuMark::Radio,
            _ => AppMenuMark::None,
        };
        assert_eq!(mark, expected, "{label}");
    }
    for (_, label, mark) in rows(&shown()) {
        let radio = label == "4.77 MHz";
        assert_eq!(mark == AppMenuMark::Radio, radio, "{label}");
        assert!(radio || mark == AppMenuMark::None, "{label}");
    }
}

/// Whether the row of `command` is in the menu, and chooseable.
fn offered(shown: &Shown, wanted: Command) -> Option<bool> {
    menu(shown).unwrap().rows().find_map(|(row, _)| match row {
        AppMenuRowView::Item(item) if command(item.id) == Some(wanted) => Some(item.enabled),
        _ => None,
    })
}

#[test]
fn copying_needs_text_on_the_screen_and_paste_becomes_stop_while_typing() {
    assert_eq!(offered(&shown(), Command::CopyText), Some(true));
    let graphics = Shown {
        clipboard: Clipboard {
            copyable: false,
            pasting: false,
        },
        ..shown()
    };
    assert_eq!(offered(&graphics, Command::CopyText), Some(false));
    assert_eq!(offered(&shown(), Command::Paste), Some(true));
    assert_eq!(offered(&shown(), Command::StopPasting), None);
    let typing = Shown {
        clipboard: Clipboard {
            copyable: true,
            pasting: true,
        },
        ..shown()
    };
    assert_eq!(offered(&typing, Command::Paste), None);
    assert_eq!(offered(&typing, Command::StopPasting), Some(true));
}

#[test]
fn a_folder_is_offered_to_share_while_the_machine_has_room_for_one() {
    assert_eq!(offered(&shown(), Command::ShareFolder), Some(true));
    let full = Shown {
        can_share: false,
        ..shown()
    };
    assert_eq!(offered(&full, Command::ShareFolder), Some(false));
}

#[test]
fn a_row_acting_in_place_keeps_the_menu_up_and_one_acting_elsewhere_does_not() {
    let menu = menu(&shown()).unwrap();
    for (row, _) in menu.rows() {
        let AppMenuRowView::Item(item) = row else {
            continue;
        };
        let elsewhere = matches!(
            command(item.id),
            Some(
                Command::ShareFolder | Command::Screenshot | Command::Debugger | Command::ShutDown
            )
        );
        assert_eq!(item.repeatable, !elsewhere, "{}", item.label);
    }
}

#[test]
fn a_running_machines_card_carries_the_windows_own_rows_in_order() {
    let push = |menu: &mut AppMenu| push_machine(menu, &shown());
    let card = card_menu(Standing::Running, Some(push)).unwrap();
    let carried: Vec<Command> = card
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => match card_command(item.id) {
                CardCommand::Machine(id) => command(id),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let own: Vec<Command> = rows(&shown())
        .into_iter()
        .map(|(command, ..)| command)
        .collect();
    assert_eq!(carried, own);
}
