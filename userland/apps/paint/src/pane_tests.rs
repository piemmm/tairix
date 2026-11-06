use alloc::string::String;
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
    for kind in [PaneKind::Layers, PaneKind::Adjustments, PaneKind::Effects] {
        assert!(!panes.shows(kind), "{kind:?} waits to be shown");
        assert_eq!(
            panes.home(kind),
            Some(Side::Right),
            "{kind:?} joins the colour"
        );
    }
    assert!(panes.is_open(PaneKind::Tools));
    assert!(
        PaneKind::ALL.iter().all(|&kind| panes.opened(kind) == 0),
        "none opened yet"
    );
}

#[test]
fn a_shown_pane_lands_at_the_foot_of_its_home_and_a_hidden_one_remembers_it() {
    let mut panes = Arrangement::default();
    panes.show(PaneKind::Layers);
    assert_eq!(
        kinds(&panes, Side::Right),
        [PaneKind::Colour, PaneKind::Layers]
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
    panes.show(PaneKind::Layers);
    // Down the same dock: the gap after the Layers pane is index two as the
    // dock stood before the move.
    panes.move_to(PaneKind::Colour, Side::Right, 2);
    assert_eq!(
        kinds(&panes, Side::Right),
        [PaneKind::Layers, PaneKind::Colour]
    );
    panes.move_to(PaneKind::Colour, Side::Right, 0);
    assert_eq!(
        kinds(&panes, Side::Right),
        [PaneKind::Colour, PaneKind::Layers]
    );
    // Across, and past the end of the other dock: held to its foot.
    panes.move_to(PaneKind::Layers, Side::Left, 9);
    assert_eq!(
        kinds(&panes, Side::Left),
        [PaneKind::Tools, PaneKind::Layers]
    );
    assert_eq!(kinds(&panes, Side::Right), [PaneKind::Colour]);
    // Every pane shown is in one dock, once: down a side, or the palette in
    // its strip; one never shown is in none.
    for kind in PaneKind::ALL {
        let count = Side::BOTH
            .iter()
            .flat_map(|&side| panes.docked(side))
            .filter(|docked| docked.kind == kind)
            .count();
        let sides = usize::from(kind != PaneKind::Palette && panes.shows(kind));
        assert_eq!(count, sides, "{kind:?}");
    }
    assert!(panes.in_strip());
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
    assert_eq!(
        text,
        "tools:left colour:right palette:strip layers:right:hidden \
         adjustments:right:hidden effects:right:hidden"
    );
    assert_eq!(Arrangement::parse(&text), Some(panes.clone()));

    panes.move_to(PaneKind::Layers, Side::Left, 0);
    panes.toggle_collapsed(PaneKind::Tools);
    panes.hide(PaneKind::Colour);
    panes.float(PaneKind::Palette);
    text.clear();
    panes.spell(&mut text);
    assert_eq!(
        text,
        "layers:left tools:left:collapsed colour:right:hidden adjustments:right:hidden \
         effects:right:hidden palette:strip:floating"
    );
    assert_eq!(Arrangement::parse(&text), Some(panes));

    let rest = "layers:right:hidden adjustments:right:hidden effects:right:hidden";
    for refused in [
        String::new(),
        String::from("tools:left colour:right palette:strip"),
        // As stored before the Layers and Effects panes were panes.
        String::from("tools:left colour:right palette:strip adjustment:right:hidden"),
        // As stored before the palette was a pane.
        alloc::format!("tools:left colour:right {rest}"),
        alloc::format!("tools:left colour:right palette:strip {rest} tools:left"),
        alloc::format!("tools:up colour:right palette:strip {rest}"),
        alloc::format!("tools:left:open colour:right palette:strip {rest}"),
        alloc::format!("tools:left:hidden:again colour:right palette:strip {rest}"),
        alloc::format!("brushes:left colour:right palette:strip {rest}"),
        alloc::format!("tools:left colour:right palette:left {rest}"),
        alloc::format!("tools:strip colour:right palette:strip {rest}"),
        alloc::format!("tools:left colour:right palette:strip:collapsed {rest}"),
    ] {
        assert_eq!(Arrangement::parse(&refused), None, "{refused:?}");
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
    assert_eq!(panes.home(PaneKind::Colour), Some(Side::Right));
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
    panes.show(PaneKind::Layers);
    panes.float(PaneKind::Tools);
    let mut spelled = String::new();
    panes.spell(&mut spelled);
    assert_eq!(
        spelled,
        "colour:right layers:right palette:strip tools:left:floating \
         adjustments:right:hidden effects:right:hidden"
    );
    assert_eq!(Arrangement::parse(&spelled), Some(panes));
    assert_eq!(
        Arrangement::parse(
            "tools:left:floating:x colour:right layers:right:hidden adjustments:right:hidden \
             effects:right:hidden palette:strip"
        ),
        None
    );
}

#[test]
fn the_palette_docks_only_in_its_strip() {
    let mut panes = Arrangement::default();
    assert!(panes.in_strip() && panes.is_open(PaneKind::Palette));
    assert_eq!(panes.place(PaneKind::Palette), None, "down neither side");
    assert_eq!(panes.home(PaneKind::Palette), None);
    panes.float(PaneKind::Palette);
    assert!(!panes.in_strip() && panes.shows(PaneKind::Palette));
    panes.move_to(PaneKind::Palette, Side::Left, 0);
    assert!(panes.in_strip(), "a side is never its dock");
    assert_eq!(kinds(&panes, Side::Left), [PaneKind::Tools]);
    panes.toggle_collapsed(PaneKind::Palette);
    assert!(panes.is_open(PaneKind::Palette), "never rolled up");
    panes.float(PaneKind::Palette);
    panes.redock(PaneKind::Palette);
    assert!(panes.in_strip() && !panes.floats(PaneKind::Palette));
    panes.hide(PaneKind::Palette);
    assert!(!panes.shows(PaneKind::Palette));
    panes.show(PaneKind::Palette);
    assert!(panes.in_strip(), "shown, it is back in its strip");
}

/// A pane shown, unrolled or moved into a dock counts as opened after every
/// other; the order is the window's own, neither stored nor compared.
#[test]
fn each_pane_opened_counts_after_the_one_before() {
    let mut panes = Arrangement::default();
    panes.show(PaneKind::Layers);
    panes.show(PaneKind::Adjustments);
    assert!(panes.opened(PaneKind::Adjustments) > panes.opened(PaneKind::Layers));
    panes.show(PaneKind::Colour);
    assert_eq!(
        panes.opened(PaneKind::Colour),
        0,
        "open already, so not opened"
    );
    panes.toggle_collapsed(PaneKind::Layers);
    assert!(
        panes.opened(PaneKind::Layers) < panes.opened(PaneKind::Adjustments),
        "rolling up opens nothing"
    );
    panes.toggle_collapsed(PaneKind::Layers);
    assert!(
        panes.opened(PaneKind::Layers) > panes.opened(PaneKind::Adjustments),
        "unrolled, it is the latest"
    );
    panes.move_to(PaneKind::Colour, Side::Left, 0);
    assert!(
        panes.opened(PaneKind::Colour) > panes.opened(PaneKind::Layers),
        "moved into a dock"
    );
    let mut spelled = String::new();
    panes.spell(&mut spelled);
    let read = Arrangement::parse(&spelled).expect("read back");
    assert_eq!(
        read, panes,
        "the order is not part of where the panes stand"
    );
    assert_eq!(read.opened(PaneKind::Colour), 0, "nor stored");
}
