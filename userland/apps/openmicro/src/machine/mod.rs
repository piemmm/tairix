//! The machines: what every model shares — its [`Timing`], the memory map's
//! windows, ROM sockets, the canvas, the cassette and the bus monitor — and
//! one module a model, the BBC Micro Model B in [`bbc`], the Acorn Electron
//! in [`electron`] and the Commodore 64 in [`c64`].
//!
//! Time is a count of the model's own cycles: the Acorn models' at 2 MHz,
//! where an access to a 1 MHz part stretches to the next coincident edge of
//! the two clocks. A device is brought up to the current cycle before the
//! CPU touches it, and a device whose outputs change of their own accord
//! states the next cycle they will, so the interrupt lines are exact at
//! every cycle the CPU samples them.

pub mod bbc;
pub mod c64;
pub mod canvas;
pub mod cassette;
pub mod electron;
mod running;
pub mod via;

pub use running::{BadTape, Machine};

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_crypto::{Fingerprint, FINGERPRINT_LEN};

use crate::snapshot::{Reader, SnapshotError, Writer};

/// Cycles a second: the 2 MHz the CPU runs at and the machine's time counts.
pub const CLOCK: u64 = 2_000_000;

/// The 1 MHz tick a cycle falls in: the clock the 1 MHz parts count.
#[must_use]
pub const fn tick(cycle: u64) -> u64 {
    cycle >> 1
}

/// The cycle 1 MHz tick `tick` begins in.
#[must_use]
pub const fn tick_start(tick: u64) -> u64 {
    tick << 1
}

/// Cycles an access to a 1 MHz part beginning at `cycle` waits for the next
/// coincident falling edge of the two clocks: one when it begins in an even
/// cycle, two in an odd one.
#[must_use]
pub const fn one_mhz_stretch(cycle: u64) -> u64 {
    if cycle & 1 == 0 {
        1
    } else {
        2
    }
}

/// Samples a second every model's sound is rendered at: the SN76489's update
/// rate, at which the Electron's counter is rendered too.
pub const SOUND_RATE: u32 = 250_000;

/// Cycles a sound sample lasts.
pub const CYCLES_PER_SAMPLE: u64 = CLOCK / SOUND_RATE as u64;
const _: () = assert!(CLOCK.is_multiple_of(SOUND_RATE as u64));

/// Samples of sound kept for the front end to take: a second's worth. The
/// sound keeps running exactly past it; the samples are dropped.
pub const AUDIO_CAPACITY: usize = SOUND_RATE as usize;

/// How a machine keeps time: its cycles a second, and the cycles each sample
/// of its sound spans. Neither is zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    clock: u64,
    cycles_per_sample: u64,
}

impl Timing {
    /// The Acorn models': [`CLOCK`], their sound at [`SOUND_RATE`].
    pub const ACORN: Self = Self::of(CLOCK, CYCLES_PER_SAMPLE);

    /// Built only in constants, where a zero fails the build.
    const fn of(clock: u64, cycles_per_sample: u64) -> Self {
        assert!(clock > 0 && cycles_per_sample > 0, "a timing counts time");
        Self {
            clock,
            cycles_per_sample,
        }
    }

    /// Cycles a second.
    #[must_use]
    pub const fn clock(self) -> u64 {
        self.clock
    }

    /// Cycles a sample of its sound spans.
    #[must_use]
    pub const fn cycles_per_sample(self) -> u64 {
        self.cycles_per_sample
    }
}

/// Where the paged ROM window starts: RAM fills the addresses below it.
pub const SIDEWAYS_BASE: u16 = 0x8000;

/// Where the OS ROM starts, and the paged window ends.
pub const OS_BASE: u16 = 0xC000;

/// Bytes of RAM.
pub const RAM_SIZE: usize = SIDEWAYS_BASE as usize;

/// Bytes in a ROM: the OS, or a paged bank, each filling its window.
pub const ROM_SIZE: usize = (OS_BASE - SIDEWAYS_BASE) as usize;

/// The ROM numbers a machine pages into its window.
pub const SLOTS: usize = 16;

/// The memory-mapped I/O pages: FRED, JIM and SHEILA.
pub const IO_BASE: u16 = 0xFC00;
/// The first byte past the I/O pages.
pub const IO_END: u16 = 0xFF00;

/// FRED, the first 1 MHz expansion page.
pub const FRED: u16 = IO_BASE;
/// JIM, the second, through which expansion memory is paged.
pub const JIM: u16 = FRED + 0x100;
/// SHEILA, the page the machine's own devices answer in.
pub const SHEILA: u16 = JIM + 0x100;

/// The analogue port's channels, every model's: two joysticks' two axes.
pub const ANALOGUE_CHANNELS: usize = 4;
/// The joystick fire buttons a machine is told are held, bit 0 the first's.
pub const FIRE_BUTTONS: u8 = 0x03;

