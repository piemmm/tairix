//! What a pointer gesture on a listed item means.
//!
//! # Why this is its own module
//!
//! The `Run` binary around it is a freestanding program — it only exists when
//! the crate is built for a bare-metal target — so nothing inside it can be
//! reached by a host test, exactly as for [`crate::command`]. Which gesture
//! opens what is worth testing: a modifier changes what activating a bundle
//! means, a second press of the *same* button completes a different gesture
//! from a second press of the other one, and a press that lands on nothing
//! must break the run rather than pair across it. All of that is a pure
//! function of the press and the remembered one, so it lives here and is
//! covered by the tests beside it.
//!
//! # The gestures
//!
//! | gesture | what it does |
//! |---|---|
//! | click | select the item alone; inside a multi-selection, on release |
//! | ctrl-click | add the item to the selection, or take it out |
//! | shift-click | select the run from the anchor to the item |
//! | click on the listing's ground | clear the selection, unless ctrl or shift is held |
//! | drag on the listing's ground | draw a band selecting what it covers; escape takes it back ([`press_step`]) |
//! | double-click | activate: descend, run a bundle, or open a file |
//! | shift-double-click | list a bundle's contents instead of running it |
//! | click on the one selected item's name | rename it, once the click can no longer pair ([`RenameArm`]) |
//! | right-click | ask the desktop for the context menu on the item |
//!
//! There is no right-*double*-click: the menu the first press opens is the
//! desktop's chain and holds the seat's grab, so the second press is consumed
//! there and never reaches this window. Its "open this and I am done here" verb
//! is a row of the menu instead ([`AfterHandoff::CloseWindow`], reached from
//! `ContextCommand::OpenAndClose`) — discoverable, and reachable from the
//! keyboard, which the gesture never was (`plans/NEW-MENUS.md` D20).
//!
//! The pairing rule itself is the shared engine's
//! ([`DoubleClickTracker`]) — keyed on the button as well as the item, so a
//! left press and a right press are never mistaken for one gesture.

use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode, PointerButtonCode};
use tairix_abi::time::Duration64;
use tairix_abi::window_ipc::{PointerAction, WindowEvent};
use tairix_browse::BundleIntent;
use tairix_geometry::{Point, Scale};
use tairix_input::{ClickKind, DoubleClickTracker, PointerButton};

/// How far a held primary press travels, in *logical* pixels along either
/// axis, before it is a drag rather than a click: past the jitter of a hand
/// holding still.
pub const DRAG_SLOP: u32 = 4;

/// A primary press on a file, from which a drag may begin.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DragArm {
    /// Where the press landed, in window pixels.
    pub at: Point,
}

impl DragArm {
    /// Whether the pointer at `to` has travelled far enough from the press,
    /// at `scale`, for the press to be a drag.
    #[must_use]
    pub fn travelled(&self, to: Point, scale: Scale) -> bool {
        let slop = i64::from(scale.scale_length(DRAG_SLOP));
        let dx = (i64::from(to.x) - i64::from(self.at.x)).abs();
        let dy = (i64::from(to.y) - i64::from(self.at.y)).abs();
        dx.max(dy) >= slop
    }
}

/// The bundle intent a gesture with (or without) shift held means.
///
/// An application bundle is both a program and a directory, so activating one
/// is genuinely ambiguous and shift is the modifier that asks for the
/// directory. One spelling, shared by the pointer and the keyboard, so
/// `Shift+Enter` and a shift-double-click cannot come to mean different
/// things.
#[must_use]
pub const fn bundle_intent(shift: bool) -> BundleIntent {
    if shift {
        BundleIntent::Browse
    } else {
        BundleIntent::Launch
    }
}

/// Whether a completed activation that handed the entry to another program
/// leaves this window with nothing left to do.
///
/// The gesture decides: a plain activation leaves the manager open on the
/// folder it was showing, while a right-double-click means "open this and I am
/// done here". A *descent* never closes the window whichever is asked for — it
/// is the window's new content, so closing it would leave the user with
/// nothing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AfterHandoff {
    /// Keep the window on the folder it is showing.
    Keep,
    /// Close the window: the entry has been handed to another program.
    CloseWindow,
}

