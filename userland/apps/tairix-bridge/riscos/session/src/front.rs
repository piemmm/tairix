//! The front end's side of a session (`plans/RISCOS-EMULATOR.md` §15.1):
//! what it has asked of the worker and been told, so that it believes an
//! event only where the worker could have sent it, and reports a session
//! that ended with the contexts that were running.
//!
//! The worker runs guest code, so nothing it sends is trusted. An event the
//! model refuses is [`Unbelievable`], changes nothing, and is the reason the
//! front end contains the session with (`SandboxSession::condemn`). The room
//! every event needs is held when the launch or relay it follows is asked,
//! so a refusal is only ever the worker's doing.

use alloc::vec::Vec;

use tairix_collections::{OrderedMap, OrderedSet};
use tairix_sandbox::WorkerEnd;

use crate::credit::Credit;
use crate::protocol::{carries, Context, Event, Screen, BOOT, WAITING_MOST};
use crate::ring::{Placed, Ring};

/// An event the worker could not have sent: why.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Unbelievable(pub &'static str);

/// Why a session ended, as the front end reports it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
    /// The worker ended — it crashed, overran its memory, was killed — as the
    /// sandbox reads its exit.
    Worker(WorkerEnd),
    /// It sent what it could not have.
    Unbelievable(Unbelievable),
}

impl Why {
    /// A terse statement of it, for the user.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Worker(end) => end.reason(),
            Self::Unbelievable(Unbelievable(why)) => why,
        }
    }
}

/// A context launched and not ended.
#[derive(PartialEq, Eq, Debug)]
struct Running {
    /// The command line it was launched with.
    line: Vec<u8>,
    credit: Credit,
    screen: Option<Screen>,
}

/// A relay unanswered: the context it was made for, and whether that
/// context is parked on it rather than the worker waiting for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Unanswered {
    context: Context,
    parked: bool,
}

/// The front end's model of a session.
#[derive(PartialEq, Eq, Debug)]
pub struct Front {
    next_launch: u32,
    /// The launches asked and not answered, with their lines.
    launches: OrderedMap<u32, Vec<u8>>,
    /// The contexts a quit was asked of that have not ended.
    quitting: OrderedSet<Context>,
    running: OrderedMap<Context, Running>,
    /// The last context made: each is numbered above the one before.
    made: Context,
    relays: OrderedMap<u32, Unanswered>,
    ring: Ring,
}

impl Front {
    /// A session's, before anything is asked: none where there is no room
    /// for the session's own relay.
    #[must_use]
    pub fn new() -> Option<Self> {
        let mut relays = OrderedMap::new();
        relays.try_reserve(1).ok()?;
        Some(Self {
            next_launch: 0,
            launches: OrderedMap::new(),
            quitting: OrderedSet::new(),
            running: OrderedMap::new(),
            made: BOOT,
            relays,
            ring: Ring::new(),
        })
    }

    /// Whether another launch or quit may be asked: the worker keeps no more
    /// than [`WAITING_MOST`] waiting.
    fn room_to_ask(&self) -> bool {
        self.launches.len() + self.quitting.len() < WAITING_MOST
    }

    /// A launch of `line` asked: the serial its command goes as, none where
    /// no launch carries the line, as many launches and quits wait as the
    /// worker keeps, or there is no room to keep it and what it makes.
    pub fn launch(&mut self, line: &[u8]) -> Option<u32> {
        let serial = self.next_launch;
        self.next_launch = serial.wrapping_add(1);
        if !carries(line) || !self.room_to_ask() || self.launches.contains_key(&serial) {
            return None;
        }
        // Room for the context each launch asked may make, and for the relay
        // each context may be parked on and the one waited for in place.
        let contexts = self.launches.len() + 1;
        self.running.try_reserve(contexts).ok()?;
        self.relays
            .try_reserve(self.running.len() + contexts + 1)
            .ok()?;
        let mut kept = Vec::new();
        kept.try_reserve_exact(line.len()).ok()?;
        kept.extend_from_slice(line);
        self.launches.try_insert(serial, kept).ok()?;
        Some(serial)
    }

    /// A quit of `context` asked: false for one not running, already
    /// asked, as many launches and quits wait as the worker keeps, or no
    /// room to note it.
    pub fn quit(&mut self, context: Context) -> bool {
        self.running.contains_key(&context)
            && !self.quitting.contains(&context)
            && self.room_to_ask()
            && self.quitting.try_insert(context).is_ok()
    }

    /// An input event for `context` sent, where its credit lets one go now.
    pub fn input(&mut self, context: Context) -> bool {
        self.running
            .get_mut(&context)
            .is_some_and(|running| running.credit.send().is_ok())
    }

