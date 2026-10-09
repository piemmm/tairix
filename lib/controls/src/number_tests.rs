//! The number field: live typing, the bounds, steps, settling, and what it
//! refuses.

use alloc::vec::Vec;

use tairix_geometry::{Rect, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::damage;
use crate::number::{NumberAction, NumberField};
use crate::state::{AuthorityState, ControlState};
use crate::testkit::{has_pixel, premul};

const BOUNDS: Rect = Rect::new(0, 0, 80, 28);

fn field(value: i32) -> NumberField {
    let mut field = NumberField::new(value, 0, 255);
    field.set_focused(true);
    field
}

fn key(field: &mut NumberField, key: Key) -> Option<NumberAction> {
    field.on_key(key, Modifiers::default(), BOUNDS, &mut damage::sink())
}

fn named(field: &mut NumberField, named: NamedKey) -> Option<NumberAction> {
    key(field, Key::Named(named))
}

fn typed(field: &mut NumberField, text: &str) -> Vec<Option<NumberAction>> {
    text.chars().map(|c| key(field, Key::Char(c))).collect()
}

fn render(field: &NumberField, theme: &Theme) -> Surface {
    let mut surface = Surface::new(BOUNDS.width, BOUNDS.height).expect("surface");
    field.render(&mut surface, BOUNDS, Scale::ONE, theme);
    surface
}

#[test]
fn a_new_field_holds_its_value_within_bounds_given_either_way_round() {
    let field = NumberField::new(300, 255, 0);
    assert_eq!((field.value(), field.range()), (255, (0, 255)));
    let field = NumberField::new(-4, 0, 255);
    assert_eq!(field.value(), 0);
}

#[test]
fn digits_take_effect_as_they_are_typed() {
    let mut field = field(0);
    assert_eq!(
        named(&mut field, NamedKey::Backspace),
        None,
        "empty spells nothing"
    );
    assert_eq!(
        typed(&mut field, "128"),
        [
            Some(NumberAction::Edited { value: 1 }),
            Some(NumberAction::Edited { value: 12 }),
            Some(NumberAction::Edited { value: 128 }),
        ]
    );
    assert_eq!(field.value(), 128);
    assert_eq!(
        named(&mut field, NamedKey::Enter),
        Some(NumberAction::Settled { value: 128 })
    );
    assert_eq!(named(&mut field, NamedKey::Enter), None, "settled once");
}

#[test]
fn a_number_past_a_bound_moves_nothing_until_committed_to_the_bound() {
    let mut field = field(0);
    named(&mut field, NamedKey::Backspace);
    let edits = typed(&mut field, "300");
    assert_eq!(edits[2], None, "300 is past the bound");
    assert_eq!(field.value(), 30);
    assert_eq!(
        field.state().validation,
        crate::state::ValidationState::Invalid
    );
    assert_eq!(
        named(&mut field, NamedKey::Enter),
        Some(NumberAction::Settled { value: 255 })
    );
    assert_eq!(
        field.state().validation,
        crate::state::ValidationState::Valid
    );
}

#[test]
fn an_invalid_field_shows_the_danger_rim() {
    let theme = Theme::dark();
    let mut field = field(0);
    named(&mut field, NamedKey::Backspace);
    assert!(has_pixel(
        &render(&field, &theme),
        premul(theme.palette().danger)
    ));
    named(&mut field, NamedKey::Enter);
    assert!(!has_pixel(
        &render(&field, &theme),
        premul(theme.palette().danger)
    ));
}

#[test]
fn a_character_that_is_not_part_of_a_number_is_refused_whole() {
    let mut field = field(7);
    let mut damage = damage::sink();
    for character in ['x', ' ', '+', '.', '-'] {
        assert_eq!(
            field.on_key(
                Key::Char(character),
                Modifiers::default(),
                BOUNDS,
                &mut damage
            ),
            None
        );
    }
    assert!(damage.is_empty());
    assert_eq!(field.value(), 7);

    let mut signed = NumberField::new(0, -100, 100);
    signed.set_focused(true);
    named(&mut signed, NamedKey::Backspace);
    assert_eq!(
        typed(&mut signed, "-42"),
        [
            None,
            Some(NumberAction::Edited { value: -4 }),
            Some(NumberAction::Edited { value: -42 }),
        ]
    );
}

#[test]
fn steps_settle_and_stop_at_the_bounds() {
    let mut field = field(250).with_steps(1, 10);
    assert_eq!(
        named(&mut field, NamedKey::Up),
        Some(NumberAction::Settled { value: 251 })
    );
    assert_eq!(
        named(&mut field, NamedKey::PageUp),
        Some(NumberAction::Settled { value: 255 })
    );
    assert_eq!(
        named(&mut field, NamedKey::Up),
        None,
        "already at the bound"
    );
    assert_eq!(
        named(&mut field, NamedKey::PageDown),
        Some(NumberAction::Settled { value: 245 })
    );
    assert_eq!(
        named(&mut field, NamedKey::Down),
        Some(NumberAction::Settled { value: 244 })
    );
}

#[test]
fn a_step_from_typing_that_spells_nothing_steps_the_last_value() {
    let mut field = field(9);
    named(&mut field, NamedKey::Backspace);
    assert_eq!(
        named(&mut field, NamedKey::Up),
        Some(NumberAction::Settled { value: 10 })
    );
}

#[test]
fn escape_takes_back_what_was_typed_since_the_settle() {
    let mut field = field(40);
    named(&mut field, NamedKey::Backspace);
    typed(&mut field, "5");
    assert_eq!(field.value(), 45);
    assert_eq!(
        named(&mut field, NamedKey::Escape),
        Some(NumberAction::Settled { value: 40 })
    );
    assert_eq!(field.value(), 40);
    assert_eq!(
        named(&mut field, NamedKey::Escape),
        None,
        "nothing left to take back"
    );
}

#[test]
fn committing_text_that_spells_nothing_shows_the_value_again() {
    let mut field = field(64);
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    field.on_key(Key::Char('a'), ctrl, BOUNDS, &mut damage::sink());
    assert_eq!(
        named(&mut field, NamedKey::Backspace),
        None,
        "empty spells nothing"
    );
    let mut damage = damage::sink();
    assert_eq!(
        field.commit(BOUNDS, &mut damage),
        None,
        "the value never moved"
    );
    assert!(!damage.is_empty(), "the text is shown again");
    assert_eq!(field.value(), 64);
}

#[test]
fn a_field_without_the_keyboard_or_the_authority_takes_no_keys() {
    let mut unfocused = NumberField::new(1, 0, 9);
    assert_eq!(named(&mut unfocused, NamedKey::Up), None);
    for state in [
        ControlState::disabled(),
        ControlState::idle().with_authority(AuthorityState::Denied),
    ] {
        let mut field = field(1);
        field.set_state(state);
        field.set_focused(true);
        assert_eq!(named(&mut field, NamedKey::Up), None, "{state:?}");
        assert_eq!(typed(&mut field, "2"), [None]);
        assert_eq!(field.value(), 1);
    }
}

#[test]
fn the_wheel_steps_a_focused_field_a_line_a_detent() {
    let theme = Theme::dark();
    let detent = tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
    let scroll = |dy| InputEvent::PointerScrolled { dx: 0, dy };
    let mut field = field(10);
    let wheel = |field: &mut NumberField, dy| {
        field.on_pointer(&scroll(dy), BOUNDS, Scale::ONE, &theme, &mut damage::sink())
    };
    assert_eq!(
        wheel(&mut field, -detent),
        Some(NumberAction::Settled { value: 11 })
    );
    assert_eq!(
        wheel(&mut field, detent / 2),
        None,
        "half a detent is carried"
    );
    assert_eq!(
        wheel(&mut field, detent / 2),
        Some(NumberAction::Settled { value: 10 })
    );
    let mut unfocused = NumberField::new(10, 0, 255);
    assert_eq!(wheel(&mut unfocused, -detent), None);
}

#[test]
fn a_paste_is_taken_whole_or_not_at_all() {
    let mut field = field(0);
    named(&mut field, NamedKey::Backspace);
    assert_eq!(
        field.insert_text("12", BOUNDS, &mut damage::sink()),
        Some(NumberAction::Edited { value: 12 })
    );
    assert_eq!(field.insert_text("4a", BOUNDS, &mut damage::sink()), None);
    assert_eq!(
        field.insert_text("34", BOUNDS, &mut damage::sink()),
        None,
        "too long"
    );
    assert_eq!(field.value(), 12);
}

#[test]
fn an_owner_value_is_shown_settled_and_held_to_the_bounds() {
    let mut field = field(3);
    named(&mut field, NamedKey::Backspace);
    field.set_value(900);
    assert_eq!(field.value(), 255);
    assert_eq!(named(&mut field, NamedKey::Escape), None, "nothing pending");
    assert_eq!(named(&mut field, NamedKey::Enter), None);
}

#[test]
fn the_preferred_width_holds_the_longest_bound() {
    let theme = Theme::dark();
    let narrow = NumberField::new(0, 0, 9).preferred_width(Scale::ONE, &theme);
    let wide = NumberField::new(0, -1000, 9).preferred_width(Scale::ONE, &theme);
    assert!(wide > narrow);
}

/// The wheel's carried fraction is never drawn: fields that differ only in
/// it compare equal and draw the same pixels.
#[test]
fn the_wheel_carry_is_not_drawn() {
    let theme = Theme::dark();
    let detent = tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
    let mut carried = field(5);
    carried.on_pointer(
        &InputEvent::PointerScrolled {
            dx: 0,
            dy: detent / 3,
        },
        BOUNDS,
        Scale::ONE,
        &theme,
        &mut damage::sink(),
    );
    let resting = field(5);
    assert_eq!(carried, resting);
    assert_eq!(
        render(&carried, &theme).pixels(),
        render(&resting, &theme).pixels()
    );
}

/// A field given decimal places holds whole hundredths, spells them with a
/// point, and takes a point only where it may stand.
#[test]
fn decimals_are_spelled_typed_and_stepped_in_the_smallest_place() {
    let mut gamma = NumberField::new(100, 10, 999).with_decimals(2);
    gamma.set_focused(true);
    assert_eq!(gamma.text(), "1.00");
    for _ in 0..4 {
        named(&mut gamma, NamedKey::Backspace);
    }
    assert_eq!(
        typed(&mut gamma, "2.5"),
        [
            Some(NumberAction::Edited { value: 200 }),
            None,
            Some(NumberAction::Edited { value: 250 }),
        ]
    );
    assert_eq!(
        key(&mut gamma, Key::Char('7')),
        Some(NumberAction::Edited { value: 257 })
    );
    assert_eq!(
        key(&mut gamma, Key::Char('1')),
        None,
        "no room past the longest spelling"
    );
    assert_eq!(gamma.value(), 257);
    assert_eq!(
        named(&mut gamma, NamedKey::Backspace),
        Some(NumberAction::Edited { value: 250 })
    );
    assert_eq!(
        named(&mut gamma, NamedKey::Up),
        Some(NumberAction::Settled { value: 251 })
    );
    assert_eq!(gamma.text(), "2.51");
    gamma.set_value(10);
    assert_eq!(gamma.text(), "0.10");
    let mut whole = field(5);
    assert_eq!(
        key(&mut whole, Key::Char('.')),
        None,
        "no point in a whole number"
    );
    let signed = NumberField::new(-5, -999, 999).with_decimals(2);
    assert_eq!(signed.text(), "-0.05");
    let theme = Theme::dark();
    assert!(
        gamma.preferred_width(Scale::ONE, &theme)
            > NumberField::new(100, 10, 999).preferred_width(Scale::ONE, &theme),
        "room for the point"
    );
}

#[test]
fn a_fixed_point_spelling_reads_back_exactly() {
    use crate::number::{parse_fixed, spell};
    let mut spelt = [0; 16];
    for (value, places, text) in [
        (0, 2, "0.00"),
        (7, 3, "0.007"),
        (-1234, 2, "-12.34"),
        (i32::MIN, 0, "-2147483648"),
        (i32::MAX, 4, "214748.3647"),
    ] {
        assert_eq!(spell(value, places, &mut spelt), text);
        assert_eq!(parse_fixed(text, places), Some(i64::from(value)), "{text}");
    }
    assert_eq!(parse_fixed(".5", 2), Some(50));
    assert_eq!(parse_fixed("3.", 1), Some(30));
    for refused in ["", "-", ".", "1.2.3", "1e3", "1.234"] {
        assert_eq!(parse_fixed(refused, 2), None, "{refused}");
    }
    assert_eq!(parse_fixed("1.0", 0), None);
}
