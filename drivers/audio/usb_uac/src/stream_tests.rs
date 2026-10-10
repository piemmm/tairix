use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::audio::SampleFormat;
use tairix_abi::driver::audio_ring::{aligned_region, PcmGeometry, PcmRing, REGION_ALIGN_PADDING};

use super::*;

const FRAME_BYTES: usize = 4;

fn layout(slots: u16, packets: u16) -> IsoLayout {
    IsoLayout::new(slots, packets, 192).expect("a valid layout")
}

fn shape(layout: IsoLayout) -> SlotShape {
    SlotShape {
        layout,
        frame_bytes: FRAME_BYTES,
        silence: 0,
    }
}

fn grantor() -> ProcId {
    ProcId::from_raw([0x7E; tairix_abi::PROC_ID_LEN])
}

/// The number the tests' streams take.
const NUMBER: core::num::NonZeroU32 = core::num::NonZeroU32::MIN;

/// A 16-bit stereo ring of 256 frames.
struct Ring {
    bytes: Vec<u8>,
    geometry: PcmGeometry,
}

impl Ring {
    fn new() -> Self {
        let geometry = PcmGeometry::new(256, SampleFormat::S16, 2).expect("valid");
        Self {
            bytes: vec![0u8; geometry.region_len() + REGION_ALIGN_PADDING],
            geometry,
        }
    }

    fn bind(&mut self) -> PcmRing<'_> {
        let len = self.geometry.region_len();
        PcmRing::bind(
            aligned_region(&mut self.bytes, len).expect("padded"),
            self.geometry,
        )
        .expect("binds")
    }

    /// Queue `frames` frames, frame `n` carrying `n` in every byte.
    fn write(&mut self, frames: u32, first: u8) {
        let mut ring = self.bind();
        let bytes: Vec<u8> = (0..frames)
            .flat_map(|n| [first.wrapping_add(u8::try_from(n % 256).expect("small")); FRAME_BYTES])
            .collect();
        assert_eq!(ring.write(&bytes), Ok(frames));
    }
}

#[test]
fn slots_are_queued_in_turn_and_finish_in_the_order_reported() {
    let mut stream = HwStream::new(0x01, layout(3, 2), grantor(), NUMBER).expect("allocated");
    assert_eq!(stream.next_free(), Some(0));
    stream.mark_queued(0, 96);
    stream.mark_queued(1, 96);
    assert_eq!(stream.queued(), 2);
    assert_eq!(stream.next_free(), Some(2));
    stream.mark_queued(2, 96);
    assert_eq!(stream.next_free(), None, "every slot is the controller's");
    stream.complete(0, 0, 10);
    stream.complete(1, 3, 20);
    // A slot reported twice, or one never queued, is a report about nothing.
    stream.complete(1, 0, 30);
    let done = stream.take_done().expect("slot 0");
    assert_eq!((done.slot, done.frames, done.completed_at), (0, 96, 10));
    assert_eq!(stream.next_free(), Some(0), "the ring wraps");
    let done = stream.take_done().expect("slot 1");
    assert_eq!((done.slot, done.skipped), (1, 3));
    assert_eq!(stream.take_done(), None);
    assert_eq!(stream.queued(), 1);
    stream.halt(Errno::NotFound);
    stream.halt(Errno::WouldBlock);
    assert_eq!(
        stream.halted(),
        Some(Errno::NotFound),
        "the first reason stands"
    );
    assert_eq!(
        stream.next_free(),
        None,
        "a halted stream takes nothing more"
    );
}

#[test]
fn a_stream_hears_only_its_own_notices_from_its_grantor() {
    let stream = HwStream::new(0x01, layout(3, 2), grantor(), NUMBER).expect("allocated");
    let halted = |endpoint, number| IsoNotify::Halted {
        endpoint,
        stream: number,
        reason: Errno::WouldBlock,
    };
    assert!(stream.hears(&halted(0x01, NUMBER), grantor()));
    assert!(
        !stream.hears(&halted(0x01, NUMBER.saturating_add(1)), grantor()),
        "another stream on the same endpoint"
    );
    assert!(!stream.hears(&halted(0x02, NUMBER), grantor()));
    let stranger = ProcId::from_raw([0x51; tairix_abi::PROC_ID_LEN]);
    assert!(!stream.hears(&halted(0x01, NUMBER), stranger));
}

