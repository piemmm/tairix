//! Transfer, recovery and wiring tests over the register-level mock
//! ([`crate::mock`]); the bring-up negotiation has its own suite
//! ([`crate::bringup_tests`]).

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use core::ptr::NonNull;

use super::*;
use crate::mock::{MockSdhci, STATE_STBY, STORE_BLOCKS};
use tairix_abi::driver::block::Block;
use tairix_abi::driver::dma::{DmaHost, DmaSlab};
use tairix_abi::driver::DriverKind;
use tairix_abi::{CapabilityId, MmioMapError, MmioMapper, RegisterWindow};

const BS: usize = BLOCK_SIZE as usize;

/// Bring `mock` up with no board supplies: the 3.3 V path.
fn open(mock: MockSdhci) -> Emmc2<MockSdhci> {
    Emmc2::open(mock, Board::default()).expect("bring-up")
}

/// Assert the one failed transfer reset the lines once and was then aborted
/// by one `CMD12` carrying the Abort command type and an R1b response.
fn assert_aborted_after_the_line_reset(host: &MockSdhci) {
    assert_eq!(host.line_resets, 1, "the failed transfer reset the lines");
    let &[(word, resets_before)] = host.stops.as_slice() else {
        panic!("expected one CMD12, got {:?}", host.stops);
    };
    assert_eq!(resets_before, 1, "the abort follows the line reset");
    assert_eq!(word & regs::CMD_TYPE_ABORT, regs::CMD_TYPE_ABORT);
    assert_eq!(
        (word >> regs::CMD_RESP_TYPE_SHIFT) & 0b11,
        regs::RESP_48_BUSY
    );
}

#[test]
fn open_runs_identification_and_reports_geometry() {
    let dev = open(MockSdhci::healthy(7));
    let geo = dev.geometry().expect("geometry");
    assert_eq!(geo.block_size, 512);
    assert_eq!(geo.block_count, (7 + 1) * 1024);
}

#[test]
fn read_single_block_returns_card_data() {
    let mut mock = MockSdhci::healthy(7);
    mock.fill_block(3, 0x40);
    let mut dev = open(mock);
    let mut buf = [0u8; BS];
    dev.read_blocks(3, &mut buf).expect("read");
    assert_eq!(buf.as_slice(), MockSdhci::expected_block(0x40).as_slice());
}

#[test]
fn read_multiple_blocks_returns_contiguous_data() {
    let mut mock = MockSdhci::healthy(7);
    mock.fill_blocks(1, 3, 0x10);
    let mut dev = open(mock);
    let mut buf = [0u8; 3 * BS];
    dev.read_blocks(1, &mut buf).expect("read");
    for n in 0..3 {
        assert_eq!(
            &buf[n * BS..(n + 1) * BS],
            MockSdhci::expected_block(MockSdhci::nth_seed(0x10, n)).as_slice()
        );
    }
}

#[test]
fn interrupt_driven_read_parks_until_the_controller_signals() {
    // Completions are visible only once the engine parks on the interrupt,
    // so a successful read proves it never busy-spins a status register.
    let mut mock = MockSdhci::healthy_deferred(7);
    mock.fill_block(5, 0x20);
    let mut dev = open(mock);
    let before = dev.host().await_calls;
    let mut buf = [0u8; BS];
    dev.read_blocks(5, &mut buf).expect("read");
    assert_eq!(buf.as_slice(), MockSdhci::expected_block(0x20).as_slice());
    assert!(dev.host().await_calls > before);
}

#[test]
fn bring_up_enables_the_completion_interrupt_signal() {
    let dev = open(MockSdhci::healthy(7));
    assert_eq!(dev.host().irpt_en, regs::INT_SIGNAL_ENABLE);
}

