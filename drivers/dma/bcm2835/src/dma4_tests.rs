//! The DMA4 engine driven against its register-level model.

use core::num::NonZeroU32;

use tairix_abi::driver::dmaengine::{
    CyclicParams, CyclicTransfer, DmaChannel, DmaChannelEvent, DmaDirection, DmaEngine, Halted,
    DMA_CONTROLLER_ENDPOINTS,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::{DriverError, RegisterBlock};

use crate::dma4::Bcm2711Dma;
use crate::dma4_model::{
    Model, Trace, Unresettable, CB, CS, CS_ACTIVE, CS_ERROR, CS_INT, CS_PROT, DEBUG,
    DEBUG_READ_CB_ERROR, DEBUG_RESET, INFO_INC, INFO_SIZE_128, TI_DEST_DREQ, TI_INTEN, TI_SRC_DREQ,
    TI_WAIT_RD_RESP, TI_WAIT_RESP,
};

/// HDMI0's audio line on the Pi 4's own tree: request line 10, AXI priority
/// 10, panic priority 15, wide source reads, bursts.
const HDMI0_AUDIO: u32 = 0x41FA_000A;
const PLAIN: u32 = 10;
const BUFFER: u64 = 0xC100_0000;
/// The HDMI MAI FIFO as the DMA4 engines reach the peripherals.
const FIFO: u64 = 0x4_7EF0_0890;
const PERIOD: u32 = 1536;
const PERIODS: u32 = 4;
const M2D: DmaDirection = DmaDirection::MemoryToDevice;
const D2M: DmaDirection = DmaDirection::DeviceToMemory;

fn line(cell: u32) -> LinkRequest {
    LinkRequest::new(
        DMA_CONTROLLER_ENDPOINTS.endpoint(5),
        0,
        &[cell],
        b"audio-rx",
    )
    .expect("valid")
}

fn transfer(buffer: u64, direction: DmaDirection) -> CyclicTransfer {
    CyclicTransfer {
        buffer,
        fifo: FIFO,
        direction,
        period_bytes: PERIOD,
        periods: PERIODS,
    }
}

/// Prepare and start channel `index` of `engine` over `buffer`.
fn running<'e, 'a, S: crate::chain::BlockStore>(
    engine: &'e mut Bcm2711Dma<'a, S>,
    index: u8,
    cell: u32,
    buffer: u64,
    direction: DmaDirection,
) -> &'e mut crate::dma4::Channel<'a, S> {
    let channel = engine.channel(index).expect("channel");
    channel
        .prepare(&line(cell), &transfer(buffer, direction))
        .expect("prepares");
    channel.start().expect("starts");
    channel
}

#[test]
fn a_cyclic_chain_is_walked_with_one_interrupt_per_period() {
    let model = Model::pi4();
    model.own(1, BUFFER, u64::from(PERIOD * PERIODS));
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(1).expect("channel 1");
    channel
        .prepare(&line(HDMI0_AUDIO), &transfer(BUFFER, M2D))
        .expect("prepares");
    channel.start().expect("starts");
    for period in 1..=2 * PERIODS {
        model.advance(1, PERIOD);
        assert_eq!(model.interrupts(1), period);
        assert_eq!(channel.take_event(), Ok(DmaChannelEvent::Boundary));
        assert!(!model.pending(1), "the interrupt was acknowledged");
        assert_eq!(channel.position(), Ok(period % PERIODS * PERIOD));
    }
    let blocks = model.loaded(1);
    let table = blocks[0].next - 32;
    for (index, block) in blocks.iter().take(4).enumerate() {
        let index = u64::try_from(index).expect("small");
        assert_eq!(block.source, BUFFER + index * u64::from(PERIOD));
        assert_eq!(block.dest, FIFO);
        assert_eq!(block.len, PERIOD);
        assert_eq!(block.next, table + (index + 1) % u64::from(PERIODS) * 32);
        assert_eq!(block.info & TI_INTEN, TI_INTEN, "every period interrupts");
    }
}

#[test]
fn the_hdmi_line_is_encoded_as_its_cell_states() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    running(&mut engine, 2, HDMI0_AUDIO, BUFFER, M2D);
    let block = model.loaded(2)[0];
    assert_eq!(block.info, TI_INTEN | 10 << 9 | TI_WAIT_RESP | TI_DEST_DREQ);
    // Wide source reads and bursts of three on both sides; only the memory
    // side increments.
    assert_eq!(block.source_info, INFO_INC | INFO_SIZE_128 | 3 << 8);
    assert_eq!(block.dest_info, 3 << 8 | 0x04);
    let started = *model.writes(2, CS).last().expect("started");
    assert_eq!(started, CS_ACTIVE | CS_PROT | 0x00FA_0000);
}

