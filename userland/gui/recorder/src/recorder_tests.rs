//! Host tests of the recorder over fakes: the conversation in its one order,
//! every refusal before and after `Ready`, every way a recording ends, and the
//! file each leaves, read back through the demuxer.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::audio::CaptureSource;
use tairix_abi::driver::audio::ChannelMap;
use tairix_abi::driver::display::DamageRect;
use tairix_abi::recorder_ipc::{
    RecordEnding, RecordSound, RecordStart, RecorderDamage, RecorderMessage, RecorderSlot,
    SessionMessage, RECORDER_SESSION_MESSAGE_MAX, RECORDER_SLOTS,
};
use tairix_abi::{Errno, ProcId, PAGE_SIZE};
use tairix_video::webm::{Codec, Demuxer};

use super::{client_of, Exit, Reason, Recorder, EXIT_ENDED, EXIT_FINISHED};
use crate::sound::PERIOD;
use crate::testing::{FakeHost, Pipes, Records, STARTED_NS};
use crate::{RECORDER_ENDED, RECORDER_REFUSED, RECORDER_UNSTARTED};

/// The handle the session delegated the output under.
const OUTPUT: u64 = 0x0F11_E000;

/// The handles the session granted the slots under, in slot order.
const SLOTS: [u64; RECORDER_SLOTS] = [0x5107_0001, 0x5107_0002];

const MS: u64 = 1_000_000;

const SECOND: u64 = 1_000 * MS;

type TestRecorder = Recorder<Pipes, Records, FakeHost>;

/// The recorded area: small, so each frame codes at once.
fn area() -> RecorderSlot {
    RecorderSlot::new(32, 16).expect("a slot")
}

/// The whole area, as a frame's damage names it.
const fn whole() -> DamageRect {
    DamageRect {
        x: 0,
        y: 0,
        width_px: 32,
        height_px: 16,
    }
}

fn start(sound: RecordSound) -> SessionMessage {
    SessionMessage::Start(RecordStart {
        slot: area(),
        sound,
        output: OUTPUT,
        slots: SLOTS,
        started_ns: STARTED_NS,
    })
}

/// A frame in `slot`, shown `after_ns` into the recording, where `rects`
/// changed.
fn frame(slot: u8, after_ns: u64, rects: &[DamageRect]) -> SessionMessage {
    SessionMessage::Frame {
        slot,
        at_ns: STARTED_NS + after_ns,
        damage: RecorderDamage::new(rects).expect("damage"),
    }
}

/// A recorder over fresh pipes, the session's end of them, and its log.
fn recorder() -> (TestRecorder, Pipes, Records) {
    let (pipes, records) = (Pipes::default(), Records::default());
    let recorder = Recorder::new(pipes.clone(), records.clone()).expect("the pipes' queues");
    (recorder, pipes, records)
}

/// One turn of the run loop: what the session wrote is read, the recorder
/// served, and what it queued written.
fn turn(recorder: &mut TestRecorder, host: &mut FakeHost) -> Option<Exit> {
    recorder.on_readable();
    let exit = recorder.serve(host);
    recorder.on_writable();
    exit.or_else(|| recorder.serve(host))
}

/// Turn until the recorder ends.
fn until_exit(recorder: &mut TestRecorder, host: &mut FakeHost) -> Exit {
    (0..64)
        .find_map(|_| turn(recorder, host))
        .expect("the recorder ends")
}

/// The one operand names the session in the spelling its spawner renders, and
/// nothing else does: no operand, two, another spelling or the kernel.
#[test]
fn the_session_is_named_by_the_one_operand_alone() {
    let session = ProcId::from_raw([0x3C; tairix_abi::PROC_ID_LEN]);
    let mut hex = [0u8; tairix_abi::PROC_ID_HEX_LEN];
    let named = session.write_hex(&mut hex);
    assert_eq!(client_of(&[named]), Some(session));
    assert_eq!(client_of(&[]), None);
    assert_eq!(client_of(&[named, named]), None);
    assert_eq!(client_of(&["3C3C"]), None);
    let mut zeros = [0u8; tairix_abi::PROC_ID_HEX_LEN];
    assert_eq!(client_of(&[ProcId::KERNEL.write_hex(&mut zeros)]), None);
}

