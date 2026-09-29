//! Bring-up negotiation tests over the register-level mock
//! ([`crate::mock`]): the card reaches the fastest bus the controller, card
//! and board drive, and every rung that fails steps down.
//!
//! The mock asserts the SD clock never exceeds what the card's state allows
//! and moves data only when the host's timing matches the card's, so each
//! negotiated [`Link`] here is one the card would actually carry.

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec::Vec;

use super::*;
use crate::bus::Rung;
use crate::mock::{MockSdhci, RailEvent, STORE_BLOCKS};
use crate::trace::{Trace, WaitEnd};
use tairix_abi::driver::block::Block;

/// Bring `mock` up with the board's supplies wired.
fn open_uhs(mock: MockSdhci) -> Result<Emmc2<MockSdhci>, BringUpFault> {
    let mut supply = mock.supply();
    Emmc2::open(
        mock,
        Board {
            base_clock_hz: None,
            supply: Some(&mut supply),
        },
    )
}

fn rail_events(dev: &Emmc2<MockSdhci>) -> Vec<RailEvent> {
    dev.host()
        .wiring
        .events
        .borrow()
        .iter()
        .map(|&(_, e)| e)
        .collect()
}

#[test]
fn without_a_supply_the_card_runs_high_speed_on_the_4bit_bus() {
    let dev = Emmc2::open(MockSdhci::healthy(7), Board::default()).expect("bring-up");
    let link = dev.link();
    assert_eq!(link.mode, BusMode::HighSpeed);
    assert_eq!(link.clock_hz, 50_000_000);
    assert_eq!(link.base_clock_hz, 100_000_000, "from the capabilities");
    assert!(link.counted_transfers);
    assert_eq!(link.fallback, None);
    let host = dev.host();
    assert!(!host.card_at_1v8());
    assert_eq!(host.acmd6_arg, Some(command::BUS_WIDTH_4BIT_ARG));
    assert_ne!(host.control0 & regs::CONTROL0_DATA_WIDTH_4BIT, 0);
    assert_ne!(host.control0 & regs::CONTROL0_HIGH_SPEED, 0);
    assert!(
        host.power_on,
        "the timing's read-modify-writes kept bus power"
    );
    assert!(
        host.acmd41_args
            .iter()
            .all(|arg| arg & command::OCR_S18 == 0),
        "1.8 V is never asked for where it cannot be undone"
    );
}

#[test]
fn with_a_supply_the_card_runs_ddr50_at_1v8() {
    let dev = open_uhs(MockSdhci::healthy(7)).expect("bring-up");
    let link = dev.link();
    assert_eq!(link.mode, BusMode::Ddr50);
    assert_eq!(link.clock_hz, 50_000_000);
    assert_eq!(link.fallback, None);
    let host = dev.host();
    assert!(host.card_at_1v8());
    assert_eq!(host.card_bus_mode(), BusMode::Ddr50);
    assert_ne!(host.control2 & regs::CONTROL2_1V8_SIGNALLING, 0);
    assert_eq!(
        (host.control2 & regs::CONTROL2_UHS_MODE_MASK) >> regs::CONTROL2_UHS_MODE_SHIFT,
        0b100
    );
    assert_eq!(
        rail_events(&dev),
        [
            RailEvent::Signal(SignalVoltage::V3_3),
            RailEvent::Signal(SignalVoltage::V1_8)
        ],
        "the power-up selection, then the switch"
    );
}

#[test]
fn the_uhs_bring_up_issues_the_protocol_in_order() {
    let dev = open_uhs(MockSdhci::healthy(7)).expect("bring-up");
    assert_eq!(
        dev.host().command_indices(),
        [0, 8, 55, 41, 55, 41, 11, 2, 3, 9, 7, 16, 55, 51, 55, 6, 6, 6, 17],
        "idle, interface, power-up, voltage switch, identify, select, SCR, \
         4-bit, query, switch, verify"
    );
}

#[test]
fn ddr50_moves_data_by_dma_with_counted_transfers() {
    let mut mock = MockSdhci::healthy_dma(7, STORE_BLOCKS);
    mock.fill_blocks(0, 4, 0x33);
    let mut dev = open_uhs(mock).expect("bring-up");
    assert!(dev.link().dma);
    let mut buf = [0u8; 4 * BLOCK_SIZE as usize];
    dev.read_blocks(0, &mut buf).expect("read at DDR50");
    dev.write_blocks(8, &buf).expect("write at DDR50");
    let mut back = [0u8; 4 * BLOCK_SIZE as usize];
    dev.read_blocks(8, &mut back).expect("read back");
    assert_eq!(back, buf);
    assert_eq!(dev.host().counted, [4, 4, 4]);
}

