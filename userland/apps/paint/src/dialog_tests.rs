use tairix_controls::Keystroke;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_image::IndexDepth;
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::ThemeRegistry;

use super::{Answer, Form};
use crate::transform::{Anchor, PaletteChoice};

const WINDOW: Rect = Rect::new(0, 0, 900, 640);

fn key(named: NamedKey) -> Keystroke {
    Keystroke {
        key: Key::Named(named),
        modifiers: Modifiers::default(),
        at_ns: 0,
    }
}

fn typed(form: &mut Form, text: &str) {
    let registry = ThemeRegistry::with_builtins();
    let mut damage = Region::new();
    for ch in text.chars() {
        let stroke = Keystroke {
            key: Key::Char(ch),
            modifiers: Modifiers::default(),
            at_ns: 0,
        };
        form.on_key(stroke, WINDOW, Scale::ONE, registry.active(), &mut damage);
    }
}

fn clear_field(form: &mut Form, len: usize) {
    let registry = ThemeRegistry::with_builtins();
    let mut damage = Region::new();
    for _ in 0..len {
        form.on_key(
            key(NamedKey::Backspace),
            WINDOW,
            Scale::ONE,
            registry.active(),
            &mut damage,
        );
    }
}

#[test]
fn enter_accepts_and_escape_turns_down() {
    let registry = ThemeRegistry::with_builtins();
    let mut damage = Region::new();
    let mut form = Form::quality(80);
    let theme = registry.active();
    assert_eq!(
        form.on_key(
            key(NamedKey::Escape),
            WINDOW,
            Scale::ONE,
            theme,
            &mut damage
        ),
        Some(Answer::Cancelled)
    );
    assert_eq!(
        form.on_key(key(NamedKey::Enter), WINDOW, Scale::ONE, theme, &mut damage),
        Some(Answer::Confirmed)
    );
    assert_eq!(form.quality_answer(), 80);
}

#[test]
fn a_new_picture_reads_its_size_and_refuses_one_out_of_bounds() {
    let mut form = Form::new_picture((640, 480));
    assert_eq!(
        form.new_picture_answer(),
        Ok(crate::document::NewPicture {
            size: (640, 480),
            depth: None,
            transparent: false,
        })
    );
    clear_field(&mut form, 3);
    typed(&mut form, "0");
    assert!(form.new_picture_answer().is_err());
    clear_field(&mut form, 1);
    typed(&mut form, "99999");
    assert!(form.new_picture_answer().is_err(), "past the longest side");
}

#[test]
fn a_new_sprite_needs_a_name_a_sprite_can_have() {
    let mut form = Form::new_sprite("sprite", (32, 32));
    let answer = form.new_sprite_answer().expect("the defaults are good");
    assert_eq!(answer.name.as_bytes(), b"sprite");
    assert_eq!(
        answer.depth,
        Some(IndexDepth::Four),
        "sixteen colours to start"
    );
    assert_eq!(answer.eig, (1, 1));
    clear_field(&mut form, 6);
    typed(&mut form, "has space");
    assert!(form.new_sprite_answer().is_err());
}

#[test]
fn a_scale_keeping_proportions_follows_the_other_side() {
    let mut form = Form::scale((200, 100), true);
    clear_field(&mut form, 3);
    typed(&mut form, "50");
    assert_eq!(form.scale_answer(), Ok(((50, 25), true)));
}

#[test]
fn a_canvas_starts_centred_and_a_conversion_to_the_depth_it_has() {
    assert_eq!(
        Form::canvas((10, 20)).canvas_answer(),
        Ok(((10, 20), Anchor::Centre))
    );
    let convert = Form::convert(Some(IndexDepth::Eight));
    assert_eq!(
        convert.convert_answer(),
        (Some(IndexDepth::Eight), PaletteChoice::Optimised, true)
    );
}

#[test]
fn clicking_the_confirm_button_accepts() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let mut form = Form::quality(50);
    let bounds = form.rect(WINDOW, Scale::ONE, theme);
    let mut damage = Region::new();
    // The confirming action is the rightmost one, near the bottom right.
    let aim = Point::new(bounds.right() - 30, bounds.bottom() - 16);
    form.on_pointer(
        &InputEvent::PointerMoved { to: aim },
        WINDOW,
        Scale::ONE,
        theme,
        &mut damage,
    );
    form.on_pointer(
        &InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        WINDOW,
        Scale::ONE,
        theme,
        &mut damage,
    );
    let answer = form.on_pointer(
        &InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        WINDOW,
        Scale::ONE,
        theme,
        &mut damage,
    );
    assert_eq!(answer, Some(Answer::Confirmed));
}
