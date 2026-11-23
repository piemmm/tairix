use alloc::vec::Vec;

use super::testkit::{feed, parse, Collect, Memory};
use super::*;

#[test]
fn closing_ends_each_open_subpath_a_lone_move_too_as_risc_os_did() {
    let input =
        parse("|2 0 0 |8 9000 0 |2 5000 5000 |3 1 1 |8 2 2 |5 |2 7 7 |7 8 8 |2 9 9 |8 10 10 |4");
    let mut out = Collect::default();
    let mut closer = Closer::new(&mut out);
    feed(&mut closer, &mut Memory::default(), &input).expect("closed");
    assert_eq!(closer.last_start, Some(Point::new(9, 9)));
    assert_eq!(
        out.elements,
        parse("|2 0 0 |8 9000 0 |5 |2 5000 5000 |5 |3 1 1 |8 2 2 |5 |2 7 7 |7 8 8 |5 |2 9 9 |8 10 10 |4")
    );
    assert!(out.finished);
}

#[test]
fn a_curve_is_flattened_from_where_the_pen_is() {
    let (c1, c2, to) = (
        Point::new(5000, 9000),
        Point::new(9000, 9000),
        Point::new(9000, 0),
    );
    let input = [
        Element::Move {
            to: Point::new(0, 0),
            winds: true,
        },
        Element::Gap {
            to: Point::new(5000, 0),
        },
        Element::Curve { c1, c2, to },
        Element::Close { gap: false },
    ];
    let threshold = Threshold::of(0);
    let mut expected = Vec::new();
    flatten::flatten::<Stop>(Point::new(5000, 0), c1, c2, to, threshold, |p| {
        expected.push(Element::Line { to: p });
        Ok(())
    })
    .expect("flattened");
    assert!(expected.len() > 1);
    let mut out = Collect::default();
    feed(
        &mut Flattener::new(&mut out, threshold),
        &mut Memory::default(),
        &input,
    )
    .expect("run");
    assert_eq!(&out.elements[..2], &input[..2]);
    assert_eq!(&out.elements[2..out.elements.len() - 1], &expected[..]);
    assert_eq!(out.elements.last(), Some(&Element::Close { gap: false }));
}

#[test]
fn a_negative_flatness_stops_only_at_a_curve() {
    let threshold = Threshold::of(0x2000_0000);
    let line = [
        Element::Move {
            to: Point::new(0, 0),
            winds: true,
        },
        Element::Line {
            to: Point::new(10, 10),
        },
    ];
    let mut out = Collect::default();
    feed(
        &mut Flattener::new(&mut out, threshold),
        &mut Memory::default(),
        &line,
    )
    .expect("lines pass");
    let curve = Element::Curve {
        c1: Point::new(0, 100),
        c2: Point::new(100, 100),
        to: Point::new(100, 0),
    };
    let mut flattener = Flattener::new(&mut out, threshold);
    assert_eq!(
        flattener.element(&mut Memory::default(), curve),
        Err(Stop::TooDeep)
    );
}

#[test]
fn a_point_the_matrix_takes_past_a_word_is_refused_unwritten() {
    let matrix = Matrix {
        a: 0x7FFF_FFFF,
        ..Matrix::IDENTITY
    };
    let mut out = Collect::default();
    let mut transformer = Transformer::new(&mut out, matrix);
    let mut guest = Memory::default();
    transformer
        .element(
            &mut guest,
            Element::Move {
                to: Point::new(1, 2),
                winds: true,
            },
        )
        .expect("in range");
    assert_eq!(
        transformer.element(
            &mut guest,
            Element::Line {
                to: Point::new(1 << 20, 0)
            }
        ),
        Err(Stop::Refused(Refusal::Overflow))
    );
    assert_eq!(out.elements.len(), 1);
}
