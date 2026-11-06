//! The app-ward event hold-back: what the session owes a window whose
//! owner's mailbox was full.
//!
//! An app's event mailbox is a bounded kernel resource
//! ([`tairix_window::EVENT_MAILBOX_CAPACITY`]), so a send into it can be
//! refused with [`Errno::WouldBlock`] whenever the owner is merely slow, and
//! with [`Errno::LimitExceeded`] while the session's own IPC budget is spent
//! on what other apps have yet to take; the kernel wakes the destination's
//! room member for either. Nothing wakes for kernel memory, so what a send
//! refused [`Errno::OutOfMemory`] is held for a flush the embedder times.
//! Dropping the refused event is right for a *delta* — the next pointer
//! sample supersedes the one before it — and wrong for everything else: a
//! `Resized` the app never sees leaves it laying out at a size the
//! compositor no longer uses, a lost `Focus`/`Minimized`/`CloseRequested`
//! is a state edge with no second telling, and a lost
//! `FilePicked`/`PickCancelled` strands the window's picker for good
//! (the engine clears its pending pick only once the conclusion is
//! accepted). This module is what the session owes instead.
//!
//! # Shape
//!
//! One ordered queue per `(destination mailbox, window)`. Order within a
//! window is exactly the order the events happened; across an owner's
//! windows nothing is ordered, because nothing in the protocol relates two
//! windows' events. A flush therefore serves an owner's windows
//! round-robin — one event each per pass — so a window with a long backlog
//! cannot starve its sibling's `Resized`.
//!
//! An application-scoped event — an icon-bar click or menu outcome — names
//! no window, so it queues under the destination's `None` slot and takes
//! its turn beside the windows in the same round-robin. It is a *discrete*
//! occurrence: a *New window* row chosen twice must open two windows, so
//! neither folds into the other.
//!
//! # Folding
//!
//! Each kind folds by the rule its own quantity obeys, so a destination
//! that stays behind accumulates work in proportion to what the app still
//! needs to know rather than to how long it was away:
//!
//! * A **state edge** (`Focus`, `Resized`, `RedrawRequested`,
//!   `CloseRequested`, `Minimized`) is a value the app converges on, not an
//!   occurrence it must witness. A later one overwrites the held one where
//!   it stands, so one window owes at most one of each.
//! * A **position sample** (`Pointer`/`Moved`) is level-triggered: the
//!   newest supersedes an unbroken run of its predecessors. So is a pinch's
//!   `Update`, whose scale is relative to where the pinch began, while the
//!   modifiers stay the same, a tool window's move sample (`ToolMoved`), and
//!   a report of where a carried drag or save is (`DragOver`, `SaveOffered`):
//!   each is numbered, and only the newest is worth an answer.
//! * The **end of a save offer** (`SaveOfferEnded`) carries where it was
//!   dropped, so it withdraws every offer still held and an earlier end that
//!   only left, a drop's end an earlier drop's too; it is never shed.
//! * The **end of a tool window's move** is where it was let go, which is
//!   all its owner still needs of every move before it: it withdraws the
//!   window's earlier move reports and is never shed.
//! * A **wheel delta** (`Scrolled`) is additive: a run in one direction
//!   made with the same modifiers held sums, at the newest place. A reversal
//!   is a distinct gesture — a turn that clamped at a range end is not undone
//!   by the turn back — and so is a change of modifier (Ctrl let go mid-turn
//!   ends the zoom), each ending the run, decided by the same
//!   `shell::continues` predicate the live drain uses.
//! * Everything else is **discrete** — a key, a button press or release, a
//!   pick conclusion — and every one is owed.
//! * The **end of the press a window holds** — its primary release, or the
//!   cancel that took the press away — is all that stops the app holding
//!   that press for ever, and is never shed on its own.
//!
//! # Bound
//!
//! [`HOLD_BACK_CAPACITY`] caps one window's queue. It is a defence, not a
//! capacity to scale: it is what stops an app that never drains from making
//! the desktop hold memory on its behalf, so it stays fixed. The rest of
//! the footprint is already bounded by the client's live windows, each of
//! which costs a mapped frame region orders of magnitude larger than its
//! queue.
//!
//! Overflow evicts the oldest *input* event — a drag's report and a save's
//! offer among them, each superseded by the next — never a state edge, a pick
//! conclusion, a move's or an offer's end or a press's end — and that is
//! total, not a preference. A held press is shed together with its end, so the app learns
//! of neither rather than holding a press it is never told is over. A window
//! holds one press at a time, so the only end left without its press is the
//! one ending a press already delivered. Folding therefore leaves a window
//! owing at most six state edges, at most one pick conclusion and one
//! authorisation's (a second of either is refused before it reaches the
//! sink), at most one move's end, at most one such press's end, at most one
//! save offer's end that only left, each superseding the one before, and at
//! most one drop's: a window has every event held before a drag's offer by
//! the time it can answer it, so a second drop is held behind a first only for
//! a window answering an offer it was never shown, and then supersedes it.
//! A queue at capacity therefore always holds an input event to shed.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

