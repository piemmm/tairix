use alloc::vec::Vec;

use super::{falloff, Path, Tip};
use crate::canvas::{Canvas, Kind, Sample};
use crate::colour::Ink;
use crate::shape::{Point, FX};
use crate::stroke::{Blend, Coat, Stroke};

fn tip(size: u32, hardness: u8, flow: u8) -> Tip {
    Tip {
        size,
        hardness,
        opacity: 100,
        flow,
        spacing: 25,
    }
}

fn black() -> Coat {
    Coat {
        ink: Ink::Colour([0, 0, 0, 255]),
        blend: Blend::Over,
    }
}

fn alpha(canvas: &Canvas, x: u32, y: u32) -> u8 {
    match canvas.sample(x, y) {
        Some(Sample::Rgba([.., a])) => a,
        other => panic!("a colour pixel, not {other:?}"),
    }
}

#[test]
fn a_falloff_is_whole_inside_none_at_the_rim_and_falls_smoothly_between() {
    assert_eq!(falloff(0, 100, 400), 255);
    assert_eq!(falloff(100, 100, 400), 255);
    assert_eq!(falloff(400, 100, 400), 0);
    let fall: Vec<u8> = (100..=400)
        .step_by(10)
        .map(|d| falloff(d, 100, 400))
        .collect();
    assert!(fall.windows(2).all(|pair| pair[0] >= pair[1]), "{fall:?}");
    assert!(
        (120..=135).contains(&falloff(250, 100, 400)),
        "half way: {}",
        falloff(250, 100, 400)
    );
}

/// A tip's percentages are the nearest 255ths, as a layer's opacity is, so
/// a brush at half opacity lays what a layer at half opacity shows.
#[test]
fn a_tips_percentages_are_the_nearest_share() {
    let at = |opacity| {
        Tip {
            opacity,
            ..tip(4, 100, 100)
        }
        .opacity_255()
    };
    assert_eq!(at(100), 255);
    assert_eq!(at(0), 0);
    assert_eq!(at(250), 255, "held to a whole");
    assert_eq!(at(50), 128, "to the nearest, not short of it");
    for percent in 0..=100 {
        let nearest = (u32::from(percent) * 255 + 50) / 100;
        assert_eq!(u32::from(at(percent)), nearest, "{percent}%");
    }
}

#[test]
fn dabs_fall_a_step_apart_however_the_path_is_moved() {
    let mut whole = Vec::new();
    let mut path = Path::new(Point { x: 0, y: 0 });
    path.to(Point { x: 1000, y: 0 }, 100, |at| {
        whole.push(at);
        Ok(())
    })
    .expect("room");
    let mut pieces = Vec::new();
    let mut path = Path::new(Point { x: 0, y: 0 });
    for x in [37, 151, 152, 600, 1000] {
        path.to(Point { x, y: 0 }, 100, |at| {
            pieces.push(at);
            Ok(())
        })
        .expect("room");
    }
    assert_eq!(whole.len(), 10);
    assert_eq!(pieces, whole, "carried from one move to the next");
    assert_eq!(path.last(), Point { x: 1000, y: 0 });
    let mut none = 0;
    Path::new(Point { x: 5, y: 5 })
        .to(Point { x: 5, y: 5 }, 100, |_| {
            none += 1;
            Ok(())
        })
        .expect("room");
    assert_eq!(none, 0, "staying put lays nothing");
}

#[test]
fn a_spacing_shrunk_mid_stroke_lays_its_owed_dab_where_the_move_begins() {
    let mut path = Path::new(Point { x: 0, y: 0 });
    path.to(Point { x: 90, y: 0 }, 100, |_| Ok(()))
        .expect("room");
    let mut laid = Vec::new();
    path.to(Point { x: 190, y: 0 }, 50, |at| {
        laid.push(at.x);
        Ok(())
    })
    .expect("room");
    assert_eq!(
        laid,
        [90, 140, 190],
        "from where the move begins, a step apart"
    );
}

