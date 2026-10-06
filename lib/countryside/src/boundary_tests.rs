use super::*;
use crate::holding::HoldingId;

const KEY: Key = Key::new(99);

fn field(index: u32) -> Side {
    Side::Field(FieldId {
        holding: HoldingId::new(0, 0),
        index,
    })
}

#[test]
fn a_run_ends_where_what_lies_beside_its_line_changes() {
    let line = [Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(10.0, 10.0)];
    let sides = |at: Point, _: Point| {
        if at.x < 3.3 {
            (field(0), Side::Water)
        } else if at.y < 6.1 {
            (field(1), field(2))
        } else {
            (Side::Out, field(2))
        }
    };
    let keeps = |left: Side, right: Side| matches!((left, right), (Side::Field(_), Side::Field(_)));
    let found = runs(&line, &sides, &keeps).expect("room");
    assert_eq!(found.len(), 1);
    let run = &found[0];
    assert_eq!((run.left, run.right), (field(1), field(2)));
    let (first, last) = (run.line[0], run.line[run.line.len() - 1]);
    assert!((first - Point::new(3.3, 0.0)).length() < 0.1, "{first:?}");
    assert!((last - Point::new(10.0, 6.1)).length() < 0.1, "{last:?}");
    assert!(run.line.contains(&Point::new(10.0, 0.0)), "it keeps the corner it turns");
}

#[test]
fn a_boundary_too_short_to_matter_is_not_kept() {
    let line = [Point::new(0.0, 0.0), Point::new(20.0, 0.0)];
    let sides = |at: Point, _: Point| {
        if (5.0..6.0).contains(&at.x) {
            (field(0), field(1))
        } else {
            (Side::Water, field(1))
        }
    };
    let found = runs(&line, &sides, &|left, _| matches!(left, Side::Field(_))).expect("room");
    assert!(found.is_empty());
}

#[test]
fn stony_ground_is_walled_and_wet_ground_ditched() {
    let style = Style {
        hedge: 1.0,
        wall: 1.0,
        fence: 0.5,
        ditch: 1.0,
        open: 0.1,
    };
    let count = |lie: Lie, kind: Kind| {
        (0..2000)
            .filter(|&place| {
                let mut draws = KEY.draws(Stage::Boundary, (place, 7));
                let custom = style.custom(lie, &mut draws);
                style.kind(lie, custom, &mut draws) == kind
            })
            .count()
    };
    let stony = Lie {
        stony: 1.0,
        ..Lie::default()
    };
    let wet = Lie {
        wet: 1.0,
        fertile: 0.5,
        ..Lie::default()
    };
    let deep = Lie {
        fertile: 1.0,
        ..Lie::default()
    };
    assert!(count(stony, Kind::Wall) > 1400);
    assert!(count(wet, Kind::Ditch) > 1000);
    assert!(count(deep, Kind::Hedge) > count(deep, Kind::Wall) * 3);
}

#[test]
fn a_gap_overlapping_one_hung_before_it_is_left_out_and_gaps_end_in_order() {
    let mut boundaries = [Boundary {
        id: BoundaryId {
            owner: Owner::Yard(HoldingId::new(0, 0)),
            run: 0,
        },
        kind: Kind::Wall,
        line: alloc::vec![Point::new(0.0, 0.0), Point::new(60.0, 0.0)],
        left: Side::Plot,
        right: field(0),
        gaps: Vec::new(),
        key: 5,
    }];
    let gap = |along: f64, through: Through| Gap {
        along,
        width: if through == Through::Path { 1.0 } else { 4.0 },
        through,
        key: 0,
    };
    let gaps = alloc::vec![
        (0, gap(40.0, Through::Path)),
        (0, gap(20.0, Through::Path)),
        (0, gap(21.0, Through::Gateway)),
        (0, gap(48.0, Through::Gateway)),
    ];
    hang(&mut boundaries, gaps).expect("room");
    let hung: Vec<(f64, Through)> = boundaries[0].gaps.iter().map(|gap| (gap.along, gap.through)).collect();
    assert_eq!(hung, [(21.0, Through::Gateway), (40.0, Through::Path), (48.0, Through::Gateway)]);
}

#[test]
fn a_path_crosses_a_boundary_by_a_stile_where_their_lines_cross() {
    let boundary = Boundary {
        id: BoundaryId {
            owner: Owner::Cut(HoldingId::new(0, 0), 1),
            run: 0,
        },
        kind: Kind::Hedge,
        line: alloc::vec![Point::new(0.0, 0.0), Point::new(0.0, 50.0)],
        left: field(0),
        right: field(1),
        gaps: Vec::new(),
        key: 9,
    };
    let path = Line {
        stations: [Point::new(-20.0, 0.0), Point::new(20.0, 30.0)]
            .into_iter()
            .map(|at| crate::route::Station {
                at,
                level: 0.0,
                width: 0.9,
                water: None,
            })
            .collect(),
    };
    let bounds = path.bounds(0.0).expect("stations");
    let stiles = stiles(KEY, (3, &boundary), &[(bounds, &path)]).expect("room");
    assert_eq!(stiles.len(), 1);
    assert_eq!(stiles[0].0, 3);
    assert!((stiles[0].1.along - 15.0).abs() < 1e-9);
    assert_eq!(stiles[0].1.through, Through::Path);
}
