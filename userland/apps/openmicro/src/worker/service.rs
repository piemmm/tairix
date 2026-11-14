//! The worker's side: the machine a session builds and runs, its picture's
//! changed rows written into the share and its sound carried to the owner at
//! the output rate.

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_abi::driver::audio::SampleFormat;
use tairix_audio::{convert, AcCoupling, FilterBank, Ratio, Resampler};
use tairix_sandbox::session::ShareWrite;
use tairix_sandbox::{FrameOut, SessionService, SessionStep};

use super::protocol::{
    Change, Configuration, Input, Lights, Loaded, Media, ModelKey, Plus1, Ran, Refusal, Reply,
    Request, Slot, Written, MAX_BULK, MAX_SAMPLES, ROW_MAP, SAMPLE_BYTES, SAMPLE_RATE, SHARE_BYTES,
    SLOTS,
};
use crate::machine::bbc::fdc::PhysicalDrive;
use crate::machine::canvas::{CANVAS_HEIGHT, CANVAS_WIDTH};
use crate::machine::electron::{CARTRIDGE_ROMS, PLUS1_ROM};
use crate::machine::{bbc, electron, Machine, RomError, CYCLES_PER_SAMPLE, SOUND_RATE};
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
enum Built {
    ModelB {
        os: Vec<u8>,
        slots: Box<[Kept; SLOTS]>,
        links: u8,
    },
    Electron {
        os: Vec<u8>,
        basic: Vec<u8>,
        plus1: Option<BuiltPlus1>,
    },
}

/// The Plus 1 a machine was built with: its ROM and the cartridges' images.
struct BuiltPlus1 {
    rom: Vec<u8>,
    cartridges: [[Option<Vec<u8>>; 2]; 2],
}

impl Built {
    fn of(configuration: &Configuration<'_>) -> Result<Self, Refusal> {
        Ok(match configuration {
            Configuration::ModelB { os, slots, links } => {
                let mut kept: Box<[Kept; SLOTS]> = Box::new(core::array::from_fn(|_| Kept::Empty));
                for (kept, slot) in kept.iter_mut().zip(slots.iter()) {
                    *kept = match slot {
                        Slot::Empty => Kept::Empty,
                        Slot::Rom(image) => Kept::Rom(owned(image)?),
                        Slot::Ram => Kept::Ram,
                    };
                }
                Self::ModelB {
                    os: owned(os)?,
                    slots: kept,
                    links: *links,
                }
            }
            Configuration::Electron { os, basic, plus1 } => Self::Electron {
                os: owned(os)?,
                basic: owned(basic)?,
                plus1: plus1.map(BuiltPlus1::of).transpose()?,
            },
        })
    }
}