#[test]
fn a_failed_voltage_switch_power_cycles_the_card_back_to_high_speed() {
    let mut mock = MockSdhci::healthy(7);
    mock.switch_fails = true;
    let dev = open_uhs(mock).expect("bring-up");
    let link = dev.link();
    assert_eq!(link.mode, BusMode::HighSpeed);
    assert_eq!(
        link.fallback,
        Some(BringUpFault {
            stage: BringUpStage::VoltageSwitch,
            error: DriverError::DeviceFault
        })
    );
    assert_eq!(dev.host().wiring.power_cycles.get(), 1);
    assert_eq!(
        rail_events(&dev),
        [
            RailEvent::Signal(SignalVoltage::V3_3),
            RailEvent::Signal(SignalVoltage::V1_8),
            RailEvent::Power(false),
            RailEvent::Signal(SignalVoltage::V3_3),
            RailEvent::Power(true),
        ],
        "the rail returns to 3.3 V only once the card is unpowered"
    );
    let last = dev.host().acmd41_args.last().copied().expect("ACMD41");
    assert_eq!(
        last & command::OCR_S18,
        0,
        "the retry does not ask for 1.8 V"
    );
}

#[test]
fn a_mode_the_board_cannot_carry_steps_down_after_its_verify_read() {
    let mut mock = MockSdhci::healthy(7);
    mock.broken_modes = 1 << BusMode::Ddr50.function();
    let dev = open_uhs(mock).expect("bring-up");
    assert_eq!(dev.link().mode, BusMode::HighSpeed);
    assert_eq!(
        dev.link().fallback.map(|f| f.stage),
        Some(BringUpStage::VerifyBus)
    );
    assert_eq!(dev.host().wiring.power_cycles.get(), 1);
}

#[test]
fn high_speed_that_fails_its_verify_read_steps_down_to_default_speed() {
    let mut mock = MockSdhci::healthy(7);
    mock.broken_modes = 1 << BusMode::HighSpeed.function();
    let mut dev = Emmc2::open(mock, Board::default()).expect("bring-up");
    let link = dev.link();
    assert_eq!(link.mode, BusMode::DefaultSpeed);
    assert_eq!(link.clock_hz, 25_000_000);
    assert!(!link.counted_transfers, "Default Speed reads no SCR");
    assert_eq!(
        link.fallback.map(|f| f.stage),
        Some(BringUpStage::VerifyBus)
    );
    assert_eq!(
        dev.host().wiring.power_cycles.get(),
        0,
        "3.3 V needs no power cycle"
    );
    let mut block = [0u8; BLOCK_SIZE as usize];
    dev.read_blocks(0, &mut block)
        .expect("reads at Default Speed");
}

#[test]
fn a_card_that_cannot_move_data_at_default_speed_is_unusable() {
    let mut mock = MockSdhci::healthy(7);
    mock.broken_modes = 0b11;
    assert_eq!(
        Emmc2::open(mock, Board::default()).err(),
        Some(BringUpFault {
            stage: BringUpStage::VerifyBus,
            error: DriverError::DeviceFault
        })
    );
}

#[test]
fn a_card_without_1v8_stays_at_3v3_even_with_a_supply() {
    let mut mock = MockSdhci::healthy(7);
    mock.uhs_card = false;
    let dev = open_uhs(mock).expect("bring-up");
    assert_eq!(dev.link().mode, BusMode::HighSpeed);
    assert_eq!(
        rail_events(&dev),
        [RailEvent::Signal(SignalVoltage::V3_3)],
        "only the power-up selection"
    );
    assert!(
        !dev.host().command_indices().contains(&11),
        "no CMD11 without S18A"
    );
}

#[test]
fn a_host_without_uhs_never_asks_for_1v8() {
    let mut mock = MockSdhci::healthy(7);
    mock.caps1 = 0;
    let dev = open_uhs(mock).expect("bring-up");
    assert_eq!(dev.link().mode, BusMode::HighSpeed);
    assert!(dev
        .host()
        .acmd41_args
        .iter()
        .all(|arg| arg & command::OCR_S18 == 0));
}

