use alloc::vec;
use alloc::vec::Vec;
use core::iter;

use tairix_abi::driver::display::{DamageList, DamageRect, DisplayFormat, DisplayMode};
use tairix_cursor::{CursorImage, PlacedCursor, Shape, VectorCursor};
use tairix_geometry::{Point, Rect};
use tairix_parallel::{JobRunner, Reversed, Threaded, SERIAL};
use tairix_raster::{Color, Pixel, Surface};

use super::{Layers, Present, Scanout, COMPOSE_GRAIN, PIXEL_BYTES};

/// Compose `damage`, or the whole screen for `None`, of `surface` over
/// `ground` under `cursor` with the veil at `reveal`, on the calling thread.
fn compose(
    scanout: &mut Scanout,
    surface: &Surface,
    ground: Option<&Surface>,
    cursor: Option<&PlacedCursor>,
    damage: Option<Rect>,
    reveal: u8,
) -> Present {
    let layers = Layers {
        cursor,
        surface,
        ground,
        reveal,
    };
    let damage = damage.unwrap_or_else(|| scanout.screen());
    scanout.compose(layers, iter::once(damage), &SERIAL)
}

/// A four-pixel-wide mode with one spare pixel of stride padding, so a wrong
/// stride shows up as a shifted row rather than passing by luck.
fn padded_mode(format: DisplayFormat) -> DisplayMode {
    DisplayMode {
        width_px: 4,
        height_px: 3,
        stride_bytes: 5 * 4,
        format,
    }
}

fn pixel(r: u8, g: u8, b: u8) -> Pixel {
    Pixel { r, g, b, a: 255 }
}

/// A surface of `mode`'s extent, every pixel distinct so a misplaced copy is
/// visible.
fn numbered(mode: &DisplayMode) -> Surface {
    let mut surface = Surface::new(mode.width_px, mode.height_px).expect("a tiny surface");
    for y in 0..mode.height_px {
        for x in 0..mode.width_px {
            let index = u8::try_from(y * mode.width_px + x).expect("a tiny surface");
            surface.set(x, y, pixel(index + 1, index + 2, index + 3));
        }
    }
    surface
}

/// A `size`×`size` cursor whose left half is opaque `colour` and whose right
/// half is transparent, with its hotspot at the image's top-left corner so a
/// pointer position is also the origin it draws from.
fn cursor(size: u32, colour: Color) -> CursorImage {
    let s = i32::try_from(size).expect("a tiny cursor");
    let shape = Shape::from_points(colour, &[(0, 0), (s / 2, 0), (s / 2, s), (0, s)]);
    VectorCursor::new(size, 0, 0, vec![shape])
        .rasterise(size)
        .expect("renderable")
}

/// A region present of exactly that rectangle.
fn region(x: u32, y: u32, width_px: u32, height_px: u32) -> Present {
    Present::Region(
        DamageList::new(&[DamageRect {
            x,
            y,
            width_px,
            height_px,
        }])
        .expect("a rectangle"),
    )
}

/// The one rectangle a region present names.
fn only(present: Present) -> DamageRect {
    let Present::Region(list) = present else {
        panic!("a sub-screen rectangle is a region present, got {present:?}");
    };
    let [rect] = list.rects() else {
        panic!("one rectangle, got {list:?}");
    };
    *rect
}

/// The four bytes frame `frame` holds for pixel `(x, y)` of `mode`.
fn at(frame: &[u8], mode: &DisplayMode, x: u32, y: u32) -> [u8; 4] {
    let stride = mode.stride_bytes as usize;
    let start = y as usize * stride + x as usize * PIXEL_BYTES;
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&frame[start..start + PIXEL_BYTES]);
    bytes
}

#[test]
fn a_mode_with_no_pixels_has_no_frame() {
    let empty = DisplayMode {
        width_px: 0,
        height_px: 0,
        stride_bytes: 0,
        format: DisplayFormat::Rgba8888,
    };
    assert!(Scanout::new(empty).is_none());
}

