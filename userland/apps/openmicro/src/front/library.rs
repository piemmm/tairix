//! A machine as the library keeps it (`plans/BEEB.md` §5): its model and what
//! is fitted to it, how it is shown, typed at and heard, and its folder in the
//! user's documents with the media of it its drives and deck held.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;

use tairix_abi::appdata_ipc::validate_bulk_name;
use tairix_crt::screen::Scaling;
use tairix_crt::Tube;
use tairix_machines::record::{get_tube, put_tube, store_holds, tube_within, Kept};
use tairix_machines::state::Bounds;
use tairix_sandbox::wire::{Reader, WireError, Writer};

use super::keys::Mapping;
use crate::machine::canvas::{CANVAS_HEIGHT, CANVAS_WIDTH};
use crate::machine::Model;
use crate::worker::protocol::{MAX_BULK, SLOTS};

/// The prefix of every installed ROM image's blob.
pub const INSTALLED: &str = "rom.";

/// The loudest a machine plays, in thousandths of its level.
pub const FULL_VOLUME: u16 = 1000;

/// The longest name a file of a machine's folder takes.
const MAX_FILE_NAME: usize = tairix_abi::FS_NAME_MAX;

/// `n` dots as a frame's measure counts them.
const fn dots(n: usize) -> u32 {
    assert!(n < 1 << 16, "a canvas past any frame");
    let bytes = n.to_le_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], 0, 0])
}

/// What a machine's state may hold: its canvas, and the largest snapshot.
pub const STATE_BOUNDS: Bounds = Bounds {
    largest: (dots(CANVAS_WIDTH), dots(CANVAS_HEIGHT)),
    most_kept: MAX_BULK,
};

/// A clean-room ROM the bundle ships.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shipped {
    /// The Model B's OS.
    Mos,
    /// The Electron's OS.
    ElectronOs,
    /// BBC BASIC.
    Basic,
    /// The DFS.
    Dfs,
    /// The Plus 1's ROM.
    Plus1,
}

impl Shipped {
    const ALL: [Self; 5] = [
        Self::Mos,
        Self::ElectronOs,
        Self::Basic,
        Self::Dfs,
        Self::Plus1,
    ];

    /// Its image, as `Run` carries it.
    #[must_use]
    pub const fn image(self) -> &'static [u8] {
        match self {
            Self::Mos => crate::firmware::MOS_IMAGE,
            Self::ElectronOs => crate::firmware::ELECTRON_OS_IMAGE,
            Self::Basic => crate::firmware::BASIC_IMAGE,
            Self::Dfs => crate::firmware::DFS_IMAGE,
            Self::Plus1 => crate::firmware::PLUS1_IMAGE,
        }
    }

    /// What it is called in a list.
    #[must_use]
    pub const fn called(self) -> &'static str {
        match self {
            Self::Mos => "OpenMOS",
            Self::ElectronOs => "OpenMOS Electron",
            Self::Basic => "TAIRiX BASIC",
            Self::Dfs => "TAIRiX DFS",
            Self::Plus1 => "OpenMOS Plus 1",
        }
    }

    fn wire(self) -> u8 {
        Self::ALL
            .iter()
            .position(|&shipped| shipped == self)
            .and_then(|at| u8::try_from(at).ok())
            .unwrap_or(u8::MAX)
    }
}

/// What a ROM socket holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Image {
    /// Nothing.
    Empty,
    /// Sixteen kilobytes of sideways RAM.
    Ram,
    /// A ROM the bundle ships.
    Shipped(Shipped),
    /// A ROM image the user installed, kept in the store's blob of this name.
    Installed(String),
}

impl Image {
    /// What it is called in a list.
    #[must_use]
    pub fn called(&self) -> &str {
        match self {
            Self::Empty => "Empty",
            Self::Ram => "Sideways RAM",
            Self::Shipped(shipped) => shipped.called(),
            Self::Installed(blob) => blob.strip_prefix(INSTALLED).unwrap_or(blob),
        }
    }

