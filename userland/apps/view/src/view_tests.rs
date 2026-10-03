//! Host tests for the composed viewer: the request/answer desk, the command
//! set, the input routing, and the damage every change owes.
//!
//! The pointer tests are written the way a user drives the app — a move to
//! where the layout actually puts a thing, then a press, then a release — and
//! no test hard-codes a coordinate, so a change to the geometry moves the
//! tests with it instead of quietly clicking empty space.

use alloc::string::String;
use alloc::vec;

use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_abi::Errno;
use tairix_controls::{damage, ScrollPart, WHEEL_STEP};
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::NoArtwork;
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Pixel, Reorient, Surface};
use tairix_sandbox::imagerender::{ViewDocument, ViewFailure, ViewFormat, ViewPage, ViewRefusal};
use tairix_sandbox::SandboxError;
use tairix_theme::{TextRole, Theme, ThemeRegistry};

use super::{Command, Outcome, Refusal, View};
use crate::{Answer, Fit, Layout, Request, ZOOM_ACTUAL_PER_MILLE};

/// The window every test drives: large enough that both panels have room and
/// every band is non-empty.
const WINDOW: (u32, u32) = (1_280, 800);

/// A still picture of `width`x`height` pixels.
fn still(width: u32, height: u32) -> ViewDocument {
    ViewDocument {
        format: ViewFormat::Png,
        animated: false,
        loop_count: None,
        count: 1,
        width,
        height,
    }
}

/// An animation of `count` frames on a `width`x`height` canvas.
fn animation(count: u32, width: u32, height: u32) -> ViewDocument {
    ViewDocument {
        format: ViewFormat::Gif,
        animated: true,
        loop_count: None,
        count,
        width,
        height,
    }
}

/// A page container of `count` independent pages.
fn pages(count: u32, width: u32, height: u32) -> ViewDocument {
    ViewDocument {
        format: ViewFormat::Tiff,
        animated: false,
        loop_count: None,
        count,
        width,
        height,
    }
}

/// The face and theme every test resolves its layout through.
fn dressing() -> (ThemeRegistry, Scale) {
    (ThemeRegistry::with_builtins(), Scale::ONE)
}

/// The face for `theme` at `scale`.
fn font(theme: &Theme, scale: Scale) -> BitmapFont {
    BitmapFont::for_role(theme.fonts(), TextRole::Body, scale)
}

/// A viewer with `document` open and its first entry decoded, laid out in
/// [`WINDOW`], plus the layout and the dressing that produced it.
fn opened(document: ViewDocument) -> (View, Layout, ThemeRegistry) {
    let (registry, scale) = dressing();
    let mut view = View::new(true);
    // The open request is the only one there can be before a document lands.
    assert_eq!(view.next_request(), Some(Request::Open { open_id: 1 }));
    let theme = registry.active();
    let layout = view.layout(WINDOW.0, WINDOW.1, theme, scale, font(theme, scale));
    let mut region = damage::sink();
    let outcome = view.deliver(
        Answer::Opened {
            open_id: 1,
            opened: Ok((document, String::from("picture.png"), 4_096)),
        },
        &layout,
        &mut region,
    );
    assert!(outcome.changed);
    let layout = view.layout(WINDOW.0, WINDOW.1, theme, scale, font(theme, scale));
    (view, layout, registry)
}

/// Answer the render `view` is asking for, as a worker holding a `page_size`
/// page would: the pixels of exactly the window asked for, and the *page's
/// own* geometry — never the render extent, which is the scaled picture.
fn serve(view: &mut View, layout: &Layout, page_size: (u32, u32)) -> bool {
    serve_into(view, layout, page_size, &mut damage::sink())
}

/// [`serve`], reporting what the answer repaints into `region`.
fn serve_into(
    view: &mut View,
    layout: &Layout,
    page_size: (u32, u32),
    region: &mut Region,
) -> bool {
    let Some(Request::Show {
        page,
        extent,
        window,
        mut pixels,
    }) = view.next_request()
    else {
        return false;
    };
    pixels.clear();
    pixels.resize((window.width * window.height * 4) as usize, 0x40);
    view.deliver(
        Answer::Shown {
            page,
            extent,
            window,
            decoded: Some(ViewPage {
                index: page,
                width: page_size.0,
                height: page_size.1,
                delay_ns: 20_000_000,
            }),
            pixels,
            outcome: Ok(()),
        },
        layout,
        region,
    )
    .changed
}

/// The whole viewer painted at `layout`, as the window shows it.
fn painted(view: &View, layout: &Layout, theme: &Theme) -> Surface {
    let window = layout.window();
    let mut surface = Surface::new(window.width, window.height).expect("a window surface");
    crate::paint::render_into(
        &mut surface,
        view,
        layout,
        theme,
        Scale::ONE,
        font(theme, Scale::ONE),
        &mut NoArtwork,
    );
    surface
}

/// The pixels of `surface` inside `band`, row by row.
fn pixels_in(surface: &Surface, band: Rect) -> vec::Vec<Pixel> {
    let width = usize::try_from(surface.width()).expect("a width");
    let (left, top) = (
        usize::try_from(band.left()).expect("on the surface"),
        usize::try_from(band.top()).expect("on the surface"),
    );
    let (columns, rows) = (band.width as usize, band.height as usize);
    (top..top + rows)
        .flat_map(|y| {
            surface.pixels()[y * width + left..y * width + left + columns]
                .iter()
                .copied()
        })
        .collect()
}

/// Whether every pixel of `rect` lies in one of the rectangles `region`
/// holds, not merely inside their bounding box.
fn covers(region: &Region, rect: Rect) -> bool {
    let mut uncovered = Region::new();
    uncovered.add(rect);
    for part in region.rects() {
        uncovered.subtract(*part);
    }
    uncovered.is_empty()
}

/// A viewer with `document` open, its first entry decoded, and its first
/// window drawn.
fn drawn(document: ViewDocument) -> (View, Layout, ThemeRegistry) {
    let page_size = (document.width, document.height);
    let (mut view, layout, registry) = opened(document);
    assert!(
        serve(&mut view, &layout, page_size),
        "the first window was drawn"
    );
    (view, layout, registry)
}

/// A viewer with `document` open and drawn, then zoomed to actual size so the
/// picture overflows the canvas and can be panned.
///
/// A document too big for its window *opens* zoomed out to fit, so overflow is
/// the user's own zoom rather than the opening state; every test that pans
/// asks for it here rather than repeating the two steps.
fn overflowing(document: ViewDocument) -> (View, Layout, ThemeRegistry) {
    let page_size = (document.width, document.height);
    let (mut view, layout, registry) = drawn(document);
    let (outcome, _) = run(&mut view, &layout, Command::ActualSize);
    assert!(
        outcome.changed,
        "the picture must not already fit, or there is nothing to pan"
    );
    assert!(
        serve(&mut view, &layout, page_size),
        "the zoomed-in render landed"
    );
    (view, layout, registry)
}

/// Advance `view` to `now_ns` with the dressing a held control needs, and
/// report whether anything drawn changed.
fn tick(view: &mut View, layout: &Layout, now_ns: u64) -> bool {
    let (registry, scale) = dressing();
    let theme = registry.active();
    let mut region = damage::sink();
    view.tick(now_ns, layout, scale, theme, &mut region)
}

/// Run `command` and report the outcome plus the damage it owed.
fn run(view: &mut View, layout: &Layout, command: Command) -> (Outcome, Region) {
    let mut region = damage::sink();
    let outcome = view.run(command, layout, &mut region);
    (outcome, region)
}

// ---- the desk ----------------------------------------------------------

