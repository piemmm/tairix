//! A machine's rack of media: each disc's and the tape's record with the file
//! it came from, what becomes of what the machine hands over, and the one
//! picker open at a time with the saves waiting for it.
//!
//! The machine worker holds the only copy of what a guest writes until it is
//! handed over, so nothing handed over is dropped: a disc or tape goes back
//! to the file it came from, or is offered to be saved.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::library::Socket;
use crate::machine::bbc::fdc::PhysicalDrive;
use crate::machine::Model;
use crate::media::disc::ImageFormat;
use crate::worker::protocol::{Media, Written};

/// A disc in a drive. One with no file is write-protected, so the machine
/// holds nothing of it the record lacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disc {
    /// The file's name.
    pub name: String,
    /// How its image lays its sides out.
    pub format: ImageFormat,
    /// The file it is written back to, while it may be written.
    pub medium: Option<u32>,
    /// Whether the file is of the machine's folder, which the machine keeps
    /// holding between runs.
    pub of_folder: bool,
    /// Its image as last read or collected.
    pub image: Vec<u8>,
}

/// The tape in the deck.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tape {
    /// The file's name.
    pub name: String,
    /// The file it may be written back to.
    pub medium: Option<u32>,
    /// Whether the file is of the machine's folder.
    pub of_folder: bool,
    /// Its image as read.
    pub image: Vec<u8>,
}

/// What a file the user opens is to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// A disc for a drive.
    Disc(PhysicalDrive),
    /// The deck's tape.
    Tape,
    /// A snapshot to restore.
    Snapshot,
    /// A ROM image to install for a socket.
    Rom(Socket),
}

impl Purpose {
    /// What a file named `name` is to a `model` by its extension, a disc
    /// going to drive 0; `None` for an image the model has nothing to take.
    #[must_use]
    pub fn of(name: &str, model: Model) -> Option<Self> {
        match extension(name)?.as_str() {
            "ssd" | "dsd" if model.has_drives() => Some(Self::Disc(PhysicalDrive::Zero)),
            "uef" | "csw" => Some(Self::Tape),
            _ => None,
        }
    }
}

/// How a disc image named `name` lays its sides out, by its extension.
#[must_use]
pub fn format_of(name: &str) -> ImageFormat {
    if extension(name).is_some_and(|extension| extension == "dsd") {
        ImageFormat::Dsd
    } else {
        ImageFormat::Ssd
    }
}

fn extension(name: &str) -> Option<String> {
    Some(name.rsplit_once('.')?.1.to_ascii_lowercase())
}

/// What a save holds, which decides the formats its picker offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holding {
    /// A disc image.
    Disc,
    /// A UEF tape image.
    Tape,
    /// A snapshot of the machine.
    Snapshot,
}

/// Bytes waiting for the user to choose where they are saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saving {
    /// The name the picker suggests.
    pub suggested: String,
    /// What they are.
    pub holding: Holding,
    /// What is saved.
    pub bytes: Vec<u8>,
}

/// A request that took media out, put them in, or collected discs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handover {
    /// The filed discs collected; nothing taken out.
    Collect,
    /// A drive's disc taken out.
    EjectDisc(PhysicalDrive),
    /// A disc put in a drive over what was there.
    InsertDisc(PhysicalDrive, Disc),
    /// The tape taken out.
    EjectTape,
    /// A tape put in over what was there.
    InsertTape(Tape),
    /// A snapshot restored over the drives and the deck.
    Restore,
}

/// Work on the files the media came from, done in the order given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileWork {
    /// `bytes` written over the file `medium` names.
    WriteBack {
        /// The file.
        medium: u32,
        /// Its new contents.
        bytes: Vec<u8>,
    },
    /// The file `medium` names let go.
    Forget(u32),
}

/// What a picker was opened for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chosen {
    /// A file to open.
    Open(Purpose),
    /// A place to save these bytes.
    Save(Vec<u8>),
}

/// The records of the media, the picker, and the saves waiting for it.
#[derive(Debug, Default)]
pub struct Rack {
    discs: [Option<Disc>; 2],
    tape: Option<Tape>,
    choosing: Option<Chosen>,
    saves: VecDeque<Saving>,
}

impl Rack {
    /// Nothing recorded, no picker open.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The disc recorded in `drive`.
    #[must_use]
    pub const fn disc(&self, drive: PhysicalDrive) -> Option<&Disc> {
        self.discs[drive.index()].as_ref()
    }

    /// The tape recorded in the deck.
    #[must_use]
    pub const fn tape(&self) -> Option<&Tape> {
        self.tape.as_ref()
    }

    /// The drives whose discs go back to a file as the machine writes them:
    /// the only ones a collect is to reach.
    #[must_use]
    pub fn filed(&self) -> [bool; 2] {
        self.discs
            .each_ref()
            .map(|disc| disc.as_ref().is_some_and(|disc| disc.medium.is_some()))
    }

