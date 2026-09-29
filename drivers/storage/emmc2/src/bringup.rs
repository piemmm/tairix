//! Card bring-up: identification and bus negotiation.
//!
//! The sequence is the SD Physical Layer Simplified Specification's
//! initialization (`CMD0`, `CMD8`, `ACMD41`, the `CMD11` voltage switch,
//! `CMD2`, `CMD3`), then selection and the speed steps: `ACMD51` for the
//! card's capabilities, `ACMD6` for the 4-bit bus, `CMD6` for the access mode
//! and current limit, the host's matching timing and clock, and a read at
//! that timing to prove it. The controller side follows the SD Host
//! Controller Simplified Specification's signal-voltage-switch and
//! clock-change procedures.

use tairix_abi::DriverError;

use crate::bus::{self, BusMode, Link, Rung};
use crate::card::{self, SCR_BYTES, SWITCH_STATUS_BYTES};
use crate::command::{self, BLOCK_SIZE};
use crate::host::{self, HostCaps};
use crate::trace::Trace;
use crate::{
    adma, regs, Board, BringUpFault, BringUpStage, CardSupply, Emmc2, SdhciHost, SignalVoltage,
    MAX_BLOCKS_PER_COMMAND,
};

/// The SD clock during identification: the SD specification's 400 kHz
/// ceiling for it.
const IDENT_CLOCK_HZ: u32 = 400_000;

/// The SD clock every card takes once selected, before any switch.
const DEFAULT_CLOCK_HZ: u32 = 25_000_000;

/// Data-timeout-counter value (`CONTROL1[19:16]`): the controller's longest.
/// The completion wait's own bound fails a silent transfer long before.
const DATA_TIMEOUT_VALUE: u32 = 0x0E;

/// How long the SD clock runs before the first command: the SD
/// specification's 1 ms, which covers its 74-clock minimum at 400 kHz.
const CLOCK_SETTLE_US: u32 = 1_000;

/// `ACMD41` rounds, one [`OP_COND_INTERVAL_US`] apart: the SD
/// specification's one second for a card to finish powering up.
const OP_COND_ROUNDS: u32 = 100;

/// The pause between `ACMD41` rounds.
const OP_COND_INTERVAL_US: u32 = 10_000;

/// After `CMD11`'s response, how long the card is given to pull its data
/// lines low: Linux's 1 ms.
const SWITCH_CARD_BUSY_US: u32 = 1_000;

/// How long the SD clock stays gated across the switch to 1.8 V: the
/// specification's 5 ms, doubled as Linux does for cards that need longer.
const SWITCH_CLOCK_GATE_US: u32 = 10_000;

/// How long the card's power stays off in a power cycle: the SD
/// specification's 1 ms below 0.5 V, with room for a rail that discharges
/// slowly and a board that declares no off-on delay of its own.
const POWER_OFF_HOLD_US: u32 = 10_000;

/// How long the card's power is given to ramp before the controller drives
/// it: Linux's power-up delay for a supply that declares none.
const POWER_ON_SETTLE_US: u32 = 10_000;

/// The fixed facts and board a bring-up works with.
struct Bench<'b> {
    caps: HostCaps,
    base_clock_hz: u32,
    supply: Option<&'b mut dyn CardSupply>,
}

/// How an attempt failed, which decides what the ladder does next.
enum Setback {
    /// The card cannot be used at any speed.
    Fatal(BringUpFault),
    /// The card never answered identification; a power cycle may revive it.
    Unanswered(BringUpFault),
    /// A step the rung added failed; a lower rung may still work.
    /// `left_3v3` says the card left 3.3 V signalling, which only a power
    /// cycle undoes.
    Speed { fault: BringUpFault, left_3v3: bool },
}

/// Which part of an attempt a failed step belongs to.
#[derive(Copy, Clone)]
enum Part {
    /// Identification before the card has answered.
    Identify,
    /// A step every speed needs.
    Card,
    /// A step the rung added.
    Speed,
}

