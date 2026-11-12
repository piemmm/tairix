//! Device requests as DOS makes them: the request header, the attribute
//! word, and the routine that calls a driver through its header — its
//! strategy entry with ES:BX at the request, then its interrupt entry.

use super::data::level;
use super::engine::{locals, Far, Flow, Id, Kernel, Outcome, Routine};
use crate::asm::Reg16;
use crate::cpu::Seg;

/// Request header fields.
pub mod field {
    /// The header's length.
    pub const LENGTH: u16 = 0x00;
    /// The unit within the driver.
    pub const UNIT: u16 = 0x01;
    /// The command.
    pub const COMMAND: u16 = 0x02;
    /// The status the driver sets.
    pub const STATUS: u16 = 0x03;
    /// A block request's media descriptor; `INIT`'s units.
    pub const MEDIA: u16 = 0x0D;
    /// A transfer's address; `INIT`'s end of memory; a media check's answer
    /// at its first byte.
    pub const ADDRESS: u16 = 0x0E;
    /// A transfer's count; `INIT`'s command line, then its BPB array.
    pub const COUNT: u16 = 0x12;
    /// A block transfer's first sector, `FFFFh` to read [`SECTOR32`].
    pub const SECTOR: u16 = 0x14;
    /// A block transfer's first sector, 32 bits.
    pub const SECTOR32: u16 = 0x1A;
    /// `INIT`'s first drive.
    pub const FIRST_DRIVE: u16 = 0x16;
    /// A generic IOCTL's category.
    pub const CATEGORY: u16 = 0x0D;
    /// Its function.
    pub const FUNCTION: u16 = 0x0E;
    /// Its parameter block.
    pub const PACKET: u16 = 0x13;
}

/// The commands.
pub mod command {
    #![allow(missing_docs, reason = "the commands' own names")]
    pub const INIT: u8 = 0x00;
    pub const MEDIA_CHECK: u8 = 0x01;
    pub const BUILD_BPB: u8 = 0x02;
    pub const IOCTL_INPUT: u8 = 0x03;
    pub const INPUT: u8 = 0x04;
    pub const PEEK: u8 = 0x05;
    pub const INPUT_STATUS: u8 = 0x06;
    pub const INPUT_FLUSH: u8 = 0x07;
    pub const OUTPUT: u8 = 0x08;
    pub const OUTPUT_VERIFY: u8 = 0x09;
    pub const OUTPUT_STATUS: u8 = 0x0A;
    pub const OUTPUT_FLUSH: u8 = 0x0B;
    pub const IOCTL_OUTPUT: u8 = 0x0C;
    pub const OPEN: u8 = 0x0D;
    pub const CLOSE: u8 = 0x0E;
    pub const REMOVABLE: u8 = 0x0F;
    pub const OUTPUT_UNTIL_BUSY: u8 = 0x10;
    pub const GENERIC_IOCTL: u8 = 0x13;
    pub const GET_LOGICAL: u8 = 0x17;
    pub const SET_LOGICAL: u8 = 0x18;
    pub const IOCTL_QUERY: u8 = 0x19;
}

/// Bytes of the header each command takes.
#[must_use]
pub const fn length(command: u8) -> u8 {
    match command {
        command::INIT => 0x19,
        command::MEDIA_CHECK => 0x13,
        command::BUILD_BPB => 0x16,
        command::IOCTL_INPUT
        | command::INPUT
        | command::OUTPUT
        | command::OUTPUT_VERIFY
        | command::IOCTL_OUTPUT
        | command::OUTPUT_UNTIL_BUSY => 0x1E,
        command::PEEK => 0x0E,
        command::GENERIC_IOCTL | command::IOCTL_QUERY => 0x17,
        _ => 0x0D,
    }
}

/// The status word's bits.
pub mod status {
    /// The request failed; the low byte says why.
    pub const ERROR: u16 = 0x8000;
    /// The device is busy, or a peek found nothing.
    pub const BUSY: u16 = 0x0200;
    /// The request is done.
    pub const DONE: u16 = 0x0100;
}

