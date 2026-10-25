//! How the kernel opens the SD card for its root volume, as part of its
//! bootstrap floor: over the host it lends — the node's register window, DMA
//! inside the node's window, its interrupt line — and, for the bring-up
//! alone, the firmware's channel.
//!
//! The firmware owns the controller's base clock and switches the card's
//! supplies; discovery states which lines on the node ([`HwNode::io_rail`],
//! [`HwNode::power_rail`]), and the channel answers those lines alone. The
//! outcome is this driver's own record: the bus the bring-up settled on, or
//! the step it failed at.

use alloc::boxed::Box;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::mailbox::MailboxChannel;
use tairix_abi::driver::sole_register_window;
use tairix_abi::driver::supply::{IoRail, PowerRail};
use tairix_abi::driver::timing::{CompletionWait, Delay};
use tairix_abi::{DriverError, DriverHost, HwNode};
use tairix_log::{log, Event, EventId, Field, FieldValue, Level, Sink};
use tairix_vcmailbox::{
    query_clock_rate, set_gpio_state, ClockRateQuery, FirmwareClock, MailboxError,
};

use crate::{
    wiring, Board, BringUpFault, BringUpStage, CardSupply, Link, SignalVoltage, DMA_DATA_BYTES,
    DMA_TABLE_BYTES,
};

/// The DMA carves [`open`] asks its host for: the ADMA2 data staging and its
/// descriptor table.
pub const DMA_CARVES: &[usize] = &[DMA_DATA_BYTES, DMA_TABLE_BYTES];

/// Audit event: the bus the bring-up settled on, and what made it settle for
/// less.
pub const LINK_SETTLED: EventId = EventId(24_940);

/// Audit event: the bring-up failed, at the step and with the error the
/// record names.
pub const BRING_UP_FAILED: EventId = EventId(24_941);

/// Open the SD card `node` describes over `host`, parking on `wait` for the
/// controller's completions and timed intervals, and record the outcome on
/// `audit`.
///
/// The host's firmware channel is asked for the controller's base clock and
/// switches the card's supplies on the lines `node`'s rails name. Without a
/// channel the card runs on the capabilities' own base clock, and without
/// both rails it stays at 3.3 V.
///
/// # Errors
///
/// What the bring-up failed with — a node naming no single register window,
/// a host lacking [`CapabilityId::MMIO_MAP`](tairix_abi::CapabilityId::MMIO_MAP)
/// or a mapper, or a step the card or controller failed — each recorded
/// under [`BRING_UP_FAILED`] first.
///
/// # Capabilities
///
/// As [`wiring::open_discovered`].
pub fn open(
    host: &dyn DriverHost,
    node: &HwNode,
    wait: &'static dyn CompletionWait,
    audit: &dyn Sink,
) -> Result<Box<dyn Block>, DriverError> {
    let opened = sole_register_window(node.resources())
        .map_err(|error| BringUpFault {
            stage: BringUpStage::MapWindow,
            error,
        })
        .and_then(|(regs_phys, _)| {
            let channel = host.mailbox();
            let mut supply = channel.and_then(|channel| FirmwareSupply::new(channel, node, wait));
            let board = Board {
                base_clock_hz: channel.and_then(base_clock_hz),
                supply: supply.as_mut().map(|supply| supply as &mut dyn CardSupply),
            };
            wiring::open_discovered(host, regs_phys, wait, board)
        });
    match opened {
        Ok(device) => {
            note_link(audit, &device.link());
            Ok(Box::new(device))
        }
        Err(fault) => {
            note_failure(audit, fault);
            Err(fault.error)
        }
    }
}

/// The base clock the firmware feeds the controller, when it reports one: a
/// zero rate is how its interface spells none.
fn base_clock_hz(mut channel: &dyn MailboxChannel) -> Option<u32> {
    query_clock_rate(&mut channel, FirmwareClock::Emmc2, ClockRateQuery::Current)
        .ok()
        .filter(|&hz| hz != 0)
}

/// The card's supplies, switched through the firmware on the lines the
/// node's rails name, each switch waiting out its rail's declared timing:
/// the I/O rail's settling time, the power rail's start-up delay, and — on
/// the way back on — whatever remains of its off-on delay.
struct FirmwareSupply<'a, D: Delay + ?Sized> {
    channel: &'a dyn MailboxChannel,
    io: IoRail,
    power: PowerRail,
    delay: &'a D,
    /// When the power rail last went off, on `delay`'s clock.
    off_since_us: Option<u64>,
}