impl BuiltPlus1 {
    fn of(plus1: Plus1<'_>) -> Result<Self, Refusal> {
        let mut cartridges: [[Option<Vec<u8>>; 2]; 2] = Default::default();
        for (kept, image) in cartridges
            .iter_mut()
            .flatten()
            .zip(plus1.cartridges.iter().flatten())
        {
            *kept = image.map(owned).transpose()?;
        }
        Ok(Self {
            rom: owned(plus1.rom)?,
            cartridges,
        })
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
            } => self.on_drives(|machine| {
                let disc =
                    Disc::decode(image, *format, *write_protected).map_err(|_| Refusal::Disc)?;
                let mut taken = Taken::default();
                taken.disc(*drive, machine.eject_disc(*drive).as_ref());
                machine.insert_disc(*drive, disc);
                Ok(Some(taken))
            }),
            Request::EjectDisc(drive) => self.on_drives(|machine| {
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
            Request::CollectDiscs(drives) => self.on_drives(|machine| {
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

    /// `act` done to a machine with drives, refused while there is none and
    /// for a model without them.
    fn on_drives<T>(
        &mut self,
        act: impl FnOnce(&mut bbc::Machine) -> Result<T, Refusal>,
    ) -> Result<T, Refusal> {
        match self.machine.as_mut().ok_or(Refusal::Unconfigured)? {
            Machine::ModelB(machine) => act(machine),
            Machine::Electron(_) => Err(Refusal::Model),
        }
    }

    fn configure(&mut self, configuration: &Configuration<'_>) -> Result<(), Refusal> {
        if self
            .machine
            .as_ref()
            .is_some_and(|machine| machine.model() != configuration.model())
        {
            return Err(Refusal::Model);
        }
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
    /// replaces, always of the same model, moved across.
    fn start(&mut self, built: &Built) -> Result<(), Refusal> {
        let mut machine = build(built).map_err(|_| Refusal::Rom)?;
        if let Some(old) = self.machine.as_mut() {
            if let (Machine::ModelB(old), Machine::ModelB(new)) = (&mut *old, &mut machine) {
                for drive in PhysicalDrive::ALL {
                    if let Some(disc) = old.eject_disc(drive) {
                        new.insert_disc(drive, disc);
                    }
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
        if let Some(sound) = &mut self.sound {
            sound.forget();
        }
        Ok(())
    }

    /// `snapshot` restored, the media it replaces taken out; refused, nothing
    /// changed, for one that does not restore or a tape too large to carry.
    fn restore(&mut self, snapshot: &[u8]) -> Result<Taken, Refusal> {
        let machine = self.machine.as_mut().ok_or(Refusal::Unconfigured)?;
        let mut taken = Taken::default();
        if let Machine::ModelB(machine) = &*machine {
            for drive in PhysicalDrive::ALL {
                taken.disc(drive, machine.disc(drive));
            }
        }
        taken.tape(machine)?;
        machine.restore(snapshot).map_err(|_| Refusal::Snapshot)?;
        self.timeline = machine.cycles();
        self.whole_picture = true;
        if let Some(sound) = &mut self.sound {
            sound.forget();
        }
        Ok(taken)
    }

    fn run(&mut self, cycles: u32, sound: bool, inputs: &[Input], out: &mut dyn FrameOut) -> bool {
        let Some(machine) = self.machine.as_mut() else {
            return send(out, &Reply::Refused(Refusal::Unconfigured));
        };
        if !inputs.iter().all(|input| takes(machine, input.change)) {
            return send(out, &Reply::Refused(Refusal::Model));
        }
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
        let (caps_lock, shift_lock) = machine.lock_lights();
        let lights = Lights {
            caps_lock,
            shift_lock,
            motor: machine.tape_motor(),
        };
        let (drives, discs) = match &*machine {
            Machine::ModelB(machine) => (
                PhysicalDrive::ALL.map(|drive| machine.drive_active(drive)),
                PhysicalDrive::ALL.map(|drive| machine.disc(drive).is_some()),
            ),
            Machine::Electron(_) => ([false; 2], [false; 2]),
        };
        let loaded = Loaded {
            discs,
            tape: machine.holds_tape(),
            recording: machine.tape_record().0,
        };
        let overrun = (machine.cycles() / CYCLES_PER_SAMPLE)
            .saturating_sub(self.timeline / CYCLES_PER_SAMPLE);
        let sound: &[u8] = match (&mut self.sound, sound) {
            (Some(pipeline), true) => pipeline.take(machine, overrun),
            (Some(pipeline), false) => {
                pipeline.forget();
                &[]
            }
            (None, _) => &[],
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
    let shown = machine.shown();
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
    let mut machine = match built {
        Built::ModelB { os, slots, links } => {
            let mut machine = bbc::Machine::new(os)?;
            for (slot, kept) in slots.iter().enumerate() {
                match kept {
                    Kept::Empty => {}
                    Kept::Rom(image) => machine.insert_rom(slot, image)?,
                    Kept::Ram => machine.insert_ram(slot)?,
                }
            }
            machine.set_startup_options(*links);
            Machine::ModelB(Box::new(machine))
        }
        Built::Electron { os, basic, plus1 } => {
            let mut machine = electron::Machine::new(os)?;
            machine.fit_basic(basic)?;
            if let Some(plus1) = plus1 {
                machine.fit_plus1();
                machine.insert_rom(PLUS1_ROM, &plus1.rom)?;
                for (roms, images) in CARTRIDGE_ROMS.iter().zip(&plus1.cartridges) {
                    for (&rom, image) in roms.iter().zip(images) {
                        if let Some(image) = image {
                            machine.insert_rom(rom, image)?;
                        }
                    }
                }
            }
            Machine::Electron(Box::new(machine))
        }
    };
    machine.set_sound_output(false);
    Ok(machine)
}

/// Whether `machine` has what `change` changes: its own keyboard's keys,
/// and an analogue port, the Electron's only with the Plus 1.
fn takes(machine: &Machine, change: Change) -> bool {
    match change {
        Change::Key(key, _) => key.model() == machine.model(),
        Change::Break(_) => true,
        Change::Analogue(..) | Change::Fire(_) => match machine {
            Machine::ModelB(_) => true,
            Machine::Electron(machine) => machine.has_plus1(),
        },
    }
}

/// `change` made, one [`takes`] has already allowed.
fn apply(machine: &mut Machine, change: Change) {
    match (machine, change) {
        (machine, Change::Break(down)) => machine.set_break(down),
        (Machine::ModelB(machine), Change::Key(ModelKey::ModelB(key), down)) => {
            machine.set_key(key, down);
        }
        (Machine::Electron(machine), Change::Key(ModelKey::Electron(key), down)) => {
            machine.set_key(key, down);
        }
        (Machine::ModelB(machine), Change::Analogue(channel, level)) => {
            machine.set_analogue(usize::from(channel), level);
        }
        (Machine::Electron(machine), Change::Analogue(channel, level)) => {
            machine.set_analogue(usize::from(channel), level);
        }
        (Machine::ModelB(machine), Change::Fire(buttons)) => machine.set_fire_buttons(buttons),
        (Machine::Electron(machine), Change::Fire(buttons)) => machine.set_fire_buttons(buttons),
        // Another keyboard's key, refused before the slice ran.
        (_, Change::Key(..)) => {}
    }
}

/// The sound's way to the owner: the chip's updates resampled to the output
/// rate, AC-coupled as the machine's output is, then signed 16-bit. A
/// slice's sound is its cycles' exactly: what the instruction that ran past
/// its end made waits for the next slice, as the picture's time does.
struct Sound {
    bank: FilterBank,
    resampler: Resampler,
    coupling: AcCoupling,
    output: Vec<f32>,
    bytes: Vec<u8>,
    /// Updates made past the last slice's end, the next slice's.
    carried: Vec<f32>,
}

impl Sound {
    fn new() -> Option<Self> {
        let bank = FilterBank::for_ratio(Ratio::new(SOUND_RATE, SAMPLE_RATE)?).ok()?;
        let resampler = Resampler::new(&bank, 1).ok()?;
        Some(Self {
            bank,
            resampler,
            coupling: AcCoupling::new(COUPLING),
            output: Vec::new(),
            bytes: Vec::new(),
            carried: Vec::new(),
        })
    }

    /// Drop what was carried: no slice follows on from it.
    fn forget(&mut self) {
        self.carried.clear();
    }

    /// The sound of the slice that ended `overrun` updates before the
    /// machine's time, encoded; nothing if a buffer could not grow, and at
    /// most [`MAX_SAMPLES`].
    fn take(&mut self, machine: &mut Machine, overrun: u64) -> &[u8] {
        self.output.clear();
        self.bytes.clear();
        let audio = machine.take_audio();
        let fresh = audio.as_slice();
        let held = usize::try_from(overrun).map_or(fresh.len(), |held| held.min(fresh.len()));
        let (this, next) = fresh.split_at(fresh.len() - held);
        let mut carried = core::mem::take(&mut self.carried);
        self.resample(&carried);
        self.resample(this);
        carried.clear();
        if carried.try_reserve(next.len()).is_ok() {
            carried.extend_from_slice(next);
        }
        self.carried = carried;
        self.output.truncate(MAX_SAMPLES);
        let length = self.output.len() * SAMPLE_BYTES;
        if self.bytes.try_reserve(length).is_err() {
            return &[];
        }
        self.bytes.resize(length, 0);
        let written = convert::encode(SampleFormat::S16, &self.output, &mut self.bytes);
        &self.bytes[..written]
    }

    /// `updates` resampled onto the output, while it has room.
    fn resample(&mut self, updates: &[f32]) {
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
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