/// The driver error codes, which DOS offsets by `13h` to extended errors.
pub mod error {
    #![allow(missing_docs, reason = "the codes' own names")]
    pub const WRITE_PROTECT: u8 = 0x00;
    pub const UNKNOWN_UNIT: u8 = 0x01;
    pub const NOT_READY: u8 = 0x02;
    pub const UNKNOWN_COMMAND: u8 = 0x03;
    pub const CRC: u8 = 0x04;
    pub const SEEK: u8 = 0x06;
    pub const UNKNOWN_MEDIA: u8 = 0x07;
    pub const NOT_FOUND: u8 = 0x08;
    pub const PAPER: u8 = 0x09;
    pub const WRITE_FAULT: u8 = 0x0A;
    pub const READ_FAULT: u8 = 0x0B;
    pub const GENERAL: u8 = 0x0C;
    pub const DISK_CHANGE: u8 = 0x0F;
}

/// Generic IOCTL's disk and console categories, and the functions each
/// takes.
pub mod generic {
    pub const DISK: u8 = 0x08;
    pub const SET_PARAMETERS: u8 = 0x40;
    pub const WRITE_TRACK: u8 = 0x41;
    pub const FORMAT_TRACK: u8 = 0x42;
    pub const SET_MEDIA_ID: u8 = 0x46;
    pub const SET_ACCESS: u8 = 0x47;
    pub const GET_PARAMETERS: u8 = 0x60;
    pub const READ_TRACK: u8 = 0x61;
    pub const VERIFY_TRACK: u8 = 0x62;
    pub const GET_MEDIA_ID: u8 = 0x66;
    pub const GET_ACCESS: u8 = 0x67;

    /// A device parameter block's fields, and the special-function bits Get
    /// and Set Device Parameters take.
    pub mod parameters {
        pub const SPECIAL: u16 = 0x00;
        pub const KIND: u16 = 0x01;
        pub const ATTRIBUTES: u16 = 0x02;
        pub const CYLINDERS: u16 = 0x04;
        pub const MEDIA_TYPE: u16 = 0x06;
        pub const BPB: u16 = 0x07;
        pub const LAYOUT_ENTRIES: u16 = 0x26;
        /// The track layout: each sector's number and size, a word each,
        /// as many as a track's CHS address counts.
        pub const LAYOUT: u16 = 0x28;
        pub const LAYOUT_MAX: u16 = tairix_partition::mbr::MAX_SECTORS as u16;
        /// A block with the longest layout.
        pub const LEN: u16 = LAYOUT + 4 * LAYOUT_MAX;
        /// Get's: the medium's BPB is asked for, not the drive's.
        pub const MEDIUM_BPB: u8 = 0x01;
        /// Set's: the BPB is the medium's, for a program formatting it; only
        /// the layout is set; the layout is every sector of a track, alike.
        pub const FAKE_BPB: u8 = 0x01;
        pub const LAYOUT_ONLY: u8 = 0x02;
        pub const GOOD_LAYOUT: u8 = 0x04;
        /// The attributes: a fixed disk, a change line.
        pub const NON_REMOVABLE: u16 = 0x0001;
        pub const CHANGE_LINE: u16 = 0x0002;

        /// The device types: a 5¼-inch drive of double or high density, a
        /// 3½-inch double-density one, a fixed disk, and any other — here a
        /// 1.44 MiB drive.
        pub mod kind {
            pub const DD525: u8 = 0x00;
            pub const HD525: u8 = 0x01;
            pub const DD35: u8 = 0x02;
            pub const FIXED: u8 = 0x05;
            pub const OTHER: u8 = 0x07;
        }
    }

    /// A track request's fields — a read or write's, and a format or verify's
    /// count of tracks — and its special-function bits.
    pub mod track {
        pub const SPECIAL: u16 = 0x00;
        pub const HEAD: u16 = 0x01;
        pub const CYLINDER: u16 = 0x03;
        pub const FIRST: u16 = 0x05;
        pub const COUNT: u16 = 0x07;
        pub const BUFFER: u16 = 0x09;
        pub const TRACKS: u16 = 0x05;
        /// A format's: whether the BIOS formats the medium is all it asks.
        pub const STATUS: u8 = 0x01;
        /// A format's or verify's: several tracks.
        pub const SEVERAL: u8 = 0x02;

        /// What a status call answers in the special-function byte.
        pub mod status {
            pub const SUPPORTED: u8 = 0;
            pub const NO_ROM: u8 = 1;
            pub const UNKNOWN_MEDIUM: u8 = 2;
            pub const EMPTY: u8 = 3;
        }
    }

