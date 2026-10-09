use alloc::vec::Vec;

use super::{Arrangement, PaneKind, Side};

fn kinds(arrangement: &Arrangement, side: Side) -> Vec<PaneKind> {
    arrangement
        .docked(side)
        .iter()
        .map(|docked| docked.kind)
        .collect()
}

#[test]
fn the_tools_start_down_the_left_and_the_colour_down_the_right() {
    let panes = Arrangement::default();
    assert_eq!(kinds(&panes, Side::Left), [PaneKind::Tools]);
    assert_eq!(kinds(&panes, Side::Right), [PaneKind::Colour]);
    assert!(!panes.shows(PaneKind::Adjustment));
    assert!(panes.is_open(PaneKind::Tools));
}

#[test]
fn a_shown_pane_lands_at_the_foot_of_its_home_and_a_hidden_one_remembers_it() {
    let mut panes = Arrangement::default();
    panes.show(PaneKind::Adjustment);
    assert_eq!(
        kinds(&panes, Side::Right),
        [PaneKind::Colour, PaneKind::Adjustment]
    );
    panes.move_to(PaneKind::Colour, Side::Left, 0);
    panes.hide(PaneKind::Colour);
    assert_eq!(kinds(&panes, Side::Left), [PaneKind::Tools]);
    panes.show(PaneKind::Colour);
    assert_eq!(
        kinds(&panes, Side::Left),
        [PaneKind::Tools, PaneKind::Colour],
        "back on the side it was hidden from"
    );
    assert_eq!(panes.place(PaneKind::Colour), Some((Side::Left, 1)));
}

#[test]
fn a_move_lands_in_the_gap_shown_whichever_dock_it_came_from() {
    let mut panes = Arrangement::default();
    panes.show(PaneKind::Adjustment);
    // Down the same dock: the gap after the adjustment pane is index two as
    // the dock stood before the move.
    panes.move_to(PaneKind::Colour, Side::Right, 2);
    assert_eq!(
        kinds(&panes, Side::Right),
        [PaneKind::Adjustment, PaneKind::Colour]
    );
    panes.move_to(PaneKind::Colour, Side::Right, 0);
    assert_eq!(
        kinds(&panes, Side::Right),
        [PaneKind::Colour, PaneKind::Adjustment]
    );
    // Across, and past the end of the other dock: held to its foot.
    panes.move_to(PaneKind::Adjustment, Side::Left, 9);
    assert_eq!(
        kinds(&panes, Side::Left),
        [PaneKind::Tools, PaneKind::Adjustment]
    );
    assert_eq!(kinds(&panes, Side::Right), [PaneKind::Colour]);
    // Every pane is in one dock, once.
    for kind in PaneKind::ALL {
        let count = Side::BOTH
            .iter()
            .flat_map(|&side| panes.docked(side))
            .filter(|docked| docked.kind == kind)
            .count();
        assert_eq!(count, 1, "{kind:?}");
    }
}

#[test]
fn a_rolled_up_pane_stays_put_and_showing_it_opens_it() {
    let mut panes = Arrangement::default();
    panes.toggle_collapsed(PaneKind::Tools);
    assert!(panes.shows(PaneKind::Tools));
    assert!(!panes.is_open(PaneKind::Tools));
    panes.move_to(PaneKind::Tools, Side::Right, 1);
    assert!(!panes.is_open(PaneKind::Tools), "a move keeps it rolled up");
    panes.show(PaneKind::Tools);
    assert!(panes.is_open(PaneKind::Tools));
    assert_eq!(panes.place(PaneKind::Tools), Some((Side::Right, 1)));
}

#[test]
fn an_arrangement_is_stored_as_every_pane_once() {
    let mut panes = Arrangement::default();
    let mut text = alloc::string::String::new();
    panes.spell(&mut text);
    assert_eq!(text, "tools:left colour:right adjustment:right:hidden");
    assert_eq!(Arrangement::parse(&text), Some(panes.clone()));

    panes.move_to(PaneKind::Adjustment, Side::Left, 0);
    panes.toggle_collapsed(PaneKind::Tools);
    panes.hide(PaneKind::Colour);
    text.clear();
    panes.spell(&mut text);
    assert_eq!(
        text,
        "adjustment:left tools:left:collapsed colour:right:hidden"
    );
    assert_eq!(Arrangement::parse(&text), Some(panes));

    for refused in [
        "",
        "tools:left colour:right",
        "tools:left colour:right adjustment:right tools:left",
        "tools:up colour:right adjustment:right",
        "tools:left:open colour:right adjustment:right",
        "tools:left:hidden:again colour:right adjustment:right",
        "brushes:left colour:right adjustment:right",
    ] {
        assert_eq!(Arrangement::parse(refused), None, "{refused:?}");
    }
}

#[test]
fn a_floating_pane_is_shown_open_in_no_dock_and_docks_again_where_it_is_moved() {
    let mut panes = Arrangement::default();
    panes.float(PaneKind::Colour);
    assert!(panes.floats(PaneKind::Colour));
    assert!(panes.shows(PaneKind::Colour) && panes.is_open(PaneKind::Colour));
    assert_eq!(panes.place(PaneKind::Colour), None);
    assert!(kinds(&panes, Side::Right).is_empty());
    assert_eq!(panes.home(PaneKind::Colour), Side::Right);
    assert_eq!(panes.floating().collect::<Vec<_>>(), [PaneKind::Colour]);

    panes.show(PaneKind::Colour);
    assert!(
        panes.floats(PaneKind::Colour),
        "showing it leaves it floating"
    );
    panes.toggle_collapsed(PaneKind::Colour);
    assert!(
        panes.is_open(PaneKind::Colour),
        "a floating pane is never rolled up"
    );

    panes.move_to(PaneKind::Colour, Side::Left, 0);
    assert!(!panes.floats(PaneKind::Colour));
    assert_eq!(
        kinds(&panes, Side::Left),
        [PaneKind::Colour, PaneKind::Tools]
    );

    panes.float(PaneKind::Tools);
    panes.hide(PaneKind::Tools);
    assert!(
        !panes.shows(PaneKind::Tools),
        "closing a floating pane hides it"
    );
    panes.show(PaneKind::Tools);
    assert_eq!(
        panes.place(PaneKind::Tools),
        Some((Side::Left, 1)),
        "home again"
    );
}

#[test]
fn a_floating_pane_is_spelled_with_its_home_and_read_back() {
    let mut panes = Arrangement::default();
    panes.show(PaneKind::Adjustment);
    panes.float(PaneKind::Tools);
    let mut spelled = alloc::string::String::new();
    panes.spell(&mut spelled);
    assert_eq!(spelled, "colour:right adjustment:right tools:left:floating");
    assert_eq!(Arrangement::parse(&spelled), Some(panes));
    assert_eq!(
        Arrangement::parse("tools:left:floating:x colour:right adjustment:right:hidden"),
        None
    );
}
