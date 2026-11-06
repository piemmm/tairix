use alloc::string::String;
use alloc::vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use tairix_parallel::{JobRunner, Reversed, SERIAL};

use super::{
    serve, supersedes, Overlay, PreviewAnswer, PreviewJob, PreviewWork, Previewed, Ticket,
};
use crate::canvas::{Canvas, Kind, Sample};
use crate::document::Layer;
use crate::effect::patterned;
use crate::mask::Mask;
use crate::op::{Halted, Op};
use crate::shape::Bounds;
use crate::tiled::{run, tiles_for};

/// Window `window`'s job, which the window counts as ten times its own number.
fn job(window: u64, ticket: &Ticket, work: PreviewWork) -> PreviewJob {
    PreviewJob {
        window,
        job: window * 10,
        stamp: ticket.stamp(),
        work,
    }
}

/// `job` served on one core with no completion pressing for it.
fn served(job: PreviewJob) -> PreviewAnswer {
    serve(job, &SERIAL, &|| false)
}

fn inverting(source: &crate::canvas::Canvas) -> PreviewWork {
    PreviewWork::Tiles {
        source: source.try_clone().expect("room"),
        op: Op::Invert,
        clip: None,
        tiles: tiles_for(source, None).expect("room"),
    }
}

#[test]
fn a_job_works_out_the_tiles_its_stamp_still_holds_for() {
    let canvas = patterned(150, 100);
    let ticket = Ticket::new();
    let answer = served(job(7, &ticket, inverting(&canvas)));
    assert_eq!((answer.window, answer.job), (7, 70));
    assert!(ticket.holds(answer.ticket));
    let Previewed::Tiles(Ok(made)) = answer.previewed else {
        panic!("tiles, not {:?}", answer.previewed);
    };
    assert_eq!(made.len(), canvas.tile_count());
    let histogram = job(7, &ticket, PreviewWork::Histogram { canvas, clip: None });
    let answer = served(histogram);
    assert!(matches!(answer.previewed, Previewed::Histogram(Ok(_))));
}

#[test]
fn a_bumped_ticket_abandons_the_job_and_disowns_its_answer() {
    let canvas = patterned(150, 100);
    let ticket = Ticket::new();
    let asked = job(1, &ticket, inverting(&canvas));
    ticket.bump();
    let answer = served(asked);
    assert!(
        !ticket.holds(answer.ticket),
        "the answer is for a ticket since bumped"
    );
    assert!(matches!(
        answer.previewed,
        Previewed::Tiles(Err(Halted::Abandoned))
    ));
    let histogram = job(1, &ticket, PreviewWork::Histogram { canvas, clip: None });
    ticket.bump();
    let answer = served(histogram);
    assert!(matches!(
        answer.previewed,
        Previewed::Histogram(Err(Halted::Abandoned))
    ));
}

#[test]
fn a_window_s_newer_job_takes_the_place_of_its_waiting_one_alone() {
    let ticket = Ticket::new();
    let histogram = |window| {
        let canvas = patterned(8, 8);
        job(
            window,
            &ticket,
            PreviewWork::Histogram { canvas, clip: None },
        )
    };
    assert!(supersedes(&histogram(3), &histogram(3)));
    assert!(!supersedes(&histogram(3), &histogram(4)));
}

#[test]
fn an_overlay_takes_the_tiles_in_view_first_and_is_complete_once_all_land() {
    let canvas = patterned(330, 200);
    let op = Op::Invert;
    let mut overlay = Overlay::new(5u64, op, &canvas, None).expect("room");
    assert!(overlay.shows(5, &op));
    assert_eq!(overlay.basis(), 5);
    assert!(!overlay.shows(6, &op));
    assert!(!overlay.shows(5, &Op::Desaturate));
    assert_eq!(*overlay.op(), op);
    assert!(!overlay.is_complete());
    let visible = Bounds {
        x0: 70,
        y0: 70,
        x1: 100,
        y1: 90,
    };
    assert_eq!(overlay.shown(), &canvas, "nothing worked out yet");
    let first = overlay.next_batch(visible).expect("room");
    assert_eq!(first, [canvas.tile_index(64, 64)]);
    overlay.land(run(&canvas, &op, None, &first, &SERIAL, &|| true).expect("room"));
    let rest = overlay.next_batch(visible).expect("room");
    assert_eq!(
        rest.len(),
        canvas.tile_count() - 1,
        "the rest, once the view is in"
    );
    assert!(!rest.contains(&first[0]));
    overlay.land(run(&canvas, &op, None, &rest, &SERIAL, &|| true).expect("room"));
    assert!(overlay.is_complete());
    assert!(overlay.next_batch(visible).expect("room").is_empty());
    let shown = overlay.shown();
    let back = shown
        .colour_at(0, 0)
        .map(|[r, g, b, a]| [255 - r, 255 - g, 255 - b, a]);
    assert_eq!(back, canvas.colour_at(0, 0), "the inverted picture shown");
    assert_eq!(overlay.done().expect("room").len(), canvas.tile_count());
}

