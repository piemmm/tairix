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

use tairix_abi::font_ipc::FamilyKey;
use tairix_fuzzseed::Prng;
use tairix_reclaim::PressureBand;
use tairix_riscos_kernel::desktop::DesktopFont;
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::memory::Shared;
use tairix_riscos_kernel::relay::Side;
use tairix_riscos_session::credit::{Credit, Inbox, INPUT_WINDOW};
use tairix_riscos_session::front::Front;
use tairix_riscos_session::protocol::{
    Begin, Command, Cursor, Ending, Error, Event, Flash, Screen, Shape, Zoned, BOOT,
    BOOT_FAILURES_MOST, LINE_MOST, MEMO_MOST,
};
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

/// A sound pressure band.
fn band(rng: &mut Prng) -> PressureBand {
    PressureBand::from_depth(rng.next_u8() % 5)
}

/// A sound cursor flash.
fn flash(rng: &mut Prng) -> Flash {
    [Flash::Steady, Flash::Fast, Flash::Slow][rng.below(3)]
}

/// A sound desktop font: the shipped family or the fixed-pitch one, sized or
/// not.
fn font(rng: &mut Prng) -> DesktopFont {
    DesktopFont {
        family: if rng.below(2) == 0 {
            DesktopFont::SHIPPED.family
        } else {
            FamilyKey::MONO
        },
        size: (rng.below(2) == 0).then(|| 1 + rng.next_u16() % 0xFFFF),
    }
}

/// A sound zone name: one to three components of the letters a zone name
/// takes.
fn zone_name(rng: &mut Prng) -> String {
    let parts = 1 + rng.below(3);
    let part = |rng: &mut Prng| -> String {
        (0..=rng.below(8))
            .map(|_| char::from(b'A' + rng.next_u8() % 26))
            .collect()
    };
    (0..parts).map(|_| part(rng)).collect::<Vec<_>>().join("/")
}

