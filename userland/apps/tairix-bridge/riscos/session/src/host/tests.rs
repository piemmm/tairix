//! The front end's side of a session over a scripted worker: the start sent
//! first, damage copied out and the ring given back at once, input past its
//! credit held in order and run together, a press dropped past the bound
//! dropping its release, what cannot be believed containing the worker, and
//! a worker's own stop told by its exit status.

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::Errno;
use tairix_log::DiscardSink as NullSink;
use tairix_sandbox::loopback::HeapShare;
use tairix_sandbox::proto::FRAME_HEADER_LEN;
use tairix_sandbox::session::SessionDescriptors;

use super::*;
use crate::credit::INPUT_WINDOW;
use crate::ring::Ring;

/// What the scripted worker sends, what it was sent, and how it ends.
#[derive(Default)]
struct Script {
    /// Framed bytes for the host to read.
    sent: Vec<u8>,
    read_at: usize,
    /// Framed bytes the host wrote.
    written: Vec<u8>,
    /// Its side closed once `sent` is read.
    closed: bool,
    status: Option<i32>,
}

struct Scripted {
    script: Rc<RefCell<Script>>,
    share: Option<Arc<dyn Share>>,
}

impl SessionTransport for Scripted {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut script = self.script.borrow_mut();
        let left = script.sent.len() - script.read_at;
        if left == 0 {
            return if script.closed {
                Ok(0)
            } else {
                Err(Errno::WouldBlock)
            };
        }
        let take = left.min(buf.len());
        let at = script.read_at;
        buf[..take].copy_from_slice(&script.sent[at..at + take]);
        script.read_at += take;
        Ok(take)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        self.script.borrow_mut().written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn descriptors(&self) -> Option<SessionDescriptors> {
        None
    }

    fn share(&self) -> Option<&Arc<dyn Share>> {
        self.share.as_ref()
    }

    fn dispose(self) -> Option<i32> {
        self.script.borrow().status
    }
}

fn begin() -> Begin<'static> {
    Begin {
        ram_pages: 0x4000,
        memo_pages: 16,
        slot_bytes: 0x8_0000,
        real_time: 0,
        zone: None,
        keyboard: 48,
        font: DesktopFont::SHIPPED,
        pressure: PressureBand::Normal,
    }
}

const SCREEN: Screen = Screen {
    width: 4,
    height: 2,
    x_eig: 1,
    y_eig: 1,
    border: 0,
    blanked: false,
};

/// A host over a scripted worker and its ring.
fn started() -> (Host<Scripted, NullSink>, Rc<RefCell<Script>>, HeapShare) {
    let script = Rc::new(RefCell::new(Script::default()));
    let ring = HeapShare::new(RING_BYTES as usize).expect("the ring");
    let transport = Scripted {
        script: Rc::clone(&script),
        share: Some(Arc::new(ring.clone())),
    };
    let host = Host::start(transport, NullSink, &begin()).expect("started");
    (host, script, ring)
}

/// `event` framed for the host to read.
fn tell(script: &Rc<RefCell<Script>>, event: &Event<'_>) {
    let mut w = Writer::new();
    event.encode(&mut w);
    let payload = w.finish();
    let mut script = script.borrow_mut();
    let length = u32::try_from(payload.len()).expect("a frame");
    script.sent.extend_from_slice(&length.to_le_bytes());
    script.sent.extend_from_slice(&payload);
}

/// The commands the host wrote, decoded, as their debug texts.
fn written(host: &mut Host<Scripted, NullSink>, script: &Rc<RefCell<Script>>) -> Vec<Vec<u8>> {
    host.on_writable();
    let script = script.borrow();
    let mut at = 0;
    let mut commands = Vec::new();
    while at + FRAME_HEADER_LEN <= script.written.len() {
        let mut length = [0; 4];
        length.copy_from_slice(&script.written[at..at + FRAME_HEADER_LEN]);
        let end = at + FRAME_HEADER_LEN + u32::from_le_bytes(length) as usize;
        commands.push(script.written[at + FRAME_HEADER_LEN..end].to_vec());
        at = end;
    }
    commands
}

fn decoded(commands: &[Vec<u8>]) -> Vec<Command<'_>> {
    commands
        .iter()
        .map(|bytes| Command::decode(bytes).expect("a command"))
        .collect()
}

/// Every event the host lends now, as its kind.
fn drained(host: &mut Host<Scripted, NullSink>) -> Result<Vec<&'static str>, Why> {
    host.on_readable();
    let mut kinds = Vec::new();
    while let Some(told) = host.told()? {
        kinds.push(match told.event {
            Event::Credit { .. } => "credit",
            Event::Launched { .. } => "launched",
            Event::Screen { .. } => "screen",
            Event::Damage { .. } => "damage",
            Event::Ended { .. } => "ended",
            _ => "other",
        });
    }
    Ok(kinds)
}

/// A host with context 1 launched and its screen told.
fn launched() -> (Host<Scripted, NullSink>, Rc<RefCell<Script>>, HeapShare) {
    let (mut host, script, ring) = started();
    let serial = host.launch(b"BASIC").expect("asked");
    tell(&script, &Event::Launched { serial, context: 1 });
    tell(
        &script,
        &Event::Screen {
            context: 1,
            screen: SCREEN,
        },
    );
    assert_eq!(drained(&mut host), Ok(vec!["launched", "screen"]));
    (host, script, ring)
}

#[test]
fn the_session_s_start_is_the_first_command_sent() {
    let (mut host, script, _) = started();
    host.run(true);
    let commands = written(&mut host, &script);
    assert_eq!(decoded(&commands), [Command::Begin(begin()), Command::Run]);
}

