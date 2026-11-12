//! Tests for the app-ward event hold-back ([`crate::holdback`]).

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::input::{KeyInput, KeyValue, Modifiers, PhysicalKey, PointerButtonCode};
use tairix_abi::window_ipc::{
    DragAt, DropOperation, OfferDrop, PointerAction, ToolOver, WindowEvent, WindowSizeState,
};
use tairix_abi::Errno;

use crate::holdback::{Delivery, Flushed, HoldBack, HOLD_BACK_CAPACITY};

const MAILBOX: u64 = 0x0A00;
const OTHER_MAILBOX: u64 = 0x0B00;
const WINDOW: u64 = 7;
const SIBLING: u64 = 9;

fn resized(window_id: u64, width_px: u32) -> WindowEvent {
    WindowEvent::Resized {
        window_id,
        width_px,
        height_px: 100,
        state: WindowSizeState::Restored,
    }
}

fn moved(window_id: u64, x: u32) -> WindowEvent {
    WindowEvent::Pointer {
        window_id,
        x,
        y: 4,
        action: PointerAction::Moved,
        modifiers: Modifiers::default(),
        at_ns: 0,
    }
}

fn pressed(window_id: u64) -> WindowEvent {
    WindowEvent::Pointer {
        window_id,
        x: 1,
        y: 1,
        action: PointerAction::Pressed(PointerButtonCode::Primary),
        modifiers: Modifiers::default(),
        at_ns: 0,
    }
}

fn released(window_id: u64) -> WindowEvent {
    WindowEvent::Pointer {
        window_id,
        x: 1,
        y: 1,
        action: PointerAction::Released(PointerButtonCode::Primary),
        modifiers: Modifiers::default(),
        at_ns: 0,
    }
}

fn cancelled(window_id: u64) -> WindowEvent {
    WindowEvent::Pointer {
        window_id,
        x: 1,
        y: 1,
        action: PointerAction::Cancelled,
        modifiers: Modifiers::default(),
        at_ns: 0,
    }
}

fn clicked_secondary(window_id: u64) -> WindowEvent {
    WindowEvent::Pointer {
        window_id,
        x: 1,
        y: 1,
        action: PointerAction::Pressed(PointerButtonCode::Secondary),
        modifiers: Modifiers::default(),
        at_ns: 0,
    }
}

fn typed(window_id: u64) -> WindowEvent {
    WindowEvent::Key {
        window_id,
        key: KeyInput::Pressed {
            key: Some(KeyValue::Char('a')),
            modifiers: Modifiers::default(),
            physical: PhysicalKey::A,
        },
    }
}

fn scrolled(window_id: u64, dy: i32) -> WindowEvent {
    WindowEvent::Scrolled {
        window_id,
        x: 4,
        y: 9,
        dx: 0,
        dy,
        modifiers: Modifiers::default(),
    }
}

fn tool_moved(window_id: u64, x: u32, ended: bool) -> WindowEvent {
    WindowEvent::ToolMoved {
        window_id,
        over: ToolOver::Parent { x, y: 3 },
        ended,
    }
}

/// Offer `event` to a destination whose mailbox is full, so it is held,
/// reporting whether it asked for a room wake.
fn hold_for(held: &mut HoldBack, endpoint: u64, event: WindowEvent) -> bool {
    match held.deliver(endpoint, &event, |_, _| Err(Errno::WouldBlock)) {
        Ok(Delivery::Owed { watch }) => watch,
        other => panic!("back-pressure is held, not {other:?}"),
    }
}

/// The same, for the destination most tests use.
fn hold(held: &mut HoldBack, event: WindowEvent) -> bool {
    hold_for(held, MAILBOX, event)
}

/// Drain everything the hold-back owes into a transcript.
fn drain(held: &mut HoldBack) -> Vec<(u64, WindowEvent)> {
    let mut out = Vec::new();
    let report = held.flush(|endpoint, event| {
        out.push((endpoint, *event));
        Ok(())
    });
    assert!(report.gone.is_empty(), "nothing died in this drain");
    out
}

/// The same transcript with the destination dropped.
fn drain_events(held: &mut HoldBack) -> Vec<WindowEvent> {
    drain(held).into_iter().map(|(_, event)| event).collect()
}

/// A bounded destination mailbox — the kernel port an app drains — so a
/// test can put the session under real back-pressure.
struct Mailbox {
    capacity: usize,
    waiting: usize,
    received: Vec<WindowEvent>,
}

impl Mailbox {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            waiting: 0,
            received: Vec::new(),
        }
    }

    fn send(&mut self, event: &WindowEvent) -> Result<(), Errno> {
        if self.waiting == self.capacity {
            return Err(Errno::WouldBlock);
        }
        self.waiting += 1;
        self.received.push(*event);
        Ok(())
    }

    /// The app catches up and empties its mailbox.
    fn drained_by_its_owner(&mut self) {
        self.waiting = 0;
    }
}

// --- The defect ------------------------------------------------------------

/// An event a full mailbox refused is *owed*, not lost.
///
/// The mailbox is a bounded resource, so a merely slow app fills it. If the
/// refusal drops the event, the app never learns its new size — it lays out
/// and hit-tests at one the compositor no longer uses — and its picker never
/// concludes, which strands the window's one pending pick for the life of the
/// window. Both must arrive the moment the app catches up, in the order they
/// happened and behind the input that preceded them.
/// A render conclusion is never shed, so a client that stopped draining while
/// it kept asking grew the hold-back without bound. The desk refuses a render
/// while one is held, and this is what it asks.
#[test]
fn a_held_render_conclusion_is_reported_until_it_goes_out() {
    let mut held = HoldBack::default();
    let concluded = WindowEvent::PreviewRendered {
        window_id: WINDOW,
        subject: tairix_abi::window_ipc::PreviewSubject::Wallpaper(0),
        width: 16,
        height: 9,
        outcome: tairix_abi::window_ipc::PreviewOutcome::Rendered,
    };
    assert!(!held.holds_render(MAILBOX, WINDOW));
    hold(&mut held, resized(WINDOW, 640));
    assert!(
        !held.holds_render(MAILBOX, WINDOW),
        "a resize is not a conclusion"
    );
    hold(&mut held, concluded);
    assert!(held.holds_render(MAILBOX, WINDOW));
    assert!(!held.holds_render(MAILBOX, SIBLING), "another window's own");
    assert!(
        !held.holds_render(OTHER_MAILBOX, WINDOW),
        "another client's own"
    );
    drain(&mut held);
    assert!(
        !held.holds_render(MAILBOX, WINDOW),
        "delivered, so no longer held"
    );
}

