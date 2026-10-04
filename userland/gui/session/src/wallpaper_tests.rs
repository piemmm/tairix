//! Unit tests for the wallpaper desk's policy.
//!
//! The handshake, the staleness rule, the deduplication that stops one picture
//! being prepared twice at once, and the colour-only shortcut — all with no
//! thread, no lock, and no sandbox.

use super::*;

use alloc::string::String;

use alloc::vec::Vec;
use tairix_raster::Color;
use tairix_wallpaper::WallpaperPath;

fn image(path: &str) -> WallpaperSource {
    WallpaperSource {
        choice: WallpaperChoice::Image(WallpaperPath::new(path).expect("a valid wallpaper path")),
        fit: WallpaperFit::default(),
        width: 800,
        height: 600,
    }
}

fn colour_only() -> WallpaperSource {
    WallpaperSource {
        choice: WallpaperChoice::None,
        fit: WallpaperFit::default(),
        width: 800,
        height: 600,
    }
}

/// The backdrop source the desk handed out, failing the test for any other
/// kind of job.
fn backdrop(desk: &mut WallpaperDesk) -> Option<WallpaperSource> {
    match desk.next_job() {
        Some(WallpaperJob::Backdrop(source)) => Some(source),
        Some(WallpaperJob::Preview(_)) => panic!("a preview was handed out, not the backdrop"),
        Some(WallpaperJob::Slide(_)) => panic!("a slide was handed out, not the backdrop"),
        None => None,
    }
}

fn screen(source: &WallpaperSource) -> Surface {
    Surface::filled(
        source.width,
        source.height,
        Color::rgba(1, 2, 3, 255).premultiply(),
    )
    .expect("a screen-sized surface")
}

#[test]
fn a_colour_only_choice_never_reaches_a_preparer() {
    let mut desk = WallpaperDesk::new();
    assert!(matches!(
        desk.take(&colour_only()),
        Prepared::Ready {
            surface: None,
            refusal: None
        }
    ));
    assert!(!desk.has_work(), "a colour-only backdrop asked for work");
    assert!(backdrop(&mut desk).is_none());
}

#[test]
fn a_first_ask_records_the_request_and_answers_pending() {
    let mut desk = WallpaperDesk::new();
    let wanted = image("/System/Graphics/Wallpapers/Space/low-orbit.jpg");
    assert!(matches!(desk.take(&wanted), Prepared::Pending));
    assert!(desk.has_work());
    assert_eq!(backdrop(&mut desk), Some(wanted));
}

#[test]
fn asking_again_while_a_preparer_holds_it_starts_no_second_preparation() {
    let mut desk = WallpaperDesk::new();
    let wanted = image("/a.png");
    assert!(matches!(desk.take(&wanted), Prepared::Pending));
    assert!(backdrop(&mut desk).is_some());
    assert!(matches!(desk.take(&wanted), Prepared::Pending));
    assert!(
        backdrop(&mut desk).is_none(),
        "a preparation in progress was handed out twice"
    );
}

#[test]
fn a_prepared_surface_is_served_once_and_installed() {
    let mut desk = WallpaperDesk::new();
    let wanted = image("/a.png");
    assert!(matches!(desk.take(&wanted), Prepared::Pending));
    let job = backdrop(&mut desk).expect("a job");
    let painted = screen(&job);
    let pixels = painted.pixels().to_vec();
    assert!(desk.deliver(job, Ok(painted)));

    let Prepared::Ready {
        surface: Some(surface),
        refusal: None,
    } = desk.take(&wanted)
    else {
        panic!("the prepared surface was not served");
    };
    assert_eq!(surface.pixels(), pixels.as_slice());
    // Consumed: the desktop has installed it, so asking again means it wants
    // the picture prepared afresh.
    assert!(matches!(desk.take(&wanted), Prepared::Pending));
}

#[test]
fn a_refusal_is_served_as_the_backdrop_colour() {
    let mut desk = WallpaperDesk::new();
    let wanted = image("/missing.png");
    assert!(matches!(desk.take(&wanted), Prepared::Pending));
    let job = backdrop(&mut desk).expect("a job");
    assert!(desk.deliver(job, Err(String::from("unreadable"))));
    let Prepared::Ready {
        surface: None,
        refusal: Some(reason),
    } = desk.take(&wanted)
    else {
        panic!("a refusal must be served with its reason");
    };
    assert_eq!(reason, "unreadable");
}

#[test]
fn a_surface_prepared_for_a_screen_the_desktop_left_is_never_painted() {
    let mut desk = WallpaperDesk::new();
    let small = image("/a.png");
    let mut large = small.clone();
    large.width = 1920;
    large.height = 1080;

    assert!(matches!(desk.take(&small), Prepared::Pending));
    let job = backdrop(&mut desk).expect("a job");
    // The screen mode changes while the picture is being fitted to the old one.
    assert!(matches!(desk.take(&large), Prepared::Pending));
    assert!(
        !desk.deliver(job.clone(), Ok(screen(&job))),
        "an abandoned preparation must report that nobody wants it"
    );
    assert!(matches!(desk.take(&large), Prepared::Pending));
    assert_eq!(
        backdrop(&mut desk),
        Some(large),
        "the new screen was not queued"
    );
}

