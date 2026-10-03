use alloc::vec;
use alloc::vec::Vec;

use tairix_controls::Keystroke;
use tairix_controls::{FieldAction, FieldGroupAction};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_image::{IndexDepth, TiffCompression};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::ThemeRegistry;

use super::{Answer, Form, Purpose, SaveChoices};
use crate::save::{Loss, SaveFormat, SaveSettings};
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

/// Every format, each with no loss to state.
fn every_format() -> SaveChoices {
    SaveChoices {
        formats: SaveFormat::ALL
            .into_iter()
            .map(|format| (format, Vec::new()))
            .collect(),
    }
}

fn choose(row: usize, index: usize) -> FieldGroupAction {
    FieldGroupAction {
        row,
        action: FieldAction::Selected { index },
    }
}

fn format_index(format: SaveFormat) -> usize {
    SaveFormat::ALL
        .iter()
        .position(|&offered| offered == format)
        .expect("a format")
}

#[test]
fn enter_accepts_and_escape_turns_down() {
    let registry = ThemeRegistry::with_builtins();
    let mut damage = Region::new();
    let settings = SaveSettings {
        jpeg_quality: 80,
        ..SaveSettings::default()
    };
    let mut form = Form::save_as(every_format(), SaveFormat::Jpeg, settings, false);
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
    assert_eq!(form.save_as_answer(), (SaveFormat::Jpeg, settings));
}

#[test]
fn a_new_picture_reads_its_size_and_refuses_one_out_of_bounds() {
    let mut form = Form::new_picture((640, 480));
    assert_eq!(
        form.new_picture_answer(),
        Ok((
            crate::document::NewPicture {
                size: (640, 480),
                depth: None,
                transparent: false,
            },
            SaveFormat::Png
        ))
    );
    // Past the format, to the width.
    let registry = ThemeRegistry::with_builtins();
    let mut damage = Region::new();
    form.on_key(
        key(NamedKey::Tab),
        WINDOW,
        Scale::ONE,
        registry.active(),
        &mut damage,
    );
    clear_field(&mut form, 3);
    typed(&mut form, "0");
    assert!(form.new_picture_answer().is_err());
    clear_field(&mut form, 1);
    typed(&mut form, "99999");
    assert!(form.new_picture_answer().is_err(), "past the longest side");
}

#[test]
fn a_new_picture_offers_the_colours_and_background_its_format_holds() {
    let mut form = Form::new_picture((64, 48));
    form.follow_new_picture(&choose(0, format_index(SaveFormat::Gif)));
    let (picture, format) = form.new_picture_answer().expect("good");
    assert_eq!(format, SaveFormat::Gif);
    assert_eq!(
        picture.depth,
        Some(IndexDepth::Eight),
        "a GIF has a palette"
    );
    assert_eq!(picture.size, (64, 48), "the size typed is kept");
    assert_eq!(form.group.rows().len(), 5, "a GIF may be clear");
    form.follow_new_picture(&choose(0, format_index(SaveFormat::Jpeg)));
    let (picture, format) = form.new_picture_answer().expect("good");
    assert_eq!(
        (picture.depth, picture.transparent, format),
        (None, false, SaveFormat::Jpeg)
    );
    assert_eq!(form.group.rows().len(), 4, "a JPEG is never clear");
    form.follow_new_picture(&choose(0, format_index(SaveFormat::Tiff)));
    assert_eq!(form.new_picture_answer().expect("good").1, SaveFormat::Tiff);
}

#[test]
fn a_new_page_reads_its_size_and_colours() {
    let form = Form::new_page((10, 20));
    assert_eq!(
        form.new_page_answer(),
        Ok(crate::document::NewPicture {
            size: (10, 20),
            depth: None,
            transparent: false,
        })
    );
}

