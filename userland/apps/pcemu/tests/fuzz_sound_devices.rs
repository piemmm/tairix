//! Deterministic fuzz harness for the sound devices a guest program drives
//! through their ports with any values at any times: the Sound Blaster's DSP
//! over DMA, the YM3812, and the parallel port's DACs. For any traffic:
//!
//! 1. Nothing panics, and advancing time always ends.
//! 2. Advancing leaves the DSP nothing due at or before then.
//! 3. The DSP holds at most one command's replies for the host.
//! 4. The mix stays finite, and the stream it renders bounded.
//!
//! Each run starts with a reset and an 8-bit block over DMA to its
//! interrupt, so the transfer path is reached by construction, then drives
//! the devices with hostile traffic. Runs the fixed smoke sweep under plain
//! `cargo test`; keeps drawing from the same seeded stream until
//! `TAIRIX_FUZZ_BUDGET_SECS` elapses under `cargo xtask fuzz`.

mod support;

use support::Flat;
use tairix_fuzzseed::Prng;
use tairix_pcemu::chips::dma::{channel_setup, Dma};
use tairix_pcemu::ports::parallel::{Device, Parallel};
use tairix_pcemu::sound::blaster::{Dsp, DMA_CHANNEL};
use tairix_pcemu::sound::opl2::Opl2;
use tairix_pcemu::sound::{Mixer, Source, TICKS_PER_SAMPLE};
use tairix_pcemu::time::{Hertz, Nanos};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set):
/// each run renders seconds of the mix.
const SMOKE_ITERATIONS: u64 = 10;
/// Port operations a run makes after its prologue.
const OPERATIONS: usize = 400;
/// The most events a run follows when it waits for the DSP: an
/// auto-initialised transfer never runs out of them.
const EVENTS: usize = 2_000;
const US: Nanos = 1_000;
/// Where the DMA buffer lies.
const BUFFER: u32 = 0x1_0000;

struct Machine {
    dsp: Dsp,
    opl: Opl2,
    parallel: Parallel,
    dma: Dma,
    memory: Flat,
    mixer: Mixer,
    /// The stream's frames, drained as a front-end drains them.
    heard: Vec<f32>,
    now: Nanos,
    interrupts: u64,
}

impl Machine {
    fn new(rng: &mut Prng) -> Self {
        let device = *rng.pick(&[Device::Nothing, Device::Covox, Device::SoundSource]);
        Self {
            dsp: Dsp::new(),
            opl: Opl2::new(),
            parallel: Parallel::new(device),
            dma: Dma::new(),
            memory: Flat(rng.noise(1 << 20)),
            mixer: Mixer::new(*rng.pick(&[44_100, 48_000, 22_050])).unwrap(),
            heard: vec![0.0; 2 * 4_096],
            now: 0,
            interrupts: 0,
        }
    }

    /// Moves time to `t`, bringing every device to it, and checks what the
    /// devices leave.
    fn wait_until(&mut self, t: Nanos) {
        self.now = self.now.max(t);
        self.dsp
            .advance(self.now, &mut self.dma, &mut self.memory, &mut self.mixer);
        assert!(
            self.dsp.next_event().is_none_or(|at| at > self.now),
            "the DSP left work due"
        );
        if self.dsp.irq() {
            self.interrupts += 1;
            self.dsp.input(0x0E);
        }
        let until = Hertz::CRYSTAL.ticks_at(self.now) / TICKS_PER_SAMPLE + 1;
        let mixer = &mut self.mixer;
        self.opl.run_to(until, &mut |n, s| {
            mixer.sample(Source::Fm, n, f32::from(s) / 32_768.0);
        });
        self.parallel.advance(self.now, &mut self.mixer);
        self.mixer.render(Hertz::CRYSTAL.ticks_at(self.now));
        loop {
            let frames = self.mixer.take(&mut self.heard);
            assert!(
                self.heard[..2 * frames]
                    .iter()
                    .all(|x| x.is_finite() && x.abs() < 64.0),
                "the mix stays finite"
            );
            if frames == 0 {
                break;
            }
        }
        assert!(self.mixer.buffered() <= 48_000);
    }

    /// Follows the DSP's events until it has none, or the run's bound.
    fn settle(&mut self) {
        for _ in 0..EVENTS {
            match self.dsp.next_event() {
                Some(at) => self.wait_until(at),
                None => return,
            }
        }
    }

