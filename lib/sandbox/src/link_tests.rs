//! The worker's timed end of a session: frames arrive whole across bounded
//! waits, a burst is drained without reading again, and the stream's ends and
//! oversize declarations are refused as the owner's seam refuses them.

use super::{TimedChannel, Waited, WorkerLink};
use crate::proto::{Channel, ProtoError, FRAME_HEADER_LEN, MAX_FRAME};
use crate::session::{SessionError, MIN_QUEUE_BYTES};
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use tairix_abi::Errno;

/// One scripted read outcome.
enum Step {
    Bytes(Vec<u8>),
    Wait,
    End,
}

/// A channel whose reads follow a script, recording each wait it was asked
/// for, and whose writes are kept.
struct Scripted {
    steps: VecDeque<Step>,
    waits: Vec<Option<u64>>,
    written: Vec<u8>,
}

impl Scripted {
    fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            steps: steps.into_iter().collect(),
            waits: Vec::new(),
            written: Vec::new(),
        }
    }
}

impl Channel for Scripted {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        self.read_within(buf, None)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }
}

impl TimedChannel for Scripted {
    fn read_within(&mut self, buf: &mut [u8], wait_ns: Option<u64>) -> Result<usize, Errno> {
        self.waits.push(wait_ns);
        match self.steps.pop_front() {
            Some(Step::Bytes(mut bytes)) => {
                let n = bytes.len().min(buf.len());
                buf[..n].copy_from_slice(&bytes[..n]);
                if n < bytes.len() {
                    self.steps.push_front(Step::Bytes(bytes.split_off(n)));
                }
                Ok(n)
            }
            Some(Step::Wait) => Err(Errno::TimedOut),
            Some(Step::End) | None => Ok(0),
        }
    }
}

fn framed(payload: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
    out.extend_from_slice(payload);
    out
}

fn link(steps: impl IntoIterator<Item = Step>) -> WorkerLink<Scripted> {
    WorkerLink::new(Scripted::new(steps), 64).unwrap()
}

fn owned(
    link: &mut WorkerLink<Scripted>,
    wait: Option<u64>,
) -> Result<Waited<Vec<u8>>, ProtoError> {
    link.recv(wait, <[u8]>::to_vec)
}

#[test]
fn a_frame_split_across_waits_arrives_whole() {
    let bytes = framed(b"hello");
    let mut link = link([
        Step::Bytes(bytes[..2].to_vec()),
        Step::Wait,
        Step::Bytes(bytes[2..6].to_vec()),
        Step::Bytes(bytes[6..].to_vec()),
    ]);
    assert_eq!(
        owned(&mut link, Some(5)),
        Ok(Waited::Nothing),
        "half a header"
    );
    assert_eq!(
        owned(&mut link, Some(7)),
        Ok(Waited::Nothing),
        "the wait ended"
    );
    assert_eq!(
        owned(&mut link, Some(9)),
        Ok(Waited::Nothing),
        "part of the payload"
    );
    assert_eq!(owned(&mut link, None), Ok(Waited::Frame(b"hello".to_vec())));
    assert_eq!(
        link.chan.waits,
        [Some(5), Some(7), Some(9), None],
        "each read waited as asked"
    );
}

#[test]
fn a_burst_is_drained_without_reading_again() {
    let mut burst = framed(b"one");
    burst.extend(framed(b""));
    burst.extend(framed(b"three"));
    let mut link = link([Step::Bytes(burst)]);
    assert_eq!(
        owned(&mut link, Some(0)),
        Ok(Waited::Frame(b"one".to_vec()))
    );
    assert_eq!(owned(&mut link, Some(0)), Ok(Waited::Frame(Vec::new())));
    assert_eq!(
        owned(&mut link, Some(0)),
        Ok(Waited::Frame(b"three".to_vec()))
    );
    assert_eq!(link.chan.waits.len(), 1, "one read took the whole burst");
    assert_eq!(owned(&mut link, Some(0)), Ok(Waited::Closed));
}

