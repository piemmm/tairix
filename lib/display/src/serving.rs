//! One turn of the display service's serve loop: what each wake does, and the
//! service of the display's event sources that a call driving the display
//! owes before the loop parks on those sources again.

use tairix_abi::display_ipc::DISPLAY_MAX_REQUEST;
use tairix_abi::driver::display::{Display, OutputMode};
use tairix_abi::seat::DisplayLease;
use tairix_abi::time::MonotonicClock;
use tairix_log::{Event, EventId, Field, FieldValue, Level, Sink};

use crate::events::{
    EVENTS_FAILED, FIRST_PRESENT, FIRST_PRESENT_MESSAGE, LATCH_OVERDUE, LATCH_UNDELIVERED,
};
use crate::server::{output_mode, Delivery, DisplayServer, Latch, PeerFacts, DISPLAY_REPLY_MAX};

/// What woke the serve loop.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Woke {
    /// A queued present's deadline, or an owed latch's retry, fell due.
    Due,
    /// The port an owed latch waits on has room.
    Room,
    /// The boot seat's lease moved.
    Lease,
    /// One of the display's event sources.
    Events,
    /// A request at the endpoint.
    Request,
}

/// The kernel's side of the serve loop that the engine is not handed: the
/// endpoint's calls, the lease's and the output's announcements, and the
/// system log.
pub(crate) trait Rendezvous: Sink {
    /// The next request at the endpoint, without waiting: its length in
    /// `request`, and its ticket.
    fn take_request(&mut self, request: &mut [u8]) -> Option<(usize, u64)>;
    /// Answer the request `ticket` with `reply`.
    fn reply(&mut self, ticket: u64, reply: &[u8]);
    /// The boot seat's lease as the kernel publishes it.
    fn lease(&mut self) -> Option<DisplayLease>;
    /// Announce `output` where it is not the output last announced.
    fn follow(&mut self, output: Option<OutputMode>);
}

/// The serve loop between parks.
pub(crate) struct Serving<C: MonotonicClock, R> {
    pub(crate) server: DisplayServer<C>,
    rendezvous: R,
    request: [u8; DISPLAY_MAX_REQUEST],
    reply: [u8; DISPLAY_REPLY_MAX],
    first_present_logged: bool,
}

impl<C: MonotonicClock, R: Rendezvous> Serving<C, R> {
    pub(crate) const fn new(server: DisplayServer<C>, rendezvous: R) -> Self {
        Self {
            server,
            rendezvous,
            request: [0; DISPLAY_MAX_REQUEST],
            reply: [0; DISPLAY_REPLY_MAX],
            first_present_logged: false,
        }
    }

    /// Act on `woke`. A display whose calls can take its event sources'
    /// wakes then has them serviced after every call that drove it, since
    /// the loop parks on those wakes next.
    pub(crate) fn on(
        &mut self,
        display: &mut dyn Display,
        peer: &mut dyn PeerFacts,
        delivery: &mut dyn Delivery,
        woke: Woke,
    ) {
        let drove = match woke {
            Woke::Due => {
                let latch = self.server.expire(display, delivery);
                self.note_latch(latch);
                true
            }
            Woke::Room => {
                self.server.redeliver(delivery);
                false
            }
            Woke::Lease => match self.rendezvous.lease() {
                Some(lease) => {
                    self.server.lease_moved(display, lease);
                    true
                }
                None => false,
            },
            Woke::Events => {
                self.service(display, delivery);
                false
            }
            Woke::Request => self.request(display, peer, delivery),
        };
        if drove && display.requests_take_wakes() {
            self.service(display, delivery);
        }
    }

    #[cfg(test)]
    pub(crate) const fn rendezvous(&self) -> &R {
        &self.rendezvous
    }

    /// Record how many latches presenters' ports refused for good, once the
    /// count has doubled since it was last recorded.
    pub(crate) fn report_undelivered(&mut self) {
        let Some((total, refusal)) = self.server.undelivered_to_report() else {
            return;
        };
        self.record(
            Level::Warn,
            LATCH_UNDELIVERED,
            "display: latches presenters' ports refused were given up on",
            &[
                Field {
                    key: "total",
                    value: FieldValue::UnsignedInt(total),
                },
                Field {
                    key: "error",
                    value: FieldValue::SignedInt(i64::from(refusal.as_i32())),
                },
            ],
        );
    }

    /// Serve the request a wake reported, answering whether one was there.
    fn request(
        &mut self,
        display: &mut dyn Display,
        peer: &mut dyn PeerFacts,
        delivery: &mut dyn Delivery,
    ) -> bool {
        // Non-blocking: the park point is the wait set, and the call the wake
        // reported may have been cancelled by its poster's exit.
        let Some((len, ticket)) = self.rendezvous.take_request(&mut self.request) else {
            return false;
        };
        let request = &self.request[..len.min(DISPLAY_MAX_REQUEST)];
        let n = self
            .server
            .serve(display, peer, delivery, ticket, request, &mut self.reply);
        self.rendezvous
            .reply(ticket, &self.reply[..n.min(DISPLAY_REPLY_MAX)]);
        if !self.first_present_logged && self.server.has_presented() {
            self.first_present_logged = true;
            self.record(Level::Info, FIRST_PRESENT, FIRST_PRESENT_MESSAGE, &[]);
        }
        true
    }

    /// Hand what woke the display's event sources to the server, and
    /// announce an output that moved.
    fn service(&mut self, display: &mut dyn Display, delivery: &mut dyn Delivery) {
        match display.service_events() {
            Ok(serviced) => {
                let latch = self.server.serviced(serviced, delivery);
                self.note_latch(latch);
            }
            Err(err) => self.record(
                Level::Warn,
                EVENTS_FAILED,
                "display: its event sources could not be read",
                &[Field {
                    key: "error",
                    value: FieldValue::SignedInt(i64::from(err.as_i32())),
                }],
            ),
        }
        self.rendezvous.follow(output_mode(display));
    }

    /// Record a queued present released at its deadline, unlatched. A latch
    /// given up on is recorded by its running total instead.
    fn note_latch(&self, latch: Latch) {
        match latch {
            Latch::Overdue => self.record(
                Level::Warn,
                LATCH_OVERDUE,
                "display: a queued present never latched; its presenter is released",
                &[],
            ),
            Latch::Idle | Latch::Posted | Latch::Owed | Latch::Gone | Latch::Undelivered(_) => {}
        }
    }

    fn record(&self, level: Level, id: EventId, message: &str, fields: &[Field<'_>]) {
        tairix_log::log(
            &self.rendezvous,
            &Event {
                level,
                id,
                message,
                fields,
            },
        );
    }
}