#[test]
fn a_resize_and_a_pick_conclusion_survive_a_full_mailbox() {
    let mut post = Mailbox::new(2);
    let mut held = HoldBack::new();

    for _ in 0..2 {
        assert_eq!(
            held.deliver(MAILBOX, &typed(WINDOW), |_, event| post.send(event)),
            Ok(Delivery::Sent),
            "room to spare"
        );
    }
    assert_eq!(post.received.len(), 2, "the mailbox is now full");

    // The app is behind, and the desktop has two things it must not lose.
    assert_eq!(
        held.deliver(MAILBOX, &resized(WINDOW, 800), |_, event| post.send(event)),
        Ok(Delivery::Owed { watch: true }),
        "the first debt asks for a room wake"
    );
    assert_eq!(
        held.deliver(
            MAILBOX,
            &WindowEvent::PickCancelled { window_id: WINDOW },
            |_, event| post.send(event)
        ),
        Ok(Delivery::Owed { watch: false }),
        "held behind the resize, on the wake already armed"
    );
    assert_eq!(post.received.len(), 2, "nothing more reached the app yet");

    // The app drains, the kernel reports room, and the session sends what it
    // owes.
    post.drained_by_its_owner();
    let report = held.flush(|_, event| post.send(event));
    assert_eq!(report.settled, vec![MAILBOX]);
    assert_eq!(
        post.received,
        vec![
            typed(WINDOW),
            typed(WINDOW),
            resized(WINDOW, 800),
            WindowEvent::PickCancelled { window_id: WINDOW },
        ]
    );
}

/// A destination already owed something takes the next event unsent, even
/// when its mailbox has room again: sending it now would put it ahead of what
/// is queued, and the app must see its events in the order they happened.
#[test]
fn a_later_event_never_overtakes_one_already_owed() {
    let mut post = Mailbox::new(1);
    let mut held = HoldBack::new();

    let _ = held
        .deliver(MAILBOX, &typed(WINDOW), |_, event| post.send(event))
        .expect("room to spare");
    let _ = held
        .deliver(MAILBOX, &resized(WINDOW, 800), |_, event| post.send(event))
        .expect("refused and held");
    post.drained_by_its_owner();
    assert_eq!(
        held.deliver(MAILBOX, &pressed(WINDOW), |_, event| post.send(event)),
        Ok(Delivery::Owed { watch: false }),
        "the destination has room again, but it owes something first"
    );
    assert_eq!(post.received.len(), 1, "the press did not jump the queue");

    // Each time the app catches up, the session sends more of what it owes.
    while held.owes(MAILBOX) {
        post.drained_by_its_owner();
        let _ = held.flush(|_, event| post.send(event));
    }
    assert_eq!(
        post.received,
        vec![typed(WINDOW), resized(WINDOW, 800), pressed(WINDOW)]
    );
}

/// An event that is owed says so, so its caller can keep the evidence its
/// responsiveness verdict rests on.
///
/// The regression this guards: an event the hold-back takes issues no send,
/// so the desktop's "not responding" detector sees no refusal of its own.
/// If the hold-back reported only "arm a wake", the very case the detector
/// exists for — an app that stops draining, so no room wake ever comes —
/// would stop producing evidence after the *first* refusal and the app
/// would never be flagged. `Owed` is that evidence; only `Sent` clears it.
#[test]
fn an_owed_event_is_distinguishable_from_a_sent_one() {
    let mut post = Mailbox::new(1);
    let mut held = HoldBack::new();

    assert_eq!(
        held.deliver(MAILBOX, &typed(WINDOW), |_, event| post.send(event)),
        Ok(Delivery::Sent)
    );
    for expected in [
        Delivery::Owed { watch: true },
        Delivery::Owed { watch: false },
        Delivery::Owed { watch: false },
    ] {
        assert_eq!(
            held.deliver(MAILBOX, &typed(WINDOW), |_, event| post.send(event)),
            Ok(expected),
            "every event the app cannot take is still owed"
        );
    }
}

/// A refusal that is not back-pressure is the destination's, not the
/// session's: it is surfaced rather than held, so the caller can tear the
/// owner down instead of accumulating events for a corpse.
#[test]
fn a_refusal_that_is_not_back_pressure_is_surfaced() {
    let mut held = HoldBack::new();
    assert_eq!(
        held.deliver(MAILBOX, &resized(WINDOW, 800), |_, _| Err(Errno::NotFound)),
        Err(Errno::NotFound)
    );
    assert!(!held.owes(MAILBOX), "nothing is owed to a dead port");
}

/// The session's own spent budget is back-pressure like a full mailbox: the
/// event is held and its destination's room wake armed — the kernel raises it
/// as the budget frees, whichever app takes one of the session's messages —
/// and the flush that wake runs delivers it, no send elsewhere needed.
/// Nothing of it counts against the app.
///
/// Regression: an event held for the budget armed nothing, and went only with
/// the next send to some app that happened to go through.
#[test]
fn an_event_the_sessions_budget_refused_waits_on_the_room_wake_and_its_flush_sends_it() {
    let mut held = HoldBack::new();
    assert_eq!(
        held.deliver(MAILBOX, &resized(WINDOW, 800), |_, _| Err(
            Errno::LimitExceeded
        )),
        Ok(Delivery::OverBudget { watch: true }),
        "watched for room, as a full mailbox is"
    );
    assert_eq!(
        held.deliver(MAILBOX, &typed(WINDOW), |_, _| Ok(())),
        Ok(Delivery::OverBudget { watch: false }),
        "held behind it, on the wake already armed"
    );
    let mut received = Vec::new();
    let report = held.flush(|_, event| {
        received.push(*event);
        Ok(())
    });
    assert_eq!(report.settled, vec![MAILBOX], "its room wake goes");
    assert_eq!(received, vec![resized(WINDOW, 800), typed(WINDOW)]);
    assert!(!held.owes(MAILBOX));
}

