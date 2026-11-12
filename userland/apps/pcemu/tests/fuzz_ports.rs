//! Deterministic fuzz harness for the ports a guest program drives with any
//! values at any times: two serial adapters with a mouse on one, a printer
//! on a parallel port, the game port and the clock card. For any traffic:
//!
//! 1. Nothing panics, and advancing time always ends, however far.
//! 2. Advancing a UART leaves nothing due at or before then.
//! 3. The printer gives back no more bytes than it was strobed.
//! 4. The clock card reads as a valid time or as none.
//!
//! Each run starts with the mouse's port set as the mouse is and its `M`
//! received, so the receive path is reached by construction, then drives
//! the ports with hostile traffic. Runs the fixed smoke sweep under plain
//! `cargo test`; keeps drawing from the same seeded stream until
//! `TAIRIX_FUZZ_BUDGET_SECS` elapses under `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_pcemu::ports::clock::ClockCard;
use tairix_pcemu::ports::game::{GamePort, Sticks};
use tairix_pcemu::ports::parallel::{control, reg, Device, Parallel};
use tairix_pcemu::ports::serial::{Model, Plug, Uart};
use tairix_pcemu::sound::Mixer;
use tairix_pcemu::time::Nanos;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 40;
/// Operations a run makes after its prologue.
const OPERATIONS: usize = 400;
/// The most events a run follows when it waits for the UARTs.
const EVENTS: usize = 2_000;
const MS: Nanos = 1_000_000;
const DAY: Nanos = 86_400_000 * MS;

struct Machine {
    uarts: [Uart; 2],
    printer: Parallel,
    game: GamePort,
    clock: ClockCard,
    mixer: Mixer,
    now: Nanos,
    /// Strobes the printer may have taken a byte on.
    strobes: usize,
    printed: usize,
}

impl Machine {
    fn new(rng: &mut Prng) -> Self {
        let models = [Model::Ins8250, Model::Ns16450];
        let first = Uart::new(*rng.pick(&models), Plug::Mouse);
        let second = Uart::new(*rng.pick(&models), Plug::Nothing);
        Self {
            uarts: [first, second],
            printer: Parallel::new(Device::Printer),
            game: GamePort::default(),
            clock: ClockCard::new(0),
            mixer: Mixer::new(48_000).unwrap(),
            now: 0,
            strobes: 0,
            printed: 0,
        }
    }

    /// Moves time to `t`, bringing the UARTs to it and checking what they
    /// leave.
    fn wait_until(&mut self, t: Nanos) {
        self.now = self.now.max(t);
        for uart in &mut self.uarts {
            uart.advance(self.now);
            assert!(
                uart.next_event().is_none_or(|at| at > self.now),
                "a UART left work due"
            );
        }
    }

    /// Follows the UARTs' events until neither has one, or the run's bound.
    fn settle(&mut self) {
        for _ in 0..EVENTS {
            let next = self.uarts.iter().filter_map(Uart::next_event).min();
            match next {
                Some(at) => self.wait_until(at),
                None => return,
            }
        }
    }

    /// Sets COM1 as the mouse sends, powers it and takes its `M`.
    fn prologue(&mut self) {
        let mouse = &mut self.uarts[0];
        for (offset, value) in [(3, 0x80), (0, 96), (1, 0), (3, 0x02), (1, 0x01), (4, 0x0B)] {
            mouse.output(offset, value, 0);
        }
        self.settle();
        assert!(self.uarts[0].irq(), "the M interrupted");
        assert_eq!(self.uarts[0].input(0, self.now), b'M');
    }

    fn print(&mut self, offset: u16, value: u8) {
        let now = self.now;
        let was = self.printer.input(reg::CONTROL, now, &mut self.mixer);
        self.printer.output(offset, value, now, &mut self.mixer);
        if offset == reg::CONTROL && was & control::STROBE == 0 && value & control::STROBE != 0 {
            self.strobes += 1;
        }
    }
}

fn run(rng: &mut Prng) {
    let mut machine = Machine::new(rng);
    machine.prologue();
    for _ in 0..OPERATIONS {
        let now = machine.now;
        match rng.below(12) {
            0 | 1 => {
                let uart = &mut machine.uarts[rng.below(2)];
                let offset = u16::try_from(rng.below(8)).unwrap();
                uart.output(offset, rng.edge_biased_u8(), now);
            }
            2 => {
                let uart = &mut machine.uarts[rng.below(2)];
                uart.input(u16::try_from(rng.below(8)).unwrap(), now);
            }
            3 => {
                let reach = *rng.pick(&[1, 127, 1_000, i32::MAX]);
                let dx = i32::try_from(rng.below(201)).unwrap() - 100;
                let dy = i32::try_from(rng.below(201)).unwrap() - 100;
                let buttons = rng.next_u8();
                machine.uarts[0].mouse(dx.saturating_mul(reach), dy, buttons, false, now);
            }
            4 => machine.settle(),
            5 => {
                let t = now + u64::try_from(rng.at_most(100)).unwrap() * MS;
                machine.wait_until(t);
            }
            6 => {
                let offset = u16::try_from(rng.below(3)).unwrap();
                machine.print(offset, rng.edge_biased_u8());
                machine.printer.input(reg::STATUS, now, &mut machine.mixer);
            }
            7 => {
                let mut out = vec![0; rng.at_most(70_000)];
                machine.printed += machine.printer.take_printed(&mut out);
                assert!(
                    machine.printed <= machine.strobes,
                    "printed more than strobed"
                );
            }
            8 => {
                let axis = |rng: &mut Prng| {
                    (rng.below(4) != 0).then(|| u16::try_from(rng.below(65_536)).unwrap())
                };
                let sticks = Sticks {
                    axes: [axis(rng), axis(rng), axis(rng), axis(rng)],
                    buttons: rng.next_u8(),
                };
                machine.game.set(sticks);
                machine.game.output(now);
                let later = now + u64::try_from(rng.at_most(2_000)).unwrap() * 1_000;
                assert_eq!(
                    machine.game.input(later) >> 4,
                    !sticks.buttons & 0x0F,
                    "buttons read low"
                );
            }
            9 => {
                let offset = u16::try_from(rng.below(16)).unwrap();
                machine.clock.output(offset, rng.edge_biased_u8(), now);
                machine
                    .clock
                    .input(u16::try_from(rng.below(16)).unwrap(), now);
            }
            10 => {
                // Years at once: the card catches up without counting each
                // second.
                let t = now + u64::try_from(rng.at_most(1_000)).unwrap() * DAY;
                machine.now = t;
                machine.clock.input(0x0D, t);
            }
            _ => {
                if let Some(time) = machine.clock.time(now) {
                    assert!(time.is_valid(), "{time:?}");
                }
            }
        }
    }
    machine.settle();
}

#[test]
fn hostile_port_traffic_never_panics_hangs_or_overflows() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_port_traffic_never_panics_hangs_or_overflows",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            run(&mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