#[test]
fn held_slots_queue_oldest_first_and_report_nothing_until_queued() {
    let mut stream = HwStream::new(0x01, layout(4, 2), grantor(), NUMBER).expect("allocated");
    assert_eq!(stream.oldest_staged(), None);
    stream.mark_staged(0, 96);
    stream.mark_staged(1, 90);
    assert_eq!((stream.staged(), stream.queued()), (2, 0));
    assert_eq!(stream.next_free(), Some(2));
    // Only the next slot can be taken, and only while free.
    stream.mark_staged(3, 96);
    stream.mark_queued(0, 96);
    assert_eq!((stream.staged(), stream.queued()), (2, 0));
    stream.complete(0, 0, 10);
    assert_eq!(stream.take_done(), None, "the controller never held it");
    assert_eq!(stream.queued_frames(), 0);

    assert_eq!(stream.oldest_staged(), Some(0));
    stream.queue_staged(0);
    assert_eq!(stream.oldest_staged(), Some(1));
    stream.queue_staged(1);
    stream.queue_staged(1);
    assert_eq!((stream.staged(), stream.queued()), (0, 2));
    assert_eq!(stream.queued_frames(), 186);
    stream.complete(0, 0, 10);
    let done = stream.take_done().expect("slot 0");
    assert_eq!((done.slot, done.frames), (0, 96));
}

#[test]
fn held_slots_across_the_end_of_the_ring_still_queue_in_turn() {
    let mut stream = HwStream::new(0x01, layout(3, 2), grantor(), NUMBER).expect("allocated");
    stream.mark_queued(0, 96);
    stream.mark_queued(1, 96);
    for (slot, at) in [(0, 10), (1, 20)] {
        stream.complete(slot, 0, at);
        stream.take_done().expect("finished");
    }
    stream.mark_staged(2, 96);
    stream.mark_staged(0, 96);
    assert_eq!(stream.oldest_staged(), Some(2));
    stream.queue_staged(2);
    assert_eq!(stream.oldest_staged(), Some(0));
}

#[test]
fn a_slot_plan_paces_each_interval_and_holds_it_to_its_budget() {
    let layout = layout(2, 10);
    let mut pacer = PacketPacer::nominal(44_100, 8);
    let (counts, total) = plan_slot(shape(layout), &mut pacer);
    assert_eq!(total, 441, "ten 1 ms intervals at 44.1 kHz");
    assert_eq!(&counts[..10], &[44, 44, 44, 44, 44, 44, 44, 44, 44, 45]);
    // A rate past the endpoint's budget is held to it: 192 bytes is 48
    // frames.
    let mut fast = PacketPacer::nominal(96_000, 8);
    let (counts, _) = plan_slot(shape(layout), &mut fast);
    assert!(counts[..10].iter().all(|&count| count == 48));
}

#[test]
fn a_whole_slot_is_read_from_the_ring_and_recorded_moved() {
    let layout = layout(2, 2);
    let mut region = vec![0u8; layout.region_len()];
    let mut ring = Ring::new();
    ring.write(96, 0);
    let filled = fill_playback(
        shape(layout),
        &mut region,
        1,
        &[48, 48],
        Some(&mut ring.bind()),
        Shortfall::Wait,
    )
    .expect("filled");
    assert_eq!(
        filled,
        Filled {
            taken: 96,
            padded: 0
        }
    );
    for packet in 0..2u16 {
        let record = layout.record(&region, 1, packet).expect("in place");
        assert_eq!(record.length, 192);
        assert_eq!(record.status, IsoPacketStatus::Moved);
    }
    // Frame 48 opens the second interval.
    assert_eq!(layout.data(&region, 1, 1).expect("in place")[0], 48);
    assert_eq!(ring.bind().readable_frames(), Ok(0));
}

