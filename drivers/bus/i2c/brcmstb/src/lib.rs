//! Broadcom set-top BSC I²C controller driver (`brcm,bcm2711-hdmi-i2c`): the
//! BCM2711 HDMI blocks' DDC buses, which carry a sink's EDID, its segment
//! pointer and SCDC.
//!
//! A command moves up to 32 bytes through eight data registers. It opens with
//! a START and the address unless it continues the message before it, and
//! closes with a STOP unless a further command continues it or a repeated
//! START follows, so every transaction shape is native
//! ([`RepeatedStart::Any`]). Every chunk after a message's first continues
//! it, so a message longer than one command is never re-addressed part way.
//!
//! The BCM2711's DDC controllers raise no interrupt the tree describes, so a
//! command's completion is read after sleeping its time on the wire, on a
//! one-shot timer rather than a spin, the step budget bounding a target that
//! stalls the bus. An HDMI block's automatic I²C engine owns the controller
//! until told to release it, which bring-up does first.
//!
//! Reference: VESA E-DDC. Broadcom publishes no description of the
//! controller; its register interface is held to the on-metal run
//! (`plans/PI.md`).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::num::NonZeroU32;
use core::ops::Range;

use tairix_abi::driver::i2c::{
    BusFacts, Direction, I2cFault, I2cMessage, I2cTransaction, RepeatedStart,
};
use tairix_abi::{
    compatible_key, CapabilityId, Delay, DriverBindKey, DriverError, DriverHandle, DriverHost,
    RegisterBlock,
};
use tairix_i2cbus::{step_budget_ns, wire_ns, I2cBus};

#[cfg(test)]
mod tests;

/// The capabilities the driver runs with, which its signed manifest requests:
/// the register windows, the node's endpoint, each channel's buffer, and the
/// log its records go to.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::SHM,
    CapabilityId::LOG_EMIT,
];

/// Per-driver `DriverHandle` marker returned by [`register`]. The bytes spell
/// `"DDC1"`.
const REGISTER_HANDLE_MARKER: u64 = 0x4444_4331_0000_0001;

/// Device-tree `compatible` string of a BCM2711 HDMI block's DDC controller.
pub const DDC_COMPATIBLE: &[u8] = b"brcm,bcm2711-hdmi-i2c";

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table, which both the bundle's signed manifest
/// and the autoload match are built from.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    compatible_key!(DDC_COMPATIBLE),
)];

/// The node's window holding the controller's registers.
pub const BSC_WINDOW: &[u8] = b"bsc";

/// The node's window holding the HDMI block's automatic I²C engine.
pub const AUTO_I2C_WINDOW: &[u8] = b"auto-i2c";

/// Bytes of register block the controller occupies (`CHIP_ADDRESS` through
/// `SCL_PARAM`).
pub const REGISTER_BLOCK_LEN: usize = 0x58;

const CHIP_ADDRESS: usize = 0x00;
const DATA_IN: usize = 0x04;
const CNT: usize = 0x24;
const CTL: usize = 0x28;
const IIC_ENABLE: usize = 0x2C;
const DATA_OUT: usize = 0x30;
const CTLHI: usize = 0x50;

const DATA_REGISTERS: usize = 8;
const DATA_REGISTER_LEN: usize = 4;

/// The most bytes one command moves.
const COMMAND_LEN: usize = DATA_REGISTERS * DATA_REGISTER_LEN;

const CTL_READ: u32 = 1 << 0;
const CTL_SCL_SEL_SHIFT: u32 = 4;
const CTL_DIV_CLK: u32 = 1 << 7;

const EN_ENABLE: u32 = 1 << 0;
const EN_INTRP: u32 = 1 << 1;
const EN_NOACK: u32 = 1 << 2;
const EN_NOSTOP: u32 = 1 << 4;
const EN_NOSTART: u32 = 1 << 5;
const EN_RESTART: u32 = 1 << 6;

const CTLHI_DATAREG_SIZE: u32 = 1 << 6;

/// The automatic engine's control register, and the bit that hands the
/// controller to software.
const AUTO_I2C_CTRL0: usize = 0x26C;
const AUTO_I2C_RELEASE_BSC: u32 = 1 << 1;