/// Where a primary press landed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PressHit {
    /// On the listed item at this index.
    Item(usize),
    /// On the listing's own ground, between or past its items.
    Empty,
    /// On the chrome around the listing: the toolbar, a gutter.
    Chrome,
}

/// How a lone press on an item changes the selection.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SelectHow {
    /// Select the item alone.
    Single,
    /// `Ctrl`: add the item to the selection, or take it out.
    Toggle,
    /// `Shift`: select the run from the selection's anchor to the item.
    Extend,
    /// The item is already one of several selected: keep them all, so a drag
    /// carries the whole selection, and select the item alone only if the
    /// press is released without dragging.
    Hold,
}

/// What a primary press on the listing resolved to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PrimaryPress {
    /// The second press of a double-click on the item at `index`: select and
    /// activate it.
    Activate {
        /// The item the pair landed on.
        index: usize,
    },
    /// A lone press on the item at `index`, changing the selection `how`.
    Select {
        /// The item the press landed on.
        index: usize,
        /// What the press does to the selection.
        how: SelectHow,
    },
    /// A press on the listing's ground: the selection is cleared unless a
    /// modifier asks to `keep` it, and a drag from here draws a marquee.
    Empty {
        /// Whether `Ctrl` or `Shift` was held, adding to the selection.
        keep: bool,
    },
    /// The press landed on the chrome around the listing.
    Chrome,
}

/// The selection keys held with a press.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SelectKeys {
    /// `Ctrl` toggles one item.
    pub toggle: bool,
    /// `Shift` extends from the anchor.
    pub extend: bool,
}

/// Decide what a primary press at monotonic time `now_ns` means, given where
/// the hit-test put it, the selection keys held, and whether the item pressed
/// is already one of several selected (`in_selection`).
///
/// A press that resolves to no item cannot begin a pair, so it resets
/// `tracker`: a click *through* the chrome and back onto the same item is never
/// mistaken for a double-click of that item.
pub fn primary_press(
    tracker: &mut DoubleClickTracker,
    now_ns: u64,
    hit: PressHit,
    keys: SelectKeys,
    in_selection: bool,
    interval: Duration64,
) -> PrimaryPress {
    let index = match hit {
        PressHit::Item(index) => index,
        PressHit::Empty => {
            tracker.reset();
            return PrimaryPress::Empty {
                keep: keys.toggle || keys.extend,
            };
        }
        PressHit::Chrome => {
            tracker.reset();
            return PrimaryPress::Chrome;
        }
    };
    let subject = u64::try_from(index).unwrap_or(u64::MAX);
    match tracker.register(now_ns, subject, PointerButton::Primary, interval) {
        ClickKind::Double => PrimaryPress::Activate { index },
        ClickKind::Single => PrimaryPress::Select {
            index,
            how: if keys.toggle {
                SelectHow::Toggle
            } else if keys.extend {
                SelectHow::Extend
            } else if in_selection {
                SelectHow::Hold
            } else {
                SelectHow::Single
            },
        },
    }
}

/// How far a held primary press has got.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PressHold {
    /// Pressed, and not yet travelled past the slop.
    Armed,
    /// Travelled: a band being dragged out.
    Dragging,
}

/// What a window event is to a held press.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PressInput {
    /// The pointer moved.
    Moved,
    /// The primary button went down.
    PrimaryPressed,
    /// The primary button came up.
    PrimaryReleased,
    /// Another button went down.
    OtherPressed,
    /// Any other pointer event: another button up, or the wheel.
    OtherPointer,
    /// `Escape` went down.
    Escape,
    /// Any other key event.
    Key,
    /// The window lost the keyboard.
    Unfocused,
    /// Anything else.
    Other,
}

