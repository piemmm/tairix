use alloc::vec::Vec;

use tairix_mix::mix64;
use tairix_parallel::{JobRunner, Reversed, Threaded, SERIAL};
use tairix_util::mathf;

use super::*;

/// Dots offered on a lattice, two in three cells offering one jittered
/// within it, each keeping its own room about it; ranked by their keys, or
/// where `ties` by their keys' last four bits, so many neighbours rank alike.
struct Dots {
    seed: u64,
    room: f64,
    reach: u32,
    ties: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Dot {
    cell: Cell,
    at: (f64, f64),
    room: f64,
    key: u64,
}

fn unit(word: u64) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "the top 53 bits, exactly")]
    let bits = (word >> 11) as f64;
    bits / 9_007_199_254_740_992.0
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a test lattice's small coordinates"
)]
fn real(value: i64) -> f64 {
    value as f64
}

impl Offers for Dots {
    type Offer = Dot;
    type Rank = u64;
    type Bound = f64;

    fn reach(&self) -> u32 {
        self.reach
    }

    fn offer(&self, (x, y): Cell) -> Option<Dot> {
        let word = |value: i64| u64::from_le_bytes(value.to_le_bytes());
        let key = mix64(self.seed ^ mix64(word(x) ^ mix64(word(y))));
        (!key.is_multiple_of(3)).then(|| Dot {
            cell: (x, y),
            at: (
                real(x) + unit(mix64(key ^ 1)),
                real(y) + unit(mix64(key ^ 2)),
            ),
            room: self.room * (0.4 + 0.6 * unit(mix64(key ^ 3))),
            key,
        })
    }

    fn rank(&self, dot: &Dot) -> u64 {
        if self.ties {
            dot.key % 16
        } else {
            dot.key
        }
    }

    fn crowds(&self, dot: &Dot, other: &Dot) -> bool {
        let (dx, dy) = (dot.at.0 - other.at.0, dot.at.1 - other.at.1);
        let room = mathf::fmax(dot.room, other.room);
        dx * dx + dy * dy < room * room
    }

    fn bound(&self, dot: &Dot) -> f64 {
        dot.room
    }

    fn join(&self, one: f64, other: f64) -> f64 {
        mathf::fmax(one, other)
    }

    fn within(&self, dot: &Dot, room: f64, cells: u32) -> bool {
        // Each dot lies within its own cell, so two a whole cell fewer apart
        // than a room may crowd.
        f64::from(cells) < mathf::fmax(dot.room, room) + 1.0
    }
}

/// The rows of the rectangle from `first`, `across` columns and rows.
fn rectangle(first: Cell, across: (i64, i64)) -> Vec<Span> {
    (first.1..first.1 + across.1)
        .map(|row| Span {
            row,
            columns: (first.0, first.0 + across.0),
        })
        .collect()
}

fn thinned(dots: &Dots, spans: Vec<Span>, runner: &dyn JobRunner) -> Vec<(Cell, Dot)> {
    thinned_in(dots, (spans, 1), runner)
}

/// What `rounds` rounds of thinning keep of `spans`.
fn thinned_in(
    dots: &Dots,
    (spans, rounds): (Vec<Span>, u32),
    runner: &dyn JobRunner,
) -> Vec<(Cell, Dot)> {
    let mut thinning = Thinning::new(spans, dots.reach, rounds).expect("a thinning");
    let mut kept = Vec::new();
    let mut steps = 0;
    while !thinning.step(dots, runner, &mut kept).expect("thins") {
        steps += 1;
        assert!(steps < 1 << 20, "a thinning that never ends");
    }
    kept
}

/// Whether `other` outranks `dot`: ranked higher, or alike and in a later row
/// or, in the same row, a later column.
fn outranks(dots: &Dots, dot: &Dot, other: &Dot) -> bool {
    let order = |dot: &Dot| (dots.rank(dot), dot.cell.1, dot.cell.0);
    order(other) > order(dot)
}

