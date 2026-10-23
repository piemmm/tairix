//! Unit tests for the window-frame pixel codec.
//!
//! Three properties carry the module: a frame round-trips an app's pixels
//! exactly at every alpha, a frame a client wrote out of range decodes to a
//! valid premultiplied pixel, and splitting the conversion across bands changes
//! the wall-clock cost and nothing else — the last asserted against a runner
//! that reports a width it does not have and runs its pieces backwards, so the
//! order dependence a real pool could expose is exercised with no thread
//! involved.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::display::{DamageList, DamageRect, DisplayFormat, DisplayMode};
use tairix_abi::Errno;
use tairix_geometry::{Rect, Region};
use tairix_parallel::{JobRunner, Reversed, SERIAL};
use tairix_raster::{Color, Pixel, Surface};

use super::{decode, decode_list, encode, lay_opaque, lay_opaque_into, lay_opaque_region};
use crate::scanout::ChannelOrder;

const WIDTH: u32 = 40;
const HEIGHT: u32 = 24;

/// A runner wide enough that a `WIDTH`×`HEIGHT` rectangle really is split.
fn splitting() -> Reversed {
    Reversed::new(HEIGHT as usize)
}

fn mode(format: DisplayFormat, stride_bytes: u32) -> DisplayMode {
    DisplayMode {
        width_px: WIDTH,
        height_px: HEIGHT,
        stride_bytes,
        format,
    }
}

fn packed(format: DisplayFormat) -> DisplayMode {
    mode(format, WIDTH * 4)
}

fn frame_for(mode: &DisplayMode) -> Vec<u8> {
    vec![0u8; (mode.stride_bytes as usize) * (mode.height_px as usize)]
}

/// A deterministic picture with a different colour and alpha per pixel, so a
/// channel swap, an alpha loss, or a transposed row all show up.
fn painted() -> Surface {
    let mut surface =
        Surface::filled(WIDTH, HEIGHT, Color::rgba(0, 0, 0, 255).premultiply()).expect("surface");
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            surface.set(x, y, sample(x, y));
        }
    }
    surface
}

/// The pixel `painted` puts at `(x, y)`: every alpha from transparent to
/// opaque, the faint ones included.
fn sample(x: u32, y: u32) -> Pixel {
    let a = u8::try_from((x * 7 + y * 3) % 256).unwrap_or(0);
    let scale = |v: u32| u8::try_from(u32::from(a) * (v % 64) / 63).unwrap_or(0);
    Pixel {
        r: scale(x),
        g: scale(y * 2),
        b: scale(x + y),
        a,
    }
}

fn blank() -> Surface {
    Surface::filled(WIDTH, HEIGHT, Color::rgba(0, 0, 0, 0).premultiply()).expect("surface")
}

fn whole() -> DamageRect {
    DamageRect {
        x: 0,
        y: 0,
        width_px: WIDTH,
        height_px: HEIGHT,
    }
}

/// Encode `source` and decode it straight back, answering the recovered surface
/// and the rectangle the decode reported as changed.
fn round_trip(source: &Surface, mode: &DisplayMode, runner: &dyn JobRunner) -> (Surface, Rect) {
    let mut frame = frame_for(mode);
    encode(source, &mut frame, mode, whole(), runner).expect("encode");
    let mut target = blank();
    let changed = decode(&frame, &mut target, mode, whole(), runner).expect("decode");
    (target, changed)
}

#[test]
fn a_frame_round_trips_every_pixel_in_both_channel_orders() {
    let source = painted();
    for format in [DisplayFormat::Rgba8888, DisplayFormat::Bgra8888] {
        let mode = packed(format);
        let (recovered, changed) = round_trip(&source, &mode, &SERIAL);
        assert_eq!(
            recovered.pixels(),
            source.pixels(),
            "{format:?} lost or reordered a channel"
        );
        assert_eq!(changed, Rect::new(0, 0, WIDTH, HEIGHT));
    }
}