#[test]
fn a_stride_too_small_for_a_scanline_is_refused() {
    let narrow = DisplayMode {
        width_px: 8,
        height_px: 8,
        stride_bytes: 8,
        format: DisplayFormat::Rgba8888,
    };
    assert!(Scanout::new(narrow).is_none());
}

/// A frame the allocator cannot hold is refused, never an abort.
#[test]
fn a_frame_too_large_to_hold_is_refused() {
    let vast = DisplayMode {
        width_px: u32::MAX / 4,
        height_px: u32::MAX,
        stride_bytes: u32::MAX,
        format: DisplayFormat::Rgba8888,
    };
    assert!(Scanout::new(vast).is_none());
}

#[test]
fn the_frame_covers_the_mode() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let scanout = Scanout::new(mode).expect("a valid mode");
    assert_eq!(scanout.screen(), Rect::new(0, 0, 4, 3));
    assert_eq!(scanout.mode(), &mode);
    assert_eq!(scanout.frame().len(), 3 * 5 * 4);
}

#[test]
fn a_whole_screen_composition_presents_the_whole_frame() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    let painted = numbered(&mode);
    assert_eq!(
        compose(&mut scanout, &painted, None, None, None, u8::MAX),
        Present::Whole
    );
    assert_eq!(at(scanout.frame(), &mode, 0, 0), [1, 2, 3, 255]);
    assert_eq!(at(scanout.frame(), &mode, 3, 2), [12, 13, 14, 255]);
}

#[test]
fn the_channel_order_follows_the_format() {
    let rgba = padded_mode(DisplayFormat::Rgba8888);
    let mut in_rgba = Scanout::new(rgba).expect("a valid mode");
    compose(&mut in_rgba, &numbered(&rgba), None, None, None, u8::MAX);
    assert_eq!(at(in_rgba.frame(), &rgba, 0, 0), [1, 2, 3, 255]);

    let bgra = padded_mode(DisplayFormat::Bgra8888);
    let mut in_bgra = Scanout::new(bgra).expect("a valid mode");
    compose(&mut in_bgra, &numbered(&bgra), None, None, None, u8::MAX);
    assert_eq!(at(in_bgra.frame(), &bgra, 0, 0), [3, 2, 1, 255]);
}

#[test]
fn the_stride_padding_is_never_written() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    compose(&mut scanout, &numbered(&mode), None, None, None, u8::MAX);
    for y in 0..mode.height_px {
        assert_eq!(
            at(scanout.frame(), &mode, 4, y),
            [0, 0, 0, 0],
            "row {y}'s padding was written"
        );
    }
}

#[test]
fn a_damaged_region_copies_only_itself() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    let damage = Rect::new(1, 1, 2, 1);

    let present = compose(
        &mut scanout,
        &numbered(&mode),
        None,
        None,
        Some(damage),
        u8::MAX,
    );
    let region = only(present);
    assert_eq!((region.x, region.y), (1, 1));
    assert_eq!((region.width_px, region.height_px), (2, 1));

    assert_eq!(at(scanout.frame(), &mode, 1, 1), [6, 7, 8, 255]);
    assert_eq!(at(scanout.frame(), &mode, 2, 1), [7, 8, 9, 255]);
    assert_eq!(
        at(scanout.frame(), &mode, 0, 0),
        [0, 0, 0, 0],
        "a pixel outside the damage was copied"
    );
    assert_eq!(at(scanout.frame(), &mode, 3, 1), [0, 0, 0, 0]);
}

#[test]
fn a_damage_covering_the_screen_is_a_whole_present() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    let whole = Rect::new(0, 0, 4, 3);
    assert_eq!(
        compose(
            &mut scanout,
            &numbered(&mode),
            None,
            None,
            Some(whole),
            u8::MAX
        ),
        Present::Whole
    );
}

