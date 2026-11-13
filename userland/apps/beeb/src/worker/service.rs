//! The worker's side: the machine a session builds and runs, its picture's
//! changed rows written into the share and its sound carried to the owner at
//! the output rate.

use alloc::vec::Vec;

use tairix_abi::driver::audio::SampleFormat;
use tairix_audio::{convert, AcCoupling, FilterBank, Ratio, Resampler};
use tairix_sandbox::session::ShareWrite;
use tairix_sandbox::{FrameOut, SessionService, SessionStep};

use super::protocol::{
    Change, Configuration, Input, Lights, Loaded, Media, Ran, Refusal, Reply, Request, Slot,
    Written, MAX_BULK, MAX_SAMPLES, ROW_MAP, SAMPLE_BYTES, SAMPLE_RATE, SHARE_BYTES, SLOTS,
};
use crate::machine::fdc::PhysicalDrive;
use crate::machine::sound::UPDATE_RATE;
use crate::machine::video::{CANVAS_HEIGHT, CANVAS_WIDTH};
use crate::machine::{Machine, RomError};
use crate::media::disc::{Disc, Encoded, ImageFormat, LAST_TRACK};
use crate::media::tape::Tape;
use crate::snapshot::SnapshotError;

/// The AC coupling's pole: a 10 Hz corner at the output rate.
const COUPLING: f32 = 0.998_691_9;
/// Output frames the resampler fills at a time.
const CHUNK: usize = 512;

/// What a slot was built with, kept to build the machine again at power-on.
enum Kept {
    Empty,
    Rom(Vec<u8>),
    Ram,
}

/// The images a machine was built from.
struct Built {
    os: Vec<u8>,
    slots: [Kept; SLOTS],
    links: u8,
}

impl Built {
    fn of(configuration: &Configuration<'_>) -> Result<Self, Refusal> {
        let mut built = Self {
            os: owned(configuration.os)?,
            slots: core::array::from_fn(|_| Kept::Empty),
            links: configuration.links,
        };
        for (kept, slot) in built.slots.iter_mut().zip(&configuration.slots) {
            *kept = match slot {
                Slot::Empty => Kept::Empty,
                Slot::Rom(image) => Kept::Rom(owned(image)?),
                Slot::Ram => Kept::Ram,
            };
        }
        Ok(built)
    }
}

/// The machine a session serves, writing its picture into `share`.
pub struct MachineService<W: ShareWrite> {
    share: W,
    machine: Option<Machine>,
    built: Option<Built>,
    sound: Option<Sound>,
    sounding: bool,
    /// Where the last slice ended as asked: slices tile this timeline, so
    /// the instruction each overruns by never accumulates.
    timeline: u64,
    /// Whether the next slice writes every row: the owner holds no picture
    /// of a machine just built or restored.
    whole_picture: bool,
}

impl<W: ShareWrite> MachineService<W> {
    /// A service with no machine yet, writing into `share`; `None` for a
    /// share too small for the picture.
    pub fn new(share: W) -> Option<Self> {
        (share.byte_len() >= SHARE_BYTES).then(|| Self {
            share,
            machine: None,
            built: None,
            sound: Sound::new(),
            sounding: false,
            timeline: 0,
            whole_picture: true,
        })
    }

