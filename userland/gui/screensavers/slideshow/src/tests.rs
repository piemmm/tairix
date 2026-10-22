//! Host tests of the slideshow: its running order, and the slides it asks for
//! and shows.

extern crate std;

use alloc::vec::Vec;

use tairix_abi::saver::SaverFeeds;
use tairix_abi::saver_ipc::{SaverMessage, Start};
use tairix_abi::{Errno, APPINFO_FLAG_FEED_SLIDES};
use tairix_raster::{Color, Surface};
use tairix_saver::testing::{self, Screen};
use tairix_saver::NextFrame;

use super::Slideshow;

const SEC: u64 = 1_000_000_000;

const SIZE: (u32, u32) = (64, 36);

fn start(slides: u16, interval: &str, order: &str) -> Start {
    let feeds = SaverFeeds::from_bits(APPINFO_FLAG_FEED_SLIDES).expect("the slides feed");
    let mut given = testing::start_fed(SIZE.0, SIZE.1, feeds);
    given.slides = slides;
    testing::give(&mut given, "interval", testing::choice(interval));
    testing::give(&mut given, "order", testing::choice(order));
    given
}

fn made(given: &Start, seed: u64) -> Slideshow {
    Slideshow::new(&testing::setup(given, seed)).expect("a slideshow")
}

/// Every slide `show` takes over `count` intervals from nought.
fn taken(show: &mut Slideshow, count: usize) -> Vec<u16> {
    (0..count)
        .map(|step| {
            let at = u64::try_from(step).expect("small") * 10 * SEC;
            show.take_due(at).expect("a slide each interval")
        })
        .collect()
}

#[test]
fn an_ordered_slideshow_goes_round_its_slides() {
    let mut show = made(&start(5, "s10", "sequential"), 0);
    assert_eq!(taken(&mut show, 6), [0, 1, 2, 3, 4, 0]);
}

#[test]
fn a_shuffled_slideshow_shows_each_slide_once_a_pass_and_never_twice_running() {
    for seed in 0..64u64 {
        let mut show = made(&start(5, "s10", "shuffled"), seed);
        let order = taken(&mut show, 5 * 8);
        for pass in order.chunks(5) {
            let mut sorted = pass.to_vec();
            sorted.sort_unstable();
            assert_eq!(sorted, [0, 1, 2, 3, 4], "seed {seed}: {pass:?}");
        }
        assert!(
            order.windows(2).all(|pair| pair[0] != pair[1]),
            "seed {seed} repeated a slide across a pass: {order:?}"
        );
    }
}

/// The first slide is asked for at once and shown the frame after it lands;
/// the next is asked for an interval later, and nothing before.
#[test]
fn a_slide_is_asked_for_shown_when_it_lands_and_followed_an_interval_on() {
    let given = start(3, "s15", "sequential");
    let mut show = made(&given, 0);
    let mut screen = Screen::new(given);
    let (next, asked) = screen.draw(&mut show, 0, 100);
    assert_eq!(asked, [SaverMessage::WantSlide(0)]);
    assert_eq!(
        next,
        NextFrame::Idle,
        "the slide's arrival brings the next frame"
    );

    let mut slide = Surface::new(SIZE.0, SIZE.1).expect("a slide");
    slide.fill(Color::rgb(200, 100, 50));
    screen.slide(&mut show, Some(&slide));
    let (next, asked) = screen.draw(&mut show, 0, 200);
    assert!(asked.is_empty());
    assert_eq!(next, NextFrame::At(100 + 15 * SEC));
    let pixel = screen.surface.get(5, 5).expect("in bounds");
    assert_eq!(
        (pixel.r, pixel.g, pixel.b),
        (200, 100, 50),
        "the slide is shown"
    );
    assert_eq!(screen.damage.bounds().width, SIZE.0);

    let (next, asked) = screen.draw(&mut show, 0, 100 + 15 * SEC - 1);
    assert!(
        asked.is_empty() && screen.damage.is_empty(),
        "nothing before it is due"
    );
    assert_eq!(next, NextFrame::At(100 + 15 * SEC));
    let (_, asked) = screen.draw(&mut show, 0, 100 + 15 * SEC);
    assert_eq!(asked, [SaverMessage::WantSlide(1)]);
}

/// A slide the host could not read leaves the screen as it is, and the next
/// is asked for when it falls due.
#[test]
fn a_slide_the_host_could_not_read_leaves_the_screen_alone() {
    let given = start(3, "s5", "sequential");
    let mut show = made(&given, 0);
    let mut screen = Screen::new(given);
    screen.draw(&mut show, 0, 0);
    screen.slide(&mut show, None);
    let (next, asked) = screen.draw(&mut show, 0, 10);
    assert!(asked.is_empty() && screen.damage.is_empty());
    assert_eq!(next, NextFrame::At(5 * SEC));
}

#[test]
fn a_slideshow_with_no_slides_asks_for_none() {
    let given = start(0, "s30", "shuffled");
    let mut show = made(&given, 0);
    let mut screen = Screen::new(given);
    let (next, asked) = screen.draw(&mut show, 0, u64::MAX);
    assert!(asked.is_empty());
    assert_eq!(next, NextFrame::Idle);
}

/// The slideshow is built only from what the host sends: an option it did
/// not send, or a value the manifest never declared, starts nothing.
#[test]
fn a_slideshow_needs_both_of_its_options_as_declared() {
    let feeds = SaverFeeds::from_bits(APPINFO_FLAG_FEED_SLIDES).expect("the slides feed");
    let mut no_order = testing::start_fed(SIZE.0, SIZE.1, feeds);
    testing::give(&mut no_order, "interval", testing::choice("s5"));
    let refused = |given: &Start| Slideshow::new(&testing::setup(given, 0)).map(|_| ());
    assert_eq!(refused(&no_order), Err(Errno::NotFound));
    assert_eq!(
        refused(&start(1, "s7", "sequential")),
        Err(Errno::OutOfRange)
    );
    assert_eq!(refused(&start(1, "s5", "random")), Err(Errno::OutOfRange));
}