/// A recorder the session has started recording `sound`, each slot mapped by
/// the handle its `Start` carried.
fn begun(host: &mut FakeHost, sound: RecordSound) -> (TestRecorder, Pipes, Records) {
    let (mut recorder, pipes, records) = recorder();
    pipes.send(&start(sound));
    assert_eq!(turn(&mut recorder, host), None);
    assert_eq!(
        pipes.said(),
        [RecorderMessage::Hello, RecorderMessage::Ready]
    );
    assert_eq!(host.mapped, SLOTS, "in slot order");
    (recorder, pipes, records)
}

/// The frames of the track coded as `codec` the file holds, each with its time
/// and whether it is a keyframe, and whether the file has its index.
fn track(bytes: &[u8], codec: Codec) -> (Vec<(u64, bool)>, bool) {
    let mut input = bytes;
    let mut demuxer = Demuxer::open(&mut input).expect("the file opens");
    let number = demuxer
        .tracks()
        .iter()
        .find(|declared| declared.codec == codec)
        .map(|declared| declared.number)
        .expect("the track");
    let mut frames = Vec::new();
    while let Some(frame) = demuxer.next_frame(&mut input).expect("every frame reads") {
        if frame.track == number {
            frames.push((frame.timestamp_ns, frame.keyframe));
        }
    }
    (frames, demuxer.has_index())
}

fn video(bytes: &[u8]) -> (Vec<(u64, bool)>, bool) {
    track(bytes, Codec::Vp8)
}

/// Whether `said` holds a `Finished`.
fn finished(said: &[RecorderMessage]) -> bool {
    said.iter()
        .any(|message| matches!(message, RecorderMessage::Finished { .. }))
}

#[test]
fn the_conversation_runs_in_its_one_order_and_stop_indexes_the_file() {
    let mut host = FakeHost::default();
    let (mut recorder, pipes, _) = begun(&mut host, RecordSound::None);
    pipes.send(&frame(0, 0, &[whole()]));
    assert_eq!(turn(&mut recorder, &mut host), None);
    let moved = DamageRect {
        x: 4,
        y: 2,
        width_px: 8,
        height_px: 8,
    };
    pipes.send(&frame(1, 40 * MS, &[moved]));
    assert_eq!(turn(&mut recorder, &mut host), None);
    pipes.send(&SessionMessage::Stop);
    let exit = until_exit(&mut recorder, &mut host);
    assert_eq!(
        exit,
        Exit {
            code: EXIT_FINISHED,
            reason: None
        }
    );
    let bytes = host.disk.bytes();
    let done = RecorderMessage::Finished {
        bytes: bytes.len() as u64,
        ending: RecordEnding::Stopped,
    };
    assert_eq!(
        pipes.said()[2..],
        [
            RecorderMessage::Released { slot: 0 },
            RecorderMessage::Released { slot: 1 },
            done
        ]
    );
    assert_eq!(
        video(&bytes),
        (vec![(0, true), (40 * MS, false)], true),
        "the seek index went into its reservation"
    );
    assert!(
        host.disk.0.borrow().synced,
        "the file was made durable before it was said finished"
    );
}

#[test]
fn a_recording_stopped_before_its_first_frame_still_makes_a_file() {
    let mut host = FakeHost::default();
    let (mut recorder, pipes, _) = begun(&mut host, RecordSound::None);
    assert!(
        host.disk.bytes().is_empty(),
        "the first job writes the header"
    );
    pipes.send(&SessionMessage::Stop);
    assert_eq!(until_exit(&mut recorder, &mut host).code, EXIT_FINISHED);
    let bytes = host.disk.bytes();
    let done = RecorderMessage::Finished {
        bytes: bytes.len() as u64,
        ending: RecordEnding::Stopped,
    };
    assert_eq!(pipes.said()[2..], [done]);
    assert!(!bytes.is_empty());
    assert!(video(&bytes).0.is_empty());
}

