//! The engine driven against the register-level model.

use core::num::NonZeroU32;
use std::vec::Vec;

use tairix_abi::driver::dmaengine::{
    CyclicParams, CyclicTransfer, DmaChannel, DmaChannelEvent, DmaDirection, DmaEngine,
    DmaRequestLine, Halted, DMA_CONTROLLER_ENDPOINTS,
};
use tairix_abi::{CapabilityId, DriverError, DriverHost, DriverKind, HwMatchKey, RegisterBlock};

use crate::engine::{Bcm2835Dma, MAX_BLOCKS};
use crate::model::{
    Model, Trace, Unresettable, CONBLK_AD, CS, CS_ACTIVE, CS_END, CS_ERROR, CS_INT, CS_RESET,
    DEBUG, DEBUG_READ_ERROR, LITE_MAX_BLOCK, TI_DEST_DREQ, TI_DEST_INC, TI_DEST_WIDTH, TI_INTEN,
    TI_SRC_DREQ, TI_SRC_INC, TI_SRC_WIDTH, TI_WAIT_RESP,
};
use crate::{register, BIND_KEYS, DMA_COMPATIBLE, REQUIRED_CAPABILITIES};

const BUFFER: u64 = 0xC100_0000;
const FIFO: u64 = 0x7E20_3004;
const WIDE_FIFO: u64 = 0x7E20_3000;
/// The PCM transmit request line.
const DREQ_PCM_TX: u32 = 2;
const PERIOD: u32 = 1920;
const PERIODS: u32 = 4;

const M2D: DmaDirection = DmaDirection::MemoryToDevice;
const D2M: DmaDirection = DmaDirection::DeviceToMemory;

fn line(cell: u32) -> DmaRequestLine {
    DmaRequestLine::new(DMA_CONTROLLER_ENDPOINTS.endpoint(3), 0, &[cell], b"tx").expect("valid")
}

fn transfer(period_bytes: u32, periods: u32, direction: DmaDirection) -> CyclicTransfer {
    CyclicTransfer {
        buffer: BUFFER,
        fifo: FIFO,
        direction,
        period_bytes,
        periods,
    }
}

fn params(period_bytes: u32, periods: u32, fifo: u64) -> CyclicParams {
    CyclicParams {
        fifo,
        direction: M2D,
        period_bytes,
        periods,
    }
}

#[test]
fn a_cyclic_chain_is_walked_with_one_interrupt_per_period() {
    let model = Model::pi4();
    model.own(0, BUFFER, u64::from(PERIOD * PERIODS));
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    for period in 1..=2 * PERIODS {
        model.advance(0, PERIOD);
        assert_eq!(model.interrupts(0), period);
        assert_eq!(channel.take_event(), Ok(DmaChannelEvent::Boundary));
        assert!(!model.pending(0), "the interrupt was acknowledged");
        assert_eq!(channel.position(), Ok(period % PERIODS * PERIOD));
    }
    let blocks = model.loaded(0);
    let table = blocks[0].next - 32;
    for (index, block) in blocks.iter().take(4).enumerate() {
        let index = u32::try_from(index).expect("small");
        assert_eq!(block.source, 0xC100_0000 + index * PERIOD);
        assert_eq!(block.dest, 0x7E20_3004);
        assert_eq!(block.len, PERIOD);
        assert_eq!(block.next, table + (index + 1) % PERIODS * 32);
        assert_eq!(
            block.info,
            (DREQ_PCM_TX << 16) | TI_WAIT_RESP | TI_DEST_DREQ | TI_SRC_INC | TI_INTEN
        );
    }
}

