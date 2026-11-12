use super::*;
use alloc::vec;

#[test]
fn the_picture_is_the_largest_four_by_three_the_window_holds_centred() {
    assert_eq!(
        fit(1280, 960),
        Some(Region {
            x: 0,
            y: 0,
            width: 1280,
            height: 960
        })
    );
    assert_eq!(
        fit(1920, 1080),
        Some(Region {
            x: 240,
            y: 0,
            width: 1440,
            height: 1080
        }),
        "pillars either side of a wide screen"
    );
    assert_eq!(
        fit(800, 900),
        Some(Region {
            x: 0,
            y: 150,
            width: 800,
            height: 600
        }),
        "bands above and below a tall one"
    );
    assert_eq!(fit(1, 1), None, "too small for a picture");
    assert_eq!(fit(0, 600), None);
}

#[test]
fn a_window_point_is_the_frames_dot_under_it() {
    let target = Region {
        x: 100,
        y: 50,
        width: 1280,
        height: 960,
    };
    let frame = (640, 200);
    assert_eq!(frame_point(target, frame, (100, 50)), (0, 0));
    assert_eq!(
        frame_point(target, frame, (100 + 1279, 50 + 959)),
        (639, 199)
    );
    assert_eq!(
        frame_point(target, frame, (100 + 640, 50 + 480)),
        (320, 100)
    );
    assert_eq!(
        frame_point(target, frame, (-5_000, i32::MAX)),
        (0, 199),
        "outside, its nearest edge"
    );
}

fn frame(width: u32, height: u32, level: impl Fn(u32) -> u8) -> Vec<u8> {
    (0..height)
        .flat_map(|y| {
            let l = level(y);
            (0..width).flat_map(move |_| [l, l, l, 255])
        })
        .collect()
}

#[test]
fn a_draw_redraws_only_the_rows_a_change_reaches_in_a_target_it_drew() {
    let mut screen = Screen::new();
    let mut surface = Surface::new(20, 10).unwrap();
    let target = Region {
        x: 2,
        y: 1,
        width: 16,
        height: 8,
    };
    assert_eq!(screen.draw(&mut surface, target), Ok(0..0), "no frame yet");
    assert!(screen.take(2, 2, &frame(2, 2, |y| [40, 200][y as usize])));
    assert_eq!(
        screen.draw(&mut surface, target),
        Ok(0..8),
        "a new target, whole"
    );
    assert_eq!(surface.get(2, 1).unwrap().r, 40);
    assert_eq!(surface.get(17, 8).unwrap().r, 200);
    assert_eq!(
        surface.get(1, 1).unwrap().a,
        0,
        "outside the target untouched"
    );
    assert!(
        !screen.take(2, 2, &frame(2, 2, |y| [40, 200][y as usize])),
        "the same frame"
    );
    assert_eq!(screen.draw(&mut surface, target), Ok(0..0), "nothing owed");
    assert!(screen.take(2, 2, &frame(2, 2, |y| [40, 90][y as usize])));
    assert_eq!(
        screen.draw(&mut surface, target),
        Ok(4..8),
        "the lower row's block"
    );
    assert_eq!(surface.get(17, 8).unwrap().r, 90);
    assert!(screen.take(2, 2, &frame(2, 2, |y| [41, 90][y as usize])));
    assert!(screen.take(2, 2, &frame(2, 2, |y| [41, 91][y as usize])));
    assert_eq!(
        screen.draw(&mut surface, target),
        Ok(0..8),
        "two frames' changes together"
    );
    let wider = Region {
        width: 18,
        ..target
    };
    assert!(!screen.take(2, 2, &frame(2, 2, |y| [41, 91][y as usize])));
    assert_eq!(
        screen.draw(&mut surface, wider),
        Ok(0..8),
        "a resized target, whole"
    );
    screen.invalidate();
    assert_eq!(
        screen.draw(&mut surface, wider),
        Ok(0..8),
        "and after its surface was lost"
    );
    assert!(screen.take(4, 2, &frame(4, 2, |_| 5)));
    assert_eq!(
        screen.draw(&mut surface, wider),
        Ok(0..8),
        "a frame of a new size, whole"
    );
}

#[test]
fn a_frame_that_is_not_its_size_changes_nothing() {
    let mut screen = Screen::new();
    assert!(!screen.take(4, 6, &[0; 4 * 6 * 4 - 1]));
    assert!(!screen.take(0, 6, &[]));
    assert!(!screen.take(u32::MAX, u32::MAX, &[]));
    assert_eq!(screen.size(), (0, 0), "and kept from the screen");
}

#[test]
fn a_thumbnail_is_the_frame_at_the_monitors_shape_on_black() {
    // A white 640x200 frame, as a CGA's, on a card wider than it is 4:3.
    let white = vec![0xFF; 640 * 200 * PIXEL];
    let size = (200, 120);
    let picture = thumbnail(640, 200, &white, size).unwrap();
    assert_eq!(picture.len(), 200 * 120 * PIXEL);
    let at = |x: usize, y: usize| &picture[(y * 200 + x) * PIXEL..][..PIXEL];
    assert_eq!(
        at(0, 60),
        [0, 0, 0, 0xFF],
        "black beside the 160x120 picture"
    );
    assert_eq!(at(19, 60), [0, 0, 0, 0xFF]);
    assert_eq!(at(20, 0), [0xFF; 4], "the frame stretched to 4:3");
    assert_eq!(at(179, 119), [0xFF; 4]);
    assert_eq!(at(180, 60), [0, 0, 0, 0xFF]);
    assert_eq!(
        thumbnail(640, 200, &white[1..], size),
        None,
        "pixels not the frame's"
    );
    assert_eq!(
        thumbnail(640, 200, &white, (1, 1)),
        None,
        "too small for the shape"
    );
}

#[test]
fn a_screenshot_is_the_monitors_shape_at_least_vga_wide_in_whole_dots() {
    let mut frame = vec![0u8; 320 * 200 * PIXEL];
    for pixel in frame.as_chunks_mut::<PIXEL>().0.iter_mut().take(320) {
        *pixel = [0xFF, 0x80, 0x00, 0xFF];
    }
    let (w, h, shot) = screenshot(320, 200, &frame).unwrap();
    assert_eq!((w, h), (640, 480), "two pixels a dot across, 4:3");
    assert_eq!(shot.len(), 640 * 480 * PIXEL);
    assert_eq!(
        &shot[..PIXEL],
        &[0xFF, 0x80, 0x00, 0xFF],
        "the top row kept its colour"
    );
    let last = shot.len() - PIXEL;
    assert_eq!(
        &shot[last..],
        &[0, 0, 0, 0xFF],
        "and the rest is the black it was"
    );
    assert_eq!(
        screenshot(720, 350, &vec![0; 720 * 350 * PIXEL]).map(|(w, h, _)| (w, h)),
        Some((720, 540))
    );
    assert_eq!(
        screenshot(320, 200, &frame[1..]),
        None,
        "pixels not the frame's"
    );
}

#[test]
fn a_screen_is_kept_once_it_has_a_frame() {
    let mut screen = Screen::new();
    assert_eq!(screen.last(), None, "nothing shown yet");
    let pixels = vec![9u8; 4 * 3 * PIXEL];
    assert!(screen.take(4, 3, &pixels));
    assert_eq!(
        screen.last(),
        Some(LastScreen {
            width: 4,
            height: 3,
            pixels
        })
    );
}