    /// Reads whatever the DSP has for the host, as a driver would; it
    /// never holds more than a command's replies.
    fn collect(&mut self) {
        let mut read = 0;
        while self.dsp.input(0x0E) & 0x80 != 0 {
            self.dsp.input(0x0A);
            read += 1;
            assert!(read <= 3, "the DSP held more than a command's replies");
        }
    }

    fn write(&mut self, byte: u8) {
        self.dsp.output(0x0C, byte, self.now);
        let t = self.now + 10 * US;
        self.wait_until(t);
    }

    /// The DSP's channel set to move `count` + 1 bytes in `mode` at the
    /// buffer.
    fn program(&mut self, mode: u8, count: u16) {
        for (port, value) in channel_setup(DMA_CHANNEL, mode, BUFFER, count) {
            self.dma.write_port(port, value);
        }
        let t = self.now;
        self.wait_until(t);
    }

    /// A reset, then an 8-bit block over DMA to its interrupt.
    fn prologue(&mut self) {
        self.dsp.output(0x06, 1, self.now);
        self.wait_until(5 * US);
        self.dsp.output(0x06, 0, self.now);
        self.wait_until(200 * US);
        self.collect();
        self.program(0x48, 63);
        for byte in [0x40, 0xD3, 0x14, 63, 0] {
            self.write(byte);
        }
        let t = self.now + 4_000 * US;
        self.wait_until(t);
    }
}

/// A command for the DSP: one the firmware knows with random parameters,
/// or any byte at all.
fn dsp_command(rng: &mut Prng) -> Vec<u8> {
    let command = *rng.pick(&[
        0x04, 0x10, 0x14, 0x16, 0x17, 0x1C, 0x1F, 0x20, 0x24, 0x2C, 0x30, 0x34, 0x38, 0x40, 0x48,
        0x58, 0x59, 0x74, 0x75, 0x76, 0x77, 0x7D, 0x7F, 0x80, 0x90, 0x91, 0x98, 0xD0, 0xD1, 0xD3,
        0xD4, 0xD8, 0xDA, 0xE0, 0xE1, 0xE2, 0xE4, 0xE8, 0xF0, 0xF1, 0xF2, 0xF4, 0xF8,
    ]);
    let first = if rng.below(5) == 0 {
        rng.next_u8()
    } else {
        command
    };
    let mut bytes = vec![first];
    let parameters = rng.below(5);
    bytes.extend((0..parameters).map(|_| rng.edge_biased_u8()));
    bytes
}

fn run(rng: &mut Prng) -> u64 {
    let mut machine = Machine::new(rng);
    machine.prologue();
    for _ in 0..OPERATIONS {
        match rng.below(11) {
            0 | 1 => {
                for byte in dsp_command(rng) {
                    machine.write(byte);
                }
            }
            2 => {
                let offset = *rng.pick(&[0x06, 0x0A, 0x0C, 0x0E, 0x00, 0x08]);
                machine
                    .dsp
                    .output(offset, rng.edge_biased_u8(), machine.now);
            }
            3 => {
                let offset = *rng.pick(&[0x0A, 0x0C, 0x0E, 0x04]);
                machine.dsp.input(offset);
            }
            4 => {
                let mode = *rng.pick(&[0x44, 0x48, 0x54, 0x58]);
                let count = u16::try_from(rng.at_most(4_096)).unwrap();
                machine.program(mode, count);
            }
            5 => {
                for _ in 0..rng.below(8) {
                    machine.opl.select(rng.next_u8());
                    machine.opl.write(rng.edge_biased_u8());
                }
            }
            6 => {
                let offset = u16::try_from(rng.below(3)).unwrap();
                let value = rng.edge_biased_u8();
                let now = machine.now;
                machine
                    .parallel
                    .output(offset, value, now, &mut machine.mixer);
                machine.parallel.input(1, now, &mut machine.mixer);
            }
            7 => machine.collect(),
            8 => {
                let t = machine.now + u64::try_from(rng.at_most(2_000)).unwrap() * 10 * US;
                machine.wait_until(t);
            }
            9 => {
                let status = machine.opl.status();
                assert_eq!(status & 0x1F, 0x06, "the OPL2's own low bits");
            }
            _ => machine.settle(),
        }
    }
    machine.settle();
    machine.interrupts
}

#[test]
fn hostile_sound_traffic_never_panics_hangs_or_runs_away() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_sound_traffic_never_panics_hangs_or_runs_away",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut interrupts = 0;
    let mut runs = 0;
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            interrupts += run(&mut rng);
            runs += 1;
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
    assert!(
        interrupts >= runs,
        "every run's prologue reached its block's interrupt"
    );
}
