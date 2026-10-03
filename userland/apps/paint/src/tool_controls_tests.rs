use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_controls::Button;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::{Theme, ThemeRegistry};

use super::{BarOutcome, Control, Placement, ToolControls};
use crate::layout::Faces;
use crate::tool::{Options, Setting, Style, Tool, WHOLE_PIXELS};

/// A bar laid out across a window, with the options it edits.
struct Bar {
    controls: ToolControls,
    options: Options,
    placement: Placement,
    registry: ThemeRegistry,
}

impl Bar {
    fn new(tool: Tool) -> Self {
        Self::within(tool, 900)
    }

    fn within(tool: Tool, width: u32) -> Self {
        let registry = ThemeRegistry::with_builtins();
        let options = Options::default();
        let bar = ToolControls::new(tool, options, true);
        let placement = place(&bar, width, registry.active());
        Self {
            controls: bar,
            options,
            placement,
            registry,
        }
    }

    fn theme(&self) -> &Theme {
        self.registry.active()
    }

    fn pointer(&mut self, event: InputEvent, at: Point) -> BarOutcome {
        let theme = self.registry.active();
        self.controls.on_pointer(
            &event,
            at,
            &self.placement,
            &mut self.options,
            (Scale::ONE, theme),
            &mut Region::new(),
        )
    }

    /// Press and release the primary button at `at`.
    fn click(&mut self, at: Point) -> BarOutcome {
        self.pointer(InputEvent::PointerMoved { to: at }, at);
        let pressed = self.pointer(
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            },
            at,
        );
        let released = self.pointer(
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            },
            at,
        );
        pressed.and(released)
    }

    fn key_with(&mut self, key: Key, modifiers: Modifiers) -> BarOutcome {
        let theme = self.registry.active();
        self.controls.on_key(
            (key, modifiers),
            &self.placement,
            &mut self.options,
            (Scale::ONE, theme),
            &mut Region::new(),
        )
    }

    fn key(&mut self, key: Key) -> BarOutcome {
        self.key_with(key, Modifiers::default())
    }

    fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            self.key(Key::Char(character));
        }
    }

    /// The centre of setting `index`'s control.
    fn control(&self, index: usize) -> Point {
        self.placement
            .control(index)
            .expect("the setting is seated")
            .center()
    }
}

fn place(bar: &ToolControls, width: u32, theme: &Theme) -> Placement {
    let height = Button::height(Scale::ONE, theme);
    bar.place(
        Rect::new(8, 8, width, height),
        Rect::new(0, 0, width + 16, 640),
        Faces::of(theme, Scale::ONE),
        Scale::ONE,
        theme,
    )
}

fn ctrl() -> Modifiers {
    Modifiers {
        ctrl: true,
        ..Modifiers::default()
    }
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::default()
    }
}

#[test]
fn a_bar_holds_the_settings_its_tool_offers_in_order() {
    let options = Options::default();
    let ellipse = ToolControls::new(Tool::Ellipse, options, true);
    assert_eq!(
        ellipse.settings().collect::<alloc::vec::Vec<_>>(),
        [Setting::Size, Setting::Style, Setting::Smooth]
    );
    assert_eq!(ellipse.tool, Tool::Ellipse);
    assert_eq!(
        ToolControls::new(Tool::Eyedropper, options, true)
            .settings()
            .count(),
        0
    );
}

#[test]
fn the_bar_is_set_out_left_to_right_inside_its_bounds() {
    let bar = Bar::new(Tool::Ellipse);
    let bounds = bar.placement.bounds();
    let mut edge = bar.placement.caption.right();
    assert_eq!(bar.placement.caption.left(), bounds.left());
    for index in 0..3 {
        let seat = bar.placement.seat(index).expect("seated");
        assert!(
            seat.cell.left() > edge,
            "setting {index} follows what is before it"
        );
        assert_eq!(seat.cell.intersection(&bounds), seat.cell, "inside the bar");
        for part in [seat.label, seat.control, seat.unit] {
            assert!(
                part.is_empty() || part.intersection(&seat.cell) == part,
                "its parts inside its cell"
            );
        }
        edge = seat.cell.right();
    }
    let faces = Faces::of(bar.theme(), Scale::ONE);
    assert_eq!(
        u32::try_from(edge - bounds.left()).expect("a width"),
        bar.controls.natural_width(faces, Scale::ONE, bar.theme()),
        "the natural width is what the placement uses"
    );
}