#[test]
fn an_empty_damage_presents_nothing_and_writes_nothing() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    let nothing = Rect::new(2, 2, 0, 0);
    assert_eq!(
        compose(
            &mut scanout,
            &numbered(&mode),
            None,
            None,
            Some(nothing),
            u8::MAX
        ),
        Present::Nothing
    );
    assert!(scanout.frame().iter().all(|byte| *byte == 0));
}

#[test]
fn a_damage_outside_the_screen_is_clipped_not_a_panic() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");

    let beyond = Rect::new(64, 64, 8, 8);
    assert_eq!(
        compose(
            &mut scanout,
            &numbered(&mode),
            None,
            None,
            Some(beyond),
            u8::MAX
        ),
        Present::Nothing
    );

    let straddling = Rect::new(3, 2, 8, 8);
    let present = compose(
        &mut scanout,
        &numbered(&mode),
        None,
        None,
        Some(straddling),
        u8::MAX,
    );
    let region = only(present);
    assert_eq!((region.width_px, region.height_px), (1, 1));
    assert_eq!(at(scanout.frame(), &mode, 3, 2), [12, 13, 14, 255]);
}

#[test]
fn a_negative_origin_is_clipped_to_the_screen() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    let present = compose(
        &mut scanout,
        &numbered(&mode),
        None,
        None,
        Some(Rect::new(-4, -4, 6, 6)),
        u8::MAX,
    );
    let region = only(present);
    assert_eq!((region.x, region.y), (0, 0));
    assert_eq!((region.width_px, region.height_px), (2, 2));
    assert_eq!(at(scanout.frame(), &mode, 0, 0), [1, 2, 3, 255]);
    assert_eq!(at(scanout.frame(), &mode, 2, 0), [0, 0, 0, 0]);
}

#[test]
fn a_surface_smaller_than_the_screen_leaves_the_rest_alone() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    let mut small = Surface::new(2, 2).expect("a tiny surface");
    small.set(0, 0, pixel(9, 9, 9));
    small.set(1, 1, pixel(8, 8, 8));

    assert_eq!(
        compose(&mut scanout, &small, None, None, None, u8::MAX),
        Present::Whole
    );
    assert_eq!(at(scanout.frame(), &mode, 0, 0), [9, 9, 9, 255]);
    assert_eq!(at(scanout.frame(), &mode, 1, 1), [8, 8, 8, 255]);
    assert_eq!(at(scanout.frame(), &mode, 3, 2), [0, 0, 0, 0]);
}

#[test]
fn the_cursor_is_blended_over_the_surface_where_it_draws_and_nowhere_else() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    let placed = PlacedCursor::new(cursor(2, Color::rgb(200, 10, 20)), Point::new(1, 1));

    assert_eq!(
        compose(
            &mut scanout,
            &numbered(&mode),
            None,
            Some(&placed),
            None,
            u8::MAX
        ),
        Present::Whole
    );
    assert_eq!(at(scanout.frame(), &mode, 1, 1), [200, 10, 20, 255]);
    assert_eq!(at(scanout.frame(), &mode, 1, 2), [200, 10, 20, 255]);
    assert_eq!(
        at(scanout.frame(), &mode, 2, 1),
        [7, 8, 9, 255],
        "the cursor's transparent half left the surface showing"
    );
    assert_eq!(
        at(scanout.frame(), &mode, 0, 0),
        [1, 2, 3, 255],
        "a pixel outside the cursor was blended"
    );
}

#[test]
fn a_composition_that_misses_the_cursor_copies_the_surface_alone() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    let placed = PlacedCursor::new(cursor(2, Color::rgb(200, 10, 20)), Point::new(1, 1));

    compose(
        &mut scanout,
        &numbered(&mode),
        None,
        Some(&placed),
        Some(Rect::new(0, 0, 4, 1)),
        u8::MAX,
    );
    assert_eq!(at(scanout.frame(), &mode, 1, 0), [2, 3, 4, 255]);
    assert_eq!(
        at(scanout.frame(), &mode, 1, 1),
        [0, 0, 0, 0],
        "a row outside the damage was written"
    );
}