/// An opaque lay keeps every colour channel as the frame wrote it in either
/// order and takes every alpha as full, however faint the frame said it was.
#[test]
fn an_opaque_lay_keeps_the_colour_and_fills_the_alpha_in_both_orders() {
    let source = painted();
    for format in [DisplayFormat::Rgba8888, DisplayFormat::Bgra8888] {
        let mode = packed(format);
        let mut frame = frame_for(&mode);
        encode(&source, &mut frame, &mode, whole(), &SERIAL).expect("encode");
        let mut laid = blank();
        lay_opaque(&frame, &mut laid, &mode, whole(), &splitting()).expect("lay");
        for (got, sent) in laid.pixels().iter().zip(source.pixels()) {
            let want = Pixel {
                a: u8::MAX,
                ..*sent
            };
            assert_eq!(*got, want, "{format:?}");
        }
    }
}

/// A lay split across bands writes exactly what one pass writes.
#[test]
fn an_opaque_lay_split_across_bands_matches_one_pass() {
    // Large enough that a band is worth splitting off.
    let (width, height) = (600, 240);
    let mode = DisplayMode {
        width_px: width,
        height_px: height,
        stride_bytes: width * 4,
        format: DisplayFormat::Bgra8888,
    };
    let frame: Vec<u8> = (0..width * height * 4)
        .map(|at| u8::try_from(at * 31 % 251).unwrap_or(0))
        .collect();
    let part = DamageRect {
        x: 3,
        y: 2,
        width_px: width - 5,
        height_px: height - 3,
    };
    let fresh = || Surface::new(width, height).expect("a surface");
    let (mut serial, mut banded) = (fresh(), fresh());
    lay_opaque(&frame, &mut serial, &mode, part, &SERIAL).expect("lay");
    let runner = Reversed::new(8);
    lay_opaque(&frame, &mut banded, &mode, part, &runner).expect("lay");
    assert!(runner.widest() > 1, "the lay was split");
    assert_eq!(serial, banded);
    assert_eq!(serial.get(2, 2), Some(Pixel::TRANSPARENT), "outside it");
    assert_eq!(serial.get(3, 2).map(|pixel| pixel.a), Some(u8::MAX));
}

/// Every rectangle of a region is checked before a pixel moves, so one whose
/// rows the frame does not hold lays none of them.
#[test]
fn an_opaque_region_with_one_bad_rectangle_lays_nothing() {
    let mode = packed(DisplayFormat::Rgba8888);
    let mut frame = frame_for(&mode);
    encode(&painted(), &mut frame, &mode, whole(), &SERIAL).expect("encode");
    let mut damage = Region::new();
    damage.add(Rect::new(0, 0, 4, 4));
    damage.add(Rect::new(0, 20, 4, 4));
    let short = &frame[..frame.len() / 2];
    let mut laid = blank();
    assert_eq!(
        lay_opaque_region(short, &mut laid, &mode, &damage, &SERIAL),
        Err(Errno::OutOfRange)
    );
    assert_eq!(laid, blank());
    lay_opaque_region(&frame, &mut laid, &mode, &damage, &SERIAL).expect("lays");
    assert_eq!(laid.get(3, 3).map(|pixel| pixel.a), Some(u8::MAX));
    assert_eq!(laid.get(3, 23).map(|pixel| pixel.a), Some(u8::MAX));
    assert_eq!(laid.get(4, 3), Some(Pixel::TRANSPARENT), "outside it");
}

/// A region laid in one pass of bands writes exactly what laying each of its
/// rectangles on the surface alone writes, and nothing beside them.
#[test]
fn an_opaque_region_laid_in_bands_matches_each_rectangle_laid_alone() {
    let (width, height) = (600, 480);
    let mode = DisplayMode {
        width_px: width,
        height_px: height,
        stride_bytes: width * 4,
        format: DisplayFormat::Bgra8888,
    };
    let frame: Vec<u8> = (0..width * height * 4)
        .map(|at| u8::try_from(at * 31 % 251).unwrap_or(0))
        .collect();
    let mut damage = Region::new();
    for (x, y, w, h) in [
        (3, 2, 500, 200),
        (400, 150, 190, 300),
        (10, 300, 590, 170),
        (700, 0, 9, 9),
    ] {
        damage.add(Rect::new(x, y, w, h));
    }
    let fresh = || Surface::new(width, height).expect("a surface");
    let (mut each, mut banded) = (fresh(), fresh());
    for named in damage
        .rects()
        .iter()
        .filter_map(|rect| super::damage_in(&mode, *rect))
    {
        lay_opaque(&frame, &mut each, &mode, named, &SERIAL).expect("lay");
    }
    let runner = Reversed::new(8);
    lay_opaque_region(&frame, &mut banded, &mode, &damage, &runner).expect("lay");
    assert!(runner.widest() > 1, "the lay was split");
    assert_eq!(each, banded);
    assert_eq!(banded.get(2, 2), Some(Pixel::TRANSPARENT), "outside it");
}