#[test]
fn sdr50_is_taken_from_a_host_that_runs_it_untuned() {
    let mut mock = MockSdhci::healthy(7);
    mock.caps1 = 0x0001;
    mock.access_modes = 0b0_0111;
    let dev = open_uhs(mock).expect("bring-up");
    assert_eq!(dev.link().mode, BusMode::Sdr50);
    assert_eq!(dev.link().clock_hz, 100_000_000);
}

#[test]
fn a_card_offering_neither_ddr50_nor_untuned_sdr50_runs_sdr25() {
    let mut mock = MockSdhci::healthy(7);
    mock.access_modes = 0b0_0111;
    let dev = open_uhs(mock).expect("bring-up");
    assert_eq!(dev.link().mode, BusMode::Sdr25);
    assert_eq!(dev.link().clock_hz, 50_000_000);
}

#[test]
fn the_current_limit_is_raised_only_where_the_host_supplies_it() {
    let pi = open_uhs(MockSdhci::healthy(7)).expect("bring-up");
    assert_eq!(
        pi.host().issued(6),
        3,
        "ACMD6, the query and the mode switch: no current-limit switch at 32 mA"
    );

    let mut mock = MockSdhci::healthy(7);
    mock.max_current = 0x0000_00C8;
    let generous = open_uhs(mock).expect("bring-up");
    assert_eq!(generous.link().mode, BusMode::Ddr50);
    assert_eq!(
        generous.host().issued(6),
        4,
        "a current-limit switch to 800 mA as well"
    );
}

#[test]
fn power_up_polling_is_paced_one_interval_apart() {
    // D237: a card that takes five rounds to power up is asked five times,
    // one interval apart, never back to back.
    let mut mock = MockSdhci::healthy(7);
    mock.acmd41_ready_after = 5;
    let dev = Emmc2::open(mock, Board::default()).expect("bring-up");
    let times = &dev.host().acmd41_times;
    assert_eq!(times.len(), 5);
    assert!(
        times.windows(2).all(|pair| pair[1] - pair[0] == 10_000),
        "{times:?}"
    );
}

#[test]
fn a_card_that_never_powers_up_fails_after_the_specifications_second() {
    let mut mock = MockSdhci::healthy(7);
    mock.acmd41_ready_after = u32::MAX;
    let Err(fault) = Emmc2::open(mock, Board::default()) else {
        panic!("a card that never powers up cannot be used");
    };
    assert_eq!(
        fault,
        BringUpFault {
            stage: BringUpStage::OpCond,
            error: DriverError::DeviceFault
        }
    );
}

#[test]
fn an_unanswering_card_is_power_cycled_once_and_revived() {
    let mut mock = MockSdhci::healthy(7);
    mock.mute_until_cycled = true;
    let dev = open_uhs(mock).expect("revived");
    assert_eq!(dev.link().mode, BusMode::Ddr50);
    assert_eq!(
        dev.link().fallback.map(|f| f.stage),
        Some(BringUpStage::SendIfCond)
    );
    assert_eq!(dev.host().wiring.power_cycles.get(), 1);

    let mut mock = MockSdhci::healthy(7);
    mock.mute_until_cycled = true;
    assert_eq!(
        Emmc2::open(mock, Board::default()).err().map(|f| f.stage),
        Some(BringUpStage::SendIfCond),
        "without a supply there is nothing to revive it with"
    );
}

#[test]
fn a_supply_that_refuses_to_switch_is_never_asked_for_1v8() {
    // A board that cannot select 3.3 V could not undo a switch to 1.8 V, so
    // the card stays at 3.3 V rather than entering a switch left stranded.
    let mock = MockSdhci::healthy(7);
    mock.wiring.refuse.set(true);
    let dev = open_uhs(mock).expect("bring-up at 3.3 V");
    let link = dev.link();
    assert_eq!(link.mode, BusMode::HighSpeed);
    assert_eq!(
        link.fallback,
        Some(BringUpFault {
            stage: BringUpStage::InitialSignalling,
            error: DriverError::DeviceFault
        })
    );
    let host = dev.host();
    assert!(
        host.acmd41_args
            .iter()
            .all(|arg| arg & command::OCR_S18 == 0),
        "1.8 V is never asked for"
    );
    assert!(!host.command_indices().contains(&11), "no CMD11");
    assert_eq!(host.wiring.power_cycles.get(), 0);
}