/// What a brute-force search over every pair keeps of the rectangle.
fn brute(dots: &Dots, first: Cell, across: (i64, i64)) -> Vec<(Cell, Dot)> {
    let reach = i64::from(dots.reach);
    let mut kept = Vec::new();
    for y in first.1..first.1 + across.1 {
        for x in first.0..first.0 + across.0 {
            let Some(dot) = dots.offer((x, y)) else {
                continue;
            };
            let yields = (y - reach..=y + reach).any(|oy| {
                (x - reach..=x + reach).any(|ox| {
                    dots.offer((ox, oy)).is_some_and(|other| {
                        outranks(dots, &dot, &other) && dots.crowds(&dot, &other)
                    })
                })
            });
            if !yields {
                kept.push(((x, y), dot));
            }
        }
    }
    kept
}

fn dots(reach: u32) -> Dots {
    Dots {
        seed: 0x5ca7,
        room: f64::from(reach),
        reach,
        ties: false,
    }
}

/// The rule kept by a brute-force search over every pair within reach:
/// whatever reach, the strips a search passes over hide nothing, and offers
/// ranked alike yield to the later cell.
#[test]
fn a_thinning_keeps_what_a_search_over_every_pair_keeps() {
    for reach in [1, 2, 5] {
        for ties in [false, true] {
            let dots = Dots {
                ties,
                ..dots(reach)
            };
            let (first, across) = ((-17, 9), (41, 23));
            let kept = thinned(&dots, rectangle(first, across), &SERIAL);
            assert_eq!(kept, brute(&dots, first, across), "reach {reach}");
            assert!(!kept.is_empty());
        }
    }
}

/// Of two offers that crowd each other, at most one is kept, and every offer
/// dropped yields to one that outranks and crowds it.
#[test]
fn no_two_kept_offers_crowd_each_other_and_every_dropped_one_yields() {
    let dots = dots(3);
    let (first, across) = ((0, 0), (48, 48));
    let kept = thinned(&dots, rectangle(first, across), &SERIAL);
    assert!(kept.len() > 100, "{} kept", kept.len());
    for (index, (_, dot)) in kept.iter().enumerate() {
        for (_, other) in &kept[index + 1..] {
            assert!(!dots.crowds(dot, other), "{dot:?} and {other:?}");
        }
    }
    let offered = (0..48)
        .flat_map(|y| (0..48).map(move |x| (x, y)))
        .filter(|&cell| dots.offer(cell).is_some())
        .count();
    assert!(kept.len() < offered, "nothing was thinned");
}