/// A flush the session's spent budget stops keeps the rest owed on the same
/// room wake, which the kernel raises for the budget too.
#[test]
fn a_flush_the_sessions_budget_stops_keeps_the_rest_owed_on_the_room_wake() {
    let mut held = HoldBack::new();
    let _ = hold(&mut held, typed(WINDOW));
    let report = held.flush(|_, _| Err(Errno::LimitExceeded));
    assert_eq!(report, Flushed::default(), "the room wake stays armed");
    assert_eq!(
        held.deliver(MAILBOX, &pressed(WINDOW), |_, _| Ok(())),
        Ok(Delivery::OverBudget { watch: false }),
        "what the debt waits on now is the budget"
    );
    assert_eq!(held.depth(MAILBOX, Some(WINDOW)), 2, "both are owed");
}

/// A send the kernel had no memory for holds the event with no room wake,
/// which would fire at once, and nothing against the app; the flush the
/// caller times delivers it.
#[test]
fn an_event_refused_for_kernel_memory_is_held_for_the_timed_flush() {
    let mut held = HoldBack::new();
    assert_eq!(
        held.deliver(MAILBOX, &resized(WINDOW, 800), |_, _| Err(
            Errno::OutOfMemory
        )),
        Ok(Delivery::Starved)
    );
    assert!(held.starved(), "a timed flush is owed");
    assert_eq!(
        held.deliver(MAILBOX, &typed(WINDOW), |_, _| Ok(())),
        Ok(Delivery::Starved),
        "held behind it, still waiting on memory"
    );
    let mut received = Vec::new();
    let report = held.flush(|_, event| {
        received.push(*event);
        Ok(())
    });
    assert_eq!(report, Flushed::default(), "it had no room wake to drop");
    assert_eq!(received, vec![resized(WINDOW, 800), typed(WINDOW)]);
    assert!(!held.starved());
}

/// A debt's wait moves between its room wake and the timed flush as its sends
/// are refused, and each flush says which room wakes to drop or arm again.
#[test]
fn a_debt_moves_between_its_room_wake_and_the_timed_flush() {
    let mut held = HoldBack::new();
    assert!(hold(&mut held, typed(WINDOW)), "watched for room");
    let report = held.flush(|_, _| Err(Errno::OutOfMemory));
    assert_eq!(
        report.settled,
        vec![MAILBOX],
        "its room wake would fire at once"
    );
    assert!(held.starved());
    let report = held.flush(|_, _| Err(Errno::WouldBlock));
    assert_eq!(
        report.watch,
        vec![MAILBOX],
        "behind a full mailbox, it is watched again"
    );
    assert!(!held.starved());
    assert_eq!(held.depth(MAILBOX, Some(WINDOW)), 1, "still owed");
}

// --- Folding ---------------------------------------------------------------

/// A state edge names what *is*: a second one replaces the first where it
/// stands, so the app converges on the truth and the queue does not grow.
/// The occurrences queued around it keep their order.
#[test]
fn a_later_state_edge_replaces_the_one_it_supersedes() {
    let mut held = HoldBack::new();
    assert!(hold(&mut held, resized(WINDOW, 100)));
    assert!(!hold(&mut held, typed(WINDOW)));
    assert!(!hold(&mut held, resized(WINDOW, 200)));

    assert_eq!(
        held.depth(MAILBOX, Some(WINDOW)),
        2,
        "one resize is owed, not two"
    );
    assert_eq!(
        drain_events(&mut held),
        vec![resized(WINDOW, 200), typed(WINDOW)]
    );
}

/// Each state edge holds its own slot: they supersede their own kind and
/// nothing else.
#[test]
fn state_edges_of_different_kinds_are_each_owed() {
    let mut held = HoldBack::new();
    let edges = [
        WindowEvent::Focus {
            window_id: WINDOW,
            focused: true,
        },
        resized(WINDOW, 100),
        WindowEvent::RedrawRequested { window_id: WINDOW },
        WindowEvent::CloseRequested { window_id: WINDOW },
        WindowEvent::Minimized { window_id: WINDOW },
        WindowEvent::Shown {
            window_id: WINDOW,
            shown: false,
        },
    ];
    for edge in edges {
        let _ = hold(&mut held, edge);
    }
    for edge in edges {
        let _ = hold(&mut held, edge);
    }
    assert_eq!(held.depth(MAILBOX, Some(WINDOW)), edges.len());
}

/// A desktop covered and uncovered while a window's mailbox was full is owed
/// as where it ended: shown.
#[test]
fn a_held_shown_edge_is_owed_as_it_last_stood() {
    let mut held = HoldBack::new();
    for shown in [false, true] {
        let _ = hold(
            &mut held,
            WindowEvent::Shown {
                window_id: WINDOW,
                shown,
            },
        );
    }
    assert_eq!(
        drain_events(&mut held),
        vec![WindowEvent::Shown {
            window_id: WINDOW,
            shown: true,
        }]
    );
}

