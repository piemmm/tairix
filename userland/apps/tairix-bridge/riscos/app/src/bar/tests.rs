use alloc::vec::Vec;

use tairix_abi::window_ipc::AppMenuRowView;
use tairix_geometry::Point;
use tairix_input::PointerButton;
use tairix_theme::ThemeRegistry;

use super::*;

const SHOWN: Shown = Shown {
    fullscreen: false,
    scaling: Scaling::Sharp,
    paused: true,
};

/// Each row's id and mark, in order.
fn rows(menu: &AppMenu) -> Vec<(AppMenuItemId, AppMenuMark)> {
    menu.rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some((item.id, item.mark)),
            _ => None,
        })
        .collect()
}

#[test]
fn every_row_is_the_command_it_says() {
    let menu = menu(&SHOWN).unwrap();
    let commands: Vec<Option<Command>> = rows(&menu).iter().map(|&(id, _)| command(id)).collect();
    assert_eq!(
        commands,
        [
            Some(Command::FullScreen),
            Some(Command::Scaling(Scaling::Sharp)),
            Some(Command::Scaling(Scaling::Smooth)),
            Some(Command::Pause),
            Some(Command::Reset),
        ]
    );
    assert_eq!(command(AppMenuItemId::new(99).unwrap()), None);
}

#[test]
fn the_rows_are_marked_as_the_screen_stands() {
    let marks = |shown: &Shown| -> Vec<AppMenuMark> {
        rows(&menu(shown).unwrap())
            .into_iter()
            .map(|(_, mark)| mark)
            .collect()
    };
    assert_eq!(
        marks(&SHOWN),
        [
            AppMenuMark::None,
            AppMenuMark::Radio,
            AppMenuMark::None,
            AppMenuMark::Check,
            AppMenuMark::None,
        ]
    );
    let other = Shown {
        fullscreen: true,
        scaling: Scaling::Smooth,
        paused: false,
    };
    assert_eq!(
        marks(&other),
        [
            AppMenuMark::Check,
            AppMenuMark::None,
            AppMenuMark::Radio,
            AppMenuMark::None,
            AppMenuMark::None,
        ]
    );
}

#[test]
fn the_bar_s_button_asks_for_the_menu_beneath_it() {
    let themes = ThemeRegistry::with_builtins();
    let looks = (Scale::ONE, themes.active());
    let mut bar = Bar::default();
    let width = 640;
    let bounds = bar.button_rect(width, Scale::ONE, themes.active());
    let centre = Point::new(
        bounds.origin.x + i32::try_from(bounds.width / 2).unwrap(),
        bounds.origin.y + i32::try_from(bounds.height / 2).unwrap(),
    );
    let mut damage = Region::new();
    let mut asked = None;
    for event in [
        InputEvent::PointerMoved { to: centre },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        asked = asked.or(bar.on_input(&event, width, looks, &mut damage));
    }
    assert_eq!(asked, Some(bounds));
    assert!(bounds.origin.x > 0, "at the trailing end");
    assert!(Bar::covers((0, 0), Scale::ONE, themes.active()));
    assert!(!Bar::covers(
        (0, Bar::height(Scale::ONE, themes.active())),
        Scale::ONE,
        themes.active()
    ));
}
