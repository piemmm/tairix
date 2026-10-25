//! The BCM2835-family GPIO block's registers (BCM2711 ARM Peripherals §5.2,
//! BCM2835 ARM Peripherals §6.1): function selects, output set and clear,
//! event-detect status, and the detect enables.

use tairix_abi::driver::gpio::HeaderLines;
use tairix_abi::{DriverError, RegisterBlock};

/// Bytes of window the registers below reach: the last asynchronous-falling
/// enable ends at `0x90`.
pub const WINDOW_LEN: usize = 0x90;

/// The BCM2711 block's fifty-eight lines.
pub const LINES: u8 = 58;

const FSEL: usize = 0x00;
const SET: usize = 0x1C;
const CLR: usize = 0x28;
const EDS: usize = 0x40;
const REN: usize = 0x4C;
const FEN: usize = 0x58;
const HEN: usize = 0x64;
const LEN: usize = 0x70;
const AREN: usize = 0x7C;
const AFEN: usize = 0x88;
/// Every detect enable, each a pair of bank registers.
const DETECTS: [usize; 6] = [REN, FEN, HEN, LEN, AREN, AFEN];

const LINES_PER_FSEL: u8 = 10;
const FSEL_BITS: u8 = 3;
const FSEL_MASK: u32 = 0b111;
const FSEL_INPUT: u32 = 0b000;
const FSEL_OUTPUT: u32 = 0b001;
const LINES_PER_BANK: u8 = 32;
/// The banks the lines fill, each register pair's halves.
const BANKS: usize = LINES.div_ceil(LINES_PER_BANK) as usize;
/// A bit for every line.
const LINE_MASK: u64 = u64::MAX >> (u64::BITS - LINES as u32);

/// What a line's function select routes it to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Function {
    /// An input.
    Input,
    /// An output.
    Output,
    /// A peripheral: alternate function 0–5.
    Alternate(u8),
}

impl Function {
    const fn from_bits(bits: u32) -> Self {
        // The select's encoding of the six alternates is not in order.
        match bits {
            FSEL_INPUT => Self::Input,
            FSEL_OUTPUT => Self::Output,
            0b100 => Self::Alternate(0),
            0b101 => Self::Alternate(1),
            0b110 => Self::Alternate(2),
            0b111 => Self::Alternate(3),
            0b011 => Self::Alternate(4),
            _ => Self::Alternate(5),
        }
    }
}

/// What a line's event detection fires on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Trigger {
    /// A rising edge.
    Rising,
    /// A falling edge.
    Falling,
    /// Either edge.
    Both,
    /// A high level.
    High,
    /// A low level.
    Low,
}

impl Trigger {
    /// The trigger a devicetree interrupt specifier's type cell names
    /// (`IRQ_TYPE_*`), or [`None`] for none the block detects.
    #[must_use]
    pub const fn from_cell(cell: u32) -> Option<Self> {
        match cell {
            1 => Some(Self::Rising),
            2 => Some(Self::Falling),
            3 => Some(Self::Both),
            4 => Some(Self::High),
            8 => Some(Self::Low),
            _ => None,
        }
    }

    /// Whether the trigger is a level, which holds its status set while the
    /// level stands.
    #[must_use]
    pub const fn is_level(self) -> bool {
        matches!(self, Self::High | Self::Low)
    }

    /// The synchronous detect enables it sets.
    const fn enables(self) -> &'static [usize] {
        match self {
            Self::Rising => &[REN],
            Self::Falling => &[FEN],
            Self::Both => &[REN, FEN],
            Self::High => &[HEN],
            Self::Low => &[LEN],
        }
    }
}

/// One controller's registers.
pub struct Block<'r> {
    regs: &'r dyn RegisterBlock,
}

impl<'r> Block<'r> {
    /// The block behind `regs`.
    #[must_use]
    pub fn new(regs: &'r dyn RegisterBlock) -> Self {
        Self { regs }
    }

    /// Disable every line's event detection and clear every latched event,
    /// so nothing a predecessor left armed fires before its consumer waits.
    ///
    /// # Errors
    ///
    /// The register access that failed.
    pub fn quiesce(&self) -> Result<(), DriverError> {
        for bank in 0..BANKS {
            for detect in DETECTS {
                self.regs.write32(detect + bank * 4, 0)?;
            }
            self.regs.write32(EDS + bank * 4, u32::MAX)?;
        }
        Ok(())
    }

