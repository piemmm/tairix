//! The worker's port: one relay parked on for each context at most, the
//! rest waited for in place; serials and mailboxes never made twice; run and
//! hold at once, a held session's waits holding until it runs; quits judged
//! as they come; the credit each context owes told once; and the room the port
//! holds from the start, a relay, answer or launch it has no room for failing
//! alone.

use core::iter;

use tairix_fuzzseed::meter::{refusing_above, refusing_after};
use tairix_sandbox::wire::Writer;

use super::*;
use crate::credit::INPUT_WINDOW;

/// A link whose commands are scripted and whose events are kept: closed
/// once a wait for ever finds nothing left.
#[derive(Default)]
struct Scripted {
    commands: VecDeque<Vec<u8>>,
    sent: Vec<Vec<u8>>,
}

impl WorkerPort for Scripted {
    fn recv<R>(
        &mut self,
        wait: Option<u64>,
        take: impl FnOnce(&[u8]) -> R,
    ) -> Result<Waited<R>, ProtoError> {
        Ok(match (self.commands.pop_front(), wait) {
            (Some(frame), _) => Waited::Frame(take(&frame)),
            (None, None) => Waited::Closed,
            (None, Some(_)) => Waited::Nothing,
        })
    }

    fn queue(&mut self, payload: &[u8]) -> Result<(), ProtoError> {
        self.sent.push(payload.to_vec());
        Ok(())
    }

    fn flush(&mut self) -> Result<(), ProtoError> {
        Ok(())
    }

    fn now(&self) -> u64 {
        0
    }
}

fn encoded(command: &Command<'_>) -> Vec<u8> {
    let mut w = Writer::new();
    command.encode(&mut w);
    w.finish()
}

fn answer(serial: u32, reply: &[u8]) -> Vec<u8> {
    encoded(&Command::Answer { serial, reply })
}

fn port() -> Port<Scripted> {
    Port::new(Scripted::default(), 0, 0).expect("room")
}

/// The context the tests' relays are made for.
const CONTEXT: Context = 1;

/// A port whose [`CONTEXT`] is current, with nothing outstanding.
fn current() -> Rc<RefCell<Port<Scripted>>> {
    let port = Rc::new(RefCell::new(port()));
    assert!(port.borrow_mut().open(CONTEXT));
    port.borrow_mut().current = CONTEXT;
    port
}

/// A port whose [`CONTEXT`] has made `requests`, each relay parked on.
fn parked(requests: &[&[u8]]) -> Rc<RefCell<Port<Scripted>>> {
    let port = current();
    let mut host = WorkerHost(Rc::clone(&port));
    for request in requests {
        let relayed = host.relay(Side::Files, request, &mut Vec::new(), true);
        assert_eq!(relayed, Relayed::Pending);
    }
    port
}

/// Whether each relay `port` has sent was parked on.
fn relays_parked(port: &Port<Scripted>) -> Vec<bool> {
    port.link
        .sent
        .iter()
        .filter_map(|payload| match Event::decode(payload) {
            Ok(Event::Relay { parked, .. }) => Some(parked),
            _ => None,
        })
        .collect()
}

#[test]
fn a_context_parked_on_a_relay_waits_in_place_for_another() {
    let port = parked(&[b"first"]);
    port.borrow_mut().link.commands.push_back(answer(1, b"two"));
    let mut host = WorkerHost(Rc::clone(&port));
    let mut reply = Vec::new();
    let second = host.relay(Side::Files, b"second", &mut reply, true);
    assert_eq!(second, Relayed::Answered, "waited for in place");
    assert_eq!(reply, b"two");
    let mut port = port.borrow_mut();
    assert_eq!(relays_parked(&port), [true, false]);
    port.link.commands.push_back(answer(0, b"one"));
    port.take_commands(Some(0));
    assert_eq!(port.end, None);
    assert!(matches!(port.reply(CONTEXT), Some(Reply::Answered(bytes)) if bytes == b"one"));
}

#[test]
fn an_abandoned_relay_s_answer_is_dropped() {
    let port = parked(&[b"read"]);
    let mut port = port.borrow_mut();
    let serial = port.parked.take().expect("left pending");
    port.abandon(serial);
    port.link.commands.push_back(answer(serial, b"late"));
    port.take_commands(Some(0));
    assert_eq!(port.end, None);
    assert!(port.reply(CONTEXT).is_none(), "no context waits on it");
}