    /// Whether it is a ROM: one shipped or installed.
    #[must_use]
    pub const fn is_rom(&self) -> bool {
        matches!(self, Self::Shipped(_) | Self::Installed(_))
    }

    /// Whether its name, where it is installed, is one the store gives.
    fn named_well(&self) -> bool {
        match self {
            Self::Installed(blob) => {
                blob.starts_with(INSTALLED)
                    && blob.len() > INSTALLED.len()
                    && validate_bulk_name(blob).is_ok()
            }
            Self::Empty | Self::Ram | Self::Shipped(_) => true,
        }
    }

    fn put(&self, w: &mut Writer) {
        match self {
            Self::Empty => w.u8(0),
            Self::Ram => w.u8(1),
            Self::Shipped(shipped) => {
                w.u8(2);
                w.u8(shipped.wire());
            }
            Self::Installed(blob) => {
                w.u8(3);
                w.str(blob);
            }
        }
    }

    fn get(r: &mut Reader<'_>) -> Result<Option<Self>, WireError> {
        Ok(match r.u8()? {
            0 => Some(Self::Empty),
            1 => Some(Self::Ram),
            2 => Shipped::ALL
                .get(usize::from(r.u8()?))
                .copied()
                .map(Self::Shipped),
            3 => Some(Self::Installed(String::from(
                r.str(tairix_abi::appdata_ipc::APPDATA_NAME_MAX)?,
            ))),
            _ => None,
        })
    }
}

/// A ROM socket of either model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Socket {
    /// The Model B's OS.
    ModelBOs,
    /// A Model B sideways slot, 0 to 15.
    Slot(u8),
    /// The Electron's OS.
    ElectronOs,
    /// The Electron's BASIC.
    Basic,
    /// The Plus 1's own ROM.
    Plus1,
    /// A Plus 1 cartridge ROM: the slot's two, then the other's.
    Cartridge(u8),
}

const MODEL_B_OS: [Image; 1] = [Image::Shipped(Shipped::Mos)];
const SIDEWAYS: [Image; 4] = [
    Image::Empty,
    Image::Ram,
    Image::Shipped(Shipped::Basic),
    Image::Shipped(Shipped::Dfs),
];
const ELECTRON_OS: [Image; 1] = [Image::Shipped(Shipped::ElectronOs)];
const BASIC: [Image; 1] = [Image::Shipped(Shipped::Basic)];
const PLUS1: [Image; 1] = [Image::Shipped(Shipped::Plus1)];
const CARTRIDGE: [Image; 1] = [Image::Empty];

impl Socket {
    /// What it may hold besides an image the user installed, in the order
    /// its list offers them.
    #[must_use]
    pub const fn offered(self) -> &'static [Image] {
        match self {
            Self::ModelBOs => &MODEL_B_OS,
            Self::Slot(_) => &SIDEWAYS,
            Self::ElectronOs => &ELECTRON_OS,
            Self::Basic => &BASIC,
            Self::Plus1 => &PLUS1,
            Self::Cartridge(_) => &CARTRIDGE,
        }
    }

    /// Whether it may hold `image`.
    #[must_use]
    pub fn takes(self, image: &Image) -> bool {
        matches!(image, Image::Installed(_)) || self.offered().contains(image)
    }

    /// What its row is called.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::ModelBOs | Self::ElectronOs => String::from("OS"),
            Self::Slot(slot) => format!("Slot {slot}"),
            Self::Basic => String::from("BASIC"),
            Self::Plus1 => String::from("Plus 1"),
            Self::Cartridge(n) => format!("Cartridge {}, ROM {}", n / 2 + 1, n % 2 + 1),
        }
    }
}

/// The Model B's start-up links: its screen mode, whether SHIFT-BREAK boots a
/// disc, and its disc timings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Links {
    /// The screen mode, 0 to 7.
    pub mode: u8,
    /// Whether SHIFT-BREAK boots, or BREAK alone.
    pub shift_boots: bool,
    /// The disc timings, 0 to 3, the slowest last.
    pub disc_timing: u8,
}