#[test]
fn an_overlay_followed_by_another_setting_shows_the_last_until_its_own_land() {
    let canvas = patterned(150, 100);
    let all = tiles_for(&canvas, None).expect("room");
    let mut overlay = Overlay::new(1u64, Op::Invert, &canvas, None).expect("room");
    overlay.land(run(&canvas, &Op::Invert, None, &all, &SERIAL, &|| true).expect("room"));
    assert!(overlay.is_complete());
    let inverted = overlay.shown().colour_at(5, 5);
    let next = Op::Posterise { levels: 3 };
    let mut overlay = overlay.followed_by(next);
    assert!(overlay.shows(1, &next) && !overlay.is_complete());
    assert!(
        overlay.done().expect("room").is_empty(),
        "nothing of its own yet"
    );
    assert_eq!(
        overlay.shown().colour_at(5, 5),
        inverted,
        "the last preview still shows"
    );
    let posterised = run(&canvas, &next, None, &all, &SERIAL, &|| true).expect("room");
    let mut expected = canvas.try_clone().expect("room");
    for (index, tile) in &posterised {
        expected.replace_tile(*index, alloc::sync::Arc::clone(tile));
    }
    overlay.land(posterised);
    assert!(overlay.is_complete());
    assert_eq!(overlay.shown(), &expected, "its own tiles in place");
    assert_eq!(overlay.done().expect("room").len(), all.len());
}

#[test]
fn an_overlay_takes_no_tile_it_does_not_want_and_none_twice() {
    let canvas = patterned(330, 200);
    let corner = Bounds {
        x0: 0,
        y0: 0,
        x1: 60,
        y1: 60,
    };
    let clip = Mask::rect(corner).expect("a selection");
    let op = Op::Invert;
    let mut overlay = Overlay::new((), op, &canvas, Some(&clip)).expect("room");
    let all = tiles_for(&canvas, None).expect("room");
    let made = run(&canvas, &op, None, &all, &SERIAL, &|| true).expect("room");
    overlay.land(made.clone());
    overlay.land(made);
    assert!(overlay.is_complete());
    assert_eq!(
        overlay.done().expect("room").len(),
        1,
        "the one tile it reaches"
    );
}