#[test]
fn a_bar_too_narrow_seats_whole_settings_only() {
    let wide = Bar::new(Tool::Ellipse);
    let needed =
        u32::try_from(wide.placement.seat(1).expect("seated").cell.right() - 8).expect("a width");
    let narrow = Bar::within(Tool::Ellipse, needed);
    assert!(narrow.placement.seat(0).is_some());
    assert!(narrow.placement.seat(1).is_some());
    assert!(
        narrow.placement.seat(2).is_none(),
        "the switch has no room and is not set out"
    );
    let tight = Bar::within(Tool::Ellipse, needed - 1);
    assert!(tight.placement.seat(1).is_none());
    assert!(
        tight.placement.seat(2).is_none(),
        "nothing after a setting cut off"
    );
}

#[test]
fn a_size_typed_applies_as_it_is_typed_and_settles() {
    let mut bar = Bar::new(Tool::Brush);
    bar.click(bar.control(0));
    assert!(
        bar.controls.focus().is_some(),
        "a press gives the field the keyboard"
    );
    bar.key_with(Key::Char('a'), ctrl());
    assert_eq!(bar.key(Key::Char('1')), BarOutcome::Changed);
    assert_eq!(bar.options.brush.size, 1, "live as it is typed");
    bar.type_text("2");
    assert_eq!(bar.options.brush.size, 12);
    assert_eq!(
        bar.key(Key::Named(NamedKey::Enter)),
        BarOutcome::Taken,
        "the settle lands where the typing already had it"
    );
    assert_eq!(bar.options.brush.size, 12);
}

#[test]
fn a_number_past_its_bound_moves_nothing_until_it_is_settled() {
    let mut bar = Bar::new(Tool::Brush);
    bar.click(bar.control(0));
    bar.key_with(Key::Char('a'), ctrl());
    bar.type_text("99");
    assert_eq!(bar.options.brush.size, 9, "99 is past the most a brush is");
    bar.controls
        .commit(&bar.placement, &mut bar.options, &mut Region::new());
    assert_eq!(bar.options.brush.size, 64, "a commit holds it to the bound");
}

#[test]
fn keys_step_a_field_and_the_wheel_steps_only_one_with_the_keyboard() {
    let mut bar = Bar::new(Tool::Brush);
    let field = bar.control(0);
    let turn = InputEvent::PointerScrolled {
        dx: 0,
        dy: -SCROLL_UNITS_PER_DETENT,
    };
    assert_eq!(bar.pointer(turn, field), BarOutcome::Taken);
    assert_eq!(
        bar.options.brush.size, 4,
        "an unfocused field takes no wheel"
    );
    bar.click(field);
    bar.key(Key::Named(NamedKey::Up));
    assert_eq!(bar.options.brush.size, 5);
    bar.key(Key::Named(NamedKey::PageUp));
    assert_eq!(bar.options.brush.size, 13, "a page is eight");
    assert_eq!(bar.pointer(turn, field), BarOutcome::Changed);
    assert_eq!(
        bar.options.brush.size, 14,
        "a detent away from the user steps up"
    );
    let away = Point::new(field.x, field.y + 200);
    assert_eq!(bar.pointer(turn, away), BarOutcome::Ignored);
    assert_eq!(
        bar.options.brush.size, 14,
        "the wheel reaches the setting under it alone"
    );
}

#[test]
fn a_number_field_claims_every_plain_key_and_passes_the_owners_chords() {
    let mut bar = Bar::new(Tool::Brush);
    bar.click(bar.control(0));
    assert_eq!(
        bar.key(Key::Char('b')),
        BarOutcome::Taken,
        "a letter is typing, never a tool's key"
    );
    assert_eq!(bar.options.brush.size, 4);
    assert_eq!(bar.key_with(Key::Char('s'), ctrl()), BarOutcome::Ignored);
    assert_eq!(
        bar.key(Key::Named(NamedKey::Escape)),
        BarOutcome::Ignored,
        "an Escape with nothing to take back is the owner's"
    );
}

#[test]
fn escape_takes_back_what_was_typed() {
    let mut bar = Bar::new(Tool::Airbrush);
    bar.click(bar.control(3));
    bar.key_with(Key::Char('a'), ctrl());
    bar.type_text("80");
    assert_eq!(bar.options.airbrush.flow, 80);
    assert_eq!(bar.key(Key::Named(NamedKey::Escape)), BarOutcome::Changed);
    assert_eq!(
        bar.options.airbrush.flow, 10,
        "back to where it was settled"
    );
}