    /// What `line` is routed to.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for a line the block does not have, or the
    /// register access that failed.
    pub fn function(&self, line: u8) -> Result<Function, DriverError> {
        let (offset, shift) = fsel(line)?;
        Ok(Function::from_bits(
            (self.regs.read32(offset)? >> shift) & FSEL_MASK,
        ))
    }

    /// What each line of `lines` is routed to, lowest first, each function
    /// select holding one of them read once. An item fails as
    /// [`Block::function`] does.
    pub fn functions(
        &self,
        lines: HeaderLines,
    ) -> impl Iterator<Item = Result<(u8, Function), DriverError>> + '_ {
        let mut select: Option<(usize, u32)> = None;
        lines.iter().map(move |line| {
            let (offset, shift) = fsel(line)?;
            let value = match select {
                Some((read, value)) if read == offset => value,
                _ => {
                    let value = self.regs.read32(offset)?;
                    select = Some((offset, value));
                    value
                }
            };
            Ok((line, Function::from_bits((value >> shift) & FSEL_MASK)))
        })
    }

    /// Route `line` as an input (`output` false) or an output.
    ///
    /// # Errors
    ///
    /// As [`Block::function`].
    pub fn set_direction(&self, line: u8, output: bool) -> Result<(), DriverError> {
        let (offset, shift) = fsel(line)?;
        let bits = if output { FSEL_OUTPUT } else { FSEL_INPUT };
        let current = self.regs.read32(offset)?;
        self.regs
            .write32(offset, (current & !(FSEL_MASK << shift)) | bits << shift)
    }

    /// Drive `line`'s output latch `high` or low. It reaches the pin once
    /// the line is an output.
    ///
    /// # Errors
    ///
    /// As [`Block::function`].
    pub fn drive(&self, line: u8, high: bool) -> Result<(), DriverError> {
        let (bank, bit) = bank_bit(line)?;
        let register = if high { SET } else { CLR };
        self.regs.write32(register + bank * 4, bit)
    }

    /// Enable `trigger`'s detection on `line` (`armed`) or disable it,
    /// reading and writing only the enables the trigger sets.
    ///
    /// # Errors
    ///
    /// As [`Block::function`].
    pub fn detect(&self, line: u8, trigger: Trigger, armed: bool) -> Result<(), DriverError> {
        let (bank, bit) = bank_bit(line)?;
        for &detect in trigger.enables() {
            let offset = detect + bank * 4;
            let current = self.regs.read32(offset)?;
            let next = if armed { current | bit } else { current & !bit };
            if next != current {
                self.regs.write32(offset, next)?;
            }
        }
        Ok(())
    }

    /// Every line whose event has latched, a bit per line.
    ///
    /// # Errors
    ///
    /// The register access that failed.
    pub fn events(&self) -> Result<u64, DriverError> {
        let mut latched = 0;
        for bank in 0..BANKS {
            latched |= u64::from(self.regs.read32(EDS + bank * 4)?) << (bank * 32);
        }
        Ok(latched & LINE_MASK)
    }

    /// Clear the latched events of every line `mask` sets.
    ///
    /// # Errors
    ///
    /// The register access that failed.
    pub fn clear_events(&self, mask: u64) -> Result<(), DriverError> {
        for bank in 0..BANKS {
            let bits = (mask >> (bank * 32)) & u64::from(u32::MAX);
            if bits != 0 {
                let bits = u32::try_from(bits).map_err(|_| DriverError::OutOfRange)?;
                self.regs.write32(EDS + bank * 4, bits)?;
            }
        }
        Ok(())
    }
}

/// The function select register holding `line` and its field's shift.
fn fsel(line: u8) -> Result<(usize, u32), DriverError> {
    let line = within(line)?;
    let register = usize::from(line / LINES_PER_FSEL);
    let shift = u32::from((line % LINES_PER_FSEL) * FSEL_BITS);
    Ok((FSEL + register * 4, shift))
}

/// The bank `line` is in and its bit there.
fn bank_bit(line: u8) -> Result<(usize, u32), DriverError> {
    let line = within(line)?;
    Ok((
        usize::from(line / LINES_PER_BANK),
        1 << (line % LINES_PER_BANK),
    ))
}

/// `line`, refused unless the block has it.
fn within(line: u8) -> Result<u8, DriverError> {
    if line < LINES {
        Ok(line)
    } else {
        Err(DriverError::OutOfRange)
    }
}