/// A position is level-triggered and a wheel tick is additive, so a run of
/// either collapses — but only while it is unbroken, and a wheel reversal is
/// a distinct gesture (an intermediate clamp at a range end is a real
/// difference the app must see).
#[test]
fn a_run_of_samples_collapses_and_a_run_of_ticks_sums() {
    let mut held = HoldBack::new();
    let _ = hold(&mut held, moved(WINDOW, 1));
    let _ = hold(&mut held, moved(WINDOW, 2));
    let _ = hold(&mut held, moved(WINDOW, 3));
    let _ = hold(&mut held, scrolled(WINDOW, 1));
    let _ = hold(&mut held, scrolled(WINDOW, 2));
    let _ = hold(&mut held, scrolled(WINDOW, -1));
    let _ = hold(&mut held, moved(WINDOW, 4));

    assert_eq!(
        drain_events(&mut held),
        vec![
            moved(WINDOW, 3),
            scrolled(WINDOW, 3),
            scrolled(WINDOW, -1),
            moved(WINDOW, 4),
        ]
    );
}

/// A run of wheel turns sums at the newest place, but a modifier let go
/// mid-run ends it: a Ctrl-wheel zoom and the plain scroll after it are two
/// gestures, and summing them would zoom by turns made without Ctrl.
#[test]
fn a_run_of_wheel_turns_ends_where_the_modifiers_change() {
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    let turned = |dy: i32, y: u32, modifiers: Modifiers| WindowEvent::Scrolled {
        window_id: WINDOW,
        x: 4,
        y,
        dx: 0,
        dy,
        modifiers,
    };
    let mut held = HoldBack::new();
    let _ = hold(&mut held, turned(120, 1, ctrl));
    let _ = hold(&mut held, turned(120, 2, ctrl));
    let _ = hold(&mut held, turned(120, 3, Modifiers::default()));
    assert_eq!(
        drain_events(&mut held),
        vec![turned(240, 2, ctrl), turned(120, 3, Modifiers::default())]
    );
}

/// A pinch's updates fold to the newest, its scale being relative to where it
/// began; its beginning and end are owed, and a modifier change ends a run.
#[test]
fn a_run_of_pinch_updates_folds_to_the_newest_between_its_edges() {
    use tairix_abi::touch::{PinchPhase, PINCH_SCALE_ONE};
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    let step = |phase, scale, modifiers| WindowEvent::Pinch {
        window_id: WINDOW,
        x: scale % 100,
        y: 4,
        phase,
        scale,
        modifiers,
    };
    let none = Modifiers::default();
    let mut held = HoldBack::new();
    for event in [
        step(PinchPhase::Begin, PINCH_SCALE_ONE, none),
        step(PinchPhase::Update, PINCH_SCALE_ONE + 1, none),
        step(PinchPhase::Update, PINCH_SCALE_ONE + 2, none),
        step(PinchPhase::Update, PINCH_SCALE_ONE + 3, alt),
        step(PinchPhase::End, PINCH_SCALE_ONE + 3, alt),
    ] {
        let _ = hold(&mut held, event);
    }
    assert_eq!(
        drain_events(&mut held),
        vec![
            step(PinchPhase::Begin, PINCH_SCALE_ONE, none),
            step(PinchPhase::Update, PINCH_SCALE_ONE + 2, none),
            step(PinchPhase::Update, PINCH_SCALE_ONE + 3, alt),
            step(PinchPhase::End, PINCH_SCALE_ONE + 3, alt),
        ]
    );
}

/// A folded sample carries the newest modifiers as well as the newest
/// position: reporting the older held set beside the newer position would name
/// a state the pointer was never in.
#[test]
fn a_collapsed_run_of_samples_carries_the_newest_modifiers() {
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let with_shift = |x: u32| WindowEvent::Pointer {
        window_id: WINDOW,
        x,
        y: 4,
        action: PointerAction::Moved,
        modifiers: shift,
        at_ns: 0,
    };
    let mut held = HoldBack::new();
    let _ = hold(&mut held, moved(WINDOW, 1));
    let _ = hold(&mut held, with_shift(2));
    assert_eq!(drain_events(&mut held), vec![with_shift(2)]);

    // And the other way: shift released mid-run is folded too.
    let mut held = HoldBack::new();
    let _ = hold(&mut held, with_shift(1));
    let _ = hold(&mut held, moved(WINDOW, 2));
    assert_eq!(drain_events(&mut held), vec![moved(WINDOW, 2)]);
}

/// A folded sample is timed by the newest: a stroke an app paints by time
/// would otherwise read the newer position as reached at the older instant.
#[test]
fn a_collapsed_run_of_samples_carries_the_newest_time() {
    let at = |x: u32, at_ns: u64| WindowEvent::Pointer {
        window_id: WINDOW,
        x,
        y: 4,
        action: PointerAction::Moved,
        modifiers: Modifiers::default(),
        at_ns,
    };
    let mut held = HoldBack::new();
    let _ = hold(&mut held, at(1, 10));
    let _ = hold(&mut held, at(2, 20));
    assert_eq!(drain_events(&mut held), vec![at(2, 20)]);
}

/// A press between two samples breaks the run: an occurrence the app must
/// witness is never reordered around by a fold.
#[test]
fn an_occurrence_between_two_samples_breaks_the_run() {
    let mut held = HoldBack::new();
    let _ = hold(&mut held, moved(WINDOW, 1));
    let _ = hold(&mut held, pressed(WINDOW));
    let _ = hold(&mut held, moved(WINDOW, 2));

    assert_eq!(
        drain_events(&mut held),
        vec![moved(WINDOW, 1), pressed(WINDOW), moved(WINDOW, 2)]
    );
}

// --- The bound -------------------------------------------------------------

/// The bound holds however long an owner stays away, and what it sheds is
/// always an input occurrence: the state edge and the pick conclusion the
/// app cannot re-derive survive the whole storm.
#[test]
fn overflow_sheds_input_and_never_a_state_edge_or_a_conclusion() {
    let mut held = HoldBack::new();
    let _ = hold(&mut held, resized(WINDOW, 100));
    let _ = hold(&mut held, WindowEvent::PickCancelled { window_id: WINDOW });
    for _ in 0..HOLD_BACK_CAPACITY * 4 {
        let _ = hold(&mut held, typed(WINDOW));
    }

    assert_eq!(held.depth(MAILBOX, Some(WINDOW)), HOLD_BACK_CAPACITY);
    let drained = drain_events(&mut held);
    assert_eq!(
        drained.first(),
        Some(&resized(WINDOW, 100)),
        "the resize outlives every keystroke"
    );
    assert!(
        drained.contains(&WindowEvent::PickCancelled { window_id: WINDOW }),
        "the picker's one conclusion is never shed"
    );
}