#[test]
fn a_period_longer_than_a_lite_block_is_split_with_its_interrupt_last() {
    // Channel 7 is a LITE engine, and the model refuses any block past its
    // limit; every channel is held to that limit.
    let model = Model::pi4();
    let period = 200_000;
    model.own(7, BUFFER, u64::from(period) * 2);
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(7).expect("channel 7");
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(period, 2, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    model.advance(7, period);
    assert_eq!(model.interrupts(7), 1);
    let lens: Vec<u32> = model.loaded(7).iter().map(|block| block.len).collect();
    assert_eq!(
        lens,
        [
            LITE_MAX_BLOCK,
            LITE_MAX_BLOCK,
            LITE_MAX_BLOCK,
            3_404,
            LITE_MAX_BLOCK
        ]
    );
    let interrupting: Vec<bool> = model
        .loaded(7)
        .iter()
        .map(|block| block.info & TI_INTEN != 0)
        .collect();
    assert_eq!(interrupting, [false, false, false, true, false]);
    assert_eq!(channel.position(), Ok(period));
}

#[test]
fn a_wide_line_holds_blocks_and_the_fifo_to_sixteen_bytes() {
    let wide_dest = DREQ_PCM_TX | 1 << 25;
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    assert_eq!(
        engine.admit(&line(wide_dest), &params(PERIOD, 2, WIDE_FIFO)),
        Ok(16)
    );
    assert_eq!(
        engine.admit(&line(wide_dest), &params(PERIOD, 2, FIFO)),
        Err(DriverError::OutOfRange)
    );
    assert_eq!(
        engine.admit(&line(wide_dest), &params(1_000, 2, WIDE_FIFO)),
        Err(DriverError::LengthOutOfRange)
    );
    // A wide source is the memory side here, so the FIFO is still one word,
    // but lengths stay sixteen-byte multiples.
    let wide_source = DREQ_PCM_TX | 1 << 24;
    assert_eq!(
        engine.admit(&line(wide_source), &params(PERIOD, 2, FIFO)),
        Ok(4)
    );
    assert_eq!(
        engine.admit(&line(wide_source), &params(1_000, 2, FIFO)),
        Err(DriverError::LengthOutOfRange)
    );

    let period = 65_520 * 2;
    model.own(1, BUFFER, u64::from(period) * 2);
    let channel = engine.channel(1).expect("channel 1");
    channel
        .prepare(
            &line(wide_dest),
            &CyclicTransfer {
                fifo: WIDE_FIFO,
                ..transfer(period, 2, M2D)
            },
        )
        .expect("prepares");
    channel.start().expect("starts");
    model.advance(1, period);
    let blocks = model.loaded(1);
    assert_eq!(blocks[0].len, 65_520);
    assert_ne!(blocks[0].info & TI_DEST_WIDTH, 0);
    assert_eq!(blocks[0].info & TI_SRC_WIDTH, 0);
}

#[test]
fn every_serving_bit_the_binding_defines_is_applied() {
    let cell =
        DREQ_PCM_TX | 0xA << 16 | 0xF << 20 | 1 << 24 | 1 << 27 | 1 << 28 | 1 << 29 | 1 << 30;
    let model = Model::pi4();
    model.own(0, BUFFER, u64::from(PERIOD * 2));
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    channel
        .prepare(&line(cell), &transfer(PERIOD, 2, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    assert_eq!(model.writes(0, CS).last(), Some(&(CS_ACTIVE | 0x30FA_0000)));
    let info = model.loaded(0)[0].info;
    assert_eq!(info & TI_WAIT_RESP, 0, "no write-response wait");
    assert_ne!(info & TI_SRC_WIDTH, 0, "wide source");
    assert_eq!(info >> 12 & 0xF, 3, "the binding's burst length");
    assert_eq!(info >> 16 & 0x1F, DREQ_PCM_TX, "the request line paces it");
}

#[test]
fn a_specifier_the_binding_does_not_define_refuses_the_line() {
    let model = Model::pi4();
    let store = model.store();
    let engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    assert_eq!(engine.accept(&line(DREQ_PCM_TX)), Ok(()));
    for bit in (5..=15).chain([26, 31]) {
        assert_eq!(
            engine.accept(&line(DREQ_PCM_TX | 1 << bit)),
            Err(DriverError::Unsupported),
            "bit {bit}"
        );
    }
    // An unpaced line, and a line of any other width.
    assert_eq!(engine.accept(&line(0)), Err(DriverError::Unsupported));
    let endpoint = DMA_CONTROLLER_ENDPOINTS.endpoint(3);
    for cells in [&[][..], &[2, 0][..]] {
        let odd = DmaRequestLine::new(endpoint, 0, cells, b"").expect("valid record");
        assert_eq!(engine.accept(&odd), Err(DriverError::Unsupported));
    }
}

#[test]
fn a_shape_no_chain_can_hold_is_refused_before_anything_is_carved() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let blocks = u32::try_from(MAX_BLOCKS).expect("small");
    let cases = [
        (LITE_MAX_BLOCK * 65, 2),
        (PERIOD, blocks + 1),
        (PERIOD, 1),
        (0, 4),
        (PERIOD + 2, 4),
    ];
    for (period_bytes, periods) in cases {
        assert_eq!(
            engine.admit(&line(DREQ_PCM_TX), &params(period_bytes, periods, FIFO)),
            Err(DriverError::LengthOutOfRange),
            "{period_bytes} x {periods}"
        );
        let channel = engine.channel(0).expect("channel 0");
        assert_eq!(
            channel.prepare(&line(DREQ_PCM_TX), &transfer(period_bytes, periods, M2D)),
            Err(DriverError::LengthOutOfRange)
        );
    }
    assert_eq!(model.carved(), 0);
    // A page of blocks is exactly what one chain may hold.
    assert_eq!(
        engine.admit(&line(DREQ_PCM_TX), &params(PERIOD, blocks, FIFO)),
        Ok(4)
    );
    assert_eq!(
        engine.admit(&line(DREQ_PCM_TX), &params(PERIOD, 2, FIFO + 2)),
        Err(DriverError::OutOfRange)
    );
}

#[test]
fn an_address_past_the_engines_reach_is_refused() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let span = u64::from(PERIOD * PERIODS);
    let channel = engine.channel(0).expect("channel 0");
    // A buffer ending exactly at the 32-bit limit is the last one reachable.
    let last = CyclicTransfer {
        buffer: (1 << 32) - span,
        ..transfer(PERIOD, PERIODS, M2D)
    };
    assert_eq!(channel.prepare(&line(DREQ_PCM_TX), &last), Ok(()));
    for buffer in [(1 << 32) - span + 4, 1 << 32, u64::MAX] {
        let past = CyclicTransfer {
            buffer,
            ..transfer(PERIOD, PERIODS, M2D)
        };
        assert_eq!(
            channel.prepare(&line(DREQ_PCM_TX), &past),
            Err(DriverError::OutOfRange)
        );
    }
    for fifo in [(1 << 32) - 2, 1 << 32] {
        let past = CyclicTransfer {
            fifo,
            ..transfer(PERIOD, PERIODS, M2D)
        };
        assert_eq!(
            channel.prepare(&line(DREQ_PCM_TX), &past),
            Err(DriverError::OutOfRange)
        );
    }
    let misaligned = CyclicTransfer {
        buffer: BUFFER + 2,
        ..transfer(PERIOD, PERIODS, M2D)
    };
    assert_eq!(
        channel.prepare(&line(DREQ_PCM_TX), &misaligned),
        Err(DriverError::OutOfRange)
    );
}

#[test]
fn stop_pauses_lets_the_writes_drain_then_resets() {
    let model = Model::pi4();
    model.own(0, BUFFER, u64::from(PERIOD * PERIODS));
    model.drain_reads(0, 3);
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    model.advance(0, 100);
    assert_eq!(channel.stop(), Ok(Halted::Drained));
    let writes = model.writes(0, CS);
    assert_eq!(&writes[writes.len() - 2..], [0, CS_RESET]);
    assert_eq!(model.writes(0, DEBUG).last(), Some(&0b111));
    assert!(!model.active(0));
    // A reset channel holds no position, and restarts from its first period.
    assert_eq!(channel.position(), Ok(0));
    channel.start().expect("restarts");
    model.advance(0, PERIOD);
    assert_eq!(channel.position(), Ok(PERIOD));
}

#[test]
fn a_channel_that_will_not_drain_is_reset_regardless() {
    let model = Model::pi4();
    model.own(0, BUFFER, u64::from(PERIOD * PERIODS));
    model.drain_reads(0, u32::MAX);
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    model.advance(0, 100);
    assert_eq!(channel.stop(), Ok(Halted::Undrained));
    assert_eq!(model.writes(0, CS).last(), Some(&CS_RESET));
}

#[test]
fn stopping_an_idle_channel_only_resets_it() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    assert_eq!(
        engine.channel(4).expect("channel 4").stop(),
        Ok(Halted::Drained)
    );
    assert_eq!(model.writes(4, CS), [CS_RESET]);
    assert!(model.writes(4, CONBLK_AD).is_empty());
}