#[test]
fn a_volume_too_full_for_the_header_leaves_the_file_empty() {
    let mut host = FakeHost::default();
    host.disk.0.borrow_mut().capacity = Some(16);
    let (mut recorder, pipes, _) = begun(&mut host, RecordSound::None);
    pipes.send(&frame(0, 0, &[whole()]));
    assert_eq!(until_exit(&mut recorder, &mut host).code, EXIT_FINISHED);
    let full = RecorderMessage::Finished {
        bytes: 0,
        ending: RecordEnding::DiskFull,
    };
    assert_eq!(
        pipes.said()[2..],
        [RecorderMessage::Released { slot: 0 }, full]
    );
    assert!(host.disk.bytes().is_empty());
}

/// Record a first frame, then have the session `send` what is refused: how
/// the recorder ended, what it said, what it logged, and the file it left.
fn refusing(send: impl FnOnce(&Pipes)) -> (Exit, Vec<RecorderMessage>, Vec<u32>, Vec<u8>) {
    let mut host = FakeHost::default();
    let (mut recorder, pipes, records) = begun(&mut host, RecordSound::None);
    pipes.send(&frame(0, 0, &[whole()]));
    assert_eq!(turn(&mut recorder, &mut host), None);
    send(&pipes);
    let exit = until_exit(&mut recorder, &mut host);
    (exit, pipes.said(), records.ids(), host.disk.bytes())
}

/// What `send` is ends the recording for `reason`: stated, logged as a
/// refusal, no `Finished` said, and the frame before it in a file finished
/// whole.
fn refused(send: impl FnOnce(&Pipes), reason: Reason) {
    let (exit, said, ids, bytes) = refusing(send);
    assert_eq!(
        exit,
        Exit {
            code: EXIT_ENDED,
            reason: Some(reason.said())
        }
    );
    assert!(ids.contains(&RECORDER_REFUSED.0) && ids.contains(&RECORDER_ENDED.0));
    assert!(!finished(&said), "a refusal is never answered Finished");
    assert_eq!(
        said[2..],
        [RecorderMessage::Released { slot: 0 }],
        "only the first frame was coded"
    );
    assert_eq!(video(&bytes), (vec![(0, true)], true));
}

#[test]
fn a_frame_naming_a_rectangle_outside_its_slot_ends_the_recording() {
    let past = DamageRect {
        x: 30,
        y: 0,
        width_px: 4,
        height_px: 1,
    };
    refused(
        |pipes| pipes.send(&frame(1, 40 * MS, &[past])),
        Reason::Outside,
    );
    let below = DamageRect {
        x: 0,
        y: 15,
        width_px: 1,
        height_px: 2,
    };
    refused(
        |pipes| pipes.send(&frame(1, 40 * MS, &[below])),
        Reason::Outside,
    );
}

#[test]
fn a_rectangle_whose_far_edge_overflows_is_outside_its_slot() {
    let wide = DamageRect {
        x: u32::MAX - 1,
        y: 0,
        width_px: 4,
        height_px: 1,
    };
    refused(
        |pipes| pipes.send(&frame(1, 40 * MS, &[wide])),
        Reason::Outside,
    );
    let tall = DamageRect {
        x: 0,
        y: 1,
        width_px: 1,
        height_px: u32::MAX,
    };
    refused(
        |pipes| pipes.send(&frame(1, 40 * MS, &[tall])),
        Reason::Outside,
    );
}

#[test]
fn a_frame_naming_a_slot_past_the_ring_is_refused() {
    let mut out = [0u8; RECORDER_SESSION_MESSAGE_MAX];
    let len = frame(1, 40 * MS, &[whole()])
        .encode(&mut out)
        .expect("encodes");
    out[1] = 2;
    refused(
        |pipes| pipes.send_raw(&out[..len]),
        Reason::Undecodable(Errno::OutOfRange),
    );
}

