use tairix_colour::{Hsl, Rgb};

use super::{
    clipped_span, Channel, ChannelLevels, ColourBalance, Curve, Curves, HueRange, HueRanges,
    Levels, Tones, WhiteBalance, IDENTITY_TABLE, MOST_POINTS,
};

fn through(tables: &[[u8; 256]; 3], colour: Rgb) -> Rgb {
    Rgb::new(
        tables[0][usize::from(colour.r)],
        tables[1][usize::from(colour.g)],
        tables[2][usize::from(colour.b)],
    )
}

fn spread(colour: Rgb) -> u8 {
    let [r, g, b] = colour.to_array();
    r.max(g).max(b) - r.min(g).min(b)
}

#[test]
fn every_neutral_setting_maps_every_level_to_itself() {
    assert_eq!(Levels::IDENTITY.tables(), [IDENTITY_TABLE; 3]);
    assert_eq!(Curves::IDENTITY.tables(), [IDENTITY_TABLE; 3]);
    assert_eq!(WhiteBalance::NEUTRAL.tables(), [IDENTITY_TABLE; 3]);
    let shifts = ColourBalance::NEUTRAL.shifts();
    let hues = HueRanges::default();
    for colour in [
        Rgb::new(0, 0, 0),
        Rgb::new(255, 255, 255),
        Rgb::new(200, 40, 90),
        Rgb::new(13, 250, 128),
        Rgb::new(128, 128, 128),
    ] {
        assert_eq!(ColourBalance::NEUTRAL.map(colour, &shifts), colour);
        assert_eq!(hues.map(colour), colour);
    }
}

#[test]
fn levels_stretch_their_inputs_onto_their_outputs() {
    let mut levels = ChannelLevels::IDENTITY;
    levels.black = 50;
    levels.white = 200;
    assert!((levels.map(50.0) - 0.0).abs() < 1e-9);
    assert!((levels.map(200.0) - 255.0).abs() < 1e-9);
    assert!((levels.map(125.0) - 127.5).abs() < 1e-9);
    assert!((levels.map(10.0)).abs() < 1e-9, "below black is black");
    levels.out_black = 255;
    levels.out_white = 0;
    assert!((levels.map(200.0)).abs() < 1e-9, "an output turned over");
}

#[test]
fn the_grey_point_stands_where_the_levels_reach_half() {
    let mut levels = ChannelLevels::IDENTITY;
    assert!((levels.grey() - 127.5).abs() < 1e-9);
    levels.set_grey(64.0);
    assert!(
        levels.gamma > 100,
        "a grey point moved down lifts the levels"
    );
    assert!((levels.grey() - 64.0).abs() < 0.5, "{}", levels.grey());
    assert!((levels.map(levels.grey()) - 127.5).abs() < 0.5);
    levels.set_grey(-40.0);
    assert_eq!(
        levels.gamma,
        ChannelLevels::GAMMA.1,
        "held to the gamma's reach"
    );
    levels.set_black(255);
    assert!(levels.black < levels.white);
    levels.set_white(0);
    assert!(levels.white > levels.black);
}

#[test]
fn a_channel_maps_before_the_composite() {
    let mut levels = Levels::IDENTITY;
    levels.of_mut(Channel::Red).black = 100;
    levels.of_mut(Channel::Composite).out_white = 128;
    let [red, green, _] = levels.tables();
    assert_eq!(red[100], 0);
    assert_eq!(red[255], 128);
    assert_eq!(green[255], 128);
    assert_eq!(green[0], 0);
}

#[test]
fn the_pickers_take_a_colour_as_black_white_or_grey() {
    let mut levels = Levels::IDENTITY;
    levels.black_point(Rgb::new(30, 20, 10));
    levels.white_point(Rgb::new(230, 240, 250));
    let tables = levels.tables();
    assert_eq!(through(&tables, Rgb::new(30, 20, 10)), Rgb::new(0, 0, 0));
    assert_eq!(
        through(&tables, Rgb::new(230, 240, 250)),
        Rgb::new(255, 255, 255)
    );
    let mut greyed = Levels::IDENTITY;
    let cast = Rgb::new(150, 120, 90);
    greyed.grey_point(cast);
    assert!(spread(through(&greyed.tables(), cast)) <= 2);
}