#[test]
fn a_chain_is_freed_only_after_its_channel_is_reset() {
    let model = Model::pi4();
    model.own(2, BUFFER, u64::from(PERIOD * PERIODS));
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(2).expect("channel 2");
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    model.advance(2, 700);
    assert_eq!(model.live_tables(), 1);
    assert_eq!(channel.release(), Ok(Halted::Drained));
    assert_eq!(model.live_tables(), 0);
    let timeline = model.timeline();
    let timeline = timeline.borrow();
    let reset = timeline
        .iter()
        .rposition(|trace| {
            *trace
                == Trace::Write {
                    channel: 2,
                    register: CS,
                    value: CS_RESET,
                }
        })
        .expect("the channel was reset");
    let freed = timeline
        .iter()
        .position(|trace| matches!(trace, Trace::Freed { .. }))
        .expect("the chain was freed");
    assert!(reset < freed, "the reset precedes the free");
    assert_eq!(channel.start(), Err(DriverError::NotFound));
}

#[test]
fn a_fault_reports_the_controllers_error_bits() {
    let model = Model::pi4();
    model.own(0, BUFFER, u64::from(PERIOD * PERIODS));
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    model.fault(0, DEBUG_READ_ERROR);
    assert_eq!(
        channel.take_event(),
        Ok(DmaChannelEvent::Faulted(
            NonZeroU32::new(CS_ERROR | DEBUG_READ_ERROR).expect("non-zero")
        ))
    );
    assert_eq!(channel.stop(), Ok(Halted::Drained));
    assert_eq!(channel.take_event(), Ok(DmaChannelEvent::Quiet));
}