impl Default for Links {
    /// As the machine left the factory: mode 7, SHIFT-BREAK booting and the
    /// slowest disc timings.
    fn default() -> Self {
        Self {
            mode: 7,
            shift_boots: true,
            disc_timing: 3,
        }
    }
}

impl Links {
    /// The links as OSBYTE &FF reads them: the mode in bits 0–2, SHIFT-BREAK
    /// booting in bit 3 and the disc timings in bits 4–5, the two links no MOS
    /// reads left open.
    #[must_use]
    pub const fn byte(self) -> u8 {
        0xC0 | (self.disc_timing & 3) << 4 | (self.shift_boots as u8) << 3 | (self.mode & 7)
    }

    const fn within(self) -> bool {
        self.mode <= 7 && self.disc_timing <= 3
    }
}

/// The Plus 1's ROM and the images its two cartridge slots hold, two a slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plus1Fitting {
    /// Its ROM.
    pub rom: Image,
    /// Each cartridge slot's two images.
    pub cartridges: [[Image; 2]; 2],
}

/// The model a machine is, and what its sockets hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fitting {
    /// A Model B: its OS, its sixteen sideways slots and its links.
    ModelB {
        /// The OS.
        os: Image,
        /// Each sideways slot, 0 to 15.
        slots: Box<[Image; SLOTS]>,
        /// The start-up links.
        links: Links,
    },
    /// An Electron: its OS, BASIC, and the Plus 1 where it is fitted.
    Electron {
        /// The OS.
        os: Image,
        /// BASIC, in ROMs 10 and 11.
        basic: Image,
        /// The Plus 1.
        plus1: Option<Box<Plus1Fitting>>,
    },
}

impl Fitting {
    /// A Model B as it left the factory with the DFS fitted: BASIC in slot 15
    /// and the DFS in slot 14.
    #[must_use]
    pub fn model_b() -> Self {
        let mut slots: Box<[Image; SLOTS]> = Box::new(core::array::from_fn(|_| Image::Empty));
        slots[15] = Image::Shipped(Shipped::Basic);
        slots[14] = Image::Shipped(Shipped::Dfs);
        Self::ModelB {
            os: Image::Shipped(Shipped::Mos),
            slots,
            links: Links::default(),
        }
    }

    /// An Electron, with its Plus 1 where `plus1`.
    #[must_use]
    pub fn electron(plus1: bool) -> Self {
        Self::Electron {
            os: Image::Shipped(Shipped::ElectronOs),
            basic: Image::Shipped(Shipped::Basic),
            plus1: plus1.then(|| {
                Box::new(Plus1Fitting {
                    rom: Image::Shipped(Shipped::Plus1),
                    cartridges: core::array::from_fn(|_| core::array::from_fn(|_| Image::Empty)),
                })
            }),
        }
    }

    /// The model it is.
    #[must_use]
    pub const fn model(&self) -> Model {
        match self {
            Self::ModelB { .. } => Model::ModelB,
            Self::Electron { .. } => Model::Electron,
        }
    }

    /// Every socket it has and what each holds, in the order its settings
    /// list them: a Model B's slots from the highest down.
    pub fn sockets(&self) -> impl Iterator<Item = (Socket, &Image)> {
        let (os, slots, electron) = match self {
            Self::ModelB { os, slots, .. } => ((Socket::ModelBOs, os), Some(slots), None),
            Self::Electron { os, basic, plus1 } => (
                (Socket::ElectronOs, os),
                None,
                Some((basic, plus1.as_deref())),
            ),
        };
        let slots = slots.into_iter().flat_map(|slots| {
            (0..SLOTS)
                .rev()
                .filter_map(|at| Some((Socket::Slot(u8::try_from(at).ok()?), &slots[at])))
        });
        let electron = electron.into_iter().flat_map(|(basic, plus1)| {
            let plus1 = plus1.into_iter().flat_map(|plus1| {
                let cartridges = plus1
                    .cartridges
                    .as_flattened()
                    .iter()
                    .zip(0u8..)
                    .map(|(image, n)| (Socket::Cartridge(n), image));
                core::iter::once((Socket::Plus1, &plus1.rom)).chain(cartridges)
            });
            core::iter::once((Socket::Basic, basic)).chain(plus1)
        });
        core::iter::once(os).chain(slots).chain(electron)
    }

