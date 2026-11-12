use alloc::vec::Vec;

use super::*;

/// A toy encoding: a jump of two bytes reaching eight either way, or of
/// six reaching a hundred; padding; a bind.
#[derive(Clone, Copy)]
enum Toy {
    Jump(Label),
    Pad(usize),
    Bind(Label),
}

impl Relaxable for Toy {
    fn binds(&self) -> Option<Label> {
        match *self {
            Self::Bind(label) => Some(label),
            _ => None,
        }
    }

    fn target(&self) -> Option<Label> {
        match *self {
            Self::Jump(label) => Some(label),
            _ => None,
        }
    }

    fn length(&self, form: usize) -> usize {
        match *self {
            Self::Jump(_) => [2, 6][form],
            Self::Pad(n) => n,
            Self::Bind(_) => 0,
        }
    }

    fn forms(&self) -> usize {
        match self {
            Self::Jump(_) => 2,
            _ => 1,
        }
    }

    fn reaches(&self, form: usize, distance: i64) -> bool {
        distance.abs() <= [8, 100][form]
    }
}

#[test]
fn near_jumps_stay_short_and_far_ones_grow() {
    let insts = [
        Toy::Jump(Label(0)),
        Toy::Pad(4),
        Toy::Bind(Label(0)),
        Toy::Jump(Label(1)),
        Toy::Pad(20),
        Toy::Bind(Label(1)),
    ];
    let layout = relax(&insts, 2).unwrap();
    assert_eq!(layout.forms, [0, 0, 0, 1, 0, 0]);
    assert_eq!(layout.labels, [6, 32]);
    assert_eq!(layout.length, 32);
}

#[test]
fn one_jump_growing_can_push_another_out_of_reach() {
    // The second jump reaches back across the first while the first is
    // short, and not once it has grown.
    let insts = [
        Toy::Bind(Label(0)),
        Toy::Pad(3),
        Toy::Jump(Label(1)),
        Toy::Jump(Label(0)),
        Toy::Pad(10),
        Toy::Bind(Label(1)),
    ];
    let layout = relax(&insts, 2).unwrap();
    assert_eq!(layout.forms[2..4], [1, 1]);
    let places: Vec<usize> = layout.places.clone();
    assert_eq!(places, [0, 0, 3, 9, 15, 25]);
}

#[test]
fn an_unbound_or_unreachable_label_is_refused() {
    assert_eq!(
        relax(&[Toy::Jump(Label(0))], 1).err(),
        Some(CompileError::Malformed)
    );
    assert_eq!(
        relax(&[Toy::Jump(Label(3))], 1).err(),
        Some(CompileError::Malformed)
    );
    let far = [Toy::Jump(Label(0)), Toy::Pad(200), Toy::Bind(Label(0))];
    assert_eq!(relax(&far, 1).err(), Some(CompileError::TooLarge));
}
