use tairix_browse::{Folders, SortDirection, SortKey, ViewMode};
use tairix_controls::{damage, FieldAction, FieldControl, FieldGroupAction, Keystroke};
use tairix_geometry::{Rect, Scale};
use tairix_input::{Key, Modifiers, NamedKey};
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::{Setting, SettingsRequest, SettingsWindow, PAGE};
use crate::preferences::{Preferences, Start};

fn press(key: Key, shift: bool) -> Keystroke {
    Keystroke {
        key,
        modifiers: Modifiers {
            shift,
            ..Modifiers::default()
        },
        at_ns: 0,
    }
}

const TAB: Key = Key::Named(NamedKey::Tab);

/// A window tall enough to show the whole page at `scale`.
fn whole(page: &SettingsWindow, scale: Scale, theme: &Theme) -> Rect {
    let (width, height) = page.extent(scale, theme);
    Rect::new(0, 0, width, height)
}

/// What every control on the page shows, row by row.
fn controls(page: &SettingsWindow) -> alloc::vec::Vec<(Option<usize>, Option<bool>)> {
    page.groups
        .iter()
        .flat_map(|group| group.rows().iter())
        .map(|row| match row.control() {
            FieldControl::Combo(combo) => (combo.selected(), None),
            FieldControl::Toggle(toggle) => (None, Some(toggle.is_on())),
            _ => (None, None),
        })
        .collect()
}

#[test]
fn the_page_shows_every_setting_it_opened_on() {
    let mut settings = Preferences {
        start: Start::Desktop,
        view: ViewMode::List,
        confirm_trash: false,
        ..Preferences::default()
    };
    settings.sort.direction = SortDirection::Descending;
    let page = SettingsWindow::new(&settings);
    assert_eq!(page.shown(), &settings);
    let shown = controls(&page);
    assert_eq!(shown[0], (Some(1), None), "open at the desktop");
    assert_eq!(shown[1], (Some(2), None), "as a list");
    assert_eq!(shown[3], (Some(1), None), "descending");
    assert_eq!(shown[7], (None, Some(true)), "the .app ending hidden");
    assert_eq!(
        shown[12],
        (None, Some(false)),
        "moving to the Trash asks nothing"
    );
    assert_eq!(
        shown.len(),
        PAGE.iter().map(|(_, rows)| rows.len()).sum::<usize>()
    );
}

#[test]
fn each_row_edits_its_own_setting_and_only_a_change_is_asked_for() {
    let was = Preferences::default();
    let chosen = |setting: Setting, index| setting.edited(&FieldAction::Selected { index }, &was);
    let set = |setting: Setting, on| setting.edited(&FieldAction::Set { on }, &was);
    assert_eq!(
        chosen(Setting::Start, 2).map(|now| now.start),
        Some(Start::Storage)
    );
    assert_eq!(
        chosen(Setting::View, 1).map(|now| now.view),
        Some(ViewMode::SmallIcons)
    );
    assert_eq!(
        chosen(Setting::SortBy, 3).map(|now| now.sort.key),
        Some(SortKey::Modified)
    );
    assert_eq!(
        set(Setting::FoldersFirst, false).map(|now| now.sort.folders),
        Some(Folders::Among)
    );
    assert_eq!(
        set(Setting::HiddenItems, true).map(|now| now.presentation.hidden_items),
        Some(true)
    );
    assert_eq!(set(Setting::HideAppEndings, true), None, "already hidden");
    assert_eq!(chosen(Setting::View, 9), None, "no such choice");
    assert_eq!(set(Setting::View, true), None, "a choice is not switched");
}

#[test]
fn a_row_s_answer_is_asked_for_as_its_edit_and_the_command_restores() {
    let mut page = SettingsWindow::new(&Preferences::default());
    let asked = page.act(
        1,
        &FieldGroupAction {
            row: 3,
            action: FieldAction::Set { on: true },
        },
    );
    let Some(SettingsRequest::Edit { was, now }) = asked else {
        panic!("an edit, not {asked:?}");
    };
    assert!(!was.presentation.hidden_items && now.presentation.hidden_items);
    assert_eq!(page.shown(), &now, "the page shows what it asked for");
    let restore = page.act(
        4,
        &FieldGroupAction {
            row: 0,
            action: FieldAction::Activated,
        },
    );
    assert_eq!(restore, Some(SettingsRequest::Restore));
}

