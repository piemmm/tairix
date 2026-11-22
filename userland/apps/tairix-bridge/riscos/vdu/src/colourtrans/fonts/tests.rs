use super::*;
use crate::colourtrans::palettes::Holding;

/// Sixteen colours from `colour`, each colour number's.
fn sixteen(colour: impl Fn(u8) -> [u8; 3]) -> Against {
    let words: alloc::vec::Vec<u32> = (0..16)
        .map(|i| {
            let [r, g, b] = colour(i);
            Rgb::new(r, g, b).riscos_word()
        })
        .collect();
    Against::of_words(Holding::Palette(2), &words)
}

fn grey(level: u8) -> Rgb {
    Rgb::new(level, level, level)
}

fn run(background: u32, foreground: u32, offset: i32) -> Chosen {
    Chosen::Run {
        background,
        foreground,
        offset,
    }
}

#[test]
fn a_run_reaches_each_colour_lying_within_a_step_of_its_shade() {
    // Each as RISC OS 5 chose it, among the colours of each palette.
    let greys = sixteen(|i| [i * 17; 3]);
    let reds = sixteen(|i| [i * 17, 0, 0]);
    let mixed = sixteen(|i| [i * 17, i * 8, 255 - i * 17]);
    let white = grey(255);
    for (against, background, foreground, chosen) in [
        (&greys, Rgb::BLACK, white, run(0, 1, 14)),
        (&greys, grey(15), white, run(1, 1, 14)),
        (&greys, grey(30), white, run(2, 2, 13)),
        (&greys, grey(75), white, run(4, 5, 10)),
        (&greys, grey(75), Rgb::BLACK, run(4, 4, -4)),
        (&greys, Rgb::BLACK, Rgb::new(255, 0, 0), run(0, 3, 1)),
        (&greys, Rgb::new(255, 0, 0), Rgb::BLACK, run(4, 1, -1)),
        (&reds, Rgb::BLACK, white, run(0, 14, 1)),
        (&reds, grey(15), white, run(1, 15, 0)),
        (&reds, Rgb::BLACK, Rgb::new(255, 0, 0), run(0, 1, 14)),
        (&mixed, Rgb::BLACK, white, run(4, 13, 2)),
        (&mixed, Rgb::BLACK, grey(135), run(4, 11, -1)),
        (&mixed, Rgb::BLACK, grey(195), run(4, 10, 2)),
        (&mixed, grey(150), Rgb::BLACK, run(10, 6, -2)),
    ] {
        assert_eq!(
            choose(against, background, foreground, 14, Loadings::DEFAULT),
            chosen,
            "{background:?} to {foreground:?}"
        );
    }
}

#[test]
fn a_shade_is_cut_toward_minus_infinity_and_a_step_toward_nought() {
    // Exact arithmetic would take this one step; RISC OS 5's takes none.
    let mixed = sixteen(|i| [i * 17, i * 8, 255 - i * 17]);
    let chosen = choose(&mixed, grey(90), grey(255), 14, Loadings::DEFAULT);
    assert_eq!(chosen, run(8, 15, 0));
}

#[test]
fn a_run_is_no_longer_than_fourteen_nor_than_asked() {
    // The fifteen steps below white all fit, but a run stops at fourteen; a
    // run of three's shades lie further apart than these colours, so only
    // one of them fits.
    let even = sixteen(|i| [i * 16 + 15; 3]);
    for (longest, offset) in [
        (14, 14),
        (15, 14),
        (255, 14),
        (i32::MAX, 14),
        (3, 1),
        (0, 0),
        (-1, 0),
        (i32::MIN, 0),
    ] {
        let offset: i32 = offset;
        let chosen = choose(&even, Rgb::BLACK, grey(255), longest, Loadings::DEFAULT);
        assert_eq!(
            chosen,
            run(0, (15 - offset).cast_unsigned(), offset),
            "{longest}"
        );
    }
}

#[test]
fn a_deeper_mode_leaves_the_words() {
    let deep = Against::of_words(Holding::Grouped, &[0; 16]);
    assert_eq!(
        choose(&deep, Rgb::BLACK, grey(255), 14, Loadings::DEFAULT),
        Chosen::Deep
    );
}
