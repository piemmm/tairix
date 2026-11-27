//! Deterministic fuzz harness for the session protocol: both decoders fed
//! the bytes of sound messages changed and cut, and noise, by a side that
//! cannot trust the other; and the damage ring and credited input driven by
//! a hostile peer.
//!
//! The invariants:
//!
//! * nothing panics, and every refusal is a value;
//! * whatever decodes re-encodes to the very bytes it came from;
//! * a ring refuses every record where the worker could not have put it,
//!   and both sides of an honest exchange stay alike;
//! * the front end's model believes every event of an honest session, and
//!   a hostile event it refuses changes nothing;
//! * a credit never lets more than its window be outstanding.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz` extends it to a wall-clock
//! budget.

use tairix_fuzzseed::Prng;
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::relay::Side;
use tairix_riscos_session::credit::{Credit, Inbox, INPUT_WINDOW};
use tairix_riscos_session::front::Front;
use tairix_riscos_session::protocol::{Command, Ending, Error, Event, Screen, BOOT, LINE_MOST};
use tairix_riscos_session::ring::{Ring, RECORD_MOST, RING_BYTES};
use tairix_sandbox::wire::Writer;

const SMOKE_RUNS: u64 = 2000;

fn input(rng: &mut Prng) -> InputEvent {
    match rng.below(4) {
        0 => InputEvent::Key(rng.next_u8()),
        1 => InputEvent::Transition {
            key: rng.next_u32() % 0x300,
            down: rng.below(2) == 0,
        },
        2 => InputEvent::Pointer {
            x: rng.next_u32().cast_signed(),
            y: rng.next_u32().cast_signed(),
            buttons: rng.next_u32() & 7,
        },
        _ => InputEvent::Motion {
            dx: rng.next_u32().cast_signed(),
            dy: rng.next_u32().cast_signed(),
            buttons: rng.next_u32() & 7,
        },
    }
}

/// `rng`'s noise, of at most `most` bytes.
fn noise(rng: &mut Prng, most: usize) -> Vec<u8> {
    let length = rng.at_most(most);
    rng.noise(length)
}

/// A sound command's bytes.
fn command(rng: &mut Prng) -> Vec<u8> {
    let bytes = noise(rng, 64);
    let line: Vec<u8> = (0..rng.at_most(LINE_MOST))
        .map(|_| b' ' + rng.next_u8() % 95)
        .collect();
    let command = match rng.below(8) {
        0 => Command::Run,
        1 => Command::Hold,
        2 => Command::Input {
            context: rng.next_u32(),
            event: input(rng),
        },
        3 => Command::Released {
            bytes: rng.next_u32(),
        },
        4 => Command::Answer {
            serial: rng.next_u32(),
            reply: &bytes,
        },
        5 => Command::Unanswered {
            serial: rng.next_u32(),
        },
        6 => Command::Launch {
            serial: rng.next_u32(),
            line: &line,
        },
        _ => Command::Quit {
            context: rng.next_u32(),
        },
    };
    let mut w = Writer::new();
    command.encode(&mut w);
    w.finish()
}

/// A sound event's bytes.
fn event(rng: &mut Prng) -> Vec<u8> {
    let bytes = noise(rng, 64);
    let text: Vec<u8> = (0..rng.at_most(64))
        .map(|_| 1 + rng.next_u8() % 255)
        .collect();
    let event = match rng.below(7) {
        0 => Event::Credit {
            context: rng.next_u32(),
            applied: rng.next_u32(),
        },
        1 => Event::Launched {
            serial: rng.next_u32(),
            context: rng.next_u32(),
        },
        2 => Event::Refused {
            serial: rng.next_u32(),
            error: (rng.below(2) == 0).then(|| Error {
                number: rng.next_u32(),
                text: &text,
            }),
        },
        3 => Event::Screen {
            context: rng.next_u32(),
            screen: Screen {
                width: 1 + rng.next_u32() % 4096,
                height: 1 + rng.next_u32() % 4096,
                x_eig: rng.next_u32() % 4,
                y_eig: rng.next_u32() % 4,
                border: rng.next_u32() & 0xFF_FFFF,
                blanked: rng.below(2) == 0,
            },
        },
        4 => Event::Damage {
            context: rng.next_u32(),
            first: rng.next_u32() % 4096,
            lines: 1 + rng.next_u32() % 4096,
            offset: rng.next_u32(),
        },
        5 => Event::Relay {
            serial: rng.next_u32(),
            context: rng.next_u32(),
            parked: rng.below(2) == 0,
            side: if rng.below(2) == 0 {
                Side::Files
            } else {
                Side::Fonts
            },
            request: &bytes,
        },
        _ => Event::Ended {
            context: rng.next_u32(),
            ending: match rng.below(4) {
                0 => Ending::Exit {
                    code: rng.next_u32(),
                },
                1 => Ending::Error(Error {
                    number: rng.next_u32(),
                    text: &text,
                }),
                2 => Ending::Reset {
                    hard: rng.below(2) == 0,
                },
                _ => Ending::Quit,
            },
        },
    };
    let mut w = Writer::new();
    event.encode(&mut w);
    w.finish()
}

