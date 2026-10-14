//! The accept queue a listener's pending children form: taken from the
//! front, left from anywhere, and dropped whole with its listener — each in
//! constant time, never by searching the table.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::*;

const PORT: u16 = 8080;

fn owner() -> ProcId {
    ProcId::from_raw([0x51; 16])
}

fn entry(id: SocketId, proto: Proto) -> SocketEntry {
    SocketEntry {
        id,
        owner: owner(),
        owner_pid: 7,
        deliver_port: 0x4000,
        family: NetAddrFamily::V4,
        local_addr: [0u8; 16],
        local_port: PORT,
        accounted_bytes: 0,
        owed_at: None,
        proto,
    }
}

/// A table holding a listener on [`PORT`] and `children` pending children it
/// completed in turn, and their handles, oldest first.
fn listening(children: usize) -> (SocketService, usize, Vec<SocketId>) {
    let mut svc = SocketService::new(HashSeed::from_words(1, 2));
    let id = svc.alloc_id();
    let lindex = svc
        .insert_entry(entry(
            id,
            Proto::Listen(Box::new(Listening {
                listener: Listener::new(PORT, ListenConfig::default()),
                oldest: None,
                newest: None,
            })),
        ))
        .expect("a listener");
    let mut ids = Vec::new();
    for peer_port in 0..children {
        let earlier = match &svc.sockets[lindex].proto {
            Proto::Listen(listening) => listening.newest,
            _ => None,
        };
        let id = svc.alloc_id();
        let port = 40_000 + u16::try_from(peer_port).expect("few");
        svc.insert_entry(entry(
            id,
            Proto::Stream(Some(Box::new(StreamConn {
                tcb: Tcb::listen(TcpConfig::default(), PORT, port, 0x1000),
                peer: SocketAddr {
                    family: NetAddrFamily::V4,
                    addr: [0u8; 16],
                    port,
                },
                iface: [0u8; IF_NAME_LEN],
                notified: Notified::Nothing,
                holder: Holder::Pending {
                    earlier,
                    later: None,
                },
                read_shutdown: false,
                room_owed: false,
            }))),
        ))
        .expect("a child");
        svc.queue_newest(lindex, id);
        ids.push(id);
    }
    svc.assert_indices_agree();
    (svc, lindex, ids)
}

/// The handles on the listener's queue, oldest first.
fn queued(svc: &SocketService) -> Vec<SocketId> {
    let Some(lindex) = svc.demux_index(NetAddrFamily::V4, PORT) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    let mut next = svc.oldest_pending(lindex);
    while let Some(at) = next {
        ids.push(svc.sockets[at].id);
        next = match &svc.sockets[at].proto {
            Proto::Stream(Some(conn)) => match conn.holder {
                Holder::Pending { later, .. } => later.and_then(|id| svc.by_id.get(&id).copied()),
                Holder::Client | Holder::Closed => None,
            },
            _ => None,
        };
    }
    ids
}

fn at(svc: &SocketService, id: SocketId) -> usize {
    *svc.by_id.get(&id).expect("live")
}

#[test]
fn children_queue_in_the_order_their_handshakes_completed() {
    let (svc, lindex, ids) = listening(3);
    assert_eq!(queued(&svc), ids);
    assert_eq!(svc.oldest_pending(lindex), Some(at(&svc, ids[0])));
}

#[test]
fn accepting_takes_the_oldest_and_leaving_takes_any() {
    let (mut svc, lindex, ids) = listening(4);
    let oldest = svc.oldest_pending(lindex).expect("ready");
    svc.release_holder(oldest, Holder::Client);
    svc.assert_indices_agree();
    assert_eq!(queued(&svc), &ids[1..], "accepted from the front");

    svc.release_holder(at(&svc, ids[2]), Holder::Closed);
    svc.assert_indices_agree();
    assert_eq!(queued(&svc), [ids[1], ids[3]], "left from the middle");

    svc.remove_at(at(&svc, ids[3]));
    svc.assert_indices_agree();
    assert_eq!(queued(&svc), [ids[1]], "removed from the back");

    let last = svc.oldest_pending(lindex).expect("one left");
    svc.release_holder(last, Holder::Client);
    svc.assert_indices_agree();
    assert_eq!(svc.oldest_pending(lindex), None, "and empty");
}

/// A child moved in the table — another socket swap-removed onto its slot —
/// leaves the queue intact, because the queue links handles, not positions.
#[test]
fn the_queue_survives_its_children_moving_in_the_table() {
    let (mut svc, lindex, ids) = listening(3);
    let newest_at = at(&svc, ids[2]);
    svc.release_holder(at(&svc, ids[0]), Holder::Client);
    svc.remove_at(at(&svc, ids[0]));
    assert_ne!(at(&svc, ids[2]), newest_at, "the newest child moved");
    svc.assert_indices_agree();
    assert_eq!(queued(&svc), &ids[1..]);
    assert_eq!(svc.oldest_pending(lindex), Some(at(&svc, ids[1])));
}