#[test]
fn a_frame_shown_before_the_last_ends_the_recording() {
    let early = SessionMessage::Frame {
        slot: 1,
        at_ns: STARTED_NS - 1,
        damage: RecorderDamage::new(&[whole()]).expect("damage"),
    };
    refused(|pipes| pipes.send(&early), Reason::Backwards);
}

#[test]
fn a_second_start_ends_the_recording() {
    refused(
        |pipes| pipes.send(&start(RecordSound::None)),
        Reason::OutOfTurn,
    );
}

#[test]
fn a_frame_for_a_slot_still_being_encoded_ends_the_recording() {
    let mut host = FakeHost::default();
    let (mut recorder, pipes, records) = begun(&mut host, RecordSound::None);
    host.hold.set(true);
    pipes.send(&frame(0, 0, &[whole()]));
    assert_eq!(turn(&mut recorder, &mut host), None);
    pipes.send(&frame(0, 40 * MS, &[whole()]));
    assert_eq!(
        turn(&mut recorder, &mut host),
        None,
        "the end waits for the frame being coded"
    );
    assert_eq!(pipes.said().len(), 2, "nothing is released while it codes");
    host.hold.set(false);
    let exit = until_exit(&mut recorder, &mut host);
    assert_eq!(
        exit,
        Exit {
            code: EXIT_ENDED,
            reason: Some(Reason::Busy.said())
        }
    );
    assert_eq!(pipes.said()[2..], [RecorderMessage::Released { slot: 0 }]);
    assert!(records.ids().contains(&RECORDER_REFUSED.0));
    assert_eq!(video(&host.disk.bytes()), (vec![(0, true)], true));
}

#[test]
fn a_closed_session_pipe_finishes_the_file() {
    let mut host = FakeHost::default();
    let (mut recorder, pipes, _) = begun(&mut host, RecordSound::None);
    pipes.send(&frame(0, 0, &[whole()]));
    pipes.close();
    let exit = until_exit(&mut recorder, &mut host);
    assert_eq!(
        exit,
        Exit {
            code: EXIT_ENDED,
            reason: Some(Reason::Orphaned.said())
        }
    );
    assert!(!finished(&pipes.said()), "a closed pipe is owed no answer");
    assert_eq!(video(&host.disk.bytes()), (vec![(0, true)], true));
}

#[test]
fn a_full_volume_ends_disk_full_with_a_whole_file() {
    let mut host = FakeHost::default();
    let (mut recorder, pipes, _) = begun(&mut host, RecordSound::None);
    pipes.send(&frame(0, 0, &[whole()]));
    assert_eq!(turn(&mut recorder, &mut host), None);
    let kept = host.disk.bytes().len();
    host.disk.0.borrow_mut().capacity = Some(kept + 8);
    pipes.send(&frame(1, 40 * MS, &[whole()]));
    let exit = until_exit(&mut recorder, &mut host);
    assert_eq!(exit.code, EXIT_FINISHED);
    let bytes = host.disk.bytes();
    assert_eq!(bytes.len(), kept, "cut at the last block written whole");
    let full = RecorderMessage::Finished {
        bytes: bytes.len() as u64,
        ending: RecordEnding::DiskFull,
    };
    assert_eq!(
        pipes.said()[2..],
        [
            RecorderMessage::Released { slot: 0 },
            RecorderMessage::Released { slot: 1 },
            full
        ]
    );
    assert_eq!(
        video(&bytes),
        (vec![(0, true)], true),
        "its index kept in the reservation"
    );
}