#[test]
fn nothing_is_asked_for_before_the_document_is_open() {
    // The property the pending state makes unrepresentable: a render before
    // an open would be a render of nothing, and would displace the open on a
    // latest-wins desk. The open stays outstanding rather than being handed
    // over once, because only the embedder knows when it holds a source.
    let mut view = View::new(true);
    assert_eq!(view.next_request(), Some(Request::Open { open_id: 1 }));
    assert_eq!(view.next_request(), Some(Request::Open { open_id: 1 }));
    assert!(view.document().is_none());
}

#[test]
fn a_document_replacing_another_costs_no_render_of_the_one_it_replaces() {
    let (mut view, layout, _registry) = drawn(still(400, 300));
    // The user chose another file, so the embedder says one is on its way.
    let open_id = view.expect_document();
    assert!(view.refusal().is_none(), "nothing has gone wrong");
    assert_eq!(
        view.next_request(),
        Some(Request::Open { open_id }),
        "the open is what is wanted, not a draw of the old picture"
    );
    let mut region = damage::sink();
    view.deliver(
        Answer::Opened {
            open_id,
            opened: Ok((still(64, 64), String::from("other.png"), 128)),
        },
        &layout,
        &mut region,
    );
    assert_eq!(view.document().expect("open").natural(), (64, 64));
    assert!(matches!(view.next_request(), Some(Request::Show { .. })));
}

#[test]
fn a_viewer_launched_with_no_document_asks_for_nothing_at_all() {
    let mut view = View::new(false);
    assert_eq!(view.next_request(), None);
    assert!(view.document().is_none());
    assert!(view.refusal().is_none(), "nothing has gone wrong yet");
}

/// The window is shown by its first present, so a viewer with nothing to draw
/// says so and its embedder withholds that present.
///
/// Two reported defects, one rule. Launched on its own, the viewer opened a
/// window and *then* asked the session's picker for a document, so an empty
/// window sat behind the chooser for as long as the user took to choose.
/// Handed a document, it showed the window at the default extent and then
/// shrank it to the picture, which reads as a flash. Both are the same
/// mistake — presenting a window whose document is not in yet — so neither
/// turns on *how* the document was asked for. What ends the wait is either
/// conclusion: the document, or the reason there is none.
#[test]
fn a_viewer_waiting_for_its_document_has_nothing_to_show() {
    // A viewer *handed* a document is waiting too, until it has been read.
    // Presenting here would put the window on screen at a size the picture
    // has not been measured against, and sizing it to the picture a moment
    // later reads as a flash.
    let handed = View::new(true);
    assert!(
        handed.nothing_to_show(),
        "a handed document has still to be read, so there is nothing to draw"
    );

    let mut waiting = View::new(false);
    assert!(waiting.nothing_to_show());
    // The user has chosen, but the document has still to be read and decoded:
    // there is nothing on the canvas yet either.
    let open_id = waiting.expect_document();
    assert!(waiting.nothing_to_show());

    let (registry, scale) = dressing();
    let theme = registry.active();
    let layout = waiting.layout(WINDOW.0, WINDOW.1, theme, scale, font(theme, scale));
    let mut region = damage::sink();
    assert!(
        waiting
            .deliver(
                Answer::Opened {
                    open_id,
                    opened: Ok((still(400, 300), String::from("picture.png"), 4_096)),
                },
                &layout,
                &mut region,
            )
            .changed
    );
    assert!(
        !waiting.nothing_to_show(),
        "the document landed, so there is a picture to show"
    );

    // And the conclusions that are a *reason* rather than a document. Each is
    // something to show — a refused ask especially, because nothing is coming
    // after it and a window withheld on it would never appear. A pick the
    // user cancelled is not among them: they chose nothing, so the embedder
    // closes that window rather than showing them a reason they already know.
    for why in [Refusal::PickRefused(Errno::AlreadyExists), Refusal::TooLong] {
        let mut refused = View::new(false);
        assert!(refused.nothing_to_show());
        assert!(refused.no_document(why));
        assert!(
            !refused.nothing_to_show(),
            "{why} left the window with nothing to show, so it would never appear"
        );
    }
}

#[test]
fn one_render_is_outstanding_at_a_time() {
    let (mut view, _layout, _registry) = opened(still(400, 300));
    assert!(matches!(view.next_request(), Some(Request::Show { .. })));
    assert_eq!(
        view.next_request(),
        None,
        "a second render is not asked for while one is in flight"
    );
}

#[test]
fn a_drawn_window_is_not_asked_for_again() {
    let (mut view, layout, _registry) = drawn(still(400, 300));
    assert_eq!(
        view.next_request(),
        None,
        "what is held is already what the state calls for"
    );
    let _ = layout;
}

#[test]
fn the_pixel_buffer_comes_back_and_is_lent_out_again() {
    // The property that keeps a pan free of allocation: the buffer the worker
    // drew into is handed back on the next request rather than a fresh one
    // being made.
    let (mut view, layout, _registry) = drawn(still(4_000, 3_000));
    // A document this big opens zoomed out to fit, so asking for actual size
    // is what moves the state here.
    let (outcome, _) = run(&mut view, &layout, Command::ActualSize);
    assert!(outcome.changed);
    let Some(Request::Show { pixels, .. }) = view.next_request() else {
        panic!("a render was asked for");
    };
    assert!(
        pixels.capacity() > 0,
        "the buffer the last render used was lent out again"
    );
}

#[test]
fn a_superseded_answer_is_dropped_rather_than_drawn() {
    let (mut view, layout, _registry) = drawn(still(4_000, 3_000));
    // Ask for one render, then change the state again so the answer describes
    // a rectangle the user has already moved away from.
    let (outcome, _) = run(&mut view, &layout, Command::ActualSize);
    assert!(outcome.changed, "a render is now called for");
    let Some(Request::Show {
        page,
        extent,
        window,
        mut pixels,
    }) = view.next_request()
    else {
        panic!("a render was asked for");
    };
    let (outcome, _) = run(&mut view, &layout, Command::FitWindow);
    assert!(outcome.changed, "the state moved on");
    pixels.resize((window.width * window.height * 4) as usize, 0xFF);
    let mut region = damage::sink();
    let delivered = view.deliver(
        Answer::Shown {
            page,
            extent,
            window,
            decoded: None,
            pixels,
            outcome: Ok(()),
        },
        &layout,
        &mut region,
    );
    assert!(!delivered.changed, "a stale answer changes nothing");
    assert!(region.is_empty(), "and owes no repaint");
}

#[test]
fn a_refused_open_states_the_reason_and_holds_no_document() {
    let (registry, scale) = dressing();
    let theme = registry.active();
    let mut view = View::new(true);
    let layout = view.layout(WINDOW.0, WINDOW.1, theme, scale, font(theme, scale));
    let mut region = damage::sink();
    let outcome = view.deliver(
        Answer::Opened {
            open_id: 1,
            opened: Err(Refusal::Failed(ViewFailure::Refused(ViewRefusal::TooLarge))),
        },
        &layout,
        &mut region,
    );
    assert!(outcome.changed);
    assert!(view.document().is_none());
    assert!(matches!(view.refusal(), Some(Refusal::Failed(_))));
    assert_eq!(
        view.next_request(),
        None,
        "nothing is asked for about a document that would not open"
    );
}

