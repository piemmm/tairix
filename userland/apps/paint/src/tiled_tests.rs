use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use tairix_image::IndexDepth;
use tairix_parallel::{Reversed, SERIAL};

use super::{block_side, complete, run, tiles_for, ROUND, WIDE_BLOCK};
use crate::canvas::{Canvas, Kind, Sample, Tile, TILE};
use crate::compose::between;
use crate::document::{Document, Picture};
use crate::effect::{patterned, pixels, Distribution, Noise, Window};
use crate::mask::Mask;
use crate::op::{Halted, Op, Work};
use crate::shape::Bounds;
use crate::tone::{
    Channel, ChannelMixer, ColourBalance, Curves, HueRange, HueRanges, Levels, WhiteBalance,
};

/// Every operation, each set to change something, an effect reaching past a
/// tile among them so the wide blocks are cut too.
fn every_operation() -> Vec<Op> {
    let mut levels = Levels::IDENTITY;
    levels.of_mut(Channel::Composite).white = 200;
    let mut curves = Curves::IDENTITY;
    let _ = curves.of_mut(Channel::Red).set(1, (255, 90));
    let mut hues = HueRanges::IDENTITY;
    hues.of_mut(HueRange::Master).hue = 60;
    let mut balance = ColourBalance::NEUTRAL;
    balance.tones[1][0] = 40;
    let mut mixer = ChannelMixer::IDENTITY;
    mixer.rows[0] = [50, 50, 0, 10];
    let noise = Noise {
        amount: 40,
        distribution: Distribution::Gaussian,
        monochrome: false,
        seed: 9,
    };
    vec![
        Op::BrightnessContrast {
            brightness: 20,
            contrast: 30,
        },
        Op::Levels(levels),
        Op::Curves(curves),
        Op::HueSaturation(hues),
        Op::ColourBalance(balance),
        Op::WhiteBalance(WhiteBalance {
            kelvin: 4000,
            tint: 10,
        }),
        Op::Desaturate,
        Op::Invert,
        Op::Posterise { levels: 5 },
        Op::Threshold { level: 100 },
        Op::ChannelMixer(mixer),
        Op::GaussianBlur { radius: 3 },
        Op::GaussianBlur { radius: 25 },
        Op::UnsharpMask {
            amount: 150,
            radius: 2,
            threshold: 4,
        },
        Op::Median { radius: 2 },
        Op::AddNoise(noise),
        Op::AddNoise(Noise {
            monochrome: true,
            ..Noise::START
        }),
        Op::FindEdges,
        Op::Emboss {
            angle: 45,
            height: 2,
            amount: 150,
        },
        Op::Pixelate { cell: 9 },
    ]
}

/// What `op` makes of `canvas` in one window over the whole picture, held to
/// `clip` as a run holds it: what a run cut into blocks must match to the bit.
fn whole(canvas: &Canvas, op: &Op, clip: Option<&Mask>) -> Vec<[u8; 4]> {
    let bounds = Bounds::picture(canvas.width(), canvas.height());
    let before = pixels(canvas);
    let made: Vec<[u8; 4]> = match op.work() {
        Work::Tone(map) => before.iter().map(|&colour| map.map(colour)).collect(),
        Work::Effect(effect) => {
            let window = Window::read(canvas, bounds).expect("room");
            effect.run(&window, bounds).expect("room")
        }
    };
    let width = i64::from(canvas.width());
    let mut shown = Vec::new();
    for (at, (&colour, changed)) in before.iter().zip(made).enumerate() {
        let at = i64::try_from(at).expect("a place");
        let share = clip.map_or(u8::MAX, |clip| clip.at(at % width, at / width));
        shown.push(match share {
            0 => colour,
            u8::MAX => changed,
            share => between(colour, changed, share),
        });
    }
    shown
}

/// `canvas` with `tiles` in place, as a run's answer is adopted.
fn laid(canvas: &Canvas, tiles: Vec<(usize, Arc<Tile>)>) -> Vec<[u8; 4]> {
    let mut shown = canvas.try_clone().expect("room");
    for (index, tile) in tiles {
        assert!(shown.fits(index, &tile), "tile {index} fits where it lands");
        shown.replace_tile(index, tile);
    }
    pixels(&shown)
}

#[test]
fn every_operation_is_the_same_to_the_bit_whole_cut_into_blocks_and_run_backwards() {
    let canvas = patterned(330, 200);
    let picture = Bounds::picture(330, 200);
    let corner = Bounds {
        x0: 37,
        y0: 21,
        x1: 251,
        y1: 170,
    };
    let selection = Mask::rect(corner)
        .and_then(|mask| mask.feathered(9, picture).ok().flatten())
        .expect("a soft selection");
    let backwards = Reversed::new(4);
    for op in every_operation() {
        for clip in [None, Some(&selection)] {
            let tiles = tiles_for(&canvas, clip).expect("room");
            let reference = whole(&canvas, &op, clip);
            let forward = run(&canvas, &op, clip, &tiles, &SERIAL, &|| true).expect("room");
            assert_eq!(laid(&canvas, forward), reference, "{op:?} in blocks");
            let reversed: Vec<usize> = tiles.iter().rev().copied().collect();
            let backward = run(&canvas, &op, clip, &reversed, &backwards, &|| true);
            let backward = backward.expect("room");
            assert_eq!(laid(&canvas, backward), reference, "{op:?} backwards");
        }
    }
    assert!(backwards.widest() > 1, "the work was spread");
}