impl<'a, D: Delay + ?Sized> FirmwareSupply<'a, D> {
    /// The supplies `node` states, or `None` unless it states both rails: a
    /// card switched to 1.8 V comes back only by a power cycle.
    fn new(channel: &'a dyn MailboxChannel, node: &HwNode, delay: &'a D) -> Option<Self> {
        Some(Self {
            channel,
            io: node.io_rail()?,
            power: node.power_rail()?,
            delay,
            off_since_us: None,
        })
    }

    fn switch(&self, line: u8, high: bool) -> Result<(), DriverError> {
        let mut channel = self.channel;
        set_gpio_state(&mut channel, line, high).map_err(MailboxError::as_driver_error)
    }
}

impl<D: Delay + ?Sized> CardSupply for FirmwareSupply<'_, D> {
    fn set_signal_voltage(&mut self, voltage: SignalVoltage) -> Result<(), DriverError> {
        let level = match voltage {
            SignalVoltage::V3_3 => self.io.level_3v3,
            SignalVoltage::V1_8 => self.io.level_1v8,
        }
        .ok_or(DriverError::Unsupported)?;
        self.switch(self.io.line, level)?;
        self.delay.delay_us(self.io.settle_us);
        Ok(())
    }

    fn set_card_power(&mut self, on: bool) -> Result<(), DriverError> {
        if on {
            if let Some(off_since) = self.off_since_us {
                let held = self.delay.now_us().saturating_sub(off_since);
                let owed = u64::from(self.power.off_on_us).saturating_sub(held);
                self.delay.delay_us(u32::try_from(owed).unwrap_or(u32::MAX));
            }
        }
        self.switch(self.power.line, self.power.level(on))?;
        if on {
            self.off_since_us = None;
            self.delay.delay_us(self.power.startup_us);
        } else {
            self.off_since_us = Some(self.delay.now_us());
        }
        Ok(())
    }
}

/// Record the bus the bring-up settled on, and what made it settle for less.
fn note_link(audit: &dyn Sink, link: &Link) {
    let signal_mv: u64 = if link.mode.signals_at_1v8() {
        1800
    } else {
        3300
    };
    let [fallback_stage, fallback_error] = fault_fields(link.fallback);
    let [dma_fallback_stage, dma_fallback_error] = fault_fields(link.dma_fallback);
    log(
        audit,
        &Event {
            level: if link.fallback.is_some() || link.dma_fallback.is_some() {
                Level::Warn
            } else {
                Level::Info
            },
            id: LINK_SETTLED,
            message: "emmc2: link",
            fields: &[
                Field {
                    key: "mode",
                    value: FieldValue::Str(link.mode.as_str()),
                },
                Field {
                    key: "clock_hz",
                    value: FieldValue::UnsignedInt(link.clock_hz.into()),
                },
                Field {
                    key: "base_clock_hz",
                    value: FieldValue::UnsignedInt(link.base_clock_hz.into()),
                },
                Field {
                    key: "signal_mv",
                    value: FieldValue::UnsignedInt(signal_mv),
                },
                Field {
                    key: "cmd23",
                    value: FieldValue::Bool(link.counted_transfers),
                },
                Field {
                    key: "dma",
                    value: FieldValue::Bool(link.dma),
                },
                Field {
                    key: "fallback_stage",
                    value: fallback_stage,
                },
                Field {
                    key: "fallback_error",
                    value: fallback_error,
                },
                Field {
                    key: "dma_fallback_stage",
                    value: dma_fallback_stage,
                },
                Field {
                    key: "dma_fallback_error",
                    value: dma_fallback_error,
                },
            ],
        },
    );
}

/// Record the step the bring-up failed at and how: on metal the only signal
/// that localises it.
fn note_failure(audit: &dyn Sink, fault: BringUpFault) {
    let [stage, error] = fault_fields(Some(fault));
    log(
        audit,
        &Event {
            level: Level::Error,
            id: BRING_UP_FAILED,
            message: "emmc2: bring-up failed",
            fields: &[
                Field {
                    key: "stage",
                    value: stage,
                },
                Field {
                    key: "error",
                    value: error,
                },
            ],
        },
    );
}

/// A fault's step and error as field values, both null for none.
fn fault_fields(fault: Option<BringUpFault>) -> [FieldValue<'static>; 2] {
    match fault {
        Some(fault) => [
            FieldValue::Str(fault.stage.as_str()),
            FieldValue::Error(fault.error.as_errno()),
        ],
        None => [FieldValue::Null, FieldValue::Null],
    }
}

#[cfg(test)]
#[path = "floor_tests.rs"]
mod tests;