#[test]
fn tab_walks_the_settings_and_off_the_bar_either_way() {
    let mut bar = Bar::new(Tool::Ellipse);
    assert!(bar
        .controls
        .enter_focus(true, &bar.placement, &mut Region::new()));
    assert_eq!(bar.controls.focus, Some(0));
    assert_eq!(bar.key(Key::Named(NamedKey::Tab)), BarOutcome::Taken);
    assert_eq!(bar.controls.focus, Some(1));
    bar.key(Key::Named(NamedKey::Tab));
    assert_eq!(bar.controls.focus, Some(2));
    assert_eq!(
        bar.key(Key::Named(NamedKey::Tab)),
        BarOutcome::Left { forward: true }
    );
    assert!(bar.controls.focus().is_none());
    assert!(bar
        .controls
        .enter_focus(false, &bar.placement, &mut Region::new()));
    assert_eq!(
        bar.controls.focus,
        Some(2),
        "entered from behind on its last"
    );
    bar.key_with(Key::Named(NamedKey::Tab), shift());
    bar.key_with(Key::Named(NamedKey::Tab), shift());
    assert_eq!(
        bar.key_with(Key::Named(NamedKey::Tab), shift()),
        BarOutcome::Left { forward: false }
    );
    let mut empty = Bar::new(Tool::Pencil);
    assert!(
        !empty
            .controls
            .enter_focus(true, &empty.placement, &mut Region::new()),
        "a tool with no settings takes no keyboard"
    );
    assert_eq!(empty.key(Key::Named(NamedKey::Tab)), BarOutcome::Ignored);
}

#[test]
fn a_tab_settles_the_field_it_leaves() {
    let mut bar = Bar::new(Tool::Airbrush);
    bar.click(bar.control(0));
    bar.key_with(Key::Char('a'), ctrl());
    bar.type_text("90");
    assert_eq!(bar.options.airbrush.size, 9);
    bar.key(Key::Named(NamedKey::Tab));
    assert_eq!(
        bar.options.airbrush.size, 64,
        "what was left in it is held to the bound"
    );
}

#[test]
fn the_style_list_owns_the_pointer_and_keyboard_until_it_closes() {
    let mut bar = Bar::new(Tool::Rectangle);
    bar.click(bar.control(1));
    assert!(bar.controls.listing());
    let popup = bar
        .controls
        .popup_rect(&bar.placement, Scale::ONE, bar.registry.active());
    assert!(!popup.is_empty() && popup.top() >= bar.placement.bounds().bottom());
    assert_eq!(
        bar.key(Key::Char('b')),
        BarOutcome::Taken,
        "the list holds the keys"
    );
    let elsewhere = Point::new(popup.right() + 40, popup.bottom() + 40);
    assert_eq!(bar.click(elsewhere), BarOutcome::Taken);
    assert!(!bar.controls.listing(), "a press away closes it");
    assert_eq!(bar.options.style, Style::Outline, "and chooses nothing");

    bar.key(Key::Named(NamedKey::Down));
    assert!(bar.controls.listing(), "Down opens the list on the choice");
    bar.key(Key::Named(NamedKey::Down));
    assert_eq!(bar.key(Key::Named(NamedKey::Enter)), BarOutcome::Changed);
    assert_eq!(bar.options.style, Style::Filled);
    assert!(!bar.controls.listing());
}

#[test]
fn the_switch_flips_smoothing_and_is_held_off_for_whole_pixels() {
    let mut bar = Bar::new(Tool::Line);
    let switch = bar.control(1);
    assert_eq!(bar.click(switch), BarOutcome::Changed);
    assert!(!bar.options.smooth);
    assert_eq!(bar.key(Key::Char(' ')), BarOutcome::Changed);
    assert!(bar.options.smooth, "Space flips it back");

    let options = bar.options;
    assert!(bar.controls.allow_partial(false, options));
    let Control::Switch(check) = &bar.controls.items[1].control else {
        panic!("a switch");
    };
    assert!(!check.state().enabled);
    assert_eq!(
        check.selection(),
        tairix_controls::SelectionState::Unselected
    );
    assert_eq!(
        bar.click(switch),
        BarOutcome::Taken,
        "a held-off switch flips nothing"
    );
    assert!(
        bar.options.smooth,
        "the option keeps its value for a colour picture"
    );
    let (_, tip) = bar.controls.tip(&bar.placement, switch).expect("a tip");
    assert_eq!(tip, WHOLE_PIXELS, "the tip says why");
    assert!(
        !bar.controls.allow_partial(false, options),
        "no change twice"
    );
}

#[test]
fn each_setting_carries_its_tip_and_the_name_none() {
    let bar = Bar::new(Tool::Fill);
    let (cell, tip) = bar
        .controls
        .tip(&bar.placement, bar.control(0))
        .expect("a tip");
    assert_eq!(tip, Setting::Tolerance.tip());
    assert_eq!(Some(cell), bar.placement.seat(0).map(|seat| seat.cell));
    assert_eq!(
        bar.controls
            .tip(&bar.placement, bar.placement.caption.center()),
        None
    );
    assert!(bar.controls.text_at(&bar.placement, bar.control(0)));
    assert!(!bar
        .controls
        .text_at(&bar.placement, bar.placement.caption.center()));
}

