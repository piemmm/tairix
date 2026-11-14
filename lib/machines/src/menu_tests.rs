use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use tairix_abi::window_ipc::AppMenuRowView;

const PAUSE: u16 = 1;
const RESET: u16 = 2;

/// A toy machine's own rows: Pause, and Reset while `reset` says so.
fn machine_rows(reset: bool) -> impl FnOnce(&mut AppMenu) -> Result<(), Errno> {
    move |menu: &mut AppMenu| {
        menu.push(AppMenuRow::Item(AppMenuItem::new(
            AppMenuItemId::new(PAUSE)?,
            AppMenuLabel::new("Pause")?,
            AppMenuSymbol::Pause,
        )))?;
        if reset {
            menu.push(AppMenuRow::Item(AppMenuItem::new(
                AppMenuItemId::new(RESET)?,
                AppMenuLabel::new("Reset")?,
                AppMenuSymbol::Restart,
            )))?;
        }
        Ok(())
    }
}

/// The rows of a machine that is not running: none.
type MachineRows = fn(&mut AppMenu) -> Result<(), Errno>;

const NOT_RUNNING: Option<MachineRows> = None;

fn card_rows(menu: &AppMenu) -> Vec<(CardCommand, bool)> {
    menu.rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some((card_command(item.id), item.enabled)),
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
    for standing in [Standing::Off, Standing::Suspended] {
        assert_eq!(
            card_rows(&card_menu(standing, NOT_RUNNING).unwrap()),
            at_rest
        );
    }
    let label = |standing| {
        let menu = card_menu(standing, NOT_RUNNING).unwrap();
        let first = menu.rows().find_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some(String::from(item.label)),
            _ => None,
        });
        first.expect("a first row")
    };
    assert_eq!(label(Standing::Off), "Start");
    assert_eq!(label(Standing::Suspended), "Resume");
    assert_eq!(
        card_rows(&card_menu(Standing::Saving, NOT_RUNNING).unwrap()),
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
    let menu = card_menu(Standing::Running, Some(machine_rows(true))).unwrap();
    let rows = card_rows(&menu);
    assert_eq!(rows.first(), Some(&(CardCommand::Start, true)));
    let machine: Vec<u16> = rows
        .iter()
        .filter_map(|(row, _)| match row {
            CardCommand::Machine(id) => Some(id.get()),
            _ => None,
        })
        .collect();
    assert_eq!(machine, [PAUSE, RESET], "the machine's own rows, in order");
    assert!(rows.contains(&(CardCommand::Duplicate, false)));
    assert!(rows.contains(&(CardCommand::Delete, false)));
    let shows = menu.rows().find_map(|(row, _)| match row {
        AppMenuRowView::Item(item) => Some(String::from(item.label)),
        _ => None,
    });
    assert_eq!(shows.as_deref(), Some("Show Window"));
}

#[test]
fn a_menu_up_is_restated_once_for_each_change_to_what_it_shows() {
    let with_reset = card_menu(Standing::Running, Some(machine_rows(true))).unwrap();
    let mut card = Followed::new(8, 3u32, with_reset);
    assert_eq!((card.open(), card.kind()), (8, 3));
    assert!(card.follow(with_reset).is_none(), "nothing moved");
    let without = card_menu(Standing::Running, Some(machine_rows(false))).unwrap();
    assert_eq!(card.follow(without), Some(&without), "a row went");
    assert!(card.follow(without).is_none(), "and is restated once");
}

#[test]
fn the_cards_rows_lie_above_every_row_an_application_numbers() {
    for raw in [CARD_ROWS, CARD_ROWS + 3] {
        assert!(!matches!(
            card_command(AppMenuItemId::new(raw).unwrap()),
            CardCommand::Machine(_)
        ));
    }
    assert_eq!(
        card_command(AppMenuItemId::new(CARD_ROWS - 1).unwrap()),
        CardCommand::Machine(AppMenuItemId::new(CARD_ROWS - 1).unwrap())
    );
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

#[test]
fn the_display_submenu_ticks_what_the_tube_draws_and_names_each_effect_by_its_row() {
    const FIRST: u16 = 20;
    let tube = Effect::Glow.toggled(Effect::Wobble.toggled(Tube::default()));
    let mut menu = AppMenu::EMPTY;
    push_display(&mut menu, tube, FIRST).unwrap();
    let rows: Vec<(Option<Effect>, AppMenuMark, bool)> = menu
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => {
                Some((effect_of(item.id.get(), FIRST), item.mark, item.repeatable))
            }
            _ => None,
        })
        .collect();
    let expected: Vec<(Option<Effect>, AppMenuMark, bool)> = Effect::ALL
        .into_iter()
        .map(|effect| {
            let on = matches!(effect, Effect::Glow | Effect::Wobble);
            (Some(effect), marked(on, AppMenuMark::Check), true)
        })
        .collect();
    assert_eq!(
        rows, expected,
        "every effect, in order, kept up when chosen"
    );
    assert_eq!(effect_of(FIRST - 1, FIRST), None);
    assert_eq!(effect_of(FIRST + 5, FIRST), None, "past the effects");
}

#[test]
fn a_row_acting_elsewhere_closes_the_menu_and_one_in_place_keeps_it() {
    let kept = |acts| {
        let mut menu = AppMenu::EMPTY;
        let row = item(9, "Row", AppMenuSymbol::Play, (AppMenuMark::None, acts)).unwrap();
        menu.push(row).unwrap();
        let repeatable = menu.rows().find_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some(item.repeatable),
            _ => None,
        });
        repeatable
    };
    assert_eq!(kept(Acts::InPlace), Some(true));
    assert_eq!(kept(Acts::Elsewhere), Some(false));
}

#[test]
fn the_icon_bar_offers_the_library_alone() {
    let mut menu = AppMenu::EMPTY;
    for row in bar_rows().expect("the fixed rows fit") {
        menu.push(row).expect("fits");
    }
    let rows: Vec<(u16, String)> = menu
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some((item.id.get(), String::from(item.label))),
            _ => None,
        })
        .collect();
    assert_eq!(rows, [(LIBRARY_ROW, String::from("Machine Library"))]);
    assert_eq!(
        BAR_CLICK,
        AppBarClick::RaiseOrOpen,
        "the slot opens the library"
    );
}