    fn answer(&mut self, request: &Request<'_>, out: &mut dyn FrameOut) -> bool {
        let answer = match request {
            Request::Run {
                cycles,
                sound,
                inputs,
            } => return self.run(*cycles, *sound, inputs, out),
            Request::TakeSnapshot => return self.snapshot(out),
            Request::Configure(configuration) => self.configure(configuration).map(|()| None),
            Request::PowerOn => self.power_on().map(|()| None),
            Request::RestoreSnapshot(snapshot) => self.restore(snapshot).map(Some),
            Request::RewindTape => self.on_machine(|machine| {
                machine.wind_tape(0);
                Ok(None)
            }),
            Request::RecordTape(on) => self.on_machine(|machine| {
                machine.set_tape_record(*on);
                Ok(None)
            }),
            Request::InsertDisc {
                drive,
                format,
                write_protected,
                image,
            } => self.on_machine(|machine| {
                let disc =
                    Disc::decode(image, *format, *write_protected).map_err(|_| Refusal::Disc)?;
                let mut taken = Taken::default();
                taken.disc(*drive, machine.eject_disc(*drive).as_ref());
                machine.insert_disc(*drive, disc);
                Ok(Some(taken))
            }),
            Request::EjectDisc(drive) => self.on_machine(|machine| {
                let mut taken = Taken::default();
                taken.disc(*drive, machine.eject_disc(*drive).as_ref());
                Ok(Some(taken))
            }),
            Request::InsertTape(image) => self.on_machine(|machine| {
                let tape = Tape::decode(image).map_err(|_| Refusal::Tape)?;
                let mut taken = Taken::default();
                taken.tape(machine)?;
                machine.insert_tape(tape);
                Ok(Some(taken))
            }),
            Request::EjectTape => self.on_machine(|machine| {
                let mut taken = Taken::default();
                taken.tape(machine)?;
                machine.eject_tape();
                Ok(Some(taken))
            }),
            Request::CollectDiscs(drives) => self.on_machine(|machine| {
                let mut taken = Taken::default();
                for drive in PhysicalDrive::ALL
                    .into_iter()
                    .filter(|drive| drives[drive.index()])
                {
                    taken.disc(drive, machine.disc(drive));
                    machine.mark_disc_saved(drive);
                }
                Ok(Some(taken))
            }),
        };
        match answer {
            Ok(None) => send(out, &Reply::Done),
            Ok(Some(taken)) => send(out, &Reply::Media(taken.media())),
            Err(refusal) => send(out, &Reply::Refused(refusal)),
        }
    }

    /// `act` done to the machine, refused while there is none.
    fn on_machine<T>(
        &mut self,
        act: impl FnOnce(&mut Machine) -> Result<T, Refusal>,
    ) -> Result<T, Refusal> {
        act(self.machine.as_mut().ok_or(Refusal::Unconfigured)?)
    }

    fn configure(&mut self, configuration: &Configuration<'_>) -> Result<(), Refusal> {
        let built = Built::of(configuration)?;
        self.start(&built)?;
        self.built = Some(built);
        Ok(())
    }

    fn power_on(&mut self) -> Result<(), Refusal> {
        let built = self.built.take().ok_or(Refusal::Unconfigured)?;
        let started = self.start(&built);
        self.built = Some(built);
        started
    }

    /// The machine built from `built` at power-on, the media of the one it
    /// replaces moved across.
    fn start(&mut self, built: &Built) -> Result<(), Refusal> {
        let mut machine = build(built).map_err(|_| Refusal::Rom)?;
        if let Some(old) = self.machine.as_mut() {
            for drive in PhysicalDrive::ALL {
                if let Some(disc) = old.eject_disc(drive) {
                    machine.insert_disc(drive, disc);
                }
            }
            if let Some(tape) = old.eject_tape() {
                machine.insert_tape(tape);
            }
        }
        machine.set_sound_output(self.sounding);
        self.timeline = machine.cycles();
        self.machine = Some(machine);
        self.whole_picture = true;
        Ok(())
    }

    /// `snapshot` restored, the media it replaces taken out; refused, nothing
    /// changed, for one that does not restore or a tape too large to carry.
    fn restore(&mut self, snapshot: &[u8]) -> Result<Taken, Refusal> {
        let machine = self.machine.as_mut().ok_or(Refusal::Unconfigured)?;
        let mut taken = Taken::default();
        for drive in PhysicalDrive::ALL {
            taken.disc(drive, machine.disc(drive));
        }
        taken.tape(machine)?;
        machine.restore(snapshot).map_err(|_| Refusal::Snapshot)?;
        self.timeline = machine.cycles();
        self.whole_picture = true;
        Ok(taken)
    }