/// A held press goes with its own release: the app learns of neither, rather
/// than holding a press it is never told is over — the latch that would leave
/// a widget dragging, or a scroll arrow repeating, for ever. A cancel that
/// follows it instead stays: it ends every press the window holds, one it was
/// told of before among them, and ends nothing where there is none.
#[test]
fn a_press_is_shed_with_its_release_and_never_with_a_cancel() {
    let mut held = HoldBack::new();
    let _ = hold(&mut held, pressed(WINDOW));
    let _ = hold(&mut held, released(WINDOW));
    // One past the bound, so exactly one shed is needed.
    for _ in 0..HOLD_BACK_CAPACITY - 1 {
        let _ = hold(&mut held, typed(WINDOW));
    }
    let drained = drain_events(&mut held);
    assert_eq!(drained.len(), HOLD_BACK_CAPACITY - 1);
    assert!(!drained.contains(&pressed(WINDOW)), "the press went first");
    assert!(
        !drained.contains(&released(WINDOW)),
        "and its release with it"
    );

    let mut held = HoldBack::new();
    let _ = hold(&mut held, pressed(WINDOW));
    let _ = hold(&mut held, cancelled(WINDOW));
    for _ in 0..HOLD_BACK_CAPACITY - 1 {
        let _ = hold(&mut held, typed(WINDOW));
    }
    let drained = drain_events(&mut held);
    assert_eq!(drained.len(), HOLD_BACK_CAPACITY);
    assert!(!drained.contains(&pressed(WINDOW)), "the press went alone");
    assert_eq!(drained.first(), Some(&cancelled(WINDOW)));
}

/// The end of a press the app was already given is all that ends it, so it
/// outlives any storm.
#[test]
fn the_end_of_a_press_already_delivered_is_never_shed() {
    for end in [released(WINDOW), cancelled(WINDOW)] {
        let mut held = HoldBack::new();
        let _ = hold(&mut held, end);
        for _ in 0..HOLD_BACK_CAPACITY * 2 {
            let _ = hold(&mut held, typed(WINDOW));
        }
        assert_eq!(held.depth(MAILBOX, Some(WINDOW)), HOLD_BACK_CAPACITY);
        assert_eq!(drain_events(&mut held).first(), Some(&end));
    }
}

/// A press shed before a cancel goes alone: the cancel ends every press the
/// window holds, the one it was told of before among them, so it stays owed.
#[test]
fn a_click_is_shed_alone_and_never_takes_a_held_press_s_end() {
    let mut held = HoldBack::new();
    let _ = hold(&mut held, clicked_secondary(WINDOW));
    let _ = hold(&mut held, cancelled(WINDOW));
    for _ in 0..HOLD_BACK_CAPACITY - 1 {
        let _ = hold(&mut held, typed(WINDOW));
    }

    let drained = drain_events(&mut held);
    assert_eq!(drained.len(), HOLD_BACK_CAPACITY);
    assert!(!drained.contains(&clicked_secondary(WINDOW)));
    assert_eq!(drained.first(), Some(&cancelled(WINDOW)));
}

/// One window's overflow is its own: a sibling window's queue is a separate
/// debt and keeps everything it was owed.
#[test]
fn the_bound_is_per_window_not_per_owner() {
    let mut held = HoldBack::new();
    let _ = hold(&mut held, resized(SIBLING, 640));
    for _ in 0..HOLD_BACK_CAPACITY * 2 {
        let _ = hold(&mut held, typed(WINDOW));
    }

    assert_eq!(held.depth(MAILBOX, Some(WINDOW)), HOLD_BACK_CAPACITY);
    assert_eq!(held.depth(MAILBOX, Some(SIBLING)), 1);
}

// --- Flushing --------------------------------------------------------------

/// The first debt to a destination is what arms its wake; everything after
/// it rides the same one, and the wake is dropped only once the destination
/// owes nothing.
#[test]
fn only_the_first_debt_asks_for_a_wake() {
    let mut held = HoldBack::new();
    assert!(hold(&mut held, resized(WINDOW, 100)));
    assert!(!hold(&mut held, typed(WINDOW)));
    assert!(!hold(&mut held, resized(SIBLING, 200)));
    assert!(
        hold_for(&mut held, OTHER_MAILBOX, typed(WINDOW)),
        "a new destination"
    );
    assert!(held.owes(MAILBOX));

    let report = held.flush(|_, _| Ok(()));
    assert_eq!(report.settled, vec![MAILBOX, OTHER_MAILBOX]);
    assert!(report.gone.is_empty());
    assert!(!held.owes(MAILBOX));
}

/// A mailbox that fills again keeps the rest owed — nothing is dropped and
/// the wake stands, so the next drain resumes exactly where this one
/// stopped.
#[test]
fn a_mailbox_that_fills_again_keeps_the_rest_owed() {
    let mut held = HoldBack::new();
    for _ in 0..5 {
        let _ = hold(&mut held, typed(WINDOW));
    }

    let mut room = 2;
    let report = held.flush(|_, _| {
        if room == 0 {
            return Err(Errno::WouldBlock);
        }
        room -= 1;
        Ok(())
    });
    assert!(report.settled.is_empty(), "the destination is not settled");
    assert!(report.gone.is_empty());
    assert_eq!(held.depth(MAILBOX, Some(WINDOW)), 3);

    assert_eq!(drain_events(&mut held).len(), 3);
    assert!(!held.owes(MAILBOX));
}

