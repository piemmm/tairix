//! Bands of whole columns: how a block of rows is cut, that a band writes what
//! the surface would and nothing another band owns, under a clip window and a
//! stated origin, from several threads at once, and over lent memory.

extern crate std;

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::color::{Color, Pixel};
use crate::lent::PixelMemory;
use crate::surface::Surface;

/// A distinct opaque pixel for band `band`.
fn ink(band: usize) -> Pixel {
    let shade = u8::try_from(band % 250).expect("small") + 1;
    Color::rgb(shade, 0, 0).premultiply()
}

/// Every row of `band` filled across every column it owns with `pixel`.
fn fill_band(band: &mut super::ColumnBand<'_>, pixel: Pixel) {
    let (rows, columns) = (band.rows(), band.columns());
    for y in rows {
        if let Some((_, span)) = band.row_span_mut(y, columns.start, columns.end - columns.start) {
            span.fill(pixel);
        }
    }
}

#[test]
fn a_block_is_cut_into_whole_columns_the_last_band_short() {
    let mut surface = Surface::new(100, 10).expect("a surface");
    let bands: Vec<_> = surface
        .column_bands_mut(2..9, 0..100, 30)
        .map(|band| (band.columns(), band.rows()))
        .collect();
    assert_eq!(
        bands,
        [
            (0..30, 2..9),
            (30..60, 2..9),
            (60..90, 2..9),
            (90..100, 2..9)
        ]
    );
    assert_eq!(surface.column_bands_mut(0..10, 0..100, 0).len(), 100);
    assert_eq!(surface.column_bands_mut(5..5, 0..100, 8).len(), 0);
    assert_eq!(surface.column_bands_mut(0..10, 40..40, 8).len(), 0);
    assert_eq!(
        surface
            .column_bands_mut(4..40, 90..400, 8)
            .map(|band| (band.columns(), band.rows()))
            .collect::<Vec<_>>(),
        [(90..98, 4..10), (98..100, 4..10)],
        "what lies off the surface is dropped"
    );
}

/// Each band writes exactly the pixels of its own columns a write through the
/// surface would, whatever span it is asked for, and none beyond them.
#[test]
fn a_band_writes_what_the_surface_would_within_its_columns() {
    let (width, height) = (37, 9);
    let mut banded = Surface::new(width, height).expect("a surface");
    let mut whole = Surface::new(width, height).expect("a surface");
    for (index, mut band) in banded.column_bands_mut(1..8, 3..35, 7).enumerate() {
        let columns = band.columns();
        for y in band.rows() {
            for (x, w) in [(0, width), (columns.start + 1, 3), (columns.end - 1, 9)] {
                if let Some((first, span)) = band.row_span_mut(y, x, w) {
                    assert!(columns.contains(&first), "{first} outside {columns:?}");
                    assert!(first as usize + span.len() <= columns.end as usize);
                    span.fill(ink(index));
                }
            }
        }
    }
    for (index, start) in (3..35).step_by(7).enumerate() {
        let columns = start..(start + 7).min(35);
        for y in 1..8 {
            for x in columns.clone() {
                whole.set(x, y, ink(index));
            }
        }
    }
    assert_eq!(banded, whole);
    assert!(banded.column_bands_mut(1..8, 3..35, 7).all(|mut band| {
        band.row_span_mut(0, 3, 30).is_none() && band.row_span_mut(8, 3, 30).is_none()
    }));
}

/// A band admits only what the clip window and the stated origin admit, so
/// a band of a clipped paint is the same picture as the clipped paint.
#[test]
fn a_band_honours_the_clip_window_and_the_origin() {
    let mut banded = Surface::new(40, 12).expect("a surface");
    let mut whole = Surface::new(40, 12).expect("a surface");
    banded.with_origin(100, 50, |surface| {
        surface.with_clip(105, 52, 20, 6, |surface| {
            for (index, mut band) in surface.column_bands_mut(50..62, 100..140, 6).enumerate() {
                assert!(band.columns().start >= 105 && band.columns().end <= 125);
                assert_eq!(band.rows(), 52..58);
                fill_band(&mut band, ink(index));
            }
        });
    });
    for (index, start) in (5..25).step_by(6).enumerate() {
        for y in 2..8 {
            for x in start..(start + 6).min(25) {
                whole.set(x, y, ink(index));
            }
        }
    }
    assert_eq!(banded, whole);
}

/// Bands filled each on a thread of its own leave the same picture as the
/// bands filled in turn: no band reaches a pixel another owns.
#[test]
fn bands_filled_at_once_on_several_threads_leave_each_its_own_columns() {
    let (width, height) = (64, 16);
    let mut threaded = Surface::new(width, height).expect("a surface");
    let mut serial = Surface::new(width, height).expect("a surface");
    std::thread::scope(|scope| {
        for (index, mut band) in threaded
            .column_bands_mut(0..height, 0..width, 5)
            .enumerate()
        {
            scope.spawn(move || fill_band(&mut band, ink(index)));
        }
    });
    for (index, mut band) in serial.column_bands_mut(0..height, 0..width, 5).enumerate() {
        fill_band(&mut band, ink(index));
    }
    assert_eq!(threaded, serial);
    for x in 0..width {
        let owner = usize::try_from(x / 5).expect("small");
        assert!((0..height).all(|y| threaded.get(x, y) == Some(ink(owner))));
    }
}

/// Memory a surface was lent, written through bands.
struct Frame(Vec<u8>);

impl PixelMemory for Frame {
    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

#[test]
fn bands_of_a_lent_surface_write_into_its_memory() {
    let (width, height) = (12u32, 3u32);
    let memory = Box::new(Frame(alloc::vec![0; (width * height * 4) as usize]));
    let mut lent = Surface::lent(width, height, memory).expect("a lent surface");
    let mut owned = Surface::new(width, height).expect("a surface");
    for target in [&mut lent, &mut owned] {
        for (index, mut band) in target.column_bands_mut(0..height, 0..width, 4).enumerate() {
            fill_band(&mut band, ink(index));
        }
    }
    assert_eq!(lent.pixels(), owned.pixels());
}