#[test]
fn switching_to_a_colour_only_backdrop_discards_a_prepared_picture() {
    let mut desk = WallpaperDesk::new();
    let wanted = image("/a.png");
    assert!(matches!(desk.take(&wanted), Prepared::Pending));
    let job = backdrop(&mut desk).expect("a job");
    assert!(desk.deliver(job.clone(), Ok(screen(&job))));

    // The user turns the wallpaper off before the answer is collected: the
    // prepared pixels must not survive to be painted later.
    assert!(matches!(
        desk.take(&colour_only()),
        Prepared::Ready {
            surface: None,
            refusal: None
        }
    ));
    assert!(matches!(desk.take(&wanted), Prepared::Pending));
}

#[test]
fn stopping_hands_out_no_more_work() {
    let mut desk = WallpaperDesk::new();
    assert!(matches!(desk.take(&image("/a.png")), Prepared::Pending));
    assert!(desk.has_work());
    desk.stop();
    assert!(desk.stopping());
    assert!(!desk.has_work());
    assert!(backdrop(&mut desk).is_none());
}

#[test]
fn the_wanted_source_is_derived_from_the_settings_and_the_screen() {
    let settings = DesktopSettings::default();
    let wanted = WallpaperSource::wanted(&settings, Rect::new(0, 0, 1280, 720));
    assert_eq!(wanted.width, 1280);
    assert_eq!(wanted.height, 720);
    assert_eq!(wanted.fit, settings.fit);
    assert_eq!(wanted.choice, settings.wallpaper);
    // The shipped default is an image, so a fresh account really does prepare
    // one — this is the path a first login takes.
    assert!(wanted.image_path().is_some());
}

/// The defect the listing desk had in the same shape: a preparer that hands
/// itself the same picture for ever.
///
/// A hand-out clones the source rather than taking it, so the request outlived
/// its own answer and the desk became workable again the instant it was
/// answered. Here each turn round that loop is a whole-screen read, decode and
/// resample, so it is the more expensive of the two.
#[test]
fn an_answered_preparation_is_never_handed_out_again() {
    let mut desk = WallpaperDesk::new();
    let wanted = image("/a.png");
    assert!(matches!(desk.take(&wanted), Prepared::Pending));
    let job = backdrop(&mut desk).expect("a job");
    let painted = screen(&job);
    assert!(desk.deliver(job, Ok(painted)));

    assert!(
        !desk.has_work(),
        "the answered preparation must not make the desk workable again"
    );
    assert!(
        backdrop(&mut desk).is_none(),
        "a preparer looking for work after answering must find none and park"
    );

    // And the surface is still there to be installed.
    assert!(matches!(
        desk.take(&wanted),
        Prepared::Ready {
            surface: Some(_),
            refusal: None
        }
    ));
}

/// A preparation the desktop has moved on from leaves its *newer* request
/// standing, so the abandoned picture costs one wasted decode and not a stall.
#[test]
fn a_stale_preparation_does_not_clear_the_newer_request() {
    let mut desk = WallpaperDesk::new();
    let first = image("/a.png");
    let second = image("/b.png");
    assert!(matches!(desk.take(&first), Prepared::Pending));
    let job = backdrop(&mut desk).expect("a job");
    let painted = screen(&job);

    assert!(matches!(desk.take(&second), Prepared::Pending));
    assert!(
        !desk.deliver(job, Ok(painted)),
        "an abandoned preparation owes no wake"
    );

    assert!(desk.has_work(), "the newer request is still owed a decode");
    assert_eq!(backdrop(&mut desk), Some(second));
}

/// A region that holds nothing, for the jobs whose drawing is not the point.
struct Nowhere;

impl PreviewTarget for Nowhere {
    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut []
    }
}

/// A region counting how many of its kind have been let go.
struct Counted(alloc::sync::Arc<core::sync::atomic::AtomicUsize>);

impl PreviewTarget for Counted {
    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut []
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
}

/// The client `serial` names.
fn client(serial: u8) -> ProcId {
    ProcId::from_raw([serial; 16])
}

/// `client`'s request for wallpaper `index` in `window_id`.
fn request(client: ProcId, window_id: u64, index: u16) -> PreviewRequest {
    PreviewRequest {
        window_id,
        client,
        size: PreviewSize {
            subject: PreviewSubject::Wallpaper(index),
            width: 160,
            height: 90,
        },
    }
}

/// A job for `request`, drawn nowhere.
fn job_for(request: PreviewRequest) -> PreviewJob {
    PreviewJob {
        request,
        path: String::from("/System/Graphics/Wallpapers/Space/low-orbit.jpg"),
        bound: tairix_wallpaper::MAX_WALLPAPER_BYTES,
        target: Box::new(Nowhere),
    }
}

/// Wallpaper `index` for `window_id`, each window its own client's.
fn preview(window_id: u64, index: u16) -> PreviewJob {
    let serial = u8::try_from(window_id).expect("a small window id");
    job_for(request(client(serial), window_id, index))
}