#[test]
fn a_frame_past_the_bound_is_refused_before_its_payload() {
    let max = 64 - FRAME_HEADER_LEN;
    let mut link = link([Step::Bytes(framed(&vec![7; max]))]);
    assert_eq!(link.max_recv_payload(), max);
    assert_eq!(
        owned(&mut link, None),
        Ok(Waited::Frame(vec![7; max])),
        "the largest fits"
    );
    let declared = u32::try_from(max + 1).unwrap().to_le_bytes().to_vec();
    let mut link = WorkerLink::new(Scripted::new([Step::Bytes(declared)]), 64).unwrap();
    assert_eq!(owned(&mut link, None), Err(ProtoError::Oversize));
    assert_eq!(
        owned(&mut link, None),
        Err(ProtoError::Oversize),
        "and stays refused"
    );
    assert_eq!(link.chan.waits.len(), 1, "no payload byte was asked for");
}

#[test]
fn the_stream_ends_cleanly_only_on_a_frame_boundary() {
    let mut link = link([Step::End]);
    assert_eq!(owned(&mut link, Some(3)), Ok(Waited::Closed));
    let mut cut = link_of_cut();
    assert_eq!(owned(&mut cut, None), Ok(Waited::Nothing));
    assert_eq!(owned(&mut cut, None), Err(ProtoError::PeerClosed));
}

fn link_of_cut() -> WorkerLink<Scripted> {
    let bytes = framed(b"cut short");
    link([Step::Bytes(bytes[..7].to_vec()), Step::End])
}

#[test]
fn a_transport_failure_is_carried_and_a_broken_pipe_is_the_peer_gone() {
    struct Failing(Errno);
    impl Channel for Failing {
        fn read(&mut self, _: &mut [u8]) -> Result<usize, Errno> {
            Err(self.0)
        }
        fn write(&mut self, _: &[u8]) -> Result<usize, Errno> {
            Err(self.0)
        }
    }
    impl TimedChannel for Failing {
        fn read_within(&mut self, _: &mut [u8], _: Option<u64>) -> Result<usize, Errno> {
            Err(self.0)
        }
    }
    let mut broken = WorkerLink::new(Failing(Errno::BrokenPipe), 64).unwrap();
    assert_eq!(broken.recv(None, |_| ()), Err(ProtoError::PeerClosed));
    assert_eq!(broken.send(b"x"), Err(ProtoError::PeerClosed));
    let mut failing = WorkerLink::new(Failing(Errno::BadAddress), 64).unwrap();
    assert_eq!(
        failing.recv(None, |_| ()),
        Err(ProtoError::Channel(Errno::BadAddress))
    );
}

#[test]
fn a_sent_payload_is_framed_and_an_oversize_one_writes_nothing() {
    let mut link = link([]);
    link.send(b"abc").unwrap();
    assert_eq!(link.chan.written, framed(b"abc"));
    assert_eq!(
        link.send(&vec![0; MAX_FRAME + 1]),
        Err(ProtoError::Oversize)
    );
    assert_eq!(link.chan.written, framed(b"abc"));
}

#[test]
fn queued_frames_go_in_one_write_at_the_flush() {
    struct Counting(Vec<Vec<u8>>);
    impl Channel for Counting {
        fn read(&mut self, _: &mut [u8]) -> Result<usize, Errno> {
            Ok(0)
        }
        fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
            self.0.push(buf.to_vec());
            Ok(buf.len())
        }
    }
    impl TimedChannel for Counting {
        fn read_within(&mut self, _: &mut [u8], _: Option<u64>) -> Result<usize, Errno> {
            Ok(0)
        }
    }
    let mut link = WorkerLink::new(Counting(Vec::new()), 64).unwrap();
    link.queue(b"one").unwrap();
    link.queue(b"").unwrap();
    link.queue(b"three").unwrap();
    assert!(link.chan.0.is_empty(), "nothing written before the flush");
    link.flush().unwrap();
    let mut expected = framed(b"one");
    expected.extend(framed(b""));
    expected.extend(framed(b"three"));
    assert_eq!(link.chan.0, [expected], "one write for all three");
    link.flush().unwrap();
    assert_eq!(link.chan.0.len(), 1, "a flush of nothing writes nothing");
    assert_eq!(
        link.queue(&vec![0; MAX_FRAME + 1]),
        Err(ProtoError::Oversize)
    );
}

#[test]
fn a_bound_too_small_for_any_frame_is_refused() {
    assert!(matches!(
        WorkerLink::new(Scripted::new([]), MIN_QUEUE_BYTES - 1),
        Err(SessionError::BoundTooSmall)
    ));
    assert!(WorkerLink::new(Scripted::new([]), MIN_QUEUE_BYTES).is_ok());
}