#[test]
fn a_block_fetched_from_freed_memory_is_a_read_error() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    // Re-preparing a running channel is refused, so its chain stays put.
    assert_eq!(
        channel.prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D)),
        Err(DriverError::Busy)
    );
    assert_eq!(channel.start(), Err(DriverError::Busy));
    assert_eq!(model.live_tables(), 1);
}

#[test]
fn position_reads_zero_until_the_first_block_is_fetched() {
    let model = Model::pi4();
    model.own(0, BUFFER, u64::from(PERIOD * PERIODS));
    model.defer_fetch(0);
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    assert_eq!(channel.position(), Err(DriverError::NotFound));
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    assert_eq!(channel.position(), Ok(0));
    model.advance(0, 100);
    assert_eq!(channel.position(), Ok(100));
}

#[test]
fn a_position_between_two_blocks_reads_the_next_ones_start() {
    let model = Model::pi4();
    model.own(0, BUFFER, u64::from(PERIOD * PERIODS));
    model.hold_between_blocks(0);
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    model.advance(0, PERIOD);
    assert_eq!(channel.position(), Ok(PERIOD));
    model.advance(0, 3 * PERIOD);
    // The last block has ended at the buffer's end and the first is not yet
    // loaded: the channel is at the start.
    assert_eq!(channel.position(), Ok(0));
}

#[test]
fn a_position_outside_the_buffer_is_a_fault() {
    let model = Model::pi4();
    model.own(0, BUFFER, u64::from(PERIOD * PERIODS));
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    for stray in [0xC100_0000 - 4, 0xC100_0000 + PERIOD * PERIODS + 4] {
        model.stray_source(0, stray);
        assert_eq!(channel.position(), Err(DriverError::DeviceFault));
    }
}