impl PressInput {
    /// What `event` is to a held press.
    #[must_use]
    pub const fn of(event: &WindowEvent) -> Self {
        match *event {
            WindowEvent::Pointer { action, .. } => match action {
                PointerAction::Moved => Self::Moved,
                PointerAction::Pressed(PointerButtonCode::Primary) => Self::PrimaryPressed,
                PointerAction::Released(PointerButtonCode::Primary) => Self::PrimaryReleased,
                PointerAction::Pressed(_) => Self::OtherPressed,
                PointerAction::Released(_) => Self::OtherPointer,
            },
            WindowEvent::Scrolled { .. } | WindowEvent::Pinch { .. } => Self::OtherPointer,
            WindowEvent::Key {
                key:
                    KeyInput::Pressed {
                        key: KeyValue::Named(NamedKeyCode::Escape),
                        ..
                    },
                ..
            } => Self::Escape,
            WindowEvent::Key { .. } => Self::Key,
            WindowEvent::Focus { focused: false, .. } => Self::Unfocused,
            _ => Self::Other,
        }
    }
}

/// Whether a window holds the keyboard, as its focus reports tell it.
///
/// The session reports a window coming forward and then delivers the press
/// that brought it, so a press straight after a gain is that press rather than
/// one on a window already in use.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Keyboard {
    /// Elsewhere.
    #[default]
    Away,
    /// Just gained: a press now is the one that brought the window forward.
    Arriving,
    /// Held since before the event in hand.
    Held,
}

impl Keyboard {
    /// Whether the window held the keyboard before `event`, and what it holds
    /// after it.
    #[must_use]
    pub const fn step(self, event: &WindowEvent) -> (bool, Self) {
        let next = match *event {
            WindowEvent::Focus { focused: true, .. } => Self::Arriving,
            WindowEvent::Focus { focused: false, .. } => Self::Away,
            _ => match self {
                Self::Away => Self::Away,
                Self::Arriving | Self::Held => Self::Held,
            },
        };
        (matches!(self, Self::Held), next)
    }
}

/// What committing an inline rename did.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RenameCommit {
    /// The entry took its new name, so its folder may re-sort.
    Renamed,
    /// The name was unchanged, so nothing moved.
    Unchanged,
    /// The name was refused; the editor stays open with its reason.
    Refused,
}

impl RenameCommit {
    /// Whether the press outside the editor that committed it goes on to act
    /// on the listing: only when nothing moved, since a re-sorted folder puts
    /// another entry where the user aimed.
    #[must_use]
    pub const fn press_acts(self) -> bool {
        matches!(self, Self::Unchanged)
    }
}

/// What a press that selected an item knew about it, which decides whether it
/// may become a rename ([`RenameArm::on_press`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NamePress {
    /// What the press did to the selection.
    pub how: SelectHow,
    /// The item was already the one selected item before the press.
    pub was_chosen: bool,
    /// The press landed on the item's drawn name.
    pub on_name: bool,
    /// The window held the keyboard before the press, so this is not the
    /// press that brought it forward.
    pub keyboard_held: bool,
}

/// A click on the name of the one selected item, waiting to open its rename.
///
/// It opens once the double-click interval has passed since the click's
/// release, the first moment the click can no longer pair into an
/// activation; anything but hovering lets it go first.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RenameArm {
    index: usize,
    at: Point,
    due_ns: Option<u64>,
}

impl RenameArm {
    /// The arm a primary press at `at` on the item at `index` sets, or `None`
    /// unless it was a plain click on the drawn name of the item that was
    /// already the one selected, in a window that held the keyboard.
    #[must_use]
    pub fn on_press(index: usize, at: Point, press: NamePress) -> Option<Self> {
        (press.how == SelectHow::Single && press.was_chosen && press.on_name && press.keyboard_held)
            .then_some(Self {
                index,
                at,
                due_ns: None,
            })
    }