/// What a test render holds before its worker has planned it.
const PREPARATION: u64 = 1 << 20;

/// A budget `preparers` unplanned renders fit within together.
fn roomy(preparers: usize) -> PreviewBudget {
    PreviewBudget::of_machine(u64::MAX / 4, PressureBand::Normal, preparers, PREPARATION)
}

/// The budget of a machine of `total` bytes in `band`, on `preparers` threads.
fn machine(total: u64, band: PressureBand, preparers: usize) -> PreviewBudget {
    PreviewBudget::of_machine(total, band, preparers, PREPARATION)
}

/// The picture the user is looking at never waits behind a thumbnail.
#[test]
fn the_backdrop_is_handed_out_before_a_waiting_preview() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(4));
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    let wanted = image("/a.png");
    assert!(matches!(desk.take(&wanted), Prepared::Pending));

    assert_eq!(
        backdrop(&mut desk),
        Some(wanted),
        "a thumbnail was preferred to the desktop's own picture"
    );
    assert!(matches!(desk.next_job(), Some(WallpaperJob::Preview(_))));
}

/// The preview a preparer takes next, failing for any other job.
fn rendering(desk: &mut WallpaperDesk) -> Option<PreviewJob> {
    match desk.next_job() {
        Some(WallpaperJob::Preview(job)) => Some(job),
        Some(_) => panic!("a preview was expected"),
        None => None,
    }
}

/// Answer `job` with a picture of the right size.
fn answer(desk: &mut WallpaperDesk, job: &PreviewJob) -> bool {
    desk.deliver_preview(PreviewDone {
        request: job.request,
        outcome: PreviewOutcome::Rendered,
    })
}

#[test]
fn a_single_slot_renders_one_preview_at_a_time() {
    let mut desk = WallpaperDesk::new();
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    assert_eq!(
        desk.want_preview(preview(7, 1)),
        Err(Errno::LimitExceeded),
        "a second preview was queued behind the first"
    );
    let job = rendering(&mut desk).expect("the preview is handed out");
    assert_eq!(job.request.size.subject, PreviewSubject::Wallpaper(0));
    assert_eq!(
        desk.want_preview(preview(7, 1)),
        Err(Errno::LimitExceeded),
        "a preview was accepted while one was still rendering"
    );
    assert!(answer(&mut desk, &job));
    assert!(desk.take_preview().is_some());
    assert_eq!(
        desk.want_preview(preview(7, 1)),
        Ok(()),
        "the slot never freed"
    );
}

#[test]
fn as_many_previews_render_at_once_as_there_are_slots() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(3));
    for index in 0..3 {
        assert_eq!(desk.want_preview(preview(7, index)), Ok(()));
    }
    assert_eq!(
        desk.want_preview(preview(7, 3)),
        Err(Errno::LimitExceeded),
        "a window held more previews than render at once"
    );
    let taken: Vec<_> = core::iter::from_fn(|| rendering(&mut desk)).collect();
    let subjects: Vec<_> = taken.iter().map(|job| job.request.size.subject).collect();
    assert_eq!(
        subjects,
        [0, 1, 2].map(PreviewSubject::Wallpaper),
        "previews render in the order they were asked for"
    );
    assert!(answer(&mut desk, &taken[1]));
    assert!(desk.take_preview().is_some());
    assert_eq!(desk.want_preview(preview(7, 3)), Ok(()));
    assert_eq!(
        rendering(&mut desk).map(|job| job.request.size.subject),
        Some(PreviewSubject::Wallpaper(3))
    );
}

/// The bound was counted per window, so one client opening windows could
/// queue slots' worth of decodes, and hold that many regions mapped, in each.
#[test]
fn one_client_shares_its_slots_across_all_its_windows() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(2));
    let ann = client(1);
    assert_eq!(desk.want_preview(job_for(request(ann, 7, 0))), Ok(()));
    assert_eq!(desk.want_preview(job_for(request(ann, 9, 0))), Ok(()));
    assert_eq!(
        desk.want_preview(job_for(request(ann, 11, 0))),
        Err(Errno::LimitExceeded),
        "a third window bought the client a third render"
    );
    assert_eq!(
        desk.want_preview(job_for(request(client(2), 12, 0))),
        Ok(()),
        "another client's share is its own"
    );
}

/// Admission is answered before any region is mapped, and refuses exactly as
/// queueing would.
#[test]
fn admission_answers_what_queueing_would_without_taking_anything() {
    let mut desk = WallpaperDesk::new();
    let ann = client(1);
    assert_eq!(desk.admits(&request(ann, 7, 0)), Ok(()));
    assert!(!desk.has_work(), "asking queued nothing");
    assert_eq!(desk.want_preview(job_for(request(ann, 7, 0))), Ok(()));
    assert_eq!(desk.admits(&request(ann, 7, 0)), Err(Errno::AlreadyExists));
    assert_eq!(desk.admits(&request(ann, 9, 1)), Err(Errno::LimitExceeded));
    desk.stop();
    assert_eq!(desk.admits(&request(client(2), 8, 0)), Err(Errno::Busy));
}

