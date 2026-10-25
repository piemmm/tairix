//! Broadcom Serial Controller (BSC) I²C bus driver (`brcm,bcm2835-i2c`).
//!
//! The BSC is the I²C master every Raspberry Pi generation exposes: a 16-byte
//! FIFO, an address and a length register, and a status register whose bits
//! raise one interrupt. [`Bsc`] is the controller as the shared `i2c-v1`
//! endpoint drives it ([`I2cBus`]); the process, the endpoint and the grants
//! are the `Run` binary's.
//!
//! # A transaction, as the controller issues it
//!
//! The BSC has no repeated-START command. It issues one by chaining: a message
//! started while the one before it is active follows it with a repeated START
//! instead of a STOP (BCM2835 ARM Peripherals §3.3, the read procedure). The
//! chain must be in place before the message before it can end, and in a
//! process that may be preempted at any instruction the only guarantee of that
//! is to withhold the write's last byte until the next message is started: the
//! controller cannot finish a write it has not been given. So a write may
//! precede any message, and a read must end the transaction
//! ([`RepeatedStart::BeforeFinalRead`]).
//!
//! # Paced by the interrupt, bounded by progress
//!
//! The FIFO is refilled or drained as its threshold interrupts ask, and the
//! transfer parks on the interrupt in between. A transaction is abandoned, and
//! the controller reset, only once the bus has made no progress for a step
//! budget — a FIFO's worth of bytes at the bus's speed plus the longest stall
//! a target may hold it for. While the controller waits on this process it
//! holds the clock, so a late wake finds work, never a timeout.
//!
//! Its divider is set from the parent clock before every transaction, so the
//! bus runs at no more than its declared speed whatever the firmware has done
//! to that clock.
//!
//! Reference: BCM2835 ARM Peripherals §3 (BSC); NXP UM10204 Rev. 7.0 (timing).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::num::NonZeroU32;
use core::ops::Range;

use tairix_abi::driver::i2c::{
    BusFacts, Direction, I2cFault, I2cMessage, I2cTransaction, RepeatedStart,
};
use tairix_abi::{
    compatible_key, CapabilityId, DriverBindKey, DriverError, DriverHandle, DriverHost,
    RegisterBlock,
};
use tairix_i2cbus::{step_budget_ns, I2cBus};

#[cfg(test)]
mod tests;

/// The capabilities the driver runs with, which its signed manifest requests:
/// the register window, the interrupt, the node's endpoint, each channel's
/// buffer, the parent clock's link, and the log its records go to.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] = &[
    CapabilityId::MMIO_MAP,
    CapabilityId::IRQ_BIND,
    CapabilityId::IPC_BIND_PRIVILEGED,
    CapabilityId::IPC_ENDPOINT,
    CapabilityId::SHM,
    CapabilityId::LOG_EMIT,
];

/// Per-driver `DriverHandle` marker returned by [`register`]. The bytes spell
/// `"BSC1"`.
const REGISTER_HANDLE_MARKER: u64 = 0x4253_4331_0000_0001;

/// Device-tree `compatible` string of the BSC. Every Pi generation's
/// controller is register-compatible with the BCM2835's and carries it.
pub const BSC_COMPATIBLE: &[u8] = b"brcm,bcm2835-i2c";

/// The bind priority [`BIND_KEYS`] carries: an exact `compatible` match.
const BIND_PRIORITY: u16 = 10;

/// The driver's canonical bind table, which both the bundle's signed manifest
/// and the autoload match are built from.
pub const BIND_KEYS: &[DriverBindKey] = &[DriverBindKey::new(
    BIND_PRIORITY,
    compatible_key!(BSC_COMPATIBLE),
)];

/// Bytes of register block the controller occupies (`C` through `CLKT`).
pub const REGISTER_BLOCK_LEN: usize = 0x20;

const C: usize = 0x00;
const S: usize = 0x04;
const DLEN: usize = 0x08;
const A: usize = 0x0C;
const FIFO: usize = 0x10;
const DIV: usize = 0x14;
const DEL: usize = 0x18;
const CLKT: usize = 0x1C;