/// How a failure in `stage` sets the ladder back.
fn setback(stage: BringUpStage, part: Part, at_1v8: bool, error: DriverError) -> Setback {
    let fault = BringUpFault::new(stage, error);
    match part {
        // Once the card left 3.3 V any failure may be the switch's doing, and
        // a power cycle back to 3.3 V may still work.
        _ if at_1v8 => Setback::Speed {
            fault,
            left_3v3: true,
        },
        Part::Identify if error == DriverError::DeviceFault => Setback::Unanswered(fault),
        Part::Speed => Setback::Speed {
            fault,
            left_3v3: false,
        },
        Part::Identify | Part::Card => Setback::Fatal(fault),
    }
}

/// What a successful attempt settled on.
struct Negotiated {
    mode: BusMode,
    clock_hz: u32,
    counted_transfers: bool,
}

impl<H: SdhciHost> Emmc2<H> {
    /// Reset the controller, negotiate the fastest bus the card, controller
    /// and board all drive, and select DMA when the host grants staging.
    pub(crate) fn init(&mut self, board: Board<'_>) -> Result<Link, BringUpFault> {
        let reset = |e| BringUpFault::new(BringUpStage::ResetClock, e);
        let caps = self.reset_controller().map_err(reset)?;
        let base_clock_hz = board
            .base_clock_hz
            .or(caps.base_clock_hz())
            .ok_or(reset(DriverError::Unsupported))?;
        self.host.trace(Trace::BaseClock {
            hz: base_clock_hz,
            from_board: board.base_clock_hz.is_some(),
        });
        let mut bench = Bench {
            caps,
            base_clock_hz,
            supply: board.supply,
        };
        let mut fallback = None;
        // A card powers up signalling at 3.3 V, and a supply that cannot
        // select it could not undo a switch to 1.8 V either: no UHS-I on it.
        if bench.supply.is_some() {
            self.host
                .trace(Trace::Stage(BringUpStage::InitialSignalling));
        }
        let selected = bench
            .supply
            .as_deref_mut()
            .map(|supply| supply.set_signal_voltage(SignalVoltage::V3_3));
        if let Some(Err(error)) = selected {
            fallback = Some(BringUpFault::new(BringUpStage::InitialSignalling, error));
            bench.supply = None;
        }
        self.power_and_clock(&bench).map_err(reset)?;

        let mut rung = if caps.uhs() && bench.supply.is_some() {
            Rung::Uhs
        } else {
            Rung::HighSpeed
        };
        let mut revived = false;
        let negotiated = loop {
            match self.attempt(&mut bench, rung) {
                Ok(negotiated) => break negotiated,
                Err(Setback::Fatal(fault)) => return Err(fault),
                Err(Setback::Unanswered(fault)) => {
                    if revived || bench.supply.is_none() {
                        return Err(fault);
                    }
                    revived = true;
                    fallback.get_or_insert(fault);
                    self.power_cycle(&mut bench)?;
                }
                Err(Setback::Speed { fault, left_3v3 }) => {
                    let Some(lower) = rung.below() else {
                        return Err(fault);
                    };
                    fallback.get_or_insert(fault);
                    if left_3v3 {
                        self.power_cycle(&mut bench)?;
                    } else {
                        self.reset_controller().map_err(reset)?;
                        self.power_and_clock(&bench).map_err(reset)?;
                    }
                    rung = lower;
                }
            }
        };

        let dma_fallback = self
            .select_dma(&bench.caps)
            .map_err(|e| BringUpFault::new(BringUpStage::SelectDma, e))?;
        let link = Link {
            mode: negotiated.mode,
            clock_hz: negotiated.clock_hz,
            base_clock_hz,
            counted_transfers: negotiated.counted_transfers,
            dma: self.dma_stage_blocks != 0,
            fallback,
            dma_fallback,
        };
        self.host.trace(Trace::Ready(link));
        Ok(link)
    }