#[test]
fn a_dry_ring_pads_with_silence_and_a_drain_sends_what_is_left() {
    let layout = layout(2, 2);
    let mut region = vec![0xAAu8; layout.region_len()];
    let mut ring = Ring::new();
    ring.write(60, 1);
    let filled = fill_playback(
        shape(layout),
        &mut region,
        0,
        &[48, 48],
        Some(&mut ring.bind()),
        Shortfall::Pad,
    )
    .expect("filled");
    assert_eq!(
        filled,
        Filled {
            taken: 60,
            padded: 36
        }
    );
    assert_eq!(filled.carried(), 96);
    let second = layout.data(&region, 0, 1).expect("in place");
    assert_eq!(second[12 * FRAME_BYTES - 1], 60, "the last frame taken");
    assert!(
        second[12 * FRAME_BYTES..].iter().all(|&b| b == 0),
        "silence"
    );
    assert_eq!(layout.record(&region, 0, 1).expect("in place").length, 192);

    ring.write(10, 1);
    let filled = fill_playback(
        shape(layout),
        &mut region,
        1,
        &[48, 48],
        Some(&mut ring.bind()),
        Shortfall::Short,
    )
    .expect("filled");
    assert_eq!(
        filled,
        Filled {
            taken: 10,
            padded: 0
        }
    );
    assert_eq!(layout.record(&region, 1, 0).expect("in place").length, 40);
    assert_eq!(
        layout.record(&region, 1, 1).expect("in place").length,
        0,
        "nothing past the drain's end"
    );
}

#[test]
fn with_no_ring_a_padded_slot_is_silence_throughout() {
    let layout = layout(2, 2);
    let mut region = vec![0xAAu8; layout.region_len()];
    let filled = fill_playback(
        shape(layout),
        &mut region,
        1,
        &[48, 47],
        None,
        Shortfall::Pad,
    )
    .expect("filled");
    assert_eq!(
        filled,
        Filled {
            taken: 0,
            padded: 95
        }
    );
    for (packet, frames) in [(0u16, 48usize), (1, 47)] {
        let record = layout.record(&region, 1, packet).expect("in place");
        assert_eq!(record.length as usize, frames * FRAME_BYTES);
        let data = layout.data(&region, 1, packet).expect("in place");
        assert!(data[..frames * FRAME_BYTES].iter().all(|&b| b == 0));
    }
}

#[test]
fn frames_a_missed_or_failed_interval_carried_are_lost() {
    // The records as the host controller leaves them: a missed interval keeps
    // its length, a failed one is rewritten to zero.
    let layout = layout(2, 3);
    let mut region = vec![0u8; layout.region_len()];
    for (packet, (length, status)) in [
        (192, IsoPacketStatus::Moved),
        (192, IsoPacketStatus::Missed),
        (0, IsoPacketStatus::Failed),
    ]
    .into_iter()
    .enumerate()
    {
        let packet = u16::try_from(packet).expect("small");
        layout
            .set_record(&mut region, 0, packet, IsoPacket { length, status })
            .expect("in place");
    }
    assert_eq!(playback_losses(shape(layout), &region, 0, 144), Ok(96));
}

#[test]
fn a_capture_slot_delivers_whole_frames_and_counts_what_it_could_not() {
    let layout = layout(2, 3);
    let mut region = vec![0u8; layout.region_len()];
    // A moved interval of 47 frames and a stray byte, a missed one, and a
    // moved one of 48.
    for (packet, length, status) in [
        (0u16, 47 * 4 + 1, IsoPacketStatus::Moved),
        (1, 0, IsoPacketStatus::Missed),
        (2, 192, IsoPacketStatus::Moved),
    ] {
        layout
            .set_record(&mut region, 1, packet, IsoPacket { length, status })
            .expect("in place");
        layout
            .data_mut(&mut region, 1, packet)
            .expect("in place")
            .fill(7);
    }
    let mut ring = Ring::new();
    ring.write(200, 0);
    let mut timeline = PacketPacer::nominal(48_000, 8);
    let delivered = deliver_capture(
        shape(layout),
        &region,
        1,
        Some(&mut ring.bind()),
        &mut timeline,
        8,
    )
    .expect("delivered");
    assert_eq!(
        delivered,
        Delivered {
            written: 56,
            overrun: 39,
            missed: 48,
            received: 95,
            moved_microframes: 16,
        }
    );
    // With no ring, the frames are only counted.
    let delivered =
        deliver_capture(shape(layout), &region, 1, None, &mut timeline, 8).expect("counted");
    assert_eq!((delivered.written, delivered.received), (0, 95));
}