#[test]
fn a_cursor_overhanging_the_screen_draws_only_the_part_that_fits() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    let placed = PlacedCursor::new(cursor(2, Color::rgb(200, 10, 20)), Point::new(-1, 2));

    assert_eq!(
        compose(
            &mut scanout,
            &numbered(&mode),
            None,
            Some(&placed),
            None,
            u8::MAX
        ),
        Present::Whole
    );
    assert_eq!(
        at(scanout.frame(), &mode, 0, 2),
        [9, 10, 11, 255],
        "the cursor's transparent column landed on the only visible one"
    );
    assert_eq!(at(scanout.frame(), &mode, 0, 0), [1, 2, 3, 255]);
}

#[test]
fn nothing_merged_with_a_present_is_that_present() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let some = region(1, 1, 2, 1);
    assert_eq!(
        Present::Nothing.merged(Present::Nothing, &mode),
        Present::Nothing
    );
    assert_eq!(Present::Nothing.merged(some, &mode), some);
    assert_eq!(some.merged(Present::Nothing, &mode), some);
    assert_eq!(
        Present::Nothing.merged(Present::Whole, &mode),
        Present::Whole
    );
}

#[test]
fn a_whole_present_swallows_whatever_it_meets() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    assert_eq!(
        Present::Whole.merged(region(1, 1, 2, 1), &mode),
        Present::Whole
    );
    assert_eq!(
        region(1, 1, 2, 1).merged(Present::Whole, &mode),
        Present::Whole
    );
    assert_eq!(Present::Whole.merged(Present::Whole, &mode), Present::Whole);
}

/// Two changes apart on the screen present apart, rather than as the box
/// between them: a ribbon's strips at opposite edges cost their own pixels.
#[test]
fn two_regions_apart_present_apart() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let rect = |x, y| DamageRect {
        x,
        y,
        width_px: 1,
        height_px: 1,
    };
    for (first, second) in [((0, 0), (2, 1)), ((2, 1), (0, 0))] {
        let merged = region(first.0, first.1, 1, 1).merged(region(second.0, second.1, 1, 1), &mode);
        let Present::Region(list) = merged else {
            panic!("still a region present, got {merged:?}");
        };
        let mut rects = list.rects().to_vec();
        rects.sort_unstable_by_key(|rect| (rect.y, rect.x));
        assert_eq!(rects, [rect(0, 0), rect(2, 1)], "either way round");
    }
}

/// Rows whose box spans the screen are still only the rows; rows that tile
/// it are the whole screen.
#[test]
fn regions_merge_into_a_whole_present_only_when_they_cover_the_screen() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    assert_ne!(
        region(0, 0, 4, 1).merged(region(0, 2, 4, 1), &mode),
        Present::Whole,
        "the middle row did not change"
    );
    assert_eq!(
        region(0, 0, 4, 2).merged(region(0, 2, 4, 1), &mode),
        Present::Whole
    );
}

#[test]
fn a_second_composition_keeps_the_pixels_the_first_wrote() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    compose(&mut scanout, &numbered(&mode), None, None, None, u8::MAX);

    let mut changed = numbered(&mode);
    changed.set(0, 0, pixel(200, 201, 202));
    compose(
        &mut scanout,
        &changed,
        None,
        None,
        Some(Rect::new(0, 0, 1, 1)),
        u8::MAX,
    );

    assert_eq!(at(scanout.frame(), &mode, 0, 0), [200, 201, 202, 255]);
    assert_eq!(
        at(scanout.frame(), &mode, 3, 2),
        [12, 13, 14, 255],
        "the untouched pixels are still the first frame's"
    );
}