#[test]
fn the_platforms_base_clock_outranks_the_capabilities() {
    // A controller fed 200 MHz whose capabilities still say 100: the mock
    // fails any command clocked past what the card allows, so dividing the
    // wrong base would not get this far.
    let mut mock = MockSdhci::healthy(7);
    mock.base_clock_hz = 200_000_000;
    let dev = Emmc2::open(
        mock,
        Board {
            base_clock_hz: Some(200_000_000),
            supply: None,
        },
    )
    .expect("bring-up");
    assert_eq!(dev.link().base_clock_hz, 200_000_000);
    assert_eq!(dev.link().clock_hz, 50_000_000);
}

#[test]
fn a_controller_declaring_no_base_clock_needs_the_platforms() {
    let mut mock = MockSdhci::healthy(7);
    mock.caps &= !0xFF00;
    assert_eq!(
        Emmc2::open(mock, Board::default()).err(),
        Some(BringUpFault {
            stage: BringUpStage::ResetClock,
            error: DriverError::Unsupported
        })
    );

    let mut mock = MockSdhci::healthy(7);
    mock.caps &= !0xFF00;
    let dev = Emmc2::open(
        mock,
        Board {
            base_clock_hz: Some(100_000_000),
            supply: None,
        },
    )
    .expect("bring-up");
    assert_eq!(dev.link().mode, BusMode::HighSpeed);
}

#[test]
fn an_scr_of_an_unknown_structure_steps_down_to_default_speed() {
    let mut mock = MockSdhci::healthy(7);
    mock.scr[0] |= 0x10;
    let dev = Emmc2::open(mock, Board::default()).expect("bring-up");
    assert_eq!(dev.link().mode, BusMode::DefaultSpeed);
    assert_eq!(
        dev.link().fallback.map(|f| f.stage),
        Some(BringUpStage::SendScr)
    );
}

#[test]
fn the_link_names_each_mode_distinctly() {
    use alloc::collections::BTreeSet;
    let modes = [
        BusMode::DefaultSpeed,
        BusMode::HighSpeed,
        BusMode::Sdr12,
        BusMode::Sdr25,
        BusMode::Sdr50,
        BusMode::Ddr50,
    ];
    let names: BTreeSet<&'static str> = modes.iter().map(|m| m.as_str()).collect();
    assert_eq!(names.len(), modes.len());
}

/// The first wait the engine traced as failed: its register, the bits it
/// wanted, the value it last read, its parks and why it ended.
fn failed_wait(traces: &[Trace]) -> Option<(usize, u32, u32, u32, WaitEnd)> {
    traces.iter().find_map(|t| match *t {
        Trace::WaitFailed {
            register,
            wanted,
            value,
            waits,
            end,
        } => Some((register, wanted, value, waits, end)),
        _ => None,
    })
}

#[test]
fn a_bring_up_traces_each_step_from_the_reset_to_the_link() {
    use BringUpStage::*;
    let dev = open_uhs(MockSdhci::healthy_dma(7, STORE_BLOCKS)).expect("bring-up");
    let host = dev.host();
    let traces = host.traces.borrow();
    let steps: Vec<Trace> = traces
        .iter()
        .copied()
        .filter(|t| matches!(t, Trace::Stage(_) | Trace::Attempt(_)))
        .collect();
    let expected: Vec<Trace> = [
        Trace::Stage(ResetClock),
        Trace::Stage(InitialSignalling),
        Trace::Attempt(Rung::Uhs),
    ]
    .into_iter()
    .chain(
        [
            GoIdle,
            SendIfCond,
            OpCond,
            VoltageSwitch,
            AllSendCid,
            SendRelativeAddr,
            SendCsd,
            SelectCard,
            SetBlockLen,
            RaiseClock,
            SendScr,
            SetBusWidth,
            SwitchFunction,
            SetBusTiming,
            VerifyBus,
            SelectDma,
            VerifyDma,
        ]
        .map(Trace::Stage),
    )
    .collect();
    assert_eq!(steps, expected);
    assert_eq!(traces.last(), Some(&Trace::Ready(dev.link())));

    assert!(traces.contains(&Trace::Controller {
        version: host.version,
        caps: host.caps,
        caps1: host.caps1,
        max_current: host.max_current,
    }));
    assert!(traces.contains(&Trace::BaseClock {
        hz: 100_000_000,
        from_board: false,
    }));
    let clocks: Vec<u32> = traces
        .iter()
        .filter_map(|t| match *t {
            Trace::Clock { hz, .. } => Some(hz),
            _ => None,
        })
        .collect();
    assert_eq!(clocks, [400_000, 25_000_000, 50_000_000]);
    assert!(traces
        .iter()
        .any(|t| matches!(*t, Trace::Ocr(ocr) if ocr & command::OCR_S18 != 0)));
    assert!(traces.iter().any(|t| matches!(t, Trace::Scr(_))));
    assert!(traces.iter().any(|t| matches!(
        t,
        Trace::Switch {
            access_modes: 0x1F,
            ..
        }
    )));
    assert!(traces.iter().any(|t| matches!(
        t,
        Trace::Dma {
            stage_blocks: DMA_STAGE_BLOCKS,
            ..
        }
    )));

    let issued: Vec<u8> = traces
        .iter()
        .filter_map(|t| match *t {
            Trace::Command { index, .. } => Some(index),
            _ => None,
        })
        .collect();
    let answered: Vec<u8> = traces
        .iter()
        .filter_map(|t| match *t {
            Trace::Response { index, .. } => Some(index),
            _ => None,
        })
        .collect();
    assert_eq!(issued, answered, "every command traced its answer");
    assert_eq!(issued.first(), Some(&0));
    assert!(issued.contains(&11), "the voltage switch is traced");
    assert!(failed_wait(&traces).is_none());
}

