use super::*;
use alloc::vec;

/// A `width`×`height` picture whose pixel `(x, y)` is `paint(x, y)`.
fn picture(width: u32, height: u32, paint: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    for y in 0..height {
        for x in 0..width {
            out.extend_from_slice(&paint(x, y));
        }
    }
    out
}

fn scaled(source: (u32, u32, &[u8]), dest: (u32, u32)) -> Surface {
    let frame = Rgba8Image::new(source.0, source.1, source.2).unwrap();
    let mut surface = Surface::new(dest.0, dest.1).unwrap();
    let target = Region {
        x: 0,
        y: 0,
        width: dest.0,
        height: dest.1,
    };
    SharpScaler::new()
        .draw(&frame, &mut surface, target, 0..dest.1)
        .unwrap();
    surface
}

fn grey(level: u8) -> [u8; 4] {
    [level, level, level, 255]
}

#[test]
fn a_whole_scale_is_a_block_of_each_source_pixel() {
    let level = |x: u32, y: u32| u8::try_from(x * 70 + y * 20).unwrap();
    let source = picture(3, 2, |x, y| grey(level(x, y)));
    let out = scaled((3, 2, &source), (9, 8));
    for y in 0..8 {
        for x in 0..9 {
            let [r, ..] = grey(level(x / 3, y / 4));
            assert_eq!(out.get(x, y).unwrap().r, r, "({x}, {y})");
        }
    }
}

#[test]
fn the_same_size_is_the_picture() {
    let byte = |n: u32| u8::try_from(n).unwrap();
    let source = picture(5, 4, |x, y| [byte(x * 50), byte(y * 60), 7, 255]);
    let out = scaled((5, 4, &source), (5, 4));
    for y in 0..4 {
        for x in 0..5 {
            let [r, g, b, a] = source[((y * 5 + x) * 4) as usize..][..4] else {
                unreachable!()
            };
            assert_eq!(out.get(x, y), Some(Pixel { r, g, b, a }));
        }
    }
}

#[test]
fn a_fractional_scale_blends_only_at_a_blocks_edge() {
    // Two pixels across seven: blocks of three, the middle pixel shared.
    let source = picture(2, 1, |x, _| grey(if x == 0 { 0 } else { 200 }));
    let out = scaled((2, 1, &source), (7, 1));
    let row: Vec<u8> = (0..7).map(|x| out.get(x, 0).unwrap().r).collect();
    assert_eq!(&row[..3], &[0, 0, 0]);
    assert_eq!(&row[4..], &[200, 200, 200]);
    assert!(row[3] > 0 && row[3] < 200, "{row:?}");
    let edges = (0..7)
        .filter(|&x| ![0, 200].contains(&out.get(x, 0).unwrap().r))
        .count();
    assert_eq!(edges, 1, "one blended pixel per edge");
}

#[test]
fn a_shrink_is_bilinear() {
    let source = picture(4, 1, |x, _| grey(if x < 2 { 0 } else { 200 }));
    let out = scaled((4, 1, &source), (2, 1));
    let row: Vec<u8> = (0..2).map(|x| out.get(x, 0).unwrap().r).collect();
    assert_eq!(row, [0, 200], "each output pixel over two of the same");
    let out = scaled((4, 1, &source), (1, 1));
    assert_eq!(out.get(0, 0).unwrap().r, 100, "the middle of the edge");
}

#[test]
fn the_picture_is_drawn_opaque() {
    let source = picture(2, 2, |_, _| [90, 80, 70, 0]);
    let out = scaled((2, 2, &source), (5, 3));
    assert!((0..3).all(|y| (0..5).all(|x| out.get(x, y).unwrap().a == 255)));
}

#[test]
fn only_the_rows_asked_for_are_drawn_and_only_inside_the_target() {
    let source = picture(4, 4, |_, _| grey(90));
    let frame = Rgba8Image::new(4, 4, &source).unwrap();
    let mut surface = Surface::new(20, 20).unwrap();
    let target = Region {
        x: 3,
        y: 2,
        width: 12,
        height: 12,
    };
    SharpScaler::new()
        .draw(&frame, &mut surface, target, 5..9)
        .unwrap();
    for y in 0..20 {
        for x in 0..20 {
            let inside = (3..15).contains(&x) && (7..11).contains(&y);
            assert_eq!(surface.get(x, y).unwrap().a == 255, inside, "({x}, {y})");
        }
    }
}

#[test]
fn the_clip_window_holds() {
    let source = picture(3, 3, |_, _| grey(40));
    let frame = Rgba8Image::new(3, 3, &source).unwrap();
    let mut surface = Surface::new(12, 12).unwrap();
    let target = Region {
        x: 0,
        y: 0,
        width: 12,
        height: 12,
    };
    let mut scaler = SharpScaler::new();
    surface.with_clip(4, 4, 4, 4, |s| {
        scaler.draw(&frame, s, target, 0..12).unwrap();
    });
    for y in 0..12 {
        for x in 0..12 {
            let inside = (4..8).contains(&x) && (4..8).contains(&y);
            assert_eq!(surface.get(x, y).unwrap().a == 255, inside, "({x}, {y})");
        }
    }
}

