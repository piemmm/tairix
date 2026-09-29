//! Unit tests for the window's pending renders ([`super::Renders`]).

use alloc::vec::Vec;

use super::*;

/// A region is its serial number, so a test can tell which one landed where.
type Region = u32;

fn wanted(index: u16) -> PictureWanted {
    PictureWanted {
        subject: PreviewSubject::Wallpaper(index),
        width: 160,
        height: 90,
    }
}

fn answer(index: u16) -> (PreviewSubject, u16, u16) {
    (PreviewSubject::Wallpaper(index), 160, 90)
}

/// Ask for `index` into a fresh or spare region, numbering new ones from
/// `made`.
fn ask(renders: &mut Renders<Region>, index: u16, made: &mut u32) -> Region {
    let region = renders
        .region(wanted(index).bytes(), |_| {
            *made += 1;
            Some(*made)
        })
        .expect("a region");
    renders.accepted(wanted(index), region);
    region
}

#[test]
fn several_renders_are_pending_each_landing_in_its_own_region() {
    let mut renders = Renders::new();
    let mut made = 0;
    let regions: Vec<Region> = (0..3)
        .map(|index| ask(&mut renders, index, &mut made))
        .collect();
    assert_eq!(regions, [1, 2, 3], "two renders shared a region");
    assert!((0..3).all(|index| renders.asked(PreviewSubject::Wallpaper(index))));

    let mut landed = None;
    assert!(renders.concluded(answer(1), |picture, region| {
        landed = Some((picture.subject, *region));
    }));
    assert_eq!(landed, Some((PreviewSubject::Wallpaper(1), 2)));
    assert!(!renders.asked(PreviewSubject::Wallpaper(1)));
}

/// The desktop answering that this window holds all it will take is not a
/// refusal of the picture: it is asked for again once a render concludes.
/// Taking it for one left a picture on its placeholder for good whenever
/// another client's render held the desktop's only slot.
#[test]
fn a_full_desktop_is_waited_on_never_taken_for_a_refusal() {
    let mut renders = Renders::new();
    let mut made = 0;
    ask(&mut renders, 0, &mut made);
    let region = renders
        .region(wanted(1).bytes(), |_| Some(99))
        .expect("a region");
    assert!(
        !renders.declined(Errno::LimitExceeded, region),
        "a full desktop refused the picture"
    );
    assert!(
        !renders.may_ask(),
        "asked again before any render concluded"
    );

    assert!(renders.concluded(answer(0), |_, _| {}));
    assert!(renders.may_ask(), "a conclusion did not free a place");
}

/// An acceptance a failed call never reported still counts against the
/// window at the desktop, so its answer must free the place, or a window
/// holding nothing it knows of would wait for good.
#[test]
fn an_answer_this_window_did_not_track_still_frees_a_place() {
    let mut renders: Renders<Region> = Renders::new();
    assert!(!renders.declined(Errno::LimitExceeded, 1));
    assert!(!renders.may_ask());
    let mut landed = false;
    assert!(!renders.concluded(answer(3), |_, _| landed = true));
    assert!(!landed, "an untracked answer landed a picture");
    assert!(
        renders.may_ask(),
        "an untracked answer left the window waiting"
    );
}

#[test]
fn a_duplicate_is_waited_on_too() {
    let mut renders: Renders<Region> = Renders::new();
    assert!(!renders.declined(Errno::AlreadyExists, 1));
    assert!(!renders.may_ask());
}

#[test]
fn any_other_refusal_refuses_the_picture_and_asking_goes_on() {
    let mut renders: Renders<Region> = Renders::new();
    assert!(renders.declined(Errno::NotFound, 1));
    assert!(renders.may_ask());
}

#[test]
fn a_region_is_kept_for_the_next_picture_once_its_answer_lands() {
    let mut renders = Renders::new();
    let mut made = 0;
    let first = ask(&mut renders, 0, &mut made);
    assert!(renders.concluded(answer(0), |_, _| {}));
    assert_eq!(
        ask(&mut renders, 1, &mut made),
        first,
        "a second region was made"
    );
    assert_eq!(made, 1);
}

#[test]
fn a_region_never_asked_with_is_kept_for_the_next_picture() {
    let mut renders = Renders::new();
    let region = renders
        .region(wanted(0).bytes(), |_| Some(1))
        .expect("a region");
    renders.unused(region);
    assert!(renders.may_ask(), "an unused region stopped the asking");
    assert_eq!(renders.region(wanted(1).bytes(), |_| Some(2)), Some(1));
}

#[test]
fn regions_of_a_size_no_longer_drawn_are_let_go() {
    let mut renders = Renders::new();
    let mut made = 0;
    ask(&mut renders, 0, &mut made);
    let larger = PictureWanted {
        width: 320,
        height: 180,
        ..wanted(1)
    };
    let region = renders
        .region(larger.bytes(), |_| Some(50))
        .expect("a region");
    assert_eq!(region, 50);
    renders.accepted(larger, region);
    // The old size's render concludes; its region is not kept for the new.
    assert!(renders.concluded(answer(0), |_, _| {}));
    assert_eq!(renders.region(larger.bytes(), |_| Some(51)), Some(51));
}

#[test]
fn a_render_asked_before_the_desktop_moved_is_waited_for_and_let_go() {
    let mut renders = Renders::new();
    let mut made = 0;
    ask(&mut renders, 0, &mut made);
    renders.restart();
    let mut landed = false;
    assert!(renders.concluded(answer(0), |_, _| landed = true));
    assert!(!landed, "a stale answer was landed");
    assert!(renders.is_idle());
}

#[test]
fn an_answer_nothing_waits_on_changes_nothing() {
    let mut renders = Renders::new();
    let mut made = 0;
    ask(&mut renders, 0, &mut made);
    let mut landed = false;
    assert!(!renders.concluded(answer(7), |_, _| landed = true));
    assert!(!landed);
    assert!(renders.asked(PreviewSubject::Wallpaper(0)));
}

#[test]
fn trimming_lets_go_of_the_spare_regions_alone() {
    let mut renders = Renders::new();
    let mut made = 0;
    ask(&mut renders, 0, &mut made);
    ask(&mut renders, 1, &mut made);
    assert!(renders.concluded(answer(0), |_, _| {}));
    renders.trim();
    assert_eq!(
        ask(&mut renders, 2, &mut made),
        3,
        "a trimmed region came back"
    );
    assert!(
        renders.asked(PreviewSubject::Wallpaper(1)),
        "a pending render was let go"
    );
}