#[test]
fn a_serial_still_outstanding_is_not_given_again_once_they_wrap() {
    let port = parked(&[b"held"]);
    let mut host = WorkerHost(Rc::clone(&port));
    port.borrow_mut().next_serial = 0;
    port.borrow_mut().current = CONTEXT + 1;
    let again = host.relay(Side::Files, b"again", &mut Vec::new(), true);
    assert_eq!(again, Relayed::Failed, "serial 0 is still outstanding");
    let next = host.relay(Side::Files, b"next", &mut Vec::new(), true);
    assert_eq!(next, Relayed::Pending);
}

#[test]
fn a_second_mailbox_is_not_made_over_the_first() {
    let port = parked(&[b"read"]);
    let mut port = port.borrow_mut();
    port.link.commands.push_back(answer(0, b"reply"));
    port.take_commands(Some(0));
    assert!(!port.open(CONTEXT));
    assert!(port.answered(CONTEXT), "the reply it held kept");
}

#[test]
fn run_and_hold_apply_at_once_and_launches_and_quits_are_kept_in_order() {
    let mut port = port();
    assert!(port.open(3));
    let launch = encoded(&Command::Launch {
        serial: 7,
        line: b"Go",
    });
    port.link.commands.extend([
        launch,
        encoded(&Command::Quit { context: 3 }),
        encoded(&Command::Run),
    ]);
    port.take_commands(Some(0));
    assert!(port.running);
    assert!(matches!(
        port.kept.iter().collect::<Vec<_>>()[..],
        [Kept::Launch { serial: 7, .. }, Kept::Quit { context: 3 }]
    ));
    port.close(3);
    assert!(
        matches!(
            port.kept.iter().collect::<Vec<_>>()[..],
            [Kept::Launch { .. }]
        ),
        "a quit of a context that ended let go with it"
    );
}

#[test]
fn a_quit_is_judged_as_it_comes() {
    let mut port = port();
    assert!(port.open(1) && port.open(2));
    port.close(2);
    port.link
        .commands
        .push_back(encoded(&Command::Quit { context: 2 }));
    port.take_commands(Some(0));
    assert!(
        port.kept.is_empty(),
        "one that has ended: it crossed the end"
    );
    assert_eq!(port.end, None);
    for never in [BOOT, 3] {
        let mut port = self::port();
        assert!(port.open(1));
        port.link
            .commands
            .push_back(encoded(&Command::Quit { context: never }));
        port.take_commands(Some(0));
        assert_eq!(
            port.end,
            Some(End::Violation("a quit of a context never made")),
            "{never}"
        );
    }
}

#[test]
fn a_held_session_s_waits_hold_until_it_runs_again() {
    let port = Rc::new(RefCell::new(port()));
    let mut host = WorkerHost(Rc::clone(&port));
    assert_eq!(
        host.wait(Some(5), Ending::Clock),
        Pulse::Stop,
        "held past its deadline, until the session closed"
    );
    let port = Rc::new(RefCell::new(self::port()));
    let mut host = WorkerHost(Rc::clone(&port));
    port.borrow_mut()
        .link
        .commands
        .push_back(encoded(&Command::Run));
    assert_eq!(host.pulse(), Pulse::Continue, "running, it goes on");
    port.borrow_mut()
        .link
        .commands
        .push_back(encoded(&Command::Hold));
    assert_eq!(host.pulse(), Pulse::Stop, "held until the session closed");
}

#[test]
fn credit_owed_is_told_once_for_each_context_that_took_input_in() {
    let mut port = port();
    assert!(port.open(1) && port.open(2) && port.open(3));
    for context in [1, 2, 3] {
        port.input(context, InputEvent::Key(b'a'));
        port.input(context, InputEvent::Key(b'b'));
    }
    port.current = 1;
    assert!(port.take_input().is_some() && port.take_input().is_some());
    port.current = 2;
    assert!(port.take_input().is_some());
    port.flush();
    port.flush();
    let credits: Vec<_> = port
        .link
        .sent
        .iter()
        .filter_map(|payload| match Event::decode(payload) {
            Ok(Event::Credit { context, applied }) => Some((context, applied)),
            _ => None,
        })
        .collect();
    assert_eq!(credits, [(1, 2), (2, 1)], "context 3 took nothing in");
    port.current = 3;
    assert!(port.take_input().is_some());
    port.close(3);
    assert_eq!(port.owing, 0, "a closed mailbox owes nothing");
}