#[test]
fn a_change_to_source_rows_redraws_exactly_the_rows_they_reach() {
    for (source_h, dest_h) in [(200, 960), (350, 768), (480, 480), (25, 7), (3, 1000)] {
        let before = picture(2, source_h, |_, _| grey(10));
        let mut scaler = SharpScaler::new();
        let frame = Rgba8Image::new(2, source_h, &before).unwrap();
        let mut surface = Surface::new(4, dest_h).unwrap();
        let target = Region {
            x: 0,
            y: 0,
            width: 4,
            height: dest_h,
        };
        scaler
            .draw(&frame, &mut surface, target, 0..dest_h)
            .unwrap();
        for changed in [0..1, source_h / 2..source_h / 2 + 2, source_h - 1..source_h] {
            let after = picture(2, source_h, |_, y| {
                grey(if changed.contains(&y) { 250 } else { 10 })
            });
            let frame = Rgba8Image::new(2, source_h, &after).unwrap();
            let mut fresh = Surface::new(4, dest_h).unwrap();
            scaler.draw(&frame, &mut fresh, target, 0..dest_h).unwrap();
            let differs: Vec<u32> = (0..dest_h)
                .filter(|&y| fresh.get(0, y) != surface.get(0, y))
                .collect();
            let reached = scaler.rows_reached(changed.clone());
            assert!(
                differs.iter().all(|y| reached.contains(y)),
                "{source_h}->{dest_h} {changed:?}: {differs:?} beyond {reached:?}"
            );
            let spanned = differs
                .first()
                .zip(differs.last())
                .map_or(0..0, |(&first, &last)| first..last + 1);
            assert!(
                spanned == reached || spanned.is_empty() && reached.is_empty(),
                "{source_h}->{dest_h} {changed:?}: {reached:?} wider than {spanned:?}"
            );
        }
    }
}

#[test]
fn taps_stay_inside_the_source_for_every_pair_of_sizes() {
    for source in [1, 2, 3, 7, 200, 640, 754] {
        for dest in [1, 2, 5, 199, 640, 1000, 1441, 2560] {
            for tap in taps(source, dest).unwrap() {
                assert!(tap.index < source, "{source}->{dest}");
                assert!(tap.weight <= WEIGHT_ONE);
                assert!(
                    tap.weight == 0 || tap.index + 1 < source,
                    "{source}->{dest}"
                );
            }
        }
    }
}

#[test]
fn taps_never_step_backwards() {
    for (source, dest) in [(640, 1441), (200, 961), (720, 1280), (3, 2)] {
        let taps = taps(source, dest).unwrap();
        let at = |t: &Tap| u32::from(t.weight) + t.index * u32::from(WEIGHT_ONE);
        assert!(
            taps.windows(2).all(|w| at(&w[0]) <= at(&w[1])),
            "{source}->{dest}"
        );
    }
}

#[test]
fn an_empty_or_overlong_target_is_refused() {
    let source = vec![0u8; 4];
    let frame = Rgba8Image::new(1, 1, &source).unwrap();
    let mut surface = Surface::new(2, 2).unwrap();
    let empty = Region {
        x: 0,
        y: 0,
        width: 0,
        height: 2,
    };
    assert_eq!(
        SharpScaler::new().draw(&frame, &mut surface, empty, 0..2),
        Err(SharpError::EmptyTarget)
    );
    let overlong = Region {
        x: 0,
        y: 0,
        width: MAX_EXTENT + 1,
        height: 1,
    };
    assert_eq!(
        SharpScaler::new().draw(&frame, &mut surface, overlong, 0..1),
        Err(SharpError::TooLarge)
    );
}

#[test]
fn the_lerp_rounds_each_channel_to_nearest() {
    let levels = [0u8, 1, 2, 127, 128, 129, 254, 255];
    for weight in 0..=WEIGHT_ONE {
        for &first in &levels {
            for &second in &levels {
                let from = Pixel {
                    r: first,
                    g: second,
                    b: first ^ second,
                    a: 3,
                };
                let to = Pixel {
                    r: second,
                    g: first,
                    b: !first,
                    a: 9,
                };
                let channel = |from: u8, to: u8| {
                    let weight = u32::from(weight);
                    let mixed = u32::from(from) * (256 - weight) + u32::from(to) * weight + 128;
                    u8::try_from(mixed >> 8).unwrap()
                };
                let want = Pixel {
                    r: channel(from.r, to.r),
                    g: channel(from.g, to.g),
                    b: channel(from.b, to.b),
                    a: 255,
                };
                assert_eq!(lerp(from, to, weight), want, "{weight} {from:?} {to:?}");
            }
        }
    }
}