    /// One pass from `CMD0` to a verified bus, reaching no faster than
    /// `rung`.
    fn attempt(&mut self, bench: &mut Bench<'_>, rung: Rung) -> Result<Negotiated, Setback> {
        self.host.trace(Trace::Attempt(rung));
        self.rca = 0;
        self.card_state_unknown = false;
        self.step(BringUpStage::GoIdle, Part::Identify, false, |dev| {
            dev.issue(command::GO_IDLE_STATE, 0, 0).map(drop)
        })?;
        self.step(
            BringUpStage::SendIfCond,
            Part::Identify,
            false,
            Self::send_if_cond,
        )?;
        let ask_1v8 = rung == Rung::Uhs;
        let max_performance = bench.caps.max_current_330_ma() > 150;
        let ocr = self.step(BringUpStage::OpCond, Part::Identify, false, |dev| {
            dev.op_cond(ask_1v8, max_performance)
        })?;
        let mut at_1v8 = false;
        if ask_1v8 && ocr & command::OCR_S18 != 0 {
            let Some(supply) = bench.supply.as_deref_mut() else {
                return Err(setback(
                    BringUpStage::VoltageSwitch,
                    Part::Speed,
                    false,
                    DriverError::Unsupported,
                ));
            };
            // The card has begun its switch the moment CMD11 is answered, so
            // any failure from here leaves it off 3.3 V.
            at_1v8 = true;
            self.step(BringUpStage::VoltageSwitch, Part::Speed, at_1v8, |dev| {
                dev.switch_to_1v8(supply)
            })?;
        }

        self.identify(at_1v8)?;
        self.step(BringUpStage::RaiseClock, Part::Card, at_1v8, |dev| {
            dev.set_sd_clock(bench, DEFAULT_CLOCK_HZ).map(drop)
        })?;

        let scr = if rung == Rung::DefaultSpeed {
            None
        } else {
            let scr = self.step(BringUpStage::SendScr, Part::Speed, at_1v8, |dev| {
                let scr = dev.read_scr()?;
                if scr.four_bit {
                    Ok(scr)
                } else {
                    Err(DriverError::Unsupported)
                }
            })?;
            Some(scr)
        };
        self.step(
            BringUpStage::SetBusWidth,
            Part::Card,
            at_1v8,
            Self::set_bus_width_4bit,
        )?;

        let mode = match scr {
            Some(scr) if scr.switch_function => {
                self.step(BringUpStage::SwitchFunction, Part::Speed, at_1v8, |dev| {
                    dev.switch_mode(bench, at_1v8)
                })?
            }
            _ if at_1v8 => BusMode::Sdr12,
            _ => BusMode::DefaultSpeed,
        };
        let clock_hz = self.step(BringUpStage::SetBusTiming, Part::Speed, at_1v8, |dev| {
            dev.set_bus_timing(bench, mode)
        })?;
        self.step(
            BringUpStage::VerifyBus,
            Part::Speed,
            at_1v8,
            Self::verify_bus,
        )?;
        Ok(Negotiated {
            mode,
            clock_hz,
            counted_transfers: scr.is_some_and(|scr| scr.set_block_count),
        })
    }

    /// Run one bring-up step, reporting it as it starts, and map its failure
    /// to the setback its `part` and the card's signalling make it.
    fn step<T>(
        &mut self,
        stage: BringUpStage,
        part: Part,
        at_1v8: bool,
        run: impl FnOnce(&mut Self) -> Result<T, DriverError>,
    ) -> Result<T, Setback> {
        self.host.trace(Trace::Stage(stage));
        run(self).map_err(|error| setback(stage, part, at_1v8, error))
    }

    /// Reset the whole controller and read what it can do.
    fn reset_controller(&mut self) -> Result<HostCaps, DriverError> {
        self.host.trace(Trace::Stage(BringUpStage::ResetClock));
        self.host
            .write32(regs::REG_CONTROL1, regs::CONTROL1_SRST_HC)?;
        self.wait_clear(regs::REG_CONTROL1, regs::CONTROL1_SRST_HC)?;
        let version = self.host.read32(regs::REG_SLOTISR_VER)?;
        let caps = self.host.read32(regs::REG_CAPABILITIES)?;
        let caps1 = self.host.read32(regs::REG_CAPABILITIES_1)?;
        let max_current = self.host.read32(regs::REG_MAX_CURRENT)?;
        self.host.trace(Trace::Controller {
            version,
            caps,
            caps1,
            max_current,
        });
        Ok(HostCaps::decode(version, caps, caps1, max_current))
    }