/// `pristine` changed, cut or replaced by noise.
fn hostile(rng: &mut Prng, pristine: &[u8]) -> Vec<u8> {
    match rng.below(4) {
        0 => pristine.to_vec(),
        1 => rng.mutated(pristine),
        2 => pristine[..rng.at_most(pristine.len())].to_vec(),
        _ => noise(rng, 32),
    }
}

fn reencodes_command(bytes: &[u8]) {
    if let Ok(command) = Command::decode(bytes) {
        let mut w = Writer::new();
        command.encode(&mut w);
        assert_eq!(w.finish(), bytes, "{command:?}");
    }
}

fn reencodes_event(bytes: &[u8]) {
    if let Ok(event) = Event::decode(bytes) {
        let mut w = Writer::new();
        event.encode(&mut w);
        assert_eq!(w.finish(), bytes, "{event:?}");
    }
}

#[test]
fn whatever_decodes_reencodes_to_its_own_bytes() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "whatever_decodes_reencodes_to_its_own_bytes",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let sound = command(&mut rng);
        assert!(Command::decode(&sound).is_ok(), "a sound command decodes");
        reencodes_command(&hostile(&mut rng, &sound));
        let sound = event(&mut rng);
        assert!(Event::decode(&sound).is_ok(), "a sound event decodes");
        reencodes_event(&hostile(&mut rng, &sound));
        runs += 1;
    }
}

#[test]
fn a_ring_believes_only_records_the_worker_could_have_put() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "a_ring_believes_only_records_the_worker_could_have_put",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS / 10 => break,
            _ => {}
        }
        let (mut worker, mut front) = (Ring::new(), Ring::new());
        let mut unreleased = std::collections::VecDeque::new();
        for _ in 0..200 {
            if rng.below(3) == 0 {
                let before = front;
                let offset = rng.next_u32() % (RING_BYTES + 1);
                let bytes = rng.next_u32() % (RECORD_MOST + 2);
                let honest = worker.place(bytes);
                match front.accept(offset, bytes) {
                    Ok(placed) => {
                        assert_eq!(Some(placed), honest, "only where it could go");
                        front = before;
                    }
                    Err(_) => assert_eq!(front, before, "nothing moved"),
                }
            }
            let bytes = 1 + rng.next_u32() % RECORD_MOST;
            if let Some(placed) = worker.place(bytes) {
                worker.take(placed);
                assert_eq!(front.accept(placed.offset, placed.bytes), Ok(placed));
                unreleased.push_back(placed.released());
            } else if let Some(oldest) = unreleased.pop_front() {
                front.release(oldest).expect("held");
                worker.release(oldest).expect("held");
            }
            assert_eq!(front, worker, "both sides alike");
            assert!(worker.held() <= RING_BYTES);
        }
        runs += 1;
    }
}

/// A context the honest worker made, as the harness keeps it.
struct Made {
    context: u32,
    screen: Option<Screen>,
    sent: u32,
    applied: u32,
}

/// The harness's own record of an honest session, from which it makes the
/// events a worker could have sent.
#[derive(Default)]
struct Honest {
    asked: Vec<u32>,
    made: u32,
    running: Vec<Made>,
    /// The relays unanswered: each one's serial, its context, and whether
    /// that context is parked on it.
    relays: Vec<(u32, u32, bool)>,
    next_relay: u32,
    ring: Ring,
    unreleased: std::collections::VecDeque<u32>,
}

/// A small screen, so that records are many and the ring wraps.
const SMALL: Screen = Screen {
    width: 64,
    height: 48,
    x_eig: 1,
    y_eig: 1,
    border: 0,
    blanked: false,
};