/// A region laid into a display's own frame, at its stride and in its order,
/// holds exactly what laying it onto a surface and encoding that holds: every
/// pixel opaque, nothing between the rectangles or in the stride's padding
/// touched, the same split into bands as in one pass, and a frame too short
/// refused whole.
#[test]
fn a_region_laid_into_a_display_frame_matches_a_lay_then_an_encode() {
    let (width, height) = (300u32, 220u32);
    let mode = DisplayMode {
        width_px: width,
        height_px: height,
        stride_bytes: width * 4,
        format: DisplayFormat::Rgba8888,
    };
    let frame: Vec<u8> = (0..width * height * 4)
        .map(|at| u8::try_from(at * 29 % 253).unwrap_or(0))
        .collect();
    let mut damage = Region::new();
    for (x, y, w, h) in [(0, 0, 40, 30), (250, 10, 50, 200), (20, 100, 200, 120)] {
        damage.add(Rect::new(x, y, w, h));
    }
    let stride = usize::try_from(width).expect("small") * 4 + 24;
    let len = stride * usize::try_from(height).expect("small");
    let mut expected = vec![0x11u8; len];
    let mut laid = Surface::new(width, height).expect("a surface");
    lay_opaque_region(&frame, &mut laid, &mode, &damage, &SERIAL).expect("laid");
    for rect in damage.rects() {
        for y in rect.top()..rect.bottom() {
            let row = usize::try_from(y).expect("on screen");
            let left = usize::try_from(rect.left()).expect("on screen");
            let pixels: Vec<Pixel> = (rect.left()..rect.right())
                .map(|x| {
                    laid.get(x.cast_unsigned(), y.cast_unsigned())
                        .expect("in bounds")
                })
                .collect();
            let at = row * stride + left * 4;
            let out = &mut expected[at..at + pixels.len() * 4];
            assert_eq!(ChannelOrder::Bgra.encode_run(&pixels, out), pixels.len());
        }
    }
    for runner in [&SERIAL as &dyn JobRunner, &Reversed::new(8)] {
        let mut target = vec![0x11u8; len];
        lay_opaque_into(
            &frame,
            &mode,
            (&mut target, stride, ChannelOrder::Bgra),
            &damage,
            runner,
        )
        .expect("laid");
        assert!(target == expected, "the display frame differs");
    }
    let mut short = vec![0x11u8; len - stride];
    assert_eq!(
        lay_opaque_into(
            &frame,
            &mode,
            (&mut short, stride, ChannelOrder::Bgra),
            &damage,
            &SERIAL
        ),
        Err(Errno::OutOfRange)
    );
    assert!(short.iter().all(|byte| *byte == 0x11), "nothing written");
    let mut narrow = vec![0x11u8; len];
    assert_eq!(
        lay_opaque_into(
            &frame,
            &mode,
            (&mut narrow, stride - 28, ChannelOrder::Bgra),
            &damage,
            &SERIAL
        ),
        Err(Errno::OutOfRange),
        "a row wider than the display's stride"
    );
    assert!(narrow.iter().all(|byte| *byte == 0x11), "nothing written");
}

#[test]
fn a_channel_a_client_wrote_above_its_alpha_decodes_clamped_to_it() {
    let mode = packed(DisplayFormat::Rgba8888);
    let mut frame = frame_for(&mode);
    // Brighter than its own coverage, and colour with no coverage at all.
    frame[..8].copy_from_slice(&[200, 90, 255, 100, 255, 255, 255, 0]);
    let mut target = blank();
    decode(&frame, &mut target, &mode, whole(), &SERIAL).expect("decode");
    assert_eq!(
        target.get(0, 0),
        Some(Pixel {
            r: 100,
            g: 90,
            b: 100,
            a: 100
        })
    );
    assert_eq!(target.get(1, 0), Some(Pixel::TRANSPARENT));
}