#[test]
fn a_command_the_controller_fails_traces_the_error_status_it_saw() {
    let mut mock = MockSdhci::healthy(7);
    mock.error_on_index = Some(8);
    let traces = Rc::clone(&mock.traces);
    let fault = Emmc2::open(mock, Board::default()).err();
    assert_eq!(fault.map(|f| f.stage), Some(BringUpStage::SendIfCond));
    let (register, wanted, value, _, end) =
        failed_wait(&traces.borrow()).expect("the failed wait is traced");
    assert_eq!(register, regs::REG_INTERRUPT);
    assert_eq!(wanted, regs::INT_CMD_DONE);
    assert_ne!(value & regs::INT_ERROR_MASK, 0, "with the error it saw");
    assert_eq!(end, WaitEnd::Error);
}

#[test]
fn a_silent_or_wedged_controller_traces_how_its_wait_ended() {
    let mut silent = MockSdhci::healthy_deferred(7);
    silent.silent = true;
    let traces = Rc::clone(&silent.traces);
    assert!(Emmc2::open(silent, Board::default()).is_err());
    assert_eq!(
        failed_wait(&traces.borrow()).map(|(_, _, _, waits, end)| (waits, end)),
        Some((1, WaitEnd::Silent))
    );

    let mut wedged = MockSdhci::healthy(7);
    wedged.stall = true;
    let traces = Rc::clone(&wedged.traces);
    assert!(Emmc2::open_with_budget(wedged, Board::default(), 8).is_err());
    assert_eq!(
        failed_wait(&traces.borrow()).map(|(_, wanted, _, waits, end)| (wanted, waits, end)),
        Some((regs::INT_CMD_DONE, 8, WaitEnd::Budget)),
        "a command that never completes spends the poll budget"
    );
}

#[test]
fn a_register_that_never_settles_traces_its_last_value() {
    let mut dev =
        Emmc2::open_with_budget(MockSdhci::healthy(7), Board::default(), 8).expect("bring-up");
    dev.host.lines_stuck = true;
    dev.host.error_on_index = Some(17);
    let mut block = [0u8; BLOCK_SIZE as usize];
    assert!(dev.read_blocks(0, &mut block).is_err());
    let lines = regs::CONTROL1_SRST_CMD | regs::CONTROL1_SRST_DATA;
    let traces = dev.host().traces.borrow();
    let reset = traces.iter().find_map(|t| match *t {
        Trace::WaitFailed {
            register: regs::REG_CONTROL1,
            wanted,
            value,
            waits,
            end,
        } => Some((wanted, value & lines, waits, end)),
        _ => None,
    });
    assert_eq!(reset, Some((lines, lines, 0, WaitEnd::Budget)));
}

#[test]
fn a_misdirected_dma_verify_traces_where_it_first_differs() {
    let mut mock = MockSdhci::healthy_dma(7, STORE_BLOCKS);
    mock.dma_misdirected = true;
    mock.fill_block(0, 0x42);
    let dev = Emmc2::open(mock, Board::default()).expect("bring-up on the data port");
    let port = MockSdhci::expected_block(0x42)[0];
    assert!(dev.host().traces.borrow().contains(&Trace::DmaMismatch {
        offset: 0,
        port,
        dma: !port,
    }));
}