/// The client's region is let go before the desk hears the render concluded,
/// and a closed window's queued jobs are handed back so theirs go outside it.
#[test]
fn a_previews_region_is_let_go_when_it_lands_or_is_withdrawn() {
    let released = alloc::sync::Arc::new(core::sync::atomic::AtomicUsize::new(0));
    let counted = |window| PreviewJob {
        target: Box::new(Counted(alloc::sync::Arc::clone(&released))),
        ..job_for(request(client(1), window, 0))
    };
    let done = land_preview(counted(7), PreviewOutcome::Refused);
    assert_eq!(done.outcome, PreviewOutcome::Refused);
    assert_eq!(released.load(core::sync::atomic::Ordering::Relaxed), 1);

    let mut desk = WallpaperDesk::new();
    assert_eq!(desk.want_preview(counted(8)), Ok(()));
    let withdrawn = desk.forget_window(8);
    assert_eq!(
        released.load(core::sync::atomic::Ordering::Relaxed),
        1,
        "not yet"
    );
    drop(withdrawn);
    assert_eq!(released.load(core::sync::atomic::Ordering::Relaxed), 2);
}

/// A window may not queue more than render at once, so another window's
/// preview waits behind at most that many of its pictures.
#[test]
fn one_window_cannot_hold_another_back() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(2));
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    assert_eq!(desk.want_preview(preview(7, 1)), Ok(()));
    assert_eq!(desk.want_preview(preview(8, 0)), Ok(()));
    assert_eq!(desk.want_preview(preview(7, 2)), Err(Errno::LimitExceeded));

    let first = rendering(&mut desk).expect("a first render");
    let _second = rendering(&mut desk).expect("a second render");
    assert!(
        rendering(&mut desk).is_none(),
        "more previews rendered at once than there are slots"
    );
    assert!(answer(&mut desk, &first));
    assert_eq!(
        rendering(&mut desk).map(|job| job.request.window_id),
        Some(8),
        "the other window's preview was not next"
    );
}

#[test]
fn a_picture_already_pending_is_not_asked_for_twice() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(4));
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    assert_eq!(desk.want_preview(preview(7, 0)), Err(Errno::AlreadyExists));
    let job = rendering(&mut desk).expect("the preview is handed out");
    assert_eq!(desk.want_preview(preview(7, 0)), Err(Errno::AlreadyExists));
    assert_eq!(
        desk.want_preview(preview(8, 0)),
        Ok(()),
        "another window's picture is its own"
    );
    assert!(answer(&mut desk, &job));
    assert!(desk.take_preview().is_some());
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()), "a fresh render");
}

/// Short memory narrows the slots; what is already rendering finishes and
/// nothing more is handed out until it has.
#[test]
fn lowering_the_slots_recalls_nothing() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(3));
    for index in 0..3 {
        assert_eq!(desk.want_preview(preview(7, index)), Ok(()));
    }
    let first = rendering(&mut desk).expect("a render");
    let second = rendering(&mut desk).expect("a render");
    desk.set_budget(roomy(1));
    assert!(rendering(&mut desk).is_none());
    assert!(answer(&mut desk, &first));
    assert!(
        rendering(&mut desk).is_none(),
        "the slot bound was exceeded"
    );
    assert!(answer(&mut desk, &second));
    assert_eq!(
        rendering(&mut desk).map(|job| job.request.size.subject),
        Some(PreviewSubject::Wallpaper(2))
    );
}

/// Memory recovering raises the slots while previews wait; the preparers the
/// new slots are for are parked, so the desk says they must be woken, or the
/// waiting previews would go on rendering one at a time.
#[test]
fn raising_the_slots_over_waiting_previews_asks_for_a_wake() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(3));
    for index in 0..3 {
        assert_eq!(desk.want_preview(preview(7, index)), Ok(()));
    }
    desk.set_budget(roomy(1));
    let _first = rendering(&mut desk).expect("a render");
    assert!(rendering(&mut desk).is_none(), "one slot rendered two");
    assert!(
        desk.set_budget(roomy(3)),
        "raised slots over waiting previews"
    );
    assert!(rendering(&mut desk).is_some());
    assert!(rendering(&mut desk).is_some());

    assert!(
        !desk.set_budget(roomy(4)),
        "nothing waits, so no wake is owed"
    );
    assert!(!desk.set_budget(roomy(1)), "lowering never asks for a wake");
}

/// Each preparer kept its sandbox worker for the life of the session, so a
/// burst left one process per CPU resident under memory pressure too.
#[test]
fn only_the_turn_to_lean_asks_idle_preparers_to_let_their_workers_go() {
    let mut desk = WallpaperDesk::new();
    assert!(
        desk.lean(),
        "a desk not told its machine renders one at a time"
    );
    assert!(!desk.set_budget(roomy(4)), "plentiful: nothing to let go");
    assert!(!desk.lean());
    let short = machine(1 << 30, PressureBand::Mild, 4);
    assert!(desk.set_budget(short), "memory became short");
    assert!(desk.lean());
    assert!(!desk.set_budget(short), "still short: nobody to wake again");
    assert!(
        !desk.set_budget(roomy(4)),
        "plentiful again: nothing to let go"
    );
    assert!(!desk.lean());
}