#[test]
fn a_frame_holds_the_premultiplied_pixels_in_the_mode_s_order() {
    let mut source = blank();
    let pixel = Pixel {
        r: 10,
        g: 20,
        b: 30,
        a: 40,
    };
    source.set(0, 0, pixel);
    for (format, bytes) in [
        (DisplayFormat::Rgba8888, [10, 20, 30, 40]),
        (DisplayFormat::Bgra8888, [30, 20, 10, 40]),
    ] {
        let mode = packed(format);
        let mut frame = frame_for(&mode);
        encode(&source, &mut frame, &mode, whole(), &SERIAL).expect("encode");
        assert_eq!(frame[..4], bytes, "{format:?}");
    }
}

#[test]
fn a_stride_wider_than_the_row_leaves_its_padding_alone() {
    let mode = mode(DisplayFormat::Rgba8888, WIDTH * 4 + 16);
    let source = painted();
    let mut frame = frame_for(&mode);
    encode(&source, &mut frame, &mode, whole(), &SERIAL).expect("encode");
    for y in 0..HEIGHT as usize {
        let row = y * mode.stride_bytes as usize;
        let padding = &frame[row + (WIDTH as usize) * 4..row + mode.stride_bytes as usize];
        assert!(padding.iter().all(|&byte| byte == 0), "padding was written");
    }
    let mut target = blank();
    decode(&frame, &mut target, &mode, whole(), &SERIAL).expect("decode");
    assert_eq!(target.pixels(), source.pixels());
}

#[test]
fn splitting_the_conversion_changes_nothing_it_produces() {
    let source = painted();
    let mode = packed(DisplayFormat::Bgra8888);

    let mut serial_frame = frame_for(&mode);
    encode(&source, &mut serial_frame, &mode, whole(), &SERIAL).expect("encode");
    let mut split_frame = frame_for(&mode);
    encode(&source, &mut split_frame, &mode, whole(), &splitting()).expect("encode");
    assert_eq!(serial_frame, split_frame, "a band wrote different bytes");

    // A decode over an already-populated surface, so the change detection —
    // which is what the bands have to agree on — is exercised rather than a
    // first fill.
    let mut serial_target = painted();
    serial_target.set(3, 5, Color::rgba(1, 2, 3, 255).premultiply());
    let mut split_target = serial_target.clone();
    let serial_changed =
        decode(&serial_frame, &mut serial_target, &mode, whole(), &SERIAL).expect("decode");
    let split_changed = decode(
        &split_frame,
        &mut split_target,
        &mode,
        whole(),
        &splitting(),
    )
    .expect("decode");
    assert_eq!(serial_changed, split_changed, "the bands' union disagreed");
    assert_eq!(serial_target.pixels(), split_target.pixels());
}

#[test]
fn an_unchanged_frame_reports_no_damage() {
    let source = painted();
    let mode = packed(DisplayFormat::Rgba8888);
    let mut frame = frame_for(&mode);
    encode(&source, &mut frame, &mode, whole(), &SERIAL).expect("encode");

    let mut target = painted();
    for runner in [&SERIAL as &dyn JobRunner, &splitting()] {
        let changed = decode(&frame, &mut target, &mode, whole(), runner).expect("decode");
        assert_eq!(changed, Rect::EMPTY, "an identical present reported damage");
    }
}

#[test]
fn the_reported_damage_is_the_box_of_the_pixels_that_really_changed() {
    let mode = packed(DisplayFormat::Rgba8888);
    let source = painted();
    let mut frame = frame_for(&mode);
    encode(&source, &mut frame, &mode, whole(), &SERIAL).expect("encode");

    // Two far-apart differences: their bounding box is the answer, and it is
    // deliberately much smaller than the whole-window damage declared.
    let mut target = painted();
    target.set(4, 6, Color::rgba(255, 255, 255, 255).premultiply());
    target.set(9, 11, Color::rgba(255, 255, 255, 255).premultiply());
    for runner in [&SERIAL as &dyn JobRunner, &splitting()] {
        let mut probe = target.clone();
        let changed = decode(&frame, &mut probe, &mode, whole(), runner).expect("decode");
        assert_eq!(changed, Rect::new(4, 6, 6, 6));
        assert_eq!(probe.pixels(), source.pixels(), "the frame did not land");
    }
}