#[test]
fn each_sound_choice_opens_its_sources_and_sets_them_moving() {
    let cases = [
        (RecordSound::None, vec![]),
        (
            RecordSound::Microphone,
            vec![(CaptureSource::Input(0), ChannelMap::MONO)],
        ),
        (
            RecordSound::Desktop,
            vec![(CaptureSource::Monitor(0), ChannelMap::STEREO)],
        ),
        (
            RecordSound::Both,
            vec![
                (CaptureSource::Monitor(0), ChannelMap::STEREO),
                (CaptureSource::Input(0), ChannelMap::MONO),
            ],
        ),
    ];
    for (choice, sources) in cases {
        let mut host = FakeHost::default();
        let _recording = begun(&mut host, choice);
        let opened: Vec<_> = host
            .streams
            .iter()
            .map(|(source, layout, _)| (*source, *layout))
            .collect();
        assert_eq!(opened, sources, "{choice:?}");
        assert!(
            host.streams
                .iter()
                .all(|(_, _, stream)| stream.0.borrow().started),
            "{choice:?}"
        );
    }
}

#[test]
fn a_recording_with_sound_carries_both_tracks_on_one_timeline() {
    let mut host = FakeHost::default();
    let (mut recorder, pipes, _) = begun(&mut host, RecordSound::Desktop);
    let desktop = host.streams[0].2.clone();
    for period in 0..10u8 {
        desktop.capture(PERIOD, 0.25);
        pipes.send(&frame(period % 2, u64::from(period) * 20 * MS, &[whole()]));
        assert_eq!(turn(&mut recorder, &mut host), None);
    }
    pipes.send(&SessionMessage::Stop);
    assert_eq!(until_exit(&mut recorder, &mut host).code, EXIT_FINISHED);
    let bytes = host.disk.bytes();
    let (pictures, _) = video(&bytes);
    let (packets, _) = track(&bytes, Codec::Opus);
    let every = (0..11)
        .map(|period| (period * 20 * MS, true))
        .collect::<Vec<_>>();
    assert_eq!(
        packets, every,
        "each period heard, and one more the coder's delay plays out in"
    );
    assert_eq!(pictures.len(), 10);
}

/// What `arrange` does to the host keeps a recording of `sound` from
/// beginning: answered `Unstarted(answered)`, logged once, the reason stated,
/// and the file left as it was.
fn unstarted(arrange: impl FnOnce(&mut FakeHost), sound: RecordSound, answered: Errno) {
    let mut host = FakeHost::default();
    arrange(&mut host);
    let file = host.disk.bytes();
    let (mut recorder, pipes, records) = recorder();
    pipes.send(&start(sound));
    let exit = until_exit(&mut recorder, &mut host);
    assert_eq!(exit.code, EXIT_ENDED, "{answered:?}");
    assert!(exit.reason.is_some(), "{answered:?}");
    assert_eq!(
        pipes.said(),
        [RecorderMessage::Hello, RecorderMessage::Unstarted(answered)]
    );
    assert_eq!(records.ids(), [RECORDER_UNSTARTED.0]);
    assert_eq!(host.disk.bytes(), file, "{answered:?}: nothing was written");
}

#[test]
fn what_cannot_begin_is_answered_unstarted_with_its_reason() {
    unstarted(
        |host| host.refuse_output = Some(Errno::NotFound),
        RecordSound::None,
        Errno::NotFound,
    );
    unstarted(
        |host| host.disk.0.borrow_mut().bytes.push(1),
        RecordSound::None,
        Errno::AlreadyExists,
    );
    unstarted(|host| host.room = 1, RecordSound::None, Errno::OutOfMemory);
    unstarted(
        |host| host.refuse_map = Some(Errno::NotFound),
        RecordSound::None,
        Errno::NotFound,
    );
    for wrong in [area().len() - 1, area().len() + PAGE_SIZE] {
        unstarted(
            |host| host.mapped_len = Some(wrong),
            RecordSound::None,
            Errno::LengthOutOfRange,
        );
    }
    unstarted(
        |host| host.refuse_sound = Some(Errno::PermissionDenied),
        RecordSound::Microphone,
        Errno::PermissionDenied,
    );
    unstarted(
        |host| host.refuse_sound = Some(Errno::SeatNotOwner),
        RecordSound::Desktop,
        Errno::PermissionDenied,
    );
}