/// Windows share their owner's one mailbox, so a window with a long backlog
/// must not spend all the room: each pass serves one event per window.
#[test]
fn windows_are_served_round_robin_so_a_backlog_starves_no_sibling() {
    let mut held = HoldBack::new();
    for _ in 0..10 {
        let _ = hold(&mut held, typed(WINDOW));
    }
    let _ = hold(&mut held, resized(SIBLING, 640));

    let mut room = 2;
    let mut served: Vec<Option<u64>> = Vec::new();
    let _ = held.flush(|_, event| {
        if room == 0 {
            return Err(Errno::WouldBlock);
        }
        room -= 1;
        served.push(event.window_id());
        Ok(())
    });
    assert_eq!(
        served,
        vec![Some(WINDOW), Some(SIBLING)],
        "the sibling's resize got the second slot, not the backlog"
    );
}

/// An owner the send proves gone takes its debts with it, and is reported so
/// the caller can drop its wake and tear its windows down.
#[test]
fn a_destination_that_is_gone_is_discarded_and_reported() {
    let mut held = HoldBack::new();
    let _ = hold(&mut held, typed(WINDOW));
    let _ = hold(&mut held, resized(SIBLING, 640));
    let _ = hold_for(&mut held, OTHER_MAILBOX, typed(WINDOW));

    let report = held.flush(|endpoint, _| {
        if endpoint == MAILBOX {
            Err(Errno::NotFound)
        } else {
            Ok(())
        }
    });
    assert_eq!(report.gone, vec![MAILBOX]);
    assert_eq!(report.settled, vec![OTHER_MAILBOX]);
    assert!(!held.owes(MAILBOX));
}

/// A refusal that waiting cannot fix is not back-pressure: that one event is
/// dropped and the rest go on, so a flush can never re-offer the same event
/// for ever.
#[test]
fn an_unrecoverable_refusal_drops_only_that_event() {
    let mut held = HoldBack::new();
    let _ = hold(&mut held, resized(WINDOW, 100));
    let _ = hold(&mut held, typed(WINDOW));

    let mut offered = 0_u32;
    let report = held.flush(|_, _| {
        offered += 1;
        if offered == 1 {
            Err(Errno::MessageTooLarge)
        } else {
            Ok(())
        }
    });
    assert_eq!(offered, 2, "the second event was still offered");
    assert_eq!(report.dropped, 1, "and the one given up on is counted");
    assert_eq!(report.settled, vec![MAILBOX]);
    assert!(!held.owes(MAILBOX));
}

/// A reaped owner's debts go with it, and the caller is told a wake it armed
/// is now stale.
#[test]
fn forgetting_an_owner_discards_everything_it_was_owed() {
    let mut held = HoldBack::new();
    let _ = hold(&mut held, typed(WINDOW));
    let _ = hold(&mut held, resized(SIBLING, 640));
    let _ = hold_for(&mut held, OTHER_MAILBOX, typed(WINDOW));

    assert!(held.forget(MAILBOX), "a wake was armed for it");
    assert!(!held.owes(MAILBOX));
    assert!(held.owes(OTHER_MAILBOX), "its neighbour is untouched");
    assert!(!held.forget(MAILBOX), "forgetting twice arms nothing");
}

#[test]
fn a_tool_windows_move_folds_to_its_newest_sample_and_its_end_withdraws_the_rest() {
    let mut held = HoldBack::new();
    hold(&mut held, tool_moved(WINDOW, 1, false));
    hold(&mut held, tool_moved(WINDOW, 2, false));
    assert_eq!(
        held.depth(MAILBOX, Some(WINDOW)),
        1,
        "a run of samples is one"
    );
    hold(&mut held, typed(WINDOW));
    hold(&mut held, tool_moved(WINDOW, 3, false));
    hold(&mut held, tool_moved(WINDOW, 4, true));
    // The first move's end is where it was let go; the next move's samples
    // and end supersede it, and the key between them is still owed in order.
    hold(&mut held, tool_moved(WINDOW, 5, false));
    hold(&mut held, tool_moved(WINDOW, 6, true));
    assert_eq!(
        drain_events(&mut held),
        vec![typed(WINDOW), tool_moved(WINDOW, 6, true)]
    );
}

#[test]
fn a_tool_windows_move_end_is_never_shed() {
    let mut held = HoldBack::new();
    hold(&mut held, tool_moved(WINDOW, 1, true));
    for _ in 0..HOLD_BACK_CAPACITY * 2 {
        hold(&mut held, typed(WINDOW));
    }
    assert_eq!(held.depth(MAILBOX, Some(WINDOW)), HOLD_BACK_CAPACITY);
    assert_eq!(drain_events(&mut held)[0], tool_moved(WINDOW, 1, true));
}

fn drag_over(window_id: u64, serial: u32) -> WindowEvent {
    WindowEvent::DragOver {
        window_id,
        serial,
        at: DragAt::Window { x: serial, y: 1 },
        shift: false,
    }
}

fn offered(window_id: u64, serial: u32) -> WindowEvent {
    WindowEvent::SaveOffered {
        window_id,
        serial,
        x: serial,
        y: 1,
    }
}

/// A drag's reports and a save's offers are numbered, and only the newest is
/// worth an answer, so a run of each folds to it: a long drag over a slow
/// window holds one report, not one per batch of motion.
#[test]
fn a_run_of_drag_reports_or_save_offers_folds_to_the_newest() {
    let mut held = HoldBack::new();
    let last = u32::try_from(HOLD_BACK_CAPACITY * 2).expect("the hold-back capacity is small");
    for serial in 1..=last {
        hold(&mut held, drag_over(WINDOW, serial));
    }
    assert_eq!(drain_events(&mut held), [drag_over(WINDOW, last)]);
    for serial in 1..=3 {
        hold(&mut held, offered(WINDOW, serial));
    }
    hold(&mut held, typed(WINDOW));
    hold(&mut held, offered(WINDOW, 4));
    assert_eq!(
        drain_events(&mut held),
        [offered(WINDOW, 3), typed(WINDOW), offered(WINDOW, 4)],
        "a key between them is an occurrence the run does not fold across"
    );
}