use tairix_abi::input::PointerButtonCode;
use tairix_abi::ipc::SendRefusal;
use tairix_abi::touch::PinchPhase;
use tairix_abi::window_ipc::{PointerAction, WindowEvent};
use tairix_abi::Errno;

use crate::shell::continues;

/// The most events one window may owe its owner before the oldest input
/// event is shed to make room.
///
/// A security bound rather than a scalable capacity: it is the ceiling on
/// what a client that stops draining can make the session hold. Comfortably
/// above the eleven slots folding can leave un-shed, and above the input a
/// user produces in the seconds before the owner is declared unresponsive.
pub const HOLD_BACK_CAPACITY: usize = 64;

/// What became of one event offered to the hold-back.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Delivery {
    /// It went straight to the destination.
    Sent,
    /// The destination is behind, so the event is owed on its room wake. Its
    /// caller still has an undeliverable event on its hands, which is the
    /// evidence a responsiveness verdict rests on.
    Owed {
        /// `true` when this is the first thing owed to the destination, so
        /// the caller must arm its room wake; a destination already owing
        /// something is already watched.
        watch: bool,
    },
    /// The session's own IPC budget is spent on what other apps have yet to
    /// take, so the event is owed on the destination's room wake — which the
    /// kernel raises when the budget frees — though the destination is not
    /// behind, and nothing counts against it.
    OverBudget {
        /// `true` when this is the first thing owed to the destination, so
        /// the caller must arm its room wake.
        watch: bool,
    },
    /// The kernel had no memory for the send, so the event is owed until a
    /// flush the caller times: no wake reports memory freeing, and the
    /// destination is not behind.
    Starved,
}

/// What a destination's debt waits for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Wait {
    /// Its mailbox to drain, which its room wake reports.
    Room,
    /// The session's own budget to free, which its room wake reports too.
    Budget,
    /// Kernel memory, which nothing reports: a flush the caller times.
    Memory,
}

impl Wait {
    /// What `refusal` waits for, or `None` for one no wait lifts.
    const fn of(refusal: SendRefusal) -> Option<Self> {
        match refusal {
            SendRefusal::Full => Some(Self::Room),
            SendRefusal::Budget => Some(Self::Budget),
            SendRefusal::Memory => Some(Self::Memory),
            SendRefusal::Gone | SendRefusal::Final(_) => None,
        }
    }
}

/// What one destination's flush concluded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Flush {
    /// Everything owed went out.
    Settled,
    /// A send was refused; the rest stays owed, waiting for what refused it.
    Waits(Wait),
    /// The send proved the destination gone; everything owed to it was
    /// discarded.
    Gone,
}

/// What a flush leaves the caller to do: the room wakes it needs no more or
/// needs again, and the clients it lost.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Flushed {
    /// Destinations whose room wake is needed no more: they owe nothing, or
    /// what they owe waits on kernel memory rather than their room.
    pub settled: Vec<u64>,
    /// Destinations whose room wake is needed again: what they owe waited on
    /// kernel memory and now waits on their room.
    pub watch: Vec<u64>,
    /// Destinations whose owner is gone. Their windows go with them; their
    /// wake is dropped like a settled one's.
    pub gone: Vec<u64>,
    /// Events a destination refused for good, which were given up on.
    pub dropped: usize,
}

/// What the session owes each `(destination mailbox, window)` pair, the
/// `None` window naming the destination's application-scoped queue.
#[derive(Debug, Default)]
pub struct HoldBack {
    owed: BTreeMap<(u64, Option<u64>), VecDeque<WindowEvent>>,
    /// What each destination owed something waits for.
    waits: BTreeMap<u64, Wait>,
}