    /// Power the bus at 3.3 V, start the identification clock, arm the
    /// interrupt sources the engine parks on, and let the clock run before
    /// the first command.
    ///
    /// The controller reset clears SD Bus Power, and the register block
    /// gates every command on it, so it is set before anything is issued.
    fn power_and_clock(&mut self, bench: &Bench<'_>) -> Result<(), DriverError> {
        self.host.write32(
            regs::REG_CONTROL0,
            regs::CONTROL0_BUS_VOLTAGE_3V3 | regs::CONTROL0_BUS_POWER,
        )?;
        self.set_sd_clock(bench, IDENT_CLOCK_HZ)?;
        self.host.write32(regs::REG_IRPT_MASK, regs::INT_ALL)?;
        self.host
            .write32(regs::REG_IRPT_EN, regs::INT_SIGNAL_ENABLE)?;
        self.host.delay_us(CLOCK_SETTLE_US);
        Ok(())
    }

    /// Stop or start the SD clock, leaving the divider as it is.
    fn sd_clock_enable(&mut self, on: bool) -> Result<(), DriverError> {
        let control1 = self.host.read32(regs::REG_CONTROL1)?;
        let control1 = if on {
            control1 | regs::CONTROL1_CLK_EN
        } else {
            control1 & !regs::CONTROL1_CLK_EN
        };
        self.host.write32(regs::REG_CONTROL1, control1)
    }

    /// Run the SD clock at the fastest rate not above `target_hz`, by the
    /// SDHCI clock-change sequence: stop the clock, program the divider, wait
    /// for the internal clock to settle, start it again.
    fn set_sd_clock(&mut self, bench: &Bench<'_>, target_hz: u32) -> Result<u32, DriverError> {
        let clock = host::sd_clock(bench.base_clock_hz, target_hz, bench.caps.divided_clock())
            .ok_or(DriverError::Unsupported)?;
        self.sd_clock_enable(false)?;
        self.host.write32(
            regs::REG_CONTROL1,
            clock.select
                | (DATA_TIMEOUT_VALUE << regs::CONTROL1_TIMEOUT_SHIFT)
                | regs::CONTROL1_CLK_INTLEN,
        )?;
        self.wait_set(regs::REG_CONTROL1, regs::CONTROL1_CLK_STABLE)?;
        self.sd_clock_enable(true)?;
        self.host.trace(Trace::Clock {
            target_hz,
            select: clock.select,
            hz: clock.hz,
        });
        Ok(clock.hz)
    }

    /// `CMD8`: a v2 card echoes the check pattern; anything else is a card
    /// this driver does not address.
    fn send_if_cond(&mut self) -> Result<(), DriverError> {
        let echo = self.issue(command::SEND_IF_COND, command::IF_COND_ARG, 0)?[0];
        if echo & 0xFF == command::IF_COND_CHECK_PATTERN {
            Ok(())
        } else {
            Err(DriverError::Unsupported)
        }
    }

    /// Poll `ACMD41` until the card finishes powering up, one
    /// [`OP_COND_INTERVAL_US`] apart for at most [`OP_COND_ROUNDS`] rounds,
    /// returning its OCR.
    ///
    /// A byte-addressed (standard-capacity) card is refused rather than
    /// mis-addressed.
    fn op_cond(&mut self, ask_1v8: bool, max_performance: bool) -> Result<u32, DriverError> {
        let arg = command::op_cond_argument(ask_1v8, max_performance);
        for round in 0..OP_COND_ROUNDS {
            if round != 0 {
                self.host.delay_us(OP_COND_INTERVAL_US);
            }
            let ocr = self.issue_app(command::SD_SEND_OP_COND, arg, 0)?[0];
            if ocr & command::OCR_READY != 0 {
                self.host.trace(Trace::Ocr(ocr));
                if ocr & command::OCR_CCS == 0 {
                    return Err(DriverError::Unsupported);
                }
                return Ok(ocr);
            }
        }
        Err(DriverError::DeviceFault)
    }