/// A file offer and an offer of desktop items are numbered like a drag's
/// reports, so a run of each folds to the newest too.
#[test]
fn a_run_of_file_or_items_offers_folds_to_the_newest() {
    let file = |serial| WindowEvent::FileOffered {
        window_id: WINDOW,
        drag: 1,
        serial,
        x: serial,
        y: 1,
    };
    let items = |serial| WindowEvent::ItemsOffered {
        window_id: WINDOW,
        drag: 2,
        serial,
        x: serial,
        y: 1,
        shift: false,
    };
    let mut held = HoldBack::new();
    for serial in 1..=4 {
        hold(&mut held, file(serial));
    }
    assert_eq!(drain_events(&mut held), [file(4)]);
    for serial in 5..=7 {
        hold(&mut held, items(serial));
    }
    hold(&mut held, typed(WINDOW));
    hold(&mut held, items(8));
    assert_eq!(drain_events(&mut held), [items(7), typed(WINDOW), items(8)]);
}

fn items_offered(serial: u32) -> WindowEvent {
    WindowEvent::ItemsOffered {
        window_id: WINDOW,
        drag: 3,
        serial,
        x: serial,
        y: 1,
        shift: false,
    }
}

fn file_offered(serial: u32) -> WindowEvent {
    WindowEvent::FileOffered {
        window_id: WINDOW,
        drag: 3,
        serial,
        x: serial,
        y: 1,
    }
}

fn items_ended(serial: u32, dropped: bool) -> WindowEvent {
    WindowEvent::ItemsOfferEnded {
        window_id: WINDOW,
        serial,
        dropped: dropped.then_some(OfferDrop {
            x: serial,
            y: 1,
            operation: DropOperation::Copy,
        }),
    }
}

/// A report offers a window the items and then the file among them, so the
/// pairs a slow window is owed fold to the newest pair, not one per report.
#[test]
fn offers_made_in_pairs_fold_to_the_newest_pair() {
    let mut held = HoldBack::new();
    let last = u32::try_from(HOLD_BACK_CAPACITY * 2).expect("the hold-back capacity is small");
    for serial in 1..=last {
        hold(&mut held, items_offered(serial));
        hold(&mut held, file_offered(serial));
    }
    assert_eq!(
        drain_events(&mut held),
        [items_offered(last), file_offered(last)]
    );
    hold(&mut held, items_offered(1));
    hold(&mut held, file_offered(1));
    hold(&mut held, typed(WINDOW));
    hold(&mut held, items_offered(2));
    hold(&mut held, file_offered(2));
    assert_eq!(
        drain_events(&mut held),
        [
            items_offered(1),
            file_offered(1),
            typed(WINDOW),
            items_offered(2),
            file_offered(2)
        ],
        "a key between them is an occurrence the run does not fold across"
    );
}

/// Items carried back and forth over a window that has stopped reading hold
/// it no more than the last crossing's end: each end withdraws what it ends.
#[test]
fn an_items_offer_that_only_left_withdraws_what_it_ends() {
    let mut held = HoldBack::new();
    let crossings = u32::try_from(HOLD_BACK_CAPACITY * 2).expect("the hold-back capacity is small");
    for crossing in 0..crossings {
        let serial = crossing * 2 + 1;
        hold(&mut held, items_offered(serial));
        hold(&mut held, file_offered(serial));
        hold(&mut held, typed(WINDOW));
        hold(&mut held, items_offered(serial + 1));
        hold(&mut held, items_ended(serial + 1, false));
        assert!(held.depth(MAILBOX, Some(WINDOW)) <= HOLD_BACK_CAPACITY);
    }
    let owed = drain_events(&mut held);
    assert_eq!(owed.last(), Some(&items_ended(crossings * 2, false)));
    assert!(
        owed.iter()
            .all(|event| *event == typed(WINDOW) || *event == items_ended(crossings * 2, false)),
        "only the keys typed and the last end are left: {owed:?}"
    );
}

/// A drop is of the newest offer before it, which a window may answer as late
/// as it reads it, so that offer stays with the drop and an end that only left
/// takes neither.
#[test]
fn an_items_drop_keeps_the_offer_it_drops() {
    let mut held = HoldBack::new();
    hold(&mut held, items_offered(1));
    hold(&mut held, file_offered(1));
    hold(&mut held, items_ended(1, true));
    hold(&mut held, items_offered(3));
    hold(&mut held, file_offered(3));
    hold(&mut held, items_ended(3, false));
    assert_eq!(
        drain_events(&mut held),
        [
            items_offered(1),
            file_offered(1),
            items_ended(1, true),
            items_ended(3, false)
        ]
    );
    hold(&mut held, items_ended(4, false));
    hold(&mut held, items_offered(5));
    hold(&mut held, items_ended(5, true));
    hold(&mut held, items_ended(7, true));
    assert_eq!(
        drain_events(&mut held),
        [items_ended(7, true)],
        "a drop takes the ends held before it, a second drop the first and its offer"
    );
}

/// A drop takes every offer held before it but the one it is of, numbered as
/// it is: only the newest is worth an answer.
#[test]
fn an_items_drop_takes_every_offer_but_the_one_it_is_of() {
    let mut held = HoldBack::new();
    hold(&mut held, items_offered(1));
    hold(&mut held, file_offered(1));
    hold(&mut held, typed(WINDOW));
    hold(&mut held, items_offered(2));
    hold(&mut held, file_offered(2));
    hold(&mut held, items_ended(2, true));
    assert_eq!(
        drain_events(&mut held),
        [
            typed(WINDOW),
            items_offered(2),
            file_offered(2),
            items_ended(2, true)
        ]
    );
}

