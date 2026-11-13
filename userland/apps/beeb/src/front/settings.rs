//! The emulator's settings: the machine it builds, and how it is shown,
//! heard and typed at, kept in the application's own store.

use alloc::string::String;
use core::fmt::Write as _;

use tairix_abi::appdata_ipc::validate_bulk_name;
use tairix_appconf::{as_bool, as_permille, as_u32, bool_text, overwrite, Live, Registry};
use tairix_crt::screen::Scaling;

use super::keys::Mapping;
use crate::worker::protocol::SLOTS;

/// A clean-room ROM the bundle ships.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shipped {
    /// The MOS.
    Mos,
    /// BBC BASIC.
    Basic,
    /// The DFS.
    Dfs,
}

impl Shipped {
    /// Its spelling in the store.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Mos => "mos",
            Self::Basic => "basic",
            Self::Dfs => "dfs",
        }
    }

    /// Its image, as `Run` carries it.
    #[must_use]
    pub const fn image(self) -> &'static [u8] {
        match self {
            Self::Mos => crate::firmware::MOS_IMAGE,
            Self::Basic => crate::firmware::BASIC_IMAGE,
            Self::Dfs => crate::firmware::DFS_IMAGE,
        }
    }
}

/// What the OS socket or a sideways slot is fitted with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Image {
    /// Nothing.
    Empty,
    /// Sixteen kilobytes of RAM.
    Ram,
    /// A ROM the bundle ships.
    Shipped(Shipped),
    /// A ROM image the user installed, kept in the store's blob of this name.
    Installed(String),
}

impl Image {
    /// The image `text` names: a shipped ROM, `empty`, `ram`, or a blob
    /// named `rom.` and more.
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "empty" => Self::Empty,
            "ram" => Self::Ram,
            "mos" => Self::Shipped(Shipped::Mos),
            "basic" => Self::Shipped(Shipped::Basic),
            "dfs" => Self::Shipped(Shipped::Dfs),
            blob if blob.starts_with(INSTALLED) && validate_bulk_name(blob).is_ok() => {
                Self::Installed(String::from(blob))
            }
            _ => return None,
        })
    }

    fn spell(&self, out: &mut String) {
        out.push_str(match self {
            Self::Empty => "empty",
            Self::Ram => "ram",
            Self::Shipped(shipped) => shipped.name(),
            Self::Installed(blob) => blob,
        });
    }
}

/// The prefix of every installed ROM's blob.
pub const INSTALLED: &str = "rom.";

/// One setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    /// How host keys reach the BBC's.
    Keyboard,
    /// How the picture is enlarged.
    Scaling,
    /// Whether the picture shows a television's scan lines.
    ScanLines,
    /// The sound's level.
    Volume,
    /// Whether a tape loads as fast as the machine can run.
    FastTape,
    /// The screen mode the links start in.
    Mode,
    /// Whether SHIFT-BREAK boots, or BREAK alone.
    Boot,
    /// The disc timings the links set.
    DiscTiming,
    /// The OS ROM.
    Os,
    /// A sideways slot, 0 to 15.
    Slot(u8),
}

const SLOT_NAMES: [&str; SLOTS] = [
    "slot.0", "slot.1", "slot.2", "slot.3", "slot.4", "slot.5", "slot.6", "slot.7", "slot.8",
    "slot.9", "slot.10", "slot.11", "slot.12", "slot.13", "slot.14", "slot.15",
];

/// The emulator's settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preferences {
    /// How host keys reach the BBC's.
    pub keyboard: Mapping,
    /// How the picture is enlarged.
    pub scaling: Scaling,
    /// Whether the picture shows scan lines.
    pub scan_lines: bool,
    /// The sound's level, in parts per thousand.
    pub volume: u32,
    /// Whether a tape loads as fast as the machine can run.
    pub fast_tape: bool,
    /// The screen mode the links start in, 0 to 7.
    pub mode: u8,
    /// Whether SHIFT-BREAK boots a disc, or BREAK alone.
    pub shift_boots: bool,
    /// The disc timings the links set, 0 to 3, the slowest last.
    pub disc_timing: u8,
    /// The OS ROM: a shipped one or an installed image.
    pub os: Image,
    /// Each sideways slot.
    pub slots: [Image; SLOTS],
}

impl Default for Preferences {
    /// The machine as it left the factory, with the DFS fitted: mode 7,
    /// SHIFT-BREAK booting, the slowest disc timings, BASIC in slot 15 and
    /// the DFS in slot 14.
    fn default() -> Self {
        let mut slots = core::array::from_fn(|_| Image::Empty);
        slots[15] = Image::Shipped(Shipped::Basic);
        slots[14] = Image::Shipped(Shipped::Dfs);
        Self {
            keyboard: Mapping::default(),
            scaling: Scaling::default(),
            scan_lines: false,
            volume: tairix_appconf::PERMILLE_FULL,
            fast_tape: true,
            mode: 7,
            shift_boots: true,
            disc_timing: 3,
            os: Image::Shipped(Shipped::Mos),
            slots,
        }
    }
}

impl Preferences {
    /// The startup options the keyboard links set, as OSBYTE &FF reads them:
    /// the mode in bits 0–2, SHIFT-BREAK booting in bit 3 and the disc
    /// timings in bits 4–5, the two links no MOS reads left open.
    #[must_use]
    pub fn links(&self) -> u8 {
        0xC0 | (self.disc_timing & 3) << 4 | u8::from(self.shift_boots) << 3 | (self.mode & 7)
    }

    /// Whether `other` builds a different machine: one to power on again.
    #[must_use]
    pub fn builds_other_than(&self, other: &Self) -> bool {
        self.os != other.os || self.slots != other.slots || self.links() != other.links()
    }
}

