//! Unit tests for the lane policy: the routing of a place to its volume, the
//! one worker a lane is served by and the holds of one, the crew's spares,
//! and the decodes the machine leaves room for.

use super::*;

use alloc::string::String;
use alloc::vec;

use tairix_abi::driver::filesystem::MountFlags;
use tairix_abi::fs::{DocumentStamp, FileId};
use tairix_abi::time::Time64;
use tairix_icon::{FolderSample, IconKind, SampleCard, Thumbnail};

fn view(target: &str, id: u8) -> ViewBindingRecord {
    ViewBindingRecord::new(target.as_bytes(), b"/", MountFlags::default(), [id; 16]).expect("view")
}

/// The default install with a stick: the root volume (1) at `/`, the system
/// volume (2) at `/System`, the catalog showing none, and the stick (3).
fn installed() -> Routes {
    Routes::new(vec![
        view("/", 1),
        view("/System", 2),
        view("/Storage", 0),
        view("/Storage/stick", 3),
    ])
}

fn parts(path: &[&str]) -> Vec<String> {
    path.iter().map(|part| String::from(*part)).collect()
}

/// A volume is shown at each binding of it, and nowhere a binding of another
/// volume, or of none, shows.
#[test]
fn a_volume_is_shown_at_each_binding_of_it() {
    let routes = Routes::new(vec![
        view("/", 1),
        view("/Storage", 0),
        view("/Storage/stick", 3),
        view("/Users/ada/Stick", 3),
    ]);
    let shown: Vec<Vec<String>> = routes
        .shown_at([3; 16])
        .map(|shown| shown.components)
        .collect();
    assert_eq!(
        shown,
        [
            parts(&["Storage", "stick"]),
            parts(&["Users", "ada", "Stick"])
        ]
    );
    assert_eq!(
        routes.shown_at([0; 16]).count(),
        0,
        "the catalog shows none"
    );
    assert_eq!(routes.shown_at([9; 16]).count(), 0);
}

#[test]
fn a_place_runs_on_the_lane_of_the_volume_covering_it() {
    let routes = installed();
    assert_eq!(
        routes.lane_of(&parts(&["Users", "ada"])),
        Lane::Volume([1; 16])
    );
    assert_eq!(
        routes.lane_of(&parts(&["System", "Commands"])),
        Lane::Volume([2; 16])
    );
    assert_eq!(
        routes.lane_of(&parts(&["Storage", "stick", "a"])),
        Lane::Volume([3; 16])
    );
    assert_eq!(
        routes.lane_of(&parts(&["Storage", "stick"])),
        Lane::Volume([3; 16])
    );
    assert_eq!(
        routes.lane_of(&parts(&["Storage"])),
        Lane::Unnamed,
        "the catalog stores nothing, and the volume above it is not its lane"
    );
    assert_eq!(routes.lane_of(&parts(&["Storage", "gone"])), Lane::Unnamed);
    assert_eq!(
        routes.lane_of_path("/Storage/stick/a.png"),
        Lane::Volume([3; 16])
    );
    assert_eq!(routes.lane_of_path("/Systemic"), Lane::Volume([1; 16]));
    assert_eq!(
        Routes::default().lane_of(&parts(&["Storage", "stick"])),
        Lane::Unnamed,
        "before the namespace is read every place shares one lane"
    );
}

#[test]
fn artwork_runs_on_the_lane_of_the_file_it_reads() {
    let routes = installed();
    let thumbnail = |path: &str| Thumbnail {
        path: String::from(path),
        stamp: DocumentStamp {
            size: 12,
            modified: Time64::from_secs(1_700_000_000),
            id: FileId {
                volume: [3; 16],
                node: 9,
            },
            content_gen: 5,
        },
    };
    assert_eq!(
        routes.lane_of_artwork(&ArtworkKey::Thumbnail(thumbnail("/Storage/stick/p.png"))),
        Lane::Volume([3; 16])
    );
    assert_eq!(
        routes.lane_of_artwork(&ArtworkKey::Asset(String::from("/Users/ada/i.png"))),
        Lane::Volume([1; 16])
    );
    let pictured = FolderSample::new([SampleCard::Picture(
        IconKind::Image,
        thumbnail("/Storage/stick/f/p.png"),
    )]);
    assert_eq!(
        routes.lane_of_artwork(&ArtworkKey::Folder(pictured.clone())),
        Lane::Volume([3; 16]),
        "a folder's cards are read from its members"
    );
    assert_eq!(
        routes.lane_of_artwork(&ArtworkKey::Folder(pictured.kinds_only())),
        Lane::Services,
        "drawn as kinds it reads the system's own artwork"
    );
    assert_eq!(
        routes.lane_of_artwork(&ArtworkKey::Raster(IconKind::Text)),
        Lane::Services
    );
}