const C_I2CEN: u32 = 1 << 15;
const C_INTR: u32 = 1 << 10;
const C_INTT: u32 = 1 << 9;
const C_INTD: u32 = 1 << 8;
const C_ST: u32 = 1 << 7;
/// Clears both FIFOs (a two-bit field; either non-zero value clears).
const C_CLEAR: u32 = 0b01 << 4;
const C_READ: u32 = 1 << 0;

const S_CLKT: u32 = 1 << 9;
const S_ERR: u32 = 1 << 8;
const S_RXD: u32 = 1 << 5;
const S_TXD: u32 = 1 << 4;
const S_RXR: u32 = 1 << 3;
const S_TXW: u32 = 1 << 2;
const S_DONE: u32 = 1 << 1;

/// Every latched `S` bit, cleared by writing it back.
const S_LATCHED: u32 = S_CLKT | S_ERR | S_DONE;

/// The smallest divider whose edge delays stay below half of it, as `DEL`
/// requires (BCM2835 ARM Peripherals §3.2). The register reads the divider as
/// even, ignoring its low bit.
const CDIV_MIN: u32 = 0x0004;
const CDIV_MAX: u32 = 0xFFFE;

const FEDL_SHIFT: u32 = 16;

/// The FIFO's depth in bytes: what one step of progress moves at most.
const FIFO_DEPTH: u64 = 16;

/// How a transfer waits for the controller to make progress.
///
/// On metal this is the bound interrupt line and the kernel's monotonic
/// clock; in a host test, a simulated controller that steps as the driver
/// parks.
pub trait BusWait {
    /// Monotonic nanoseconds.
    fn now_ns(&self) -> u64;

    /// Park until the controller interrupts, or `timeout_ns` elapses.
    ///
    /// A refused park returns rather than spins: the caller re-reads the clock
    /// and fails closed once its budget is spent.
    fn wait(&self, timeout_ns: u64);
}

/// The clock the controller divides its bus clock from.
pub trait ParentClock {
    /// The rate it runs at now.
    ///
    /// # Errors
    ///
    /// A clock whose rate cannot be read.
    fn rate(&mut self) -> Result<u64, DriverError>;
}

/// The divider that runs the bus at no more than `speed_hz` from a parent at
/// `parent_hz`: rounded up, then up to even, which is how the register reads
/// it.
fn divider(parent_hz: u64, speed_hz: NonZeroU32) -> Result<u32, DriverError> {
    let divider = parent_hz.div_ceil(u64::from(speed_hz.get()));
    let even = divider.saturating_add(divider & 1);
    u32::try_from(even)
        .ok()
        .filter(|divider| (CDIV_MIN..=CDIV_MAX).contains(divider))
        .ok_or(DriverError::OutOfRange)
}

/// The edge delays for `divider`: data driven a sixteenth of a bit after the
/// falling edge, within UM10204's data-valid time in Standard and Fast mode,
/// and sampled a quarter after the rising, mid-way through SCL high — both
/// under the `CDIV / 2` the `DEL` register must stay below (BCM2835 ARM
/// Peripherals §3.2).
fn edge_delays(divider: u32) -> u32 {
    let falling = (divider / 16).max(1);
    let rising = (divider / 4).max(1);
    falling << FEDL_SHIFT | rising
}

/// The control word that starts `message`, interrupting on every condition
/// it can raise, completion among them so a refusal ends the wait.
const fn start_word(message: I2cMessage) -> u32 {
    let base = C_I2CEN | C_ST | C_INTD;
    match message.direction {
        Direction::Read => base | C_READ | C_INTR,
        Direction::Write if message.len != 0 => base | C_INTT,
        Direction::Write => base,
    }
}

/// One transaction in flight.
struct Run<'t> {
    messages: &'t [I2cMessage],
    /// The last message started.
    started: usize,
    /// The message whose bytes move next.
    cursor: usize,
    /// Where they are in the buffer.
    span: Range<usize>,
    /// The earliest message the controller has not been seen to finish: the
    /// one a refusal is reported against.
    active: usize,
}