/// Bytes the printer and serial ports keep for the front end to take; more
/// are dropped.
pub const PORT_CAPACITY: usize = 1 << 16;

/// Keep what fits of `bytes` beneath `PORT_CAPACITY`.
fn collect_port(output: &mut Vec<u8>, bytes: impl IntoIterator<Item = u8>) {
    let room = PORT_CAPACITY.saturating_sub(output.len());
    output.extend(bytes.into_iter().take(room));
}

/// The models.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// The 32K BBC Micro Model B.
    ModelB,
    /// The Acorn Electron.
    Electron,
    /// The Commodore 64.
    C64,
}

impl Model {
    /// Every model, in the order a snapshot numbers them.
    pub const ALL: [Self; 3] = [Self::ModelB, Self::Electron, Self::C64];

    /// Its number, as a snapshot and the worker's messages record it.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::ModelB => 0,
            Self::Electron => 1,
            Self::C64 => 2,
        }
    }

    /// The model numbered `number`, if there is one.
    #[must_use]
    pub fn from_number(number: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|model| model.number() == number)
    }

    /// What it is called.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::ModelB => "BBC Micro Model B",
            Self::Electron => "Acorn Electron",
            Self::C64 => "Commodore 64",
        }
    }

    /// How often a second its OS scans the keyboard: the MOS each
    /// centisecond, the KERNAL each jiffy.
    #[must_use]
    pub const fn key_scans_per_second(self) -> u64 {
        match self {
            Self::ModelB | Self::Electron => 100,
            Self::C64 => 60,
        }
    }

    /// Whether its deck has a PLAY key: a C64's datasette.
    #[must_use]
    pub const fn has_play_key(self) -> bool {
        matches!(self, Self::C64)
    }

    /// Whether it has CAPS LOCK's light.
    #[must_use]
    pub const fn has_caps_lock(self) -> bool {
        matches!(self, Self::ModelB | Self::Electron)
    }

    /// Whether it has SHIFT LOCK's light.
    #[must_use]
    pub const fn has_shift_lock(self) -> bool {
        matches!(self, Self::ModelB)
    }

    /// Record the model a snapshot is taken on: its first section.
    fn save(self, w: &mut Writer) {
        w.section(*b"MODL", |w| w.put(&self.number()));
    }

    /// Refuse a snapshot that does not open with this model.
    fn check(self, r: &mut Reader<'_>) -> Result<(), SnapshotError> {
        let number = r.section(*b"MODL", Reader::get::<u8>)?;
        match Self::from_number(number) {
            Some(model) if model == self => Ok(()),
            Some(_) => Err(SnapshotError::Model),
            None => Err(SnapshotError::Malformed),
        }
    }
}

/// A ROM image, shared rather than copied, and the fingerprint a snapshot
/// records it by, taken once.
#[derive(Clone, Debug)]
struct Rom {
    image: Rc<[u8]>,
    fingerprint: [u8; FINGERPRINT_LEN],
}

impl Rom {
    fn new(image: Box<[u8]>) -> Self {
        let fingerprint = rom_fingerprint(&image);
        Self {
            image: image.into(),
            fingerprint,
        }
    }

    /// The image `bytes` make, mirrored to fill its window.
    fn of(bytes: &[u8]) -> Result<Self, RomError> {
        Ok(Self::new(rom_image(bytes)?))
    }
}

/// What a paged slot holds.
#[derive(Clone, Debug, Default)]
enum Slot {
    /// An empty socket: reads see the floating bus.
    #[default]
    Empty,
    /// A ROM image.
    Rom(Rom),
    /// 16 KiB of RAM.
    Ram(Box<[u8]>),
}

impl Slot {
    /// The image the slot pages into the window, if it holds one.
    fn image(&self) -> Option<&[u8]> {
        match self {
            Self::Rom(rom) => Some(&rom.image),
            Self::Ram(bank) => Some(bank),
            Self::Empty => None,
        }
    }
}

/// Why a ROM image was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RomError {
    /// Not 16 KiB or a power-of-two fraction of it.
    BadSize,
    /// No such slot, or not one the model fits.
    BadSlot,
}

/// A ROM image of 2, 4, 8 or 16 KiB, mirrored to fill 16 KiB as the
/// unused address lines leave it.
fn rom_image(bytes: &[u8]) -> Result<Box<[u8]>, RomError> {
    if !matches!(bytes.len(), 0x800 | 0x1000 | 0x2000 | 0x4000) {
        return Err(RomError::BadSize);
    }
    let mut image = vec![0; ROM_SIZE];
    for chunk in image.chunks_exact_mut(bytes.len()) {
        chunk.copy_from_slice(bytes);
    }
    Ok(image.into_boxed_slice())
}