#[test]
fn device_to_memory_waits_for_reads_and_increments_the_destination() {
    let model = Model::pi4();
    model.own(1, BUFFER, u64::from(PERIOD * PERIODS));
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let channel = running(&mut engine, 1, PLAIN, BUFFER, D2M);
    let block = model.loaded(1)[0];
    assert_eq!(
        block.info,
        TI_INTEN | 10 << 9 | TI_WAIT_RESP | TI_SRC_DREQ | TI_WAIT_RD_RESP
    );
    assert_eq!(block.dest_info & INFO_INC, INFO_INC);
    assert_eq!(block.source_info & INFO_INC, 0);
    model.advance(1, PERIOD + 40);
    assert_eq!(channel.position(), Ok(PERIOD + 40));
}

#[test]
fn a_buffer_above_four_gibibytes_is_named_through_its_top_byte() {
    let high = 0x8_4000_0000;
    let model = Model::pi4();
    model.own(3, high, u64::from(PERIOD * PERIODS));
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let channel = running(&mut engine, 3, PLAIN, high, M2D);
    let block = model.loaded(3)[0];
    assert_eq!(block.source, high);
    assert_eq!(block.source_info & 0xFF, 0x08);
    model.advance(3, 3 * PERIOD + 8);
    assert_eq!(channel.position(), Ok(3 * PERIOD + 8));
}

#[test]
fn an_address_past_the_engines_reach_is_refused() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(1).expect("channel");
    let beyond = 1 << 36;
    assert_eq!(
        channel.prepare(&line(PLAIN), &transfer(beyond, M2D)),
        Err(DriverError::OutOfRange)
    );
    let mut far_fifo = transfer(BUFFER, M2D);
    far_fifo.fifo = beyond;
    assert_eq!(
        channel.prepare(&line(PLAIN), &far_fifo),
        Err(DriverError::OutOfRange)
    );
    assert_eq!(model.live_tables(), 0, "nothing carved past the reach");
}

#[test]
fn a_wide_line_holds_the_buffer_to_sixteen_bytes() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let params = |period_bytes| CyclicParams {
        fifo: FIFO,
        direction: M2D,
        period_bytes,
        periods: PERIODS,
    };
    assert_eq!(engine.admit(&line(HDMI0_AUDIO), &params(PERIOD)), Ok(4));
    assert_eq!(
        engine.admit(&line(HDMI0_AUDIO), &params(PERIOD + 4)),
        Err(DriverError::LengthOutOfRange)
    );
    let channel = engine.channel(1).expect("channel");
    assert_eq!(
        channel.prepare(&line(HDMI0_AUDIO), &transfer(BUFFER + 4, M2D)),
        Err(DriverError::OutOfRange)
    );
}

#[test]
fn a_specifier_the_binding_does_not_define_refuses_the_line() {
    let model = Model::pi4();
    let store = model.store();
    let engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    assert_eq!(engine.accept(&line(0)), Err(DriverError::Unsupported));
    assert_eq!(
        engine.accept(&line(1 << 26 | PLAIN)),
        Err(DriverError::Unsupported)
    );
    assert_eq!(engine.accept(&line(HDMI0_AUDIO)), Ok(()));
}

#[test]
fn stop_pauses_lets_the_transactions_finish_then_resets() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let channel = running(&mut engine, 1, PLAIN, BUFFER, M2D);
    model.drain_reads(1, 3);
    assert_eq!(channel.stop(), Ok(Halted::Drained));
    assert!(!model.active(1));
    let writes = model.writes(1, CS);
    assert_eq!(
        writes[writes.len() - 2] & CS_ACTIVE,
        0,
        "paused before the reset"
    );
    assert_eq!(*writes.last().expect("reset"), CS_PROT);
    assert!(model
        .writes(1, DEBUG)
        .iter()
        .any(|&value| value & DEBUG_RESET != 0));
}

#[test]
fn a_channel_that_will_not_finish_its_transactions_is_reset_regardless() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let channel = running(&mut engine, 1, PLAIN, BUFFER, M2D);
    model.drain_reads(1, u32::MAX);
    assert_eq!(channel.stop(), Ok(Halted::Undrained));
    assert!(!model.active(1));
}

#[test]
fn stopping_an_idle_channel_only_resets_it() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(1).expect("channel");
    assert_eq!(channel.stop(), Ok(Halted::Drained));
    assert_eq!(model.writes(1, CS), [CS_PROT]);
    assert_eq!(model.writes(1, DEBUG), [DEBUG_RESET]);
}

#[test]
fn start_names_the_chain_by_its_address_shifted_right_by_five() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    running(&mut engine, 1, PLAIN, BUFFER, M2D);
    let named = *model.writes(1, CB).last().expect("named");
    let first = model.loaded(1)[0];
    assert_eq!(u64::from(named) << 5, first.next - 32);
}

#[test]
fn a_fault_reports_the_controllers_error_bits() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let channel = running(&mut engine, 1, PLAIN, BUFFER, M2D);
    model.fault(1, 0b0101);
    let bits = NonZeroU32::new(CS_ERROR | 0b0101).expect("nonzero");
    assert_eq!(channel.take_event(), Ok(DmaChannelEvent::Faulted(bits)));
}