#[test]
fn the_save_as_sheet_holds_its_formats_settings_alone_and_keeps_the_rest() {
    let mut form = Form::save_as(
        every_format(),
        SaveFormat::Png,
        SaveSettings::default(),
        true,
    );
    assert_eq!(form.purpose(), Purpose::SaveAs { then_close: true });
    assert_eq!(form.group.rows().len(), 1, "a PNG has no settings");
    form.follow_save_as(&choose(0, format_index(SaveFormat::Gif)));
    assert_eq!(form.group.rows().len(), 2, "interlacing");
    form.follow_save_as(&FieldGroupAction {
        row: 1,
        action: FieldAction::Set { on: true },
    });
    form.follow_save_as(&choose(0, format_index(SaveFormat::Tiff)));
    form.follow_save_as(&choose(1, 2));
    form.follow_save_as(&choose(0, format_index(SaveFormat::Jpeg)));
    form.follow_save_as(&FieldGroupAction {
        row: 1,
        action: FieldAction::Settled { permille: 0 },
    });
    let (format, settings) = form.save_as_answer();
    assert_eq!(format, SaveFormat::Jpeg);
    assert_eq!(settings.jpeg_quality, 1);
    assert!(settings.gif.interlaced, "a format left keeps its settings");
    assert_eq!(settings.tiff.compression, TiffCompression::Deflate);
}

#[test]
fn the_save_as_sheet_says_what_its_format_will_not_keep() {
    let choices = SaveChoices {
        formats: vec![
            (SaveFormat::Png, Vec::new()),
            (SaveFormat::Jpeg, vec![Loss::Transparency, Loss::PixelShape]),
        ],
    };
    let mut form = Form::save_as(choices, SaveFormat::Png, SaveSettings::default(), false);
    assert_eq!(form.dialog.message(), None);
    form.follow_save_as(&choose(0, 1));
    let message = form.dialog.message().expect("what is lost");
    assert!(message.contains(Loss::Transparency.message()));
    assert!(message.contains(Loss::PixelShape.message()));
    let offered = Form::save_as(
        SaveChoices {
            formats: vec![(SaveFormat::Tiff, Vec::new())],
        },
        SaveFormat::Png,
        SaveSettings::default(),
        false,
    );
    assert_eq!(
        offered.save_as_answer().0,
        SaveFormat::Tiff,
        "a format not offered is not chosen"
    );
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
    let mut form = Form::canvas((10, 10));
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

#[test]
fn a_filter_form_holds_a_slider_a_number_and_follows_them() {
    use crate::filter::Filter;
    let start = Filter::Sharpen {
        amount: 100,
        radius: 2,
    };
    let mut form = Form::filter(start);
    assert_eq!(form.purpose(), Purpose::Filter);
    assert_eq!(form.group.rows().len(), 2, "amount and radius");
    assert_eq!(form.filter_answer(), Some(start));
    form.follow_filter(&FieldGroupAction {
        row: 1,
        action: FieldAction::Settled { permille: 1000 },
    });
    assert_eq!(
        form.filter_answer(),
        Some(Filter::Sharpen {
            amount: 100,
            radius: 32
        }),
        "the far end of the slider is the most it holds"
    );
    assert!(
        Form::filter(Filter::Edges).group.rows().is_empty(),
        "nothing to set"
    );
}

#[test]
fn a_layer_form_keeps_the_exact_opacity_until_its_slider_moves() {
    use crate::document::Shown;
    let shown = Shown {
        name: alloc::string::String::from("Haze"),
        opacity: 127,
        visible: true,
    };
    let mut form = Form::layer(&shown);
    assert_eq!(form.purpose(), Purpose::Layer);
    assert_eq!(
        form.layer_answer(),
        Ok(shown.clone()),
        "nothing moved, nothing changed"
    );
    form.follow_opacity(&FieldGroupAction {
        row: 1,
        action: FieldAction::Settled { permille: 1000 },
    });
    let answered = form.layer_answer().expect("a name");
    assert_eq!((answered.opacity, answered.visible), (255, true));
    clear_field(&mut form, 4);
    assert!(form.layer_answer().is_err(), "a layer has a name");
    typed(&mut form, "Mist");
    assert_eq!(
        form.layer_answer().map(|shown| shown.name),
        Ok(alloc::string::String::from("Mist"))
    );
}