    /// What `socket` holds, to change.
    pub fn socket_mut(&mut self, socket: Socket) -> Option<&mut Image> {
        match (self, socket) {
            (Self::ModelB { os, .. }, Socket::ModelBOs)
            | (Self::Electron { os, .. }, Socket::ElectronOs) => Some(os),
            (Self::ModelB { slots, .. }, Socket::Slot(slot)) => slots.get_mut(usize::from(slot)),
            (Self::Electron { basic, .. }, Socket::Basic) => Some(basic),
            (Self::Electron { plus1, .. }, Socket::Plus1) => {
                plus1.as_deref_mut().map(|p| &mut p.rom)
            }
            (Self::Electron { plus1, .. }, Socket::Cartridge(n)) => plus1
                .as_deref_mut()?
                .cartridges
                .as_flattened_mut()
                .get_mut(usize::from(n)),
            _ => None,
        }
    }

    /// Whether every socket holds what it may, and the links are in range.
    fn holds_well(&self) -> bool {
        let links = match self {
            Self::ModelB { links, .. } => links.within(),
            Self::Electron { .. } => true,
        };
        links
            && self
                .sockets()
                .all(|(socket, image)| socket.takes(image) && image.named_well())
    }

    fn put(&self, w: &mut Writer) {
        w.u8(self.model().number());
        match self {
            Self::ModelB { os, slots, links } => {
                os.put(w);
                for slot in slots.iter() {
                    slot.put(w);
                }
                w.u8(links.mode);
                w.u8(u8::from(links.shift_boots));
                w.u8(links.disc_timing);
            }
            Self::Electron { os, basic, plus1 } => {
                os.put(w);
                basic.put(w);
                w.u8(u8::from(plus1.is_some()));
                if let Some(plus1) = plus1 {
                    plus1.rom.put(w);
                    for image in plus1.cartridges.as_flattened() {
                        image.put(w);
                    }
                }
            }
        }
    }

    fn get(r: &mut Reader<'_>) -> Result<Option<Self>, WireError> {
        let Some(model) = Model::from_number(r.u8()?) else {
            return Ok(None);
        };
        let Some(os) = Image::get(r)? else {
            return Ok(None);
        };
        Ok(match model {
            Model::ModelB => {
                let mut slots: Box<[Image; SLOTS]> =
                    Box::new(core::array::from_fn(|_| Image::Empty));
                for slot in slots.iter_mut() {
                    let Some(image) = Image::get(r)? else {
                        return Ok(None);
                    };
                    *slot = image;
                }
                let (mode, boots, disc_timing) = (r.u8()?, r.u8()?, r.u8()?);
                (boots <= 1).then_some(Self::ModelB {
                    os,
                    slots,
                    links: Links {
                        mode,
                        shift_boots: boots == 1,
                        disc_timing,
                    },
                })
            }
            Model::Electron => {
                let Some(basic) = Image::get(r)? else {
                    return Ok(None);
                };
                let plus1 = match r.u8()? {
                    0 => None,
                    1 => {
                        let Some(rom) = Image::get(r)? else {
                            return Ok(None);
                        };
                        let mut cartridges: [[Image; 2]; 2] =
                            core::array::from_fn(|_| core::array::from_fn(|_| Image::Empty));
                        for image in cartridges.as_flattened_mut() {
                            let Some(read) = Image::get(r)? else {
                                return Ok(None);
                            };
                            *image = read;
                        }
                        Some(Box::new(Plus1Fitting { rom, cartridges }))
                    }
                    _ => return Ok(None),
                };
                Some(Self::Electron { os, basic, plus1 })
            }
        })
    }
}