#[test]
fn a_run_no_longer_wanted_stops_between_blocks() {
    let canvas = patterned(330, 200);
    let tiles = tiles_for(&canvas, None).expect("room");
    let asked = AtomicUsize::new(0);
    let wanted = || asked.fetch_add(1, Ordering::Relaxed) < 2;
    let halted = run(&canvas, &Op::Invert, None, &tiles, &SERIAL, &wanted);
    assert_eq!(halted.err(), Some(Halted::Abandoned));
    let all = run(&canvas, &Op::Invert, None, &tiles, &SERIAL, &|| true).expect("room");
    assert_eq!(
        all.len(),
        tiles.len(),
        "wanted throughout, every tile is answered"
    );
}

/// A run stops at the round in which a block halted, rather than working out
/// the rest of the picture before it answers.
#[test]
fn a_run_stops_at_the_round_a_block_halted_in() {
    let canvas = patterned(330, 200);
    let tiles = tiles_for(&canvas, None).expect("room");
    let asked = AtomicUsize::new(0);
    // Asked before the first round, then before each of its blocks, the
    // first of which halts.
    let wanted = || asked.fetch_add(1, Ordering::Relaxed) != 1;
    let halted = run(&canvas, &Op::Invert, None, &tiles, &SERIAL, &wanted);
    assert_eq!(halted.err(), Some(Halted::Abandoned));
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1 + ROUND,
        "the first round's blocks, and no later round's"
    );
}

#[test]
fn a_palette_picture_takes_no_run() {
    let kind = Kind::Indexed {
        depth: IndexDepth::One,
        palette: vec![[0, 0, 0, 255], [255, 255, 255, 255]],
        masked: false,
    };
    let canvas = Canvas::new(8, 8, kind, Sample::Index(1, u8::MAX)).expect("fits");
    let tiles = tiles_for(&canvas, None).expect("room");
    let halted = run(&canvas, &Op::Invert, None, &tiles, &SERIAL, &|| true);
    assert_eq!(halted.err(), Some(Halted::NeedsColour));
}

#[test]
fn a_run_writes_the_tiles_its_selection_reaches_and_shares_what_it_leaves() {
    let canvas = patterned(330, 200);
    let corner = Bounds {
        x0: 10,
        y0: 5,
        x1: 70,
        y1: 20,
    };
    let clip = Mask::rect(corner).expect("a selection");
    assert_eq!(tiles_for(&canvas, Some(&clip)).expect("room"), [0, 1]);
    let tiles = tiles_for(&canvas, None).expect("room");
    assert_eq!(tiles.len(), canvas.tile_count());
    let unchanged = Op::Levels(Levels::IDENTITY);
    let made = run(&canvas, &unchanged, None, &tiles, &SERIAL, &|| true).expect("room");
    let shared = made
        .iter()
        .all(|(index, tile)| Arc::ptr_eq(tile, canvas.tile(*index)));
    assert!(shared, "a tile left as it was is the very tile");
}

#[test]
fn an_incomplete_preview_is_completed_as_the_whole_run_would_be() {
    let canvas = patterned(330, 200);
    let op = Op::Median { radius: 1 };
    let tiles = tiles_for(&canvas, None).expect("room");
    let (first, _) = tiles.split_at(tiles.len() / 2);
    let done = run(&canvas, &op, None, first, &SERIAL, &|| true).expect("room");
    let completed = complete(&canvas, &op, None, done, &SERIAL).expect("room");
    assert_eq!(completed.len(), tiles.len());
    assert_eq!(laid(&canvas, completed), whole(&canvas, &op, None));
}

#[test]
fn applying_is_one_step_and_undoing_it_puts_back_every_bit() {
    let canvas = patterned(330, 200);
    let before = pixels(&canvas);
    let mut document = Document::new(Picture::plain(canvas));
    let shown = |document: &Document| pixels(document.picture().expect("a picture").canvas());
    let source = document.picture().expect("a picture").canvas().try_clone();
    let source = source.expect("room");
    let op = Op::GaussianBlur { radius: 4 };
    let tiles = tiles_for(&source, None).expect("room");
    let made = run(&source, &op, None, &tiles, &SERIAL, &|| true).expect("room");
    assert_eq!(document.adopt_tiles(0, made), Ok(true));
    let applied = shown(&document);
    assert_ne!(applied, before);
    assert_eq!(applied, whole(&source, &op, None));
    assert!(document.undo().is_ok());
    assert_eq!(shown(&document), before, "every bit put back");
    assert!(document.undo().is_err(), "one step, and only one");
    assert!(document.redo().is_ok());
    assert_eq!(shown(&document), applied);
}

/// A block's side follows its effect's reach, so what a block reads about the
/// tiles it writes is never more than two and a half times what it writes,
/// and no window it reads outgrows the widest block and the furthest reach.
#[test]
fn a_blocks_side_follows_its_reach_and_bounds_what_it_reads() {
    let furthest = Op::EFFECTS
        .iter()
        .map(|op| {
            let mut most = *op;
            for (index, parameter) in op.parameters().iter().enumerate() {
                most.set(index, parameter.most);
            }
            match most.work() {
                Work::Effect(effect) => effect.reach(),
                Work::Tone(_) => 0,
            }
        })
        .max()
        .expect("effects");
    let mut widest = 0;
    for reach in 0..=furthest {
        let side = i64::from(block_side(reach) * TILE);
        let read = side + 2 * i64::from(reach);
        assert!(
            2 * read * read <= 5 * side * side,
            "reach {reach}: {read} read about {side} written"
        );
        widest = widest.max(read);
    }
    assert!(widest <= i64::from(WIDE_BLOCK * TILE) + 2 * i64::from(furthest));
    assert!(
        block_side(furthest) > block_side(TILE),
        "a far reach is cut wider"
    );
}
