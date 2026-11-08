//! The settings window over a registry: an edit live and settled, a command
//! row's ask, Restore, a category chosen, what the store says, and Escape.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_appconf::{as_bool, as_u32, bool_text, overwrite, Live, Registry};
use tairix_controls::{
    Button, ColumnAction, FieldAction, FieldControl, FieldGroup, FieldRow, NumberField, Tab, Toggle,
};
use tairix_geometry::{Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
use tairix_theme::Theme;

use super::{Edit, Preferences, SettingsRequest, SettingsWindow, Then};
use crate::docapp::{AppRequest, AppView, Relayout};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Prefs {
    grid: bool,
    across: u32,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum PrefKey {
    Grid,
    Across,
}

impl Registry for Prefs {
    type Key = PrefKey;
    const KEYS: &'static [PrefKey] = &[PrefKey::Grid, PrefKey::Across];

    fn name(key: PrefKey) -> &'static str {
        match key {
            PrefKey::Grid => "grid.shown",
            PrefKey::Across => "grid.across",
        }
    }

    fn read(&mut self, key: PrefKey, text: &str) -> bool {
        match key {
            PrefKey::Grid => as_bool(text).map(|on| self.grid = on).is_ok(),
            PrefKey::Across => as_u32(text).map(|n| self.across = n).is_ok(),
        }
    }

    fn spell(&self, key: PrefKey, out: &mut String) -> bool {
        match key {
            PrefKey::Grid => out.write_str(bool_text(self.grid)).is_ok(),
            PrefKey::Across => write!(out, "{}", self.across).is_ok(),
        }
    }
}

impl Live for Prefs {
    fn take(&mut self, other: &Self, key: PrefKey) -> bool {
        match key {
            PrefKey::Grid => overwrite(&mut self.grid, other.grid),
            PrefKey::Across => overwrite(&mut self.across, other.across),
        }
    }
}

impl Preferences for Prefs {
    const TITLE: &'static str = "Test settings";
    type Ask = &'static str;

    fn categories() -> Vec<Tab> {
        vec![Tab::new("General"), Tab::new("Grid")]
    }

    fn groups(&self, category: usize) -> Vec<FieldGroup> {
        let rows = match category {
            0 => vec![FieldRow::new(
                "Show the grid",
                FieldControl::Toggle(Toggle::new("", self.grid)),
            )],
            _ => vec![
                FieldRow::new(
                    "Across",
                    FieldControl::Number(NumberField::new(
                        i32::try_from(self.across).unwrap_or(1),
                        1,
                        64,
                    )),
                ),
                FieldRow::new(
                    "Panes",
                    FieldControl::Button(Button::labelled("Use the front window's")),
                ),
            ],
        };
        vec![FieldGroup::new("GRID", rows)]
    }

    fn act(&mut self, category: usize, action: &ColumnAction) -> Option<&'static str> {
        match (category, action.action.row, &action.action.action) {
            (0, 0, FieldAction::Set { on }) => self.grid = *on,
            (1, 0, FieldAction::Number(number)) => {
                self.across = u32::try_from(number.value()).unwrap_or(1);
            }
            (1, 1, FieldAction::Activated) => return Some("take"),
            _ => {}
        }
        None
    }

    fn reshapes(&self, _: &Self, _: usize) -> bool {
        false
    }
}

type Window = SettingsWindow<Prefs, ()>;

fn opened(record: Prefs, category: usize) -> (Window, tairix_controls::SettingsLayout, Theme) {
    let theme = Theme::light();
    let mut window = Window::new(record, category);
    let layout = window.layout(600, 440, &theme, Scale::ONE, ());
    window.settle(&layout, &mut Region::default());
    (window, layout, theme)
}

type Asked = crate::docapp::Outcome<AppRequest<SettingsRequest<Prefs>>>;

fn feed(
    window: &mut Window,
    event: &InputEvent,
    layout: &tairix_controls::SettingsLayout,
    theme: &Theme,
) -> Asked {
    window.input(event, 0, layout, Scale::ONE, theme, &mut Region::default())
}

fn key(
    window: &mut Window,
    key: Key,
    layout: &tairix_controls::SettingsLayout,
    theme: &Theme,
) -> Asked {
    let event = InputEvent::KeyPressed {
        key,
        modifiers: Modifiers::default(),
    };
    feed(window, &event, layout, theme)
}

/// The request an outcome carries for the application.
fn own(outcome: Asked) -> Option<SettingsRequest<Prefs>> {
    match outcome.request? {
        AppRequest::Own(request) => Some(request),
        AppRequest::Close => None,
    }
}

