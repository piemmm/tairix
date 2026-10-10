//! The controller's register map (Intel High Definition Audio Specification
//! 1.0a, section 3.3) and the window the engine reaches it through.

use tairix_abi::{DriverError, RegisterWindow};

/// Global capabilities: stream counts and 64-bit addressing.
pub const GCAP: usize = 0x00;
/// Global control.
pub const GCTL: usize = 0x08;
/// Codecs that asked for attention, one bit per SDIN line.
pub const STATESTS: usize = 0x0E;
/// Interrupt control.
pub const INTCTL: usize = 0x20;
/// Interrupt status.
pub const INTSTS: usize = 0x24;
/// Command ring lower and upper base.
pub const CORBLBASE: usize = 0x40;
pub const CORBUBASE: usize = 0x44;
/// Command ring write pointer.
pub const CORBWP: usize = 0x48;
/// Command ring read pointer, and its reset.
pub const CORBRP: usize = 0x4A;
/// Command ring control.
pub const CORBCTL: usize = 0x4C;
/// Command ring size and the sizes it supports.
pub const CORBSIZE: usize = 0x4E;
/// Response ring lower and upper base.
pub const RIRBLBASE: usize = 0x50;
pub const RIRBUBASE: usize = 0x54;
/// Response ring write pointer, and its reset.
pub const RIRBWP: usize = 0x58;
/// Responses per response interrupt.
pub const RINTCNT: usize = 0x5A;
/// Response ring control.
pub const RIRBCTL: usize = 0x5C;
/// Response ring status.
pub const RIRBSTS: usize = 0x5D;
/// Response ring size and the sizes it supports.
pub const RIRBSIZE: usize = 0x5E;
/// DMA position buffer lower base, with its enable.
pub const DPLBASE: usize = 0x70;
/// DMA position buffer upper base.
pub const DPUBASE: usize = 0x74;
/// The first stream descriptor.
pub const SD_BASE: usize = 0x80;
/// Bytes between stream descriptors.
pub const SD_STRIDE: usize = 0x20;

/// `GCAP`: output streams (bits 15:12), input streams (11:8), bidirectional
/// streams (7:3), 64-bit addressing (bit 0).
pub mod gcap {
    pub const OSS_SHIFT: u16 = 12;
    pub const ISS_SHIFT: u16 = 8;
    pub const BSS_SHIFT: u16 = 3;
    pub const BSS_MASK: u16 = 0x1F;
    pub const STREAMS_MASK: u16 = 0xF;
    pub const OK64: u16 = 1;
}

/// `GCTL`: the controller is out of reset; it accepts unsolicited responses.
pub mod gctl {
    pub const CRST: u32 = 1;
    pub const UNSOL: u32 = 1 << 8;
}

/// `INTCTL` and `INTSTS`: global and controller bits, one bit per stream
/// descriptor below them.
pub mod intr {
    pub const GLOBAL: u32 = 1 << 31;
    pub const CONTROLLER: u32 = 1 << 30;
    pub const STREAMS: u32 = (1 << 30) - 1;
}

/// `CORBRP` and `RIRBWP`: the pointer-reset bit.
pub const POINTER_RESET: u16 = 1 << 15;

/// `CORBCTL` and `RIRBCTL`: the ring's DMA runs.
pub const RING_RUN: u8 = 1 << 1;

/// `RIRBCTL`: interrupt on a response.
pub const RIRB_INTERRUPT: u8 = 1;
/// `RIRBCTL`: interrupt on an overrun.
pub const RIRB_OVERRUN_INTERRUPT: u8 = 1 << 2;

/// `RIRBSTS`: a response arrived; responses were lost to an overrun.
pub mod rirbsts {
    pub const RESPONSE: u8 = 1;
    pub const OVERRUN: u8 = 1 << 2;
}

/// `CORBSIZE` and `RIRBSIZE`: the entry counts the ring supports (bits 6:4,
/// for 256, 16 and 2 entries) and the encoding that selects one (bits 1:0).
pub mod ring_size {
    pub const CAP_256: u8 = 1 << 6;
    pub const CAP_16: u8 = 1 << 5;
    pub const CAP_2: u8 = 1 << 4;
    pub const SELECT_256: u8 = 0b10;
    pub const SELECT_16: u8 = 0b01;
    pub const SELECT_2: u8 = 0b00;
}

