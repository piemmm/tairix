//! The colour models: each turns a colour into its fields' values and back,
//! keeps the hue a grey has none of, and says when a value lies past sRGB.

use tairix_colour::Rgb;

use crate::colour_model::{ColourModel, PickerView, MOST_CHANNELS};

const COLOURS: [Rgb; 8] = [
    Rgb::new(0, 0, 0),
    Rgb::new(255, 255, 255),
    Rgb::new(51, 102, 153),
    Rgb::new(255, 0, 0),
    Rgb::new(12, 200, 90),
    Rgb::new(128, 128, 128),
    Rgb::new(250, 240, 10),
    Rgb::new(90, 20, 160),
];

fn apart(a: Rgb, b: Rgb) -> u8 {
    a.r.abs_diff(b.r)
        .max(a.g.abs_diff(b.g))
        .max(a.b.abs_diff(b.b))
}

#[test]
fn every_model_turns_a_colour_into_its_values_and_back() {
    for model in ColourModel::ALL {
        let reach = match model {
            ColourModel::Rgb => 0,
            ColourModel::Grey => 255,
            _ => 4,
        };
        for colour in COLOURS {
            let values = model.values(colour, [0; MOST_CHANNELS]);
            for (value, channel) in values.iter().zip(model.channels()) {
                assert!(
                    (channel.least..=channel.most).contains(value),
                    "{model:?} {colour:?}: {value} in {channel:?}"
                );
            }
            let (back, clipped) = model.colour(values);
            // Whole Lab values about a colour on sRGB's edge may lie just past
            // it; only those models can say so.
            assert!(
                !clipped || model.reaches_past_srgb(),
                "{model:?}: {colour:?}"
            );
            assert!(
                apart(back, colour) <= reach,
                "{model:?}: {colour:?} came back {back:?}"
            );
        }
    }
    let grey = Rgb::new(77, 77, 77);
    let values = ColourModel::Grey.values(grey, [0; MOST_CHANNELS]);
    assert!(apart(ColourModel::Grey.colour(values).0, grey) <= 2);
}

#[test]
fn a_grey_keeps_the_hue_it_was_shown_at() {
    let near = [210, 50, 50, 0];
    assert_eq!(ColourModel::Hsv.values(Rgb::new(90, 90, 90), near)[0], 210);
    assert_eq!(ColourModel::Hsl.values(Rgb::new(90, 90, 90), near)[0], 210);
    let lch_near = [400, 300, 3000, 0];
    assert_eq!(
        ColourModel::Lch.values(Rgb::new(90, 90, 90), lch_near)[2],
        3000
    );
}

#[test]
fn a_lab_or_lch_value_past_srgb_is_clipped_and_says_so() {
    let slate = ColourModel::Lab.values(Rgb::new(51, 102, 153), [0; MOST_CHANNELS]);
    assert!(
        !ColourModel::Lab.colour(slate).1,
        "a colour well inside sRGB"
    );
    let (_, clipped) = ColourModel::Lab.colour([500, 1270, -1280, 0]);
    assert!(clipped);
    let (_, clipped) = ColourModel::Lch.colour([900, 1500, 2600, 0]);
    assert!(clipped);
    assert!(ColourModel::Lab.reaches_past_srgb() && !ColourModel::Cmyk.reaches_past_srgb());
}

#[test]
fn the_models_and_views_name_themselves() {
    assert_eq!(ColourModel::Cmyk.channels().len(), 4);
    assert_eq!(ColourModel::Grey.channels().len(), 1);
    assert!(ColourModel::ALL
        .iter()
        .all(|model| !model.label().is_empty()));
    assert_eq!(
        PickerView::ALL.map(PickerView::label),
        ["Square", "Wheel", "Sliders"]
    );
}
