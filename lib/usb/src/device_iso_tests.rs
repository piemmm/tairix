use super::*;

const LAYOUT: IsoLayout = match IsoLayout::new(4, 2, 192) {
    Ok(layout) => layout,
    Err(_) => panic!("a valid layout"),
};

fn stream(direction: UsbDirection) -> Stream {
    new_stream(LAYOUT, direction, 192, 0, Stream::stride_for(192)).expect("memory for a stream")
}

fn ring() -> ProducerRing {
    ProducerRing::new(ISO_RING_TRBS, 0x10_0000)
        .expect("a ring")
        .0
}

/// Queue `slot`'s two intervals as single-TRB TDs, as the engine would.
fn queue(stream: &mut Stream, ring: &mut ProducerRing, slot: u16, length: u32) {
    for packet in 0..2 {
        let at = u16::try_from(ring.enqueue_slot()).expect("a ring slot");
        ring.push(Trb::new(TrbType::Isoch, 0, 0, 0)).expect("room");
        stream.tds.push_back(Td {
            slot,
            packet,
            first: at,
            last: at,
            length,
            first_length: length,
        });
    }
    stream.slots[usize::from(slot)].pending = Some(2);
    stream.order.push_back(slot);
}

fn outcome(stream: &Stream, slot: u16, packet: u16) -> IsoPacket {
    stream.outcomes[stream.outcome_index(slot, packet)]
}

#[test]
fn the_bus_clock_extends_mfindex_across_its_wrap() {
    let clock = BusClock::start(0x3F00, 0);
    // 0x200 microframes later the counter has wrapped to 0x100.
    let later = clock.advance(0x100, 0x200 * 125);
    assert_eq!(later.microframe(), 0x4100);
    // A reading taken after a long silence lands in the epoch the clock
    // predicts, not the first one carrying the same low bits.
    let silence = 10 * MFINDEX_SPAN + 5;
    let after = later.advance(0x105, (0x200 + silence) * 125);
    assert_eq!(after.microframe(), 0x4100 + silence);
    // A clock that ran slow never takes the count backwards.
    let slow = after.advance(0x104, (0x200 + silence) * 125);
    assert!(slow.microframe() >= after.microframe());
}

#[test]
fn a_fresh_stream_starts_on_the_first_frame_it_can_make() {
    let stream = stream(UsbDirection::Out);
    // IST 1, lead 8: the earliest microframe is 109, so the next frame is 112.
    assert_eq!(stream.place(1, 100, 1, true), (112, 0));
    // An interval longer than a frame starts on its own boundary.
    assert_eq!(stream.place(16, 100, 1, true), (112, 0));
    assert_eq!(stream.place(32, 100, 1, true), (128, 0));
}

#[test]
fn a_stream_ahead_of_the_controller_follows_on() {
    let mut stream = stream(UsbDirection::Out);
    stream.next = Some(200);
    assert_eq!(stream.place(8, 100, 1, true), (200, 0));
}

#[test]
fn a_late_slot_restarts_where_it_can_and_counts_the_gap() {
    let mut stream = stream(UsbDirection::Out);
    stream.next = Some(40);
    // Earliest 109; frame 112 is the restart; (112 - 40) / 8 intervals passed.
    assert_eq!(stream.place(8, 100, 1, true), (112, 9));
    // Without CFC an empty ring still takes the Frame ID of its first TD.
    assert_eq!(stream.place(8, 100, 1, false), (112, 9));
    // A busy ring on such a controller runs back to back, so no gap is placed.
    let mut ring = ring();
    queue(&mut stream, &mut ring, 0, 8);
    assert_eq!(stream.place(8, 100, 1, false), (40, 0));
    assert_eq!(stream.place(8, 100, 1, true), (112, 9));
}

#[test]
fn completions_finish_a_slot_with_what_each_interval_moved() {
    let mut stream = stream(UsbDirection::In);
    let mut ring = ring();
    queue(&mut stream, &mut ring, 1, 192);
    let first = stream.tds[0].first;
    let second = stream.tds[1].first;
    stream.on_event(Some(first), Ok(CompletionCode::ShortPacket), 16, &mut ring);
    assert_eq!(
        outcome(&stream, 1, 0),
        IsoPacket {
            length: 176,
            status: IsoPacketStatus::Moved
        }
    );
    assert_eq!(stream.slots[1].pending, Some(1));
    stream.on_event(Some(second), Ok(CompletionCode::Success), 0, &mut ring);
    assert_eq!(outcome(&stream, 1, 1).length, 192);
    assert_eq!(stream.slots[1].pending, Some(0));
    assert_eq!(ring.in_flight(), 0, "every TRB retired");
}