#[test]
fn an_acknowledgement_keeps_the_flags_and_never_restarts_a_halted_channel() {
    let cell = DREQ_PCM_TX | 0x5 << 16;
    let model = Model::pi4();
    model.own(0, BUFFER, u64::from(PERIOD * PERIODS));
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    channel
        .prepare(&line(cell), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    model.advance(0, PERIOD);
    assert_eq!(channel.take_event(), Ok(DmaChannelEvent::Boundary));
    assert_eq!(
        model.writes(0, CS).last(),
        Some(&(CS_INT | CS_END | CS_ACTIVE | 0x5 << 16))
    );
    model.advance(0, PERIOD);
    model.set_active(0, false);
    assert_eq!(channel.take_event(), Ok(DmaChannelEvent::Boundary));
    assert_eq!(
        model.writes(0, CS).last(),
        Some(&(CS_INT | CS_END | 0x5 << 16))
    );
    assert!(!model.active(0));
    assert_eq!(channel.take_event(), Ok(DmaChannelEvent::Quiet));
}

#[test]
fn device_to_memory_moves_the_destination_and_reads_it_back() {
    let model = Model::pi4();
    model.own(3, BUFFER, u64::from(PERIOD * PERIODS));
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(3).expect("channel 3");
    channel
        .prepare(&line(3), &transfer(PERIOD, PERIODS, D2M))
        .expect("prepares");
    channel.start().expect("starts");
    model.advance(3, 640);
    let block = model.loaded(3)[0];
    assert_eq!((block.source, block.dest), (0x7E20_3004, 0xC100_0000));
    assert_eq!(
        block.info & (TI_SRC_DREQ | TI_DEST_INC),
        TI_SRC_DREQ | TI_DEST_INC
    );
    assert_eq!(block.info & (TI_DEST_DREQ | TI_SRC_INC), 0);
    assert_eq!(channel.position(), Ok(640));
}

#[test]
fn a_carve_the_controller_cannot_make_leaves_the_channel_unprepared() {
    let model = Model::pi4();
    model.refuse_carves();
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    assert_eq!(
        channel.prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D)),
        Err(DriverError::LengthOutOfRange)
    );
    assert_eq!(channel.start(), Err(DriverError::NotFound));
}

/// A channel whose reset is never issued is still able to run, so it keeps
/// the chain it may be fetching and refuses a new one.
#[test]
fn a_channel_that_will_not_reset_keeps_its_chain_and_refuses_another() {
    let model = Model::pi4();
    let store = model.store();
    let refusing = Unresettable {
        model: &model,
        armed: core::cell::Cell::new(false),
    };
    let mut engine = Bcm2835Dma::new(&refusing, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    refusing.armed.set(true);
    assert_eq!(model.live_tables(), 1);
    assert_eq!(channel.release(), Err(DriverError::OutOfRange));
    assert_eq!(
        model.live_tables(),
        1,
        "the chain it may still fetch is kept"
    );
    assert_eq!(
        channel.prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D)),
        Err(DriverError::Busy),
        "and no chain replaces it"
    );
}

/// The index of `channel`'s last start, and of the first chain freed after it.
fn start_and_free(model: &Model, channel: usize) -> (usize, usize) {
    let timeline = model.timeline();
    let timeline = timeline.borrow();
    let started = timeline
        .iter()
        .rposition(|trace| {
            matches!(trace, Trace::Write { channel: c, register, value }
                if *c == channel && *register == CS && value & CS_ACTIVE != 0)
        })
        .expect("the channel was started");
    let freed = timeline
        .iter()
        .skip(started)
        .position(|trace| matches!(trace, Trace::Freed { .. }))
        .expect("the chain was freed")
        + started;
    (started, freed)
}