/// The veil the login screen dissolves through was painted into the surface
/// until it moved here, so what it presents must not have changed by a byte.
///
/// The reference is that former paint: a copy of the surface washed with black
/// at the veil's own alpha, blitted whole. If the two ever disagree the fade
/// has changed appearance, which is not what making it cheap was for.
#[test]
fn a_veiled_blit_presents_exactly_what_the_painted_wash_presented() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let painted = numbered(&mode);

    for closed in [0u8, 1, 64, 128, 200, 254, 255] {
        let reveal = u8::MAX - closed;

        let mut direct = Scanout::new(mode).expect("a valid mode");
        compose(&mut direct, &painted, None, None, None, reveal);

        let mut washed = numbered(&mode);
        let black = Color {
            r: 0,
            g: 0,
            b: 0,
            a: closed,
        };
        let (w, h) = (washed.width(), washed.height());
        washed.fill_vertical_gradient(0, 0, w, h, black, black);
        let mut reference = Scanout::new(mode).expect("a valid mode");
        compose(&mut reference, &washed, None, None, None, u8::MAX);

        assert_eq!(
            direct.frame(),
            reference.frame(),
            "the veil closed {closed}/255 of the way"
        );
    }
}

/// A fully closed veil presents black, whatever was painted under it.
#[test]
fn a_fully_closed_veil_presents_black() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    compose(&mut scanout, &numbered(&mode), None, None, None, 0);

    for y in 0..mode.height_px {
        for x in 0..mode.width_px {
            assert_eq!(at(scanout.frame(), &mode, x, y), [0, 0, 0, 255]);
        }
    }
}

/// The pointer stays crisp over a dissolving screen: the veil is applied to
/// what is under it, exactly as the wash it replaced was painted beneath the
/// cursor the composer samples over the top.
#[test]
fn the_veil_dims_the_screen_under_the_cursor_and_not_the_cursor() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let painted = numbered(&mode);
    let placed = PlacedCursor::new(cursor(2, Color::rgb(250, 10, 20)), Point::new(0, 0));

    let mut scanout = Scanout::new(mode).expect("a valid mode");
    compose(&mut scanout, &painted, None, Some(&placed), None, 128);

    assert_eq!(
        at(scanout.frame(), &mode, 0, 0),
        [250, 10, 20, 255],
        "the opaque half of the cursor keeps its own colour"
    );
    let under = painted.get(3, 2).expect("a pixel");
    assert_eq!(
        at(scanout.frame(), &mode, 3, 2),
        [
            tairix_raster::div255(u32::from(under.r) * 128),
            tairix_raster::div255(u32::from(under.g) * 128),
            tairix_raster::div255(u32::from(under.b) * 128),
            255
        ],
        "a pixel the cursor does not reach is dimmed"
    );
}

/// A surface transparent in its left column, half-covered in its middle ones
/// and opaque in its right: what a login column over a scene looks like.
fn over_a_scene(mode: &DisplayMode) -> Surface {
    let mut surface = Surface::new(mode.width_px, mode.height_px).expect("a tiny surface");
    for y in 0..mode.height_px {
        surface.set(
            1,
            y,
            Pixel {
                r: 64,
                g: 0,
                b: 0,
                a: 128,
            },
        );
        surface.set(
            2,
            y,
            Pixel {
                r: 64,
                g: 0,
                b: 0,
                a: 128,
            },
        );
        surface.set(3, y, pixel(200, 200, 200));
    }
    surface
}

/// Where the surface is transparent the scene shows through untouched, where
/// it is opaque the scene is hidden, and between them the two are blended.
#[test]
fn a_surface_over_a_scene_shows_the_scene_through_its_transparency() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let scene = numbered(&mode);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    assert_eq!(
        compose(
            &mut scanout,
            &over_a_scene(&mode),
            Some(&scene),
            None,
            None,
            u8::MAX
        ),
        Present::Whole
    );
    for y in 0..mode.height_px {
        let under = scene.get(0, y).expect("a pixel");
        assert_eq!(
            at(scanout.frame(), &mode, 0, y),
            [under.r, under.g, under.b, 255]
        );
        assert_eq!(at(scanout.frame(), &mode, 3, y), [200, 200, 200, 255]);
        let [r, g, b, a] = at(scanout.frame(), &mode, 1, y);
        let behind = scene.get(1, y).expect("a pixel");
        assert!(r > behind.r / 2 && r < 64 + behind.r, "row {y}: red {r}");
        assert!(
            g < behind.g && b < behind.b,
            "row {y}: the scene is half hidden"
        );
        assert_eq!(a, 255);
    }
}