#[test]
fn a_hard_dab_covers_its_disc_and_a_soft_one_falls_away_toward_its_rim() {
    let centre = Point::centre_of(20, 20);
    let mut canvas = Canvas::new(40, 40, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let mut stroke = Stroke::building(black(), 255, None);
    tip(16, 100, 100)
        .dab(&mut stroke, &mut canvas, centre, true)
        .expect("room");
    assert_eq!(alpha(&canvas, 20, 20), 255);
    assert_eq!(alpha(&canvas, 26, 20), 255, "inside the rim");
    assert_eq!(alpha(&canvas, 29, 20), 0, "past it");
    let mut canvas = Canvas::new(40, 40, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let mut stroke = Stroke::building(black(), 255, None);
    tip(16, 0, 100)
        .dab(&mut stroke, &mut canvas, centre, true)
        .expect("room");
    let across: Vec<u8> = (20..29).map(|x| alpha(&canvas, x, 20)).collect();
    assert!(across[0] > 240, "{across:?}");
    assert!(
        across.windows(2).all(|pair| pair[0] >= pair[1]),
        "{across:?}"
    );
    assert!((1..200).contains(&across[5]), "{across:?}");
}

#[test]
fn flow_builds_up_and_opacity_caps_the_stroke() {
    let centre = Point::centre_of(10, 10);
    let mut canvas = Canvas::new(20, 20, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let mut stroke = Stroke::building(black(), 255, None);
    let half = tip(6, 100, 50);
    half.dab(&mut stroke, &mut canvas, centre, true)
        .expect("room");
    let once = alpha(&canvas, 10, 10);
    half.dab(&mut stroke, &mut canvas, centre, true)
        .expect("room");
    let twice = alpha(&canvas, 10, 10);
    assert_eq!(once, 128);
    assert_eq!(twice, 192, "built up: 1 - (1 - 128/255)^2 of the way");
    let mut canvas = Canvas::new(20, 20, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let capped = Tip {
        opacity: 40,
        ..tip(6, 100, 100)
    };
    let mut stroke = Stroke::building(black(), capped.opacity_255(), None);
    for _ in 0..5 {
        capped
            .dab(&mut stroke, &mut canvas, centre, true)
            .expect("room");
    }
    assert_eq!(alpha(&canvas, 10, 10), 102, "never past the opacity");
}

/// However faint each dab, a stroke's dabs keep adding until it lays its
/// whole opacity: no share of what is left is rounded away to nothing.
#[test]
fn the_faintest_flow_still_builds_the_stroke_to_its_opacity() {
    let centre = Point::centre_of(10, 10);
    for opacity in [100, 60] {
        let faint = Tip {
            opacity,
            ..tip(6, 100, 1)
        };
        let mut canvas = Canvas::new(20, 20, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
        let mut stroke = Stroke::building(black(), faint.opacity_255(), None);
        let mut laid = Vec::new();
        for _ in 0..1500 {
            faint
                .dab(&mut stroke, &mut canvas, centre, true)
                .expect("room");
            laid.push(alpha(&canvas, 10, 10));
        }
        assert!(
            laid.windows(2).all(|pair| pair[0] <= pair[1]),
            "never falls back"
        );
        assert_eq!(
            laid.last().copied(),
            Some(faint.opacity_255()),
            "{opacity}%: reaches the opacity rather than stalling short of it"
        );
    }
}

#[test]
fn a_tip_on_whole_pixels_is_hard_and_lays_all_its_paint() {
    let soft = Tip {
        size: 9,
        hardness: 10,
        opacity: 30,
        flow: 5,
        spacing: 40,
    };
    let whole = soft.whole();
    assert_eq!((whole.hardness, whole.opacity, whole.flow), (100, 100, 100));
    assert_eq!(
        (whole.size, whole.spacing),
        (9, 40),
        "its size and spacing kept"
    );
    assert_eq!(soft.step(), 9 * FX * 40 / 100);
    assert_eq!(
        Tip {
            size: 1,
            spacing: 1,
            ..soft
        }
        .step(),
        FX / 8,
        "never nearer than an eighth"
    );
}
