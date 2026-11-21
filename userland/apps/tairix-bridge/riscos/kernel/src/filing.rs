//! The filing vectors' defaults, which answer while no FileSwitch is: nothing
//! exists and no file is open. Each answer is the one genuine RISC OS gives
//! for a name it holds nothing under and a handle it has not opened
//! (`tests/corpus/no-filing-system.scenarios`); what only a filing system
//! could do — select or register one, read its names, set its options — is
//! refused as RISC OS refuses a filing system that is not present.
//!
//! The errors are FileSwitch's own, which the `filing` crate's FileSwitch
//! raises in turn.

use crate::env::{self, COMMAND_LINE};
use crate::error::Error;
use crate::memory::layout::APPLICATION;
use crate::os::{Fail, Os, Regs};
use crate::swi::{os, X};

/// "File '%0' not found".
pub const NOT_FOUND: Error = Error {
    number: 0xD6,
    token: "FileNotFound",
};
/// "Directory '%0' not found".
pub const NO_DIRECTORY: Error = Error {
    number: 0xD6,
    token: "DirectoryNotFound",
};
/// "'%0' is a file".
pub const IS_A_FILE: Error = Error {
    number: 0x41C,
    token: "ObjectIsAFile",
};
/// "'%0' is a directory".
pub const IS_A_DIRECTORY: Error = Error {
    number: 0xA8,
    token: "ObjectIsADirectory",
};
/// A handle no file is open on.
pub const NOT_OPEN: Error = Error {
    number: 0xDE,
    token: "HandleNotOpen",
};
/// No filing system to do what was asked.
pub const NOT_PRESENT: Error = Error {
    number: 0xF8,
    token: "NoFilingSystemPresent",
};
/// "Unable to start application".
pub const CANT_START: Error = Error {
    number: 0x600,
    token: "ApplicationStartRefused",
};
/// A reason `OS_FSControl` does not know.
pub const BAD_FS_CONTROL: Error = Error {
    number: 0x401,
    token: "UnknownFsControlReason",
};
/// A reason `OS_File` does not know.
pub const BAD_FILE: Error = Error {
    number: 0x402,
    token: "UnknownFileReason",
};
/// A reason `OS_Args` does not know.
pub const BAD_ARGS: Error = Error {
    number: 0x403,
    token: "UnknownArgsReason",
};
/// A reason `OS_GBPB` does not know.
pub const BAD_GBPB: Error = Error {
    number: 0x404,
    token: "UnknownGbpbReason",
};
/// A reason `OS_Find` does not know.
pub const BAD_FIND: Error = Error {
    number: 0x405,
    token: "UnknownFindReason",
};
/// What a filing system does not do.
pub const UNSUPPORTED: Error = Error {
    number: 0x40E,
    token: "NotSupported",
};

/// The longest name an error quotes.
const NAME_MOST: usize = 255;

/// `UpCall_NewApplication`.
const NEW_APPLICATION_UPCALL: u32 = 256;
/// The handlers a new application may find left in application space.
const CODE_HANDLERS: [u32; 12] = [
    env::UNDEFINED,
    env::PREFETCH_ABORT,
    env::DATA_ABORT,
    env::ADDRESS_EXCEPTION,
    env::ERROR,
    env::CALLBACK,
    env::BREAKPOINT,
    env::ESCAPE,
    env::EVENT,
    env::EXIT,
    env::UNUSED_SWI,
    env::UPCALL,
];