    /// Switch the card and host to 1.8 V signalling: `CMD11`, the card pulls
    /// its data lines low, the clock stops, both ends switch, the clock
    /// restarts, and the card releases the lines to say it is done.
    fn switch_to_1v8(&mut self, supply: &mut dyn CardSupply) -> Result<(), DriverError> {
        let status = self.issue(command::VOLTAGE_SWITCH, 0, 0)?[0];
        if status & command::R1_ERRORS != 0 {
            return Err(DriverError::DeviceFault);
        }
        self.host.delay_us(SWITCH_CARD_BUSY_US);
        if self.dat0_high()? {
            return Err(DriverError::DeviceFault);
        }
        self.sd_clock_enable(false)?;
        supply.set_signal_voltage(SignalVoltage::V1_8)?;
        let control2 = self.host.read32(regs::REG_CONTROL2)?;
        self.host
            .write32(regs::REG_CONTROL2, control2 | regs::CONTROL2_1V8_SIGNALLING)?;
        self.host.delay_us(SWITCH_CLOCK_GATE_US);
        // A controller whose own regulator did not settle drops the bit.
        if self.host.read32(regs::REG_CONTROL2)? & regs::CONTROL2_1V8_SIGNALLING == 0 {
            return Err(DriverError::DeviceFault);
        }
        self.sd_clock_enable(true)?;
        self.host.delay_us(CLOCK_SETTLE_US);
        if self.dat0_high()? {
            Ok(())
        } else {
            Err(DriverError::DeviceFault)
        }
    }

    fn dat0_high(&mut self) -> Result<bool, DriverError> {
        Ok(self.host.read32(regs::REG_STATUS)? & regs::STATUS_DAT0_LEVEL != 0)
    }

    /// `CMD2`, `CMD3`, `CMD9`, `CMD7`, `CMD16`: take the card from `ready` to
    /// selected in `tran` with its geometry read.
    fn identify(&mut self, at_1v8: bool) -> Result<(), Setback> {
        self.step(BringUpStage::AllSendCid, Part::Card, at_1v8, |dev| {
            dev.issue(command::ALL_SEND_CID, 0, 0).map(drop)
        })?;
        let r6 = self.step(BringUpStage::SendRelativeAddr, Part::Card, at_1v8, |dev| {
            dev.issue(command::SEND_RELATIVE_ADDR, 0, 0)
        })?;
        // RCA occupies R6 bits [31:16], where every addressed command takes it.
        self.rca = r6[0] & 0xFFFF_0000;
        self.step(BringUpStage::SendCsd, Part::Card, at_1v8, |dev| {
            let csd = dev.issue(command::SEND_CSD, dev.rca, 0)?;
            dev.geometry = command::geometry_from_csd(csd)?;
            Ok(())
        })?;
        self.step(BringUpStage::SelectCard, Part::Card, at_1v8, |dev| {
            dev.issue_awaiting_busy(command::SELECT_CARD, dev.rca)
        })?;
        self.step(BringUpStage::SetBlockLen, Part::Card, at_1v8, |dev| {
            dev.issue(command::SET_BLOCKLEN, BLOCK_SIZE, 0).map(drop)
        })
    }

    /// `ACMD51`: read and decode the SCR; an unknown structure is refused.
    fn read_scr(&mut self) -> Result<card::Scr, DriverError> {
        let mut scr = [0u8; SCR_BYTES];
        self.read_register(command::SEND_SCR, 0, true, &mut scr)?;
        self.host.trace(Trace::Scr(scr));
        card::decode_scr(&scr).ok_or(DriverError::Unsupported)
    }

    /// Put the card and the controller on the 4-bit bus.
    fn set_bus_width_4bit(&mut self) -> Result<(), DriverError> {
        self.issue_app(command::SET_BUS_WIDTH, command::BUS_WIDTH_4BIT_ARG, 0)?;
        let control0 = self.host.read32(regs::REG_CONTROL0)?;
        self.host.write32(
            regs::REG_CONTROL0,
            control0 | regs::CONTROL0_DATA_WIDTH_4BIT,
        )
    }

    /// `CMD6`, one 64-byte status block.
    fn switch_function(
        &mut self,
        set: bool,
        access_mode: u8,
        current_limit: u8,
    ) -> Result<card::SwitchStatus, DriverError> {
        let mut status = [0u8; SWITCH_STATUS_BYTES];
        let arg = card::switch_argument(set, access_mode, current_limit);
        self.read_register(command::SWITCH_FUNC, arg, false, &mut status)?;
        let status = card::decode_switch_status(&status);
        self.host.trace(Trace::Switch {
            access_modes: status.access_modes,
            current_limits: status.current_limits,
            access_mode: status.access_mode,
        });
        Ok(status)
    }