#[test]
fn the_layers_panes_work_is_served_and_abandoned_as_any_other() {
    let white = Canvas::new(40, 30, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    let ticket = Ticket::new();
    let thumbnails = |ticket: &Ticket| {
        let layers = vec![(7, white.try_clone().expect("room"))];
        let work = PreviewWork::LayerThumbnails {
            side: 16,
            aspect: (1, 1),
            layers,
        };
        job(2, ticket, work)
    };
    let answer = served(thumbnails(&ticket));
    assert_eq!(answer.job, 20);
    let Previewed::LayerThumbnails { side, drawn } = answer.previewed else {
        panic!("thumbnails, not {:?}", answer.previewed);
    };
    assert_eq!(side, 16);
    let drawn = drawn.expect("drawn");
    assert!(
        matches!(drawn.as_slice(), [(7, Some(_))]),
        "drawn by revision"
    );
    let asked = thumbnails(&ticket);
    ticket.bump();
    let Previewed::LayerThumbnails { drawn, .. } = served(asked).previewed else {
        panic!("thumbnails");
    };
    assert_eq!(
        drawn.err(),
        Some(Halted::Abandoned),
        "abandoned before the first, not drawn as nothing"
    );
    let under = |ticket: &Ticket| {
        let layer = Layer::new(white.try_clone().expect("room"), String::from("beneath"));
        job(
            2,
            ticket,
            PreviewWork::Under {
                layers: vec![layer],
            },
        )
    };
    let Previewed::Under(Ok(laid)) = served(under(&ticket)).previewed else {
        panic!("the layers laid together");
    };
    assert_eq!(laid.colour_at(3, 3), Some([255; 4]));
    let stale = under(&ticket);
    ticket.bump();
    assert!(matches!(
        served(stale).previewed,
        Previewed::Under(Err(Halted::Abandoned))
    ));
}

/// A job gives way to an Apply's completion pressing for the cores, its ticket
/// still holding, so its window asks for it again once the cores are free.
#[test]
fn a_job_gives_way_to_a_completion_its_ticket_still_holding() {
    let canvas = patterned(150, 100);
    let ticket = Ticket::new();
    let pressing = || true;
    let tiles = serve(job(4, &ticket, inverting(&canvas)), &SERIAL, &pressing);
    assert!(ticket.holds(tiles.ticket), "not stale, so asked again");
    assert!(matches!(
        tiles.previewed,
        Previewed::Tiles(Err(Halted::Abandoned))
    ));
    let histogram = job(4, &ticket, PreviewWork::Histogram { canvas, clip: None });
    assert!(matches!(
        serve(histogram, &SERIAL, &pressing).previewed,
        Previewed::Histogram(Err(Halted::Abandoned))
    ));
    let white = Canvas::new(40, 30, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    let thumbnails = PreviewWork::LayerThumbnails {
        side: 16,
        aspect: (1, 1),
        layers: vec![(7, white.try_clone().expect("room"))],
    };
    let answer = serve(job(4, &ticket, thumbnails), &SERIAL, &pressing);
    let Previewed::LayerThumbnails { drawn, .. } = answer.previewed else {
        panic!("thumbnails");
    };
    assert_eq!(
        drawn.err(),
        Some(Halted::Abandoned),
        "given way, not drawn as nothing"
    );
    let under = PreviewWork::Under {
        layers: vec![Layer::new(white, String::from("beneath"))],
    };
    assert!(matches!(
        serve(job(4, &ticket, under), &SERIAL, &pressing).previewed,
        Previewed::Under(Err(Halted::Abandoned))
    ));
}

/// The composite beneath an opacity changing asks before each tile whether it
/// is still wanted, so one given way mid-way stops between tiles rather than
/// holding the cores to its end.
#[test]
fn a_composite_given_way_mid_way_stops_between_tiles() {
    let ticket = Ticket::new();
    let ground = Layer::new(patterned(330, 200), String::from("beneath"));
    let asked = AtomicUsize::new(0);
    let pressing = || asked.fetch_add(1, Ordering::Relaxed) >= 3;
    let under = PreviewWork::Under {
        layers: vec![ground],
    };
    let answer = serve(job(6, &ticket, under), &SERIAL, &pressing);
    assert!(
        matches!(answer.previewed, Previewed::Under(Err(Halted::Abandoned))),
        "given up between tiles"
    );
    assert!(
        ticket.holds(answer.ticket),
        "its ticket holds, so it is asked again"
    );
}

/// The Layers pane's thumbnails drawn a layer each across several cores, in any
/// order, are those drawn on one.
#[test]
fn thumbnails_drawn_across_cores_are_those_drawn_on_one() {
    let ticket = Ticket::new();
    let drawn = |runner: &dyn JobRunner| {
        let layers = vec![
            (1, patterned(40, 30)),
            (2, patterned(50, 20)),
            (3, patterned(30, 60)),
        ];
        let work = PreviewWork::LayerThumbnails {
            side: 16,
            aspect: (1, 1),
            layers,
        };
        let answer = serve(job(5, &ticket, work), runner, &|| false);
        let Previewed::LayerThumbnails { drawn, .. } = answer.previewed else {
            panic!("thumbnails, not {:?}", answer.previewed);
        };
        drawn.expect("drawn")
    };
    let across = Reversed::new(4);
    let spread = drawn(&across);
    assert!(across.widest() > 1, "a layer a piece");
    assert_eq!(spread, drawn(&SERIAL));
    assert_eq!(spread.len(), 3);
}