impl Os<'_> {
    /// The name at `address`, to its first control character, for an error.
    fn name_error(&mut self, error: Error, address: u32) -> Fail {
        match self.read_until(address, NAME_MOST, |b| b < b' ') {
            Ok((name, _)) => self.error_with(error, &[&name]),
            Err(fail) => fail,
        }
    }

    /// `OS_File` 19: the error FileSwitch gives for the object named at
    /// `name` being of `kind` — nothing, a file, a directory or an image, or
    /// a directory missing — and none for another kind.
    ///
    /// # Errors
    ///
    /// That error.
    pub fn object_error(&mut self, name: u32, kind: u32) -> Result<(), Fail> {
        match kind {
            0 => Err(self.name_error(NOT_FOUND, name)),
            1 => Err(self.name_error(IS_A_FILE, name)),
            2 | 3 => Err(self.name_error(IS_A_DIRECTORY, name)),
            0x100 => Err(self.name_error(NO_DIRECTORY, name)),
            _ => Ok(()),
        }
    }

    /// `FileV`'s default: every name names nothing.
    pub(crate) fn default_file(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        match regs.r[0] {
            5 | 6 | 13 | 15 | 17 | 20..=23 => {
                regs.r[0] = 0;
                Ok(())
            }
            19 => self.object_error(regs.r[1], regs.r[2]),
            0..=24 | 255 => Err(self.name_error(NOT_FOUND, regs.r[1])),
            _ => Err(self.error(BAD_FILE)),
        }
    }

    /// `FindV`'s default: no file to open, none open to close.
    pub(crate) fn default_find(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let mode = regs.r[0] & 0xFF;
        match mode {
            0 if regs.r[1] == 0 => Ok(()),
            0 => Err(self.error(NOT_OPEN)),
            1..=0x3F => Err(self.error(BAD_FIND)),
            _ if mode & os::find::INPUT != 0 && mode & os::find::MUST_EXIST == 0 => {
                regs.r[0] = 0;
                Ok(())
            }
            _ => Err(self.name_error(NOT_FOUND, regs.r[1])),
        }
    }

    /// `ArgsV`'s default: no file is open, and no filing system is current.
    pub(crate) fn default_args(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        match (regs.r[0], regs.r[1]) {
            (os::args::STATUS, _) => {
                regs.r[0] = os::args::status::UNALLOCATED;
                Ok(())
            }
            (0, 0) => {
                regs.r[0] = 0;
                Ok(())
            }
            (255, 0) => Ok(()),
            (0..=8 | 255, _) => Err(self.error(NOT_OPEN)),
            _ => Err(self.error(BAD_ARGS)),
        }
    }

    /// `BGetV`'s and `BPutV`'s default: no file is open.
    pub(crate) fn default_byte_stream(&mut self, _: &mut Regs) -> Result<(), Fail> {
        Err(self.error(NOT_OPEN))
    }

    /// `GBPBV`'s default: no file is open, no directory exists, and no
    /// filing system has names to read.
    pub(crate) fn default_gbpb(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        match regs.r[0] {
            1..=4 => Err(self.error(NOT_OPEN)),
            5..=8 => Err(self.error(NOT_PRESENT)),
            9..=12 => Err(self.name_error(NOT_FOUND, regs.r[1])),
            _ => Err(self.error(BAD_GBPB)),
        }
    }

    /// `FSCV`'s default: no prefix names a filing system, no filing system or
    /// module is current, no object exists, and no file is open, a handle
    /// converting to nought; a copy to no name at all is a bad string.
    pub(crate) fn default_fs_control(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        use crate::swi::os::fs_control::{
            BOOT, CLOSE_ALL, COPY, HANDLE, PREFIX, PRESENT, RESTORE, SECONDARY_MODULE,
            START_APPLICATION, TEMPORARY_MODULE,
        };
        match regs.r[0] {
            PREFIX => {
                regs.r[2] = u32::MAX;
                regs.r[3] = 0;
                Ok(())
            }
            PRESENT => {
                regs.r[2] = 0;
                Ok(())
            }
            TEMPORARY_MODULE | SECONDARY_MODULE | HANDLE => {
                regs.r[1] = 0;
                regs.r[2] = 0;
                Ok(())
            }
            START_APPLICATION => self.start_new_application(regs),
            BOOT | RESTORE | CLOSE_ALL => Ok(()),
            COPY if regs.r[2] == 0 => Err(self.error(crate::error::BAD_STRING)),
            0 | 1 | 4..=9 | 24..=28 | 32 | 47..=57 => Err(self.name_error(NOT_FOUND, regs.r[1])),
            3 | 29 | 34 => Err(self.error(BAD_FS_CONTROL)),
            0..=58 => Err(self.error(NOT_PRESENT)),
            _ => Err(self.error(BAD_FS_CONTROL)),
        }
    }
}

impl Os<'_> {
    /// `OS_FSControl` 2 as FileSwitch performs it: `UpCall_NewApplication`
    /// and then `Service_NewApplication` may refuse the new application;
    /// unless the exit handler is in application space, every handler left
    /// there is reset; then the current active object, the command line —
    /// the name, a space and the tail — and its start time are set.
    ///
    /// # Errors
    ///
    /// "Unable to start application" where the upcall or the service refuses.
    pub fn start_new_application(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (tail, object, name) = (regs.r[1], regs.r[2], regs.r[3]);
        let mut upcall = Regs::default();
        upcall.r[0] = NEW_APPLICATION_UPCALL;
        upcall.r[2] = object;
        self.perform(X | os::UPCALL, &mut upcall)?;
        if upcall.r[0] == 0 {
            return Err(self.error(CANT_START));
        }
        let mut service = Regs::default();
        service.r[1] = crate::service::NEW_APPLICATION;
        service.r[2] = object;
        self.service(&mut service)?;
        if service.r[1] == 0 {
            // The caller has given its handlers back to its parent, which is
            // where the refusal goes.
            return match self.error(CANT_START) {
                Fail::Error(block) => Ok(self.raise(block)?),
                unwound @ Fail::Unwind(_) => Err(unwound),
            };
        }
        let limit = self.world.env.slot(env::MEMORY_LIMIT).address;
        let in_application = |address: u32| (APPLICATION..limit).contains(&address);
        if !in_application(self.world.env.slot(env::EXIT).address) {
            for handler in CODE_HANDLERS {
                if in_application(self.world.env.slot(handler).address) {
                    self.world.env.reinstate(&[handler]);
                }
            }
        }
        self.world.env.set_active_object(object);
        let most = COMMAND_LINE as usize - 1;
        let (mut line, _) = self.read_until(name, most, |b| b <= b' ')?;
        let (tail, _) = self.read_until(tail, most, |b| b < b' ')?;
        if !tail.is_empty() {
            let room = most.saturating_sub(line.len() + 1).min(tail.len());
            if line.try_reserve(room + 1).is_err() {
                return Err(self.no_memory());
            }
            line.push(b' ');
            line.extend_from_slice(&tail[..room]);
        }
        let buffers = self.world.env.buffers;
        self.write_terminated(buffers.command, &line, 0, line.len() + 1)?;
        let now = self.real_time();
        self.write_five(buffers.start, now)
    }
}

pub mod fs;

#[cfg(test)]
mod tests;