impl<'t> Run<'t> {
    fn new(messages: &'t [I2cMessage]) -> Option<Self> {
        Some(Self {
            span: messages.first()?.span(),
            messages,
            started: 0,
            cursor: 0,
            active: 0,
        })
    }

    fn last(&self) -> usize {
        self.messages.len() - 1
    }
}

/// A message's index as a fault names it; a transaction holds at most eight.
fn index(message: usize) -> u8 {
    u8::try_from(message).unwrap_or(u8::MAX)
}

/// The Broadcom Serial Controller.
pub struct Bsc<'a, P: ParentClock> {
    regs: &'a dyn RegisterBlock,
    wait: &'a dyn BusWait,
    parent: P,
    speed_hz: NonZeroU32,
}

impl<'a, P: ParentClock> Bsc<'a, P> {
    /// The controller behind `regs`, run at no more than `speed_hz` divided
    /// from `parent`, and left idle with any state a previous owner left
    /// cleared.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a window shorter than the
    /// registers, or the refusal of a register access.
    pub fn new(
        regs: &'a dyn RegisterBlock,
        wait: &'a dyn BusWait,
        parent: P,
        speed_hz: NonZeroU32,
    ) -> Result<Self, DriverError> {
        if regs.block_len() < REGISTER_BLOCK_LEN {
            return Err(DriverError::LengthOutOfRange);
        }
        let bsc = Self {
            regs,
            wait,
            parent,
            speed_hz,
        };
        bsc.quiesce()?;
        Ok(bsc)
    }

    /// Stop any transfer, clear both FIFOs and every latched status bit, and
    /// leave the controller enabled, idle and interrupting on nothing.
    ///
    /// The control register has no abort: dropping the enable bit is the only
    /// way to stop a transfer in flight, so the controller is disabled,
    /// cleared and re-enabled rather than left wedged under the next caller.
    fn quiesce(&self) -> Result<(), DriverError> {
        self.regs.write32(C, C_CLEAR)?;
        self.regs.write32(S, S_LATCHED)?;
        self.regs.write32(C, C_I2CEN | C_CLEAR)
    }

    /// Set the divider for the parent's rate now, answering the bus clock it
    /// makes. A target the controller times out on its own would be reset
    /// without telling which message stalled, so its stretch timeout stays
    /// off and the step budget bounds every stall.
    fn clock(&mut self) -> Result<u64, DriverError> {
        let parent_hz = self.parent.rate()?;
        let divider = divider(parent_hz, self.speed_hz)?;
        self.regs.write32(DIV, divider)?;
        self.regs.write32(DEL, edge_delays(divider))?;
        self.regs.write32(CLKT, 0)?;
        Ok(parent_hz / u64::from(divider))
    }

    fn start(&self, message: I2cMessage) -> Result<(), DriverError> {
        self.regs.write32(A, u32::from(message.address.get()))?;
        self.regs.write32(DLEN, u32::from(message.len))?;
        self.regs.write32(C, start_word(message))
    }

    /// Hand the FIFO the cursor's write bytes it has room for, starting the
    /// next message before the last byte so the controller cannot end the
    /// write first. Answers whether anything moved.
    fn fill(&self, run: &mut Run<'_>, buffer: &[u8]) -> Result<bool, DriverError> {
        let mut moved = false;
        while !run.span.is_empty() && self.regs.read32(S)? & S_TXD != 0 {
            if run.span.len() == 1 && run.cursor < run.last() && run.started == run.cursor {
                run.started += 1;
                self.start(run.messages[run.started])?;
            }
            let byte = buffer
                .get(run.span.start)
                .ok_or(DriverError::LengthOutOfRange)?;
            self.regs.write32(FIFO, u32::from(*byte))?;
            run.span.start += 1;
            moved = true;
        }
        if run.span.is_empty() && run.cursor < run.last() {
            run.cursor += 1;
            run.span = run.messages[run.cursor].span();
        }
        Ok(moved)
    }