#[test]
fn a_worker_handed_no_ring_is_never_started() {
    let script = Rc::new(RefCell::new(Script::default()));
    let small = HeapShare::new(64).expect("a share");
    for share in [None, Some(Arc::new(small) as Arc<dyn Share>)] {
        let transport = Scripted {
            script: Rc::clone(&script),
            share,
        };
        assert!(matches!(
            Host::start(transport, NullSink, &begin()),
            Err(Unstarted::Share)
        ));
    }
}

#[test]
fn damage_is_copied_out_and_the_ring_given_back_at_once() {
    let (mut host, script, ring) = launched();
    let placed = Ring::new().place(2 * 4 * 4).expect("room");
    let pixels: Vec<u8> = (0..32).collect();
    let mut region = ring.clone();
    assert!(tairix_sandbox::session::ShareWrite::write_with(
        &mut region,
        &mut |bytes| {
            bytes[placed.offset as usize..][..32].copy_from_slice(&pixels);
            true
        }
    ));
    tell(
        &script,
        &Event::Damage {
            context: 1,
            first: 0,
            lines: 2,
            offset: placed.offset,
        },
    );
    host.on_readable();
    let told = host.told().expect("believed").expect("an event");
    assert_eq!(told.pixels, pixels.as_slice());
    let commands = written(&mut host, &script);
    assert_eq!(
        decoded(&commands).last(),
        Some(&Command::Released {
            bytes: placed.released()
        })
    );
}

#[test]
fn input_past_its_credit_waits_and_goes_once_the_credit_comes() {
    let (mut host, script, _) = launched();
    let key = |code| InputEvent::Key(code);
    for code in 0..INPUT_WINDOW {
        assert!(host.input(1, key(u8::try_from(code).unwrap_or(0))));
    }
    assert!(host.input(1, key(0xAA)), "held");
    let motion = |dx| InputEvent::Motion {
        dx,
        dy: 1,
        buttons: 0,
    };
    assert!(
        host.input(1, motion(2)) && host.input(1, motion(3)),
        "run together"
    );
    let before = decoded(&written(&mut host, &script)).len();
    tell(
        &script,
        &Event::Credit {
            context: 1,
            applied: INPUT_WINDOW,
        },
    );
    assert_eq!(drained(&mut host), Ok(vec!["credit"]));
    let commands = written(&mut host, &script);
    let sent: Vec<_> = decoded(&commands)
        .into_iter()
        .skip(before)
        .filter_map(|command| match command {
            Command::Input { event, .. } => Some(event),
            _ => None,
        })
        .collect();
    assert_eq!(
        sent,
        [
            key(0xAA),
            InputEvent::Motion {
                dx: 5,
                dy: 2,
                buttons: 0
            }
        ]
    );
}

#[test]
fn a_press_dropped_past_the_bound_drops_its_release_and_no_other() {
    let mut held = Held::default();
    for _ in 0..HELD_MOST {
        assert!(held.hold(InputEvent::Key(1)));
    }
    let edge = |key, down| InputEvent::Transition { key, down };
    assert!(!held.hold(edge(0x40, true)), "dropped");
    held.events.pop_front();
    assert!(!held.hold(edge(0x40, false)), "its release with it");
    assert!(held.hold(edge(0x41, false)), "another's release is kept");
    assert!(
        !held.hold(edge(0x500, true)),
        "a key past the marks' reach is dropped too"
    );
}

#[test]
fn input_for_a_context_not_running_is_dropped() {
    let (mut host, _, _) = started();
    assert!(!host.input(1, InputEvent::Key(b'a')));
}

#[test]
fn an_event_the_worker_could_not_have_sent_contains_it() {
    let (mut host, script, _) = launched();
    tell(
        &script,
        &Event::Damage {
            context: 9,
            first: 0,
            lines: 1,
            offset: 0,
        },
    );
    let why = drained(&mut host).expect_err("contained");
    assert!(matches!(why, Why::Unbelievable(_)), "{why:?}");
    assert_eq!(host.told().map(|told| told.is_some()), Err(why), "for good");
    assert!(!host.wants_read() && !host.wants_write());
}

#[test]
fn a_worker_that_stops_by_itself_is_told_by_its_status() {
    let (mut host, script, _) = started();
    {
        let mut script = script.borrow_mut();
        script.closed = true;
        script.status = Some(Stopped::Refused.code());
    }
    assert_eq!(drained(&mut host), Err(Why::Stopped(Stopped::Refused)));
    let (mut host, script, _) = started();
    {
        let mut script = script.borrow_mut();
        script.closed = true;
        script.status = Some(tairix_abi::FAULT_EXIT_STATUS);
    }
    assert_eq!(drained(&mut host), Err(Why::Worker(WorkerEnd::Crashed)));
}

#[test]
fn commands_with_no_room_in_the_queue_wait_and_go_in_order() {
    let (mut host, script, _) = launched();
    let reply = vec![7; crate::protocol::RELAY_MOST];
    for serial in 0..3 {
        tell(
            &script,
            &Event::Relay {
                serial,
                context: 1,
                parked: serial == 0,
                side: tairix_riscos_kernel::relay::Side::Files,
                request: b"",
            },
        );
        host.on_readable();
        let _ = host.told().expect("believed");
        assert!(host.answer(serial, &reply));
    }
    host.run(true);
    let mut sent = Vec::new();
    for _ in 0..8 {
        sent = written(&mut host, &script);
    }
    let order: Vec<_> = decoded(&sent)
        .into_iter()
        .filter_map(|command| match command {
            Command::Answer { serial, .. } => Some(serial),
            Command::Run => Some(u32::MAX),
            _ => None,
        })
        .collect();
    assert_eq!(order, [0, 1, 2, u32::MAX]);
}