#[test]
fn a_refused_render_states_the_reason_and_keeps_showing_what_it_had() {
    let (mut view, layout, _registry) = drawn(still(4_000, 3_000));
    let (outcome, _) = run(&mut view, &layout, Command::ActualSize);
    assert!(outcome.changed);
    let Some(Request::Show {
        page,
        extent,
        window,
        pixels,
    }) = view.next_request()
    else {
        panic!("a render was asked for");
    };
    let mut region = damage::sink();
    let delivered = view.deliver(
        Answer::Shown {
            page,
            extent,
            window,
            decoded: None,
            pixels,
            outcome: Err(Refusal::Failed(ViewFailure::Sandbox(
                SandboxError::WorkerFailed,
            ))),
        },
        &layout,
        &mut region,
    );
    assert!(delivered.changed);
    assert!(matches!(view.refusal(), Some(Refusal::Failed(_))));
    assert!(
        view.document().is_some(),
        "the document is still open; only this draw failed"
    );
}

#[test]
fn a_refused_ask_is_recorded_only_while_nothing_is_open() {
    // A refused optional action is an answer, not a death: the window appears
    // stating why there is no document.
    let mut view = View::new(false);
    let why = Refusal::PickRefused(Errno::AlreadyExists);
    assert!(view.no_document(why));
    assert!(matches!(view.refusal(), Some(Refusal::PickRefused(_))));

    // With a document already open it says nothing new: the picture on screen
    // is still what the user is looking at.
    let (mut open, _layout, _registry) = drawn(still(400, 300));
    assert!(!open.no_document(why));
    assert!(open.refusal().is_none());
}

#[test]
fn an_answer_to_an_abandoned_open_is_dropped_rather_than_adopted() {
    // A window closed with a read still in flight, then a document asked for
    // in another window: the worker's answer to the first open names an id
    // this viewer has moved past, and adopting it would put one window's
    // document in another.
    let (registry, scale) = dressing();
    let theme = registry.active();
    let mut view = View::new(false);
    let layout = view.layout(WINDOW.0, WINDOW.1, theme, scale, font(theme, scale));
    let mut region = damage::sink();
    let abandoned = view.expect_document();
    let current = view.expect_document();
    assert_ne!(
        abandoned, current,
        "an open id is minted per ask, never reused"
    );

    let outcome = view.deliver(
        Answer::Opened {
            open_id: abandoned,
            opened: Ok((still(400, 300), String::from("stale.png"), 4_096)),
        },
        &layout,
        &mut region,
    );
    assert!(!outcome.changed, "a superseded answer changes nothing");
    assert!(view.document().is_none(), "and lands no document");
    assert!(region.is_empty(), "and owes no repaint");
    assert_eq!(
        view.next_request(),
        Some(Request::Open { open_id: current }),
        "the open the viewer is actually waiting for is still outstanding"
    );

    // The answer it is waiting for is adopted.
    assert!(
        view.deliver(
            Answer::Opened {
                open_id: current,
                opened: Ok((still(64, 64), String::from("wanted.png"), 128)),
            },
            &layout,
            &mut region,
        )
        .changed
    );
    assert_eq!(view.document().expect("open").natural(), (64, 64));
}

// ---- commands ----------------------------------------------------------

#[test]
fn the_zoom_tools_step_the_ladder_and_the_slider_follows() {
    let (mut view, layout, _registry) = drawn(still(4_000, 3_000));
    let before = view.viewport().zoom();
    let (outcome, region) = run(&mut view, &layout, Command::ZoomIn);
    assert!(outcome.changed);
    assert!(view.viewport().zoom() > before);
    assert_eq!(
        view.viewport().fit,
        Fit::Free,
        "a tool sets the user's factor"
    );
    assert!(
        region
            .rects()
            .iter()
            .any(|rect| *rect == layout.zoom_slider())
            || region
                .rects()
                .iter()
                .any(|rect| { !rect.intersection(&layout.zoom_slider()).is_empty() }),
        "the slider reports the zoom, so it is repainted"
    );
    assert_eq!(
        crate::slider_at_zoom(view.viewport().zoom()),
        view.zoom_control().value(),
        "the control is a view of the zoom, never a second copy of it"
    );
}

#[test]
fn actual_size_and_fit_are_different_answers() {
    let (mut view, layout, _registry) = drawn(still(4_000, 3_000));
    let (_, _) = run(&mut view, &layout, Command::ActualSize);
    assert_eq!(view.viewport().zoom(), ZOOM_ACTUAL_PER_MILLE);
    assert_eq!(view.viewport().fit, Fit::Actual);
    let (_, _) = run(&mut view, &layout, Command::FitWindow);
    assert!(
        view.viewport().zoom() < ZOOM_ACTUAL_PER_MILLE,
        "a big picture shrinks to fit"
    );
    assert_eq!(view.viewport().fit, Fit::Window);
}

#[test]
fn turning_composes_and_swaps_the_displayed_axes() {
    let (mut view, layout, _registry) = drawn(still(400, 300));
    let (outcome, _) = run(&mut view, &layout, Command::RotateRight);
    assert!(outcome.changed);
    assert_eq!(view.viewport().reorient, Reorient::QuarterTurnRight);
    assert_eq!(view.viewport().displayed((400, 300)), (300, 400));
    let (_, _) = run(&mut view, &layout, Command::RotateLeft);
    assert_eq!(
        view.viewport().reorient,
        Reorient::None,
        "back where it started"
    );
    let (_, _) = run(&mut view, &layout, Command::Mirror);
    assert_eq!(view.viewport().reorient, Reorient::FlipHorizontal);
}

#[test]
fn a_turn_is_asked_for_in_page_space_and_the_answer_is_shown_turned() {
    let (mut view, layout, _registry) = drawn(still(400, 300));
    let (_, _) = run(&mut view, &layout, Command::RotateRight);
    let Some(Request::Show { extent, window, .. }) = view.next_request() else {
        panic!("a render was asked for");
    };
    // The worker knows nothing of the turn, so both the extent and the window
    // it is sent are in the page's own axes.
    assert!(
        extent.0 >= extent.1,
        "a landscape page stays landscape in the request: {extent:?}"
    );
    assert!(window.width <= extent.0 && window.height <= extent.1);
}

#[test]
fn stepping_pages_stops_at_either_end() {
    const PAGE: (u32, u32) = (400, 300);
    let (mut view, layout, _registry) = drawn(pages(3, 400, 300));
    assert_eq!(view.document().expect("open").index(), 0);
    let (outcome, _) = run(&mut view, &layout, Command::PreviousPage);
    assert!(!outcome.changed, "already at the first page");

    for expected in 1..3 {
        let (outcome, _) = run(&mut view, &layout, Command::NextPage);
        assert!(outcome.changed, "moved to page {expected}");
        assert!(serve(&mut view, &layout, PAGE));
        assert_eq!(view.document().expect("open").index(), expected);
    }
    let (outcome, _) = run(&mut view, &layout, Command::NextPage);
    assert!(!outcome.changed, "already at the last page");
}

#[test]
fn a_page_change_keeps_the_turn_and_the_fit_but_not_the_pan() {
    const PAGE: (u32, u32) = (4_000, 3_000);
    // Actual size is asked for rather than assumed: a container this big
    // opens zoomed out to fit, and what this test turns on is that whatever
    // fit is in force survives a page change.
    let (mut view, layout, _registry) = overflowing(pages(3, 4_000, 3_000));
    let (_, _) = run(&mut view, &layout, Command::RotateRight);
    assert!(serve(&mut view, &layout, PAGE));
    let (_, _) = run(&mut view, &layout, Command::Pan { dx: 3, dy: 3 });
    assert_ne!(view.viewport().pan(), (0, 0));

    let (outcome, _) = run(&mut view, &layout, Command::NextPage);
    assert!(outcome.changed);
    assert_eq!(
        view.viewport().reorient,
        Reorient::QuarterTurnRight,
        "the turn is the user's and survives"
    );
    assert_eq!(view.viewport().fit, Fit::Actual, "so is the fit");
    assert_eq!(
        view.viewport().pan(),
        (0, 0),
        "a different page is not the same picture at the same offset"
    );
}