/// A sound command's bytes.
fn command(rng: &mut Prng) -> Vec<u8> {
    let bytes = noise(rng, 64);
    let line: Vec<u8> = (0..rng.at_most(LINE_MOST))
        .map(|_| b' ' + rng.next_u8() % 95)
        .collect();
    let name = zone_name(rng);
    let command = match rng.below(13) {
        8 => Command::Begin(Begin {
            ram_pages: rng.next_u32() % (1 << 20),
            memo_pages: 1 + rng.next_u32() % MEMO_MOST,
            slot_bytes: (1 + rng.next_u32() % 0x2000) * 0x1000,
            real_time: rng.next_u64(),
            zone: (rng.below(2) == 0).then(|| Zoned {
                store: &bytes,
                name: &name,
            }),
            keyboard: rng.next_u32() % 0x80,
            font: font(rng),
            pressure: band(rng),
        }),
        9 => Command::Boot {
            serial: rng.next_u32(),
        },
        10 => Command::Pause {
            context: rng.next_u32(),
            paused: rng.below(2) == 0,
        },
        11 => Command::Pressure(band(rng)),
        12 => Command::Font(font(rng)),
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
/// What a sound event may borrow: a relay's bytes, an error's text, a boot
/// failure's line and a pointer's pixels.
struct Bodies {
    bytes: Vec<u8>,
    text: Vec<u8>,
    line: Vec<u8>,
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

impl Bodies {
    fn drawn(rng: &mut Prng) -> Self {
        let bytes = noise(rng, 64);
        let text = (0..rng.at_most(64))
            .map(|_| 1 + rng.next_u8() % 255)
            .collect();
        let (width, height) = (1 + rng.next_u32() % 32, 1 + rng.next_u32() % 32);
        let pixels = (0..width * height).map(|_| rng.next_u8() % 4).collect();
        let line = noise(rng, LINE_MOST);
        Self {
            bytes,
            text,
            line,
            pixels,
            width,
            height,
        }
    }
}

fn event(rng: &mut Prng) -> Vec<u8> {
    let bodies = Bodies::drawn(rng);
    let event = if rng.below(2) == 0 {
        shown_event(rng, &bodies)
    } else {
        told_event(rng, &bodies)
    };
    let mut w = Writer::new();
    event.encode(&mut w);
    w.finish()
}

/// A sound event of a screen's.
fn shown_event<'a>(rng: &mut Prng, bodies: &'a Bodies) -> Event<'a> {
    match rng.below(7) {
        0 => Event::Cursor {
            context: rng.next_u32(),
            cursor: (rng.below(2) == 0).then(|| {
                let top = rng.next_u32() % 16;
                Cursor {
                    x: rng.next_u32() % 4096,
                    y: rng.next_u32() % 4096,
                    width: 1 + rng.next_u32() % 64,
                    top,
                    bottom: top + rng.next_u32() % 16,
                    flash: flash(rng),
                }
            }),
        },
        1 => Event::Pointer {
            context: rng.next_u32(),
            shape: (rng.below(2) == 0).then(|| Shape {
                width: bodies.width,
                height: bodies.height,
                active: (rng.next_u8(), rng.next_u8()),
                colours: [0; 3].map(|_| rng.next_u32() & 0xFF_FFFF),
                pixels: &bodies.pixels,
            }),
        },
        2 => Event::PointerAt {
            context: rng.next_u32(),
            x: rng.next_u32().cast_signed(),
            y: rng.next_u32().cast_signed(),
        },
        3 => Event::PointerSet {
            context: rng.next_u32(),
        },
        4 => Event::Screen {
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
        5 => Event::Hourglass {
            context: rng.next_u32(),
            shown: rng.below(2) == 0,
        },
        _ => Event::Damage {
            context: rng.next_u32(),
            first: rng.next_u32() % 4096,
            lines: 1 + rng.next_u32() % 4096,
            offset: rng.next_u32(),
        },
    }
}

/// A sound event of anything but a screen's.
fn told_event<'a>(rng: &mut Prng, bodies: &'a Bodies) -> Event<'a> {
    let (bytes, text, line) = (&bodies.bytes, &bodies.text, &bodies.line);
    match rng.below(9) {
        0 => Event::Own {
            context: rng.next_u32(),
            slot: rng.next_u32(),
            screen: rng.next_u32(),
        },
        1 => Event::Shared(Shared {
            system_heap: rng.next_u32(),
            rma: rng.next_u32(),
            sprites: rng.next_u32(),
            font_cache: rng.next_u32(),
            ram_disc: rng.next_u32(),
            made: rng.next_u32(),
            free: rng.next_u32(),
        }),
        2 => Event::BootFailed {
            context: rng.next_u32(),
            line,
            error: Error {
                number: rng.next_u32(),
                text,
            },
        },
        3 => Event::BootOmitted {
            context: rng.next_u32(),
            count: rng.next_u32(),
        },
        4 => Event::Credit {
            context: rng.next_u32(),
            applied: rng.next_u32(),
        },
        5 => Event::Launched {
            serial: rng.next_u32(),
            context: rng.next_u32(),
        },
        6 => Event::Refused {
            serial: rng.next_u32(),
            error: (rng.below(2) == 0).then(|| Error {
                number: rng.next_u32(),
                text,
            }),
        },
        7 => Event::Relay {
            serial: rng.next_u32(),
            context: rng.next_u32(),
            parked: rng.below(2) == 0,
            side: if rng.below(2) == 0 {
                Side::Files
            } else {
                Side::Fonts
            },
            request: bytes,
        },
        _ => Event::Ended {
            context: rng.next_u32(),
            ending: match rng.below(4) {
                0 => Ending::Exit {
                    code: rng.next_u32(),
                },
                1 => Ending::Error(Error {
                    number: rng.next_u32(),
                    text,
                }),
                2 => Ending::Reset {
                    hard: rng.below(2) == 0,
                },
                _ => Ending::Quit,
            },
        },
    }
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
    /// For the boot's, the failures told and whether the rest were counted.
    boot: Option<(usize, bool)>,
}

/// A pointer two pixels square, each of its colours once.
const POINTER: [u8; 4] = [0, 1, 2, 3];

