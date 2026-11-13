use super::*;
use alloc::vec;

#[test]
fn a_machine_window_opens_to_show_a_vga_screen_at_one_pixel_a_dot() {
    for bar in [0, 26, 40] {
        let (width, height) = opening_extent(bar);
        assert_eq!(
            fit(width, height - bar),
            Some(Region {
                x: 0,
                y: 0,
                width: 640,
                height: 480
            }),
            "over a {bar}-pixel bar"
        );
    }
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
    assert_eq!(last_screen(&screen), None, "nothing shown yet");
    let pixels = vec![9u8; 4 * 3 * PIXEL];
    assert!(screen.take(4, 3, &pixels));
    assert_eq!(
        last_screen(&screen),
        Some(LastScreen {
            width: 4,
            height: 3,
            pixels
        })
    );
}