#[test]
fn a_still_picture_cannot_be_played() {
    let (mut view, layout, _registry) = drawn(still(400, 300));
    let (outcome, _) = run(&mut view, &layout, Command::TogglePlayback);
    assert!(!outcome.changed);
    assert!(!view.playing());
    assert_eq!(view.deadline_ns(), None, "and arms no timer");
}

#[test]
fn playback_arms_exactly_one_deadline_and_a_pause_disarms_it() {
    let (mut view, layout, _registry) = drawn(animation(4, 400, 300));
    assert_eq!(view.deadline_ns(), None, "a paused viewer arms nothing");
    let (outcome, _) = run(&mut view, &layout, Command::TogglePlayback);
    assert!(outcome.changed);
    assert!(view.playing());

    view.arm_deadline(1_000);
    let first = view.deadline_ns().expect("a deadline was armed");
    assert!(first > 1_000);
    view.arm_deadline(2_000);
    assert_eq!(
        view.deadline_ns(),
        Some(first),
        "one deadline, not one per call"
    );

    let (_, _) = run(&mut view, &layout, Command::TogglePlayback);
    assert!(!view.playing());
    assert_eq!(view.deadline_ns(), None, "a pause arms no timer at all");
}

#[test]
fn a_tick_before_the_deadline_does_nothing_and_one_after_it_steps_the_frame() {
    const PAGE: (u32, u32) = (400, 300);
    let (mut view, layout, _registry) = drawn(animation(4, 400, 300));
    let (_, _) = run(&mut view, &layout, Command::TogglePlayback);
    view.arm_deadline(0);
    let deadline = view.deadline_ns().expect("armed");
    assert!(!tick(&mut view, &layout, deadline - 1), "not due yet");
    assert!(tick(&mut view, &layout, deadline), "due");
    assert_eq!(view.deadline_ns(), None, "the deadline is spent");
    assert!(serve(&mut view, &layout, PAGE));
    assert_eq!(view.document().expect("open").index(), 1);
}

#[test]
fn playback_wraps_at_the_last_frame() {
    const PAGE: (u32, u32) = (400, 300);
    let (mut view, layout, _registry) = drawn(animation(2, 400, 300));
    let (_, _) = run(&mut view, &layout, Command::TogglePlayback);
    for expected in [1, 0, 1] {
        view.arm_deadline(0);
        let deadline = view.deadline_ns().expect("armed");
        assert!(tick(&mut view, &layout, deadline));
        assert!(serve(&mut view, &layout, PAGE));
        assert_eq!(view.document().expect("open").index(), expected);
    }
}

#[test]
fn a_paused_viewer_never_steps_however_late_the_clock_is() {
    let (mut view, layout, _registry) = drawn(animation(4, 400, 300));
    assert!(!tick(&mut view, &layout, u64::MAX));
    assert_eq!(view.document().expect("open").index(), 0);
}

// ---- damage ------------------------------------------------------------

/// A pan moves the bars' thumbs and asks for a render; until that render
/// lands the canvas and the status line draw exactly the pixels they drew
/// before, so repainting them for the pan was work spent for nothing. The
/// answer is what reports them.
#[test]
fn a_pan_repaints_its_bars_and_leaves_the_canvas_to_its_render() {
    const PAGE: (u32, u32) = (4_000, 3_000);
    let (mut view, layout, registry) = overflowing(still(4_000, 3_000));
    let before = painted(&view, &layout, registry.active());
    let (outcome, region) = run(&mut view, &layout, Command::Pan { dx: 1, dy: 1 });
    assert!(outcome.changed);
    assert!(covers(&region, layout.vertical_bar()), "the thumbs moved");
    assert!(covers(&region, layout.horizontal_bar()));
    assert!(!region.intersects(layout.canvas()));
    assert!(!region.intersects(layout.status()));

    let after = painted(&view, &layout, registry.active());
    for band in [layout.canvas(), layout.status()] {
        assert_eq!(
            pixels_in(&before, band),
            pixels_in(&after, band),
            "{band:?} draws nothing a pan changes"
        );
    }

    let mut answered = damage::sink();
    assert!(serve_into(&mut view, &layout, PAGE, &mut answered));
    assert!(covers(&answered, layout.canvas()), "the render lands in it");
}
#[test]
fn opening_a_panel_repaints_the_window_because_every_band_moved() {
    let (mut view, layout, _registry) = drawn(still(400, 300));
    let (outcome, region) = run(&mut view, &layout, Command::ToggleInfo);
    assert!(outcome.changed);
    assert!(view.info_open());
    assert!(
        region
            .rects()
            .iter()
            .any(|part| part.intersection(&layout.window()) == layout.window()),
        "the bands moved, so nothing on screen can be kept"
    );
}

#[test]
fn a_command_that_changes_nothing_reports_no_damage() {
    // The property that stops a viewer repainting on every keystroke: a
    // command whose state is already what it asks for owes nothing.
    let (mut view, layout, _registry) = drawn(still(400, 300));
    let (_, _) = run(&mut view, &layout, Command::FitWindow);
    let (outcome, region) = run(&mut view, &layout, Command::FitWindow);
    assert!(!outcome.changed);
    assert!(region.is_empty());
}

#[test]
fn a_command_with_nothing_open_touches_only_what_it_can() {
    let mut view = View::new(false);
    let (registry, scale) = dressing();
    let theme = registry.active();
    let layout = view.layout(WINDOW.0, WINDOW.1, theme, scale, font(theme, scale));
    for command in [
        Command::ZoomIn,
        Command::ZoomOut,
        Command::FitWindow,
        Command::RotateRight,
        Command::NextPage,
        Command::TogglePlayback,
        Command::Pan { dx: 1, dy: 1 },
    ] {
        let (outcome, region) = run(&mut view, &layout, command);
        assert!(!outcome.changed, "{command:?} acted on no picture");
        assert!(region.is_empty(), "{command:?} owed no repaint");
    }
    // The panel toggle acts with nothing open, because it is about the window
    // rather than about a picture.
    let (outcome, _) = run(&mut view, &layout, Command::ToggleInfo);
    assert!(outcome.changed);
    assert!(view.info_open());
}

// ---- input routing -----------------------------------------------------

#[test]
fn clicking_a_tool_runs_its_command() {
    let (mut view, layout, registry) = drawn(still(4_000, 3_000));
    let theme = registry.active();
    let tools = layout.tools();
    // The zoom-out tool is first, so its own slot is the leading one.
    let target = Point {
        x: tools.left() + tairix_geometry::to_i32(tools.height / 2),
        y: tools.top() + tairix_geometry::to_i32(tools.height / 2),
    };
    let before = view.viewport().zoom();
    let mut region = damage::sink();
    for event in [
        InputEvent::PointerMoved { to: target },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        view.on_pointer(&event, &layout, Scale::ONE, theme, &mut region);
    }
    assert!(
        view.viewport().zoom() < before,
        "the leading tool reduced the picture"
    );
}

#[test]
fn dragging_the_canvas_pans_the_picture_the_other_way() {
    let (mut view, layout, registry) = overflowing(still(4_000, 3_000));
    let theme = registry.active();
    let centre = layout.canvas().center();
    let mut region = damage::sink();
    let mut feed = |view: &mut View, event: InputEvent| {
        view.on_pointer(&event, &layout, Scale::ONE, theme, &mut region)
    };
    feed(&mut view, InputEvent::PointerMoved { to: centre });
    feed(
        &mut view,
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
    );
    let outcome = feed(
        &mut view,
        InputEvent::PointerMoved {
            to: Point {
                x: centre.x - 40,
                y: centre.y - 30,
            },
        },
    );
    assert!(outcome.changed, "the drag panned");
    // Dragging the picture leftward moves the view rightward, which is what
    // grabbing a picture and pulling it means.
    assert_eq!(view.viewport().pan(), (40, 30));
    feed(
        &mut view,
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    );
    // A move after the release is not a pan: the drag has ended.
    let outcome = feed(&mut view, InputEvent::PointerMoved { to: centre });
    assert!(!outcome.changed);
    assert_eq!(view.viewport().pan(), (40, 30));
}