/// A file of the machine's folder a drive or the deck held, and its digest
/// when the machine was suspended holding it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// The file's name in the folder.
    pub name: String,
    /// What it held when the machine was suspended, while it is.
    pub digest: Option<[u8; 32]>,
}

impl Held {
    fn named_well(&self) -> bool {
        self.name.len() <= MAX_FILE_NAME && tairix_path::validate_visible_name(&self.name).is_ok()
    }

    fn put(held: Option<&Self>, w: &mut Writer) {
        let Some(held) = held else {
            w.u8(0);
            return;
        };
        w.u8(1 + u8::from(held.digest.is_some()));
        w.str(&held.name);
        if let Some(digest) = held.digest {
            w.raw(&digest);
        }
    }

    /// What a drive or the deck held, a tag no record writes refused as
    /// malformed.
    fn get(r: &mut Reader<'_>) -> Result<Option<Self>, WireError> {
        let tag = r.u8()?;
        if tag == 0 {
            return Ok(None);
        }
        if tag > 2 {
            return Err(WireError::Malformed);
        }
        let name = String::from(r.str(MAX_FILE_NAME)?);
        let digest = if tag == 2 {
            Some(<[u8; 32]>::try_from(r.take(32)?).map_err(|_| WireError::Malformed)?)
        } else {
            None
        };
        Ok(Some(Self { name, digest }))
    }
}

/// A machine of the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    /// Its number.
    pub id: u32,
    /// Its name, which its window is titled with.
    pub name: String,
    /// Its model and what is fitted.
    pub fitting: Fitting,
    /// The picture tube its screen is drawn through.
    pub tube: Tube,
    /// How its picture is enlarged.
    pub scaling: Scaling,
    /// How host keys reach its keyboard.
    pub keyboard: Mapping,
    /// Its sound's level, in thousandths.
    pub volume: u16,
    /// Whether a tape loads as fast as the machine can run.
    pub fast_tape: bool,
    /// Its folder's name in the application's documents folder, once made.
    pub folder: Option<String>,
    /// The files of its folder each drive held.
    pub discs: [Option<Held>; 2],
    /// The file of its folder the deck held.
    pub tape: Option<Held>,
}

impl Machine {
    /// A machine numbered `id` called `name`, fitted so, shown and heard as
    /// a new one is.
    #[must_use]
    pub fn new(id: u32, name: String, fitting: Fitting) -> Self {
        Self {
            id,
            name,
            fitting,
            tube: Tube::default(),
            scaling: Scaling::default(),
            keyboard: Mapping::default(),
            volume: FULL_VOLUME,
            fast_tape: true,
            folder: None,
            discs: [None, None],
            tape: None,
        }
    }

    /// The model it is.
    #[must_use]
    pub const fn model(&self) -> Model {
        self.fitting.model()
    }
}

/// Why the library may not hold a machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// A socket holds what it may not, or the links are out of range.
    Fitting,
    /// Its tube draws past what a tube may.
    Display,
    /// Its volume is past full.
    Volume,
    /// Its folder, or a file of it, is not a name a folder holds.
    Folder,
    /// A drive holds a disc on a model with no drives.
    Media,
}

/// The folders' names: a machine's folder must be one visible name.
fn folder_named_well(folder: &str) -> bool {
    folder.len() <= MAX_FILE_NAME && tairix_path::validate_visible_name(folder).is_ok()
}