/// A window reads a drop by the offer it is of, so a window that stays behind
/// never loses that offer to overflow: the input held after the drop goes
/// instead.
#[test]
fn overflow_never_sheds_the_offer_a_held_drop_is_of() {
    let mut held = HoldBack::new();
    hold(&mut held, items_offered(1));
    hold(&mut held, file_offered(1));
    hold(&mut held, items_ended(1, true));
    for _ in 0..HOLD_BACK_CAPACITY * 2 {
        hold(&mut held, typed(WINDOW));
    }
    assert_eq!(held.depth(MAILBOX, Some(WINDOW)), HOLD_BACK_CAPACITY);
    assert_eq!(
        drain_events(&mut held)[..3],
        [items_offered(1), file_offered(1), items_ended(1, true)]
    );
}

/// The end of a save offer carries where it was dropped, so it withdraws the
/// offers still held and is itself always owed.
#[test]
fn a_save_offers_end_withdraws_the_offers_held_before_it() {
    let mut held = HoldBack::new();
    hold(&mut held, offered(WINDOW, 1));
    hold(&mut held, typed(WINDOW));
    hold(&mut held, offered(WINDOW, 2));
    let ended = WindowEvent::SaveOfferEnded {
        window_id: WINDOW,
        serial: 2,
        dropped: Some((2, 1)),
    };
    hold(&mut held, ended);
    assert_eq!(drain_events(&mut held), [typed(WINDOW), ended]);

    let left = |serial| WindowEvent::SaveOfferEnded {
        window_id: WINDOW,
        serial,
        dropped: None,
    };
    hold(&mut held, left(3));
    hold(&mut held, ended);
    hold(&mut held, left(5));
    assert_eq!(
        drain_events(&mut held),
        [ended, left(5)],
        "an end that only left never takes a drop another pick still waits on"
    );
    let dropped = |serial| WindowEvent::SaveOfferEnded {
        window_id: WINDOW,
        serial,
        dropped: Some((2, 1)),
    };
    hold(&mut held, dropped(6));
    hold(&mut held, dropped(8));
    assert_eq!(
        drain_events(&mut held),
        [dropped(8)],
        "a second drop is held only for a window answering an offer it was never shown"
    );
}

fn key_edge(window_id: u64, physical: PhysicalKey, pressed: bool) -> WindowEvent {
    let (key, modifiers) = (None, Modifiers::default());
    WindowEvent::Key {
        window_id,
        key: if pressed {
            KeyInput::Pressed {
                key,
                modifiers,
                physical,
            }
        } else {
            KeyInput::Released {
                key,
                modifiers,
                physical,
            }
        },
    }
}

fn locked_motion(window_id: u64, dx: i32, dy: i32, at_ns: u64) -> WindowEvent {
    WindowEvent::PointerMotion {
        window_id,
        dx,
        dy,
        modifiers: Modifiers::default(),
        at_ns,
    }
}

fn lock(window_id: u64, locked: bool) -> WindowEvent {
    WindowEvent::PointerLock { window_id, locked }
}

/// A key's release is never shed, and a key's press goes with its own release
/// and no other: however much is typed into a window that does not drain, no
/// key it saw go down is left down.
#[test]
fn a_key_release_is_never_shed_and_a_press_goes_with_its_own() {
    let mut held = HoldBack::new();
    let shift = PhysicalKey::LEFT_SHIFT;
    let delivered = key_edge(WINDOW, PhysicalKey::SPACE, false);
    let _ = hold(&mut held, key_edge(WINDOW, shift, true));
    let _ = hold(&mut held, delivered);
    let _ = hold(&mut held, key_edge(WINDOW, shift, false));
    for _ in 0..HOLD_BACK_CAPACITY {
        let _ = hold(&mut held, typed(WINDOW));
    }
    let drained = drain_events(&mut held);
    assert_eq!(
        drained.first(),
        Some(&delivered),
        "a delivered press's release stays"
    );
    assert!(
        !drained.contains(&key_edge(WINDOW, shift, true)),
        "Shift went"
    );
    assert!(
        !drained.contains(&key_edge(WINDOW, shift, false)),
        "with its own release"
    );
    assert!(drained.len() <= HOLD_BACK_CAPACITY);
}

/// The releases of presses already delivered are what end them, so they
/// outlast the bound however many there are: a key is down at most once, so
/// they are at most one per key.
#[test]
fn releases_of_delivered_presses_outlast_the_bound() {
    let mut held = HoldBack::new();
    let count = HOLD_BACK_CAPACITY + 4;
    for at in 0..count {
        let usage = 0x04 + u16::try_from(at).expect("small");
        let key = PhysicalKey::from_usage(usage).expect("a key");
        let _ = hold(&mut held, key_edge(WINDOW, key, false));
    }
    assert_eq!(held.depth(MAILBOX, Some(WINDOW)), count);
}

/// A locked pointer's motion sums at the tail, stamped with the newest time,
/// and a lock's start or end takes the one held before it, staying behind the
/// motion of the lock it ends.
#[test]
fn a_locked_pointers_motion_sums_and_a_lock_change_takes_the_one_held() {
    let mut held = HoldBack::new();
    let _ = hold(&mut held, lock(WINDOW, true));
    let _ = hold(&mut held, locked_motion(WINDOW, 3, -1, 10));
    let _ = hold(&mut held, locked_motion(WINDOW, 2, 5, 20));
    let _ = hold(&mut held, lock(WINDOW, false));
    assert_eq!(
        drain_events(&mut held),
        [locked_motion(WINDOW, 5, 4, 20), lock(WINDOW, false)]
    );

    // A window that toggles its lock without draining owes one lock change.
    for locked in [true, false, true, false, true] {
        let _ = hold(&mut held, lock(WINDOW, locked));
    }
    assert_eq!(drain_events(&mut held), [lock(WINDOW, true)]);

    // A modifier change ends a run of motion.
    let shifted = WindowEvent::PointerMotion {
        window_id: WINDOW,
        dx: 1,
        dy: 1,
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        at_ns: 30,
    };
    let _ = hold(&mut held, locked_motion(WINDOW, 1, 1, 25));
    let _ = hold(&mut held, shifted);
    assert_eq!(drain_events(&mut held).len(), 2);
}