/// Tab into the column, past the sidebar.
fn into_column(window: &mut Window, layout: &tairix_controls::SettingsLayout, theme: &Theme) {
    for _ in 0..2 {
        let _ = key(window, Key::Named(NamedKey::Tab), layout, theme);
    }
}

#[test]
fn a_switch_is_one_settled_edit_from_what_was_to_what_is() {
    let (mut window, layout, theme) = opened(Prefs::default(), 0);
    assert_eq!(window.title(), "Test settings");
    into_column(&mut window, &layout, &theme);
    let asked = own(key(&mut window, Key::Char(' '), &layout, &theme));
    let now = Prefs {
        grid: true,
        ..Prefs::default()
    };
    assert_eq!(
        asked,
        Some(SettingsRequest {
            edit: Some(Edit {
                was: Prefs::default(),
                now: now.clone(),
                settled: true,
            }),
            then: None,
        })
    );
    assert_eq!(window.record(), &now);
}

#[test]
fn typing_is_a_live_edit_and_enter_settles_it() {
    let record = Prefs {
        across: 16,
        ..Prefs::default()
    };
    let (mut window, layout, theme) = opened(record.clone(), 1);
    into_column(&mut window, &layout, &theme);
    let _ = key(&mut window, Key::Named(NamedKey::End), &layout, &theme);
    let _ = key(
        &mut window,
        Key::Named(NamedKey::Backspace),
        &layout,
        &theme,
    );
    let live = own(key(&mut window, Key::Char('2'), &layout, &theme)).and_then(|asked| asked.edit);
    assert_eq!(
        live.as_ref().map(|edit| (edit.now.across, edit.settled)),
        Some((12, false))
    );
    let settled = own(key(
        &mut window,
        Key::Named(NamedKey::Enter),
        &layout,
        &theme,
    ))
    .and_then(|asked| asked.edit);
    assert_eq!(
        settled.map(|edit| (edit.now.across, edit.settled)),
        Some((12, true))
    );
}

#[test]
fn a_command_row_asks_the_application_and_restore_is_asked_for() {
    let (mut window, layout, theme) = opened(Prefs::default(), 1);
    into_column(&mut window, &layout, &theme);
    let _ = key(&mut window, Key::Named(NamedKey::Down), &layout, &theme);
    let asked = own(key(&mut window, Key::Char(' '), &layout, &theme));
    assert_eq!(
        asked.and_then(|asked| asked.then),
        Some(Then::Ask("take")),
        "the panes row is the application's to carry out"
    );
    let _ = key(&mut window, Key::Named(NamedKey::Tab), &layout, &theme);
    let restore = own(key(&mut window, Key::Char(' '), &layout, &theme));
    assert_eq!(restore.and_then(|asked| asked.then), Some(Then::Restore));
}

#[test]
fn choosing_a_category_shows_its_groups_and_lays_the_window_out_again() {
    let (mut window, layout, theme) = opened(Prefs::default(), 0);
    assert_eq!(window.frame().column().groups()[0].rows().len(), 1);
    let _ = key(&mut window, Key::Named(NamedKey::Tab), &layout, &theme);
    let _ = key(&mut window, Key::Named(NamedKey::Down), &layout, &theme);
    let chosen = key(&mut window, Key::Named(NamedKey::Enter), &layout, &theme);
    assert_eq!(chosen.relayout, Relayout::Whole);
    assert!(chosen.request.is_none(), "showing a category edits nothing");
    assert_eq!(window.category(), 1);
    assert_eq!(window.frame().column().groups()[0].rows().len(), 2);
}

#[test]
fn the_store_is_adopted_unless_a_control_is_held_and_its_refusal_said() {
    let (mut window, layout, theme) = opened(Prefs::default(), 0);
    let stored = Prefs {
        grid: true,
        across: 8,
    };
    let mut damage = Region::default();
    assert!(
        window.adopt(&stored, &layout, &mut damage),
        "laid out again"
    );
    assert_eq!(window.record(), &stored);
    assert!(!window.adopt(&stored, &layout, &mut damage), "nothing new");
    window.say(Some(String::from("not saved")), &layout, &mut damage);
    assert_eq!(window.frame().note(), Some("not saved"));
    let close = key(&mut window, Key::Named(NamedKey::Escape), &layout, &theme);
    assert!(matches!(close.request, Some(AppRequest::Close)));
}