    /// The item whose rename this arm opens.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// What `input`, arriving at monotonic `now_ns` with the pointer at `to`,
    /// does to the arm, at `scale` and double-click `interval`.
    ///
    /// The release starts the wait. A press, a key, a scroll, another button,
    /// or the window losing the keyboard lets the arm go, as does travelling
    /// past the drag slop before the release; hovering afterwards does not.
    #[must_use]
    pub fn step(
        self,
        input: PressInput,
        to: Point,
        now_ns: u64,
        interval: Duration64,
        scale: Scale,
    ) -> Option<Self> {
        match input {
            PressInput::Moved => {
                let dragged = self.due_ns.is_none() && DragArm { at: self.at }.travelled(to, scale);
                (!dragged).then_some(self)
            }
            PressInput::PrimaryReleased => {
                Some(Self {
                    due_ns: Some(self.due_ns.unwrap_or_else(|| {
                        now_ns.saturating_add(interval.saturating_total_nanos())
                    })),
                    ..self
                })
            }
            PressInput::Other => Some(self),
            PressInput::PrimaryPressed
            | PressInput::OtherPressed
            | PressInput::OtherPointer
            | PressInput::Escape
            | PressInput::Key
            | PressInput::Unfocused => None,
        }
    }

    /// The listing moved under the arm: carry it to where `moved` says its
    /// item now is, or let it go once that answers nothing, so it renames the
    /// item clicked and never one that took its place.
    #[must_use]
    pub fn follow(self, moved: impl FnOnce(usize) -> Option<usize>) -> Option<Self> {
        moved(self.index).map(|index| Self { index, ..self })
    }

    /// When the rename opens, once the click has been released.
    #[must_use]
    pub const fn due(&self) -> Option<u64> {
        self.due_ns
    }

    /// Whether the rename opens at `now_ns`.
    #[must_use]
    pub fn is_due(&self, now_ns: u64) -> bool {
        self.due_ns.is_some_and(|due| now_ns >= due)
    }
}

/// What a held press does with an event.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PressStep {
    /// Follow the pointer: a band grows, and an arm begins once it travels.
    Grow,
    /// End, keeping what it selected.
    End,
    /// End, keeping what it selected, and route the event as though nothing
    /// were held.
    EndAndRoute,
    /// Take back what it selected, and end.
    Cancel,
    /// Swallow the event.
    Hold,
    /// Not the press's: route it.
    Route,
}