    /// Take the bytes the controller has received into the cursor's read
    /// span. Answers whether anything moved, or `None` for a byte the
    /// message has no room for.
    fn drain(&self, run: &mut Run<'_>, buffer: &mut [u8]) -> Result<Option<bool>, DriverError> {
        let mut moved = false;
        while self.regs.read32(S)? & S_RXD != 0 {
            if run.span.is_empty() {
                return Ok(None);
            }
            let slot = buffer
                .get_mut(run.span.start)
                .ok_or(DriverError::LengthOutOfRange)?;
            // The FIFO is byte-wide; its register's upper bits read as zero.
            *slot = self.regs.read32(FIFO)?.to_le_bytes()[0];
            run.span.start += 1;
            moved = true;
        }
        Ok(Some(moved))
    }

    /// End the transaction with `outcome`, the controller left idle.
    fn end(&self, outcome: Result<(), I2cFault>) -> Result<Result<(), I2cFault>, DriverError> {
        self.quiesce()?;
        Ok(outcome)
    }

    /// Judge a transaction the controller reports done: complete only if
    /// every message was started and every byte moved.
    fn finish(
        &self,
        run: &mut Run<'_>,
        buffer: &mut [u8],
    ) -> Result<Result<(), I2cFault>, DriverError> {
        let reading = run.messages[run.cursor].direction == Direction::Read;
        let drained = if reading {
            self.drain(run, buffer)?
        } else {
            Some(false)
        };
        let complete = drained.is_some()
            && run.started == run.last()
            && run.cursor == run.last()
            && run.span.is_empty();
        if complete {
            self.end(Ok(()))
        } else {
            self.end(Err(I2cFault::BusFault {
                message: index(run.active),
            }))
        }
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
        let Some(mut run) = Run::new(messages) else {
            return Err(DriverError::LengthOutOfRange);
        };
        let budget = step_budget_ns(FIFO_DEPTH, self.clock()?);
        self.regs.write32(S, S_LATCHED)?;
        self.regs.write32(C, C_I2CEN | C_CLEAR)?;
        self.start(messages[0])?;
        let mut deadline = self.wait.now_ns().saturating_add(budget);
        loop {
            let status = self.regs.read32(S)?;
            let message = index(run.active);
            if status & S_ERR != 0 {
                return self.end(Err(I2cFault::NoAck { message }));
            }
            if status & S_CLKT != 0 {
                return self.end(Err(I2cFault::BusFault { message }));
            }
            if status & S_DONE != 0 {
                return self.finish(&mut run, buffer);
            }
            let current = &messages[run.cursor];
            // Wanting bytes the message does not hold, or delivering them, is
            // the controller miscounting the message.
            let overrun = Err(I2cFault::BusFault {
                message: index(run.cursor),
            });
            let progressed = match current.direction {
                Direction::Write if status & S_TXW != 0 && current.len != 0 => {
                    if run.span.is_empty() {
                        return self.end(overrun);
                    }
                    run.active = run.cursor;
                    self.fill(&mut run, buffer)?
                }
                Direction::Read if status & (S_RXR | S_RXD) != 0 => {
                    run.active = run.cursor;
                    match self.drain(&mut run, buffer)? {
                        Some(moved) => moved,
                        None => return self.end(overrun),
                    }
                }
                _ => false,
            };
            let now = self.wait.now_ns();
            if progressed {
                deadline = now.saturating_add(budget);
                continue;
            }
            if now >= deadline {
                return self.end(Err(I2cFault::TimedOut {
                    message: index(run.active),
                }));
            }
            self.wait.wait(deadline - now);
        }
    }
}

impl<P: ParentClock> I2cBus for Bsc<'_, P> {
    fn facts(&self) -> BusFacts {
        BusFacts {
            speed_hz: self.speed_hz.get(),
            repeated_start: RepeatedStart::BeforeFinalRead,
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
            let _ = self.quiesce();
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