/// A region thinned in pieces — rectangles, and rows whose spans widen as a
/// view's do — keeps exactly what it keeps thinned whole within each piece.
#[test]
fn a_region_thinned_in_pieces_keeps_what_it_keeps_whole() {
    let dots = dots(4);
    let whole = thinned(&dots, rectangle((-40, -40), (96, 96)), &SERIAL);
    let within = |spans: &[Span]| {
        whole
            .iter()
            .filter(|((x, y), _)| {
                spans
                    .iter()
                    .any(|span| span.row == *y && (span.columns.0..span.columns.1).contains(x))
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    let cone: Vec<Span> = (0..40)
        .map(|row| Span {
            row,
            columns: (-row / 2, row / 2 + 1),
        })
        .collect();
    for piece in [
        rectangle((-40, -40), (48, 48)),
        rectangle((8, -40), (48, 96)),
        rectangle((-3, 5), (7, 1)),
        cone,
    ] {
        assert_eq!(thinned(&dots, piece.clone(), &SERIAL), within(&piece));
    }
}

/// However many cores share a thinning's rows, and in whatever order they
/// take them, it keeps what one core keeps, in the same order: rows wider
/// than a core's piece and runs of rows narrower.
#[test]
fn a_thinning_shared_among_cores_keeps_what_one_core_keeps() {
    let dots = dots(2);
    for spans in [rectangle((0, 0), (3000, 12)), rectangle((-5, 3), (37, 400))] {
        let alone = thinned(&dots, spans.clone(), &SERIAL);
        assert!(!alone.is_empty());
        for runner in [&Reversed::new(3) as &dyn JobRunner, &Threaded::new(4)] {
            assert_eq!(thinned(&dots, spans.clone(), runner), alone);
        }
    }
}

/// Spans lying past where a thinning reckons them exactly are refused, never
/// thinned into an overflow; and a thinning is refused offers reaching
/// further than it was drawn for, which would keep two that crowd.
#[test]
fn a_thinning_is_refused_spans_it_cannot_reckon_and_offers_it_cannot_hold() {
    let refused = |span: Span| {
        Thinning::<Dot, u64, f64>::new(alloc::vec![span], 1, 1).err() == Some(Error::Shape)
    };
    assert!(refused(Span {
        row: i64::MIN + 1,
        columns: (0, 1),
    }));
    assert!(refused(Span {
        row: 0,
        columns: (-(1 << 62), 1 << 62),
    }));
    assert!(refused(Span {
        row: 0,
        columns: (0, i64::MAX - 1),
    }));
    assert!(!refused(Span {
        row: FARTHEST,
        columns: (-FARTHEST, FARTHEST),
    }));
    let span = Span {
        row: 0,
        columns: (0, 8),
    };
    let mut near = Thinning::<Dot, u64, f64>::new(alloc::vec![span], 1, 1).expect("a thinning");
    let far = Dots {
        reach: 3,
        ..dots(1)
    };
    assert_eq!(near.step(&far, &SERIAL, &mut Vec::new()), Err(Error::Shape));
}

/// A refused step leaves the thinning where its last step left it: refused
/// between every two steps, over three rounds, it keeps what it keeps
/// thinned straight through.
#[test]
fn a_refused_step_leaves_the_thinning_where_it_was() {
    let near = dots(2);
    let far = Dots {
        reach: 5,
        ..dots(2)
    };
    let spans = rectangle((0, 0), (64, 64));
    let straight = thinned_in(&near, (spans.clone(), 3), &SERIAL);
    let mut thinning = Thinning::new(spans, near.reach, 3).expect("a thinning");
    let (mut kept, mut steps) = (Vec::new(), 0);
    loop {
        assert_eq!(thinning.step(&far, &SERIAL, &mut kept), Err(Error::Shape));
        steps += 1;
        if thinning.step(&near, &SERIAL, &mut kept).expect("thins") {
            break;
        }
    }
    assert!(steps > 2, "{steps}");
    assert_eq!(kept, straight);
}

/// Rows out of order, and a row that with its reach runs off the lattice,
/// are refused; no rows at all are thinned at once.
#[test]
fn a_thinning_is_refused_rows_it_cannot_hold() {
    let span = |row: i64| Span {
        row,
        columns: (0, 4),
    };
    assert_eq!(
        Thinning::<Dot, u64, f64>::new(alloc::vec![span(3), span(2)], 1, 1).err(),
        Some(Error::Shape)
    );
    assert_eq!(
        Thinning::<Dot, u64, f64>::new(alloc::vec![span(3), span(3)], 1, 1).err(),
        Some(Error::Shape)
    );
    let edge = Span {
        row: 0,
        columns: (i64::MIN, 0),
    };
    assert_eq!(
        Thinning::<Dot, u64, f64>::new(alloc::vec![edge], 1, 1).err(),
        Some(Error::Shape)
    );
    let mut empty = Thinning::<Dot, u64, f64>::new(Vec::new(), 1, 1).expect("a thinning");
    let mut kept = Vec::new();
    assert_eq!(empty.step(&dots(1), &SERIAL, &mut kept), Ok(true));
    assert!(kept.is_empty());
    assert!((empty.done() - 1.0).abs() < f64::EPSILON);
    for rounds in [0, MOST_ROUNDS + 1] {
        assert_eq!(
            Thinning::<Dot, u64, f64>::new(alloc::vec![span(3)], 1, rounds).err(),
            Some(Error::Shape)
        );
    }
}

/// What thinning the offers of the square `square` one by one from the
/// highest ranked keeps — each kept unless one kept before it crowds it —
/// within the cells `within` of it.
fn one_by_one(dots: &Dots, square: (Cell, i64), within: (Cell, (i64, i64))) -> Vec<(Cell, Dot)> {
    let ((x0, y0), side) = square;
    let mut offered: Vec<Dot> = (y0..y0 + side)
        .flat_map(|y| (x0..x0 + side).map(move |x| (x, y)))
        .filter_map(|cell| dots.offer(cell))
        .collect();
    offered.sort_by(|a, b| {
        let order = |dot: &Dot| (dots.rank(dot), dot.cell.1, dot.cell.0);
        order(b).cmp(&order(a))
    });
    let mut kept: Vec<Dot> = Vec::new();
    for dot in offered {
        if !kept.iter().any(|other| dots.crowds(&dot, other)) {
            kept.push(dot);
        }
    }
    let ((wx, wy), (across, down)) = within;
    let mut within: Vec<(Cell, Dot)> = kept
        .into_iter()
        .filter(|dot| {
            (wx..wx + across).contains(&dot.cell.0) && (wy..wy + down).contains(&dot.cell.1)
        })
        .map(|dot| (dot.cell, dot))
        .collect();
    within.sort_by_key(|(cell, _)| (cell.1, cell.0));
    within
}

/// Round by round a thinning keeps more of what thinning one by one from the
/// highest ranked keeps, and nothing else, until enough rounds keep all of
/// it: far enough inside a wider square that what lies beyond it makes no
/// difference.
#[test]
fn rounds_keep_more_of_what_thinning_one_by_one_keeps_until_they_keep_it_all() {
    for ties in [false, true] {
        let dots = Dots { ties, ..dots(2) };
        let window = ((-6, -6), (12, 12));
        // Wide enough to hold every offer sixteen rounds can reach.
        let truth = one_by_one(&dots, ((-72, -72), 144), window);
        let mut last = 0;
        for rounds in [1, 2, 3, 4, 8, 16] {
            let kept = thinned_in(&dots, (rectangle(window.0, window.1), rounds), &SERIAL);
            assert!(
                kept.iter().all(|found| truth.contains(found)),
                "{rounds} rounds keep only what one by one keeps"
            );
            assert!(kept.len() >= last, "{rounds} rounds keep no less");
            last = kept.len();
        }
        let enough = thinned_in(&dots, (rectangle(window.0, window.1), 16), &SERIAL);
        assert_eq!(enough, truth, "ties {ties}");
        let once = thinned_in(&dots, (rectangle(window.0, window.1), 1), &SERIAL);
        assert!(once.len() < truth.len(), "one round keeps less");
    }
}

/// Over rounds, a region thinned in pieces keeps what it keeps thinned whole,
/// however many cores share it and in whatever order.
#[test]
fn rounds_keep_alike_in_pieces_and_across_cores() {
    let dots = dots(2);
    let whole = thinned_in(&dots, (rectangle((-40, -40), (96, 96)), 3), &SERIAL);
    let cone: Vec<Span> = (0..40)
        .map(|row| Span {
            row,
            columns: (-row / 2, row / 2 + 1),
        })
        .collect();
    for piece in [
        rectangle((-40, -40), (48, 48)),
        rectangle((8, -40), (48, 96)),
        cone,
    ] {
        let within: Vec<(Cell, Dot)> = whole
            .iter()
            .filter(|((x, y), _)| {
                piece
                    .iter()
                    .any(|span| span.row == *y && (span.columns.0..span.columns.1).contains(x))
            })
            .cloned()
            .collect();
        assert_eq!(thinned_in(&dots, (piece.clone(), 3), &SERIAL), within);
        for runner in [&Reversed::new(3) as &dyn JobRunner, &Threaded::new(4)] {
            assert_eq!(thinned_in(&dots, (piece.clone(), 3), runner), within);
        }
    }
}