#[test]
fn a_start_of_another_protocol_is_answered_unstarted() {
    let mut out = [0u8; RECORDER_SESSION_MESSAGE_MAX];
    let len = start(RecordSound::None).encode(&mut out).expect("encodes");
    out[2] = 2;
    let mut host = FakeHost::default();
    let (mut recorder, pipes, _) = recorder();
    pipes.send_raw(&out[..len]);
    assert_eq!(until_exit(&mut recorder, &mut host).code, EXIT_ENDED);
    assert_eq!(
        pipes.said(),
        [
            RecorderMessage::Hello,
            RecorderMessage::Unstarted(Errno::AbiVersionUnsupported)
        ]
    );
}

#[test]
fn a_frame_before_start_is_answered_unstarted() {
    let mut host = FakeHost::default();
    let (mut recorder, pipes, _) = recorder();
    pipes.send(&frame(0, 0, &[whole()]));
    assert_eq!(until_exit(&mut recorder, &mut host).code, EXIT_ENDED);
    assert_eq!(
        pipes.said(),
        [
            RecorderMessage::Hello,
            RecorderMessage::Unstarted(Errno::BadMagic)
        ]
    );
    assert!(
        host.mapped.is_empty(),
        "no slot is mapped for a conversation that never began"
    );
}

/// A slot granted to its last page is the slot's own size: it is mapped, and
/// its frames are coded.
#[test]
fn a_slot_granted_to_its_last_page_is_mapped() {
    let mut host = FakeHost {
        mapped_len: Some(PAGE_SIZE),
        ..FakeHost::default()
    };
    let (mut recorder, pipes, _) = begun(&mut host, RecordSound::None);
    pipes.send(&frame(0, 0, &[whole()]));
    assert_eq!(turn(&mut recorder, &mut host), None);
    assert_eq!(pipes.said()[2..], [RecorderMessage::Released { slot: 0 }]);
}

/// A slot that does not map refuses the start before any sound is opened.
#[test]
fn a_slot_that_does_not_map_opens_no_sound() {
    let mut host = FakeHost {
        refuse_map: Some(Errno::NotFound),
        ..FakeHost::default()
    };
    let (mut recorder, pipes, _) = recorder();
    pipes.send(&start(RecordSound::Both));
    assert_eq!(until_exit(&mut recorder, &mut host).code, EXIT_ENDED);
    assert_eq!(
        pipes.said(),
        [
            RecorderMessage::Hello,
            RecorderMessage::Unstarted(Errno::NotFound)
        ]
    );
    assert!(
        host.streams.is_empty(),
        "no sound is opened for a start refused"
    );
}

#[test]
fn a_session_gone_before_it_starts_ends_the_recorder_with_its_reason() {
    let mut host = FakeHost::default();
    let (mut recorder, pipes, records) = recorder();
    pipes.close();
    let exit = until_exit(&mut recorder, &mut host);
    assert_eq!(
        exit,
        Exit {
            code: EXIT_ENDED,
            reason: Some(Reason::Orphaned.said())
        }
    );
    assert_eq!(pipes.said(), [RecorderMessage::Hello]);
    assert_eq!(records.ids(), [RECORDER_ENDED.0]);
}

#[test]
fn keyframes_come_every_ten_seconds_of_recording() {
    let mut host = FakeHost::default();
    let (mut recorder, pipes, _) = begun(&mut host, RecordSound::None);
    let corner = DamageRect {
        x: 0,
        y: 0,
        width_px: 2,
        height_px: 2,
    };
    for second in 0..26u8 {
        pipes.send(&frame(second % 2, u64::from(second) * SECOND, &[corner]));
        assert_eq!(turn(&mut recorder, &mut host), None);
    }
    pipes.send(&SessionMessage::Stop);
    assert_eq!(until_exit(&mut recorder, &mut host).code, EXIT_FINISHED);
    let (pictures, indexed) = video(&host.disk.bytes());
    let keys: Vec<u64> = pictures
        .iter()
        .filter(|(_, key)| *key)
        .map(|(at, _)| at / SECOND)
        .collect();
    assert_eq!(keys, [0, 10, 20]);
    assert_eq!(pictures.len(), 26);
    assert!(indexed);
}