    fn run(&mut self, cycles: u32, sound: bool, inputs: &[Input], out: &mut dyn FrameOut) -> bool {
        let Some(machine) = self.machine.as_mut() else {
            return send(out, &Reply::Refused(Refusal::Unconfigured));
        };
        if sound != self.sounding {
            machine.set_sound_output(sound);
            self.sounding = sound;
        }
        let start = self.timeline;
        for input in inputs {
            machine.run_until(start + u64::from(input.at));
            apply(machine, input.change);
        }
        self.timeline = start + u64::from(cycles);
        machine.run_until(self.timeline);
        let rows = picture(&mut self.share, machine, &mut self.whole_picture);
        let (caps_lock, shift_lock) = machine.lock_leds();
        let lights = Lights {
            caps_lock,
            shift_lock,
            motor: machine.tape_motor(),
        };
        let drives = PhysicalDrive::ALL.map(|drive| machine.drive_active(drive));
        let loaded = Loaded {
            discs: PhysicalDrive::ALL.map(|drive| machine.disc(drive).is_some()),
            tape: machine.holds_tape(),
            recording: machine.tape_record().0,
        };
        let sound: &[u8] = match (&mut self.sound, sound) {
            (Some(pipeline), true) => pipeline.take(machine),
            _ => &[],
        };
        send(
            out,
            &Reply::Ran(Ran {
                cycles,
                rows,
                sound,
                lights,
                drives,
                loaded,
            }),
        )
    }

    fn snapshot(&mut self, out: &mut dyn FrameOut) -> bool {
        let Some(machine) = self.machine.as_mut() else {
            return send(out, &Reply::Refused(Refusal::Unconfigured));
        };
        match machine.snapshot() {
            Ok(snapshot) if snapshot.len() <= MAX_BULK => send(out, &Reply::Snapshot(&snapshot)),
            Ok(_) | Err(SnapshotError::TooLarge) => send(out, &Reply::Refused(Refusal::TooLarge)),
            Err(_) => send(out, &Reply::Refused(Refusal::Memory)),
        }
    }
}

impl<W: ShareWrite> SessionService for MachineService<W> {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        let sent = match Request::decode(request) {
            Ok(request) => self.answer(&request, out),
            Err(_) => send(out, &Reply::Refused(Refusal::Malformed)),
        };
        if sent {
            SessionStep::Continue
        } else {
            SessionStep::Finished
        }
    }
}

fn send(out: &mut dyn FrameOut, reply: &Reply<'_>) -> bool {
    out.frame(&reply.encode()).is_ok()
}

/// The rows of `machine`'s picture that changed written into `share`, and
/// their map; every row when `whole` asks, which is then answered.
fn picture<W: ShareWrite>(share: &mut W, machine: &mut Machine, whole: &mut bool) -> [u8; ROW_MAP] {
    let mut map = [0; ROW_MAP];
    let dirty = machine.take_dirty_rows();
    let all = core::mem::replace(whole, false);
    let shown = machine.video().shown();
    let written = share.write_with(&mut |canvas| {
        for row in (0..CANVAS_HEIGHT).filter(|&row| all || dirty.contains(row)) {
            let span = row * CANVAS_WIDTH..(row + 1) * CANVAS_WIDTH;
            let (Some(to), Some(from)) = (canvas.get_mut(span.clone()), shown.get(span)) else {
                return false;
            };
            to.copy_from_slice(from);
            map[row / 8] |= 1 << (row % 8);
        }
        true
    });
    if written {
        map
    } else {
        *whole = true;
        [0; ROW_MAP]
    }
}

/// Media a request takes out of the machine, held for its answer.
#[derive(Default)]
struct Taken {
    discs: Vec<(PhysicalDrive, ImageFormat, Encoded)>,
    tape: Option<Vec<u8>>,
}

impl Taken {
    /// `disc`, from `drive`, if it was written since last collected.
    fn disc(&mut self, drive: PhysicalDrive, disc: Option<&Disc>) {
        if let Some(disc) = disc.filter(|disc| disc.is_dirty()) {
            self.discs.push((drive, disc.format(), disc.encode()));
        }
    }

