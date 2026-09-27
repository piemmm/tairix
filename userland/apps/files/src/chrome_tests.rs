//! Tests for the per-window chrome bands, the keys that toggle them, and the
//! window's accelerators.

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use super::{Accelerator, Chrome};
use tairix_abi::input::{KeyValue, Modifiers, NamedKeyCode};
use tairix_browse::{ManagerTool, Places, ToolbarBand, ToolbarCommand};

/// One rail model, so `rail` has something to hand back.
fn places() -> Places {
    let home: Vec<_> = vec!["Users".to_string(), "ann".to_string()];
    Places::new(&home, &[])
}

#[test]
fn a_window_opens_showing_the_listing_alone() {
    assert_eq!(
        Chrome::HIDDEN,
        Chrome {
            rail: false,
            toolbar: ToolbarBand::Hidden,
        }
    );
    let places = places();
    assert!(
        Chrome::HIDDEN.rail(&places).is_none(),
        "a hidden rail is not hit-testable"
    );
}

#[test]
fn f9_toggles_the_rail_and_ctrl_f9_the_toolbar() {
    let f9 = KeyValue::Named(NamedKeyCode::F9);
    let plain = Modifiers::default();
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };

    let shown = Chrome::HIDDEN.toggled_by(f9, plain).expect("F9 toggles");
    assert!(shown.rail);
    assert_eq!(shown.toolbar, ToolbarBand::Hidden, "only the rail moved");
    let places = places();
    assert!(shown.rail(&places).is_some());

    let banded = Chrome::HIDDEN
        .toggled_by(f9, ctrl)
        .expect("Ctrl+F9 toggles");
    assert_eq!(banded.toolbar, ToolbarBand::Shown);
    assert!(!banded.rail, "only the toolbar moved");

    // Both toggles are reversible from wherever they are.
    assert_eq!(shown.toggled_by(f9, plain), Some(Chrome::HIDDEN));
    assert_eq!(banded.toggled_by(f9, ctrl), Some(Chrome::HIDDEN));
}

#[test]
fn any_other_key_names_neither_band() {
    let plain = Modifiers::default();
    for key in [
        KeyValue::Named(NamedKeyCode::F5),
        KeyValue::Named(NamedKeyCode::Escape),
        KeyValue::Char('9'),
    ] {
        assert_eq!(Chrome::HIDDEN.toggled_by(key, plain), None, "{key:?}");
    }
}

#[test]
fn the_accelerators_name_the_toolbars_commands_and_the_new_folder_tool() {
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    let ctrl_shift = Modifiers {
        ctrl: true,
        shift: true,
        ..Modifiers::default()
    };
    let named = |code| KeyValue::Named(code);
    for (key, modifiers, want) in [
        (named(NamedKeyCode::Left), alt, ToolbarCommand::Back),
        (named(NamedKeyCode::Right), alt, ToolbarCommand::Forward),
        (named(NamedKeyCode::Up), alt, ToolbarCommand::Up),
        (
            named(NamedKeyCode::F5),
            Modifiers::default(),
            ToolbarCommand::Refresh,
        ),
    ] {
        assert_eq!(
            Accelerator::of(key, modifiers),
            Some(Accelerator::Command(want)),
            "{key:?}"
        );
    }
    for letter in ['n', 'N'] {
        assert_eq!(
            Accelerator::of(KeyValue::Char(letter), ctrl_shift),
            Some(Accelerator::Tool(ManagerTool::NewFolder)),
            "Shift may deliver {letter:?}"
        );
    }
}

#[test]
fn a_key_a_field_moves_or_types_with_is_no_accelerator() {
    let plain = Modifiers::default();
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    for (key, modifiers) in [
        (KeyValue::Named(NamedKeyCode::Left), plain),
        (KeyValue::Named(NamedKeyCode::Up), plain),
        (KeyValue::Named(NamedKeyCode::Down), alt),
        // Alt+Enter is the selection's Properties: the focused field's.
        (KeyValue::Named(NamedKeyCode::Enter), alt),
        (KeyValue::Char('n'), ctrl),
        (KeyValue::Char('n'), plain),
        // The chrome toggle is applied before any field sees a key.
        (KeyValue::Named(NamedKeyCode::F9), plain),
    ] {
        assert_eq!(
            Accelerator::of(key, modifiers),
            None,
            "{key:?} {modifiers:?}"
        );
    }
}