#[test]
fn a_desk_always_has_a_slot() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(0));
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    assert!(rendering(&mut desk).is_some());
}

#[test]
fn a_rendered_preview_is_handed_over_once() {
    let mut desk = WallpaperDesk::new();
    assert_eq!(desk.want_preview(preview(7, 3)), Ok(()));
    let job = rendering(&mut desk).expect("the preview is handed out");
    assert!(desk.deliver_preview(PreviewDone {
        request: job.request,
        outcome: PreviewOutcome::Rendered,
    }));
    let done = desk.take_preview().expect("the rendered preview");
    assert_eq!(done.request, job.request);
    assert_eq!(done.outcome, PreviewOutcome::Rendered);
    assert!(
        desk.take_preview().is_none(),
        "the same preview was handed over twice"
    );
}

#[test]
fn rendered_previews_are_handed_over_in_the_order_they_finished() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(2));
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    assert_eq!(desk.want_preview(preview(7, 1)), Ok(()));
    let first = rendering(&mut desk).expect("a render");
    let second = rendering(&mut desk).expect("a render");
    assert!(answer(&mut desk, &second));
    assert!(answer(&mut desk, &first));
    let order: Vec<_> = core::iter::from_fn(|| desk.take_preview())
        .map(|done| done.request.size.subject)
        .collect();
    assert_eq!(order, [1, 0].map(PreviewSubject::Wallpaper));
}

/// Nothing is recalled, so the slot has to free itself: an answer to a
/// request whose window has since closed is still the answer to it.
#[test]
fn an_answer_frees_the_slot_however_stale_its_window() {
    let mut desk = WallpaperDesk::new();
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    let job = rendering(&mut desk).expect("the preview is handed out");
    assert!(desk.deliver_preview(PreviewDone {
        request: job.request,
        outcome: PreviewOutcome::Refused,
    }));
    assert!(desk.take_preview().is_some());
    assert_eq!(
        desk.want_preview(preview(8, 1)),
        Ok(()),
        "a refused render left the slot stuck"
    );
}

/// An answer the desk is not rendering is dropped rather than handed over,
/// so a preparer answering twice cannot conclude a request nobody made.
#[test]
fn an_answer_to_nothing_is_dropped() {
    let mut desk = WallpaperDesk::new();
    assert!(!desk.deliver_preview(PreviewDone {
        request: preview(7, 0).request,
        outcome: PreviewOutcome::Rendered,
    }));
    assert!(desk.take_preview().is_none());
}

/// A window that keeps closing and reopening may not leave the decodes it
/// asked for queued ahead of another window's.
#[test]
fn a_closed_windows_waiting_previews_are_withdrawn() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(2));
    for window in 10..40 {
        assert_eq!(desk.want_preview(preview(window, 0)), Ok(()));
        assert_eq!(desk.want_preview(preview(window, 1)), Ok(()));
        assert_eq!(desk.forget_window(window).len(), 2, "both handed back");
    }
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    assert_eq!(
        rendering(&mut desk).map(|job| job.request.window_id),
        Some(7),
        "a closed window's preview was rendered first"
    );
    assert!(
        rendering(&mut desk).is_none(),
        "a withdrawn preview was still queued"
    );
}

/// A render already taken cannot be recalled, so it still holds and frees
/// its slot, and what it rendered is not handed to anyone.
#[test]
fn a_render_under_way_when_its_window_closes_finishes_into_nothing() {
    let mut desk = WallpaperDesk::new();
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    let job = rendering(&mut desk).expect("the preview is handed out");
    assert!(
        desk.forget_window(7).is_empty(),
        "a taken render is not recalled"
    );
    assert_eq!(desk.want_preview(preview(8, 0)), Ok(()));
    assert!(
        rendering(&mut desk).is_none(),
        "more rendered at once than there are slots"
    );
    assert!(!answer(&mut desk, &job), "its answer is kept for no one");
    assert!(desk.take_preview().is_none());
    assert_eq!(
        rendering(&mut desk).map(|job| job.request.window_id),
        Some(8)
    );
}

#[test]
fn a_closed_windows_rendered_previews_are_not_handed_over() {
    let mut desk = WallpaperDesk::new();
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    let job = rendering(&mut desk).expect("the preview is handed out");
    assert!(answer(&mut desk, &job));
    assert!(desk.forget_window(7).is_empty());
    assert!(desk.take_preview().is_none());
}

/// A rendered preview the serve loop has not yet handed over is still
/// pending: its window has not been answered.
#[test]
fn a_rendered_preview_not_yet_handed_over_is_still_pending() {
    let mut desk = WallpaperDesk::new();
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    let job = rendering(&mut desk).expect("the preview is handed out");
    assert!(answer(&mut desk, &job));
    assert_eq!(desk.want_preview(preview(7, 0)), Err(Errno::AlreadyExists));
    assert_eq!(desk.want_preview(preview(7, 1)), Err(Errno::LimitExceeded));
    assert!(desk.take_preview().is_some());
    assert_eq!(desk.want_preview(preview(7, 1)), Ok(()));
}