/// As long as a launch's line may be: longer than any request the port
/// makes of its own, so refusing every request of its length refuses only
/// its copy.
const LONG: [u8; 1023] = [b'L'; 1023];

#[test]
fn a_port_or_a_mailbox_there_is_no_room_for_is_not_made() {
    assert!(refusing_after(0, || Port::new(Scripted::default(), 0, 0)).is_none());
    let mut port = port();
    assert!(!refusing_after(0, || port.open(1)), "no room for its inbox");
    assert_eq!(port.made, BOOT, "nothing of it noted");
    assert!(port.open(1), "made once there is room");
}

#[test]
fn a_made_context_s_credit_window_and_the_commands_kept_need_no_more_room() {
    let last = Context::try_from(WAITING_MOST).expect("a context");
    let mut port = port();
    for context in 1..=last {
        assert!(port.open(context));
    }
    let key = encoded(&Command::Input {
        context: 1,
        event: InputEvent::Key(b'k'),
    });
    port.link
        .commands
        .extend(iter::repeat_n(key, INPUT_WINDOW as usize));
    port.link
        .commands
        .extend((1..=last).map(|context| encoded(&Command::Quit { context })));
    refusing_after(0, || port.take_commands(Some(0)));
    assert_eq!(port.end, None, "no command lost for want of room");
    assert_eq!(port.kept.len(), WAITING_MOST);
    port.current = 1;
    assert_eq!(
        iter::from_fn(|| port.take_input()).count(),
        INPUT_WINDOW as usize
    );
}

#[test]
fn a_reply_there_is_no_room_to_copy_fails_its_relay() {
    let port = parked(&[b"read"]);
    let mut port = port.borrow_mut();
    port.link.commands.push_back(answer(0, &LONG));
    refusing_above(LONG.len() - 1, || port.take_commands(Some(0)));
    assert_eq!(
        port.end, None,
        "the worker's want of room, not the front end's"
    );
    assert!(matches!(port.reply(CONTEXT), Some(Reply::Failed)));
}

#[test]
fn a_relay_there_is_no_room_to_note_fails_untold() {
    let port = current();
    let mut host = WorkerHost(Rc::clone(&port));
    let relayed = refusing_after(0, || {
        host.relay(Side::Files, b"read", &mut Vec::new(), true)
    });
    assert_eq!(relayed, Relayed::Failed);
    let port = port.borrow();
    assert!(
        port.link.sent.is_empty(),
        "the front end told nothing of it"
    );
    assert_eq!(port.end, None);
}

#[test]
fn an_answer_there_is_no_room_to_append_fails_its_relay() {
    let port = current();
    port.borrow_mut().link.commands.push_back(answer(0, &LONG));
    let mut host = WorkerHost(Rc::clone(&port));
    let mut reply = b"head".to_vec();
    // The answer's copy asks for its length, and its append for more.
    let relayed = refusing_above(LONG.len() + 1, || {
        host.relay(Side::Files, b"read", &mut reply, false)
    });
    assert_eq!(relayed, Relayed::Failed);
    assert_eq!(reply, b"head", "nothing appended");
    assert_eq!(port.borrow().end, None);
}

#[test]
fn a_launch_whose_line_there_is_no_room_to_keep_is_refused() {
    let mut port = port();
    port.link.commands.push_back(encoded(&Command::Launch {
        serial: 9,
        line: &LONG,
    }));
    refusing_above(LONG.len() - 1, || port.take_commands(Some(0)));
    assert_eq!(port.end, None);
    assert!(port.kept.is_empty(), "nothing kept of it");
    let refused: Vec<_> = port
        .link
        .sent
        .iter()
        .filter_map(|payload| match Event::decode(payload) {
            Ok(Event::Refused { serial, error }) => Some((serial, error.is_none())),
            _ => None,
        })
        .collect();
    assert_eq!(refused, [(9, true)], "with no error of the guest's");
}