/// Bytes of the automatic engine's window bring-up reaches.
pub const AUTO_I2C_BLOCK_LEN: usize = AUTO_I2C_CTRL0 + 4;

/// The `SCL_SEL` field selecting one of the four base clocks.
const fn scl(select: u32) -> u32 {
    select << CTL_SCL_SEL_SHIFT
}

/// The bus clocks the controller makes and the control bits that select
/// each: four set by `SCL_SEL`, each halved by `DIV_CLK`.
const SPEEDS: [(u32, u32); 8] = [
    (375_000, scl(0)),
    (390_000, scl(1)),
    (187_500, scl(2)),
    (200_000, scl(3)),
    (93_750, scl(0) | CTL_DIV_CLK),
    (97_500, scl(1) | CTL_DIV_CLK),
    (46_875, scl(2) | CTL_DIV_CLK),
    (50_000, scl(3) | CTL_DIV_CLK),
];

/// The fastest bus clock the controller makes at or below `declared`, and
/// the control bits that select it.
fn speed(declared: NonZeroU32) -> Option<(u32, u32)> {
    SPEEDS
        .iter()
        .copied()
        .filter(|&(hz, _)| hz <= declared.get())
        .max_by_key(|&(hz, _)| hz)
}

/// The conditions that frame the chunk `chunk` of `message`, the last
/// message of its transaction or not: a START and the address unless it
/// continues the message, and a STOP unless the message continues or a
/// repeated START follows.
fn conditions(message: I2cMessage, chunk: &Range<usize>, last: bool) -> u32 {
    let span = message.span();
    let mut conditions = 0;
    if chunk.start != span.start {
        conditions |= EN_NOSTART;
    }
    if chunk.end != span.end {
        conditions |= EN_NOSTOP;
    } else if !last {
        conditions |= EN_RESTART | EN_NOSTOP;
    }
    conditions
}

/// A whole number of microseconds no shorter than `ns`.
fn micros(ns: u64) -> u32 {
    u32::try_from(ns.div_ceil(1_000).max(1)).unwrap_or(u32::MAX)
}

/// How a command ended.
enum Command {
    Done,
    NoAck,
    Stalled,
}

/// A BCM2711 HDMI block's DDC controller.
pub struct Ddc<'a, D: Delay> {
    regs: &'a dyn RegisterBlock,
    delay: D,
    speed_hz: u32,
    clock: u32,
}

impl<'a, D: Delay> Ddc<'a, D> {
    /// The controller behind `regs`, taken from the automatic engine behind
    /// `auto_i2c`, its bus run at the fastest clock it makes at or below
    /// `declared`, and waiting through `delay`.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a window shorter than the
    /// registers, [`DriverError::OutOfRange`] for a declared speed below
    /// every clock it makes, or the refusal of a register access.
    pub fn new(
        regs: &'a dyn RegisterBlock,
        auto_i2c: &dyn RegisterBlock,
        delay: D,
        declared: NonZeroU32,
    ) -> Result<Self, DriverError> {
        if regs.block_len() < REGISTER_BLOCK_LEN || auto_i2c.block_len() < AUTO_I2C_BLOCK_LEN {
            return Err(DriverError::LengthOutOfRange);
        }
        let (speed_hz, clock) = speed(declared).ok_or(DriverError::OutOfRange)?;
        auto_i2c.write32(AUTO_I2C_CTRL0, AUTO_I2C_RELEASE_BSC)?;
        regs.write32(IIC_ENABLE, 0)?;
        regs.write32(CTLHI, CTLHI_DATAREG_SIZE)?;
        regs.write32(CTL, clock)?;
        Ok(Self {
            regs,
            delay,
            speed_hz,
            clock,
        })
    }

    /// Leave the controller idle: no command enabled, no completion latched.
    fn idle(&self) -> Result<(), DriverError> {
        self.regs.write32(CNT, 0)?;
        self.regs.write32(IIC_ENABLE, 0)
    }