/// The domain a ROM image's fingerprint is taken in.
const ROM_DOMAIN: &[u8] = b"tairix.openmicro.rom";

/// The fingerprint a snapshot records a ROM image by.
fn rom_fingerprint(image: &[u8]) -> [u8; FINGERPRINT_LEN] {
    let mut fingerprint = Fingerprint::new(ROM_DOMAIN);
    fingerprint.field(image);
    fingerprint.finish()
}

/// What a snapshot records each paged slot as holding.
mod fitting {
    pub const EMPTY: u8 = 0;
    pub const ROM: u8 = 1;
    pub const RAM: u8 = 2;
}

/// Record the OS by fingerprint, each slot's ROM by its fingerprint and its
/// RAM by its contents.
fn save_fitting(os: &Rom, slots: &[Slot], w: &mut Writer) {
    w.put(&os.fingerprint);
    for slot in slots {
        match slot {
            Slot::Empty => w.put(&fitting::EMPTY),
            Slot::Rom(rom) => {
                w.put(&fitting::ROM);
                w.put(&rom.fingerprint);
            }
            Slot::Ram(bank) => {
                w.put(&fitting::RAM);
                w.bytes(bank);
            }
        }
    }
}

/// The slots a snapshot restores: these ROMs where it recorded the same
/// ones, its own RAM's contents, and a refusal for any socket fitted
/// otherwise, the OS among them.
fn load_fitting<const N: usize>(
    os: &Rom,
    fitted: &[Slot; N],
    r: &mut Reader<'_>,
) -> Result<[Slot; N], SnapshotError> {
    let holds = |holds: bool| {
        if holds {
            Ok(())
        } else {
            Err(SnapshotError::Fitting)
        }
    };
    holds(r.get::<[u8; FINGERPRINT_LEN]>()? == os.fingerprint)?;
    let mut slots: [Slot; N] = core::array::from_fn(|_| Slot::Empty);
    for (slot, fitted_now) in slots.iter_mut().zip(fitted) {
        *slot = match (r.get::<u8>()?, fitted_now) {
            (fitting::EMPTY, Slot::Empty) => Slot::Empty,
            (fitting::ROM, Slot::Rom(rom)) => {
                holds(r.get::<[u8; FINGERPRINT_LEN]>()? == rom.fingerprint)?;
                Slot::Rom(rom.clone())
            }
            (fitting::RAM, Slot::Ram(_)) => {
                let bank = r.bytes_at_most(ROM_SIZE)?;
                Reader::check(bank.len() == ROM_SIZE)?;
                Slot::Ram(bank.into())
            }
            (fitting::EMPTY | fitting::ROM | fitting::RAM, _) => {
                return Err(SnapshotError::Fitting)
            }
            _ => return Err(SnapshotError::Malformed),
        };
    }
    Ok(slots)
}

/// How a machine's letter keys type letters now, which pasted text is typed
/// by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LetterCase {
    /// Small letters alone and capitals with SHIFT: CAPS LOCK off, or the
    /// C64's lower-case set.
    #[default]
    Small,
    /// Capitals alone and small letters with SHIFT: CAPS LOCK on.
    Capitals,
    /// Capitals alone, SHIFT making graphics: the C64's upper-case set, where
    /// a small letter is typed as its capital.
    CapitalsOnly,
}

/// One access to the memory-mapped I/O pages, as a bus monitor records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoAccess {
    /// The cycle the device saw it in.
    pub cycle: u64,
    /// The address.
    pub addr: u16,
    /// The byte transferred.
    pub value: u8,
    /// Whether the CPU wrote it.
    pub write: bool,
}

/// The most accesses a bus monitor keeps before it drops the oldest.
pub const IO_TRACE_CAPACITY: usize = 4096;

/// A bus monitor: while asked, the accesses to the I/O pages, oldest first.
#[derive(Debug, Default)]
struct Monitor {
    trace: Option<VecDeque<IoAccess>>,
}

impl Monitor {
    fn set(&mut self, on: bool) {
        self.trace = on.then(VecDeque::new);
    }

    /// A monitor recording as this one does, holding nothing yet.
    fn fresh(&self) -> Self {
        Self {
            trace: self.trace.as_ref().map(|_| VecDeque::new()),
        }
    }

    fn record(&mut self, access: IoAccess) {
        if let Some(trace) = &mut self.trace {
            if trace.len() == IO_TRACE_CAPACITY {
                trace.pop_front();
            }
            trace.push_back(access);
        }
    }

    fn take(&mut self) -> impl Iterator<Item = IoAccess> + '_ {
        self.trace.iter_mut().flat_map(|trace| trace.drain(..))
    }
}

#[cfg(test)]
mod tests;
