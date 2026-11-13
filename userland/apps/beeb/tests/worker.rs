//! The machine worker driven as the front end drives it: a supervised
//! session over a loopback transport, each reply checked by the client, and
//! the picture copied out of the share.

use tairix_beeb::worker::protocol::{
    session_bounds, Configuration, Lights, Loaded, Ran, Reply, Request, Slot, ROW_MAP, SHARE_BYTES,
    SLOTS,
};
use tairix_beeb::worker::{MachineClient, MachineService};
use tairix_log::DiscardSink;
use tairix_sandbox::loopback::{HeapShare, LoopbackShareLauncher};
use tairix_sandbox::supervise::{send_waiting, Sent};
use tairix_sandbox::{FrameOut, SessionService, SessionStep, SupervisedSession};

type Session<F> = SupervisedSession<LoopbackShareLauncher<F>, DiscardSink>;

fn supervised<S: SessionService, F: FnMut(HeapShare) -> S>(factory: F) -> Session<F> {
    let bounds = session_bounds().expect("bounds");
    let mut session = SupervisedSession::new(
        LoopbackShareLauncher::new(SHARE_BYTES, factory),
        bounds,
        DiscardSink,
    );
    session.start(0).expect("a worker starts");
    session
}

/// The answer to `request`, its bytes, once the client believes it; or why
/// it did not.
fn ask<S: SessionService, F: FnMut(HeapShare) -> S>(
    session: &mut Session<F>,
    client: &mut MachineClient,
    request: &Request<'_>,
) -> Result<Vec<u8>, &'static str> {
    client.ask(request).expect("idle");
    assert_eq!(send_waiting(client, session, 0), Sent::Queued);
    for _ in 0..1000 {
        session.on_readable(0).expect("the worker lives");
        if let Some(answer) = session
            .recv(0, |frame| client.answer(frame).map(|_| frame.to_vec()))
            .expect("a frame")
        {
            return answer;
        }
    }
    panic!("no answer");
}

#[test]
fn the_front_end_runs_the_machine_and_reads_its_picture() {
    let mut session = supervised(|share| MachineService::new(share).expect("a share large enough"));
    let mut client = MachineClient::new();
    let os = tairix_beeb::rom::mos::build().expect("the MOS").bytes;
    let basic = tairix_beeb::rom::basic::build().expect("BASIC").bytes;
    let mut slots = [Slot::Empty; SLOTS];
    slots[15] = Slot::Rom(&basic);
    let configure = Request::Configure(Box::new(Configuration {
        os: &os,
        slots,
        links: 0,
    }));
    assert_eq!(
        ask(&mut session, &mut client, &configure).map(|bytes| bytes == Reply::Done.encode()),
        Ok(true)
    );
    let reply = ask(
        &mut session,
        &mut client,
        &Request::Run {
            cycles: 2_000_000,
            sound: false,
            inputs: Vec::new(),
        },
    )
    .expect("believed");
    let Ok(Reply::Ran(ran)) = Reply::decode(&reply) else {
        panic!("a slice ran")
    };
    let share = session.share().expect("the picture's share").clone();
    let mut canvas = vec![0u8; SHARE_BYTES];
    assert!(share.copy_out(0, &mut canvas));
    assert!(
        ran.row_changed(0) && canvas.iter().any(|&slot| slot != 0),
        "a picture"
    );
}

/// A worker that answers every request with a slice nobody asked for.
struct Liar;

impl SessionService for Liar {
    fn handle(&mut self, _request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        let ran = Ran {
            cycles: 7,
            rows: [0; ROW_MAP],
            sound: &[],
            lights: Lights::default(),
            drives: [false; 2],
            loaded: Loaded::default(),
        };
        let _ = out.frame(&Reply::Ran(ran).encode());
        SessionStep::Continue
    }
}

#[test]
fn a_worker_that_answers_what_was_not_asked_is_condemned() {
    let mut session = supervised(|_| Liar);
    let mut client = MachineClient::new();
    assert!(ask(&mut session, &mut client, &Request::PowerOn).is_err());
    session.condemn(0, "an answer to another request");
    assert!(!session.is_live());
    let due = session.restart_deadline().expect("a replacement is due");
    session.start(due).expect("a replacement starts");
    client.restart();
    assert!(client.is_idle());
}
