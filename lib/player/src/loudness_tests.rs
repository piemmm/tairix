use alloc::string::ToString;

use tairix_sound::{Metadata, Tag, TagKey, TagKind};

use super::{parse_gain, parse_peak, track_millibel};

fn tagged(pairs: &[(&str, &str)]) -> Metadata {
    let mut metadata = Metadata::default();
    for (key, value) in pairs {
        metadata.tags.push(Tag {
            kind: TagKind::Other(TagKey::uppercase(key.as_bytes()).expect("a key")),
            value: (*value).to_string(),
        });
    }
    metadata
}

#[test]
fn a_gain_is_read_in_hundredths_of_a_decibel_rounded_half_away_from_zero() {
    for (text, millibel) in [
        ("-6.10 dB", -610),
        ("+3.5 dB", 350),
        ("2 dB", 200),
        ("-0.005 dB", -1),
        ("-0.004", 0),
        ("  -12.345db ", -1_235),
        (".5 DB", 50),
        ("-60.00 dB", -6_000),
    ] {
        assert_eq!(parse_gain(text), Some(millibel), "{text}");
    }
    for bad in [
        "",
        "dB",
        "-",
        "- 6 dB",
        "6,1 dB",
        "1e3 dB",
        "-60.01 dB",
        "1000 dB",
        "x",
    ] {
        assert_eq!(parse_gain(bad), None, "{bad:?}");
    }
}

#[test]
fn a_peak_is_read_as_a_fraction_of_full_scale() {
    assert_eq!(
        parse_peak("1.000000").map(f32::to_bits),
        Some(1.0f32.to_bits())
    );
    assert_eq!(parse_peak("0.5").map(f32::to_bits), Some(0.5f32.to_bits()));
    assert_eq!(parse_peak(" 2 ").map(f32::to_bits), Some(2.0f32.to_bits()));
    for bad in ["", ".5", "0", "0.000", "-0.5", "0.5.1", "1e0", "1234"] {
        assert_eq!(parse_peak(bad), None, "{bad:?}");
    }
}

#[test]
fn a_cut_is_taken_whole_and_a_boost_only_as_far_as_the_peak_allows() {
    let cut = tagged(&[
        ("replaygain_track_gain", "-7.25 dB"),
        ("replaygain_track_peak", "1.0"),
    ]);
    assert_eq!(track_millibel(&cut), Some(-725));
    let room = tagged(&[
        ("REPLAYGAIN_TRACK_GAIN", "+3.00 dB"),
        ("REPLAYGAIN_TRACK_PEAK", "0.5"),
    ]);
    assert_eq!(track_millibel(&room), Some(300), "six decibels of headroom");
    let tight = tagged(&[
        ("REPLAYGAIN_TRACK_GAIN", "+9.00 dB"),
        ("REPLAYGAIN_TRACK_PEAK", "0.5"),
    ]);
    assert_eq!(track_millibel(&tight), Some(602), "held at the peak");
    let unstated = tagged(&[("REPLAYGAIN_TRACK_GAIN", "+4 dB")]);
    assert_eq!(track_millibel(&unstated), Some(0), "never raised blind");
    let damaged = tagged(&[
        ("REPLAYGAIN_TRACK_GAIN", "-3 dB"),
        ("REPLAYGAIN_TRACK_PEAK", "nonsense"),
    ]);
    assert_eq!(track_millibel(&damaged), Some(-300));
}

#[test]
fn a_track_that_states_no_readable_gain_plays_at_unity() {
    assert_eq!(track_millibel(&Metadata::default()), None);
    let album_only = tagged(&[("REPLAYGAIN_ALBUM_GAIN", "-5 dB")]);
    assert_eq!(track_millibel(&album_only), None);
    let unreadable = tagged(&[("REPLAYGAIN_TRACK_GAIN", "loud")]);
    assert_eq!(track_millibel(&unreadable), None);
}
