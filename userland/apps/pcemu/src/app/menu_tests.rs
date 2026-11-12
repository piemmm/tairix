use super::*;
use alloc::vec::Vec;
use tairix_abi::window_ipc::AppMenuRowView;

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

#[test]
fn an_effect_turns_on_at_its_own_strength_and_off_again() {
    let tube = Effect::ScanLines.toggled(Tube::default());
    assert!(Effect::ScanLines.is_on(tube));
    assert!(!Effect::Glow.is_on(tube), "only the one");
    assert!(tube.scanlines > 0 && tube.scanlines <= tairix_crt::FULL);
    assert_eq!(Effect::ScanLines.toggled(tube), Tube::default());
    let full = Tube {
        fuzz: tairix_crt::FULL,
        ..Tube::default()
    };
    assert_eq!(
        Effect::Noise.toggled(full).fuzz,
        0,
        "any strength turns off"
    );
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

/// Every chooseable row of a card's menu: its command, label and whether it
/// may be chosen.
fn card_rows(standing: Standing, running: Option<&Shown>) -> Vec<(CardCommand, bool)> {
    card_menu(standing, running)
        .unwrap()
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some((
                card_command(item.id).expect("every row is a command"),
                item.enabled,
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn a_cards_menu_starts_configures_copies_and_deletes_a_machine_at_rest() {
    let at_rest = [
        (CardCommand::Start, true),
        (CardCommand::Settings, true),
        (CardCommand::Duplicate, true),
        (CardCommand::Delete, true),
    ];
    assert_eq!(card_rows(Standing::Off, None), at_rest);
    assert_eq!(card_rows(Standing::Suspended, None), at_rest);
    let label = |standing| {
        let menu = card_menu(standing, None).unwrap();
        let first = menu.rows().find_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some(alloc::string::String::from(item.label)),
            _ => None,
        });
        first.expect("a first row")
    };
    assert_eq!(label(Standing::Off), "Start");
    assert_eq!(label(Standing::Suspended), "Resume");
    assert_eq!(
        card_rows(Standing::Saving, None),
        [
            (CardCommand::Start, false),
            (CardCommand::Settings, true),
            (CardCommand::Duplicate, false),
            (CardCommand::Delete, false),
        ],
        "one being kept is left to be kept"
    );
}

#[test]
fn a_running_machines_card_carries_its_own_rows_and_is_neither_copied_nor_deleted() {
    let rows = card_rows(Standing::Running, Some(&shown()));
    assert_eq!(rows.first(), Some(&(CardCommand::Start, true)));
    let machine: Vec<Command> = rows
        .iter()
        .filter_map(|(row, _)| match row {
            CardCommand::Machine(command) => Some(*command),
            _ => None,
        })
        .collect();
    let own: Vec<Command> = rows_of(&shown());
    assert_eq!(machine, own, "the machine window's own rows, in its order");
    assert!(rows.contains(&(CardCommand::Duplicate, false)));
    assert!(rows.contains(&(CardCommand::Delete, false)));
    assert_eq!(
        card_command(AppMenuItemId::new(PAUSE).unwrap()),
        Some(CardCommand::Machine(Command::Pause))
    );
    assert_eq!(card_command(AppMenuItemId::new(99).unwrap()), None);
}

/// The commands of a machine window's own menu, in order.
fn rows_of(shown: &Shown) -> Vec<Command> {
    rows(shown)
        .into_iter()
        .map(|(command, ..)| command)
        .collect()
}

/// A menu up follows what it is for: a paste ending, or its machine filling
/// the display, is restated once, and the rows it already has never are.
#[test]
fn a_menu_up_is_restated_once_for_each_change_to_what_it_shows() {
    let pasting = Shown {
        clipboard: Clipboard {
            copyable: true,
            pasting: true,
        },
        ..shown()
    };
    let mut up = Followed::new(7, (), menu(&pasting).unwrap());
    assert_eq!((up.open(), up.kind()), (7, ()));
    assert!(
        up.follow(menu(&pasting).unwrap()).is_none(),
        "nothing moved"
    );
    let pasted = menu(&shown()).unwrap();
    assert_eq!(up.follow(pasted), Some(&pasted), "the paste ended");
    assert!(up.follow(pasted).is_none(), "and is restated once");

    let windowed = card_menu(Standing::Running, Some(&shown())).unwrap();
    let mut card = Followed::new(8, 3u32, windowed);
    let filling = Shown {
        windows: Windows {
            fullscreen: true,
            ..Windows::default()
        },
        ..shown()
    };
    let full = card_menu(Standing::Running, Some(&filling)).unwrap();
    assert_eq!(
        card.follow(full),
        Some(&full),
        "its machine filled the display"
    );
    assert_eq!(card.kind(), 3);
}