    /// Run one command moving the bytes of `chunk`, framed by `conditions`.
    fn command(
        &mut self,
        direction: Direction,
        chunk: Range<usize>,
        buffer: &mut [u8],
        conditions: u32,
    ) -> Result<Command, DriverError> {
        let bytes = buffer
            .get_mut(chunk.clone())
            .ok_or(DriverError::LengthOutOfRange)?;
        let read = direction == Direction::Read;
        self.regs
            .write32(CTL, self.clock | if read { CTL_READ } else { 0 })?;
        self.regs.write32(
            CNT,
            u32::try_from(bytes.len()).map_err(|_| DriverError::OutOfRange)?,
        )?;
        if !read {
            for (at, word) in bytes.chunks(DATA_REGISTER_LEN).enumerate() {
                let mut packed = [0u8; DATA_REGISTER_LEN];
                packed[..word.len()].copy_from_slice(word);
                self.regs
                    .write32(DATA_IN + at * DATA_REGISTER_LEN, u32::from_le_bytes(packed))?;
            }
        }
        self.regs.write32(IIC_ENABLE, conditions | EN_ENABLE)?;
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let speed = u64::from(self.speed_hz);
        let interval = micros(wire_ns(len, speed));
        let deadline = self
            .delay
            .now_us()
            .saturating_add(step_budget_ns(len, speed).div_ceil(1_000));
        let status = loop {
            self.delay.delay_us(interval);
            let status = self.regs.read32(IIC_ENABLE)?;
            if status & EN_INTRP != 0 {
                break status;
            }
            if self.delay.now_us() >= deadline {
                self.idle()?;
                return Ok(Command::Stalled);
            }
        };
        if status & EN_NOACK != 0 {
            self.idle()?;
            return Ok(Command::NoAck);
        }
        if read {
            for (at, word) in bytes.chunks_mut(DATA_REGISTER_LEN).enumerate() {
                let received = self.regs.read32(DATA_OUT + at * DATA_REGISTER_LEN)?;
                word.copy_from_slice(&received.to_le_bytes()[..word.len()]);
            }
        }
        self.idle()?;
        Ok(Command::Done)
    }

    fn run(
        &mut self,
        messages: &[I2cMessage],
        buffer: &mut [u8],
    ) -> Result<Result<(), I2cFault>, DriverError> {
        if messages
            .iter()
            .any(|message| message.span().end > buffer.len())
        {
            return Err(DriverError::LengthOutOfRange);
        }
        // The controller sends an address only with a byte after it.
        if messages.iter().any(|message| message.len == 0) {
            return Err(DriverError::Unsupported);
        }
        self.idle()?;
        let last = messages.len().saturating_sub(1);
        for (index, message) in messages.iter().enumerate() {
            let read = u8::from(message.direction == Direction::Read);
            self.regs
                .write32(CHIP_ADDRESS, u32::from((message.address.get() << 1) | read))?;
            let which = u8::try_from(index).unwrap_or(u8::MAX);
            let span = message.span();
            let mut at = span.start;
            while at < span.end {
                let chunk = at..span.end.min(at + COMMAND_LEN);
                let framing = conditions(*message, &chunk, index == last);
                match self.command(message.direction, chunk.clone(), buffer, framing)? {
                    Command::Done => at = chunk.end,
                    Command::NoAck => return Ok(Err(I2cFault::NoAck { message: which })),
                    Command::Stalled => return Ok(Err(I2cFault::TimedOut { message: which })),
                }
            }
        }
        Ok(Ok(()))
    }
}

impl<D: Delay> I2cBus for Ddc<'_, D> {
    fn facts(&self) -> BusFacts {
        BusFacts {
            speed_hz: self.speed_hz,
            repeated_start: RepeatedStart::Any,
        }
    }

    fn transfer(
        &mut self,
        transaction: &I2cTransaction,
        buffer: &mut [u8],
    ) -> Result<Result<(), I2cFault>, DriverError> {
        let outcome = self.run(transaction.messages(), buffer);
        if outcome.is_err() {
            // Whatever refused, the controller is left idle for the next
            // transaction; a register that refuses this too has nothing left
            // to say.
            let _ = self.idle();
        }
        outcome
    }
}

/// Driver entry point.
///
/// # Errors
///
/// [`DriverError::PermissionDenied`] if the host did not grant
/// [`CapabilityId::DRV_LOAD`].
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}