    /// Query the card's modes, raise its current limit where the mode
    /// defines one and both ends allow it, and switch it to the fastest mode
    /// both ends drive at its signalling.
    ///
    /// A current limit the card declines leaves it at its 200 mA default,
    /// which every mode works within; an access mode it declines fails the
    /// rung.
    fn switch_mode(&mut self, bench: &Bench<'_>, at_1v8: bool) -> Result<BusMode, DriverError> {
        let offered = self.switch_function(false, card::KEEP, card::KEEP)?;
        let mode = bus::fastest(&bench.caps, offered.access_modes, at_1v8);
        if let Some(limit) = bus::current_limit(
            bench.caps.max_current_330_ma(),
            offered.current_limits,
            mode,
        ) {
            self.switch_function(true, card::KEEP, limit)?;
        }
        if mode.function() != 0 {
            let switched = self.switch_function(true, mode.function(), card::KEEP)?;
            if switched.access_mode != mode.function() {
                return Err(DriverError::Unsupported);
            }
        }
        Ok(mode)
    }

    /// Give the controller `mode`'s timing and clock, returning the clock.
    ///
    /// The UHS mode select changes only with the SD clock stopped, which
    /// [`Self::set_sd_clock`] then restarts at the mode's rate.
    fn set_bus_timing(&mut self, bench: &Bench<'_>, mode: BusMode) -> Result<u32, DriverError> {
        self.sd_clock_enable(false)?;
        let control0 = self.host.read32(regs::REG_CONTROL0)? & !regs::CONTROL0_HIGH_SPEED;
        let high_speed = if mode.high_speed_timing() {
            regs::CONTROL0_HIGH_SPEED
        } else {
            0
        };
        self.host
            .write32(regs::REG_CONTROL0, control0 | high_speed)?;
        if bench.caps.divided_clock() {
            let control2 = self.host.read32(regs::REG_CONTROL2)? & !regs::CONTROL2_UHS_MODE_MASK;
            self.host.write32(
                regs::REG_CONTROL2,
                control2 | (mode.uhs_select() << regs::CONTROL2_UHS_MODE_SHIFT),
            )?;
        }
        self.set_sd_clock(bench, mode.max_clock_hz())
    }

    /// Read the first block at the negotiated timing: a mode the card or the
    /// board's wiring cannot actually carry fails here, where a lower rung can
    /// still be tried, rather than on the first real read.
    fn verify_bus(&mut self) -> Result<(), DriverError> {
        let mut block = [0u8; BLOCK_SIZE as usize];
        self.read_blocks_pio(0, &mut block)
    }

    /// Cut the card's power and bring it back at 3.3 V signalling, then
    /// reset the controller for a fresh identification.
    fn power_cycle(&mut self, bench: &mut Bench<'_>) -> Result<(), BringUpFault> {
        self.host.trace(Trace::Stage(BringUpStage::PowerCycle));
        let cycle = |e| BringUpFault::new(BringUpStage::PowerCycle, e);
        let supply = bench
            .supply
            .as_deref_mut()
            .ok_or(cycle(DriverError::Unsupported))?;
        self.sd_clock_enable(false).map_err(cycle)?;
        self.host.write32(regs::REG_CONTROL0, 0).map_err(cycle)?;
        let control2 = self.host.read32(regs::REG_CONTROL2).map_err(cycle)?;
        self.host
            .write32(
                regs::REG_CONTROL2,
                control2 & !(regs::CONTROL2_1V8_SIGNALLING | regs::CONTROL2_UHS_MODE_MASK),
            )
            .map_err(cycle)?;
        // The rail drops to 3.3 V only once the card is unpowered, so 3.3 V
        // is never driven into a card still signalling at 1.8 V.
        supply.set_card_power(false).map_err(cycle)?;
        supply
            .set_signal_voltage(SignalVoltage::V3_3)
            .map_err(cycle)?;
        self.host.delay_us(POWER_OFF_HOLD_US);
        supply.set_card_power(true).map_err(cycle)?;
        self.host.delay_us(POWER_ON_SETTLE_US);
        let reset = |e| BringUpFault::new(BringUpStage::ResetClock, e);
        self.reset_controller().map_err(reset)?;
        self.power_and_clock(bench).map_err(reset)
    }

