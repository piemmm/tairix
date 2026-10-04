use tairix_controls::ScrollOrientation;

use super::{
    mark_grid, tool_box, view_strip, Options, Setting, Style, Tool, ViewCommand, MAX_SIZE,
    MOST_SETTINGS, VIEW_COMMANDS,
};

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
    assert_eq!(Tool::for_key('q'), None);
}

#[test]
fn the_tool_box_is_every_tool_down_a_column_the_one_in_use_marked() {
    let tools = tool_box(Tool::Brush);
    assert_eq!(tools.len(), Tool::ALL.len());
    assert_eq!(
        tools,
        tool_box(Tool::Brush).with_orientation(ScrollOrientation::Vertical),
        "already a column"
    );
    assert!(tools.is_active(2));
    assert_eq!(
        (0..tools.len()).filter(|&at| tools.is_active(at)).count(),
        1
    );
}

#[test]
fn the_view_strip_marks_the_grid_while_it_shows() {
    let mut strip = view_strip(false);
    assert_eq!(strip.len(), VIEW_COMMANDS.len());
    assert!((0..strip.len()).all(|at| !strip.is_active(at)));
    mark_grid(&mut strip, true);
    let grid = VIEW_COMMANDS
        .iter()
        .position(|&(_, command, _)| command == ViewCommand::Grid)
        .expect("a grid command");
    assert!(strip.is_active(grid));
    assert_eq!(strip, view_strip(true));
    mark_grid(&mut strip, false);
    assert_eq!(strip, view_strip(false));
}

#[test]
fn the_most_settings_is_what_the_busiest_tool_offers() {
    let busiest = Tool::ALL
        .iter()
        .map(|tool| tool.settings().len())
        .max()
        .expect("tools");
    assert_eq!(MOST_SETTINGS, busiest);
    assert_eq!(Tool::Ellipse.settings().len(), 3);
    assert!(Tool::Eyedropper.settings().is_empty());
}

#[test]
fn a_number_setting_is_held_to_its_bounds() {
    let mut options = Options::default();
    options.set_number(Tool::Line, Setting::Size, 0);
    assert_eq!(options.size, 1);
    options.set_number(Tool::Line, Setting::Size, 1000);
    assert_eq!(options.size, MAX_SIZE);
    options.set_number(Tool::Fill, Setting::Tolerance, -5);
    assert_eq!(options.tolerance, 0);
    options.set_number(Tool::Fill, Setting::Tolerance, 400);
    assert_eq!(options.tolerance, u8::MAX);
    options.set_number(Tool::Airbrush, Setting::Flow, 0);
    assert_eq!(options.airbrush.flow, 1);
    options.set_number(Tool::Airbrush, Setting::Flow, 55);
    assert_eq!(options.number(Tool::Airbrush, Setting::Flow), Some(55));
    let before = options;
    options.set_number(Tool::Rectangle, Setting::Style, 2);
    options.set_number(Tool::Brush, Setting::Smooth, 0);
    options.set_number(Tool::Line, Setting::Hardness, 3);
    assert_eq!(
        options, before,
        "a choice, a switch and a tool with no tip hold no such number"
    );
    assert_eq!(options.number(Tool::Rectangle, Setting::Style), None);
    assert_eq!(options.number(Tool::Line, Setting::Flow), None);
}

#[test]
fn every_number_setting_has_bounds_holding_its_default_and_steps() {
    let options = Options::default();
    for tool in Tool::ALL {
        for &setting in tool.settings() {
            let Some((least, most)) = setting.bounds() else {
                continue;
            };
            let value = options.number(tool, setting).expect("a number");
            assert!((least..=most).contains(&value), "{tool:?} {setting:?}");
        }
    }
    for setting in [
        Setting::Size,
        Setting::Tolerance,
        Setting::Hardness,
        Setting::Opacity,
        Setting::Flow,
        Setting::Spacing,
        Setting::Feather,
    ] {
        let (line, page) = setting.steps();
        assert!(line > 0 && page > line, "{setting:?}");
        assert!(!setting.tip().is_empty());
    }
    for setting in [Setting::Style, Setting::Smooth] {
        assert_eq!(setting.bounds(), None);
        assert_eq!(setting.unit(), "");
    }
    assert_eq!(Style::ALL.len(), 3);
}

#[test]
fn a_choice_setting_lists_its_choices_and_holds_one() {
    use super::Marquee;
    use crate::mask::Combine;
    let mut options = Options::default();
    for (setting, count) in [
        (Setting::Style, 3),
        (Setting::Marquee, 5),
        (Setting::Combine, 4),
    ] {
        assert_eq!(setting.choices().len(), count);
        assert_eq!(
            options.choice(setting),
            Some(0),
            "{setting:?} starts on its first"
        );
        assert_eq!(setting.bounds(), None, "a choice is not a number");
    }
    assert!(options.set_choice(Setting::Marquee, 4));
    assert_eq!(options.marquee, Marquee::Wand);
    assert_eq!(Setting::Marquee.choices()[4], Marquee::Wand.label());
    assert!(!options.set_choice(Setting::Marquee, 4), "already held");
    assert!(
        !options.set_choice(Setting::Combine, 4),
        "one past its choices"
    );
    assert!(options.set_choice(Setting::Combine, 2));
    assert_eq!(options.combine, Combine::Subtract);
    assert_eq!(Setting::Size.choices(), &[] as &[&str]);
    assert_eq!(options.choice(Setting::Size), None);
    assert!(Tool::Select.settings().contains(&Setting::Feather));
    assert_eq!(Setting::Feather.bounds(), Some((0, 100)));
}

#[test]
fn each_tipped_tool_keeps_a_tip_of_its_own() {
    let mut options = Options::default();
    options.set_number(Tool::Brush, Setting::Size, 30);
    options.set_number(Tool::Eraser, Setting::Hardness, 20);
    assert_eq!(options.brush.size, 30);
    assert_eq!(
        options.number(Tool::Airbrush, Setting::Size),
        Some(16),
        "its own"
    );
    assert_eq!(
        options.number(Tool::Line, Setting::Size),
        Some(4),
        "a line's width apart"
    );
    assert_eq!(options.eraser.hardness, 20);
    assert_eq!(options.number(Tool::Brush, Setting::Hardness), Some(100));
    for tool in Tool::ALL {
        assert_eq!(options.tip(tool).is_some(), tool.tipped(), "{tool:?}");
    }
    assert!(Setting::Opacity.lays_part() && !Setting::Spacing.lays_part());
}