/// The eject desk carries an eject out behind the reads on its volume; an act
/// that never waits there is placed where its own reads would run.
#[test]
fn an_eject_waits_on_its_volumes_lane() {
    let routes = installed();
    let eject = MountAct::Eject {
        volume_id: [3; 16],
        name: String::from("stick"),
        asked: crate::mount::EjectAsked::ByUser,
    };
    assert_eq!(routes.lane_of_act(&eject), Lane::Volume([3; 16]));
    let attach = MountAct::Attach {
        path: String::from("/Users/ada/disc.iso"),
        read_only: true,
        handoff: None,
    };
    assert_eq!(routes.lane_of_act(&attach), Lane::Volume([1; 16]));
}

#[test]
fn a_lane_is_served_by_one_worker_at_a_time() {
    let mut serving = Serving::new();
    let stick = Lane::Volume([3; 16]);
    assert!(serving.is_free(stick));
    serving.begin(stick);
    serving.begin(Lane::Services);
    assert!(!serving.is_free(stick) && !serving.is_free(Lane::Services));
    assert!(serving.is_free(Lane::Unnamed));
    serving.end(stick);
    assert!(serving.is_free(stick));
    assert!(
        !serving.is_free(Lane::Services),
        "ending one lane frees no other"
    );
}

/// A hold of a lane waits for the work under way there and takes nothing
/// from it, and from the moment it waits no worker takes more there: the
/// desktop's eject once overlapped an icon read of its volume, which refused
/// it as busy.
#[test]
fn a_hold_waits_for_the_work_under_way_and_no_worker_takes_more() {
    let mut serving = Serving::new();
    let stick = Lane::Volume([3; 16]);
    serving.begin(stick);
    assert!(serving.await_lane(stick));
    assert!(
        !serving.is_free(stick),
        "a worker could take more work there"
    );
    assert!(
        serving.is_free(Lane::Services),
        "a hold stood other lanes down"
    );
    assert!(
        !serving.take_awaited(stick),
        "held while a worker serves it"
    );
    assert!(serving.end(stick), "the worker's end woke no hold");
    assert!(serving.take_awaited(stick));
    assert!(!serving.is_free(stick), "a held lane was free");
    assert!(
        !serving.end(stick),
        "a hold ending woke a hold none waits for"
    );
    assert!(serving.is_free(stick));
}

/// Two holds of one lane take it in turn, the first's end waking the second.
#[test]
fn holds_of_one_lane_take_it_in_turn() {
    let mut serving = Serving::new();
    let stick = Lane::Volume([3; 16]);
    assert!(serving.await_lane(stick) && serving.await_lane(stick));
    assert!(serving.take_awaited(stick));
    assert!(
        !serving.take_awaited(stick),
        "two holds had the lane at once"
    );
    assert!(serving.end(stick), "the first hold's end woke no second");
    assert!(serving.take_awaited(stick));
    assert!(!serving.end(stick));
    assert!(serving.is_free(stick));
}

/// A hold the heap has no room to note leaves the lane as it was, and a lane
/// the heap has no room to note is begun by no worker.
#[test]
fn a_lane_the_heap_cannot_note_is_neither_held_nor_served() {
    let stick = Lane::Volume([3; 16]);
    let mut serving = Serving::new();
    let (awaited, room) = tairix_fuzzseed::meter::refusing_above(0, || {
        (serving.await_lane(stick), serving.has_room())
    });
    assert!(!awaited && !room);
    assert!(serving.is_free(stick));
    assert!(!serving.take_awaited(stick), "a hold never noted was taken");
    assert!(serving.has_room());
}