    /// A media-ID record's fields, and its length.
    pub mod media {
        pub const SERIAL: u16 = 0x02;
        pub const LABEL: u16 = 0x06;
        pub const FILE_SYSTEM: u16 = 0x11;
        pub const LEN: u16 = 0x19;
    }

    /// An access-flag record's flag, clear for a partition holding no volume
    /// DOS can read, and its length.
    pub mod access {
        pub const FLAG: u16 = 0x01;
        pub const LEN: u16 = 0x02;
    }

    /// The console's category, and its display information's functions.
    pub const DISPLAY: u8 = 0x03;
    pub const SET_DISPLAY: u8 = 0x5F;
    pub const GET_DISPLAY: u8 = 0x7F;

    /// A display-information record's fields: its level, the length of what
    /// follows that, the blink flag, text or graphics, the colours, the
    /// picture's pixels and its character cells. Lengths unknown for a mode
    /// are `FFFFh`.
    pub mod display {
        pub const LEVEL: u16 = 0x00;
        pub const LENGTH: u16 = 0x02;
        pub const FLAGS: u16 = 0x04;
        pub const MODE: u16 = 0x06;
        pub const COLOURS: u16 = 0x08;
        pub const WIDTH: u16 = 0x0A;
        pub const HEIGHT: u16 = 0x0C;
        pub const COLUMNS: u16 = 0x0E;
        pub const ROWS: u16 = 0x10;
        pub const LEN: u16 = 0x12;
        /// What [`LENGTH`] holds.
        pub const DATA: u16 = LEN - FLAGS;
        /// [`FLAGS`]'s one bit: the attribute's top bit is intensity, not
        /// blink.
        pub const INTENSITY: u16 = 0x0001;
        /// [`MODE`]'s two.
        pub const TEXT: u8 = 1;
        pub const GRAPHICS: u8 = 2;
        /// A console's refusals: a record it cannot take, a mode the
        /// hardware cannot show, a font it does not have.
        pub const INVALID: u8 = super::super::error::UNKNOWN_UNIT;
        pub const NOT_SUPPORTED: u8 = super::super::error::WRITE_FAULT;
        pub const FONT_MISSING: u8 = super::super::error::GENERAL;
    }
}

/// The attribute word's bits.
pub mod attribute {
    /// A character device.
    pub const CHARACTER: u16 = 0x8000;
    /// It takes IOCTL reads and writes.
    pub const IOCTL: u16 = 0x4000;
    /// A character device's output-until-busy; a block device's non-IBM
    /// format.
    pub const UNTIL_BUSY: u16 = 0x2000;
    /// It takes open, close and removable-media requests.
    pub const OPEN_CLOSE: u16 = 0x0800;
    /// It takes IOCTL queries.
    pub const QUERY: u16 = 0x0080;
    /// It takes generic IOCTLs and logical-drive requests.
    pub const GENERIC: u16 = 0x0040;
    /// The console written through INT 29h.
    pub const FAST: u16 = 0x0010;
    /// The clock.
    pub const CLOCK: u16 = 0x0008;
    /// `NUL`.
    pub const NUL: u16 = 0x0004;
    /// A character device's standard output; a block device's 32-bit
    /// sectors.
    pub const STDOUT: u16 = 0x0002;
    /// The standard input.
    pub const STDIN: u16 = 0x0001;
}

/// The console's name, which a driver that replaces it takes too.
pub const CON: &[u8; 8] = b"CON     ";

/// A device header's fields.
pub mod header {
    /// The next header in the chain; `FFFFh` in its offset ends it.
    pub const NEXT: u16 = 0x00;
    /// The attribute word.
    pub const ATTRIBUTES: u16 = 0x04;
    /// The strategy entry's offset in the header's segment.
    pub const STRATEGY: u16 = 0x06;
    /// The interrupt entry's.
    pub const INTERRUPT: u16 = 0x08;
    /// A character device's name; a block device's unit count at its
    /// first byte.
    pub const NAME: u16 = 0x0A;
}

locals! {
    /// Calls the driver whose header lies at `header` with the running
    /// call's request: its strategy entry, then its interrupt entry. Returns
    /// the status the driver left.
    pub struct Device {
        header: Far,
    }
}

