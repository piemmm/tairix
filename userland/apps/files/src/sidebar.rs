//! The places / devices rail's input routing: every decision the shortcut
//! column down the leading edge of the file manager's window makes about a
//! pointer or keyboard event.
//!
//! # Why this is its own module
//!
//! The `Run` binary around it is a freestanding program — it only exists when
//! the crate is built for a bare-metal target — so nothing inside it can be
//! reached by a host test. The rail's behaviour is worth testing: which press
//! lands on which row, which key moves the cursor, where the focus goes, and
//! what happens when a place cannot be listed. All of that is a pure function
//! of the event, the window's pointer, the rail model, and the browser, so it
//! lives here, compiles on the host, and is covered by the tests beside it.
//!
//! # No I/O, and no reporting
//!
//! Nothing here opens, lists, or writes anything. Reading the mount table and
//! the user's home is the program's job, and so is stating a refusal on the
//! error stream: a navigation the filesystem refuses comes back as
//! [`SidebarOutcome::refused`] — the text to state — so the program keeps the
//! one reporting path it already has rather than growing a second one.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode, PointerButtonCode};
use tairix_abi::window_ipc::{PointerAction, WindowEvent};
use tairix_browse::render::{
    sidebar_index_at, sidebar_reveal, sidebar_scroll_pointer, sidebar_scroll_wheel, sidebar_view,
    toolbar_command_at,
};
use tairix_browse::{Browser, DirectorySource, Places, ToolbarBand, ToolbarCommand, Volume};
use tairix_controls::damage;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_theme::Theme;
use tairix_window::{pointer_input_events, pointer_point, Repaint};

use crate::chrome::{Accelerator, Chrome};

/// What routing an event to the rail did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SidebarOutcome {
    /// What the window owes the screen: nothing, the rectangles the rail
    /// reported, or the whole window when the listing behind it moved too.
    pub repaint: Repaint,
    /// The reason a place could not be opened, ready to be stated on the
    /// error stream, or `None` when nothing was refused.
    ///
    /// Carried out rather than written here so the program states it through
    /// the single fail-loud reporting path it already uses for every other
    /// refusal, and so a test can read exactly what a user would be told.
    pub refused: Option<String>,
}

impl Default for SidebarOutcome {
    fn default() -> Self {
        Self::QUIET
    }
}

impl SidebarOutcome {
    /// Nothing changed and nothing was refused.
    pub const QUIET: Self = Self {
        repaint: Repaint::Nothing,
        refused: None,
    };

    /// An outcome that repainted what it reported (or nothing at all) and
    /// refused nothing.
    #[must_use]
    pub const fn reported(changed: bool) -> Self {
        Self {
            repaint: if changed {
                Repaint::Reported
            } else {
                Repaint::Nothing
            },
            refused: None,
        }
    }
}

/// The rail's drawn interaction state — every field of it a round can move
/// without the listing beside it changing.
///
/// The rail is drawn from this state rather than from retained per-row
/// controls, so what a round repainted is the difference between the state
/// before it and the state after: exactly the two rows a mark moved between,
/// or the whole rail when the focus flipped, since a rail that holds the
/// keyboard draws *every* row as a member of the focus field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RailMark {
    focused: bool,
    cursor: usize,
    hovered: Option<usize>,
}

impl RailMark {
    /// The rail's drawn state right now.
    #[must_use]
    pub fn of(places: &Places) -> Self {
        Self {
            focused: places.is_focused(),
            cursor: places.cursor(),
            hovered: places.hovered(),
        }
    }

    /// Report what moving from `self` to the rail's current state repainted,
    /// answering whether anything did.
    ///
    /// The window is the *whole* window: the rail lays out below the toolbar
    /// band across the leading edge, and that inset is the renderer's own
    /// ([`sidebar_view`]), so the reported rectangles are the painted ones.
    pub fn report(
        self,
        places: &Places,
        scale: Scale,
        theme: &Theme,
        window: Rect,
        toolbar: ToolbarBand,
        damage: &mut Region,
    ) -> bool {
        let now = Self::of(places);
        if now == self {
            return false;
        }
        let Some(view) = sidebar_view(window, scale, theme, Some(places), toolbar) else {
            return true;
        };
        if now.focused != self.focused {
            damage.add(view.rail_rect());
            return true;
        }
        damage::move_mark(
            self.hovered,
            now.hovered,
            |row| view.shown_row_rect(row),
            damage,
        );
        // The cursor is drawn only while the rail holds the keyboard.
        if now.focused {
            damage::move_mark(
                Some(self.cursor),
                Some(now.cursor),
                |row| view.shown_row_rect(row),
                damage,
            );
        }
        true
    }
}