/// Only the pixels asked for are composed: the scene's changes elsewhere wait
/// for a composition that names them.
#[test]
fn a_scene_is_composed_only_where_it_is_asked_for() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    let clear = Surface::new(mode.width_px, mode.height_px).expect("a tiny surface");
    let scene = numbered(&mode);
    compose(
        &mut scanout,
        &clear,
        Some(&scene),
        None,
        Some(Rect::new(0, 0, 1, 1)),
        u8::MAX,
    );
    let first = scene.get(0, 0).expect("a pixel");
    assert_eq!(
        at(scanout.frame(), &mode, 0, 0),
        [first.r, first.g, first.b, 255]
    );
    assert_eq!(
        at(scanout.frame(), &mode, 1, 0),
        [0, 0, 0, 0],
        "never composed"
    );
}

/// A ground that is not the surface's size is no layer of this screen, and is
/// left out rather than read at the wrong stride.
#[test]
fn a_ground_of_another_size_is_left_out() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let surface = numbered(&mode);
    let stranger = Surface::filled(2, 2, pixel(9, 9, 9)).expect("a tiny surface");

    let mut plain = Scanout::new(mode).expect("a valid mode");
    compose(&mut plain, &surface, None, None, None, u8::MAX);
    let mut grounded = Scanout::new(mode).expect("a valid mode");
    compose(
        &mut grounded,
        &surface,
        Some(&stranger),
        None,
        None,
        u8::MAX,
    );
    assert_eq!(plain.frame(), grounded.frame());
}

/// The veil closes over the scene exactly as over the surface: fully closed,
/// the screen is black whatever lies under the column.
#[test]
fn a_closed_veil_covers_the_scene_too() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let scene = numbered(&mode);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    compose(
        &mut scanout,
        &over_a_scene(&mode),
        Some(&scene),
        None,
        None,
        0,
    );
    for y in 0..mode.height_px {
        for x in 0..mode.width_px {
            assert_eq!(at(scanout.frame(), &mode, x, y), [0, 0, 0, 255]);
        }
    }
}

/// Blackening covers every pixel of the screen and presents all of it, and
/// leaves the stride padding untouched.
#[test]
fn blackening_covers_the_whole_screen_and_nothing_beyond_it() {
    let mode = padded_mode(DisplayFormat::Bgra8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    compose(&mut scanout, &numbered(&mode), None, None, None, u8::MAX);
    assert_eq!(scanout.blacken(), Present::Whole);
    for y in 0..mode.height_px {
        for x in 0..mode.width_px {
            assert_eq!(at(scanout.frame(), &mode, x, y), [0, 0, 0, 255]);
        }
        assert_eq!(at(scanout.frame(), &mode, 4, y), [0, 0, 0, 0], "padding");
    }
}

/// A screen large enough that a compose is split into several bands, with
/// stride padding so a band that lost its place shows as a shifted row.
fn wide_mode() -> DisplayMode {
    DisplayMode {
        width_px: 512,
        height_px: 256,
        stride_bytes: 520 * 4,
        format: DisplayFormat::Bgra8888,
    }
}

/// A surface of `mode`'s extent whose every pixel differs from its
/// neighbours', translucent wherever `translucent` holds.
fn patterned(mode: &DisplayMode, salt: u32, translucent: bool) -> Surface {
    let mut surface = Surface::new(mode.width_px, mode.height_px).expect("a small surface");
    for y in 0..mode.height_px {
        for x in 0..mode.width_px {
            let mixed = (x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B) ^ salt)
                .rotate_left(13)
                .to_le_bytes();
            let a = if translucent { mixed[3] } else { u8::MAX };
            let premultiplied =
                |channel: u8| tairix_raster::div255(u32::from(channel) * u32::from(a));
            surface.set(
                x,
                y,
                Pixel {
                    r: premultiplied(mixed[0]),
                    g: premultiplied(mixed[1]),
                    b: premultiplied(mixed[2]),
                    a,
                },
            );
        }
    }
    surface
}