#[test]
fn auto_levels_set_aside_a_sliver_at_each_end() {
    let mut counts = [0u64; 256];
    for count in &mut counts[10..=200] {
        *count = 1000;
    }
    counts[255] = 10;
    assert_eq!(clipped_span(&counts, 0.001), Some((10, 200)));
    assert_eq!(clipped_span(&counts, 0.0), Some((10, 255)));
    assert_eq!(clipped_span(&[0; 256], 0.001), None);
    let mut levels = Levels::IDENTITY;
    levels.auto(&[counts, counts, [0; 256]], 0.001);
    assert_eq!(
        (levels.of(Channel::Red).black, levels.of(Channel::Red).white),
        (10, 200)
    );
    assert_eq!(
        *levels.of(Channel::Blue),
        ChannelLevels::IDENTITY,
        "an empty channel is left"
    );
}

#[test]
fn a_curve_keeps_its_points_in_order_and_two_at_least() {
    let mut curve = Curve::IDENTITY;
    assert_eq!(curve.add((64, 100)), Some(1));
    assert_eq!(curve.add((64, 10)), None, "an input already taken");
    assert_eq!(curve.add((32, 50)), Some(1));
    assert_eq!(curve.points(), [(0, 0), (32, 50), (64, 100), (255, 255)]);
    assert_eq!(
        curve.set(1, (90, 70)),
        Some((63, 70)),
        "held short of the next"
    );
    assert_eq!(
        curve.set(1, (0, 70)),
        Some((1, 70)),
        "held past the one before"
    );
    assert!(curve.remove(1));
    assert!(curve.remove(1));
    assert!(!curve.remove(1), "two are kept");
    assert_eq!(curve, Curve::IDENTITY);
    for input in 1..=u8::try_from(MOST_POINTS).expect("small") {
        let _ = curve.add((input * 10, input * 10));
    }
    assert_eq!(curve.points().len(), MOST_POINTS);
    assert_eq!(curve.add((250, 3)), None, "full");
}