/// The harness's own record of an honest session, from which it makes the
/// events a worker could have sent.
#[derive(Default)]
struct Honest {
    asked: Vec<u32>,
    /// The boot's launch, once asked.
    boot: Option<u32>,
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
    /// A launch asked answered: refused, or made.
    fn answer_launch(&mut self, rng: &mut Prng) -> Event<'static> {
        let serial = self.asked.swap_remove(rng.below(self.asked.len()));
        if rng.below(4) == 0 {
            return Event::Refused {
                serial,
                error: None,
            };
        }
        self.made += 1;
        self.running.push(Made {
            context: self.made,
            screen: None,
            sent: 0,
            applied: 0,
            boot: (self.boot == Some(serial)).then_some((0, false)),
        });
        Event::Launched {
            serial,
            context: self.made,
        }
    }

    /// A cursor within running context `at`'s screen, where it has one.
    fn cursor(&self, at: usize, rng: &mut Prng) -> Option<Event<'static>> {
        let screen = self.running[at].screen?;
        let width = 1 + rng.next_u32() % screen.width;
        let x = rng.next_u32() % (screen.width - width + 1);
        let bottom = rng.next_u32() % screen.height;
        let y = rng.next_u32() % (screen.height - bottom);
        Some(Event::Cursor {
            context: self.running[at].context,
            cursor: Some(Cursor {
                x,
                y,
                width,
                top: rng.next_u32() % (bottom + 1),
                bottom,
                flash: flash(rng),
            }),
        })
    }

    /// A pointer's shape or place over running context `at`'s screen, or
    /// its hourglass, where it has one.
    fn pointer(&self, at: usize, rng: &mut Prng) -> Option<Event<'static>> {
        self.running[at].screen?;
        let context = self.running[at].context;
        Some(match rng.below(3) {
            0 => Event::Pointer {
                context,
                shape: Some(Shape {
                    width: 2,
                    height: 2,
                    active: (rng.next_u8(), rng.next_u8()),
                    colours: [0xFF_FFFF, 0, 0x80_8080],
                    pixels: &POINTER,
                }),
            },
            1 => Event::PointerAt {
                context,
                x: rng.next_u32().cast_signed(),
                y: rng.next_u32().cast_signed(),
            },
            _ => Event::Hourglass {
                context,
                shown: rng.below(2) == 0,
            },
        })
    }

    /// A boot failure of running context `at`'s, where it runs the boot and
    /// its report may hold one more.
    fn boot_report(&mut self, at: usize, rng: &mut Prng) -> Option<Event<'static>> {
        let context = self.running[at].context;
        let (told, omitted) = self.running[at].boot.as_mut()?;
        if *omitted {
            return None;
        }
        if *told < BOOT_FAILURES_MOST && rng.below(4) != 0 {
            *told += 1;
            return Some(Event::BootFailed {
                context,
                line: b"Run Boot:Choices.Boot.PreDesk.Thing",
                error: Error {
                    number: 0xD6,
                    text: b"Not found",
                },
            });
        }
        *omitted = true;
        Some(Event::BootOmitted {
            context,
            count: rng.next_u32(),
        })
    }

    fn event(&mut self, rng: &mut Prng) -> Option<Event<'static>> {
        if !self.asked.is_empty() && rng.below(3) == 0 {
            return Some(self.answer_launch(rng));
        }
        if self.running.is_empty() {
            return self.relay(BOOT, false);
        }
        let at = rng.below(self.running.len());
        let context = self.running[at].context;
        match rng.below(10) {
            6 => self.cursor(at, rng),
            7 => self.pointer(at, rng),
            8 => Some(match rng.below(3) {
                0 => Event::PointerSet { context },
                1 => Event::Own {
                    context,
                    slot: rng.next_u32(),
                    screen: rng.next_u32(),
                },
                _ => Event::Shared(Shared::default()),
            }),
            9 => self.boot_report(at, rng),
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
        4 if honest.boot.is_none() => {
            let serial = main.boot();
            assert_eq!(twin.boot(), serial);
            assert_eq!(main.boot(), None, "one boot a session");
            assert_eq!(twin.boot(), None);
            honest.asked.extend(serial);
            honest.boot = serial;
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
