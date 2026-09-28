//! Host tests of the slideshow's running order.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Duration64;
use tairix_wallpaper::{SlideOrder, SlideSource, SlideshowOptions, WallpaperCategory};
use tairix_window::WallpaperName;

use super::Slides;

const SEC: u64 = 1_000_000_000;

fn catalog() -> Vec<WallpaperName> {
    [
        ("Abstract", "a.jpg"),
        ("Nature", "n0.jpg"),
        ("Nature", "n1.jpg"),
        ("Space", "s.jpg"),
        ("Nature", "n2.jpg"),
    ]
    .iter()
    .map(|(category, file)| WallpaperName {
        category: String::from(*category),
        file: String::from(*file),
    })
    .collect()
}

fn options(order: SlideOrder, category: Option<&str>) -> SlideshowOptions {
    SlideshowOptions {
        interval: Duration64::from_secs(10),
        order,
        source: category
            .and_then(WallpaperCategory::new)
            .map_or(SlideSource::Every, SlideSource::Category),
    }
}

/// Every position a slideshow shows over `passes` whole passes, stepping one
/// interval at a time from `start`.
fn shown(slides: &mut Slides, start: u64, count: usize) -> Vec<usize> {
    (0..count)
        .map(|step| {
            let at = start + u64::try_from(step).expect("small") * 10 * SEC;
            slides.take_due(at).expect("a picture is due each interval")
        })
        .collect()
}

#[test]
fn an_ordered_slideshow_shows_the_catalog_round_and_round_one_interval_apart() {
    let mut slides = Slides::new(&catalog(), &options(SlideOrder::Sequential, None), 100)
        .expect("a running order");
    assert_eq!(slides.due_ns(), Some(100), "the first is shown at once");
    assert_eq!(slides.take_due(100), Some(0));
    assert_eq!(slides.take_due(100), None, "the next waits its interval");
    assert_eq!(slides.due_ns(), Some(100 + 10 * SEC));
    assert_eq!(shown(&mut slides, 100 + 10 * SEC, 5), [1, 2, 3, 4, 0]);
}

#[test]
fn a_category_narrows_the_pictures_to_its_own() {
    let mut slides = Slides::new(
        &catalog(),
        &options(SlideOrder::Sequential, Some("Nature")),
        0,
    )
    .expect("a running order");
    assert_eq!(shown(&mut slides, 0, 4), [1, 2, 4, 1]);
}

/// A choice an update left behind still shows pictures rather than nothing.
#[test]
fn a_category_the_store_no_longer_holds_shows_every_picture() {
    let mut slides = Slides::new(
        &catalog(),
        &options(SlideOrder::Sequential, Some("Gone")),
        0,
    )
    .expect("a running order");
    assert_eq!(shown(&mut slides, 0, 5), [0, 1, 2, 3, 4]);
}

#[test]
fn a_shuffled_slideshow_shows_each_picture_once_a_pass_and_never_twice_running() {
    for seed in 0..64u64 {
        let mut slides = Slides::new(&catalog(), &options(SlideOrder::Shuffled, None), seed)
            .expect("a running order");
        let order = shown(&mut slides, seed, 5 * 8);
        for pass in order.chunks(5) {
            let mut sorted = pass.to_vec();
            sorted.sort_unstable();
            assert_eq!(sorted, [0, 1, 2, 3, 4], "seed {seed}: {pass:?}");
        }
        assert!(
            order.windows(2).all(|pair| pair[0] != pair[1]),
            "seed {seed} repeated a picture across a pass: {order:?}"
        );
    }
}

#[test]
fn an_empty_catalog_asks_for_no_picture() {
    let mut slides =
        Slides::new(&[], &options(SlideOrder::Shuffled, None), 0).expect("an empty running order");
    assert_eq!(slides.due_ns(), None);
    assert_eq!(slides.take_due(u64::MAX), None);
}