/// The window-local point of a primary-button press, or `None` for any other
/// pointer action.
///
/// The rail's hit-test and the view's own routers below it all resolve a press
/// the same way, so this is the one definition of "a primary press, here"
/// rather than a copy per router.
#[must_use]
pub fn press_point(action: PointerAction, x: u32, y: u32) -> Option<Point> {
    if action != PointerAction::Pressed(PointerButtonCode::Primary) {
        return None;
    }
    Some(pointer_point(x, y))
}

/// Whether `event` is the user asking the window to re-read what is there:
/// the refresh accelerator or a press on the toolbar's Refresh command.
///
/// An attach or a removal reaches the rail through the mount notice without
/// one. `window` is the whole window, the band the toolbar was actually drawn
/// across, so the press is tested against the control the user saw.
pub fn is_refresh_request<S: DirectorySource>(
    browser: &Browser<S>,
    scale: Scale,
    theme: &Theme,
    window: Rect,
    toolbar: ToolbarBand,
    event: &WindowEvent,
) -> bool {
    match event {
        WindowEvent::Key {
            key: KeyInput::Pressed { key, modifiers },
            ..
        } => {
            Accelerator::of(*key, *modifiers) == Some(Accelerator::Command(ToolbarCommand::Refresh))
        }
        WindowEvent::Pointer { x, y, action, .. } => press_point(*action, *x, *y)
            .and_then(|point| toolbar_command_at(browser, scale, theme, window, toolbar, point))
            .is_some_and(|command| command == ToolbarCommand::Refresh),
        _ => false,
    }
}

/// Rebuild `places` from a freshly read `home` and `volumes`, keeping the
/// keyboard focus, the cursor and the scroll where they were.
///
/// A refresh must never move the selection out from under the user, so the
/// interaction state survives the rebuild; a cursor past the end of the new
/// rail is simply not restored, which leaves it at the first row, and a scroll
/// past the end of a shorter rail is clamped wherever it is next read. The row
/// lit under the pointer is found by the whole repaint that follows
/// ([`follow_pointer`]).
pub fn refresh_places(places: &mut Places, home: &[String], volumes: &[Volume]) {
    let focused = places.is_focused();
    let cursor = places.cursor();
    let scroll = core::mem::take(places.scroll_mut());
    *places = Places::new(home, volumes);
    places.set_focused(focused);
    places.set_cursor(cursor);
    *places.scroll_mut() = scroll;
}

/// Light the row under `pointer` in a window showing `chrome`, ahead of a
/// repaint of the whole window.
///
/// A rebuilt rail, a toggled band, a resize and a re-theme each move the rows
/// under a pointer that did not move, and none of them says which rows, so a
/// whole repaint finds the row again rather than drawing the one the rows left
/// behind. A window drawing no rail lights none of its rows.
pub fn follow_pointer(
    places: &mut Places,
    (window, scale, theme): (Rect, Scale, &Theme),
    chrome: Chrome,
    pointer: Option<Point>,
) {
    let row = pointer.and_then(|at| {
        sidebar_index_at(
            window,
            scale,
            theme,
            chrome.rail(places),
            chrome.toolbar,
            at,
        )
    });
    places.set_hovered(row);
}