impl Registry for Preferences {
    type Key = Setting;
    const KEYS: &'static [Setting] = &[
        Setting::Keyboard,
        Setting::Scaling,
        Setting::ScanLines,
        Setting::Volume,
        Setting::FastTape,
        Setting::Mode,
        Setting::Boot,
        Setting::DiscTiming,
        Setting::Os,
        Setting::Slot(0),
        Setting::Slot(1),
        Setting::Slot(2),
        Setting::Slot(3),
        Setting::Slot(4),
        Setting::Slot(5),
        Setting::Slot(6),
        Setting::Slot(7),
        Setting::Slot(8),
        Setting::Slot(9),
        Setting::Slot(10),
        Setting::Slot(11),
        Setting::Slot(12),
        Setting::Slot(13),
        Setting::Slot(14),
        Setting::Slot(15),
    ];

    fn name(key: Setting) -> &'static str {
        match key {
            Setting::Keyboard => "keyboard",
            Setting::Scaling => "scaling",
            Setting::ScanLines => "scan-lines",
            Setting::Volume => "volume",
            Setting::FastTape => "fast-tape",
            Setting::Mode => "mode",
            Setting::Boot => "boot",
            Setting::DiscTiming => "disc-timing",
            Setting::Os => "os",
            Setting::Slot(slot) => SLOT_NAMES.get(usize::from(slot)).copied().unwrap_or(""),
        }
    }

    fn read(&mut self, key: Setting, text: &str) -> bool {
        match key {
            Setting::Keyboard => match text {
                "logical" => self.keyboard = Mapping::Logical,
                "physical" => self.keyboard = Mapping::Physical,
                _ => return false,
            },
            Setting::Scaling => match text {
                "sharp" => self.scaling = Scaling::Sharp,
                "smooth" => self.scaling = Scaling::Smooth,
                _ => return false,
            },
            Setting::ScanLines => {
                return as_bool(text).is_ok_and(|on| set(&mut self.scan_lines, on))
            }
            Setting::Volume => {
                return as_permille(text).is_ok_and(|level| set(&mut self.volume, level));
            }
            Setting::FastTape => return as_bool(text).is_ok_and(|on| set(&mut self.fast_tape, on)),
            Setting::Mode => return small(text, 7).is_some_and(|mode| set(&mut self.mode, mode)),
            Setting::Boot => match text {
                "shift" => self.shift_boots = true,
                "break" => self.shift_boots = false,
                _ => return false,
            },
            Setting::DiscTiming => {
                return small(text, 3).is_some_and(|timing| set(&mut self.disc_timing, timing));
            }
            Setting::Os => match Image::parse(text) {
                Some(image @ (Image::Shipped(Shipped::Mos) | Image::Installed(_))) => {
                    self.os = image;
                }
                _ => return false,
            },
            Setting::Slot(slot) => {
                let (Some(image), Some(fitted)) =
                    (Image::parse(text), self.slots.get_mut(usize::from(slot)))
                else {
                    return false;
                };
                *fitted = image;
            }
        }
        true
    }

    fn spell(&self, key: Setting, out: &mut String) -> bool {
        match key {
            Setting::Keyboard => out.push_str(match self.keyboard {
                Mapping::Logical => "logical",
                Mapping::Physical => "physical",
            }),
            Setting::Scaling => out.push_str(match self.scaling {
                Scaling::Sharp => "sharp",
                Scaling::Smooth => "smooth",
            }),
            Setting::ScanLines => out.push_str(bool_text(self.scan_lines)),
            Setting::Volume => return write!(out, "{}", self.volume).is_ok(),
            Setting::FastTape => out.push_str(bool_text(self.fast_tape)),
            Setting::Mode => return write!(out, "{}", self.mode).is_ok(),
            Setting::Boot => out.push_str(if self.shift_boots { "shift" } else { "break" }),
            Setting::DiscTiming => return write!(out, "{}", self.disc_timing).is_ok(),
            Setting::Os => self.os.spell(out),
            Setting::Slot(slot) => match self.slots.get(usize::from(slot)) {
                Some(image) => image.spell(out),
                None => return false,
            },
        }
        true
    }
}

impl Live for Preferences {
    fn take(&mut self, other: &Self, key: Setting) -> bool {
        match key {
            Setting::Keyboard => overwrite(&mut self.keyboard, other.keyboard),
            Setting::Scaling => overwrite(&mut self.scaling, other.scaling),
            Setting::ScanLines => overwrite(&mut self.scan_lines, other.scan_lines),
            Setting::Volume => overwrite(&mut self.volume, other.volume),
            Setting::FastTape => overwrite(&mut self.fast_tape, other.fast_tape),
            Setting::Mode => overwrite(&mut self.mode, other.mode),
            Setting::Boot => overwrite(&mut self.shift_boots, other.shift_boots),
            Setting::DiscTiming => overwrite(&mut self.disc_timing, other.disc_timing),
            Setting::Os => overwrite(&mut self.os, other.os.clone()),
            Setting::Slot(slot) => {
                let at = usize::from(slot);
                match (self.slots.get_mut(at), other.slots.get(at)) {
                    (Some(mine), Some(theirs)) => overwrite(mine, theirs.clone()),
                    _ => false,
                }
            }
        }
    }
}

/// Store `value` in `slot`, answering that it was taken.
fn set<T>(slot: &mut T, value: T) -> bool {
    *slot = value;
    true
}

/// `text` as a number no greater than `most`.
fn small(text: &str, most: u8) -> Option<u8> {
    as_u32(text)
        .ok()
        .and_then(|value| u8::try_from(value).ok())
        .filter(|&value| value <= most)
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
