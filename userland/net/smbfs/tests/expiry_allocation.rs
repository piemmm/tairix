//! A volume's call reaching its deadline with no memory to spare.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tairix_abi::fsserve::{decode_reply, Request};
use tairix_abi::netshare_ipc::NetshareRequest;
use tairix_abi::time::NANOS_PER_SEC;
use tairix_abi::DriverError;
use tairix_fuzzseed::meter::{refusing_above, Metered};
use tairix_smb::peer::DISK_SHARE;
use tairix_smbfs::front::CALL_DEADLINE_NS;
use tairix_smbfs::nodes::ROOT;
use tairix_smbfs::testing::{alice, connect, nas, Bench, PASSWORD};

#[global_allocator]
static ALLOC: Metered = Metered;

/// Refused above this: no list of the calls timing out could be made, while
/// each reply the world keeps is smaller.
const REFUSED_ABOVE: usize = 127;

#[test]
fn a_call_past_its_deadline_is_answered_with_no_memory_to_spare() {
    let (done, finished) = mpsc::channel();
    // On a thread of its own, so an expiry that never ends fails the test
    // rather than hanging it.
    thread::spawn(move || {
        let mut bench = Bench::new(nas());
        let mounting = bench.call(
            alice(),
            &NetshareRequest::Mount {
                connect: connect(PASSWORD),
                share: DISK_SHARE,
                read_only: false,
            },
        );
        let (_, offer) = bench.mounted(mounting);
        // Past what the mount cached of the root, so the call goes to the
        // server, which holds its answer back.
        bench.pass(2 * u64::from(NANOS_PER_SEC));
        bench.world().hold(true);
        let request = Request::NodeInfo { node: ROOT };
        let ticket = bench.serve(offer, &request);
        let volume = bench.world().volume(offer).expect("offered");
        assert!(volume
            .answers
            .iter()
            .all(|(answered, _)| *answered != ticket));
        volume.answers.reserve(8);
        bench.now += CALL_DEADLINE_NS;
        let now = bench.now;
        refusing_above(REFUSED_ABOVE, || bench.front.expire(now));
        let reply = bench.answer(offer, ticket);
        let result = decode_reply(&reply, &request)
            .expect("a reply the kernel reads")
            .result
            .map(|_| ());
        done.send(result).expect("the test waits for it");
    });
    let result = finished
        .recv_timeout(Duration::from_secs(60))
        .expect("the expiry ends");
    assert_eq!(result, Err(DriverError::TimedOut));
}