impl HoldBack {
    /// A hold-back owing nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            owed: BTreeMap::new(),
            waits: BTreeMap::new(),
        }
    }

    /// Offer `event` to `send`, and take responsibility for it when the
    /// mailbox refuses it as back-pressure.
    ///
    /// A destination already owed something takes this event unsent:
    /// sending it now would put it ahead of what is queued, and the app
    /// must see its events in the order they happened. `send` carries an
    /// event to the destination it names.
    ///
    /// # Errors
    ///
    /// Any refusal no wait lifts — not [`Errno::WouldBlock`], the
    /// destination's mailbox full, [`Errno::LimitExceeded`], the session's own
    /// budget spent, or [`Errno::OutOfMemory`], the kernel's memory short: the
    /// destination is gone, or the send is one it will never take, so holding
    /// the event would strand it.
    pub fn deliver<F>(
        &mut self,
        endpoint: u64,
        event: &WindowEvent,
        mut send: F,
    ) -> Result<Delivery, Errno>
    where
        F: FnMut(u64, &WindowEvent) -> Result<(), Errno>,
    {
        if !self.owes(endpoint) {
            let Err(refused) = send(endpoint, event) else {
                return Ok(Delivery::Sent);
            };
            let wait = Wait::of(SendRefusal::of(refused)).ok_or(refused)?;
            self.waits.insert(endpoint, wait);
        }
        let watch = self.hold(endpoint, *event);
        Ok(match self.waits.get(&endpoint) {
            Some(Wait::Budget) => Delivery::OverBudget { watch },
            Some(Wait::Memory) => Delivery::Starved,
            Some(Wait::Room) | None => Delivery::Owed { watch },
        })
    }

    /// Take responsibility for `event`, folding it into what `endpoint`
    /// already owes that window where the kind allows, and report whether
    /// this is the first thing owed to `endpoint`.
    fn hold(&mut self, endpoint: u64, event: WindowEvent) -> bool {
        let first = !self.owes(endpoint);
        let queue = self.owed.entry((endpoint, event.window_id())).or_default();
        if !fold(queue, &event) {
            if queue.len() >= HOLD_BACK_CAPACITY {
                shed_oldest_input(queue);
            }
            queue.push_back(event);
        }
        first
    }

    /// Whether `endpoint` is owed anything.
    #[must_use]
    pub fn owes(&self, endpoint: u64) -> bool {
        self.windows_of(endpoint).next().is_some()
    }

    /// Whether `endpoint` is owed a render conclusion for `window_id`.
    ///
    /// A conclusion is never shed, so while one waits no further render is
    /// accepted for the window: what this holds for a client that stopped
    /// draining its mailbox stays bounded by what it had asked for.
    #[must_use]
    pub fn holds_render(&self, endpoint: u64, window_id: u64) -> bool {
        self.owed
            .get(&(endpoint, Some(window_id)))
            .is_some_and(|queue| {
                queue
                    .iter()
                    .any(|event| matches!(event, WindowEvent::PreviewRendered { .. }))
            })
    }

    /// How much `endpoint` owes `window_id` — the queue depth the fold and
    /// the bound act on. `None` asks after its application-scoped queue.
    #[must_use]
    pub fn depth(&self, endpoint: u64, window_id: Option<u64>) -> usize {
        self.owed
            .get(&(endpoint, window_id))
            .map_or(0, VecDeque::len)
    }

    /// Discard everything owed to `endpoint` — its owner is gone, so the
    /// events have nowhere to land. Returns `true` when something was
    /// discarded, so the caller knows a wake is armed and must be dropped.
    pub fn forget(&mut self, endpoint: u64) -> bool {
        self.waits.remove(&endpoint);
        let windows: Vec<Option<u64>> = self.windows_of(endpoint).collect();
        for window_id in &windows {
            self.owed.remove(&(endpoint, *window_id));
        }
        !windows.is_empty()
    }

    /// Offer everything owed to `send`, destination by destination, and
    /// report what the caller must do about each one's room wake.
    ///
    /// Each pass over a destination offers one event per window, so no
    /// window's backlog starves another's. A destination stops at its first
    /// [`Errno::WouldBlock`] or [`Errno::LimitExceeded`] — its mailbox filled
    /// again, or the session's budget ran out, both its room wake's to report —
    /// or [`Errno::OutOfMemory`], which no wake reports, and keeps the rest
    /// owed. An [`Errno::NotFound`] is the owner proving itself gone and
    /// discards the lot. Any other refusal is one the destination cannot
    /// recover from by waiting, so that single event is given up on, counted,
    /// and the rest go on; nothing is ever re-offered in a loop.
    pub fn flush<F>(&mut self, mut send: F) -> Flushed
    where
        F: FnMut(u64, &WindowEvent) -> Result<(), Errno>,
    {
        let mut report = Flushed::default();
        for endpoint in self.endpoints() {
            let flushed = self.flush_one(endpoint, &mut send, &mut report.dropped);
            let watched = self.waits.remove(&endpoint) != Some(Wait::Memory);
            match flushed {
                Flush::Settled => {
                    if watched {
                        report.settled.push(endpoint);
                    }
                }
                Flush::Waits(wait) => {
                    let watching = wait != Wait::Memory;
                    if watched && !watching {
                        report.settled.push(endpoint);
                    } else if watching && !watched {
                        report.watch.push(endpoint);
                    }
                    self.waits.insert(endpoint, wait);
                }
                Flush::Gone => {
                    self.forget(endpoint);
                    report.gone.push(endpoint);
                }
            }
        }
        report
    }

    /// Whether anything owed waits on kernel memory, so the caller owes a
    /// timed flush.
    #[must_use]
    pub fn starved(&self) -> bool {
        self.waits.values().any(|&wait| wait == Wait::Memory)
    }

    /// Drain one destination, round-robin across its windows, counting in
    /// `dropped` what it refused for good.
    fn flush_one<F>(&mut self, endpoint: u64, send: &mut F, dropped: &mut usize) -> Flush
    where
        F: FnMut(u64, &WindowEvent) -> Result<(), Errno>,
    {
        // Fixed for the whole drain: `send` cannot add to the hold-back, so
        // the only change under it is a queue emptying, which `get_mut`
        // then simply misses.
        let windows: Vec<Option<u64>> = self.windows_of(endpoint).collect();
        loop {
            let mut served = false;
            for window_id in &windows {
                let Some(queue) = self.owed.get_mut(&(endpoint, *window_id)) else {
                    continue;
                };
                let Some(event) = queue.front() else {
                    continue;
                };
                if let Err(refused) = send(endpoint, event) {
                    let refusal = SendRefusal::of(refused);
                    if let Some(wait) = Wait::of(refusal) {
                        return Flush::Waits(wait);
                    }
                    if refusal == SendRefusal::Gone {
                        return Flush::Gone;
                    }
                    // Answered for either way, so a flush never re-offers it
                    // for ever.
                    *dropped = dropped.saturating_add(1);
                }
                queue.pop_front();
                served = true;
                if queue.is_empty() {
                    self.owed.remove(&(endpoint, *window_id));
                }
            }
            if !served {
                return Flush::Settled;
            }
        }
    }

    /// The destinations currently owed something, each once.
    fn endpoints(&self) -> Vec<u64> {
        let mut seen: Vec<u64> = Vec::new();
        for (endpoint, _) in self.owed.keys() {
            if seen.last() != Some(endpoint) {
                seen.push(*endpoint);
            }
        }
        seen
    }

    /// The windows `endpoint` owes something to, in id order, the `None`
    /// slot (its application-scoped queue) first.
    fn windows_of(&self, endpoint: u64) -> impl Iterator<Item = Option<u64>> + '_ {
        self.owed
            .range((endpoint, None)..=(endpoint, Some(u64::MAX)))
            .map(|((_, window_id), _)| *window_id)
    }
}