/// While every worker is busy a spare is started, so work for a free lane
/// never waits for a busy one; a third waiting worker leaves.
#[test]
fn a_spare_waits_while_the_others_work_and_no_more_than_two_wait() {
    let mut crew = Crew::new();
    crew.start();
    assert!(!crew.took(), "the first worker is still on its way");
    crew.arrived();
    assert!(
        crew.took(),
        "the only worker is busy, so a spare is started"
    );
    assert!(!crew.took(), "a spare is already on its way");
    crew.arrived();
    assert_eq!(crew.rest(), Rest::Wait, "the spare waits");
    assert_eq!(crew.alive(), 2);
    crew.woke();
    assert!(crew.took(), "the spare took work, so another is started");
    crew.arrived();
    assert_eq!(crew.alive(), 3);
    for _ in 0..2 {
        assert_eq!(crew.rest(), Rest::Wait);
    }
    assert_eq!(
        crew.rest(),
        Rest::Leave,
        "two already wait, so the third leaves"
    );
    assert_eq!(crew.alive(), 2);
}

/// Once two wait, a lone piece of work now and then is taken by one of them:
/// no thread is made for it and none ended after.
#[test]
fn a_lone_piece_of_work_now_and_then_makes_and_ends_no_thread() {
    let mut crew = Crew::new();
    crew.start();
    crew.arrived();
    crew.woke();
    assert!(crew.took());
    crew.arrived();
    assert_eq!(crew.rest(), Rest::Wait);
    assert_eq!(crew.rest(), Rest::Wait);
    for _ in 0..4 {
        crew.woke();
        assert!(!crew.took(), "one still waits, so no spare is started");
        assert_eq!(crew.rest(), Rest::Wait, "only one waits, so none leaves");
    }
    assert_eq!(crew.alive(), 2);
}

/// A refused start is stated once and tried again once a worker rests, never
/// on every take: a crew of one or two, which can rest but not leave, still
/// grows once what the refused start lacked is back.
#[test]
fn a_refused_spare_is_tried_again_once_a_worker_rests() {
    for workers in [1, 2, 3] {
        let mut crew = Crew::new();
        crew.start();
        crew.arrived();
        for _ in 1..workers {
            assert!(crew.took(), "every worker is busy, so a spare is started");
            crew.arrived();
        }
        assert!(crew.took(), "{workers}: the busy crew starts a spare");
        assert!(crew.refused(), "{workers}: the first refusal is stated");
        assert_eq!(crew.alive(), workers, "a refused spare is not counted");
        assert!(
            !crew.took(),
            "{workers}: no spare is tried again on every take"
        );
        assert_eq!(crew.rest(), Rest::Wait);
        crew.woke();
        assert!(crew.took(), "{workers}: a rest lets a spare be tried again");
        assert!(
            !crew.refused(),
            "{workers}: a run of refusals is stated once"
        );
        assert_eq!(crew.rest(), Rest::Wait);
        crew.woke();
        assert!(crew.took());
        crew.arrived();
        assert!(crew.took(), "{workers}: the crew is busy again");
        assert!(
            crew.refused(),
            "{workers}: a refusal after a worker arrived is stated again"
        );
    }
}

/// A decode ending says whether it freed a slot none was free in, so a decode
/// recorded meanwhile, which woke no worker, is handed to one.
#[test]
fn a_decode_ending_says_whether_it_freed_a_full_pool() {
    let mut decodes = Decodes::new();
    decodes.admit(2);
    decodes.started();
    assert!(!decodes.ended(), "a slot was free throughout");
    decodes.started();
    decodes.started();
    assert!(decodes.ended(), "the pool was full");
    assert!(!decodes.ended(), "one ran beside a free slot");
}

/// No more decodes start than the machine leaves room for, and at least one
/// always may.
#[test]
fn decodes_start_no_more_than_the_machine_leaves_room_for() {
    let mut decodes = Decodes::new();
    assert!(decodes.may_start());
    decodes.started();
    assert!(
        !decodes.may_start(),
        "one at a time until the machine is known"
    );
    decodes.admit(2);
    assert!(decodes.may_start());
    decodes.started();
    assert!(!decodes.may_start());
    assert!(decodes.ended());
    assert!(decodes.may_start());
    decodes.admit(0);
    assert_eq!(decodes.most(), 1, "never fewer than one");
}

#[test]
fn a_stopped_crew_starts_no_spare_and_every_worker_leaves() {
    let mut crew = Crew::new();
    crew.start();
    crew.arrived();
    assert_eq!(crew.rest(), Rest::Wait);
    crew.stop();
    assert!(crew.stopping());
    crew.woke();
    assert!(!crew.took(), "no spare is started for a stopping crew");
    assert_eq!(crew.rest(), Rest::Leave);
    assert_eq!(crew.alive(), 0);
}