#[test]
fn a_sub_rectangle_touches_nothing_outside_itself() {
    let mode = packed(DisplayFormat::Rgba8888);
    let source = painted();
    let damage = DamageRect {
        x: 8,
        y: 4,
        width_px: 6,
        height_px: 3,
    };

    let mut frame = frame_for(&mode);
    encode(&source, &mut frame, &mode, damage, &splitting()).expect("encode");
    let mut target = blank();
    let changed = decode(&frame, &mut target, &mode, damage, &splitting()).expect("decode");
    assert_eq!(changed, Rect::new(8, 4, 6, 3));
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let inside = (8..14).contains(&x) && (4..7).contains(&y);
            let want = if inside {
                sample(x, y)
            } else {
                Color::rgba(0, 0, 0, 0).premultiply()
            };
            assert_eq!(target.get(x, y), Some(want), "at ({x}, {y})");
        }
    }
}

/// A list converts each of its rectangles and reports what each changed; one
/// refused anywhere in it writes none of the others, so a hostile present
/// never half-converts a window.
#[test]
fn a_list_converts_each_part_and_refused_anywhere_writes_nothing() {
    let mode = packed(DisplayFormat::Rgba8888);
    let source = painted();
    let mut frame = frame_for(&mode);
    encode(&source, &mut frame, &mode, whole(), &splitting()).expect("encode");
    let first = DamageRect {
        x: 0,
        y: 0,
        width_px: 4,
        height_px: 2,
    };
    let second = DamageRect {
        x: 6,
        y: 3,
        width_px: 2,
        height_px: 2,
    };
    let mut target = blank();
    let mut reported = Vec::new();
    let both = DamageList::new(&[first, second]).expect("a list");
    decode_list(&frame, &mut target, &mode, &both, &splitting(), |rect| {
        reported.push(rect);
    })
    .expect("decode");
    assert_eq!(reported, [Rect::new(0, 0, 4, 2), Rect::new(6, 3, 2, 2)]);

    let past = DamageRect {
        x: 0,
        y: HEIGHT - 1,
        width_px: WIDTH,
        height_px: 2,
    };
    let refused = DamageList::new(&[first, past]).expect("a list");
    let mut untouched = blank();
    reported.clear();
    assert_eq!(
        decode_list(
            &frame,
            &mut untouched,
            &mode,
            &refused,
            &splitting(),
            |rect| {
                reported.push(rect);
            }
        ),
        Err(Errno::OutOfRange)
    );
    assert_eq!(untouched.pixels(), blank().pixels(), "nothing was written");
    assert!(reported.is_empty());
}

/// Every refusal shape, asserted to leave the target exactly as it was: a
/// hostile geometry must never half-convert a window.
#[test]
fn a_refused_geometry_writes_nothing() {
    let source = painted();
    let good = packed(DisplayFormat::Rgba8888);
    let cases: [(DisplayMode, DamageRect, usize); 4] = [
        // A rectangle reaching past the surface.
        (
            good,
            DamageRect {
                x: 0,
                y: 0,
                width_px: WIDTH + 1,
                height_px: HEIGHT,
            },
            frame_for(&good).len(),
        ),
        // A row span wider than the stride claims.
        (
            mode(DisplayFormat::Rgba8888, WIDTH * 4 - 4),
            whole(),
            frame_for(&good).len(),
        ),
        // A frame too short for the rows named.
        (good, whole(), frame_for(&good).len() - 1),
        // An overflowing origin.
        (
            good,
            DamageRect {
                x: u32::MAX,
                y: 0,
                width_px: 2,
                height_px: 1,
            },
            frame_for(&good).len(),
        ),
    ];
    for (mode, damage, frame_len) in cases {
        let mut frame = vec![0u8; frame_len];
        assert_eq!(
            encode(&source, &mut frame, &mode, damage, &splitting()),
            Err(Errno::OutOfRange)
        );
        assert!(frame.iter().all(|&byte| byte == 0), "a refusal wrote bytes");

        let mut target = blank();
        assert_eq!(
            decode(&frame, &mut target, &mode, damage, &splitting()),
            Err(Errno::OutOfRange)
        );
        assert_eq!(target.pixels(), blank().pixels(), "a refusal wrote pixels");
    }
}