/// Feed `events` to `view` at `layout`, reporting into `region`, and answer
/// the last outcome.
fn feed(
    view: &mut View,
    layout: &Layout,
    theme: &Theme,
    events: &[InputEvent],
    region: &mut Region,
) -> Outcome {
    let mut last = Outcome::changed(false);
    for event in events {
        last = view.on_pointer(event, layout, Scale::ONE, theme, region);
    }
    last
}

const PRESS: InputEvent = InputEvent::PointerPressed {
    button: PointerButton::Primary,
};

const RELEASE: InputEvent = InputEvent::PointerReleased {
    button: PointerButton::Primary,
};

/// The regression: a drag moved both thumbs but reported only the canvas and
/// the status line, so the bars showed the old pan until a render happened to
/// land. It reports the bars it moved.
#[test]
fn dragging_the_canvas_reports_the_bars_it_moves() {
    let (mut view, layout, registry) = overflowing(still(4_000, 3_000));
    let theme = registry.active();
    let centre = layout.canvas().center();
    feed(
        &mut view,
        &layout,
        theme,
        &[InputEvent::PointerMoved { to: centre }, PRESS],
        &mut damage::sink(),
    );
    let mut region = damage::sink();
    let dragged = InputEvent::PointerMoved {
        to: Point {
            x: centre.x - 40,
            y: centre.y - 30,
        },
    };
    assert!(feed(&mut view, &layout, theme, &[dragged], &mut region).changed);
    assert!(covers(&region, layout.vertical_bar()));
    assert!(covers(&region, layout.horizontal_bar()));
    assert!(
        !region.intersects(layout.canvas()),
        "the canvas waits for its render"
    );
}

/// The regression: dragging the zoom slider reported only its own knob, so
/// the canvas, the status line's magnification and the bars all showed the old
/// zoom. The slider reframes the picture as a zoom tool does.
#[test]
fn a_zoom_slider_drag_reports_what_a_zoom_redraws() {
    let (mut view, layout, registry) = overflowing(still(4_000, 3_000));
    let theme = registry.active();
    let slider = layout.zoom_slider();
    let low = Point {
        x: slider.left() + 2,
        y: slider.center().y,
    };
    let before = view.viewport().zoom();
    let mut region = damage::sink();
    let pressed = feed(
        &mut view,
        &layout,
        theme,
        &[InputEvent::PointerMoved { to: low }, PRESS],
        &mut region,
    );
    assert!(pressed.changed);
    assert!(view.viewport().zoom() < before, "the slider zoomed out");
    for band in [
        layout.canvas(),
        layout.status(),
        layout.vertical_bar(),
        layout.horizontal_bar(),
    ] {
        assert!(covers(&region, band), "{band:?} shows the zoom");
    }
}

/// A line of the canvas is one length: the bar's end button steps exactly as
/// far as an arrow key pans.
#[test]
fn a_bar_end_button_steps_as_far_as_an_arrow_key() {
    let (mut keyed, layout, registry) = overflowing(still(4_000, 3_000));
    let theme = registry.active();
    keyed.on_key(
        Key::Named(NamedKey::Down),
        Modifiers::default(),
        &layout,
        &mut damage::sink(),
    );
    let step = keyed.viewport().pan().1;
    assert_eq!(step, crate::pan_step(layout.canvas().height));

    let (mut pressed, layout, _registry) = overflowing(still(4_000, 3_000));
    let end = pressed
        .bars()
        .0
        .part_rect(
            ScrollPart::Increment,
            layout.vertical_bar(),
            Scale::ONE,
            theme,
        )
        .expect("the vertical bar draws its end button");
    feed(
        &mut pressed,
        &layout,
        theme,
        &[
            InputEvent::PointerMoved { to: end.center() },
            PRESS,
            RELEASE,
        ],
        &mut damage::sink(),
    );
    assert_eq!(pressed.viewport().pan().1, step);
}

/// A tool's tip is read through the strip's own layout, so a strip the
/// wheel scrolled names the tool now under the pointer — the tip used to be
/// the tool a fixed slot from the strip's start, whatever it showed.
#[test]
fn a_tool_tip_names_the_tool_under_the_pointer_however_the_strip_scrolled() {
    let (registry, scale) = dressing();
    let theme = registry.active();
    let face = font(theme, scale);
    let mut view = View::new(false);
    // A window too narrow for the tools, so their strip scrolls.
    let (narrow, _) = super::min_client_size(theme, scale, face);
    let layout = view.layout(narrow, WINDOW.1, theme, scale, face);
    let strip = layout.tools();
    let first = view
        .toolbar_control()
        .tool_rect(0, strip, scale, theme)
        .expect("the first tool is seated");
    let at = first.center();
    view.on_pointer(
        &InputEvent::PointerMoved { to: at },
        &layout,
        scale,
        theme,
        &mut damage::sink(),
    );
    assert_eq!(
        view.tool_tip(&layout, scale, theme),
        Some((first, super::TOOLS[0].2))
    );

    let scrolled = view.on_pointer(
        &InputEvent::PointerScrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT,
        },
        &layout,
        scale,
        theme,
        &mut damage::sink(),
    );
    assert!(scrolled.changed, "the strip had tools to scroll to");
    let under = view
        .toolbar_control()
        .tool_at(strip, scale, theme, at)
        .expect("a tool is under the pointer");
    assert_ne!(under, 0, "the wheel scrolled the first tool away");
    let (rect, tip) = view
        .tool_tip(&layout, scale, theme)
        .expect("the tool under the pointer has a tip");
    assert!(rect.contains(at));
    assert_eq!(tip, super::TOOLS[under].2);
}

#[test]
fn a_secondary_press_on_the_canvas_asks_for_the_context_menu() {
    let (mut view, layout, registry) = drawn(still(400, 300));
    let theme = registry.active();
    let centre = layout.canvas().center();
    let mut region = damage::sink();
    view.on_pointer(
        &InputEvent::PointerMoved { to: centre },
        &layout,
        Scale::ONE,
        theme,
        &mut region,
    );
    let outcome = view.on_pointer(
        &InputEvent::PointerPressed {
            button: PointerButton::Secondary,
        },
        &layout,
        Scale::ONE,
        theme,
        &mut region,
    );
    assert_eq!(outcome.menu, Some(centre));
    assert!(
        !outcome.changed,
        "the plate is the session's, not the app's"
    );
}

/// Turn the wheel by `(dx, dy)` scroll units with the pointer at `at`,
/// reporting into `region`.
fn wheel(
    view: &mut View,
    layout: &Layout,
    theme: &Theme,
    at: Point,
    (dx, dy): (i32, i32),
    region: &mut Region,
) -> Outcome {
    view.on_pointer(
        &InputEvent::PointerMoved { to: at },
        layout,
        Scale::ONE,
        theme,
        &mut damage::sink(),
    );
    view.on_pointer(
        &InputEvent::PointerScrolled { dx, dy },
        layout,
        Scale::ONE,
        theme,
        region,
    )
}

/// The pixels one wheel detent pans at 100%.
fn detent() -> u32 {
    Scale::ONE.scale_length(WHEEL_STEP)
}