impl Routine for Device {
    const ID: Id = Id::Device;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        let entry = |k: &mut Kernel<'_, '_>, field: u16, header: Far| {
            Far::new(header.segment, k.read16_at(header.add(field)))
        };
        match *phase {
            0 => {
                let request = k.scratch(level::REQUEST);
                let strategy = entry(k, header::STRATEGY, self.header);
                k.g.regs_mut().set_sreg(Seg::Es, super::data::SEGMENT);
                k.g.set_r16(Reg16::Bx, request);
                k.call_far(strategy);
                *phase = 1;
                Flow::Wait
            }
            1 => {
                let interrupt = entry(k, header::INTERRUPT, self.header);
                k.call_far(interrupt);
                *phase = 2;
                Flow::Wait
            }
            _ => {
                let at = k.scratch(level::REQUEST + field::STATUS);
                Flow::Return(Ok(u32::from(k.get16(at))))
            }
        }
    }
}

impl Kernel<'_, '_> {
    /// Clears the running call's request and fills in its head: the
    /// header's length for `command`, the unit and the command.
    pub fn request(&mut self, unit: u8, command: u8) {
        let at = self.scratch(level::REQUEST);
        self.g.write_bytes(super::data::SEGMENT, at, &[0; 0x20]);
        self.set8(at + field::LENGTH, length(command));
        self.set8(at + field::UNIT, unit);
        self.set8(at + field::COMMAND, command);
    }

    /// A byte of the running call's request.
    pub fn request8(&mut self, field: u16) -> u8 {
        let at = self.scratch(level::REQUEST + field);
        self.get8(at)
    }

    /// Sets a byte of the running call's request.
    pub fn set_request8(&mut self, field: u16, value: u8) {
        let at = self.scratch(level::REQUEST + field);
        self.set8(at, value);
    }

    /// A word of the running call's request.
    pub fn request16(&mut self, field: u16) -> u16 {
        let at = self.scratch(level::REQUEST + field);
        self.get16(at)
    }

    /// Sets a word of the running call's request.
    pub fn set_request16(&mut self, field: u16, value: u16) {
        let at = self.scratch(level::REQUEST + field);
        self.set16(at, value);
    }

    /// A far pointer in the running call's request.
    pub fn request_far(&mut self, field: u16) -> Far {
        let at = self.scratch(level::REQUEST + field);
        self.get_far(at)
    }

    /// Sets a far pointer in the running call's request.
    pub fn set_request_far(&mut self, field: u16, pointer: Far) {
        let at = self.scratch(level::REQUEST + field);
        self.set_far(at, pointer);
    }

    /// Sets a double word in the running call's request.
    pub fn set_request32(&mut self, field: u16, value: u32) {
        let at = self.scratch(level::REQUEST + field);
        self.set32(at, value);
    }

    /// The attribute word of the device whose header is at `header`.
    pub fn attributes(&mut self, header: Far) -> u16 {
        self.read16_at(header.add(header::ATTRIBUTES))
    }
}

/// The extended error a driver's status word stands for.
#[must_use]
pub const fn extended(status: u16) -> u16 {
    (status & 0xFF) + 0x13
}

/// The driver's error code an extended error from [`DEVICE_ERRORS`] stands
/// for.
#[must_use]
pub const fn driver_code(extended_error: u16) -> u16 {
    extended_error.saturating_sub(extended(0))
}

/// The extended errors drivers' errors stand for, as calls return them.
pub mod failed {
    use super::{error, extended};
    pub const WRITE_PROTECT: u16 = extended(error::WRITE_PROTECT as u16);
    pub const NOT_READY: u16 = extended(error::NOT_READY as u16);
    pub const CRC: u16 = extended(error::CRC as u16);
    pub const UNKNOWN_MEDIA: u16 = extended(error::UNKNOWN_MEDIA as u16);
    pub const NOT_FOUND: u16 = extended(error::NOT_FOUND as u16);
    pub const WRITE_FAULT: u16 = extended(error::WRITE_FAULT as u16);
    pub const READ_FAULT: u16 = extended(error::READ_FAULT as u16);
    pub const GENERAL: u16 = extended(error::GENERAL as u16);
}

/// The extended errors a driver's status can stand for: the ones a
/// critical error is raised for.
pub const DEVICE_ERRORS: core::ops::RangeInclusive<u16> =
    extended(0)..=extended(error::DISK_CHANGE as u16);
