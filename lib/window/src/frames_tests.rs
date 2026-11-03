//! Unit tests for [`draw_frame`]: a window's frames, each drawn only through
//! what its staleness hands it, hold the whole picture whenever they are
//! presented.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::display::{DamageList, DamageRect, DisplayFormat, DisplayMode};
use tairix_abi::Errno;
use tairix_display::{stale_frames, winframe, SERIAL};
use tairix_geometry::Region;
use tairix_raster::{Color, Surface};

use super::{draw_frame, region_bytes};

const MODE: DisplayMode = DisplayMode {
    width_px: 12,
    height_px: 9,
    stride_bytes: 48,
    format: DisplayFormat::Bgra8888,
};

const FRAMES: usize = 2;

/// The bytes one frame holds when it shows all of `surface`.
fn whole(surface: &Surface) -> Vec<u8> {
    let mut frame = vec![0u8; region_bytes(&MODE, 1).expect("a frame")];
    winframe::encode(surface, &mut frame, &MODE, DamageRect::full(&MODE), &SERIAL)
        .expect("the whole frame encodes");
    frame
}

fn one(rect: DamageRect) -> DamageList {
    DamageList::new(&[rect]).expect("a non-empty rectangle")
}

/// Frame `index` of `pixels`.
fn frame(pixels: &[u8], index: usize) -> &[u8] {
    let len = region_bytes(&MODE, 1).expect("a frame");
    &pixels[index * len..(index + 1) * len]
}

#[test]
fn alternate_frames_each_hold_the_whole_picture_once_drawn() {
    let mut pixels = vec![0xEEu8; region_bytes(&MODE, 2).expect("a region")];
    let mut surface = Surface::new(MODE.width_px, MODE.height_px).expect("a surface");
    let mut stale = stale_frames(FRAMES, &MODE);
    let mut changed = Region::new();
    let mut rng = tairix_fuzzseed::Prng::new(0xF2A3_E5D0_0B1E_5EED);
    for present in 0..64u32 {
        let draw = rng.next_u64();
        let x = u32::try_from(draw % 12).unwrap_or(0);
        let y = u32::try_from((draw >> 8) % 9).unwrap_or(0);
        let rect = DamageRect {
            x,
            y,
            width_px: 1 + u32::try_from((draw >> 16) % u64::from(MODE.width_px - x)).unwrap_or(0),
            height_px: 1 + u32::try_from((draw >> 24) % u64::from(MODE.height_px - y)).unwrap_or(0),
        };
        let shade = u8::try_from(present % 251).unwrap_or(0);
        surface.fill_rect(
            rect.x,
            rect.y,
            rect.width_px,
            rect.height_px,
            Color::rgb(shade, shade ^ 0x5A, 0x80),
        );
        let index = usize::try_from(present).unwrap_or(0) % FRAMES;
        draw_frame(
            &mut pixels,
            index,
            (&mut stale, &mut changed),
            &surface,
            &MODE,
            &one(rect),
        )
        .expect("drawn");
        assert_eq!(
            frame(&pixels, index),
            whole(&surface).as_slice(),
            "frame {index} after present {present}"
        );
    }
}

#[test]
fn a_frame_past_the_region_is_refused_and_nothing_is_written() {
    let mut pixels = vec![0u8; region_bytes(&MODE, 2).expect("a region")];
    let surface = Surface::new(MODE.width_px, MODE.height_px).expect("a surface");
    let mut stale = stale_frames(FRAMES, &MODE);
    let mut changed = Region::new();
    assert_eq!(
        draw_frame(
            &mut pixels,
            FRAMES,
            (&mut stale, &mut changed),
            &surface,
            &MODE,
            &one(DamageRect::full(&MODE)),
        ),
        Err(Errno::OutOfRange)
    );
    assert!(pixels.iter().all(|&byte| byte == 0));
}

#[test]
fn a_refused_draw_leaves_the_frame_owing_everything_it_lacked() {
    let mut pixels = vec![0u8; region_bytes(&MODE, 2).expect("a region")];
    let mut stale = stale_frames(FRAMES, &MODE);
    let mut changed = Region::new();
    let small = Surface::new(MODE.width_px - 1, MODE.height_px).expect("a smaller surface");
    let corner = one(DamageRect {
        x: 0,
        y: 0,
        width_px: 1,
        height_px: 1,
    });
    assert!(
        draw_frame(
            &mut pixels,
            0,
            (&mut stale, &mut changed),
            &small,
            &MODE,
            &corner
        )
        .is_err(),
        "a surface the frame cannot be drawn from is refused"
    );
    let surface = Surface::filled(
        MODE.width_px,
        MODE.height_px,
        Color::rgb(9, 99, 199).premultiply(),
    )
    .expect("a surface");
    draw_frame(
        &mut pixels,
        0,
        (&mut stale, &mut changed),
        &surface,
        &MODE,
        &corner,
    )
    .expect("drawn");
    assert_eq!(
        frame(&pixels, 0),
        whole(&surface).as_slice(),
        "the frame was still owed the whole picture"
    );
}