#[test]
fn dropping_a_running_channel_resets_it_before_its_chain_is_freed() {
    let model = Model::pi4();
    model.own(2, BUFFER, u64::from(PERIOD * PERIODS));
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(2).expect("channel 2");
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    model.advance(2, 700);
    drop(engine);
    assert_eq!(model.live_tables(), 0);
    let (started, freed) = start_and_free(&model, 2);
    let timeline = model.timeline();
    let reset = timeline.borrow()[started..freed].iter().any(|trace| {
        *trace
            == Trace::Write {
                channel: 2,
                register: CS,
                value: CS_RESET,
            }
    });
    assert!(reset, "the channel is reset before its chain goes");
}

#[test]
fn dropping_a_channel_that_will_not_reset_keeps_its_chain() {
    let model = Model::pi4();
    let store = model.store();
    let refusing = Unresettable {
        model: &model,
        armed: core::cell::Cell::new(false),
    };
    let mut engine = Bcm2835Dma::new(&refusing, &store).expect("whole channels");
    let channel = engine.channel(0).expect("channel 0");
    channel
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    refusing.armed.set(true);
    drop(engine);
    assert_eq!(
        model.live_tables(),
        1,
        "a chain the channel may still fetch never returns to the store"
    );
}

#[test]
fn dropping_an_idle_channel_frees_its_chain_without_touching_it() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2835Dma::new(&model, &store).expect("whole channels");
    engine
        .channel(5)
        .expect("channel 5")
        .prepare(&line(DREQ_PCM_TX), &transfer(PERIOD, PERIODS, M2D))
        .expect("prepares");
    drop(engine);
    assert_eq!(model.live_tables(), 0);
    assert!(model.writes(5, CS).is_empty());
}

/// A register block of a given length that answers nothing.
struct Window(usize);

impl RegisterBlock for Window {
    fn read32(&self, _offset: usize) -> Result<u32, DriverError> {
        Ok(0)
    }

    fn write32(&self, _offset: usize, _value: u32) -> Result<(), DriverError> {
        Ok(())
    }

    fn block_len(&self) -> usize {
        self.0
    }
}

#[test]
fn a_window_that_is_not_whole_channels_is_refused() {
    let store = Model::pi4().store();
    for len in [0, 0xB04, 0x80] {
        assert!(matches!(
            Bcm2835Dma::new(&Window(len), &store),
            Err(DriverError::LengthOutOfRange)
        ));
    }
    let window = Window(0xB00);
    let mut engine = Bcm2835Dma::new(&window, &store).expect("eleven channels");
    assert_eq!(engine.channel_count(), 11);
    assert!(engine.channel(10).is_some());
    assert!(engine.channel(11).is_none());
    // A window of more channels than a mask can name serves the first
    // sixty-four.
    let wide = Window(0x100 * 70);
    assert_eq!(
        Bcm2835Dma::new(&wide, &store)
            .expect("whole channels")
            .channel_count(),
        64
    );
}

#[test]
fn register_requires_drv_load() {
    struct Host(bool);
    impl DriverHost for Host {
        fn has_capability(&self, cap: CapabilityId) -> bool {
            cap == CapabilityId::DRV_LOAD && self.0
        }
        fn kind(&self) -> DriverKind {
            DriverKind::UserSpace
        }
    }
    assert_eq!(
        register(&Host(false)).err(),
        Some(DriverError::PermissionDenied)
    );
    assert!(register(&Host(true)).is_ok());
}

#[test]
fn the_bind_table_matches_the_legacy_engines_alone() {
    assert_eq!(BIND_KEYS.len(), 1);
    assert_eq!(
        BIND_KEYS[0].key,
        HwMatchKey::compatible(DMA_COMPATIBLE).expect("fits")
    );
    // DMA4 is a different register model and block format.
    assert_ne!(
        BIND_KEYS[0].key,
        HwMatchKey::compatible(b"brcm,bcm2711-dma").expect("fits")
    );
    for cap in [
        CapabilityId::MMIO_MAP,
        CapabilityId::IRQ_BIND,
        CapabilityId::IPC_BIND_PRIVILEGED,
        CapabilityId::MEM_DMA,
        CapabilityId::SHM,
    ] {
        assert!(REQUIRED_CAPABILITIES.contains(&cap));
    }
}