/// The regression: the wheel's turn was read as a count of pan steps, so one
/// detent of scroll units panned a hundred and twenty steps of an eighth of
/// the canvas. A detent pans the desktop's one wheel distance on its own
/// axis, and the bars that moved are what it reports.
#[test]
fn a_wheel_detent_over_the_canvas_pans_one_wheel_step() {
    let (mut view, layout, registry) = overflowing(still(4_000, 3_000));
    let theme = registry.active();
    let at = layout.canvas().center();
    let mut region = damage::sink();
    let outcome = wheel(
        &mut view,
        &layout,
        theme,
        at,
        (0, SCROLL_UNITS_PER_DETENT),
        &mut region,
    );
    assert!(outcome.changed);
    assert_eq!(view.viewport().pan(), (0, detent()));
    assert!(covers(&region, layout.vertical_bar()));
    assert!(
        !region.intersects(layout.canvas()),
        "the canvas waits for its render"
    );

    wheel(
        &mut view,
        &layout,
        theme,
        at,
        (SCROLL_UNITS_PER_DETENT, 0),
        &mut damage::sink(),
    );
    assert_eq!(view.viewport().pan(), (detent(), detent()));

    // The wheel over the status line is not the canvas's.
    let outcome = wheel(
        &mut view,
        &layout,
        theme,
        layout.status().center(),
        (0, SCROLL_UNITS_PER_DETENT),
        &mut damage::sink(),
    );
    assert!(!outcome.changed);
    assert_eq!(view.viewport().pan(), (detent(), detent()));
}

/// A detent delivered a unit at a time pans as far as one delivered whole.
#[test]
fn a_fine_wheel_adds_up_to_whole_steps() {
    let (mut view, layout, registry) = overflowing(still(4_000, 3_000));
    let theme = registry.active();
    let at = layout.canvas().center();
    for _ in 0..SCROLL_UNITS_PER_DETENT {
        wheel(&mut view, &layout, theme, at, (0, 1), &mut damage::sink());
    }
    assert_eq!(view.viewport().pan(), (0, detent()));
}

/// The picture and its bar are one view: a turn over either pans the same
/// distance, and what half a turn over one leaves is made up over the other.
#[test]
fn the_wheel_pans_as_far_over_a_bar_as_over_the_canvas() {
    let (mut view, layout, registry) = overflowing(still(4_000, 3_000));
    let theme = registry.active();
    // Just over half a detent over the picture and just under half over its
    // bar make one detent only if what the first left short of a pixel is
    // carried into the second.
    let half = SCROLL_UNITS_PER_DETENT / 2;
    let over = |at: Rect| at.center();
    wheel(
        &mut view,
        &layout,
        theme,
        over(layout.canvas()),
        (0, half + 1),
        &mut damage::sink(),
    );
    wheel(
        &mut view,
        &layout,
        theme,
        over(layout.vertical_bar()),
        (0, half - 1),
        &mut damage::sink(),
    );
    assert_eq!(view.viewport().pan(), (0, detent()));
}
#[test]
fn the_keyboard_reaches_the_commands_the_tools_do() {
    let (mut view, layout, _registry) = drawn(still(4_000, 3_000));
    let mut region = damage::sink();
    let mut press =
        |view: &mut View, key: Key| view.on_key(key, Modifiers::default(), &layout, &mut region);
    press(&mut view, Key::Char('1'));
    assert_eq!(view.viewport().zoom(), ZOOM_ACTUAL_PER_MILLE);
    press(&mut view, Key::Char('0'));
    assert_eq!(view.viewport().fit, Fit::Window);
    press(&mut view, Key::Char(']'));
    assert_eq!(view.viewport().reorient, Reorient::QuarterTurnRight);
    press(&mut view, Key::Char('m'));
    assert_eq!(
        view.viewport().reorient,
        Reorient::QuarterTurnRight.then(Reorient::FlipHorizontal)
    );
    assert!(press(&mut view, Key::Char('i')).changed);
    assert!(view.info_open());
    assert!(press(&mut view, Key::Char('o')).pick, "asks for the picker");
    assert!(
        press(&mut view, Key::Named(NamedKey::Escape)).close,
        "escape closes the window"
    );
}

#[test]
fn the_arrow_keys_pan_and_shifted_they_change_page() {
    const PAGE: (u32, u32) = (4_000, 3_000);
    let (mut view, layout, _registry) = overflowing(pages(3, 4_000, 3_000));
    let mut region = damage::sink();
    let outcome = view.on_key(
        Key::Named(NamedKey::Right),
        Modifiers::default(),
        &layout,
        &mut region,
    );
    assert!(outcome.changed);
    assert_ne!(view.viewport().pan().0, 0, "an arrow key panned");
    assert_eq!(
        view.document().expect("open").index(),
        0,
        "and stayed on the page"
    );

    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let outcome = view.on_key(Key::Named(NamedKey::Right), shift, &layout, &mut region);
    assert!(outcome.changed);
    assert!(serve(&mut view, &layout, PAGE));
    assert_eq!(
        view.document().expect("open").index(),
        1,
        "shifted, it turned the page"
    );
}

#[test]
fn an_unbound_key_changes_nothing() {
    let (mut view, layout, _registry) = drawn(still(400, 300));
    let mut region = damage::sink();
    let outcome = view.on_key(Key::Char('q'), Modifiers::default(), &layout, &mut region);
    assert!(!outcome.changed);
    assert!(!outcome.close);
    assert!(!outcome.pick);
    assert!(region.is_empty());
}

// ---- the layout and the viewport stay in step -------------------------

#[test]
fn a_resize_refits_a_fitted_zoom_and_reclamps_the_pan() {
    const PAGE: (u32, u32) = (4_000, 3_000);
    let (mut view, _layout, registry) = drawn(still(4_000, 3_000));
    let theme = registry.active();
    // A document this big opens zoomed out to fit, which is the fit whose
    // refitting is the subject here.
    assert_eq!(view.viewport().fit, Fit::Window);
    let fitted = view.viewport().zoom();
    let bigger = view.layout(1_920, 1_200, theme, Scale::ONE, font(theme, Scale::ONE));
    assert!(
        view.viewport().zoom() > fitted,
        "a wider window fits the picture larger"
    );
    assert!(!bigger.canvas().is_empty());

    let (_, _) = run(&mut view, &bigger, Command::ActualSize);
    assert!(serve(&mut view, &bigger, PAGE));
    let (_, _) = run(&mut view, &bigger, Command::Pan { dx: 8, dy: 8 });
    let panned = view.viewport().pan();
    assert_ne!(panned, (0, 0));
    let _ = view.layout(3_840, 2_400, theme, Scale::ONE, font(theme, Scale::ONE));
    assert!(
        view.viewport().pan().0 <= panned.0 && view.viewport().pan().1 <= panned.1,
        "a larger canvas can reach less far, so the pan came back inside it"
    );
}

#[test]
fn a_window_with_no_canvas_asks_for_no_render() {
    let (mut view, _layout, registry) = drawn(still(400, 300));
    let theme = registry.active();
    let _ = view.layout(0, 0, theme, Scale::ONE, font(theme, Scale::ONE));
    assert_eq!(
        view.next_request(),
        None,
        "there is no rectangle to draw into"
    );
}

#[test]
fn the_scrollbars_report_the_pan_they_are_a_view_of() {
    let (mut view, layout, _registry) = overflowing(still(4_000, 3_000));
    let (_, _) = run(&mut view, &layout, Command::Pan { dx: 2, dy: 2 });
    let (vertical, horizontal) = view.bars();
    let pan = view.viewport().pan();
    assert_eq!(vertical.model().offset(), u64::from(pan.1));
    assert_eq!(horizontal.model().offset(), u64::from(pan.0));
}