#[test]
fn an_event_past_unreported_tds_marks_them_missed() {
    let mut stream = stream(UsbDirection::Out);
    let mut ring = ring();
    queue(&mut stream, &mut ring, 0, 96);
    queue(&mut stream, &mut ring, 1, 96);
    // A Missed Service Error naming nothing, then the controller resumes at
    // slot 1's second interval.
    stream.on_event(None, Ok(CompletionCode::MissedService), 0, &mut ring);
    let resumed = stream.tds[3].first;
    stream.on_event(Some(resumed), Ok(CompletionCode::Success), 0, &mut ring);
    for (slot, packet) in [(0, 0), (0, 1), (1, 0)] {
        assert_eq!(
            outcome(&stream, slot, packet).status,
            IsoPacketStatus::Missed
        );
    }
    assert_eq!(
        outcome(&stream, 1, 1),
        IsoPacket {
            length: 96,
            status: IsoPacketStatus::Moved
        }
    );
    assert!(stream.halted.is_none());
    assert_eq!(ring.in_flight(), 0);
}

#[test]
fn a_missed_service_error_naming_a_td_misses_it() {
    let mut stream = stream(UsbDirection::Out);
    let mut ring = ring();
    queue(&mut stream, &mut ring, 0, 96);
    let first = stream.tds[0].first;
    stream.on_event(Some(first), Ok(CompletionCode::MissedService), 0, &mut ring);
    assert_eq!(outcome(&stream, 0, 0).status, IsoPacketStatus::Missed);
    assert_eq!(stream.slots[0].pending, Some(1));
}

#[test]
fn an_underrun_misses_everything_still_recorded() {
    let mut stream = stream(UsbDirection::Out);
    let mut ring = ring();
    queue(&mut stream, &mut ring, 2, 96);
    stream.on_event(None, Ok(CompletionCode::RingUnderrun), 0, &mut ring);
    assert_eq!(stream.slots[2].pending, Some(0));
    assert_eq!(outcome(&stream, 2, 1).status, IsoPacketStatus::Missed);
    assert_eq!(ring.in_flight(), 0);
}

#[test]
fn a_transaction_error_fails_one_interval_and_a_trb_error_halts() {
    let mut stream = stream(UsbDirection::In);
    let mut ring = ring();
    queue(&mut stream, &mut ring, 0, 192);
    let first = stream.tds[0].first;
    stream.on_event(
        Some(first),
        Ok(CompletionCode::UsbTransactionError),
        0,
        &mut ring,
    );
    assert_eq!(outcome(&stream, 0, 0).status, IsoPacketStatus::Failed);
    assert!(stream.halted.is_none());
    let second = stream.tds[0].first;
    stream.on_event(Some(second), Ok(CompletionCode::TrbError), 0, &mut ring);
    assert_eq!(stream.halted, Some(DriverError::DeviceFault));
}

#[test]
fn an_event_for_a_finished_td_or_another_ring_changes_nothing() {
    let mut stream = stream(UsbDirection::Out);
    let mut ring = ring();
    queue(&mut stream, &mut ring, 0, 96);
    let first = stream.tds[0].first;
    stream.on_event(Some(first), Ok(CompletionCode::Success), 0, &mut ring);
    // A controller that also posts the TD's second word.
    stream.on_event(Some(first), Ok(CompletionCode::Success), 0, &mut ring);
    assert_eq!(stream.slots[0].pending, Some(1));
    assert_eq!(stream.tds.len(), 1);
    // An event whose pointer is in no ring of this endpoint fails closed.
    stream.on_event(None, Ok(CompletionCode::Success), 0, &mut ring);
    assert_eq!(stream.halted, Some(DriverError::DeviceFault));
}

#[test]
fn a_td_split_at_a_boundary_reports_bytes_from_either_half() {
    let td = Td {
        slot: 0,
        packet: 0,
        first: 254,
        last: 0,
        length: 6000,
        first_length: 1000,
    };
    assert!(td.occupies(254) && td.occupies(0) && !td.occupies(1));
    assert_eq!(td.trbs(), 2);
    assert_eq!(td.received(254, 400), 600);
    assert_eq!(td.received(0, 1000), 5000);
}

#[test]
fn interval_buffers_never_straddle_a_page() {
    assert_eq!(Stream::stride_for(192), 256);
    assert_eq!(Stream::stride_for(3072), 4096);
    assert_eq!(Stream::stride_for(3), 64);
    assert_eq!(Stream::trbs_per_td(Stream::stride_for(4096)), 1);
    assert_eq!(Stream::stride_for(6000), 6016);
    assert_eq!(Stream::trbs_per_td(Stream::stride_for(6000)), 2);
}