/// The damage a ribbon frame and a keystroke leave together: narrow strips
/// across the screen, overlapping parts, and a rectangle hanging off it.
fn scattered_damage() -> Vec<Rect> {
    let mut damage: Vec<Rect> = (0u32..24)
        .map(|strip| {
            let at = i32::try_from(strip).expect("a small index");
            Rect::new(at * 21, (at * 7) % 60, 16, 140 + strip % 5 * 20)
        })
        .collect();
    damage.extend([
        Rect::new(100, 90, 200, 40),
        Rect::new(150, 100, 300, 12),
        Rect::new(480, -20, 64, 300),
    ]);
    damage
}

/// One compose of `damage` from these layers across `runner`.
fn composed_across(runner: &dyn JobRunner, damage: &[Rect], reveal: u8) -> (Vec<u8>, Present) {
    let mode = wide_mode();
    let (surface, scene) = (patterned(&mode, 1, true), patterned(&mode, 2, false));
    let placed = PlacedCursor::new(cursor(24, Color::rgb(250, 10, 20)), Point::new(140, 100));
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    scanout.spread_over(runner);
    let layers = Layers {
        cursor: Some(&placed),
        surface: &surface,
        ground: Some(&scene),
        reveal,
    };
    let present = scanout.compose(layers, damage.iter().copied(), runner);
    (scanout.frame().to_vec(), present)
}

/// A compose split into bands writes the frame a whole-frame walk writes,
/// byte for byte, whichever band runs first and when bands run at once —
/// over a scene, under the pointer, through the veil, with rectangles that
/// overlap and one that hangs off the screen.
#[test]
fn a_compose_in_bands_writes_what_one_walk_writes() {
    let damage = scattered_damage();
    for reveal in [u8::MAX, 77] {
        let (serial, serial_present) = composed_across(&SERIAL, &damage, reveal);
        let reversed = Reversed::new(4);
        let (banded, banded_present) = composed_across(&reversed, &damage, reveal);
        assert!(reversed.widest() > 1, "the compose was split");
        assert_eq!(banded, serial, "bands run in reverse, veil {reveal}");
        assert_eq!(banded_present, serial_present);
        let (threaded, _) = composed_across(&Threaded::new(4), &damage, reveal);
        assert_eq!(threaded, serial, "bands run at once, veil {reveal}");
    }
}

/// Every rectangle a compose is given is copied, and the present names each
/// of them rather than the box between them.
#[test]
fn every_rectangle_of_a_compose_is_copied_and_presented() {
    let mode = padded_mode(DisplayFormat::Rgba8888);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    let painted = numbered(&mode);
    let layers = Layers {
        cursor: None,
        surface: &painted,
        ground: None,
        reveal: u8::MAX,
    };
    let apart = [Rect::new(0, 0, 1, 1), Rect::new(3, 2, 1, 1)];
    let Present::Region(list) = scanout.compose(layers, apart.iter().copied(), &SERIAL) else {
        panic!("two pixels apart are a region present");
    };
    assert_eq!(list.len(), 2);
    assert_eq!(at(scanout.frame(), &mode, 0, 0), [1, 2, 3, 255]);
    assert_eq!(at(scanout.frame(), &mode, 3, 2), [12, 13, 14, 255]);
    assert_eq!(
        at(scanout.frame(), &mode, 1, 1),
        [0, 0, 0, 0],
        "between them"
    );
}