/// An idle worker's holdings are kept only while memory is plentiful, and the
/// waiting workers are woken once, as it first runs short.
#[test]
fn a_crew_is_lean_whenever_memory_is_short_and_says_so_once() {
    let mut crew = Crew::new();
    assert!(!crew.lean(), "a new crew keeps what its workers hold");
    assert!(!crew.adopt_band(PressureBand::Normal));
    for band in PressureBand::ALL
        .into_iter()
        .filter(|band| *band != PressureBand::Normal)
    {
        assert!(
            crew.adopt_band(band),
            "{band:?} owes the waiting workers a wake"
        );
        assert!(crew.lean());
        assert!(!crew.adopt_band(band), "the same band owes none again");
        assert!(
            !crew.adopt_band(PressureBand::Critical),
            "a deeper band owes none either"
        );
        assert!(!crew.adopt_band(PressureBand::Normal));
        assert!(!crew.lean(), "plentiful memory is kept again");
    }
}

/// A reader stuck on one volume holds back only that volume's reads: a read
/// wanted on another is handed out beside it, and the stuck volume's next
/// waits for its lane, as the desktop's pickers and the file manager take
/// them.
#[test]
fn a_hung_volume_holds_back_only_its_own_reads() {
    let routes = installed();
    let mut serving = Serving::new();
    let mut desk = crate::ListingDesk::<u8>::new();
    let free = |serving: &Serving, wanted: &[String]| serving.is_free(routes.lane_of(wanted));
    let _ = desk.take(1, &parts(&["Storage", "stick"]));
    let hung = desk
        .next_job_where(|wanted| free(&serving, wanted))
        .expect("the stick's read");
    serving.begin(routes.lane_of(hung.target()));
    let _ = desk.take(2, &parts(&["Storage", "stick", "a"]));
    let _ = desk.take(3, &parts(&["Users", "ada"]));
    let next = desk.next_job_where(|wanted| free(&serving, wanted));
    assert_eq!(
        next.as_ref().map(crate::ListingJob::client),
        Some(3),
        "the home folder's read is not held behind the stick's"
    );
    assert!(
        desk.next_job_where(|wanted| free(&serving, wanted))
            .is_none(),
        "the stick's next waits for its lane"
    );
    serving.end(routes.lane_of(hung.target()));
    assert_eq!(
        desk.next_job_where(|wanted| free(&serving, wanted))
            .map(|job| job.client()),
        Some(2)
    );
}

/// A picture stuck on one volume holds back only that volume's pictures: a
/// bundle's own icon wanted on another — a carried own picture or a menu row's
/// bundle may name any volume — is decoded beside it, and the stuck volume's
/// next waits for its lane, as the desktop's artwork readers take them.
#[test]
fn a_stuck_volume_s_picture_holds_back_no_other_volume_s() {
    let routes = installed();
    let mut serving = Serving::new();
    let mut desk = tairix_icon::ArtworkDesk::<Lane>::new(crate::tests::DESK_KEYS);
    let bundle = |dir: &str| ArtworkKey::Bundle {
        dir: String::from(dir),
        version: None,
    };
    let route = |job: &tairix_icon::ArtworkJob| routes.lane_of_artwork(&job.key);
    let _ = desk.collect(&bundle("/Storage/stick/One.app"), 48);
    let (stuck, _) = desk
        .next_job_where(route, |lane| serving.is_free(*lane))
        .expect("the stick's picture");
    serving.begin(stuck);
    let _ = desk.collect(&bundle("/Storage/stick/Two.app"), 48);
    let _ = desk.collect(&bundle("/Users/ada/Desktop/Three.app"), 48);
    assert_eq!(
        desk.next_job_where(route, |lane| serving.is_free(*lane))
            .map(|(lane, job)| (lane, job.key)),
        Some((
            Lane::Volume([1; 16]),
            bundle("/Users/ada/Desktop/Three.app")
        )),
        "the home volume's picture is not held behind the stick's"
    );
    assert!(
        desk.next_job_where(route, |lane| serving.is_free(*lane))
            .is_none(),
        "the stick's next waits for its lane"
    );
    serving.end(stuck);
    assert_eq!(
        desk.next_job_where(route, |lane| serving.is_free(*lane))
            .map(|(_, job)| job.key),
        Some(bundle("/Storage/stick/Two.app"))
    );
}
