use super::{handles, set_out, Grab, Side};
use crate::shape::Bounds;

const PICTURE: Bounds = Bounds {
    x0: 0,
    y0: 0,
    x1: 100,
    y1: 80,
};

const BOX: Bounds = Bounds {
    x0: 20,
    y0: 20,
    x1: 60,
    y1: 50,
};

#[test]
fn a_press_takes_an_edge_a_corner_the_middle_or_nothing() {
    let reach = 4;
    assert_eq!(
        Grab::of(BOX, (21, 35), reach),
        Some(Grab::Edges {
            across: Side::Near,
            down: Side::Neither
        })
    );
    assert_eq!(
        Grab::of(BOX, (58, 48), reach),
        Some(Grab::Edges {
            across: Side::Far,
            down: Side::Far
        }),
        "a corner"
    );
    assert_eq!(Grab::of(BOX, (40, 35), reach), Some(Grab::Whole), "inside");
    assert_eq!(Grab::of(BOX, (90, 35), reach), None, "outside");
    let narrow = Bounds {
        x0: 20,
        y0: 20,
        x1: 23,
        y1: 50,
    };
    assert_eq!(
        Grab::of(narrow, (23, 35), reach),
        Some(Grab::Edges {
            across: Side::Far,
            down: Side::Neither
        }),
        "the nearer of two edges in reach"
    );
}

#[test]
fn a_drag_moves_what_it_took_and_is_held_to_the_picture() {
    let left = Grab::Edges {
        across: Side::Near,
        down: Side::Neither,
    };
    assert_eq!(left.dragged(BOX, (-5, 9), PICTURE).x0, 15);
    assert_eq!(
        left.dragged(BOX, (-50, 0), PICTURE).x0,
        0,
        "held to the picture"
    );
    assert_eq!(
        left.dragged(BOX, (90, 0), PICTURE).x0,
        59,
        "never under a pixel across"
    );
    let moved = Grab::Whole.dragged(BOX, (100, -100), PICTURE);
    assert_eq!(
        moved,
        Bounds {
            x0: 60,
            y0: 0,
            x1: 100,
            y1: 30
        },
        "moved whole as far as the picture lets it"
    );
}

#[test]
fn a_box_is_set_out_either_way_and_handled_at_eight_places() {
    let drawn = set_out((50, 40), (10, 70), PICTURE);
    assert_eq!(
        drawn,
        Bounds {
            x0: 10,
            y0: 40,
            x1: 51,
            y1: 71
        }
    );
    assert_eq!(
        set_out((90, 70), (130, 99), PICTURE).x1,
        100,
        "held to the picture"
    );
    let marks = handles(BOX, 6);
    assert_eq!(marks.len(), 8);
    for mark in marks {
        assert_eq!((mark.x1 - mark.x0, mark.y1 - mark.y0), (6, 6));
    }
    assert_eq!(
        (marks[0].x0, marks[0].y0),
        (17, 17),
        "centred on the corner"
    );
}