/// A rectangle covering the screen copies it whole, and anything else the
/// damage names adds nothing to what is written or presented.
#[test]
fn a_rectangle_covering_the_screen_swallows_the_rest() {
    let damage = scattered_damage();
    let mode = wide_mode();
    let mut covering = damage.clone();
    covering.insert(
        3,
        Rect::new(-5, -5, mode.width_px + 10, mode.height_px + 10),
    );
    let (whole, whole_present) = composed_across(&SERIAL, &[Rect::new(0, 0, 512, 256)], 200);
    let (swallowed, swallowed_present) = composed_across(&Reversed::new(4), &covering, 200);
    assert_eq!(swallowed, whole);
    assert_eq!(
        (whole_present, swallowed_present),
        (Present::Whole, Present::Whole)
    );
}

/// A compose is split no finer than the scanlines held for it, and one too
/// small to be worth handing off stays on the calling thread.
#[test]
fn a_compose_is_split_only_as_far_as_its_scanlines_and_its_size_allow() {
    let mode = wide_mode();
    let surface = patterned(&mode, 3, false);
    let layers = Layers {
        cursor: None,
        surface: &surface,
        ground: None,
        reveal: u8::MAX,
    };
    let screen = Rect::new(0, 0, mode.width_px, mode.height_px);

    let unspread = Reversed::new(4);
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    scanout.compose(layers, iter::once(screen), &unspread);
    assert_eq!(unspread.dispatches(), 0, "one scanline held, so one band");

    let spread = Reversed::new(4);
    scanout.spread_over(&spread);
    scanout.compose(layers, iter::once(screen), &spread);
    assert_eq!(spread.widest(), 4, "a band for each scanline held");

    let small = Reversed::new(4);
    let side = u32::try_from(COMPOSE_GRAIN.isqrt()).expect("a small grain") - 1;
    scanout.compose(layers, iter::once(Rect::new(0, 0, side, side)), &small);
    assert_eq!(small.dispatches(), 0, "less than a grain of pixels");
}

/// A row of the surface holding long clear runs either side of its ink lays
/// the ink exactly as blending the whole row over the scene would, however
/// the runs fall against the sixteen pixels the clear test takes at a time:
/// trimming the clear runs off loses no ink at either end.
#[test]
fn a_row_with_long_clear_runs_lays_its_ink_as_a_whole_row_blend_would() {
    let mode = DisplayMode {
        width_px: 96,
        height_px: 6,
        stride_bytes: 96 * 4,
        format: DisplayFormat::Bgra8888,
    };
    let scene = patterned(&mode, 9, false);
    let mut surface = Surface::new(mode.width_px, mode.height_px).expect("a small surface");
    let ink = Pixel {
        r: 90,
        g: 40,
        b: 10,
        a: 160,
    };
    for (y, inked) in (0u32..).zip([1..2, 17..18, 31..64, 15..81, 40..41, 0..96]) {
        for x in inked {
            surface.set(x, y, ink);
        }
    }
    let mut scanout = Scanout::new(mode).expect("a valid mode");
    let layers = Layers {
        cursor: None,
        surface: &surface,
        ground: Some(&scene),
        reveal: u8::MAX,
    };
    let screen = scanout.screen();
    scanout.compose(layers, iter::once(screen), &SERIAL);

    let order = tairix_display::ChannelOrder::Bgra;
    for y in 0..mode.height_px {
        let width = usize::try_from(mode.width_px).expect("small");
        let start = usize::try_from(y).expect("small") * width;
        let mut laid = scene.pixels()[start..start + width].to_vec();
        tairix_raster::blend_span(
            &mut laid,
            &surface.pixels()[start..start + width],
            u8::MAX,
            tairix_raster::DitherRow::at(y),
            0,
        );
        for (x, pixel) in (0u32..).zip(&laid) {
            assert_eq!(
                at(scanout.frame(), &mode, x, y),
                order.encode(*pixel),
                "({x}, {y})"
            );
        }
    }
}