/// Track a pointer motion over the rail — its bar's look and any drag the bar
/// holds, then the row it lights — reporting what that repainted and
/// answering whether anything did.
///
/// Kept apart from [`apply_event`] because a motion that *leaves* the rail
/// must both clear the highlight and still reach the view below — the bundle
/// drag-out detector depends on seeing every motion.
pub fn track_hover(
    places: &mut Places,
    scale: Scale,
    theme: &Theme,
    window: Rect,
    toolbar: ToolbarBand,
    event: &WindowEvent,
    damage: &mut Region,
) -> bool {
    let WindowEvent::Pointer {
        x,
        y,
        action: PointerAction::Moved,
        ..
    } = event
    else {
        return false;
    };
    let at = pointer_point(*x, *y);
    let frame = (window, scale, theme);
    let mut drew = damage::sink();
    route_bar(
        places,
        frame,
        toolbar,
        (at, PointerAction::Moved),
        &mut drew,
    );
    rehover(places, frame, toolbar, Some(at), &mut drew);
    for rect in drew.rects() {
        damage.add(*rect);
    }
    !drew.is_empty()
}

/// Light the row `pointer` rests on, reporting the row the highlight left and
/// the row it reached, and answering whether it moved.
///
/// Every motion asks, and so does every round that moved the rows under a
/// pointer that did not move, so the row lit is always the one under it.
fn rehover(
    places: &mut Places,
    (window, scale, theme): (Rect, Scale, &Theme),
    toolbar: ToolbarBand,
    pointer: Option<Point>,
    damage: &mut Region,
) -> bool {
    let view = sidebar_view(window, scale, theme, Some(places), toolbar);
    let row = pointer.and_then(|at| view.as_ref()?.index_at(at));
    let moved = damage::move_mark(
        places.hovered(),
        row,
        |marked| view.as_ref()?.shown_row_rect(marked),
        damage,
    );
    places.set_hovered(row);
    moved
}

/// Route a pointer event to the rail's bar, answering whether the bar took it.
fn route_bar(
    places: &mut Places,
    frame: (Rect, Scale, &Theme),
    toolbar: ToolbarBand,
    (point, action): (Point, PointerAction),
    damage: &mut Region,
) -> bool {
    let mut taken = false;
    for input in pointer_input_events(action, point) {
        taken |= sidebar_scroll_pointer(places, frame, toolbar, (point, &input), damage).is_some();
    }
    taken
}

