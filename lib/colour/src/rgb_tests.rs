use crate::{Rgb, Rgba};

const BLACK: Rgba = Rgba::rgb(0, 0, 0);
const WHITE: Rgba = Rgba::rgb(255, 255, 255);

#[test]
fn rgba_constructors_and_accessors() {
    assert_eq!(Rgba::rgb(1, 2, 3), Rgba::new(1, 2, 3, 255));
    assert!(Rgba::rgb(1, 2, 3).is_opaque());
    assert!(!Rgba::TRANSPARENT.is_opaque());
    assert_eq!(Rgba::rgb(1, 2, 3).with_alpha(0).a, 0);
    assert_eq!(Rgba::new(9, 8, 7, 6).to_array(), [9, 8, 7, 6]);
}

#[test]
fn mix_endpoints_are_exact() {
    assert_eq!(BLACK.mix(WHITE, 0), BLACK);
    assert_eq!(BLACK.mix(WHITE, 1000), WHITE);
}

#[test]
fn over_resolves_a_translucent_role_against_its_ground() {
    // A wash authored at half opacity lands halfway to the ground and comes
    // back opaque, so laying it down tints the surface instead of cutting a
    // hole in it.
    let ground = Rgba::rgb(0, 0, 0);
    let resolved = WHITE.with_alpha(128).over(ground);
    assert_eq!(resolved.a, ground.a, "the ground keeps its own opacity");
    assert_eq!(resolved, Rgba::rgb(128, 128, 128));
    assert_eq!(WHITE.with_alpha(0).over(ground), ground);
    assert_eq!(WHITE.over(ground), WHITE);
}

#[test]
fn mix_interpolates_each_channel_independently() {
    let from = Rgba::new(0, 100, 200, 40);
    let to = Rgba::new(200, 100, 0, 240);
    assert_eq!(from.mix(to, 500), Rgba::new(100, 100, 100, 140));
}

#[test]
fn mix_saturates_an_out_of_range_weight() {
    assert_eq!(BLACK.mix(WHITE, u16::MAX), WHITE);
}

#[test]
fn mix_rounds_to_nearest() {
    let to = Rgba::rgb(1, 1, 1);
    assert_eq!(BLACK.mix(to, 499), BLACK);
    assert_eq!(BLACK.mix(to, 500), to);
}

#[test]
fn an_opaque_colour_and_its_channels_convert_both_ways() {
    let rgb = Rgb::new(12, 34, 56);
    assert_eq!(rgb.opaque(), Rgba::new(12, 34, 56, 255));
    assert_eq!(Rgba::from(rgb), rgb.opaque());
    assert_eq!(rgb.with_alpha(7), Rgba::new(12, 34, 56, 7));
    assert_eq!(rgb.with_alpha(7).without_alpha(), rgb);
    assert_eq!(Rgba::from_array([1, 2, 3, 4]).to_array(), [1, 2, 3, 4]);
    assert_eq!(rgb.to_array(), [12, 34, 56]);
    assert!(rgb.opaque().is_opaque());
    assert!(!rgb.with_alpha(254).is_opaque());
}
