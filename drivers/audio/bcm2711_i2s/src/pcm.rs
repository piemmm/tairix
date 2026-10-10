//! The PCM block's registers, and the framing a link's format makes of them
//! (BCM2711 ARM Peripherals, chapter 7; Linux `sound/soc/bcm/bcm2835-i2s.c`).
//!
//! The control registers may change only while the block neither transmits
//! nor receives, so a stream is framed with transmit off. The block takes
//! FIFO words for its two channels in turn, so every stream starts from a
//! cleared FIFO with transmit off: a word left over would swap the channels.

use tairix_abi::driver::codec::{DaiFormat, DaiLink, SampleWidths};
use tairix_abi::{DriverError, RegisterBlock};

const CS: usize = 0x00;
pub(crate) const MODE: usize = 0x08;
const RXC: usize = 0x0C;
const TXC: usize = 0x10;
const DREQ: usize = 0x14;
const INTEN: usize = 0x18;
const INTSTC: usize = 0x1C;

/// The FIFO's offset in the block, which the DMA channel writes.
pub const FIFO: u64 = 0x04;

/// Bytes of window the registers lie within.
pub const WINDOW_LEN: usize = 0x24;

const CS_EN: u32 = 1 << 0;
pub(crate) const CS_TXON: u32 = 1 << 2;
pub(crate) const CS_TXCLR: u32 = 1 << 3;
/// Transmit's FIFO threshold at "less than full", as Linux sets it.
const CS_TXTHR: u32 = 1 << 5;
const CS_DMAEN: u32 = 1 << 9;
/// The latched FIFO error, cleared by writing one.
const CS_TXERR: u32 = 1 << 15;
/// Echoes the value written two bit clocks later.
const CS_SYNC: u32 = 1 << 24;
/// Takes the FIFO RAMs out of standby.
const CS_STBY: u32 = 1 << 25;

const MODE_FLEN_SHIFT: u32 = 10;
const MODE_FSI: u32 = 1 << 20;
const MODE_FSM: u32 = 1 << 21;
const MODE_CLKI: u32 = 1 << 22;
const MODE_CLKM: u32 = 1 << 23;
const MODE_FTXP: u32 = 1 << 24;

const CH_WIDTH_EXTEND: u32 = 1 << 15;
const CH_ENABLE: u32 = 1 << 14;
const CH_POSITION_SHIFT: u32 = 4;
const CH1_SHIFT: u32 = 16;

/// The FIFO levels, in words, below which the DMA channel is asked for more
/// and then asked with priority, as Linux sets them.
const DREQ_TX: u32 = 0x30 << 8;
const DREQ_TX_PANIC: u32 = 0x10 << 24;

/// Every interrupt status bit, each cleared by writing one.
const INTSTC_ALL: u32 = 0xF;

/// Status reads a FIFO clear is given for the sync bit to echo, two bit
/// clocks: 7.8 µs at the slowest bit clock a stream runs, 8 kHz frames of two
/// 16-bit slots, which the budget outlasts at any read slower than 1.9 ns.
const SYNC_BUDGET: u32 = 4_096;

/// How the block frames a link's samples, each in a slot as wide as itself,
/// two slots a frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Framing {
    mode: u32,
    channels: u32,
    width: u8,
}

impl Framing {
    /// The framing of `link` for `width`-bit samples, on this, the link's CPU
    /// side.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] for a width `codec-v1` does not name.
    pub fn new(link: &DaiLink, width: u8) -> Result<Self, DriverError> {
        if !SampleWidths::ALL.contains(&width) {
            return Err(DriverError::Unsupported);
        }
        let slot = u32::from(width);
        // The bit clocks from the frame's start to the first channel's first
        // bit, the frame clock's length, and whether a frame starts on its
        // falling edge.
        let (delay, sync_length, falling) = match link.format {
            DaiFormat::I2s => (1, slot, true),
            DaiFormat::LeftJustified | DaiFormat::RightJustified => (0, slot, false),
            DaiFormat::DspA => (1, 1, false),
            DaiFormat::DspB => (0, 1, false),
        };
        let shape = CH_ENABLE | if width >= 24 { CH_WIDTH_EXTEND } else { 0 } | ((slot - 8) & 0xF);
        let channel = |position: u32| shape | position << CH_POSITION_SHIFT;
        let mut mode = (2 * slot - 1) << MODE_FLEN_SHIFT | sync_length;
        // The block's own sense samples on the falling edge, so the normal
        // clock is its inverted one.
        if !link.inversion.bit_clock() {
            mode |= MODE_CLKI;
        }
        if falling != link.inversion.frame_clock() {
            mode |= MODE_FSI;
        }
        if link.codec_drives_bit_clock {
            mode |= MODE_CLKM;
        }
        if link.codec_drives_frame_clock {
            mode |= MODE_FSM;
        }
        // Sixteen-bit samples travel two to a FIFO word, as a ring of them
        // holds them.
        if width == 16 {
            mode |= MODE_FTXP;
        }
        Ok(Self {
            mode,
            channels: channel(delay) << CH1_SHIFT | channel(slot + delay),
            width,
        })
    }

