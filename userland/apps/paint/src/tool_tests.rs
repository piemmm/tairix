use tairix_controls::{FieldAction, FieldControl};

use super::{
    from_permille, permille, strip, strip_item, strip_tip, Options, StripItem, Style, Tool,
    ViewCommand, MAX_SIZE,
};

#[test]
fn a_slider_reaches_every_whole_size_and_back() {
    for size in 1..=MAX_SIZE {
        assert_eq!(from_permille(permille(size, MAX_SIZE), MAX_SIZE), size);
    }
    assert_eq!(from_permille(0, MAX_SIZE), 1);
    assert_eq!(from_permille(1000, MAX_SIZE), MAX_SIZE);
}

#[test]
fn each_key_chooses_its_tool_and_names_itself_in_its_label() {
    for tool in Tool::ALL {
        let key = tool
            .label()
            .rsplit_once('(')
            .and_then(|(_, rest)| rest.chars().next())
            .expect("a key");
        assert_eq!(Tool::for_key(key), Some(tool));
    }
    assert_eq!(Tool::for_key('z'), None);
}

#[test]
fn the_strip_is_the_tools_then_the_commands() {
    let toolbar = strip(Tool::Brush);
    assert_eq!(toolbar.len(), Tool::ALL.len() + 5);
    assert!(toolbar.is_active(2));
    assert_eq!(strip_item(0), Some(StripItem::Tool(Tool::Select)));
    assert_eq!(
        strip_item(Tool::ALL.len()),
        Some(StripItem::Command(ViewCommand::ZoomOut))
    );
    assert_eq!(strip_item(Tool::ALL.len() + 5), None);
    assert_eq!(strip_tip(Tool::ALL.len() + 4), Some("Pixel grid (G)"));
}

#[test]
fn a_panel_offers_the_settings_its_tool_uses_and_adopts_their_values() {
    let mut options = Options::default();
    assert!(options.panel(Tool::Picker, true).is_empty());
    let panel = options.panel(Tool::Ellipse, true);
    assert_eq!(panel.len(), 3);
    let label = options.adopt(Tool::Ellipse, 0, &FieldAction::Settled { permille: 1000 });
    assert_eq!(options.size, MAX_SIZE);
    assert_eq!(label.as_deref(), Some("Size: 64 px"));
    options.adopt(Tool::Ellipse, 1, &FieldAction::Selected { index: 2 });
    assert_eq!(options.style, Style::Both);
    options.adopt(Tool::Ellipse, 2, &FieldAction::Set { on: false });
    assert!(!options.smooth);
    assert!(options
        .adopt(Tool::Ellipse, 9, &FieldAction::Set { on: true })
        .is_none());
}

#[test]
fn smoothing_is_offered_but_held_off_on_a_palette_picture() {
    let options = Options::default();
    let panel = options.panel(Tool::Brush, false);
    let row = &panel.rows()[1];
    assert!(!row.state().is_actionable());
    let FieldControl::Toggle(toggle) = row.control() else {
        panic!("a toggle");
    };
    assert!(!toggle.is_on());
}
