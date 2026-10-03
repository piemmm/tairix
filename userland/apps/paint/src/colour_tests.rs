use alloc::vec;
use alloc::vec::Vec;

use tairix_image::IndexDepth;

use super::{nearest, Ink, Nearest, WHITE};
use crate::canvas::{Kind, Sample};

fn indexed(palette: Vec<[u8; 4]>, masked: bool) -> Kind {
    Kind::Indexed {
        depth: IndexDepth::Eight,
        palette,
        masked,
    }
}

#[test]
fn the_nearest_entry_wins_and_a_tie_goes_to_the_lowest() {
    let palette = [[0, 0, 0, 255], [10, 10, 10, 255], [10, 10, 10, 255]];
    assert_eq!(nearest(&palette, [9, 9, 9, 255]), 1);
    assert_eq!(nearest(&palette, [1, 1, 1, 255]), 0);
    assert_eq!(nearest(&[], [1, 1, 1, 255]), 0);
}

/// A generator of spread-out colours, deterministic so a failure repeats.
fn colours(count: usize, seed: u32) -> Vec<[u8; 4]> {
    let mut state = seed;
    (0..count)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let bytes = state.to_le_bytes();
            [
                bytes[0],
                bytes[1],
                bytes[2],
                if bytes[3] > 200 { bytes[3] } else { 255 },
            ]
        })
        .collect()
}

#[test]
fn the_ordered_search_answers_exactly_as_the_plain_one() {
    for (size, seed) in [(1, 3), (2, 5), (16, 7), (200, 11), (256, 13)] {
        let palette = colours(size, seed);
        let mut search = Nearest::new(&palette).expect("room");
        for colour in colours(2000, seed + 1) {
            assert_eq!(
                search.find(colour),
                nearest(&palette, colour),
                "{colour:?} in {size}"
            );
        }
    }
}

#[test]
fn duplicate_entries_are_answered_by_the_lowest_index() {
    let palette = vec![
        [5, 5, 5, 255],
        [7, 7, 7, 255],
        [5, 5, 5, 255],
        [7, 7, 7, 255],
    ];
    let mut search = Nearest::new(&palette).expect("room");
    assert_eq!(search.find([5, 5, 5, 255]), 0);
    assert_eq!(search.find([7, 7, 7, 255]), 1);
}

#[test]
fn an_ink_moves_to_the_nearest_a_palette_can_put_down() {
    let palette = vec![[0, 0, 0, 255], [250, 250, 250, 255], [200, 0, 0, 255]];
    let masked = indexed(palette.clone(), true);
    let plain = indexed(palette, false);
    let red = Ink::Colour([255, 10, 10, 255]);
    assert_eq!(red.adapted(&Kind::Rgba, &plain), Ink::Index(2));
    assert_eq!(Ink::Clear.adapted(&Kind::Rgba, &masked), Ink::Clear);
    assert_eq!(
        Ink::Clear.adapted(&Kind::Rgba, &plain),
        Ink::Index(1),
        "nothing on a picture that cannot be cleared is the paper, white"
    );
    assert_eq!(
        Ink::Colour([10, 10, 10, 20]).adapted(&Kind::Rgba, &masked),
        Ink::Clear
    );
    assert_eq!(
        Ink::Index(2).adapted(&plain, &Kind::Rgba),
        Ink::Colour([200, 0, 0, 255])
    );
    assert_eq!(Ink::Index(2).adapted(&plain, &plain), Ink::Index(2));
    assert_eq!(Ink::Index(1).shown(&plain), [250, 250, 250, 255]);
    assert_eq!(Ink::Clear.shown(&plain), [0; 4]);
    assert_eq!(WHITE, [255; 4]);
}

#[test]
fn a_picked_pixel_is_the_ink_that_would_put_it_back() {
    let masked = indexed(vec![[0, 0, 0, 255]; 2], true);
    assert_eq!(Ink::of_sample(Sample::Index(1, 0), &masked), Ink::Clear);
    assert_eq!(
        Ink::of_sample(Sample::Index(1, 255), &masked),
        Ink::Index(1)
    );
    assert_eq!(
        Ink::of_sample(Sample::Rgba([1, 2, 3, 0]), &Kind::Rgba),
        Ink::Clear
    );
    assert_eq!(
        Ink::of_sample(Sample::Rgba([1, 2, 3, 9]), &Kind::Rgba),
        Ink::Colour([1, 2, 3, 9])
    );
}

/// A colour with no alpha laid over would change nothing, so the ink for it
/// clears; any alpha at all is a colour.
#[test]
fn a_colour_with_no_alpha_is_the_clear_ink() {
    assert_eq!(Ink::of_colour([9, 8, 7, 0]), Ink::Clear);
    assert_eq!(Ink::of_colour([9, 8, 7, 1]), Ink::Colour([9, 8, 7, 1]));
    assert_eq!(Ink::of_colour([0, 0, 0, 255]), Ink::Colour([0, 0, 0, 255]));
}

/// An ink naming a palette entry still names it once that entry's colour is
/// edited, or a mask added: it moves by colour only when the depth changes.
#[test]
fn an_index_ink_keeps_its_entry_through_a_palette_edit() {
    let before = indexed(
        vec![[0, 0, 0, 255], [255, 0, 0, 255], [80, 80, 80, 255]],
        false,
    );
    let edited = indexed(
        vec![[0, 0, 0, 255], [0, 0, 255, 255], [80, 80, 80, 255]],
        true,
    );
    assert_eq!(Ink::Index(1).adapted(&before, &edited), Ink::Index(1));
    let shallower = Kind::Indexed {
        depth: IndexDepth::Four,
        palette: vec![[0, 0, 0, 255], [80, 80, 80, 255], [255, 0, 0, 255]],
        masked: false,
    };
    assert_eq!(
        Ink::Index(1).adapted(&before, &shallower),
        Ink::Index(2),
        "a new depth's entries are found again by colour"
    );
}
