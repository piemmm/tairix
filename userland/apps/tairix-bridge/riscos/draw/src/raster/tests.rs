use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use super::*;

#[path = "recorded.rs"]
mod recorded;

/// A scan's steps, none refused.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a scan's step, which its host may refuse"
)]
fn unstopped() -> Result<(), NoRoom> {
    Ok(())
}

/// A square from pixel (1, 1) to pixel (10, 10), drawn anticlockwise.
fn square() -> [Segment; 4] {
    [
        ((256, 256), (2560, 256)),
        ((2560, 256), (2560, 2560)),
        ((2560, 2560), (256, 2560)),
        ((256, 2560), (256, 256)),
    ]
    .map(|(from, to)| Segment {
        from,
        to,
        boundary: true,
        winds: true,
    })
}

/// Each pixel of a `width` by `height` window's class, as a recording
/// writes it.
fn classes_of(segments: &[Segment], rule: Rule, width: i32, height: i32) -> Vec<Vec<char>> {
    let window = Bounds {
        left: 0,
        bottom: 0,
        right: width - 1,
        top: height - 1,
    };
    let index = |v: i32| usize::try_from(v).expect("within the window");
    let mut grid = vec![vec!['?'; index(width)]; index(height)];
    for (bit, mark) in [
        (class::EXTERIOR, '.'),
        (class::EXTERIOR_BOUNDARY, 'o'),
        (class::INTERIOR_BOUNDARY, '#'),
        (class::INTERIOR, '@'),
    ] {
        scan(
            segments,
            rule,
            bit,
            window,
            &mut Scratch::default(),
            unstopped,
            |row, from, to| {
                for x in from..=to {
                    let cell = &mut grid[index(height - 1 - row)][index(x)];
                    assert_eq!(*cell, '?', "a pixel in two classes at ({x}, {row})");
                    *cell = mark;
                }
            },
        )
        .expect("room");
    }
    grid
}

fn segments_of(case: &recorded::Case) -> Vec<Segment> {
    case.segments
        .iter()
        .map(|&(from, to, boundary, winds)| Segment {
            from,
            to,
            boundary,
            winds,
        })
        .collect()
}

#[test]
fn every_recorded_box_is_classed_as_genuine_risc_os_classed_it() {
    let mut wrong = Vec::new();
    for case in recorded::CASES {
        let (width, height) = case.size;
        let grid = classes_of(&segments_of(case), Rule::of(case.rule), width, height);
        let ours: Vec<String> = grid
            .iter()
            .map(|row| {
                row.iter()
                    .map(|&c| if case.stroke && c != '.' { 'o' } else { c })
                    .collect()
            })
            .collect();
        let genuine: Vec<String> = case.rows.iter().map(|r| (*r).to_string()).collect();
        if ours != genuine {
            wrong.push(format!(
                "{}:\nours\n{}\ngenuine\n{}",
                case.name,
                ours.join("\n"),
                genuine.join("\n")
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
}

#[test]
fn a_window_bounds_the_rows_and_columns_classed() {
    let square = square();
    let window = Bounds {
        left: 3,
        bottom: 4,
        right: 5,
        top: 6,
    };
    let mut seen = Vec::new();
    scan(
        &square,
        Rule::NonZero,
        class::ALL,
        window,
        &mut Scratch::default(),
        unstopped,
        |row, from, to| {
            seen.push((row, from, to));
        },
    )
    .expect("room");
    assert_eq!(seen, [(4, 3, 5), (5, 3, 5), (6, 3, 5)]);
}

#[test]
fn a_segment_far_outside_the_window_costs_only_its_rows() {
    let far = Segment {
        from: (-(1 << 40), 300),
        to: (1 << 40, 900),
        boundary: true,
        winds: false,
    };
    let window = Bounds {
        left: 0,
        bottom: 0,
        right: 9,
        top: 9,
    };
    let mut rows = Vec::new();
    scan(
        &[far],
        Rule::NonZero,
        class::EXTERIOR_BOUNDARY,
        window,
        &mut Scratch::default(),
        unstopped,
        |row, from, to| {
            rows.push((row, from, to));
        },
    )
    .expect("room");
    assert_eq!(
        rows.len(),
        1,
        "a nearly level line crosses one row: {rows:?}"
    );
    assert_eq!((rows[0].1, rows[0].2), (0, 9));
}

#[test]
fn an_empty_style_or_window_plots_nothing() {
    let edge = Segment {
        from: (0, 0),
        to: (2560, 2560),
        boundary: true,
        winds: true,
    };
    let window = Bounds {
        left: 0,
        bottom: 0,
        right: 9,
        top: 9,
    };
    let mut any = false;
    scan(
        &[edge],
        Rule::NonZero,
        0,
        window,
        &mut Scratch::default(),
        unstopped,
        |_, _, _| any = true,
    )
    .expect("room");
    let inverted = Bounds {
        left: 9,
        bottom: 0,
        right: 0,
        top: 9,
    };
    scan(
        &[edge],
        Rule::NonZero,
        class::ALL,
        inverted,
        &mut Scratch::default(),
        unstopped,
        |_, _, _| any = true,
    )
    .expect("room");
    assert!(!any);
}

#[test]
fn only_the_exterior_class_scans_the_rows_no_segment_reaches() {
    let window = Bounds {
        left: 0,
        bottom: 0,
        right: 99,
        top: 999,
    };
    let rows_scanned = |classes| {
        let mut rows = 0;
        scan(
            &square(),
            Rule::NonZero,
            classes,
            window,
            &mut Scratch::default(),
            || {
                rows += 1;
                unstopped()
            },
            |_, _, _| {},
        )
        .expect("room");
        rows
    };
    assert!(
        rows_scanned(class::INTERIOR | class::INTERIOR_BOUNDARY | class::EXTERIOR_BOUNDARY) <= 12
    );
    assert_eq!(rows_scanned(class::ALL), 1000);
}

#[test]
fn a_scan_stops_at_the_step_its_host_refuses() {
    let window = Bounds {
        left: 0,
        bottom: 0,
        right: 99,
        top: 99,
    };
    let mut left = 3;
    let mut spans = 0;
    let stopped = scan(
        &square(),
        Rule::NonZero,
        class::ALL,
        window,
        &mut Scratch::default(),
        || {
            left -= 1;
            if left == 0 {
                Err(NoRoom)
            } else {
                Ok(())
            }
        },
        |_, _, _| spans += 1,
    );
    assert_eq!(stopped, Err(NoRoom));
    assert_eq!(spans, 2, "a span for each row scanned before the refusal");
}

#[test]
fn a_scratch_kept_from_one_scan_gives_the_next_what_a_fresh_one_would() {
    let window = Bounds {
        left: 0,
        bottom: 0,
        right: 63,
        top: 63,
    };
    let collect = |segments: &[Segment], scratch: &mut Scratch| {
        let mut spans = Vec::new();
        scan(
            segments,
            Rule::NonZero,
            class::ALL,
            window,
            scratch,
            unstopped,
            |row, from, to| {
                spans.push((row, from, to));
            },
        )
        .expect("room");
        spans
    };
    let mut kept = Scratch::default();
    let wide = [((0, 0), (20_000, 9000))].map(|(from, to)| Segment {
        from,
        to,
        boundary: true,
        winds: true,
    });
    let _ = collect(&wide, &mut kept);
    assert_eq!(
        collect(&square(), &mut kept),
        collect(&square(), &mut Scratch::default())
    );
}