impl Honest {
    /// An event a worker could send now, its record moved on by it: none
    /// where there is nothing to make one of.
    fn event(&mut self, rng: &mut Prng) -> Option<Event<'static>> {
        if !self.asked.is_empty() && rng.below(3) == 0 {
            let serial = self.asked.swap_remove(rng.below(self.asked.len()));
            if rng.below(4) == 0 {
                return Some(Event::Refused {
                    serial,
                    error: None,
                });
            }
            self.made += 1;
            self.running.push(Made {
                context: self.made,
                screen: None,
                sent: 0,
                applied: 0,
            });
            return Some(Event::Launched {
                serial,
                context: self.made,
            });
        }
        if self.running.is_empty() {
            return self.relay(BOOT, false);
        }
        let at = rng.below(self.running.len());
        let context = self.running[at].context;
        match rng.below(6) {
            0 => {
                self.running[at].screen = Some(SMALL);
                Some(Event::Screen {
                    context,
                    screen: SMALL,
                })
            }
            1 => {
                let made = &mut self.running[at];
                let more = u32::try_from(rng.at_most((made.sent - made.applied) as usize)).ok()?;
                made.applied += more;
                Some(Event::Credit {
                    context,
                    applied: made.applied,
                })
            }
            2 | 3 => {
                let screen = self.running[at].screen?;
                let lines = 1 + rng.next_u32() % screen.height;
                let first = rng.next_u32() % (screen.height - lines + 1);
                let placed = self.ring.place(lines * screen.width * 4)?;
                self.ring.take(placed);
                self.unreleased.push_back(placed.released());
                Some(Event::Damage {
                    context,
                    first,
                    lines,
                    offset: placed.offset,
                })
            }
            4 => self.relay(context, rng.below(2) == 0),
            _ => {
                self.running.swap_remove(at);
                Some(Event::Ended {
                    context,
                    ending: Ending::Quit,
                })
            }
        }
    }

    /// A relay for `context`, parked on where `parked`: none where an honest
    /// worker could not make it now — a second one parked on for the
    /// context, or one waited for in place while another is.
    fn relay(&mut self, context: u32, parked: bool) -> Option<Event<'static>> {
        let blocked = self.relays.iter().any(|&(_, other, other_parked)| {
            other_parked == parked && (!parked || other == context)
        });
        if blocked {
            return None;
        }
        let serial = self.next_relay;
        self.next_relay += 1;
        self.relays.push((serial, context, parked));
        Some(Event::Relay {
            serial,
            context,
            parked,
            side: Side::Files,
            request: b"request",
        })
    }
}

/// One step of the front end's own, made on both models alike.
fn front_end_acts(rng: &mut Prng, honest: &mut Honest, fronts: [&mut Front; 2]) {
    let [main, twin] = fronts;
    match rng.below(5) {
        3 if !honest.running.is_empty() => {
            let context = honest.running[rng.below(honest.running.len())].context;
            assert_eq!(main.quit(context), twin.quit(context));
        }
        0 => {
            let line = if rng.below(8) == 0 {
                &b"Bad\rline"[..]
            } else {
                b"BASIC"
            };
            let serial = main.launch(line);
            assert_eq!(twin.launch(line), serial);
            honest.asked.extend(serial);
        }
        1 if !honest.running.is_empty() => {
            let at = rng.below(honest.running.len());
            let made = &mut honest.running[at];
            let sent = main.input(made.context);
            assert_eq!(twin.input(made.context), sent);
            made.sent += u32::from(sent);
        }
        2 if !honest.relays.is_empty() => {
            let (serial, ..) = honest.relays.swap_remove(rng.below(honest.relays.len()));
            assert!(main.answer(serial) && twin.answer(serial), "asked");
        }
        _ => {
            if let Some(oldest) = honest.unreleased.pop_front() {
                assert!(main.release(oldest) && twin.release(oldest), "held");
                honest.ring.release(oldest).expect("held");
            }
        }
    }
}

#[test]
fn the_front_believes_what_the_worker_could_have_sent_and_nothing_else() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_front_believes_what_the_worker_could_have_sent_and_nothing_else",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS / 10 => break,
            _ => {}
        }
        let (mut main, mut twin) = (Front::new().expect("room"), Front::new().expect("room"));
        let mut honest = Honest::default();
        // Until a hostile event happens to be believed, the record and the
        // models agree, and every honest event is believed.
        let mut trusting = true;
        for _ in 0..300 {
            match rng.below(4) {
                0 => front_end_acts(&mut rng, &mut honest, [&mut main, &mut twin]),
                1 if trusting => {
                    if let Some(event) = honest.event(&mut rng) {
                        let believed = main.believe(&event);
                        assert!(believed.is_ok(), "{event:?}: {believed:?}");
                        assert_eq!(twin.believe(&event), believed);
                    }
                }
                _ => {
                    let sound = event(&mut rng);
                    let bytes = hostile(&mut rng, &sound);
                    if let Ok(event) = Event::decode(&bytes) {
                        if let Ok(placed) = main.believe(&event) {
                            assert_eq!(twin.believe(&event), Ok(placed));
                            trusting = false;
                        }
                    }
                }
            }
            assert_eq!(main, twin, "a refused event changed nothing");
        }
        runs += 1;
    }
}

#[test]
fn no_more_than_a_window_is_ever_outstanding() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "no_more_than_a_window_is_ever_outstanding",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS / 10 => break,
            _ => {}
        }
        let (mut credit, mut inbox) = (Credit::new(), Inbox::new().expect("room"));
        for _ in 0..500 {
            match rng.below(3) {
                0 => {
                    if credit.send().is_ok() {
                        inbox.receive(input(&mut rng)).expect("within the window");
                    }
                }
                1 => {
                    inbox.take();
                    credit.taken(inbox.applied()).expect("as the inbox counts");
                }
                _ => {
                    let before = credit;
                    if credit.taken(rng.next_u32()).is_err() {
                        assert_eq!(credit, before, "nothing moved");
                    } else {
                        credit = before;
                    }
                }
            }
            assert!(credit.outstanding() <= INPUT_WINDOW);
        }
        runs += 1;
    }
}
