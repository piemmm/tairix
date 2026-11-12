use super::*;
use alloc::vec::Vec;

/// A ring of `room` frames that plays nothing until told.
struct Ring {
    room: usize,
    held: Vec<u8>,
    refuse: bool,
}

impl Ring {
    fn of(frames: usize) -> Self {
        Self {
            room: frames,
            held: Vec::new(),
            refuse: false,
        }
    }

    fn play(&mut self, frames: usize) -> Vec<u8> {
        let n = (frames * AUDIO_FRAME).min(self.held.len());
        self.held.drain(..n).collect()
    }
}

impl Stream for Ring {
    fn append(&mut self, samples: &[u8]) -> Result<usize, StreamError> {
        if self.refuse {
            return Err(StreamError);
        }
        let free = self.room * AUDIO_FRAME - self.held.len();
        let taken = samples.len().min(free) / AUDIO_FRAME * AUDIO_FRAME;
        self.held.extend_from_slice(&samples[..taken]);
        Ok(taken)
    }

    fn queued(&mut self) -> u32 {
        u32::try_from(self.held.len() / AUDIO_FRAME).unwrap()
    }
}

fn frames(from: u8, count: u8) -> Vec<u8> {
    (from..from + count)
        .flat_map(|n| [n; AUDIO_FRAME])
        .collect()
}

#[test]
fn sound_goes_straight_through_and_waits_in_order_while_the_ring_is_full() {
    let (mut sound, mut ring) = (Sound::new(100), Ring::of(4));
    sound.take(&frames(0, 3), &mut ring).unwrap();
    assert_eq!(sound.fill(&mut ring), 3);
    sound.take(&frames(3, 3), &mut ring).unwrap();
    assert_eq!(ring.queued(), 4, "the ring full");
    assert_eq!(sound.fill(&mut ring), 6, "two waiting beside it");
    assert_eq!(ring.play(2), frames(0, 2));
    sound.feed(&mut ring).unwrap();
    sound.take(&frames(6, 1), &mut ring).unwrap();
    assert_eq!(ring.play(4), frames(2, 4));
    sound.feed(&mut ring).unwrap();
    assert_eq!(ring.play(4), frames(6, 1), "in order, none lost");
}

#[test]
fn sound_too_far_ahead_drops_its_oldest() {
    let (mut sound, mut ring) = (Sound::new(3), Ring::of(1));
    sound.take(&frames(0, 2), &mut ring).unwrap();
    sound.take(&frames(2, 3), &mut ring).unwrap();
    assert_eq!(
        sound.fill(&mut ring),
        4,
        "the ring's one and the newest three"
    );
    ring.play(1);
    sound.feed(&mut ring).unwrap();
    assert_eq!(ring.play(1), frames(2, 1), "frame 1 was the oldest waiting");
    sound.take(&frames(10, 9), &mut ring).unwrap();
    assert_eq!(
        sound.fill(&mut ring),
        3,
        "a block larger than the bound keeps its last three, the first in the ring"
    );
    assert_eq!(ring.play(1), frames(16, 1));
    sound.feed(&mut ring).unwrap();
    assert_eq!(ring.play(1), frames(17, 1));
}

#[test]
fn a_partial_frame_is_never_sent() {
    let (mut sound, mut ring) = (Sound::new(10), Ring::of(10));
    let mut block = frames(0, 2);
    block.push(9);
    sound.take(&block, &mut ring).unwrap();
    assert_eq!(ring.held, frames(0, 2));
}

#[test]
fn a_refused_ring_drops_what_waits() {
    let (mut sound, mut ring) = (Sound::new(10), Ring::of(1));
    sound.take(&frames(0, 3), &mut ring).unwrap();
    ring.refuse = true;
    assert_eq!(sound.feed(&mut ring), Err(StreamError));
    ring.refuse = false;
    assert_eq!(sound.fill(&mut ring), 1, "only the ring's own frame");
}
