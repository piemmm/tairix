//! The sink on the far side of the link, over the port's DDC bus: its EDID,
//! read a block at a time through E-DDC's segment pointer, and its SCDC
//! registers.

use tairix_abi::driver::i2c::{Direction, I2cAddress, I2cFault, I2cMessage, I2cTransaction};
use tairix_abi::{Delay, DriverError, Errno};
use tairix_edid::{block_count, Edid, BLOCK_LEN, MAX_BLOCKS};
use tairix_linkclient::{ChannelBuffer, I2cClient, LinkCall};

/// The DDC bus the sink is read over: transactions run with their bytes
/// staged in, and found in, the channel's buffer. In a driver it is the bus
/// controller's channel; a test answers it from a model sink.
pub trait Ddc {
    /// The channel's buffer.
    fn buffer(&mut self) -> &mut [u8];

    /// Run `transaction`.
    ///
    /// # Errors
    ///
    /// The bus's fault, a target that did not answer its address being
    /// [`DriverError::NotFound`], or the transport's refusal.
    fn run(&mut self, transaction: &I2cTransaction) -> Result<(), DriverError>;
}

impl<C: LinkCall, B: ChannelBuffer> Ddc for I2cClient<C, B> {
    fn buffer(&mut self) -> &mut [u8] {
        Self::buffer(self)
    }

    fn run(&mut self, transaction: &I2cTransaction) -> Result<(), DriverError> {
        Self::run(self, transaction)
            .map_err(DriverError::from_errno)?
            .map_err(I2cFault::as_driver_error)
    }
}

/// The EDID's target address.
const EDID: u8 = 0x50;
/// The E-DDC segment pointer: each segment holds two blocks.
const SEGMENT: u8 = 0x30;
/// The status and control data channel's target address.
const SCDC: u8 = 0x54;
/// SCDC's TMDS configuration register.
const TMDS_CONFIG: u8 = 0x20;
/// The TMDS clock runs at a quarter of the character rate.
const TMDS_HIGH_CLOCK_RATIO: u8 = 1 << 1;
const SCRAMBLING_ENABLE: u8 = 1;
/// SCDC's scrambler status register.
const SCRAMBLER_STATUS: u8 = 0x21;
/// The sink has seen the link scrambled.
const SCRAMBLING_DETECTED: u8 = 1;

/// Reads a block may take: a sink still powering up after asserting hotplug
/// does not answer at once.
const BLOCK_ATTEMPTS: u32 = 4;
/// The pause between them.
const RETRY_US: u32 = 10_000;

fn address(raw: u8) -> Result<I2cAddress, DriverError> {
    I2cAddress::new(raw).map_err(|_| DriverError::OutOfRange)
}

fn message(address: I2cAddress, direction: Direction, offset: u16, len: u16) -> I2cMessage {
    I2cMessage {
        address,
        direction,
        offset,
        len,
    }
}

/// Read EDID block `block` into `out`.
fn read_block(ddc: &mut impl Ddc, block: usize, out: &mut [u8]) -> Result<(), DriverError> {
    let (segment, offset) = (block / 2, (block % 2) * BLOCK_LEN);
    let edid = address(EDID)?;
    let buffer = ddc.buffer();
    buffer[0] = u8::try_from(segment).map_err(|_| DriverError::OutOfRange)?;
    buffer[1] = u8::try_from(offset).map_err(|_| DriverError::OutOfRange)?;
    let len = u16::try_from(BLOCK_LEN).map_err(|_| DriverError::OutOfRange)?;
    let read = message(edid, Direction::Read, 2, len);
    let transaction = if segment == 0 {
        I2cTransaction::new(&[message(edid, Direction::Write, 1, 1), read])
    } else {
        I2cTransaction::new(&[
            message(address(SEGMENT)?, Direction::Write, 0, 1),
            message(edid, Direction::Write, 1, 1),
            read,
        ])
    }
    .map_err(|_| DriverError::OutOfRange)?;
    ddc.run(&transaction)?;
    out.copy_from_slice(&ddc.buffer()[2..2 + BLOCK_LEN]);
    Ok(())
}

/// Read EDID block `block`, retrying a sink that does not answer yet.
fn read_block_retrying(
    ddc: &mut impl Ddc,
    delay: &dyn Delay,
    block: usize,
    out: &mut [u8],
) -> Result<(), DriverError> {
    let mut last = Err(DriverError::DeviceFault);
    for attempt in 0..BLOCK_ATTEMPTS {
        if attempt != 0 {
            delay.delay_us(RETRY_US);
        }
        last = read_block(ddc, block, out);
        if last.is_ok() {
            break;
        }
    }
    last
}

/// What the sink on `ddc` describes itself as, read into `scratch`: [`None`]
/// for a sink whose description cannot be read or is no EDID.
pub(crate) fn read_edid(
    ddc: &mut impl Ddc,
    delay: &dyn Delay,
    scratch: &mut [u8; MAX_BLOCKS * BLOCK_LEN],
) -> Option<Edid> {
    read_block_retrying(ddc, delay, 0, &mut scratch[..BLOCK_LEN]).ok()?;
    let mut blocks = 1;
    let mut declared = block_count(&scratch[..BLOCK_LEN]).ok()?;
    while blocks < declared {
        let range = blocks * BLOCK_LEN..(blocks + 1) * BLOCK_LEN;
        if read_block_retrying(ddc, delay, blocks, &mut scratch[range]).is_err() {
            break;
        }
        blocks += 1;
        // The first extension may override the count the base block states.
        if blocks == 2 {
            declared = block_count(&scratch[..2 * BLOCK_LEN]).ok()?;
        }
    }
    Edid::parse(&scratch[..blocks * BLOCK_LEN]).ok()
}

fn read_scdc(ddc: &mut impl Ddc, register: u8) -> Result<u8, DriverError> {
    let scdc = address(SCDC)?;
    ddc.buffer()[0] = register;
    let read = I2cTransaction::new(&[
        message(scdc, Direction::Write, 0, 1),
        message(scdc, Direction::Read, 1, 1),
    ])
    .map_err(|_| DriverError::OutOfRange)?;
    ddc.run(&read)?;
    Ok(ddc.buffer()[1])
}

/// Tell the sink whether the link is scrambled, its clock at a quarter of the
/// character rate as scrambling needs.
pub(crate) fn set_scrambling(ddc: &mut impl Ddc, scrambled: bool) -> Result<(), DriverError> {
    let config = read_scdc(ddc, TMDS_CONFIG)?;
    let flags = TMDS_HIGH_CLOCK_RATIO | SCRAMBLING_ENABLE;
    let buffer = ddc.buffer();
    buffer[0] = TMDS_CONFIG;
    buffer[1] = if scrambled {
        config | flags
    } else {
        config & !flags
    };
    let write = I2cTransaction::new(&[message(address(SCDC)?, Direction::Write, 0, 2)])
        .map_err(|_: Errno| DriverError::OutOfRange)?;
    ddc.run(&write)
}

/// Whether the sink's TMDS configuration still asks for scrambling: a sink
/// replugged has forgotten it.
pub(crate) fn scrambling_set(ddc: &mut impl Ddc) -> Result<bool, DriverError> {
    Ok(read_scdc(ddc, TMDS_CONFIG)? & SCRAMBLING_ENABLE != 0)
}

/// Whether the sink has seen the link scrambled.
pub(crate) fn scrambling_seen(ddc: &mut impl Ddc) -> Result<bool, DriverError> {
    Ok(read_scdc(ddc, SCRAMBLER_STATUS)? & SCRAMBLING_DETECTED != 0)
}