#[test]
fn a_curve_runs_through_its_points_and_never_overshoots_them() {
    let mut lifted = Curve::IDENTITY;
    lifted.add((64, 128));
    let table = lifted.table();
    assert_eq!(table[64], 128);
    assert!(table.windows(2).all(|pair| pair[0] <= pair[1]), "monotone");
    let mut peak = Curve::IDENTITY;
    peak.set(1, (255, 0));
    peak.add((128, 255));
    let table = peak.table();
    assert_eq!(table[128], 255);
    assert!(table[..=128].windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(table[128..].windows(2).all(|pair| pair[0] >= pair[1]));
    let mut flat = Curve::IDENTITY;
    flat.add((100, 120));
    flat.add((150, 120));
    let table = flat.table();
    assert!(
        table[100..=150].iter().all(|&level| level == 120),
        "flat between equal points"
    );
}

#[test]
fn a_channel_curve_runs_before_the_composite() {
    let mut curves = Curves::IDENTITY;
    curves.of_mut(Channel::Green).set(1, (255, 128));
    curves.of_mut(Channel::Composite).set(0, (0, 64));
    let [red, green, _] = curves.tables();
    assert_eq!(red[0], 64);
    assert_eq!(green[255], curves.of(Channel::Composite).table()[128]);
    assert!(!curves.is_identity());
}

#[test]
fn white_balance_warms_cools_and_tints_keeping_a_greys_luminance() {
    for setting in [
        WhiteBalance {
            kelvin: 3000,
            tint: 0,
        },
        WhiteBalance {
            kelvin: 9000,
            tint: 40,
        },
        WhiteBalance {
            kelvin: 6500,
            tint: -100,
        },
    ] {
        let gains = setting.gains();
        let luminance = 0.212_672_9 * gains[0] + 0.715_152_2 * gains[1] + 0.072_175_0 * gains[2];
        assert!((luminance - 1.0).abs() < 1e-9, "{setting:?}");
    }
    let warm = WhiteBalance {
        kelvin: 9000,
        tint: 0,
    }
    .cast();
    assert!(warm.r > warm.b, "a bluer light is warmed: {warm:?}");
    let cool = WhiteBalance {
        kelvin: 3000,
        tint: 0,
    }
    .cast();
    assert!(cool.b > cool.r, "a redder light is cooled: {cool:?}");
    let magenta = WhiteBalance {
        kelvin: 6500,
        tint: 100,
    }
    .cast();
    assert!(
        magenta.g < magenta.r && magenta.g < magenta.b,
        "{magenta:?}"
    );
}

#[test]
fn the_neutral_picker_balances_the_cast_it_reads() {
    let light = WhiteBalance {
        kelvin: 3400,
        tint: 25,
    };
    // A grey lit by that light: its white, scaled to a mid tone.
    let lit = light.light().map(|channel| channel * 0.4);
    let read = WhiteBalance::neutralising_linear(lit).expect("a colour");
    assert!(read.kelvin.abs_diff(light.kelvin) <= 30, "{read:?}");
    assert!(read.tint.abs_diff(light.tint) <= 2, "{read:?}");
    let encoded = tairix_colour::encode_linear(lit).rgb;
    assert!(spread(through(&read.tables(), encoded)) <= 3);
    assert_eq!(WhiteBalance::neutralising(Rgb::new(0, 0, 0)), None);
}

#[test]
fn colour_balance_moves_each_band_and_one_move_everywhere_is_even() {
    let mut balance = ColourBalance::NEUTRAL;
    balance.keep_luminosity = false;
    balance.tones[Tones::Shadows.index()][0] = 100;
    let shifts = balance.shifts();
    assert!(shifts[0][20] > 100, "a dark colour reddened");
    assert_eq!(shifts[0][240], 0, "a light one not");
    let mut even = ColourBalance::NEUTRAL;
    for band in &mut even.tones {
        band[2] = 50;
    }
    let shifts = even.shifts();
    for level in [0, 64, 128, 192, 255] {
        assert!(
            shifts[2][level].abs_diff(shifts[2][128]) <= 1,
            "level {level}: {}",
            shifts[2][level]
        );
    }
    let mut kept = ColourBalance::NEUTRAL;
    kept.tones[Tones::Midtones.index()] = [60, -30, 10];
    let colour = Rgb::new(120, 130, 110);
    let moved = kept.map(colour, &kept.shifts());
    assert_ne!(moved, colour);
    let lightness = |rgb| Hsl::from_rgb(rgb, Hsl::default()).lightness.byte();
    assert!(lightness(moved).abs_diff(lightness(colour)) <= 1);
}

#[test]
fn a_hue_range_fades_out_a_range_away_and_leaves_greys() {
    let mut hues = HueRanges::default();
    hues.of_mut(HueRange::Reds).hue = 120;
    let green = hues.map(Rgb::new(255, 0, 0));
    assert!(green.g > 250 && green.r < 5, "{green:?}");
    assert_eq!(
        hues.map(Rgb::new(0, 0, 255)),
        Rgb::new(0, 0, 255),
        "blue is a range away"
    );
    let (half, _, _) = hues.shift_at(30.0, 1.0);
    assert!(
        (half - 60.0).abs() < 1e-9,
        "orange takes half of the reds' turn"
    );
    let mut lighter = HueRanges::default();
    lighter.of_mut(HueRange::Reds).lightness = 100;
    assert_eq!(lighter.map(Rgb::new(90, 90, 90)), Rgb::new(90, 90, 90));
    lighter.of_mut(HueRange::Master).lightness = 100;
    assert_eq!(lighter.map(Rgb::new(90, 90, 90)), Rgb::new(255, 255, 255));
}