    /// The machine's tape as it now is, if it was recorded on; refused when
    /// it is too large to carry.
    fn tape(&mut self, machine: &mut Machine) -> Result<(), Refusal> {
        if let Some(tape) = machine.tape().filter(|tape| tape.is_dirty()) {
            let image = tape.encode_uef();
            if image.len() > MAX_BULK {
                return Err(Refusal::TooLarge);
            }
            self.tape = Some(image);
        }
        Ok(())
    }

    fn media(&self) -> Media<'_> {
        Media {
            discs: self
                .discs
                .iter()
                .map(|(drive, format, encoded)| Written {
                    drive: *drive,
                    format: *format,
                    image: &encoded.image,
                    lost: encoded.lost.map(|(side, track)| {
                        (
                            u8::from(side != 0),
                            u8::try_from(track).map_or(LAST_TRACK, |track| track.min(LAST_TRACK)),
                        )
                    }),
                })
                .collect(),
            tape: self.tape.as_deref(),
        }
    }
}

fn owned(image: &[u8]) -> Result<Vec<u8>, Refusal> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(image.len())
        .map_err(|_| Refusal::Memory)?;
    copy.extend_from_slice(image);
    Ok(copy)
}

fn build(built: &Built) -> Result<Machine, RomError> {
    let mut machine = Machine::new(&built.os)?;
    for (slot, kept) in built.slots.iter().enumerate() {
        match kept {
            Kept::Empty => {}
            Kept::Rom(image) => machine.insert_rom(slot, image)?,
            Kept::Ram => machine.insert_ram(slot)?,
        }
    }
    machine.set_startup_options(built.links);
    machine.set_sound_output(false);
    Ok(machine)
}

fn apply(machine: &mut Machine, change: Change) {
    match change {
        Change::Key(key, down) => machine.set_key(key, down),
        Change::Break(down) => machine.set_break(down),
        Change::Analogue(channel, level) => machine.set_analogue(usize::from(channel), level),
        Change::Fire(buttons) => machine.set_fire_buttons(buttons),
    }
}

/// The sound's way to the owner: the chip's updates resampled to the output
/// rate, AC-coupled as the machine's output is, then signed 16-bit.
struct Sound {
    bank: FilterBank,
    resampler: Resampler,
    coupling: AcCoupling,
    output: Vec<f32>,
    bytes: Vec<u8>,
}

impl Sound {
    fn new() -> Option<Self> {
        let bank = FilterBank::for_ratio(Ratio::new(UPDATE_RATE, SAMPLE_RATE)?).ok()?;
        let resampler = Resampler::new(&bank, 1).ok()?;
        Some(Self {
            bank,
            resampler,
            coupling: AcCoupling::new(COUPLING),
            output: Vec::new(),
            bytes: Vec::new(),
        })
    }

    /// The machine's sound since last asked, encoded; nothing if a buffer
    /// could not grow, and at most [`MAX_SAMPLES`].
    fn take(&mut self, machine: &mut Machine) -> &[u8] {
        self.output.clear();
        self.bytes.clear();
        let audio = machine.take_audio();
        let updates = audio.as_slice();
        let mut frames = [0.0f32; CHUNK];
        let mut at = 0;
        while at < updates.len() && self.output.len() < MAX_SAMPLES {
            let Ok((consumed, produced)) =
                self.resampler
                    .process(&self.bank, &updates[at..], &mut frames)
            else {
                break;
            };
            if self.output.try_reserve(produced).is_err() {
                break;
            }
            let coupling = &mut self.coupling;
            self.output
                .extend(frames[..produced].iter().map(|&frame| coupling.step(frame)));
            if consumed == 0 {
                break;
            }
            at += consumed;
        }
        self.output.truncate(MAX_SAMPLES);
        let length = self.output.len() * SAMPLE_BYTES;
        if self.bytes.try_reserve(length).is_err() {
            return &[];
        }
        self.bytes.resize(length, 0);
        let written = convert::encode(SampleFormat::S16, &self.output, &mut self.bytes);
        &self.bytes[..written]
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
