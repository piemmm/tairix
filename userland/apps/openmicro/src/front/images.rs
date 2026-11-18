//! The images a machine is built from, each ROM as `Run` carries it or as it
//! was read, and the configuration they build: one mapping from a fitting,
//! whoever reads the images a start of it needs.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use super::library::{Fitting, Image};
use crate::machine::c64::{sid, Standard};
use crate::worker::protocol::{Configuration, Plus1, Slot, SLOTS};

/// Why a socket's image could not be had where it needs a ROM.
pub const NO_ROM: &str = "a socket that needs a ROM holds none";

/// A ROM image as a machine is built from it.
pub enum Rom {
    /// As `Run` carries it.
    Carried(&'static [u8]),
    /// As it was read: from the store, or built.
    Read(Vec<u8>),
}

impl Rom {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Carried(bytes) => bytes,
            Self::Read(bytes) => bytes,
        }
    }
}

/// What a sideways slot or cartridge holds, read.
pub enum Socketed {
    /// Nothing.
    Empty,
    /// Sideways RAM.
    Ram,
    /// A ROM.
    Rom(Rom),
}

/// The images a machine is built from, read.
pub enum Images {
    /// A Model B's OS, its slots and its links.
    ModelB {
        /// The OS.
        os: Rom,
        /// Each slot.
        slots: Box<[Socketed; SLOTS]>,
        /// The links, as OSBYTE &FF reads them.
        links: u8,
    },
    /// An Electron's OS, BASIC and Plus 1.
    Electron {
        /// The OS.
        os: Rom,
        /// BASIC.
        basic: Rom,
        /// The Plus 1's ROM and its cartridges' images.
        plus1: Option<(Rom, [[Option<Rom>; 2]; 2])>,
    },
    /// A Commodore 64's standard and SID, its ROMs and each fitted drive's
    /// DOS.
    C64 {
        /// The television standard.
        standard: Standard,
        /// The SID.
        sid: sid::Model,
        /// The KERNAL, BASIC and the character ROM.
        roms: [Rom; 3],
        /// Each drive's DOS, where it is fitted.
        drives: [Option<Rom>; 2],
    },
}

impl Images {
    /// The images `fitting` names, each of its ROMs as `rom` reads it.
    ///
    /// # Errors
    ///
    /// Why `rom` could not read one, or [`NO_ROM`] for a socket that needs a
    /// ROM holding none.
    pub fn of(
        fitting: &Fitting,
        rom: &mut dyn FnMut(&Image) -> Result<Rom, String>,
    ) -> Result<Self, String> {
        let mut needed = |image: &Image| match image {
            Image::Empty | Image::Ram => Err(String::from(NO_ROM)),
            image => rom(image),
        };
        Ok(match fitting {
            Fitting::ModelB { os, slots, links } => {
                let os = needed(os)?;
                let mut read: Box<[Socketed; SLOTS]> =
                    Box::new(core::array::from_fn(|_| Socketed::Empty));
                for (into, image) in read.iter_mut().zip(slots.iter()) {
                    *into = socketed(image, &mut needed)?;
                }
                Self::ModelB {
                    os,
                    slots: read,
                    links: links.byte(),
                }
            }
            Fitting::Electron { os, basic, plus1 } => Self::Electron {
                os: needed(os)?,
                basic: needed(basic)?,
                plus1: match plus1 {
                    Some(plus1) => {
                        let rom = needed(&plus1.rom)?;
                        let mut cartridges: [[Option<Rom>; 2]; 2] = Default::default();
                        for (into, image) in cartridges
                            .as_flattened_mut()
                            .iter_mut()
                            .zip(plus1.cartridges.as_flattened())
                        {
                            *into = match socketed(image, &mut needed)? {
                                Socketed::Rom(rom) => Some(rom),
                                Socketed::Empty | Socketed::Ram => None,
                            };
                        }
                        Some((rom, cartridges))
                    }
                    None => None,
                },
            },
            Fitting::C64 {
                standard,
                sid,
                kernal,
                basic,
                character,
                drives,
            } => Self::C64 {
                standard: *standard,
                sid: *sid,
                roms: [needed(kernal)?, needed(basic)?, needed(character)?],
                drives: [
                    drives[0].as_ref().map(&mut needed).transpose()?,
                    drives[1].as_ref().map(&mut needed).transpose()?,
                ],
            },
        })
    }

    /// The configuration the images build.
    #[must_use]
    pub fn configuration(&self) -> Configuration<'_> {
        match self {
            Self::ModelB { os, slots, links } => {
                let mut built = Box::new([Slot::Empty; SLOTS]);
                for (slot, fitted) in built.iter_mut().zip(slots.iter()) {
                    *slot = match fitted {
                        Socketed::Empty => Slot::Empty,
                        Socketed::Ram => Slot::Ram,
                        Socketed::Rom(rom) => Slot::Rom(rom.bytes()),
                    };
                }
                Configuration::ModelB {
                    os: os.bytes(),
                    slots: built,
                    links: *links,
                }
            }
            Self::Electron { os, basic, plus1 } => Configuration::Electron {
                os: os.bytes(),
                basic: basic.bytes(),
                plus1: plus1.as_ref().map(|(rom, cartridges)| Plus1 {
                    rom: rom.bytes(),
                    cartridges: cartridges
                        .each_ref()
                        .map(|pair| pair.each_ref().map(|rom| rom.as_ref().map(Rom::bytes))),
                }),
            },
            Self::C64 {
                standard,
                sid,
                roms: [kernal, basic, character],
                drives,
            } => Configuration::C64 {
                standard: *standard,
                sid: *sid,
                kernal: kernal.bytes(),
                basic: basic.bytes(),
                character: character.bytes(),
                drives: drives.each_ref().map(|dos| dos.as_ref().map(Rom::bytes)),
            },
        }
    }
}

/// What a socket holding `image` holds, its ROM as `rom` reads it.
fn socketed(
    image: &Image,
    rom: &mut impl FnMut(&Image) -> Result<Rom, String>,
) -> Result<Socketed, String> {
    match image {
        Image::Empty => Ok(Socketed::Empty),
        Image::Ram => Ok(Socketed::Ram),
        image => rom(image).map(Socketed::Rom),
    }
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