    /// Relay `serial` answered: false for one not asked.
    pub fn answer(&mut self, serial: u32) -> bool {
        self.relays.remove(&serial).is_some()
    }

    /// The ring's oldest `bytes` given back, once the records they hold are
    /// copied out: false for more than it holds.
    pub fn release(&mut self, bytes: u32) -> bool {
        self.ring.release(bytes).is_ok()
    }

    /// The screen `context` was last told of.
    #[must_use]
    pub fn screen(&self, context: Context) -> Option<Screen> {
        self.running.get(&context)?.screen
    }

    /// The contexts running, in order, each with the command line it was
    /// launched with.
    pub fn running(&self) -> impl Iterator<Item = (Context, &[u8])> {
        self.running
            .iter()
            .map(|(&context, running)| (context, running.line.as_slice()))
    }

    /// `event` believed, where the worker could have sent it: for damage,
    /// where its record is in the ring, which the front end copies out and
    /// gives back with [`Self::release`].
    ///
    /// # Errors
    ///
    /// [`Unbelievable`] for an event the worker could not have sent, nothing
    /// changed. Each event's room is held as its launch is asked, or as the
    /// model is made, so none is refused for want of room to note it.
    pub fn believe(&mut self, event: &Event<'_>) -> Result<Option<Placed>, Unbelievable> {
        match *event {
            Event::Credit { context, applied } => {
                let running = self.running_mut(context, "a credit for a context not running")?;
                running
                    .credit
                    .taken(applied)
                    .map_err(|_| Unbelievable("a credit for input never sent"))?;
            }
            Event::Launched { serial, context } => {
                if context <= self.made {
                    return Err(Unbelievable("a context numbered as one made before"));
                }
                // Held when the launch was asked, so neither takes any.
                let unnoted = |_| Unbelievable("a launch made with no room to note it");
                self.running.try_reserve(1).map_err(unnoted)?;
                let line = self
                    .launches
                    .remove(&serial)
                    .ok_or(Unbelievable("a launch made that was never asked"))?;
                let running = Running {
                    line,
                    credit: Credit::new(),
                    screen: None,
                };
                self.running.try_insert(context, running).map_err(unnoted)?;
                self.made = context;
            }
            Event::Refused { serial, .. } => {
                self.launches
                    .remove(&serial)
                    .ok_or(Unbelievable("a launch refused that was never asked"))?;
            }
            Event::Screen { context, screen } => {
                self.running_mut(context, "a screen for a context not running")?
                    .screen = Some(screen);
            }
            Event::Damage {
                context,
                first,
                lines,
                offset,
            } => {
                let screen = self
                    .running_mut(context, "damage to a context not running")?
                    .screen
                    .ok_or(Unbelievable("damage to a screen never described"))?;
                if first + lines > screen.height {
                    return Err(Unbelievable("damage below its screen"));
                }
                let bytes = lines
                    .checked_mul(screen.width)
                    .and_then(|pixels| pixels.checked_mul(4))
                    .ok_or(Unbelievable("damage the ring could not hold"))?;
                let placed = self
                    .ring
                    .accept(offset, bytes)
                    .map_err(|_| Unbelievable("damage where the worker could not have put it"))?;
                return Ok(Some(placed));
            }
            Event::Relay {
                serial,
                context,
                parked,
                ..
            } => {
                if context != BOOT && !self.running.contains_key(&context) {
                    return Err(Unbelievable("a relay for a context not running"));
                }
                if parked && context == BOOT {
                    return Err(Unbelievable("the session's own relay parked"));
                }
                if self.relays.contains_key(&serial) {
                    return Err(Unbelievable("a relay as one unanswered"));
                }
                let alike = |other: &Unanswered| {
                    other.parked == parked && (!parked || other.context == context)
                };
                if self.relays.values().any(alike) {
                    return Err(Unbelievable(if parked {
                        "a context parked on a second relay"
                    } else {
                        "a second relay waited for in place"
                    }));
                }
                // Room held when the launch was asked, or the session made.
                self.relays
                    .try_insert(serial, Unanswered { context, parked })
                    .map_err(|_| Unbelievable("a relay with no room to note it"))?;
            }
            Event::Ended { context, .. } => {
                self.running
                    .remove(&context)
                    .ok_or(Unbelievable("an end of a context not running"))?;
                self.quitting.remove(&context);
            }
        }
        Ok(None)
    }

    fn running_mut(
        &mut self,
        context: Context,
        why: &'static str,
    ) -> Result<&mut Running, Unbelievable> {
        self.running.get_mut(&context).ok_or(Unbelievable(why))
    }
}

#[cfg(test)]
mod tests;