#[test]
fn tab_walks_the_rows_from_group_to_group_and_wraps_at_the_ends() {
    let theme = Theme::dark();
    let mut page = SettingsWindow::new(&Preferences::default());
    let window = whole(&page, Scale::ONE, &theme);
    let mut drew = damage::sink();
    assert_eq!(page.focus(), Some((0, 0)));
    for _ in 0..6 {
        page.on_key(press(TAB, false), window, (Scale::ONE, &theme), &mut drew);
    }
    assert_eq!(page.focus(), Some((1, 0)), "off the end of the first group");
    page.on_key(
        press(Key::Named(NamedKey::Up), false),
        window,
        (Scale::ONE, &theme),
        &mut drew,
    );
    assert_eq!(page.focus(), Some((0, 5)), "Up leaves a group by its top");
    page.on_key(
        press(Key::Named(NamedKey::Down), false),
        window,
        (Scale::ONE, &theme),
        &mut drew,
    );
    assert_eq!(page.focus(), Some((1, 0)), "and Down by its bottom");
    let mut start = SettingsWindow::new(&Preferences::default());
    start.on_key(press(TAB, true), window, (Scale::ONE, &theme), &mut drew);
    assert_eq!(
        start.focus(),
        Some((4, 0)),
        "Shift+Tab wraps to the last row"
    );
    assert!(!drew.is_empty());
}

#[test]
fn escape_closes_the_window() {
    let theme = Theme::dark();
    let mut page = SettingsWindow::new(&Preferences::default());
    let window = whole(&page, Scale::ONE, &theme);
    let mut drew = damage::sink();
    assert_eq!(
        page.on_key(
            press(Key::Named(NamedKey::Escape), false),
            window,
            (Scale::ONE, &theme),
            &mut drew
        ),
        Some(SettingsRequest::Close)
    );
}

#[test]
fn a_page_taller_than_its_window_scrolls_to_the_row_the_keyboard_is_on() {
    let theme = Theme::dark();
    let mut page = SettingsWindow::new(&Preferences::default());
    let (width, height) = page.extent(Scale::ONE, &theme);
    let window = Rect::new(0, 0, width, height / 3);
    let mut drew = damage::sink();
    page.on_key(press(TAB, true), window, (Scale::ONE, &theme), &mut drew);
    assert_eq!(page.focus(), Some((4, 0)));
    let laid = page.page(window, Scale::ONE, &theme);
    let shown = page.groups[4]
        .focus_rect(laid.groups[4], Scale::ONE, &theme)
        .expect("the focused row");
    assert!(
        shown.top() >= 0 && shown.bottom() <= laid.body.bottom(),
        "{shown:?} shows in {:?}",
        laid.body
    );
    assert!(page.on_wheel((0, -120), window, (Scale::ONE, &theme), &mut drew));
}

#[test]
fn adopting_the_store_s_settings_moves_every_control_to_them() {
    let theme = Theme::dark();
    let mut page = SettingsWindow::new(&Preferences::default());
    let window = whole(&page, Scale::ONE, &theme);
    let mut stored = Preferences {
        view: ViewMode::SmallIcons,
        ..Preferences::default()
    };
    stored.presentation.pictures.thumbnails = false;
    let mut drew = damage::sink();
    page.adopt(&stored, window, &mut drew);
    assert_eq!(page.shown(), &stored);
    let shown = controls(&page);
    assert_eq!(shown[1], (Some(1), None));
    assert_eq!(shown[10], (None, Some(false)));
    assert!(!drew.is_empty());

    let mut again = damage::sink();
    page.adopt(&stored, window, &mut again);
    assert!(again.is_empty(), "what is already shown moves nothing");
}

#[test]
fn a_refusal_is_stated_beneath_restore_defaults() {
    let theme = Theme::dark();
    let mut page = SettingsWindow::new(&Preferences::default());
    let window = whole(&page, Scale::ONE, &theme);
    let mut drew = damage::sink();
    page.state(
        Some(alloc::string::String::from("not saved")),
        window,
        &mut drew,
    );
    assert_eq!(page.groups[4].footnote(), Some("not saved"));
    assert!(!drew.is_empty());
}

#[test]
fn the_page_draws_at_every_density() {
    let theme = Theme::light();
    for percent in [100, 150, 250] {
        let scale = Scale::from_percent(percent).expect("a scale");
        let page = SettingsWindow::new(&Preferences::default());
        let window = whole(&page, scale, &theme);
        let mut surface = Surface::new(window.width, window.height).expect("a surface");
        page.render(&mut surface, window, scale, &theme);
        let first = surface.pixels()[0];
        assert!(
            surface.pixels().iter().any(|pixel| *pixel != first),
            "{percent}%: the page drew something"
        );
    }
}