    /// What the machine handed over for `handover`: each written disc back
    /// to its file or, taken out with none, offered to be saved; a recorded
    /// tape back to the UEF file it came from, else offered as one. The
    /// records then follow the handover, each file let go once its last
    /// write is asked for; the work is pushed onto `work` in order.
    pub fn handed(&mut self, media: &Media<'_>, handover: Handover, work: &mut Vec<FileWork>) {
        let taking_out = handover != Handover::Collect;
        for written in &media.discs {
            self.written_disc(written, taking_out, work);
        }
        let old_tape = if matches!(
            handover,
            Handover::EjectTape | Handover::InsertTape(_) | Handover::Restore
        ) {
            self.tape.take()
        } else {
            None
        };
        if let Some(image) = media.tape {
            self.recorded_tape(old_tape.as_ref(), image, work);
        }
        work.extend(old_tape.and_then(|tape| tape.medium).map(FileWork::Forget));
        match handover {
            Handover::EjectDisc(drive) => self.replace_disc(drive, None, work),
            Handover::InsertDisc(drive, disc) => self.replace_disc(drive, Some(disc), work),
            Handover::InsertTape(tape) => self.tape = Some(tape),
            Handover::Restore => {
                for drive in PhysicalDrive::ALL {
                    self.replace_disc(drive, None, work);
                }
            }
            Handover::Collect | Handover::EjectTape => {}
        }
    }

    fn written_disc(&mut self, written: &Written<'_>, taking_out: bool, work: &mut Vec<FileWork>) {
        let held = self.discs[written.drive.index()].as_mut();
        if let Some((disc, medium)) = held.and_then(|disc| disc.medium.map(|medium| (disc, medium)))
        {
            disc.image = written.image.to_vec();
            work.push(FileWork::WriteBack {
                medium,
                bytes: written.image.to_vec(),
            });
        } else if taking_out {
            let suggested = self.discs[written.drive.index()].as_ref().map_or_else(
                || String::from(unnamed_disc(written.format)),
                |disc| disc.name.clone(),
            );
            self.offer(Saving {
                suggested,
                holding: Holding::Disc,
                bytes: written.image.to_vec(),
            });
        }
    }

    fn recorded_tape(&mut self, old: Option<&Tape>, image: &[u8], work: &mut Vec<FileWork>) {
        let uef = old.and_then(|tape| {
            tape.medium
                .filter(|_| extension(&tape.name).is_some_and(|extension| extension == "uef"))
        });
        if let Some(medium) = uef {
            work.push(FileWork::WriteBack {
                medium,
                bytes: image.to_vec(),
            });
            return;
        }
        let stem = old.map_or("Tape", |tape| {
            tape.name
                .rsplit_once('.')
                .map_or(tape.name.as_str(), |(stem, _)| stem)
        });
        self.offer(Saving {
            suggested: format!("{stem}.uef"),
            holding: Holding::Tape,
            bytes: image.to_vec(),
        });
    }

    /// `drive` given `disc`, the file of the one it replaces let go unless
    /// it is the same file.
    fn replace_disc(&mut self, drive: PhysicalDrive, disc: Option<Disc>, work: &mut Vec<FileWork>) {
        let medium = disc.as_ref().and_then(|disc| disc.medium);
        let old = core::mem::replace(&mut self.discs[drive.index()], disc);
        work.extend(
            old.and_then(|disc| disc.medium)
                .filter(|&old| Some(old) != medium)
                .map(FileWork::Forget),
        );
    }

    /// The record of `disc`, which the machine already holds in `drive` from
    /// its snapshot, its file found unchanged: whether the drive had none
    /// recorded to take it.
    pub fn adopt_disc(&mut self, drive: PhysicalDrive, disc: Disc) -> bool {
        let slot = &mut self.discs[drive.index()];
        if slot.is_some() {
            return false;
        }
        *slot = Some(disc);
        true
    }

    /// The record of `tape`, which the machine already holds from its
    /// snapshot: whether the deck had none recorded to take it.
    pub fn adopt_tape(&mut self, tape: Tape) -> bool {
        if self.tape.is_some() {
            return false;
        }
        self.tape = Some(tape);
        true
    }

    /// `saving` waits for the picker, which shows one at a time, so no offer
    /// displaces another's bytes.
    pub fn offer(&mut self, saving: Saving) {
        self.saves.push_back(saving);
    }

    /// A picker opens for a file to open as `purpose`; refused while one is
    /// open.
    pub fn choose_to_open(&mut self, purpose: Purpose) -> bool {
        if self.choosing.is_some() {
            return false;
        }
        self.choosing = Some(Chosen::Open(purpose));
        true
    }

    /// The next save waiting, its picker now taken as open: the name to
    /// suggest and what it holds. `None` while a picker is open or none
    /// waits.
    pub fn choose_to_save(&mut self) -> Option<(String, Holding)> {
        if self.choosing.is_some() {
            return None;
        }
        let saving = self.saves.pop_front()?;
        self.choosing = Some(Chosen::Save(saving.bytes));
        Some((saving.suggested, saving.holding))
    }

    /// The picker closed: what it was opened for, a save's bytes with it.
    pub const fn chosen(&mut self) -> Option<Chosen> {
        self.choosing.take()
    }

    /// Whether no picker is open and no save waits for one.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.choosing.is_none() && self.saves.is_empty()
    }
}

/// The name a disc no file named is suggested under.
#[must_use]
pub const fn unnamed_disc(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Ssd => "Disc.ssd",
        ImageFormat::Dsd => "Disc.dsd",
    }
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;
