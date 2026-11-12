//! Unit tests for the terminal's effect strengths: translucency, blur, and the
//! slider each effect is set by.

use super::{EffectKey, Effects, MAX_BLUR_RADIUS_PX, MIN_OPACITY};
use tairix_crt::FULL;

// --- Effects::default -------------------------------------------------------

#[test]
fn default_is_see_through_and_frosted_with_no_passes() {
    let effects = Effects::default();
    assert!(
        effects.background_alpha() < 255,
        "the default is see-through"
    );
    assert_eq!(
        effects.blur_radius_px(),
        MAX_BLUR_RADIUS_PX / 2,
        "the window reads as frosted glass, at half the strength the slider \
         reaches"
    );
    let (_, len) = effects.tube.passes(100);
    assert_eq!(len, 0);
}

// --- Effects::background_alpha ----------------------------------------------

#[test]
fn background_alpha_full_opacity_is_fully_opaque() {
    let effects = Effects {
        opacity: FULL,
        ..Effects::default()
    };
    assert_eq!(effects.background_alpha(), 255);
}

#[test]
fn background_alpha_at_the_minimum_opacity_is_proportional() {
    let effects = Effects {
        opacity: MIN_OPACITY,
        ..Effects::default()
    };
    // 300/1000 of 255, rounded to nearest, is 77.
    assert_eq!(effects.background_alpha(), 77);
}

#[test]
fn background_alpha_never_drops_below_the_minimum_opacitys_alpha() {
    let effects = Effects {
        opacity: MIN_OPACITY / 2,
        ..Effects::default()
    };
    assert_eq!(effects.background_alpha(), 77);
}

#[test]
fn background_alpha_never_exceeds_full_opacity() {
    let effects = Effects {
        opacity: FULL + 500,
        ..Effects::default()
    };
    assert_eq!(effects.background_alpha(), 255);
}

// --- Effects::blur_radius_px -------------------------------------------------

#[test]
fn blur_radius_is_zero_when_fully_opaque_however_high_the_slider() {
    let effects = Effects {
        opacity: FULL,
        blur: FULL,
        ..Effects::default()
    };
    assert_eq!(effects.blur_radius_px(), 0);
}

#[test]
fn blur_radius_is_proportional_when_translucent() {
    let effects = Effects {
        opacity: 500,
        blur: 500,
        ..Effects::default()
    };
    assert_eq!(effects.blur_radius_px(), 12);
}

#[test]
fn blur_radius_never_exceeds_the_maximum() {
    let effects = Effects {
        opacity: 1,
        blur: FULL,
        ..Effects::default()
    };
    assert!(effects.blur_radius_px() <= MAX_BLUR_RADIUS_PX);
    assert_eq!(effects.blur_radius_px(), MAX_BLUR_RADIUS_PX);
}

// --- EffectKey ----------------------------------------------------------------

#[test]
fn every_effect_key_reads_and_writes_only_its_own_field() {
    // The settings sheet builds a slider per key and reads the row back
    // through the same list, so a key aliasing another's field would move the
    // wrong effect.
    const MARK: u16 = 111;
    for key in EffectKey::ALL {
        let mut effects = Effects::default();
        key.set(&mut effects, MARK);
        assert_eq!(key.of(effects), MARK, "{} does not read back", key.label());
        for other in EffectKey::ALL {
            if other == key {
                continue;
            }
            assert_eq!(
                other.of(effects),
                other.of(Effects::default()),
                "{} moved when {} was set",
                other.label(),
                key.label()
            );
        }
    }
}

#[test]
fn effect_keys_and_their_labels_are_all_distinct() {
    assert_eq!(EffectKey::ALL.len(), EffectKey::COUNT);
    for (index, key) in EffectKey::ALL.iter().enumerate() {
        for other in EffectKey::ALL.iter().skip(index + 1) {
            assert_ne!(key, other, "duplicated key");
            assert_ne!(key.label(), other.label(), "duplicated label");
        }
    }
}

#[test]
fn only_opacity_has_a_raised_floor() {
    for key in EffectKey::ALL {
        let (min, max) = key.bounds();
        assert_eq!(max, FULL, "{} spans to full strength", key.label());
        let floor = if key == EffectKey::Opacity {
            MIN_OPACITY
        } else {
            0
        };
        assert_eq!(min, floor, "{} floor", key.label());
    }
}