    /// Bit clocks a frame lasts.
    #[must_use]
    pub const fn frame_bits(&self) -> u32 {
        2 * self.width as u32
    }
}

/// The PCM block, reached through its register window.
pub struct Pcm<'r, R: RegisterBlock + ?Sized> {
    regs: &'r R,
    /// The control bits last written, the sync bit's among them.
    control: u32,
}

impl<'r, R: RegisterBlock + ?Sized> Pcm<'r, R> {
    /// The block behind `regs`.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a window short of the registers.
    pub fn new(regs: &'r R) -> Result<Self, DriverError> {
        if regs.block_len() < WINDOW_LEN {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok(Self { regs, control: 0 })
    }

    fn set_control(&mut self, bits: u32) -> Result<(), DriverError> {
        self.control = self.control & CS_SYNC | bits;
        self.regs.write32(CS, self.control)
    }

    /// Enable the block, its FIFO out of standby and transmit off, with its
    /// interrupts masked: the DMA channel's boundaries are the only events.
    ///
    /// # Errors
    ///
    /// A register access's failure.
    pub fn enable(&mut self) -> Result<(), DriverError> {
        // The sync bit reads back what was last written, so a clear's echo
        // is read against what is actually there.
        self.control = self.regs.read32(CS)? & CS_SYNC;
        self.regs.write32(INTEN, 0)?;
        self.regs.write32(INTSTC, INTSTC_ALL)?;
        self.set_control(CS_EN | CS_STBY)
    }

    /// Frame streams as `framing` states. Transmit is turned off first, as
    /// the control registers require.
    ///
    /// # Errors
    ///
    /// A register write's failure.
    pub fn frame(&mut self, framing: &Framing) -> Result<(), DriverError> {
        self.set_control(CS_EN | CS_STBY)?;
        self.regs.write32(MODE, framing.mode)?;
        self.regs.write32(RXC, 0)?;
        self.regs.write32(TXC, framing.channels)?;
        self.regs.write32(DREQ, DREQ_TX_PANIC | DREQ_TX)?;
        self.set_control(CS_EN | CS_STBY | CS_TXTHR | CS_DMAEN)
    }

    /// Turn transmit off and clear the FIFO, waiting the two bit clocks the
    /// clear takes.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] when no bit clock runs to complete it, or
    /// a register access's failure.
    pub fn clear(&mut self) -> Result<(), DriverError> {
        self.control &= !CS_TXON;
        self.control ^= CS_SYNC;
        self.regs.write32(CS, self.control | CS_TXCLR)?;
        for _ in 0..SYNC_BUDGET {
            if self.regs.read32(CS)? & CS_SYNC == self.control & CS_SYNC {
                return Ok(());
            }
        }
        Err(DriverError::DeviceFault)
    }

    /// Start or stop transmitting. Starting clears the latched FIFO error.
    ///
    /// # Errors
    ///
    /// A register write's failure.
    pub fn transmit(&mut self, on: bool) -> Result<(), DriverError> {
        if on {
            self.control |= CS_TXON;
            self.regs.write32(CS, self.control | CS_TXERR)
        } else {
            self.control &= !CS_TXON;
            self.regs.write32(CS, self.control)
        }
    }
}

#[cfg(test)]
pub(crate) mod model;

#[cfg(test)]
#[path = "pcm_tests.rs"]
mod tests;
