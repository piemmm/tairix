//! The activation clients PID 1 owes a reply once their service is ready.
//!
//! The reply is owed to a call ticket, which only the transport holds, while
//! readiness is the engine's to report, so the two are joined here.

use alloc::vec::Vec;

use tairix_abi::Errno;

use crate::manager::ServiceId;
use crate::service::ClientId;

/// A client parked on its service's readiness, and the ticket its reply is
/// owed to.
#[derive(Debug)]
struct Parked {
    service: ServiceId,
    client: ClientId,
    ticket: u64,
}

/// Room for one client to be parked, taken before the engine queues it.
#[derive(Debug)]
pub struct Room(());

/// Every parked client. Each is one outstanding call on the activation
/// endpoint, so the kernel's bound on those bounds this.
#[derive(Debug, Default)]
pub struct ParkedClients {
    parked: Vec<Parked>,
}

impl ParkedClients {
    /// Room for one more client to be parked.
    ///
    /// Taken before the engine queues the client, so a refusal answers the
    /// caller with nothing queued, where running out after would leave the
    /// client queued and its reply lost.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the heap cannot give the room.
    pub fn make_room(&mut self) -> Result<Room, Errno> {
        self.parked.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        Ok(Room(()))
    }

    /// Park `client` on `service`, its reply owed to `ticket`, in `room`.
    pub fn park(&mut self, _room: Room, service: ServiceId, client: ClientId, ticket: u64) {
        self.parked.push(Parked {
            service,
            client,
            ticket,
        });
    }

    /// Hand `answer` the ticket of every parking of `client` on `service`,
    /// taking each.
    pub fn take(&mut self, client: ClientId, service: ServiceId, mut answer: impl FnMut(u64)) {
        // Swap-remove in place: the entry moved into `at` is the one examined
        // next, so the scan stays one pass.
        let mut at = 0;
        while at < self.parked.len() {
            if self.parked[at].client == client && self.parked[at].service == service {
                answer(self.parked.swap_remove(at).ticket);
            } else {
                at += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use tairix_abi::ProcId;

    fn client(n: u8) -> ClientId {
        ClientId::new(ProcId::from_raw([n; tairix_abi::PROC_ID_LEN]))
    }

    /// Parking a client in room already taken allocates nothing, so a client
    /// the engine has queued is always recorded.
    ///
    /// Regression: the record was allocated after the engine queued the
    /// client, so a refused allocation ended PID 1 with the reply lost.
    #[test]
    fn parking_in_the_room_taken_allocates_nothing() {
        let mut parked = ParkedClients::default();
        let room = parked.make_room().expect("room");
        let held = (parked.parked.capacity(), parked.parked.as_ptr());
        parked.park(room, ServiceId(3), client(7), 70);
        assert_eq!(
            (parked.parked.capacity(), parked.parked.as_ptr()),
            held,
            "the record went into the room taken for it"
        );
    }

    #[test]
    fn a_client_s_tickets_on_its_service_are_taken_and_no_others() {
        let mut parked = ParkedClients::default();
        let (fontd, soundd) = (ServiceId(0), ServiceId(1));
        for (service, who, ticket) in [
            (fontd, 1, 10),
            (soundd, 1, 11),
            (fontd, 2, 12),
            (fontd, 1, 13),
        ] {
            let room = parked.make_room().expect("room");
            parked.park(room, service, client(who), ticket);
        }
        let mut answered = vec![];
        parked.take(client(1), fontd, |ticket| answered.push(ticket));
        answered.sort_unstable();
        assert_eq!(answered, [10, 13]);
        let mut rest = vec![];
        parked.take(client(1), soundd, |ticket| rest.push(ticket));
        parked.take(client(2), fontd, |ticket| rest.push(ticket));
        assert_eq!(rest, [11, 12]);
        assert!(parked.parked.is_empty());
    }
}
