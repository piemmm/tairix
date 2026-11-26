//! The scene drawn on the software device is the scene stated, restated here
//! pixel by pixel, at every extent and in both frame formats.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::time::MonotonicClock;
use tairix_fuzzseed::meter::{metered, refusing_above};
use tairix_gpu::{Error, Format};

use super::{rendered, Layout, Order, TILE};

/// A clock that never moves.
struct Still;

impl MonotonicClock for Still {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// The scene at `(width, height)` as stated: an opaque blue ground; a band
/// from an eighth of the way down, 64 rows or to the bottom, red rising across
/// it and green down it over half blue; a row of 32-pixel tiles halfway down,
/// 96 pixels apart from 32 in, of 4-pixel checks, while they fit; and the
/// first tile copied to 8 pixels in from the far corner where the two stand
/// apart.
fn stated((width, height): (u32, u32), bgra: bool) -> Vec<u8> {
    let colour = |[r, g, b]: [u8; 3]| {
        if bgra {
            [b, g, r, 0xFF]
        } else {
            [r, g, b, 0xFF]
        }
    };
    let (w, h) = (
        usize::try_from(width).expect("a width"),
        usize::try_from(height).expect("a height"),
    );
    let mut picture = vec![colour([0, 0, 0xFF]); w * h];
    let top = h / 8;
    let rows = 64.min(h - top);
    for row in 0..rows {
        for x in 0..w {
            let red = x * 255 / (w - 1).max(1);
            let green = row * 255 / (rows - 1).max(1);
            let level = |value: usize| u8::try_from(value).expect("a level");
            picture[(top + row) * w + x] = colour([level(red), level(green), 0x80]);
        }
    }
    let check = |x: usize, y: usize| {
        if (x / 4 + y / 4) % 2 == 1 {
            [0x20, 0xC0, 0xF0]
        } else {
            [0xF0, 0x40, 0x20]
        }
    };
    let mut tiles = Vec::new();
    for at in 0..8 {
        let (x, y) = (32 + at * 96, h / 2);
        if x + 32 > w || y + 32 > h {
            break;
        }
        tiles.push((x, y));
        for ty in 0..32 {
            for tx in 0..32 {
                picture[(y + ty) * w + x + tx] = colour(check(tx, ty));
            }
        }
    }
    if let Some(&(x, y)) = tiles.first() {
        if w >= 40 && h >= 40 {
            let (to_x, to_y) = (w - 40, h - 40);
            if to_x >= x + 32 || to_y >= y + 32 {
                for ty in 0..32 {
                    for tx in 0..32 {
                        picture[(to_y + ty) * w + to_x + tx] = picture[(y + ty) * w + x + tx];
                    }
                }
            }
        }
    }
    picture.into_iter().flatten().collect()
}

#[test]
fn the_scene_drawn_is_the_scene_stated_at_every_extent_and_in_both_formats() {
    for extent in [
        (1024, 768),
        (300, 200),
        (97, 51),
        (40, 30),
        (1, 1),
        (130, 9),
    ] {
        for (format, bgra) in [(Format::Bgra8Unorm, true), (Format::Rgba8Unorm, false)] {
            let drawn = rendered(extent, format, Arc::new(Still)).expect("drawn");
            assert!(
                drawn == stated(extent, bgra),
                "{extent:?} {format:?}: the scene drawn is not the scene stated"
            );
        }
    }
}

#[test]
fn a_frame_format_of_other_channels_is_refused() {
    assert_eq!(
        rendered((8, 8), Format::R8Unorm, Arc::new(Still)).err(),
        Some(Error::Unsupported)
    );
}

/// The copies' source is reserved whole before it is written, so writing it
/// never grows it, and the tile starts where its offset says.
#[test]
fn the_source_reserved_is_what_the_band_and_the_tile_take() {
    for extent in [(1024, 768), (97, 51), (40, 30), (1, 1), (130, 9)] {
        let layout = Layout::of(extent);
        let (bytes, metering) = metered(|| layout.source(Order::Bgra));
        let bytes = bytes.expect("reserved");
        assert_eq!(metering.allocations, 1, "{extent:?}: grown while written");
        assert_eq!(
            u64::try_from(bytes.len()).expect("small"),
            layout.tile_offset() + u64::from(TILE * TILE) * 4,
            "{extent:?}"
        );
    }
}

/// A host that cannot hold the copies' source refuses it as a value, never an
/// abort.
#[test]
fn a_source_the_host_cannot_hold_is_out_of_memory() {
    let layout = Layout::of((1024, 768));
    assert_eq!(
        refusing_above(0, || layout.source(Order::Bgra)).err(),
        Some(Error::OutOfMemory)
    );
}

/// A host that cannot hold the picture read back refuses it before the
/// device draws anything.
#[test]
fn a_picture_the_host_cannot_hold_is_refused_before_it_is_drawn() {
    let clock = Arc::new(Still);
    let (answer, metering) = metered(move || {
        refusing_above(0, move || rendered((1024, 768), Format::Bgra8Unorm, clock))
    });
    assert_eq!(answer.err(), Some(Error::OutOfMemory));
    assert_eq!(
        metering.allocations, 1,
        "only the picture's room is asked for"
    );
}