#[test]
fn a_picture_that_fits_gives_its_bars_nothing_to_scroll() {
    let (view, _layout, _registry) = drawn(still(100, 100));
    let (vertical, horizontal) = view.bars();
    assert!(!vertical.model().range().is_scrollable());
    assert!(!horizontal.model().range().is_scrollable());
}

#[test]
fn a_page_whose_own_size_differs_from_the_declared_canvas_is_refitted_to_itself() {
    // A page container's declared geometry is its *largest* page, so the
    // first render of a smaller page is asked for against the wrong figure.
    // The engine adopts the entry, refits to it, and asks again rather than
    // drawing a rectangle that describes the container instead of the page.
    let (mut view, layout, _registry) = opened(pages(2, 4_000, 3_000));
    let Some(Request::Show {
        page,
        extent,
        window,
        mut pixels,
    }) = view.next_request()
    else {
        panic!("a render was asked for");
    };
    pixels.resize((window.width * window.height * 4) as usize, 0x20);
    let mut region = damage::sink();
    let delivered = view.deliver(
        Answer::Shown {
            page,
            extent,
            window,
            decoded: Some(ViewPage {
                index: page,
                width: 400,
                height: 300,
                delay_ns: 0,
            }),
            pixels,
            outcome: Ok(()),
        },
        &layout,
        &mut region,
    );
    assert!(!delivered.changed, "those pixels describe the wrong extent");
    assert_eq!(
        view.document().expect("open").natural(),
        (400, 300),
        "the entry's own size is what the viewer now shows"
    );
    assert!(
        matches!(view.next_request(), Some(Request::Show { .. })),
        "and the render is asked for again, against the page itself"
    );
}

#[test]
fn an_answer_about_an_entry_the_user_has_left_is_recorded_but_not_drawn() {
    let (mut view, layout, _registry) = drawn(pages(3, 400, 300));
    let Some(Request::Show { .. }) = ({
        let (outcome, _) = run(&mut view, &layout, Command::NextPage);
        assert!(outcome.changed);
        view.next_request()
    }) else {
        panic!("a render was asked for");
    };
    // While that render is in flight the user turns the page again, so the
    // answer is about an entry nobody is looking at any more.
    let (outcome, _) = run(&mut view, &layout, Command::NextPage);
    assert!(outcome.changed);
    let mut region = damage::sink();
    let delivered = view.deliver(
        Answer::Shown {
            page: 1,
            extent: (400, 300),
            window: Rect::new(0, 0, 400, 300),
            decoded: Some(ViewPage {
                index: 1,
                width: 400,
                height: 300,
                delay_ns: 0,
            }),
            pixels: vec![0u8; 400 * 300 * 4],
            outcome: Ok(()),
        },
        &layout,
        &mut region,
    );
    assert!(!delivered.changed, "not the entry being shown");
    assert_eq!(view.document().expect("open").index(), 2);
    assert!(
        !view.document().expect("open").decoded(),
        "the worker holds a different entry from the one on screen"
    );
}

// ---- the window sizes itself to the picture ---------------------------

#[test]
fn a_document_opens_at_a_hundred_percent_or_zoomed_out_to_fit() {
    // Small enough for the window: shown at the size it was authored at.
    let (small, _layout, _registry) = opened(still(320, 240));
    assert_eq!(small.viewport().zoom(), ZOOM_ACTUAL_PER_MILLE);
    assert_eq!(small.viewport().fit, Fit::Actual);

    // Too big for the window: zoomed out until it fits, rather than opening
    // part-shown with the rest behind its own corner.
    let (big, layout, _registry) = opened(still(4_000, 3_000));
    assert_eq!(big.viewport().fit, Fit::Window);
    assert!(
        big.viewport().zoom() < ZOOM_ACTUAL_PER_MILLE,
        "a picture that does not fit is zoomed out, not shown at 100%"
    );
    // "Fits" means it fits: the whole picture is inside the canvas.
    let natural = big.document().expect("open").natural();
    let scaled = big.viewport().scaled(natural);
    let canvas = layout.canvas();
    assert!(
        scaled.0 <= canvas.width && scaled.1 <= canvas.height,
        "the fitted picture {scaled:?} does not sit inside the canvas {:?}",
        (canvas.width, canvas.height)
    );

    // Exactly the canvas is still a fit, so it opens at 100%.
    let exact = opened(still(1, 1)).0;
    assert_eq!(exact.viewport().fit, Fit::Actual);
}

#[test]
fn the_preferred_client_shrinks_to_a_small_picture_and_leaves_a_big_one_alone() {
    let (registry, scale) = dressing();
    let theme = registry.active();
    let face = font(theme, scale);

    // A small picture: the window hugs it, so its canvas is exactly the
    // picture's own pixels.
    let (small, ..) = opened(still(320, 240));
    let want = small
        .preferred_client_size(theme, scale, face)
        .expect("a picture that fits has a preference");
    let laid = Layout::for_window(
        want.0,
        want.1,
        theme,
        scale,
        face,
        small.toolbar_control().natural_length(scale, theme),
        small.info_open(),
    );
    assert_eq!((laid.canvas().width, laid.canvas().height), (320, 240));
    // It only ever shrinks.
    assert!(
        want.0 <= WINDOW.0 && want.1 <= WINDOW.1,
        "hugging a picture must not grow the window ({want:?} against {WINDOW:?})"
    );

    // A picture zoomed out to fit keeps the window it was given: hugging one
    // axis would only leave the fitted picture smaller.
    let (big, ..) = opened(still(4_000, 3_000));
    assert_eq!(
        big.preferred_client_size(theme, scale, face),
        None,
        "a fitted picture states no preference"
    );
    // Including one that overflows on a single axis only.
    let (tall, ..) = opened(still(320, 4_000));
    assert_eq!(tall.preferred_client_size(theme, scale, face), None);

    // Nothing open: no size to prefer.
    let empty = View::new(false);
    assert_eq!(empty.preferred_client_size(theme, scale, face), None);
}

#[test]
fn the_preferred_client_never_falls_below_the_derived_minimum() {
    let (registry, scale) = dressing();
    let theme = registry.active();
    let face = font(theme, scale);
    let (tiny, ..) = opened(still(1, 1));
    let want = tiny
        .preferred_client_size(theme, scale, face)
        .expect("a document is open");
    let floor = Layout::min_client(
        theme,
        scale,
        face,
        tiny.toolbar_control().min_length(scale, theme),
    );
    assert!(
        want.0 >= floor.0 && want.1 >= floor.1,
        "a one-pixel picture would otherwise leave no room for the chrome ({want:?} against {floor:?})"
    );
}

/// Turn the wheel by `dy` scroll units with Ctrl held and the pointer at `at`.
fn ctrl_wheel(view: &mut View, layout: &Layout, theme: &Theme, at: Point, dy: i32) -> Outcome {
    view.on_pointer(
        &InputEvent::ModifiersChanged {
            modifiers: Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        },
        layout,
        Scale::ONE,
        theme,
        &mut damage::sink(),
    );
    wheel(view, layout, theme, at, (0, dy), &mut damage::sink())
}

/// The point of the scaled picture drawn at canvas-local `at`, as a fraction
/// of the scaled picture's extent, so two zooms can be compared.
fn under(view: &View, layout: &Layout, natural: (u32, u32), at: Point) -> (f64, f64) {
    let viewport = view.viewport();
    let scaled = viewport.scaled(natural);
    let placed = viewport.placement(natural, layout.canvas());
    let visible = viewport.visible(natural, (layout.canvas().width, layout.canvas().height));
    (
        f64::from(visible.origin.x + (at.x - placed.origin.x)) / f64::from(scaled.0),
        f64::from(visible.origin.y + (at.y - placed.origin.y)) / f64::from(scaled.1),
    )
}