#[test]
fn a_stopped_desk_takes_no_preview() {
    let mut desk = WallpaperDesk::new();
    desk.stop();
    assert_eq!(desk.want_preview(preview(7, 0)), Err(Errno::Busy));
    assert!(desk.next_job().is_none());
}

/// The slide a preparer takes next, failing for any other job.
fn slide(desk: &mut WallpaperDesk) -> Option<WallpaperSource> {
    match desk.next_job() {
        Some(WallpaperJob::Slide(source)) => Some(source),
        Some(WallpaperJob::Backdrop(_)) => panic!("the backdrop was handed out, not a slide"),
        Some(WallpaperJob::Preview(_)) => panic!("a preview was handed out, not a slide"),
        None => None,
    }
}

#[test]
fn a_wanted_slide_is_prepared_once_and_handed_over_once() {
    let mut desk = WallpaperDesk::new();
    let one = image("/System/Graphics/Wallpapers/one.jpg");
    desk.want_slide(one.clone());
    assert!(desk.has_work());
    assert_eq!(slide(&mut desk), Some(one.clone()));
    assert!(
        !desk.has_work(),
        "a slide in preparation is not handed out twice"
    );
    assert!(desk.deliver_slide(&one, Ok(screen(&one))));
    assert!(desk.take_slide().is_some_and(|outcome| outcome.is_ok()));
    assert!(desk.take_slide().is_none());
}

#[test]
fn the_backdrop_is_prepared_before_a_slide() {
    let mut desk = WallpaperDesk::new();
    let paper = image("/Users/ada/paper.png");
    let one = image("/System/Graphics/Wallpapers/one.jpg");
    desk.want_slide(one.clone());
    assert!(matches!(desk.take(&paper), Prepared::Pending));
    assert_eq!(backdrop(&mut desk), Some(paper));
    assert_eq!(slide(&mut desk), Some(one));
}

/// A slide finished after the screensaver went down owes the loop nothing.
#[test]
fn a_slide_for_a_screensaver_that_went_down_is_dropped() {
    let mut desk = WallpaperDesk::new();
    let one = image("/System/Graphics/Wallpapers/one.jpg");
    desk.want_slide(one.clone());
    assert_eq!(slide(&mut desk), Some(one.clone()));
    desk.forget_slides();
    assert!(!desk.deliver_slide(&one, Ok(screen(&one))));
    assert!(desk.take_slide().is_none());
}

/// Only the newest slide asked for is worth preparing.
#[test]
fn a_newer_slide_replaces_one_not_yet_taken() {
    let mut desk = WallpaperDesk::new();
    let (one, two) = (
        image("/System/Graphics/Wallpapers/one.jpg"),
        image("/System/Graphics/Wallpapers/two.jpg"),
    );
    desk.want_slide(one);
    desk.want_slide(two.clone());
    assert_eq!(slide(&mut desk), Some(two));
    assert_eq!(slide(&mut desk), None);
}

/// A subject resolves only to a picture the desktop ships itself: a catalog
/// position it listed, or a screensaver's own preview, each read under the
/// bound its kind of picture is held to.
#[test]
fn a_preview_subject_resolves_only_to_a_shipped_picture() {
    let catalog = [tairix_window::WallpaperName {
        category: String::from("Space"),
        file: String::from("low-orbit.jpg"),
    }];
    assert_eq!(
        preview_source(PreviewSubject::Wallpaper(0), &catalog),
        Some((
            String::from("/System/Graphics/Wallpapers/Space/low-orbit.jpg"),
            tairix_wallpaper::MAX_WALLPAPER_BYTES
        ))
    );
    assert_eq!(preview_source(PreviewSubject::Wallpaper(1), &catalog), None);
    for kind in tairix_wallpaper::ScreensaverKind::ALL {
        assert_eq!(
            preview_source(PreviewSubject::Screensaver(kind), &catalog),
            Some((
                tairix_wallpaper::preview_path(kind),
                tairix_wallpaper::MAX_SCREENSAVER_PREVIEW_BYTES
            ))
        );
    }
    let request = preview(7, 0).request;
    assert_eq!(request.pixel_bytes(), Some(160 * 90 * 4));
}

/// Eight renders at once each holding a whole picture and its decode is what
/// ran a small machine out of memory; there, the budget fits one.
#[test]
fn a_machine_short_of_memory_renders_one_preview_at_a_time() {
    let mut desk = WallpaperDesk::new();
    let small = PreviewBudget::of_machine(256 << 20, PressureBand::Normal, 8, 64 << 20);
    desk.set_budget(small);
    assert!(desk.lean());
    for index in 0..3 {
        assert_eq!(desk.want_preview(preview(7, index)), Ok(()));
    }
    let first = rendering(&mut desk).expect("one render always runs");
    assert!(
        rendering(&mut desk).is_none(),
        "a second render was let start"
    );
    assert!(answer(&mut desk, &first));
    assert!(
        rendering(&mut desk).is_some(),
        "the next starts once it is done"
    );
}

