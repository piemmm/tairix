//! What the engine reports as it works: the step it is starting, what the
//! controller and card answered, and how a wait on the controller ended when
//! it did not get what it wanted.
//!
//! QEMU models no EMMC2, so on metal this is the only view into a bring-up.
//! A host that records nothing keeps [`crate::SdhciHost::trace`]'s default.

use crate::bus::{Link, Rung};
use crate::card::SCR_BYTES;
use crate::BringUpStage;

/// How a wait on the controller ended without what it wanted.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WaitEnd {
    /// The controller raised an error status bit.
    Error,
    /// No interrupt arrived within the completion wait's budget.
    Silent,
    /// The poll budget ran out: the register never reached the state, or the
    /// interrupt kept firing without the wanted status.
    Budget,
}

impl WaitEnd {
    /// A stable, terse name, for the operator-facing trace.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error status",
            Self::Silent => "no interrupt",
            Self::Budget => "poll budget",
        }
    }
}

/// One report from the engine.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Trace {
    /// A negotiation attempt is starting at this rung.
    Attempt(Rung),
    /// A bring-up step is starting.
    Stage(BringUpStage),
    /// The controller's version, capability and maximum-current registers,
    /// read after a reset.
    Controller {
        /// `SLOTISR_VER`.
        version: u32,
        /// `CAPABILITIES`.
        caps: u32,
        /// `CAPABILITIES_1`.
        caps1: u32,
        /// `MAX_CURRENT`.
        max_current: u32,
    },
    /// The base clock the SD clock is divided from.
    BaseClock {
        /// Its rate, in Hz.
        hz: u32,
        /// The board supplied it, rather than the capabilities register.
        from_board: bool,
    },
    /// The SD clock was programmed.
    Clock {
        /// The rate asked for, in Hz.
        target_hz: u32,
        /// The frequency-select bits written to `CONTROL1`.
        select: u32,
        /// The rate they yield, in Hz.
        hz: u32,
    },
    /// A command is being issued.
    Command {
        /// Its index (`CMDn`).
        index: u8,
        /// Its argument.
        arg: u32,
    },
    /// A command completed.
    Response {
        /// Its index (`CMDn`).
        index: u8,
        /// Its first response word.
        response: u32,
    },
    /// A wait on a controller register ended without what it wanted.
    WaitFailed {
        /// The register's offset.
        register: usize,
        /// The bits waited for.
        wanted: u32,
        /// The register's last value.
        value: u32,
        /// Completion parks the wait took.
        waits: u32,
        /// Why it ended.
        end: WaitEnd,
    },
    /// The card's operating conditions register, once powered up.
    Ocr(u32),
    /// The card's SCR, as it sent it.
    Scr([u8; SCR_BYTES]),
    /// A switch-function status block.
    Switch {
        /// The access modes the card offers (`CMD6` group 1 bitmap).
        access_modes: u16,
        /// The current limits it offers (group 4 bitmap).
        current_limits: u16,
        /// The access mode it reports selected.
        access_mode: u8,
    },
    /// The ADMA2 staging the transfers will use.
    Dma {
        /// Device address of the data area.
        data: u64,
        /// Device address of the descriptor table.
        table: u64,
        /// Blocks moved per command.
        stage_blocks: usize,
    },
    /// The DMA verify read differed from the data port's.
    DmaMismatch {
        /// The first byte offset that differed.
        offset: usize,
        /// What the data port read there.
        port: u8,
        /// What landed in the staging.
        dma: u8,
    },
    /// Bring-up finished on this link.
    Ready(Link),
}