#[test]
fn a_read_or_write_of_the_wrong_shape_or_range_is_refused() {
    let mut dev = open(MockSdhci::healthy(0));
    let mut empty: [u8; 0] = [];
    assert_eq!(
        dev.read_blocks(0, &mut empty),
        Err(DriverError::BufferTooSmall)
    );
    let mut partial = [0u8; 200];
    assert_eq!(
        dev.read_blocks(0, &mut partial),
        Err(DriverError::BufferTooSmall)
    );
    assert_eq!(
        dev.write_blocks(0, &partial),
        Err(DriverError::BufferTooSmall)
    );
    // c_size 0 → 1024 blocks; LBA 1024 is one past the end.
    let mut block = [0u8; BS];
    assert_eq!(
        dev.read_blocks(1024, &mut block),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(
        dev.write_blocks(1024, &block),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(
        dev.read_blocks(u64::MAX, &mut block),
        Err(DriverError::LengthOutOfRange)
    );
}

#[test]
fn byte_addressed_pre_v2_and_csd_v1_cards_are_unsupported() {
    type BreakCard = fn(&mut MockSdhci);
    let cases: [(BreakCard, BringUpStage); 3] = [
        (|m| m.high_capacity = false, BringUpStage::OpCond),
        (|m| m.if_cond_echo = false, BringUpStage::SendIfCond),
        (|m| m.csd_structure_v2 = false, BringUpStage::SendCsd),
    ];
    for (break_card, stage) in cases {
        let mut mock = MockSdhci::healthy(7);
        break_card(&mut mock);
        assert_eq!(
            Emmc2::open(mock, Board::default()).err(),
            Some(BringUpFault {
                stage,
                error: DriverError::Unsupported
            })
        );
    }
}

#[test]
fn a_stalled_or_unpowered_controller_fails_closed_at_the_first_command() {
    let mut stalled = MockSdhci::healthy(7);
    stalled.stall = true;
    let mut unpowered = MockSdhci::healthy(7);
    unpowered.power_wired = false;
    for mock in [stalled, unpowered] {
        assert_eq!(
            Emmc2::open_with_budget(mock, Board::default(), 8).err(),
            Some(BringUpFault {
                stage: BringUpStage::GoIdle,
                error: DriverError::DeviceFault
            })
        );
    }
}

#[test]
fn a_silent_controller_fails_closed_instead_of_hanging() {
    let mut mock = MockSdhci::healthy_deferred(7);
    mock.silent = true;
    let Err(fault) = Emmc2::open(mock, Board::default()) else {
        panic!("a silent controller cannot identify")
    };
    assert_eq!(DriverError::from(fault), DriverError::DeviceFault);
}

#[test]
fn a_command_error_fails_the_transfer_closed() {
    for (index, write) in [(17, false), (24, true)] {
        let mut dev = open(MockSdhci::healthy(7));
        dev.host.error_on_index = Some(index);
        let mut block = [0u8; BS];
        let result = if write {
            dev.write_blocks(0, &block)
        } else {
            dev.read_blocks(0, &mut block)
        };
        assert_eq!(result, Err(DriverError::DeviceFault), "CMD{index}");
    }
}

#[test]
fn writes_persist_and_leave_their_neighbours_untouched() {
    let mut mock = MockSdhci::healthy(7);
    mock.fill_block(2, 0x20);
    mock.fill_block(6, 0x60);
    let mut dev = open(mock);
    let mut payload = MockSdhci::expected_block(0xA0);
    payload.extend_from_slice(&MockSdhci::expected_block(0xB0));
    payload.extend_from_slice(&MockSdhci::expected_block(0xC0));
    dev.write_blocks(3, &payload).expect("multi write");
    dev.write_blocks(5, &MockSdhci::expected_block(0xD0))
        .expect("single write");

    let mut buf = vec![0u8; 5 * BS];
    dev.read_blocks(2, &mut buf).expect("read back");
    let seeds = [0x20u8, 0xA0, 0xB0, 0xD0, 0x60];
    for (n, seed) in seeds.into_iter().enumerate() {
        assert_eq!(
            &buf[n * BS..(n + 1) * BS],
            MockSdhci::expected_block(seed).as_slice(),
            "block {}",
            2 + n
        );
    }
}

#[test]
fn every_write_is_confirmed_by_the_cards_status() {
    for dma in [false, true] {
        let mock = if dma {
            MockSdhci::healthy_dma(7, STORE_BLOCKS)
        } else {
            MockSdhci::healthy(7)
        };
        let mut dev = open(mock);
        dev.host.commands.clear();
        dev.write_blocks(0, &[0u8; BS]).expect("single write");
        dev.write_blocks(0, &[0u8; 2 * BS]).expect("multi write");
        let mut buf = [0u8; 2 * BS];
        dev.read_blocks(0, &mut buf).expect("read");
        assert_eq!(
            dev.host.command_indices(),
            [24, 13, 25, 13, 18],
            "dma {dma}: a write's status is collected, a read's is not"
        );
    }
}

#[test]
fn a_write_the_card_could_not_program_fails_closed() {
    let mut dev = open(MockSdhci::healthy(7));
    // Write protection violated: the card says so in the status after it.
    dev.host.pending_status_errors = 1 << 26;
    assert_eq!(
        dev.write_blocks(0, &[0u8; BS]),
        Err(DriverError::DeviceFault)
    );
    dev.host.commands.clear();
    dev.write_blocks(0, &[0u8; BS])
        .expect("the error was read and cleared");
    assert_eq!(
        dev.host.command_indices(),
        [13, 24, 13],
        "the card whose write failed is asked its state first"
    );
}

#[test]
fn a_programming_card_is_awaited_before_the_write_returns() {
    let mut dev = open(MockSdhci::healthy(7));
    dev.host.card_state = command::STATE_PRG;
    dev.host.commands.clear();
    let parks = dev.host.await_calls;
    dev.write_blocks(0, &[0u8; BS]).expect("write");
    assert_eq!(dev.host.command_indices(), [24, 13, 13, 13]);
    let busy = dev.host.commands[2];
    assert_eq!(
        (busy >> regs::CMD_RESP_TYPE_SHIFT) & 0b11,
        regs::RESP_48_BUSY,
        "the second ask waits on the card's busy"
    );
    assert!(
        dev.host.await_calls > parks,
        "the busy ended on the interrupt"
    );
}

#[test]
fn a_transfer_the_card_reports_failed_is_failed() {
    // Out of range, reported in the read command's own status.
    let mut dev = open(MockSdhci::healthy(7));
    dev.host.r1_errors_on = Some((18, 1 << 31));
    let mut buf = [0u8; 2 * BS];
    assert_eq!(dev.read_blocks(0, &mut buf), Err(DriverError::DeviceFault));
    assert_aborted_after_the_line_reset(&dev.host);
}

#[test]
fn multi_block_transfers_announce_their_length_when_the_card_takes_cmd23() {
    let mut dev = open(MockSdhci::healthy_dma(7, STORE_BLOCKS));
    assert!(dev.link().counted_transfers);
    let mut buf = [0u8; 3 * BS];
    dev.read_blocks(0, &mut buf).expect("read");
    dev.write_blocks(4, &buf[..2 * BS]).expect("write");
    dev.read_blocks(9, &mut buf[..BS]).expect("single read");
    assert_eq!(dev.host.counted, [3, 2], "a single block needs no count");
    assert!(dev.host.stops.is_empty(), "nothing needed stopping");
}

#[test]
fn a_card_without_cmd23_has_its_transfers_stopped() {
    let mut mock = MockSdhci::healthy(7);
    mock.scr[3] &= !0x02;
    let mut dev = open(mock);
    assert!(!dev.link().counted_transfers);
    dev.host.commands.clear();
    let mut buf = [0u8; 2 * BS];
    dev.read_blocks(0, &mut buf).expect("read");
    let read = dev.host.commands[0];
    assert_eq!(read & (0b11 << 2), regs::TM_AUTO_CMD12);
    assert!(dev.host.counted.is_empty());
}

#[test]
fn a_transfer_longer_than_one_command_is_split_on_the_pio_path() {
    let blocks = MAX_BLOCKS_PER_COMMAND + 2;
    let mut mock = MockSdhci::healthy(64);
    mock.store = vec![0u8; blocks * BS];
    mock.fill_block(0, 0x11);
    mock.fill_block(blocks - 1, 0x77);
    let mut dev = open(mock);
    dev.host.commands.clear();
    let mut buf = vec![0u8; blocks * BS];
    dev.read_blocks(0, &mut buf).expect("read");
    assert_eq!(&buf[..BS], MockSdhci::expected_block(0x11).as_slice());
    assert_eq!(
        &buf[(blocks - 1) * BS..],
        MockSdhci::expected_block(0x77).as_slice()
    );
    let full = u32::try_from(MAX_BLOCKS_PER_COMMAND).expect("fits");
    assert_eq!(dev.host.counted, [full, 2]);
}

#[test]
fn a_failed_multi_block_pio_transfer_is_aborted_after_the_line_reset() {
    for (index, write) in [(18, false), (25, true)] {
        let mut dev = open(MockSdhci::healthy(7));
        dev.host.error_on_index = Some(index);
        let mut buf = [0u8; 2 * BS];
        let result = if write {
            dev.write_blocks(0, &buf)
        } else {
            dev.read_blocks(0, &mut buf)
        };
        assert_eq!(result, Err(DriverError::DeviceFault));
        assert_aborted_after_the_line_reset(&dev.host);
    }
}

#[test]
fn a_failed_single_block_transfer_resets_the_lines_but_is_not_aborted() {
    for failing in [17, 24] {
        let mut dev = open(MockSdhci::healthy(7));
        dev.host.error_on_index = Some(failing);
        let mut block = [0u8; BS];
        let failed = if failing == 17 {
            dev.read_blocks(0, &mut block)
        } else {
            dev.write_blocks(0, &block)
        };
        assert_eq!(failed, Err(DriverError::DeviceFault));
        assert_eq!(dev.host.line_resets, 1, "the failure reset the lines");
        assert!(dev.host.stops.is_empty(), "no abort was sent");
    }
}

// --- ADMA2 DMA transfer path ----------------------------------------------

#[test]
fn bring_up_selects_adma2_only_with_staging() {
    let dma = open(MockSdhci::healthy_dma(7, STORE_BLOCKS));
    assert_eq!(
        dma.host().control0 & regs::CONTROL0_DMA_SELECT_MASK,
        regs::CONTROL0_DMA_SELECT_ADMA2
    );
    assert!(dma.link().dma);
    assert!(dma.host().power_on, "ADMA2 select preserved SD bus power");
    assert_ne!(dma.host().control0 & regs::CONTROL0_DATA_WIDTH_4BIT, 0);

    let pio = open(MockSdhci::healthy(7));
    assert_ne!(
        pio.host().control0 & regs::CONTROL0_DMA_SELECT_MASK,
        regs::CONTROL0_DMA_SELECT_ADMA2
    );
    assert!(!pio.link().dma);
}

#[test]
fn dma_that_lands_data_anywhere_but_the_staging_is_abandoned_at_bring_up() {
    for zero_block in [false, true] {
        let mut mock = MockSdhci::healthy_dma(7, STORE_BLOCKS);
        mock.dma_misdirected = true;
        if !zero_block {
            mock.fill_block(0, 0x42);
        }
        let mut dev = open(mock);
        let link = dev.link();
        assert!(!link.dma, "zero block {zero_block}");
        assert_eq!(
            link.dma_fallback,
            Some(BringUpFault {
                stage: BringUpStage::VerifyDma,
                error: DriverError::DeviceFault
            }),
            "even a block of zeroes is told apart from staging that was never written"
        );
        assert_eq!(dev.host.control0 & regs::CONTROL0_DMA_SELECT_MASK, 0);
        dev.host.fill_block(2, 0x24);
        let mut buf = [0u8; BS];
        dev.read_blocks(2, &mut buf)
            .expect("reads over the data port");
        assert_eq!(buf.as_slice(), MockSdhci::expected_block(0x24).as_slice());
    }
}

#[test]
fn dma_that_errors_is_abandoned_at_bring_up_for_the_data_port() {
    let mut mock = MockSdhci::healthy_dma(7, STORE_BLOCKS);
    mock.dma_fails = true;
    let mut dev = open(mock);
    assert!(!dev.link().dma);
    assert_eq!(
        dev.link().dma_fallback.map(|f| f.stage),
        Some(BringUpStage::VerifyDma)
    );
    dev.write_blocks(1, &MockSdhci::expected_block(0x99))
        .expect("writes over the data port");
}

#[test]
fn a_controller_without_adma2_stays_on_pio_even_with_staging() {
    let mut mock = MockSdhci::healthy_dma(7, STORE_BLOCKS);
    mock.caps &= !(1 << 19);
    let dev = open(mock);
    assert!(!dev.link().dma);
}

#[test]
fn dma_read_publishes_the_table_and_data_then_consumes_the_data() {
    let mut mock = MockSdhci::healthy_dma(7, STORE_BLOCKS);
    mock.fill_block(3, 0x40);
    let mut dev = open(mock);
    dev.host.dma_syncs.clear();
    let mut buf = [0u8; BS];
    dev.read_blocks(3, &mut buf).expect("dma read");
    assert_eq!(buf.as_slice(), MockSdhci::expected_block(0x40).as_slice());
    assert_eq!(
        dev.host().dma_syncs,
        [
            (DmaArea::Table, 0, adma::DESC_BYTES),
            (DmaArea::Data, 0, BS),
            (DmaArea::Data, 0, BS),
        ]
    );
}

#[test]
fn a_dma_transfer_longer_than_the_staging_is_split_and_reassembled() {
    let total = DMA_STAGE_BLOCKS + 40;
    let mut mock = MockSdhci::healthy_dma(0, total + 8);
    mock.fill_blocks(0, total, 0x01);
    let mut dev = open(mock);
    dev.host.dma_syncs.clear();
    let mut buf = vec![0u8; total * BS];
    dev.read_blocks(0, &mut buf).expect("dma read");
    for n in 0..total {
        assert_eq!(
            &buf[n * BS..(n + 1) * BS],
            MockSdhci::expected_block(MockSdhci::nth_seed(0x01, n)).as_slice(),
            "block {n}"
        );
    }
    let tail = 40 * BS;
    assert_eq!(
        dev.host().dma_syncs,
        [
            (DmaArea::Table, 0, DMA_TABLE_BYTES),
            (DmaArea::Data, 0, DMA_DATA_BYTES),
            (DmaArea::Data, 0, DMA_DATA_BYTES),
            (DmaArea::Table, 0, adma::DESC_BYTES),
            (DmaArea::Data, 0, tail),
            (DmaArea::Data, 0, tail),
        ],
        "one full window, then the tail"
    );
    let window = u32::try_from(DMA_STAGE_BLOCKS).expect("fits");
    assert_eq!(dev.host().counted, [window, 40]);
}

#[test]
fn a_dma_write_then_read_round_trips_across_windows() {
    let total = DMA_STAGE_BLOCKS + 5;
    let mut dev = open(MockSdhci::healthy_dma(0, total + 8));
    let mut payload = vec![0u8; total * BS];
    for (n, block) in payload.chunks_mut(BS).enumerate() {
        block.copy_from_slice(&MockSdhci::expected_block(MockSdhci::nth_seed(0x80, n)));
    }
    dev.write_blocks(0, &payload).expect("dma write");
    let mut buf = vec![0u8; total * BS];
    dev.read_blocks(0, &mut buf).expect("dma read back");
    assert_eq!(buf, payload);
}

#[test]
fn a_sensitive_transfer_leaves_no_copy_in_the_staging() {
    let mut mock = MockSdhci::healthy_dma(7, STORE_BLOCKS);
    mock.fill_blocks(0, 2, 0x5A);
    let mut dev = open(mock);
    let mut buf = [0u8; 2 * BS];

    dev.read_blocks_with_class(0, &mut buf, BufferClass::NonSensitive)
        .expect("ordinary read");
    assert!(
        dev.host.dma_data[..2 * BS].iter().any(|&b| b != 0),
        "an ordinary read's staging is left"
    );

    dev.read_blocks_with_class(0, &mut buf, BufferClass::Sensitive)
        .expect("sensitive read");
    assert_eq!(&buf[..BS], MockSdhci::expected_block(0x5A).as_slice());
    assert!(dev.host.dma_data[..2 * BS].iter().all(|&b| b == 0));
    assert_eq!(dev.host.dma_syncs.last(), Some(&(DmaArea::Data, 0, 2 * BS)));

    let secret = [0xC3u8; BS];
    dev.write_blocks_with_class(4, &secret, BufferClass::Sensitive)
        .expect("sensitive write");
    assert!(dev.host.dma_data[..BS].iter().all(|&b| b == 0));
    dev.host.error_on_index = Some(24);
    assert!(dev
        .write_blocks_with_class(4, &secret, BufferClass::Sensitive)
        .is_err());
    assert!(
        dev.host.dma_data[..BS].iter().all(|&b| b == 0),
        "a failed one too"
    );
}

#[test]
fn a_failed_dma_transfer_resets_the_lines_before_the_staging_is_reused() {
    let mut mock = MockSdhci::healthy_dma(7, STORE_BLOCKS);
    mock.fill_blocks(0, 1, 0x30);
    let mut dev = open(mock);
    dev.host.error_on_index = Some(17);
    let mut buf = [0u8; BS];
    assert_eq!(dev.read_blocks(0, &mut buf), Err(DriverError::DeviceFault));
    assert_eq!(dev.host.line_resets, 1);
    assert!(dev.host.stops.is_empty(), "a single block is not aborted");
    dev.host.error_on_index = None;
    dev.read_blocks(0, &mut buf)
        .expect("the controller recovered, so the staging is reused");
    assert_eq!(buf.as_slice(), MockSdhci::expected_block(0x30).as_slice());
}

#[test]
fn a_controller_that_will_not_recover_is_handed_the_staging_no_more() {
    let mut dev = open(MockSdhci::healthy_dma(7, STORE_BLOCKS));
    let withheld = alloc::rc::Rc::clone(&dev.host.withheld);
    dev.host.error_on_index = Some(17);
    dev.host.lines_stuck = true;
    let mut buf = [0u8; BS];
    assert_eq!(dev.read_blocks(0, &mut buf), Err(DriverError::DeviceFault));
    let programmed = dev.host.adma_addr;
    dev.host.adma_addr = 0;
    dev.host.error_on_index = None;
    assert_eq!(dev.read_blocks(0, &mut buf), Err(DriverError::DeviceFault));
    assert_eq!(
        dev.host.adma_addr, 0,
        "nothing was programmed over the held staging"
    );
    assert_ne!(programmed, 0);
    drop(dev);
    assert!(withheld.get(), "and it is never returned");
}

#[test]
fn a_failed_multi_block_dma_transfer_is_aborted_after_the_line_reset() {
    for (index, write) in [(18, false), (25, true)] {
        let mut mock = MockSdhci::healthy_dma(7, STORE_BLOCKS);
        mock.fill_blocks(0, 2, 0x30);
        let mut dev = open(mock);
        dev.host.error_on_index = Some(index);
        let mut buf = [0u8; 2 * BS];
        let result = if write {
            dev.write_blocks(0, &buf)
        } else {
            dev.read_blocks(0, &mut buf)
        };
        assert_eq!(result, Err(DriverError::DeviceFault));
        assert_aborted_after_the_line_reset(&dev.host);
        assert_eq!(
            dev.host.interrupt & regs::INT_DATA_DONE,
            0,
            "the abort's busy was waited out"
        );
        dev.host.error_on_index = None;
        dev.read_blocks(0, &mut buf)
            .expect("a recovered controller is handed the staging again");
    }
}

#[test]
fn an_unanswered_abort_leaves_the_transfer_error_standing_and_dma_usable() {
    let mut mock = MockSdhci::healthy_dma(7, STORE_BLOCKS);
    mock.fill_blocks(0, 2, 0x50);
    let mut dev = open(mock);
    let withheld = alloc::rc::Rc::clone(&dev.host.withheld);
    dev.host.error_on_index = Some(18);
    dev.host.stop_unanswered = true;
    let mut buf = [0u8; 2 * BS];
    assert_eq!(dev.read_blocks(0, &mut buf), Err(DriverError::DeviceFault));
    assert_aborted_after_the_line_reset(&dev.host);
    dev.host.error_on_index = None;
    dev.host.stop_unanswered = false;
    dev.read_blocks(0, &mut buf)
        .expect("the line reset halted the engine, so the staging is reused");
    assert_eq!(&buf[..BS], MockSdhci::expected_block(0x50).as_slice());
    drop(dev);
    assert!(!withheld.get(), "and returned to its pool");
}

#[test]
fn a_controller_whose_line_reset_never_confirms_is_sent_no_abort() {
    let mut dev = open(MockSdhci::healthy_dma(7, STORE_BLOCKS));
    let withheld = alloc::rc::Rc::clone(&dev.host.withheld);
    dev.host.error_on_index = Some(18);
    dev.host.lines_stuck = true;
    let mut buf = [0u8; 2 * BS];
    assert_eq!(dev.read_blocks(0, &mut buf), Err(DriverError::DeviceFault));
    assert!(
        dev.host.stops.is_empty(),
        "a controller still in reset takes no command"
    );
    drop(dev);
    assert!(withheld.get(), "and the staging stays withheld");
}

// --- The card-state check ---------------------------------------------------

/// Open `mock`, then fail one single-block read, from which the recovery
/// cannot prove the card back in `tran`, and clear the command log.
fn with_unknown_card_state(mock: MockSdhci) -> Emmc2<MockSdhci> {
    let mut dev = open(mock);
    dev.host.error_on_index = Some(17);
    let mut block = [0u8; BS];
    assert_eq!(
        dev.read_blocks(0, &mut block),
        Err(DriverError::DeviceFault)
    );
    dev.host.error_on_index = None;
    dev.host.commands.clear();
    dev
}

/// Assert the next read is refused at the state check alone, then that once
/// `heal` fixes the card the read after it asks again and proceeds.
fn assert_refused_then_asked_again(dev: &mut Emmc2<MockSdhci>, heal: impl FnOnce(&mut MockSdhci)) {
    let mut block = [0u8; BS];
    assert_eq!(
        dev.read_blocks(0, &mut block),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(
        dev.host.command_indices(),
        [13],
        "no data command reached the card"
    );
    heal(&mut dev.host);
    dev.host.commands.clear();
    dev.read_blocks(0, &mut block)
        .expect("the healed card reads");
    assert_eq!(
        dev.host.command_indices(),
        [13, 17],
        "the state was kept unknown"
    );
}

#[test]
fn a_healthy_card_is_asked_its_state_only_after_a_write() {
    for mock in [
        MockSdhci::healthy(7),
        MockSdhci::healthy_dma(7, STORE_BLOCKS),
    ] {
        let mut dev = open(mock);
        dev.host.commands.clear();
        let mut buf = vec![0u8; 2 * BS];
        dev.read_blocks(0, &mut buf[..BS]).expect("single read");
        dev.read_blocks(2, &mut buf).expect("multi read");
        assert_eq!(dev.host.command_indices(), [17, 18]);
    }
}

#[test]
fn an_answered_abort_proves_the_card_in_tran_so_the_next_command_asks_nothing() {
    let mut dev = open(MockSdhci::healthy(7));
    dev.host.error_on_index = Some(25);
    let payload = [0u8; 2 * BS];
    assert_eq!(dev.write_blocks(0, &payload), Err(DriverError::DeviceFault));
    dev.host.error_on_index = None;
    dev.host.commands.clear();
    dev.write_blocks(0, &payload).expect("write");
    assert_eq!(dev.host.command_indices(), [25, 13]);
}

#[test]
fn a_failed_single_block_transfer_checks_the_card_state_before_the_next_command() {
    let mut dev = with_unknown_card_state(MockSdhci::healthy(7));
    let mut block = [0u8; BS];
    dev.read_blocks(0, &mut block)
        .expect("the card answered tran");
    assert_eq!(dev.host.command_indices(), [13, 17]);
    dev.host.commands.clear();
    dev.read_blocks(0, &mut block).expect("read");
    assert_eq!(
        dev.host.command_indices(),
        [17],
        "a card proven in tran is not asked again"
    );
}

#[test]
fn an_unanswered_abort_leaves_the_card_state_for_the_next_command_to_check() {
    let mut dev = open(MockSdhci::healthy(7));
    dev.host.error_on_index = Some(18);
    dev.host.stop_unanswered = true;
    let mut buf = [0u8; 2 * BS];
    assert_eq!(dev.read_blocks(0, &mut buf), Err(DriverError::DeviceFault));
    dev.host.error_on_index = None;
    dev.host.stop_unanswered = false;
    dev.host.commands.clear();
    dev.read_blocks(0, &mut buf)
        .expect("the card answered tran");
    assert_eq!(dev.host.command_indices(), [13, 18]);
}

#[test]
fn a_recovery_whose_line_reset_never_confirms_leaves_the_card_state_unknown() {
    let mut dev = open(MockSdhci::healthy(7));
    dev.host.error_on_index = Some(18);
    dev.host.lines_stuck = true;
    let mut buf = [0u8; 2 * BS];
    assert_eq!(dev.read_blocks(0, &mut buf), Err(DriverError::DeviceFault));
    assert!(dev.host.stops.is_empty(), "no abort reached the card");
    dev.host.error_on_index = None;
    dev.host.lines_stuck = false;
    dev.host.commands.clear();
    dev.read_blocks(0, &mut buf)
        .expect("the lines reset, so the card can be asked");
    assert_eq!(dev.host.command_indices(), [13, 18]);
}

#[test]
fn a_card_still_sending_or_receiving_is_aborted_and_asked_again() {
    let mut dev = with_unknown_card_state(MockSdhci::healthy(7));
    dev.host.card_state = command::STATE_DATA;
    let mut block = [0u8; BS];
    dev.read_blocks(0, &mut block)
        .expect("the aborted card reached tran");
    assert_eq!(dev.host.command_indices(), [13, 12, 13, 17]);

    let mut dev = with_unknown_card_state(MockSdhci::healthy(7));
    dev.host.card_state = command::STATE_RCV;
    dev.write_blocks(0, &block)
        .expect("the aborted card reached tran");
    assert_eq!(dev.host.command_indices(), [13, 12, 13, 24, 13]);
}

#[test]
fn a_programming_card_is_awaited_on_its_busy_interrupt_before_the_next_command() {
    let mut dev = with_unknown_card_state(MockSdhci::healthy(7));
    dev.host.card_state = command::STATE_PRG;
    let parks = dev.host.await_calls;
    dev.write_blocks(0, &[0u8; BS])
        .expect("the card finished programming");
    assert_eq!(dev.host.command_indices(), [13, 13, 13, 24, 13]);
    let response = |word: u32| (word >> regs::CMD_RESP_TYPE_SHIFT) & 0b11;
    assert_eq!(response(dev.host.commands[0]), regs::RESP_48);
    assert_eq!(response(dev.host.commands[1]), regs::RESP_48_BUSY);
    assert!(dev.host.await_calls > parks);
}

#[test]
fn a_card_in_an_unexpected_state_or_unanswering_is_asked_again_next_time() {
    let mut dev = with_unknown_card_state(MockSdhci::healthy(7));
    dev.host.card_state = STATE_STBY;
    assert_refused_then_asked_again(&mut dev, |host| host.card_state = command::STATE_TRAN);

    let mut dev = with_unknown_card_state(MockSdhci::healthy(7));
    dev.host.error_on_index = Some(13);
    assert_refused_then_asked_again(&mut dev, |host| host.error_on_index = None);
}

#[test]
fn a_card_that_stays_sending_after_every_abort_fails_closed_after_the_bound() {
    let mut dev = with_unknown_card_state(MockSdhci::healthy(7));
    dev.host.card_state = command::STATE_DATA;
    dev.host.ignored_aborts = CARD_STATE_ROUNDS;
    let mut block = [0u8; BS];
    assert_eq!(
        dev.read_blocks(0, &mut block),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(
        dev.host.command_indices(),
        [13u8, 12].repeat(CARD_STATE_ROUNDS)
    );
}

#[test]
fn every_transfer_path_asks_a_card_of_unknown_state_before_its_command() {
    for dma in [false, true] {
        for (write, blocks, expected) in [
            (false, 1, &[13u8, 17][..]),
            (false, 2, &[13, 18]),
            (true, 1, &[13, 24, 13]),
            (true, 2, &[13, 25, 13]),
        ] {
            let mock = if dma {
                MockSdhci::healthy_dma(7, STORE_BLOCKS)
            } else {
                MockSdhci::healthy(7)
            };
            let mut dev = with_unknown_card_state(mock);
            let mut buf = vec![0u8; blocks * BS];
            let moved = if write {
                dev.write_blocks(0, &buf)
            } else {
                dev.read_blocks(0, &mut buf)
            };
            moved.expect("the card answered tran");
            assert_eq!(dev.host.command_indices(), expected, "dma {dma}");
        }
    }
}

// --- `wiring` capability gate ---------------------------------------------

/// A completion and delay seam for the `wiring` tests, which stop at the
/// capability or mapper gate or at a controller that never resets.
struct NoIrq;

impl crate::CompletionWait for NoIrq {
    fn await_irq(&self) -> CompletionSignal {
        CompletionSignal::Fired
    }
}

impl Delay for NoIrq {
    fn delay_us(&self, _us: u32) {}

    fn now_us(&self) -> u64 {
        0
    }
}

/// A RAM-backed mapper: plain memory, which models no controller.
struct MockMapper {
    phys: u64,
    backing: Vec<u32>,
    granted: bool,
}

impl MmioMapper for MockMapper {
    fn map_window(&self, phys_base: u64, len: usize) -> Result<RegisterWindow, MmioMapError> {
        if !self.granted {
            return Err(MmioMapError::CapabilityMissing);
        }
        if phys_base != self.phys || len == 0 || len > self.backing.len() * 4 {
            return Err(MmioMapError::Unsupported);
        }
        let base = NonNull::new(self.backing.as_ptr() as *mut u8).expect("non-null heap buffer");
        // SAFETY: `base` covers `backing.len() * 4 >= len` bytes, 4-byte
        // aligned as a `Vec<u32>` allocation is; the backing outlives the
        // window within the test, which never reads it concurrently.
        Ok(unsafe { RegisterWindow::from_mapping(phys_base, base, len) })
    }
}

struct MockHost {
    drv_load: bool,
    mmio_map: bool,
    mapper: Option<MockMapper>,
    dma: Option<QuiesceProbe>,
}

/// A DMA facility that carves nothing and counts the quiesce declarations
/// it receives.
#[derive(Default)]
struct QuiesceProbe {
    declared: core::cell::Cell<usize>,
}

impl DmaHost for QuiesceProbe {
    fn alloc_dma_zeroed(&self, _size: usize) -> Result<DmaSlab, DriverError> {
        Err(DriverError::LengthOutOfRange)
    }

    fn device_quiesced(&self) {
        self.declared.set(self.declared.get() + 1);
    }
}

impl DriverHost for MockHost {
    fn has_capability(&self, cap: CapabilityId) -> bool {
        match cap {
            CapabilityId::DRV_LOAD => self.drv_load,
            CapabilityId::MMIO_MAP => self.mmio_map,
            _ => false,
        }
    }
    fn kind(&self) -> DriverKind {
        DriverKind::UserSpace
    }
    fn mmio_mapper(&self) -> Option<&dyn MmioMapper> {
        self.mapper.as_ref().map(|m| m as &dyn MmioMapper)
    }
    fn dma_host(&self) -> Option<&dyn DmaHost> {
        self.dma.as_ref().map(|d| d as &dyn DmaHost)
    }
}

const EMMC2_PHYS: u64 = 0xFE34_0000;

fn mapper() -> MockMapper {
    MockMapper {
        phys: EMMC2_PHYS,
        backing: vec![0u32; regs::REGS_LEN_BYTES / 4],
        granted: true,
    }
}

#[test]
fn register_requires_drv_load() {
    let host = |drv_load| MockHost {
        drv_load,
        mmio_map: false,
        mapper: None,
        dma: None,
    };
    assert!(register(&host(true)).is_ok());
    assert_eq!(register(&host(false)), Err(DriverError::PermissionDenied));
}

#[test]
fn open_discovered_requires_mmio_map_and_a_mapper() {
    let unmapped = MockHost {
        drv_load: true,
        mmio_map: false,
        mapper: Some(mapper()),
        dma: None,
    };
    let no_mapper = MockHost {
        drv_load: true,
        mmio_map: true,
        mapper: None,
        dma: None,
    };
    for (host, error) in [
        (unmapped, DriverError::PermissionDenied),
        (no_mapper, DriverError::Unsupported),
    ] {
        assert_eq!(
            wiring::open_discovered(&host, EMMC2_PHYS, NoIrq, Board::default()).err(),
            Some(BringUpFault {
                stage: BringUpStage::MapWindow,
                error
            })
        );
    }
}

#[test]
fn a_controller_whose_reset_never_completes_is_never_declared_quiesced() {
    let host = MockHost {
        drv_load: true,
        mmio_map: true,
        mapper: Some(mapper()),
        dma: Some(QuiesceProbe::default()),
    };
    // Plain memory never clears the reset bit it is written.
    assert!(wiring::open_discovered(&host, EMMC2_PHYS, NoIrq, Board::default()).is_err());
    assert_eq!(host.dma.as_ref().map(|dma| dma.declared.get()), Some(0));
}

#[test]
fn bind_table_matches_the_bcm2711_emmc2_node() {
    assert_eq!(BIND_KEYS.len(), 1);
    assert_eq!(BIND_KEYS[0].priority, BIND_PRIORITY);
    let emmc2 = HwMatchKey::compatible(b"brcm,bcm2711-emmc2").expect("fits");
    assert!(BIND_KEYS[0].key.matches(&emmc2));
    let pcie = HwMatchKey::compatible(b"brcm,bcm2711-pcie").expect("fits");
    assert!(!BIND_KEYS[0].key.matches(&pcie));
}

#[test]
fn bring_up_stage_names_every_step_distinctly() {
    use alloc::collections::BTreeSet;
    let stages = [
        BringUpStage::MapWindow,
        BringUpStage::ResetClock,
        BringUpStage::InitialSignalling,
        BringUpStage::GoIdle,
        BringUpStage::SendIfCond,
        BringUpStage::OpCond,
        BringUpStage::VoltageSwitch,
        BringUpStage::AllSendCid,
        BringUpStage::SendRelativeAddr,
        BringUpStage::SendCsd,
        BringUpStage::SelectCard,
        BringUpStage::SetBlockLen,
        BringUpStage::RaiseClock,
        BringUpStage::SendScr,
        BringUpStage::SetBusWidth,
        BringUpStage::SwitchFunction,
        BringUpStage::SetBusTiming,
        BringUpStage::VerifyBus,
        BringUpStage::SelectDma,
        BringUpStage::VerifyDma,
        BringUpStage::PowerCycle,
    ];
    let names: BTreeSet<&'static str> = stages.iter().map(|s| s.as_str()).collect();
    assert_eq!(names.len(), stages.len());
    assert!(names.iter().all(|n| !n.is_empty()));
}

#[test]
fn bring_up_fault_converts_to_its_driver_error() {
    let fault = BringUpFault {
        stage: BringUpStage::OpCond,
        error: DriverError::DeviceFault,
    };
    assert_eq!(DriverError::from(fault), DriverError::DeviceFault);
}