#[test]
fn a_press_on_the_bar_away_from_a_setting_is_taken_and_moves_no_keyboard() {
    let mut bar = Bar::new(Tool::Brush);
    let empty = Point::new(bar.placement.bounds().right() - 4, bar.control(0).y);
    assert_eq!(bar.click(empty), BarOutcome::Taken);
    assert!(bar.controls.focus().is_none());
    let outside = Point::new(empty.x, empty.y + 100);
    assert_eq!(bar.click(outside), BarOutcome::Ignored);
}

#[test]
fn every_bar_seats_each_setting_across_the_least_width_in_its_rows() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let faces = Faces::of(theme, Scale::ONE);
    let least = ToolControls::least_width(faces, Scale::ONE, theme);
    let most = ToolControls::most_rows(least, faces, Scale::ONE, theme);
    for tool in Tool::ALL {
        let bar = ToolControls::new(tool, Options::default(), true);
        let rows = bar.rows(least, faces, Scale::ONE, theme);
        assert!(rows <= most, "{tool:?}");
        let height = Button::height(Scale::ONE, theme);
        let gap = super::spacing(Scale::ONE, theme).0;
        let bounds = Rect::new(8, 8, least, height * rows + gap * (rows - 1));
        let placement = bar.place(bounds, bounds, faces, Scale::ONE, theme);
        for index in 0..tool.settings().len() {
            let seat = placement
                .seat(index)
                .unwrap_or_else(|| panic!("{tool:?} {index} seated"));
            assert_eq!(
                seat.cell.intersection(&bounds),
                seat.cell,
                "{tool:?} {index} inside"
            );
        }
        let wide = bar.natural_width(faces, Scale::ONE, theme);
        assert_eq!(
            bar.rows(wide, faces, Scale::ONE, theme),
            1,
            "one row at its natural width"
        );
    }
}

#[test]
fn a_setting_with_no_room_left_in_its_row_starts_the_next() {
    let wide = Bar::new(Tool::Brush);
    let faces = Faces::of(wide.theme(), Scale::ONE);
    let natural = wide.controls.natural_width(faces, Scale::ONE, wide.theme());
    let first_row =
        u32::try_from(wide.placement.seat(2).expect("seated").cell.right() - 8).expect("a width");
    let height = Button::height(Scale::ONE, wide.theme());
    let gap = super::spacing(Scale::ONE, wide.theme()).0;
    let bounds = Rect::new(8, 8, first_row, height * 3 + gap * 2);
    let theme = wide.registry.active();
    let wrapped = wide
        .controls
        .place(bounds, bounds, faces, Scale::ONE, theme);
    let row_of = |index: usize| {
        (wrapped.seat(index).expect("seated").cell.top() - 8)
            / i32::try_from(height + gap).expect("small")
    };
    assert_eq!([row_of(0), row_of(1), row_of(2)], [0, 0, 0]);
    assert_eq!(row_of(3), 1, "the fourth starts the next row");
    assert_eq!(
        wrapped.seat(3).expect("seated").cell.left(),
        8,
        "at its start"
    );
    assert!(natural > first_row);
    assert!(wide.controls.rows(first_row, faces, Scale::ONE, theme) >= 2);
}

#[test]
fn a_held_off_switch_is_no_stop_for_the_keyboard() {
    let mut bar = Bar::new(Tool::Line);
    bar.click(bar.control(1));
    assert_eq!(bar.controls.focus(), Some(1));
    let options = bar.options;
    bar.controls.allow_partial(false, options);
    assert_eq!(
        bar.controls.focus(),
        None,
        "held off, it gives the keyboard up"
    );
    bar.click(bar.control(1));
    assert_eq!(
        bar.controls.focus(),
        None,
        "and a press does not give it back"
    );
    bar.click(bar.control(0));
    assert_eq!(
        bar.key(Key::Named(NamedKey::Tab)),
        BarOutcome::Left { forward: true },
        "Tab passes it"
    );
}

#[test]
fn a_palette_picture_holds_off_what_lays_part_of_a_pixel() {
    let mut bar = Bar::new(Tool::Brush);
    let options = bar.options;
    assert!(bar.controls.allow_partial(false, options));
    let held_off: alloc::vec::Vec<bool> = bar
        .controls
        .items
        .iter()
        .map(|item| !item.control.enabled())
        .collect();
    assert_eq!(
        held_off,
        [false, true, true, true, false, true],
        "size and spacing stay"
    );
    let hardness = bar.control(1);
    assert_eq!(bar.click(hardness), BarOutcome::Taken);
    assert_eq!(
        bar.controls.focus(),
        None,
        "a held-off field takes no keyboard"
    );
    let (_, tip) = bar.controls.tip(&bar.placement, hardness).expect("a tip");
    assert_eq!(tip, WHOLE_PIXELS);
    assert!(bar.controls.allow_partial(true, options));
    assert!(bar.controls.items.iter().all(|item| item.control.enabled()));
}