/// Fold `next` into what `queue` already owes, reporting `true` when it left
/// nothing new to queue.
fn fold(queue: &mut VecDeque<WindowEvent>, next: &WindowEvent) -> bool {
    if let Some(held) = state_edge_slot(queue, next) {
        *held = *next;
        return true;
    }
    if withdraw_superseded(queue, next) {
        return false;
    }
    // A sample or a delta folds only into an unbroken run at the tail:
    // anything queued behind it is an occurrence the app must see in order.
    match (queue.back_mut(), *next) {
        (
            Some(
                held @ WindowEvent::Pointer {
                    action: PointerAction::Moved,
                    ..
                },
            ),
            WindowEvent::Pointer {
                action: PointerAction::Moved,
                ..
            },
        ) => {
            // The fold delivers the newest sample whole: an older field kept
            // beside the newer position would report a state the pointer was
            // never in, or an instant it was not there.
            *held = *next;
            true
        }
        (
            Some(WindowEvent::Scrolled {
                x,
                y,
                dx: sideways,
                dy: downward,
                modifiers,
                ..
            }),
            WindowEvent::Scrolled {
                x: newest_x,
                y: newest_y,
                dx: across,
                dy: down,
                modifiers: newest_modifiers,
                ..
            },
        ) if *modifiers == newest_modifiers
            && continues(*sideways, across)
            && continues(*downward, down) =>
        {
            *sideways = sideways.saturating_add(across);
            *downward = downward.saturating_add(down);
            *x = newest_x;
            *y = newest_y;
            true
        }
        (Some(held @ WindowEvent::DragOver { .. }), WindowEvent::DragOver { .. })
        | (Some(held @ WindowEvent::SaveOffered { .. }), WindowEvent::SaveOffered { .. }) => {
            *held = *next;
            true
        }
        (
            Some(WindowEvent::ToolMoved {
                over, ended: false, ..
            }),
            WindowEvent::ToolMoved {
                over: newest,
                ended: false,
                ..
            },
        ) => {
            *over = newest;
            true
        }
        (
            Some(WindowEvent::Pinch {
                phase: PinchPhase::Update,
                x,
                y,
                scale,
                modifiers,
                ..
            }),
            WindowEvent::Pinch {
                phase: PinchPhase::Update,
                x: newest_x,
                y: newest_y,
                scale: newest_scale,
                modifiers: newest_modifiers,
                ..
            },
        ) if *modifiers == newest_modifiers => {
            *x = newest_x;
            *y = newest_y;
            *scale = newest_scale;
            true
        }
        _ => false,
    }
}