/// `DPLBASE`: the DMA position buffer is written.
pub const DMA_POSITION_ENABLE: u32 = 1;

/// One stream descriptor's registers, from its base.
pub mod sd {
    /// Control, three bytes: the low byte and the stream byte above it.
    pub const CTL: usize = 0x00;
    /// The control byte holding the stream number.
    pub const CTL_STREAM: usize = 0x02;
    pub const STS: usize = 0x03;
    pub const CBL: usize = 0x08;
    pub const LVI: usize = 0x0C;
    pub const FMT: usize = 0x12;
    pub const BDPL: usize = 0x18;
    pub const BDPU: usize = 0x1C;

    /// Control: reset, run, and the three interrupt enables.
    pub const SRST: u8 = 1;
    pub const RUN: u8 = 1 << 1;
    pub const IOCE: u8 = 1 << 2;
    pub const FEIE: u8 = 1 << 3;
    pub const DEIE: u8 = 1 << 4;

    /// The stream byte: the stream number in its top four bits; a
    /// bidirectional descriptor's direction (set for output) below it.
    pub const STREAM_SHIFT: u8 = 4;
    pub const BIDIRECTIONAL_OUTPUT: u8 = 1 << 3;

    /// Status: a buffer completed, the FIFO ran dry or over, a descriptor
    /// could not be fetched.
    pub const BCIS: u8 = 1 << 2;
    pub const FIFOE: u8 = 1 << 3;
    pub const DESE: u8 = 1 << 4;
    pub const STATUS_MASK: u8 = BCIS | FIFOE | DESE;
}

/// The controller's registers, as the engine reaches them.
pub trait Registers {
    /// Read a byte.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] outside the window.
    fn read8(&self, offset: usize) -> Result<u8, DriverError>;

    /// Read a half-word.
    ///
    /// # Errors
    ///
    /// As [`Self::read8`].
    fn read16(&self, offset: usize) -> Result<u16, DriverError>;

    /// Read a word.
    ///
    /// # Errors
    ///
    /// As [`Self::read8`].
    fn read32(&self, offset: usize) -> Result<u32, DriverError>;

    /// Write a byte.
    ///
    /// # Errors
    ///
    /// As [`Self::read8`].
    fn write8(&mut self, offset: usize, value: u8) -> Result<(), DriverError>;

    /// Write a half-word.
    ///
    /// # Errors
    ///
    /// As [`Self::read8`].
    fn write16(&mut self, offset: usize, value: u16) -> Result<(), DriverError>;

    /// Write a word.
    ///
    /// # Errors
    ///
    /// As [`Self::read8`].
    fn write32(&mut self, offset: usize, value: u32) -> Result<(), DriverError>;
}

impl Registers for RegisterWindow {
    fn read8(&self, offset: usize) -> Result<u8, DriverError> {
        self.read_u8(offset).map_err(|_| DriverError::DeviceFault)
    }

    fn read16(&self, offset: usize) -> Result<u16, DriverError> {
        self.read_u16(offset).map_err(|_| DriverError::DeviceFault)
    }

    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        self.read_u32(offset).map_err(|_| DriverError::DeviceFault)
    }

    fn write8(&mut self, offset: usize, value: u8) -> Result<(), DriverError> {
        self.write_u8(offset, value)
            .map_err(|_| DriverError::DeviceFault)
    }

    fn write16(&mut self, offset: usize, value: u16) -> Result<(), DriverError> {
        self.write_u16(offset, value)
            .map_err(|_| DriverError::DeviceFault)
    }

    fn write32(&mut self, offset: usize, value: u32) -> Result<(), DriverError> {
        self.write_u32(offset, value)
            .map_err(|_| DriverError::DeviceFault)
    }
}

/// The base of stream descriptor `index`.
#[must_use]
pub const fn descriptor(index: u8) -> usize {
    SD_BASE + index as usize * SD_STRIDE
}