    /// Select ADMA2 when the controller has it and the host granted staging
    /// the 32-bit descriptors can address, and keep it only once a read by it
    /// matches the data port's; otherwise stay on programmed I/O, returning
    /// the failure that decided it.
    fn select_dma(&mut self, caps: &HostCaps) -> Result<Option<BringUpFault>, DriverError> {
        let stage_blocks = match self.host.dma_region() {
            Some(region) if caps.adma2() => staging_blocks(&region),
            _ => 0,
        };
        if stage_blocks == 0 {
            return Ok(None);
        }
        self.host.trace(Trace::Stage(BringUpStage::SelectDma));
        self.select_adma2(true)?;
        self.dma_stage_blocks = stage_blocks;
        if let Some(region) = self.host.dma_region() {
            let (data, table) = (region.data_device, region.table_device);
            self.host.trace(Trace::Dma {
                data,
                table,
                stage_blocks,
            });
        }
        self.host.trace(Trace::Stage(BringUpStage::VerifyDma));
        let Err(error) = self.verify_dma() else {
            return Ok(None);
        };
        self.dma_stage_blocks = 0;
        self.select_adma2(false)?;
        Ok(Some(BringUpFault::new(BringUpStage::VerifyDma, error)))
    }

    fn select_adma2(&mut self, adma2: bool) -> Result<(), DriverError> {
        let control0 = self.host.read32(regs::REG_CONTROL0)? & !regs::CONTROL0_DMA_SELECT_MASK;
        let select = if adma2 {
            regs::CONTROL0_DMA_SELECT_ADMA2
        } else {
            0
        };
        self.host.write32(regs::REG_CONTROL0, control0 | select)
    }

    /// Read the first block by ADMA2 into staging filled with the inverse of
    /// what the data port reads there, and require the two to agree: a
    /// descriptor, bus address or cache maintenance that lands the data
    /// anywhere but where the CPU reads it leaves the inverse behind.
    fn verify_dma(&mut self) -> Result<(), DriverError> {
        let mut by_port = [0u8; BLOCK_SIZE as usize];
        self.read_blocks_pio(0, &mut by_port)?;
        {
            let region = self.host.dma_region().ok_or(DriverError::DeviceFault)?;
            for (staged, &byte) in region.data.iter_mut().zip(&by_port) {
                *staged = !byte;
            }
        }
        let mut by_dma = [0u8; BLOCK_SIZE as usize];
        self.read_blocks_dma(0, &mut by_dma)?;
        let Some(offset) = by_port
            .iter()
            .zip(&by_dma)
            .position(|(port, dma)| port != dma)
        else {
            return Ok(());
        };
        self.host.trace(Trace::DmaMismatch {
            offset,
            port: by_port[offset],
            dma: by_dma[offset],
        });
        Err(DriverError::DeviceFault)
    }
}

/// How many blocks `region` stages per command: its whole data area in
/// blocks, bounded by the descriptors its table holds and a command's block
/// count, or `0` when it is unusable — no whole block, or an area the
/// 32-bit ADMA2 fields cannot address.
fn staging_blocks(region: &crate::DmaRegion<'_>) -> usize {
    let addressable = |base: u64, len: usize| {
        base.checked_add(len as u64)
            .is_some_and(|end| end <= 1 << 32)
    };
    if !addressable(region.data_device, region.data.len())
        || !addressable(region.table_device, region.table.len())
    {
        return 0;
    }
    let block = BLOCK_SIZE as usize;
    let per_descriptor = adma::MAX_DESC_BYTES / block;
    let described = (region.table.len() / adma::DESC_BYTES).saturating_mul(per_descriptor);
    (region.data.len() / block)
        .min(described)
        .min(MAX_BLOCKS_PER_COMMAND)
}