impl Kept for Machine {
    type Invalid = Invalid;
    const MAGIC: u32 = u32::from_le_bytes(*b"OMLB");
    const VERSION: u16 = 1;
    const MAX_MACHINES: usize = 20;
    const MAX_RECORD: usize = 64 * 1024;
    const PARTS: &'static [(&'static str, usize)] = &[];

    fn id(&self) -> u32 {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn put(&self, w: &mut Writer) {
        self.fitting.put(w);
        put_tube(w, self.tube);
        w.u8(match self.scaling {
            Scaling::Sharp => 0,
            Scaling::Smooth => 1,
        });
        w.u8(match self.keyboard {
            Mapping::Logical => 0,
            Mapping::Physical => 1,
        });
        w.u16(self.volume);
        w.u8(u8::from(self.fast_tape));
        match &self.folder {
            Some(folder) => {
                w.u8(1);
                w.str(folder);
            }
            None => w.u8(0),
        }
        for disc in &self.discs {
            Held::put(disc.as_ref(), w);
        }
        Held::put(self.tape.as_ref(), w);
    }

    fn get(id: u32, name: String, r: &mut Reader<'_>) -> Result<Option<Self>, WireError> {
        let Some(fitting) = Fitting::get(r)? else {
            return Ok(None);
        };
        let tube = get_tube(r)?;
        let scaling = match r.u8()? {
            0 => Scaling::Sharp,
            1 => Scaling::Smooth,
            _ => return Ok(None),
        };
        let keyboard = match r.u8()? {
            0 => Mapping::Logical,
            1 => Mapping::Physical,
            _ => return Ok(None),
        };
        let volume = r.u16()?;
        let fast_tape = match r.u8()? {
            0 => false,
            1 => true,
            _ => return Ok(None),
        };
        let folder = match r.u8()? {
            0 => None,
            1 => Some(String::from(r.str(MAX_FILE_NAME)?)),
            _ => return Ok(None),
        };
        let (zero, one, tape) = (Held::get(r)?, Held::get(r)?, Held::get(r)?);
        Ok(Some(Self {
            id,
            name,
            fitting,
            tube,
            scaling,
            keyboard,
            volume,
            fast_tape,
            folder,
            discs: [zero, one],
            tape,
        }))
    }

    fn invalid(&self) -> Option<Invalid> {
        let media = self
            .discs
            .iter()
            .chain(core::iter::once(&self.tape))
            .flatten();
        if !self.fitting.holds_well() {
            Some(Invalid::Fitting)
        } else if !tube_within(self.tube) {
            Some(Invalid::Display)
        } else if self.volume > FULL_VOLUME {
            Some(Invalid::Volume)
        } else if !self.folder.as_deref().is_none_or(folder_named_well)
            || !media.clone().all(Held::named_well)
            || (self.folder.is_none() && media.clone().next().is_some())
        {
            Some(Invalid::Folder)
        } else if !self.model().has_drives() && self.discs.iter().any(Option::is_some) {
            Some(Invalid::Media)
        } else {
            None
        }
    }

    fn holds(&self, _word: &str, _index: usize) -> bool {
        false
    }
}

// The store holds the record's two copies and a state a machine, and room
// beside them for the ROM images the user installs.
const _: () = assert!(Machine::MAX_MACHINES + 2 < store_holds(Machine::PARTS));

/// What a machine is, as its card says beneath it.
#[must_use]
pub fn describe(machine: &Machine) -> String {
    describe_fitting(&machine.fitting)
}

/// What a machine fitted so is.
#[must_use]
pub fn describe_fitting(fitting: &Fitting) -> String {
    match fitting {
        Fitting::ModelB { slots, .. } => {
            let dfs = slots.contains(&Image::Shipped(Shipped::Dfs));
            format!(
                "{}{}",
                Model::ModelB.title(),
                if dfs { " with the DFS" } else { "" }
            )
        }
        Fitting::Electron { plus1, .. } => format!(
            "{}{}",
            Model::Electron.title(),
            if plus1.is_some() {
                " with a Plus 1"
            } else {
                ""
            }
        ),
    }
}

#[cfg(test)]
#[path = "library_tests.rs"]
mod tests;
