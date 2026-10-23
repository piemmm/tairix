//! Surfaces drawing into memory they were lent.

extern crate std;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tairix_reclaim::CachedBytes;

use super::PixelMemory;
use crate::color::{Color, Pixel};
use crate::surface::Surface;

const RED: Color = Color::rgb(255, 0, 0);
const RED_BYTES: [u8; 4] = [255, 0, 0, 255];
const WIDTH: u32 = 4;
const HEIGHT: u32 = 2;
const LEN: usize = (WIDTH * HEIGHT * 4) as usize;

/// Memory a test lends a surface: its bytes are handed back when the surface
/// lets it go, and it answers none once `shrunk` is set.
struct Lent {
    bytes: Vec<u8>,
    returned: Arc<Mutex<Vec<u8>>>,
    shrunk: Arc<AtomicBool>,
}

impl PixelMemory for Lent {
    fn bytes_mut(&mut self) -> &mut [u8] {
        if self.shrunk.load(Ordering::Relaxed) {
            &mut []
        } else {
            &mut self.bytes
        }
    }
}

impl Drop for Lent {
    fn drop(&mut self) {
        *self.returned.lock().expect("unpoisoned") = core::mem::take(&mut self.bytes);
    }
}

struct Lender {
    returned: Arc<Mutex<Vec<u8>>>,
    shrunk: Arc<AtomicBool>,
}

impl Lender {
    fn new() -> Self {
        Self {
            returned: Arc::new(Mutex::new(Vec::new())),
            shrunk: Arc::new(AtomicBool::new(false)),
        }
    }

    fn lend(&self, bytes: Vec<u8>) -> Box<dyn PixelMemory> {
        Box::new(Lent {
            bytes,
            returned: Arc::clone(&self.returned),
            shrunk: Arc::clone(&self.shrunk),
        })
    }

    fn returned(&self) -> Vec<u8> {
        self.returned.lock().expect("unpoisoned").clone()
    }
}

fn pixel_at(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * WIDTH + x) * 4) as usize;
    bytes[at..at + 4].try_into().expect("a pixel")
}

#[test]
fn a_lent_surface_draws_into_the_lenders_memory() {
    let lender = Lender::new();
    let mut surface = Surface::lent(WIDTH, HEIGHT, lender.lend(vec![0x11; LEN])).expect("fits");
    surface.fill_rect(1, 0, 2, 2, RED);
    drop(surface);
    let bytes = lender.returned();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let want = if (1..3).contains(&x) {
                RED_BYTES
            } else {
                [0x11; 4]
            };
            assert_eq!(pixel_at(&bytes, x, y), want, "pixel ({x}, {y})");
        }
    }
}

#[test]
fn a_lent_surface_takes_its_pixels_as_they_stand() {
    let lender = Lender::new();
    let mut bytes = vec![0; LEN];
    bytes[4..8].copy_from_slice(&[9, 8, 7, 200]);
    let surface = Surface::lent(WIDTH, HEIGHT, lender.lend(bytes)).expect("fits");
    assert_eq!(
        surface.get(1, 0),
        Some(Pixel {
            r: 9,
            g: 8,
            b: 7,
            a: 200
        })
    );
    assert_eq!(surface.get(0, 0), Some(Pixel::TRANSPARENT));
}

#[test]
fn memory_shorter_than_the_pixels_is_refused() {
    let lender = Lender::new();
    assert!(Surface::lent(WIDTH, HEIGHT, lender.lend(vec![0; LEN - 1])).is_none());
    assert!(Surface::lent(WIDTH, HEIGHT, lender.lend(vec![0; LEN + 3])).is_some());
}

#[test]
fn a_copy_of_a_lent_surface_owns_its_pixels() {
    let lender = Lender::new();
    let surface = Surface::lent(WIDTH, HEIGHT, lender.lend(vec![0; LEN])).expect("fits");
    let mut copy = surface.clone();
    copy.fill(RED);
    assert_eq!(surface.payload_bytes(), 0, "lent memory is not the heap's");
    assert_eq!(copy.payload_bytes(), LEN);
    drop(surface);
    assert!(lender.returned().iter().all(|byte| *byte == 0));
}

#[test]
fn a_lent_surface_reshaped_is_given_a_buffer_of_its_own() {
    let lender = Lender::new();
    let surface = Surface::lent(WIDTH, HEIGHT, lender.lend(vec![5; LEN])).expect("fits");
    let mut reshaped = surface.reshaped(2, 2).expect("reshapes");
    assert_eq!(
        lender.returned(),
        vec![5; LEN],
        "the memory goes back untouched"
    );
    reshaped.fill(RED);
    assert_eq!(reshaped.payload_bytes(), 16);
    assert_eq!(reshaped.get(1, 1), Some(RED.premultiply()));
}

#[test]
fn lent_and_owned_surfaces_holding_the_same_pixels_are_equal() {
    let lender = Lender::new();
    let mut lent = Surface::lent(WIDTH, HEIGHT, lender.lend(vec![0; LEN])).expect("fits");
    let mut owned = Surface::new(WIDTH, HEIGHT).expect("a surface");
    lent.fill_rect(0, 1, 3, 1, RED);
    owned.fill_rect(0, 1, 3, 1, RED);
    assert_eq!(lent, owned);
}

/// A lender is asked for its memory once, as the surface is built: what it
/// would answer later reaches nothing, and memory short of the geometry then
/// is refused and handed back.
#[test]
fn memory_is_asked_for_once_as_the_surface_is_built() {
    let lender = Lender::new();
    let mut surface = Surface::lent(WIDTH, HEIGHT, lender.lend(vec![0; LEN])).expect("fits");
    lender.shrunk.store(true, Ordering::Relaxed);
    surface.fill(RED);
    assert_eq!(surface.get(WIDTH - 1, HEIGHT - 1), Some(RED.premultiply()));
    drop(surface);
    assert!(lender
        .returned()
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| *pixel == RED_BYTES));

    let short = Lender::new();
    short.shrunk.store(true, Ordering::Relaxed);
    assert!(Surface::lent(WIDTH, HEIGHT, short.lend(vec![0; LEN])).is_none());
    assert_eq!(short.returned(), vec![0; LEN], "handed back");
}

#[test]
fn a_lent_surface_splits_into_row_bands() {
    let lender = Lender::new();
    let mut surface = Surface::lent(WIDTH, HEIGHT, lender.lend(vec![0; LEN])).expect("fits");
    let mut bands = 0;
    for mut band in surface.row_bands_mut(0..HEIGHT, 1) {
        bands += 1;
        let row = band.rows().start;
        if let Some((_, span)) = band.row_span_mut(row, 0, WIDTH) {
            span.fill(RED.premultiply());
        }
    }
    assert_eq!(bands, 2);
    drop(surface);
    assert!(lender
        .returned()
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| *pixel == RED_BYTES));
}

/// A lent surface's fallible copy is the surface, in memory of its own: what
/// the lender's memory holds afterwards is not the copy's to change.
#[test]
fn a_lent_surfaces_copy_is_its_own() {
    let lender = Lender::new();
    let mut surface = Surface::lent(WIDTH, HEIGHT, lender.lend(vec![0; LEN])).expect("fits");
    surface.fill(RED);
    let mut copy = surface.try_clone().expect("a copy");
    assert_eq!(copy, surface);
    assert!(copy.payload_bytes() > 0, "held on the heap, not lent");
    copy.fill(Color::rgb(0, 0, 255));
    assert_eq!(
        surface.get(0, 0),
        Some(RED.premultiply()),
        "the lent pixels stand"
    );
}