#[test]
fn a_planned_render_holds_what_its_plan_says() {
    let mut desk = WallpaperDesk::new();
    // A tenth of the machine: room for one unplanned render and half again.
    desk.set_budget(machine(PREPARATION * 15, PressureBand::Normal, 4));
    for index in 0..2 {
        assert_eq!(desk.want_preview(preview(7, index)), Ok(()));
    }
    let first = rendering(&mut desk).expect("a render");
    assert!(rendering(&mut desk).is_none(), "two preparations fit");
    assert_eq!(
        desk.acquire_preview(&first.request, PREPARATION / 4),
        Acquisition::Granted
    );
    assert!(
        rendering(&mut desk).is_some(),
        "the small plan left room for the next"
    );
}

#[test]
fn a_plan_too_large_beside_others_waits_first_in_the_queue() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(machine(PREPARATION * 25, PressureBand::Normal, 4));
    for index in 0..3 {
        assert_eq!(desk.want_preview(preview(7, index)), Ok(()));
    }
    let first = rendering(&mut desk).expect("a render");
    let second = rendering(&mut desk).expect("a render");
    let large = PREPARATION * 2;
    assert_eq!(
        desk.acquire_preview(&first.request, large),
        Acquisition::Wait
    );
    assert!(desk.requeue_preview(first, large).is_none(), "queued again");
    assert!(
        rendering(&mut desk).is_none(),
        "the preview behind it jumped the queue, or it started without room"
    );
    assert!(answer(&mut desk, &second));
    let again = rendering(&mut desk).expect("room for its plan now");
    assert_eq!(again.request.size.subject, PreviewSubject::Wallpaper(0));
    assert_eq!(
        desk.acquire_preview(&again.request, large),
        Acquisition::Granted
    );
}

/// One render always makes progress, whatever it costs.
#[test]
fn a_plan_past_the_budget_runs_with_nothing_beside_it() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(machine(PREPARATION * 25, PressureBand::Normal, 4));
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    let only = rendering(&mut desk).expect("a render");
    assert_eq!(
        desk.acquire_preview(&only.request, u64::MAX / 2),
        Acquisition::Granted,
        "with nothing else running it goes on"
    );
    assert_eq!(desk.want_preview(preview(8, 0)), Ok(()));
    assert!(
        rendering(&mut desk).is_none(),
        "a render started beside one running past the budget"
    );
    assert!(answer(&mut desk, &only));
    assert!(rendering(&mut desk).is_some(), "the queue moves on");
}

#[test]
fn a_plan_that_does_not_fit_at_critical_pressure_is_unavailable() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(4));
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    let job = rendering(&mut desk).expect("a render");
    desk.set_budget(machine(1 << 30, PressureBand::Critical, 4));
    assert_eq!(
        desk.acquire_preview(&job.request, PREPARATION),
        Acquisition::Unavailable
    );
}

#[test]
fn nothing_starts_while_memory_is_critical() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(machine(1 << 30, PressureBand::Critical, 4));
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    assert!(rendering(&mut desk).is_none());
    assert!(desk.set_budget(roomy(4)), "relief over a waiting preview");
    assert!(rendering(&mut desk).is_some());
}

/// The screen's picture and the surface made from it are memory the
/// backdrop holds beside its render.
#[test]
fn the_backdrop_holds_the_surface_it_draws_as_well_as_its_render() {
    let mut desk = WallpaperDesk::new();
    // Room for four preparations: enough for a backdrop's render and a
    // preview, not for its 800×600 surface beside them.
    desk.set_budget(machine(PREPARATION * 40, PressureBand::Normal, 4));
    let wanted = image("/a.png");
    assert!(matches!(desk.take(&wanted), Prepared::Pending));
    assert_eq!(backdrop(&mut desk), Some(wanted));
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    assert!(
        rendering(&mut desk).is_none(),
        "a preview started against memory the backdrop's surface holds"
    );
}

#[test]
fn the_desktops_own_picture_holds_its_share_of_the_budget() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(machine(PREPARATION * 15, PressureBand::Normal, 4));
    let wanted = image("/a.png");
    assert!(matches!(desk.take(&wanted), Prepared::Pending));
    assert_eq!(backdrop(&mut desk), Some(wanted.clone()));
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    assert!(
        rendering(&mut desk).is_none(),
        "a preview started beside the backdrop past the budget"
    );
    assert!(
        desk.plan_backdrop(PREPARATION / 4),
        "a small plan left room for the preview"
    );
    assert!(rendering(&mut desk).is_some());
    assert!(desk.deliver(wanted, Err(String::from("refused"))));
}

#[test]
fn a_slide_forgotten_in_preparation_holds_its_share_until_it_is_done() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(machine(PREPARATION * 15, PressureBand::Normal, 4));
    let one = image("/System/Graphics/Wallpapers/one.jpg");
    desk.want_slide(one.clone());
    assert_eq!(slide(&mut desk), Some(one.clone()));
    desk.forget_slides();
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    assert!(
        rendering(&mut desk).is_none(),
        "a preview started against memory the slide still holds"
    );
    assert!(!desk.deliver_slide(&one, Ok(screen(&one))));
    assert!(desk.take_slide().is_none());
    assert!(rendering(&mut desk).is_some());
}