/// Route one event to the rail, returning `Some(outcome)` when the rail
/// consumed it and `None` when it did not (the caller then routes the event to
/// the browser view as usual).
///
/// The rail owns its bar — a press on it, the drag that follows, and a motion
/// over it — a wheel turn while `pointer`, where the pointer last was over the
/// window, is over the rail, and a primary press on one of its rows — which
/// focuses the rail, puts its cursor on that row, and navigates — and, only
/// while it holds the keyboard focus, the arrows that move its cursor
/// (scrolling it into view), the `Enter` that activates it, and the `Escape`
/// that hands focus back. `Tab` moves the focus between the rail and the file
/// view from either side, so every state the row control offers is reachable
/// from the keyboard alone. A window [`Accelerator`] is never the rail's.
#[allow(clippy::too_many_arguments)] // The rail, its geometry, the pointer, the event, and the round's report.
pub fn apply_event<S: DirectorySource>(
    browser: &mut Browser<S>,
    places: &mut Places,
    scale: Scale,
    theme: &Theme,
    window: Rect,
    toolbar: ToolbarBand,
    pointer: Option<Point>,
    event: &WindowEvent,
    damage: &mut Region,
) -> Option<SidebarOutcome> {
    let before = RailMark::of(places);
    let marked = |places: &Places, damage: &mut Region| {
        SidebarOutcome::reported(before.report(places, scale, theme, window, toolbar, damage))
    };
    let frame = (window, scale, theme);
    match event {
        WindowEvent::Key {
            key: KeyInput::Pressed { key, modifiers },
            ..
        } => {
            if matches!(key, KeyValue::Named(NamedKeyCode::Tab))
                && !modifiers.ctrl
                && !modifiers.alt
            {
                let focused = places.is_focused();
                places.set_focused(!focused);
                return Some(marked(places, damage));
            }
            if !places.is_focused() || Accelerator::of(*key, *modifiers).is_some() {
                return None;
            }
            match key {
                KeyValue::Named(NamedKeyCode::Down | NamedKeyCode::Up) => {
                    let step = if *key == KeyValue::Named(NamedKeyCode::Down) {
                        1
                    } else {
                        -1
                    };
                    places.move_cursor(step);
                    let revealed = sidebar_reveal(places, frame, toolbar, damage);
                    if revealed {
                        rehover(places, frame, toolbar, pointer, damage);
                    }
                    let marks = before.report(places, scale, theme, window, toolbar, damage);
                    Some(SidebarOutcome::reported(revealed || marks))
                }
                // Alt+Enter asks for the selection's Properties, which a place
                // does not have; it must not navigate as a plain Enter does.
                KeyValue::Named(NamedKeyCode::Enter) if !modifiers.alt => {
                    let cursor = places.cursor();
                    Some(navigate_to(browser, places, cursor))
                }
                KeyValue::Named(NamedKeyCode::Escape) => {
                    places.set_focused(false);
                    Some(marked(places, damage))
                }
                // While the rail holds focus its keys are its own: a keystroke
                // it has no use for is swallowed rather than navigating the
                // listing behind it.
                _ => Some(SidebarOutcome::QUIET),
            }
        }
        WindowEvent::Scrolled { dx, dy, .. } => {
            let rail = sidebar_view(window, scale, theme, Some(places), toolbar)?.rail_rect();
            if !pointer.is_some_and(|at| rail.contains(at)) {
                return None;
            }
            let mut moved = sidebar_scroll_wheel(places, frame, toolbar, (*dx, *dy), damage);
            if moved {
                moved |= rehover(places, frame, toolbar, pointer, damage);
            }
            Some(SidebarOutcome::reported(moved))
        }
        // A motion is `track_hover`'s; a press or release on the bar, or the
        // release ending a drag it holds, is the bar's.
        WindowEvent::Pointer { x, y, action, .. } if *action != PointerAction::Moved => {
            let mut drew = damage::sink();
            let taken = route_bar(
                places,
                frame,
                toolbar,
                (pointer_point(*x, *y), *action),
                &mut drew,
            );
            for rect in drew.rects() {
                damage.add(*rect);
            }
            if taken {
                return Some(SidebarOutcome::reported(!drew.is_empty()));
            }
            let point = press_point(*action, *x, *y)?;
            let index = sidebar_index_at(window, scale, theme, Some(places), toolbar, point)?;
            places.set_focused(true);
            places.set_cursor(index);
            let mut outcome = navigate_to(browser, places, index);
            if outcome.repaint == Repaint::Nothing {
                // The press moved the focus and the cursor even where it
                // navigated nowhere, so those rows are what it repainted.
                outcome.repaint = marked(places, damage).repaint;
            }
            Some(outcome)
        }
        _ => None,
    }
}

/// Navigate the browser to the place at `index`.
///
/// A row the rail has already found unavailable does nothing — a disabled
/// control does not act. A navigation the filesystem refuses leaves the
/// browser exactly where it was, marks the row unavailable so it reads
/// disabled from then on, and returns the reason to state: a place that cannot
/// be listed says so rather than wedging or blanking the window.
pub fn navigate_to<S: DirectorySource>(
    browser: &mut Browser<S>,
    places: &mut Places,
    index: usize,
) -> SidebarOutcome {
    let Some(place) = places.rows().get(index) else {
        return SidebarOutcome::QUIET;
    };
    if !place.is_available() {
        return SidebarOutcome::QUIET;
    }
    let label = String::from(place.label());
    let components: Vec<String> = place.components().to_vec();
    let Ok(moved) = browser.navigate_to(components) else {
        places.set_unavailable(index);
        return SidebarOutcome {
            // The row reads disabled from now on and the refusal is stated;
            // which rows that changes is not a report the rail can make.
            repaint: Repaint::Whole,
            refused: Some(alloc::format!("could not open {label}")),
        };
    };
    SidebarOutcome {
        // A move replaces the listing, the toolbar's enable states, and the
        // rail's own selected row together — no report describes that.
        repaint: if moved {
            Repaint::Whole
        } else {
            Repaint::Nothing
        },
        refused: None,
    }
}

#[cfg(test)]
#[path = "sidebar_tests.rs"]
mod tests;