/// Withdraw what an ending `next` supersedes, answering whether it was one:
/// a tool window's move end takes every move held before it, and a save
/// offer's end every offer held and every end that only left, a drop's end an
/// earlier drop's too. An end that only left never takes a drop: picks are
/// concurrent, so another pick's drag can end over a window whose answer to an
/// earlier drop that pick still waits on.
fn withdraw_superseded(queue: &mut VecDeque<WindowEvent>, next: &WindowEvent) -> bool {
    match next {
        WindowEvent::ToolMoved { ended: true, .. } => {
            queue.retain(|held| !matches!(held, WindowEvent::ToolMoved { .. }));
        }
        WindowEvent::SaveOfferEnded { dropped, .. } => {
            let drop = dropped.is_some();
            queue.retain(|held| match held {
                WindowEvent::SaveOffered { .. } => false,
                WindowEvent::SaveOfferEnded { dropped, .. } => dropped.is_some() && !drop,
                _ => true,
            });
        }
        _ => return false,
    }
    true
}

/// The held event `next` replaces outright, if `next` is a state edge and
/// one of its kind is already owed.
///
/// A state edge names what *is*, so a window owes at most one of each kind
/// and the newest wins. Replacing where it stands rather than re-queueing at
/// the back keeps the queue bounded without disturbing the order of the
/// occurrences around it.
fn state_edge_slot<'q>(
    queue: &'q mut VecDeque<WindowEvent>,
    next: &WindowEvent,
) -> Option<&'q mut WindowEvent> {
    if !is_state_edge(next) {
        return None;
    }
    queue
        .iter_mut()
        .find(|held| core::mem::discriminant(*held) == core::mem::discriminant(next))
}

/// Whether `event` carries a state the app converges on rather than an
/// occurrence it must witness.
const fn is_state_edge(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::Focus { .. }
            | WindowEvent::Resized { .. }
            | WindowEvent::RedrawRequested { .. }
            | WindowEvent::CloseRequested { .. }
            | WindowEvent::Minimized { .. }
    )
}

/// Whether `event` is an input occurrence — the only class overflow sheds.
///
/// Losing one costs the app a keystroke or a pointer action it can act
/// without; losing a state edge or a pick conclusion costs it something it
/// cannot re-derive.
const fn is_input(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::Key { .. }
            | WindowEvent::Pointer { .. }
            | WindowEvent::Scrolled { .. }
            | WindowEvent::Pinch { .. }
            | WindowEvent::ToolMoved { ended: false, .. }
            | WindowEvent::DragOver { .. }
            | WindowEvent::SaveOffered { .. }
    )
}

/// Whether `event` is a press the window holds until it is told it ended.
/// The primary button is the one that grabs; any other press is a click.
const fn holds_press(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::Pointer {
            action: PointerAction::Pressed(PointerButtonCode::Primary),
            ..
        }
    )
}

/// Whether `event` ends the press a window holds: its release, or the cancel
/// that took it away.
const fn ends_press(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::Pointer {
            action: PointerAction::Released(PointerButtonCode::Primary) | PointerAction::Cancelled,
            ..
        }
    )
}

/// Make room in a queue at capacity by shedding its oldest input event other
/// than a press's end, and a held press together with its end.
///
/// A window holds one press at a time, so the first end after a held press is
/// that press's own. The module's bound argument is why a full queue always
/// holds something to shed; the `None` arm cannot be reached from a queue this
/// module built, and shedding nothing is the fail-safe answer if it ever were.
fn shed_oldest_input(queue: &mut VecDeque<WindowEvent>) {
    let Some(oldest) = queue
        .iter()
        .position(|event| is_input(event) && !ends_press(event))
    else {
        return;
    };
    if queue.get(oldest).is_some_and(holds_press) {
        if let Some(end) = queue.iter().skip(oldest + 1).position(ends_press) {
            queue.remove(oldest + 1 + end);
        }
    }
    queue.remove(oldest);
}