#[test]
fn a_preview_short_of_memory_beside_others_runs_again_alone() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(3));
    for index in 0..3 {
        assert_eq!(desk.want_preview(preview(7, index)), Ok(()));
    }
    let first = rendering(&mut desk).expect("a render");
    let second = rendering(&mut desk).expect("a render");
    assert!(
        desk.retry_preview(first).is_none(),
        "queued again, not concluded"
    );
    assert!(
        rendering(&mut desk).is_none(),
        "the retry started beside the render it ran short beside"
    );
    assert!(answer(&mut desk, &second));
    assert!(desk.take_preview().is_some());
    let retried = rendering(&mut desk).expect("the retry runs first, alone");
    assert_eq!(retried.request.size.subject, PreviewSubject::Wallpaper(0));
    assert!(
        rendering(&mut desk).is_none(),
        "something started beside it"
    );
    let concluded = desk.retry_preview(retried).expect("short alone: concluded");
    assert!(desk.deliver_preview(land_preview(concluded, PreviewOutcome::Unavailable)));
    assert_eq!(
        desk.take_preview().map(|done| done.outcome),
        Some(PreviewOutcome::Unavailable)
    );
    assert!(rendering(&mut desk).is_some(), "the queue moves on");
}

#[test]
fn a_preview_short_of_memory_on_its_own_is_concluded_unavailable() {
    let mut desk = WallpaperDesk::new();
    assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
    let job = rendering(&mut desk).expect("a render");
    assert!(desk.retry_preview(job).is_some());
}

/// A closed window's renders a preparer had already taken are refused their
/// memory, are never queued again however they end, and answer into nothing.
#[test]
fn a_closed_windows_renders_under_way_run_no_further_and_answer_into_nothing() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(3));
    for index in 0..2 {
        assert_eq!(desk.want_preview(preview(7, index)), Ok(()));
    }
    let short = rendering(&mut desk).expect("a render");
    let crowded = rendering(&mut desk).expect("a render");
    assert!(desk.forget_window(7).is_empty(), "both were under way");
    assert_eq!(
        desk.acquire_preview(&short.request, PREPARATION),
        Acquisition::Unavailable
    );
    let ended = [
        (short, PreviewRun::Concluded(PreviewOutcome::Unavailable)),
        (crowded, PreviewRun::Deferred(PREPARATION)),
    ];
    for (job, run) in ended {
        let (outcome, back) = desk.after_preview(job, run);
        let back = back.expect("handed back to conclude, not queued again");
        assert!(!desk.deliver_preview(land_preview(back, outcome)));
    }
    assert!(desk.take_preview().is_none());
    assert!(
        rendering(&mut desk).is_none(),
        "the closed window ran again"
    );
}

/// A render the desk withheld memory from — memory critical, or the desk
/// stopping — concludes unavailable at once rather than waiting to retry.
#[test]
fn a_render_withheld_its_memory_concludes_unavailable_at_once() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(3));
    for index in 0..2 {
        assert_eq!(desk.want_preview(preview(7, index)), Ok(()));
    }
    let withheld = rendering(&mut desk).expect("a render");
    let _beside = rendering(&mut desk).expect("a render");
    desk.set_budget(machine(1 << 30, PressureBand::Critical, 3));
    assert_eq!(
        desk.acquire_preview(&withheld.request, PREPARATION),
        Acquisition::Unavailable
    );
    let (outcome, back) = desk.after_preview(withheld, PreviewRun::Withheld);
    let back = back.expect("concluded, not queued again");
    assert!(desk.deliver_preview(land_preview(back, outcome)));
    assert_eq!(
        desk.take_preview().map(|done| done.outcome),
        Some(PreviewOutcome::Unavailable)
    );
}

/// A drawn or refused render concludes as it ended.
#[test]
fn a_drawn_or_refused_render_concludes_as_it_ended() {
    let mut desk = WallpaperDesk::new();
    desk.set_budget(roomy(3));
    for outcome in [PreviewOutcome::Rendered, PreviewOutcome::Refused] {
        assert_eq!(desk.want_preview(preview(7, 0)), Ok(()));
        let job = rendering(&mut desk).expect("a render");
        let (concluded, back) = desk.after_preview(job, PreviewRun::Concluded(outcome));
        assert_eq!(concluded, outcome);
        assert!(desk.deliver_preview(land_preview(back.expect("concluded"), concluded)));
        assert_eq!(desk.take_preview().map(|done| done.outcome), Some(outcome));
    }
}

/// A region of `len` bytes.
struct Sized(Vec<u8>);

impl PreviewTarget for Sized {
    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

/// A picture is drawn into exactly its request's pixels, and a region granted
/// too small for them is drawn into not at all.
#[test]
fn a_preview_is_drawn_into_exactly_its_pixels_or_not_at_all() {
    let request = request(client(7), 7, 0);
    let pixels = request.pixel_bytes().expect("a small preview");
    let drawn = |len: usize| {
        let mut region = Sized(alloc::vec![0; len]);
        request.canvas(&mut region).map(|canvas| canvas.len())
    };
    assert_eq!(drawn(pixels + 64), Some(pixels));
    assert_eq!(drawn(pixels), Some(pixels));
    assert_eq!(drawn(pixels - 1), None);
}