#[test]
fn a_running_channels_chain_stays_put() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let channel = running(&mut engine, 1, PLAIN, BUFFER, M2D);
    assert_eq!(
        channel.prepare(&line(PLAIN), &transfer(BUFFER, M2D)),
        Err(DriverError::Busy)
    );
    assert_eq!(channel.start(), Err(DriverError::Busy));
    assert_eq!(model.live_tables(), 1);
}

#[test]
fn a_block_fetch_error_is_reported_as_the_controllers_bits() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let channel = running(&mut engine, 1, PLAIN, BUFFER, M2D);
    model.fault(1, DEBUG_READ_CB_ERROR);
    let bits = NonZeroU32::new(CS_ERROR | DEBUG_READ_CB_ERROR).expect("nonzero");
    assert_eq!(channel.take_event(), Ok(DmaChannelEvent::Faulted(bits)));
    assert_eq!(channel.stop(), Ok(Halted::Drained));
    assert_eq!(
        channel.take_event(),
        Ok(DmaChannelEvent::Quiet),
        "the reset cleared it"
    );
}

#[test]
fn position_reads_zero_until_the_first_block_is_fetched() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let channel = engine.channel(1).expect("channel");
    channel
        .prepare(&line(PLAIN), &transfer(BUFFER, M2D))
        .expect("prepares");
    model.defer_fetch(1);
    channel.start().expect("starts");
    assert_eq!(channel.position(), Ok(0));
}

#[test]
fn an_acknowledgement_keeps_the_flags_and_never_restarts_a_halted_channel() {
    let model = Model::pi4();
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
    let channel = running(&mut engine, 1, HDMI0_AUDIO, BUFFER, M2D);
    model.advance(1, PERIOD);
    model.fault(1, 0);
    assert_eq!(channel.take_event(), Ok(DmaChannelEvent::Boundary));
    let acknowledged = *model.writes(1, CS).last().expect("acknowledged");
    assert_eq!(acknowledged, CS_INT | CS_PROT | 0x00FA_0000);
    assert!(!model.active(1), "a halted channel stays halted");
}

#[test]
fn a_channel_that_will_not_reset_keeps_its_chain_and_refuses_another() {
    let model = Model::pi4();
    let refusing = Unresettable {
        model: &model,
        armed: core::cell::Cell::new(false),
        ignored: false,
    };
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&refusing, &store).expect("whole channels");
    let channel = running(&mut engine, 1, PLAIN, BUFFER, M2D);
    refusing.armed.set(true);
    assert_eq!(channel.stop(), Err(DriverError::OutOfRange));
    assert_eq!(
        channel.prepare(&line(PLAIN), &transfer(BUFFER, M2D)),
        Err(DriverError::Busy)
    );
    assert_eq!(model.live_tables(), 1);
}

#[test]
fn a_reset_the_channel_ignores_is_not_taken_as_idling_it() {
    let model = Model::pi4();
    let ignoring = Unresettable {
        model: &model,
        armed: core::cell::Cell::new(false),
        ignored: true,
    };
    let store = model.store();
    let mut engine = Bcm2711Dma::new(&ignoring, &store).expect("whole channels");
    let channel = running(&mut engine, 1, PLAIN, BUFFER, M2D);
    ignoring.armed.set(true);
    assert_eq!(channel.stop(), Err(DriverError::DeviceFault));
}

#[test]
fn dropping_a_running_channel_resets_it_before_its_chain_is_freed() {
    let model = Model::pi4();
    let store = model.store();
    {
        let mut engine = Bcm2711Dma::new(&model, &store).expect("whole channels");
        running(&mut engine, 1, PLAIN, BUFFER, M2D);
    }
    let timeline = model.timeline();
    let timeline = timeline.borrow();
    let reset = timeline
        .iter()
        .position(|trace| {
            matches!(*trace, Trace::Write { channel: 1, register, value }
                if register == DEBUG && value & DEBUG_RESET != 0)
        })
        .expect("reset");
    let freed = timeline
        .iter()
        .position(|trace| matches!(trace, Trace::Freed { .. }))
        .expect("freed");
    assert!(reset < freed);
}

#[test]
fn dropping_a_channel_that_will_not_reset_keeps_its_chain() {
    let model = Model::pi4();
    let refusing = Unresettable {
        model: &model,
        armed: core::cell::Cell::new(false),
        ignored: false,
    };
    let store = model.store();
    {
        let mut engine = Bcm2711Dma::new(&refusing, &store).expect("whole channels");
        running(&mut engine, 1, PLAIN, BUFFER, M2D);
        refusing.armed.set(true);
    }
    assert_eq!(model.live_tables(), 1, "withheld for the quarantine");
}

#[test]
fn a_window_that_is_not_whole_channels_is_refused() {
    struct Short;
    impl RegisterBlock for Short {
        fn read32(&self, _: usize) -> Result<u32, DriverError> {
            Ok(0)
        }
        fn write32(&self, _: usize, _: u32) -> Result<(), DriverError> {
            Ok(())
        }
        fn block_len(&self) -> usize {
            0x180
        }
    }
    let model = Model::pi4();
    let store = model.store();
    assert!(matches!(
        Bcm2711Dma::new(&Short, &store),
        Err(DriverError::LengthOutOfRange)
    ));
}