#[test]
fn ctrl_and_the_wheel_zoom_a_rung_a_detent_holding_the_point_under_the_pointer() {
    let natural = (4_000, 3_000);
    let (mut view, layout, registry) = overflowing(still(natural.0, natural.1));
    let theme = registry.active();
    let canvas = layout.canvas();
    let at = Point::new(
        canvas.origin.x + tairix_geometry::to_i32(canvas.width / 3),
        canvas.origin.y + tairix_geometry::to_i32(canvas.height / 4),
    );
    let before = under(&view, &layout, natural, at);
    let outcome = ctrl_wheel(&mut view, &layout, theme, at, -SCROLL_UNITS_PER_DETENT);
    assert!(outcome.changed);
    assert_eq!(
        view.viewport().zoom(),
        crate::zoom_rung_above(ZOOM_ACTUAL_PER_MILLE),
        "a detent away from the user is one rung in"
    );
    let after = under(&view, &layout, natural, at);
    let pixel = 1.0 / f64::from(view.viewport().scaled(natural).0);
    assert!(
        (after.0 - before.0).abs() <= 2.0 * pixel && (after.1 - before.1).abs() <= 2.0 * pixel,
        "the point under the pointer stayed: {before:?} then {after:?}"
    );
    // And a detent back is a rung back out, to where it began.
    ctrl_wheel(&mut view, &layout, theme, at, SCROLL_UNITS_PER_DETENT);
    assert_eq!(view.viewport().zoom(), ZOOM_ACTUAL_PER_MILLE);
}

#[test]
fn a_fine_ctrl_wheel_zooms_a_rung_per_detent_turned_and_a_reversal_starts_afresh() {
    let (mut view, layout, registry) = overflowing(still(4_000, 3_000));
    let theme = registry.active();
    let at = layout.canvas().center();
    for _ in 0..7 {
        ctrl_wheel(&mut view, &layout, theme, at, -15);
    }
    assert_eq!(
        view.viewport().zoom(),
        ZOOM_ACTUAL_PER_MILLE,
        "seven eighths of a detent is no rung yet"
    );
    // A turn back drops what the turn in left, so it too needs a detent.
    for _ in 0..7 {
        ctrl_wheel(&mut view, &layout, theme, at, 15);
    }
    assert_eq!(view.viewport().zoom(), ZOOM_ACTUAL_PER_MILLE);
    ctrl_wheel(&mut view, &layout, theme, at, 15);
    assert_eq!(
        view.viewport().zoom(),
        crate::zoom_rung_below(ZOOM_ACTUAL_PER_MILLE),
        "the eighth step of the turn back completes its detent"
    );
}

#[test]
fn without_ctrl_the_wheel_still_pans_and_a_released_ctrl_is_heard() {
    let (mut view, layout, registry) = overflowing(still(4_000, 3_000));
    let theme = registry.active();
    let at = layout.canvas().center();
    ctrl_wheel(&mut view, &layout, theme, at, -SCROLL_UNITS_PER_DETENT);
    let zoomed = view.viewport().zoom();
    view.on_pointer(
        &InputEvent::ModifiersChanged {
            modifiers: Modifiers::default(),
        },
        &layout,
        Scale::ONE,
        theme,
        &mut damage::sink(),
    );
    let pan = view.viewport().pan();
    wheel(
        &mut view,
        &layout,
        theme,
        at,
        (0, SCROLL_UNITS_PER_DETENT),
        &mut damage::sink(),
    );
    assert_eq!(view.viewport().zoom(), zoomed, "no zoom without Ctrl");
    assert_ne!(view.viewport().pan(), pan, "the turn panned instead");
}

/// One step of a pinch at `at`, its place stated first as the window client
/// states it.
fn pinch(
    view: &mut View,
    layout: &Layout,
    theme: &Theme,
    phase: tairix_input::PinchPhase,
    scale: u32,
    at: Point,
) -> Outcome {
    let mut outcome = Outcome::changed(false);
    for input in [
        InputEvent::PointerMoved { to: at },
        InputEvent::Pinch { phase, scale, at },
    ] {
        outcome = view.on_pointer(&input, layout, Scale::ONE, theme, &mut damage::sink());
    }
    outcome
}

#[test]
fn a_pinch_zooms_continuously_holding_and_then_carrying_the_point_it_began_on() {
    use tairix_abi::touch::PINCH_SCALE_ONE;
    use tairix_input::PinchPhase;

    let natural = (4_000, 3_000);
    let (mut view, layout, registry) = overflowing(still(natural.0, natural.1));
    let theme = registry.active();
    let canvas = layout.canvas();
    let at = Point::new(
        canvas.origin.x + tairix_geometry::to_i32(canvas.width / 2),
        canvas.origin.y + tairix_geometry::to_i32(canvas.height / 2),
    );
    let before = under(&view, &layout, natural, at);
    pinch(
        &mut view,
        &layout,
        theme,
        PinchPhase::Begin,
        PINCH_SCALE_ONE,
        at,
    );
    let outcome = pinch(
        &mut view,
        &layout,
        theme,
        PinchPhase::Update,
        PINCH_SCALE_ONE * 5 / 4,
        at,
    );
    assert!(outcome.changed);
    assert_eq!(
        view.viewport().zoom(),
        ZOOM_ACTUAL_PER_MILLE * 5 / 4,
        "between the ladder's rungs"
    );
    let held = under(&view, &layout, natural, at);
    let pixel = 1.0 / f64::from(view.viewport().scaled(natural).0);
    assert!(
        (held.0 - before.0).abs() <= 2.0 * pixel && (held.1 - before.1).abs() <= 2.0 * pixel,
        "{before:?} then {held:?}"
    );
    // The fingers move: the point they began on goes with them.
    let moved = Point::new(at.x - 40, at.y - 30);
    pinch(
        &mut view,
        &layout,
        theme,
        PinchPhase::Update,
        PINCH_SCALE_ONE * 5 / 4,
        moved,
    );
    let carried = under(&view, &layout, natural, moved);
    assert!(
        (carried.0 - before.0).abs() <= 2.0 * pixel && (carried.1 - before.1).abs() <= 2.0 * pixel,
        "{before:?} then {carried:?}"
    );
    pinch(
        &mut view,
        &layout,
        theme,
        PinchPhase::End,
        PINCH_SCALE_ONE * 5 / 4,
        moved,
    );
    assert_eq!(
        view.viewport().zoom(),
        ZOOM_ACTUAL_PER_MILLE * 5 / 4,
        "the zoom stands"
    );
}

#[test]
fn a_cancelled_pinch_puts_the_view_back_and_an_unbegun_one_does_nothing() {
    use tairix_abi::touch::PINCH_SCALE_ONE;
    use tairix_input::PinchPhase;

    let (mut view, layout, registry) = overflowing(still(4_000, 3_000));
    let theme = registry.active();
    let at = layout.canvas().center();
    let before = *view.viewport();
    assert!(
        !pinch(
            &mut view,
            &layout,
            theme,
            PinchPhase::Update,
            2 * PINCH_SCALE_ONE,
            at
        )
        .changed,
        "no pinch had begun"
    );
    pinch(
        &mut view,
        &layout,
        theme,
        PinchPhase::Begin,
        PINCH_SCALE_ONE,
        at,
    );
    pinch(
        &mut view,
        &layout,
        theme,
        PinchPhase::Update,
        2 * PINCH_SCALE_ONE,
        at,
    );
    assert_ne!(*view.viewport(), before);
    pinch(
        &mut view,
        &layout,
        theme,
        PinchPhase::Cancel,
        2 * PINCH_SCALE_ONE,
        at,
    );
    assert_eq!(*view.viewport(), before, "put back");
}