/// What a press at `hold` does with `input`: a band on the listing's ground,
/// or a press on a selected item that may yet become a drag.
///
/// A dragging band holds every pointer event and key until its release, so
/// nothing can navigate, open a dialog over it, or change the selection it is
/// diffing. A primary press while it drags proves the release that would have
/// ended it went elsewhere, so it ends the band and is routed as the press it
/// is. An arm has changed nothing yet, so a key, another button, a second
/// press, or the window losing the keyboard lets it go: a menu or dialog that
/// took its release never leaves it to begin on the next hover.
#[must_use]
pub const fn press_step(hold: PressHold, input: PressInput) -> PressStep {
    match (hold, input) {
        (_, PressInput::Moved) => PressStep::Grow,
        (_, PressInput::PrimaryReleased) => PressStep::End,
        (PressHold::Dragging, PressInput::Escape) => PressStep::Cancel,
        (
            PressHold::Dragging,
            PressInput::OtherPressed | PressInput::OtherPointer | PressInput::Key,
        ) => PressStep::Hold,
        (_, PressInput::PrimaryPressed | PressInput::Unfocused)
        | (PressHold::Armed, PressInput::OtherPressed | PressInput::Escape | PressInput::Key) => {
            PressStep::EndAndRoute
        }
        (PressHold::Armed, PressInput::OtherPointer) | (_, PressInput::Other) => PressStep::Route,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bundle_intent, press_step, primary_press as press_with, DragArm, Keyboard, NamePress,
        PressHit, PressHold, PressInput, PressStep, PrimaryPress, RenameArm, RenameCommit,
        SelectHow, SelectKeys, DRAG_SLOP,
    };
    use tairix_abi::input::{KeyInput, KeyValue, Modifiers, NamedKeyCode, PointerButtonCode};
    use tairix_abi::window_ipc::{PointerAction, WindowEvent};
    use tairix_geometry::{Point, Scale};

    use tairix_abi::desktop::DOUBLE_CLICK_DEFAULT;
    use tairix_abi::time::Duration64;
    use tairix_browse::BundleIntent;
    use tairix_input::DoubleClickTracker;

    /// A press with no selection keys held, on an item not already among
    /// several selected (`None` is the chrome).
    fn primary_press(
        tracker: &mut DoubleClickTracker,
        now_ns: u64,
        index: Option<usize>,
        interval: Duration64,
    ) -> PrimaryPress {
        let hit = index.map_or(PressHit::Chrome, PressHit::Item);
        press_with(tracker, now_ns, hit, SelectKeys::default(), false, interval)
    }

    const fn single(index: usize) -> PrimaryPress {
        PrimaryPress::Select {
            index,
            how: SelectHow::Single,
        }
    }

    /// A press outside the rename editor acts only on a listing that did not
    /// move: a rename that took may re-sort the folder under the pointer, and
    /// a refusal keeps the editor open.
    #[test]
    fn only_a_commit_that_moved_nothing_lets_its_press_act() {
        assert!(RenameCommit::Unchanged.press_acts());
        assert!(!RenameCommit::Renamed.press_acts());
        assert!(!RenameCommit::Refused.press_acts());
    }

    #[test]
    fn a_press_on_the_listings_ground_clears_unless_a_key_keeps_the_selection() {
        let mut tracker = DoubleClickTracker::new();
        let keys = |toggle, extend| SelectKeys { toggle, extend };
        for (held, keep) in [
            (keys(false, false), false),
            (keys(true, false), true),
            (keys(false, true), true),
        ] {
            assert_eq!(
                press_with(
                    &mut tracker,
                    0,
                    PressHit::Empty,
                    held,
                    false,
                    DOUBLE_CLICK_DEFAULT
                ),
                PrimaryPress::Empty { keep }
            );
        }
    }

    #[test]
    fn the_selection_keys_toggle_or_extend_and_a_press_inside_several_holds_them() {
        let mut tracker = DoubleClickTracker::new();
        let mut once = |keys, in_selection| {
            tracker.reset();
            press_with(
                &mut tracker,
                0,
                PressHit::Item(3),
                keys,
                in_selection,
                DOUBLE_CLICK_DEFAULT,
            )
        };
        let how = |press| match press {
            PrimaryPress::Select { how, .. } => how,
            other => panic!("not a select: {other:?}"),
        };
        let ctrl = SelectKeys {
            toggle: true,
            extend: false,
        };
        let shift = SelectKeys {
            toggle: false,
            extend: true,
        };
        assert_eq!(how(once(ctrl, true)), SelectHow::Toggle);
        assert_eq!(how(once(shift, false)), SelectHow::Extend);
        assert_eq!(how(once(SelectKeys::default(), true)), SelectHow::Hold);
        assert_eq!(how(once(SelectKeys::default(), false)), SelectHow::Single);
    }

    #[test]
    fn a_press_on_the_ground_breaks_a_half_finished_pair() {
        let mut tracker = DoubleClickTracker::new();
        assert_eq!(
            primary_press(&mut tracker, 0, Some(2), DOUBLE_CLICK_DEFAULT),
            single(2)
        );
        press_with(
            &mut tracker,
            1,
            PressHit::Empty,
            SelectKeys::default(),
            false,
            DOUBLE_CLICK_DEFAULT,
        );
        assert_eq!(
            primary_press(&mut tracker, 2, Some(2), DOUBLE_CLICK_DEFAULT),
            single(2)
        );
    }

    #[test]
    fn a_press_is_a_drag_only_once_it_travels_past_the_slop() {
        let arm = DragArm {
            at: Point::new(100, 100),
        };
        let slop = i32::try_from(DRAG_SLOP).expect("small");
        assert!(
            !arm.travelled(Point::new(100 + slop - 1, 100 - slop + 1), Scale::ONE),
            "jitter"
        );
        assert!(
            arm.travelled(Point::new(100, 100 - slop), Scale::ONE),
            "either axis"
        );
        let doubled = Scale::from_percent(200).expect("a valid scale");
        assert!(
            !arm.travelled(Point::new(100 + slop, 100), doubled),
            "the slop is logical, so a denser screen asks for more pixels"
        );
    }

    #[test]
    fn a_lone_left_click_selects_and_a_quick_second_activates() {
        let mut tracker = DoubleClickTracker::new();
        assert_eq!(
            primary_press(&mut tracker, 0, Some(2), DOUBLE_CLICK_DEFAULT),
            single(2)
        );
        assert_eq!(
            primary_press(&mut tracker, 1_000, Some(2), DOUBLE_CLICK_DEFAULT),
            PrimaryPress::Activate { index: 2 }
        );
    }

    #[test]
    fn a_left_click_on_the_chrome_breaks_the_run() {
        let mut tracker = DoubleClickTracker::new();
        assert_eq!(
            primary_press(&mut tracker, 0, Some(2), DOUBLE_CLICK_DEFAULT),
            single(2)
        );
        assert_eq!(
            primary_press(&mut tracker, 1, None, DOUBLE_CLICK_DEFAULT),
            PrimaryPress::Chrome
        );
        // Back on the same item, the run has been broken: a fresh single.
        assert_eq!(
            primary_press(&mut tracker, 2, Some(2), DOUBLE_CLICK_DEFAULT),
            single(2)
        );
    }

    #[test]
    fn a_right_click_breaks_a_half_finished_left_pair() {
        let mut tracker = DoubleClickTracker::new();
        assert_eq!(
            primary_press(&mut tracker, 0, Some(2), DOUBLE_CLICK_DEFAULT),
            single(2)
        );
        // A right press asks the desktop for the menu and resets the tracker,
        // exactly as the app's own secondary-press path does, so the click after it
        // is a fresh single rather than the second half of the left pair.
        tracker.reset();
        assert_eq!(
            primary_press(&mut tracker, 1, Some(2), DOUBLE_CLICK_DEFAULT),
            single(2)
        );
    }

    /// A second press is paired under the interval it is handed, so the one
    /// the desktop publishes is the one the listing honours.
    #[test]
    fn a_press_pairs_under_the_interval_the_desktop_publishes() {
        let mut tracker = DoubleClickTracker::new();
        let short = tairix_abi::time::Duration64::from_millis(200);
        assert_eq!(primary_press(&mut tracker, 0, Some(2), short), single(2));
        assert_eq!(
            primary_press(&mut tracker, 300_000_000, Some(2), short),
            single(2),
            "300 ms is past a 200 ms interval"
        );
    }

    #[test]
    fn shift_asks_for_the_bundles_contents_and_nothing_else_does() {
        assert_eq!(bundle_intent(true), BundleIntent::Browse);
        assert_eq!(bundle_intent(false), BundleIntent::Launch);
    }

    fn pointer(action: PointerAction) -> WindowEvent {
        WindowEvent::Pointer {
            window_id: 1,
            x: 5,
            y: 5,
            action,
            modifiers: Modifiers::default(),
        }
    }

    fn key(key: KeyValue) -> WindowEvent {
        WindowEvent::Key {
            window_id: 1,
            key: KeyInput::Pressed {
                key,
                modifiers: Modifiers::default(),
            },
        }
    }

    #[test]
    fn a_held_press_reads_each_event_by_what_it_is() {
        use PointerButtonCode::{Primary, Secondary};
        let cases = [
            (pointer(PointerAction::Moved), PressInput::Moved),
            (
                pointer(PointerAction::Pressed(Primary)),
                PressInput::PrimaryPressed,
            ),
            (
                pointer(PointerAction::Released(Primary)),
                PressInput::PrimaryReleased,
            ),
            (
                pointer(PointerAction::Pressed(Secondary)),
                PressInput::OtherPressed,
            ),
            (
                pointer(PointerAction::Released(Secondary)),
                PressInput::OtherPointer,
            ),
            (
                key(KeyValue::Named(NamedKeyCode::Escape)),
                PressInput::Escape,
            ),
            (key(KeyValue::Named(NamedKeyCode::Delete)), PressInput::Key),
            (key(KeyValue::Char('a')), PressInput::Key),
            (
                WindowEvent::Focus {
                    window_id: 1,
                    focused: false,
                },
                PressInput::Unfocused,
            ),
            (
                WindowEvent::Focus {
                    window_id: 1,
                    focused: true,
                },
                PressInput::Other,
            ),
        ];
        for (event, input) in cases {
            assert_eq!(PressInput::of(&event), input, "{event:?}");
        }
    }

    /// A dragging band holds every key and pointer event to its release, so no
    /// key can navigate or open a dialog under it; a primary press proves its
    /// release was lost and ends it, routed as the press it is.
    #[test]
    fn a_dragging_band_holds_the_window_until_its_release() {
        let live = |input| press_step(PressHold::Dragging, input);
        assert_eq!(live(PressInput::Moved), PressStep::Grow);
        assert_eq!(live(PressInput::PrimaryReleased), PressStep::End);
        assert_eq!(live(PressInput::Escape), PressStep::Cancel);
        for held in [
            PressInput::Key,
            PressInput::OtherPressed,
            PressInput::OtherPointer,
        ] {
            assert_eq!(live(held), PressStep::Hold, "{held:?}");
        }
        assert_eq!(live(PressInput::PrimaryPressed), PressStep::EndAndRoute);
        assert_eq!(live(PressInput::Unfocused), PressStep::EndAndRoute);
        assert_eq!(live(PressInput::Other), PressStep::Route);
    }

    /// The press that brings a window forward arrives straight after its focus
    /// gain, so it is not one on a window that held the keyboard; whatever the
    /// window sees next settles the gain, and losing focus is losing it.
    #[test]
    fn the_press_after_a_focus_gain_is_the_one_that_brought_the_window_forward() {
        let gained = WindowEvent::Focus {
            window_id: 1,
            focused: true,
        };
        let lost = WindowEvent::Focus {
            window_id: 1,
            focused: false,
        };
        let press = pointer(PointerAction::Pressed(PointerButtonCode::Primary));
        let moved = pointer(PointerAction::Moved);
        let (_, arriving) = Keyboard::Away.step(&gained);
        assert_eq!(arriving, Keyboard::Arriving);
        assert_eq!(arriving.step(&press), (false, Keyboard::Held));
        assert_eq!(arriving.step(&moved), (false, Keyboard::Held));
        assert_eq!(Keyboard::Held.step(&press), (true, Keyboard::Held));
        assert_eq!(Keyboard::Held.step(&lost), (true, Keyboard::Away));
        assert_eq!(Keyboard::Away.step(&press), (false, Keyboard::Away));
    }

    /// A plain click on the name of the item already chosen, in a window that
    /// held the keyboard.
    const fn name_click() -> NamePress {
        NamePress {
            how: SelectHow::Single,
            was_chosen: true,
            on_name: true,
            keyboard_held: true,
        }
    }

    /// Only a plain click on the drawn name of the one item already selected,
    /// in a window that held the keyboard, arms a rename: the click that brings
    /// a window forward, or that selects the item, never starts editing.
    #[test]
    fn only_a_plain_click_on_the_chosen_items_name_arms_a_rename() {
        let at = Point::new(10, 10);
        assert!(RenameArm::on_press(3, at, name_click()).is_some());
        for press in [
            NamePress {
                how: SelectHow::Toggle,
                ..name_click()
            },
            NamePress {
                how: SelectHow::Extend,
                ..name_click()
            },
            NamePress {
                how: SelectHow::Hold,
                ..name_click()
            },
            NamePress {
                was_chosen: false,
                ..name_click()
            },
            NamePress {
                on_name: false,
                ..name_click()
            },
            NamePress {
                keyboard_held: false,
                ..name_click()
            },
        ] {
            assert_eq!(RenameArm::on_press(3, at, press), None, "{press:?}");
        }
    }

    /// The wait starts at the release and lasts the double-click interval, so
    /// the rename opens the first moment the click can no longer pair.
    #[test]
    fn a_rename_opens_one_interval_after_the_release() {
        let interval = Duration64::from_millis(400);
        let arm = RenameArm::on_press(3, Point::new(10, 10), name_click()).expect("armed");
        assert_eq!(arm.due(), None);
        assert!(!arm.is_due(u64::MAX), "nothing is due before the release");
        let released = arm
            .step(
                PressInput::PrimaryReleased,
                Point::new(11, 10),
                1_000,
                interval,
                Scale::ONE,
            )
            .expect("still armed");
        assert_eq!(released.due(), Some(400_001_000));
        assert!(!released.is_due(400_000_999));
        assert!(released.is_due(400_001_000));
        assert_eq!(released.index(), 3);
    }

    /// A listing change carries the arm with its item, and an item that went
    /// takes the arm with it.
    #[test]
    fn a_rename_arm_follows_its_item_or_goes_with_it() {
        let arm = RenameArm::on_press(3, Point::new(1, 1), name_click()).expect("armed");
        assert_eq!(
            arm.follow(|at| Some(at + 2)).map(|arm| arm.index()),
            Some(5)
        );
        assert_eq!(arm.follow(|_| None), None);
    }

    /// Hovering after the release keeps the arm; travelling past the drag slop
    /// before it is a drag, and every other input lets the arm go.
    #[test]
    fn anything_but_hovering_lets_a_rename_arm_go() {
        let interval = Duration64::from_millis(400);
        let at = Point::new(100, 100);
        let slop = i32::try_from(DRAG_SLOP).expect("small");
        let arm = RenameArm::on_press(3, at, name_click()).expect("armed");
        let step = |arm: RenameArm, input, to| arm.step(input, to, 0, interval, Scale::ONE);
        assert!(
            step(arm, PressInput::Moved, Point::new(101, 100)).is_some(),
            "jitter"
        );
        assert_eq!(
            step(arm, PressInput::Moved, Point::new(100 + slop, 100)),
            None,
            "a drag"
        );
        let released = step(arm, PressInput::PrimaryReleased, at).expect("released");
        assert!(
            step(released, PressInput::Moved, Point::new(300, 300)).is_some(),
            "hovering after the release"
        );
        assert!(step(released, PressInput::Other, at).is_some());
        for input in [
            PressInput::PrimaryPressed,
            PressInput::OtherPressed,
            PressInput::OtherPointer,
            PressInput::Escape,
            PressInput::Key,
            PressInput::Unfocused,
        ] {
            assert_eq!(step(released, input, at), None, "{input:?}");
        }
    }

    /// An arm has changed nothing, so a right-click, a key, a second press, or
    /// the window losing the keyboard drops it and is routed as usual: a menu
    /// or dialog opened from it never leaves it to begin on the next hover.
    #[test]
    fn an_arm_lets_go_of_anything_but_its_own_drag() {
        let armed = |input| press_step(PressHold::Armed, input);
        assert_eq!(armed(PressInput::Moved), PressStep::Grow);
        assert_eq!(armed(PressInput::PrimaryReleased), PressStep::End);
        for dropped in [
            PressInput::OtherPressed,
            PressInput::Escape,
            PressInput::Key,
            PressInput::PrimaryPressed,
            PressInput::Unfocused,
        ] {
            assert_eq!(armed(dropped), PressStep::EndAndRoute, "{dropped:?}");
        }
        assert_eq!(armed(PressInput::OtherPointer), PressStep::Route);
        assert_eq!(armed(PressInput::Other), PressStep::Route);
    }
}
