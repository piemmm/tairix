//! A machine's sound on its way to the audio stream: the worker's blocks kept
//! while the stream's ring is full, and how much both hold, which steers the
//! worker's mix.

use alloc::collections::VecDeque;

use crate::link::AUDIO_FRAME;

/// The stream the sound plays through: a ring the front-end appends to.
pub trait Stream {
    /// Append whole frames of `samples`: the bytes the ring took, fewer when
    /// it filled.
    ///
    /// # Errors
    ///
    /// The ring's refusal; what it took before refusing is lost.
    fn append(&mut self, samples: &[u8]) -> Result<usize, StreamError>;

    /// Frames the ring holds that have not yet played.
    fn queued(&mut self) -> u32;
}

/// Why the stream refused sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamError;

/// The sound waiting for room in the stream's ring.
#[derive(Debug)]
pub struct Sound {
    waiting: VecDeque<u8>,
    most: usize,
}

impl Sound {
    /// Keep at most `frames` frames waiting; a machine further ahead of the
    /// stream than that has its oldest sound dropped, as the worker is
    /// steered back by the fill it is told.
    #[must_use]
    pub fn new(frames: u32) -> Self {
        let most = usize::try_from(frames)
            .unwrap_or(usize::MAX)
            .saturating_mul(AUDIO_FRAME);
        Self {
            waiting: VecDeque::new(),
            most,
        }
    }

    /// Take a block of the worker's sound, whole frames of it, and hand
    /// what the stream takes on.
    ///
    /// # Errors
    ///
    /// The stream's refusal, as [`Self::feed`].
    pub fn take(&mut self, pcm: &[u8], stream: &mut impl Stream) -> Result<(), StreamError> {
        let whole = pcm.len() - pcm.len() % AUDIO_FRAME;
        let mut rest = &pcm[..whole];
        if self.waiting.is_empty() {
            let taken = stream.append(rest)?;
            rest = &rest[taken.min(rest.len())..];
        }
        if rest.len() >= self.most {
            self.waiting.clear();
            rest = &rest[rest.len() - self.most..];
        } else {
            let over = (self.waiting.len() + rest.len()).saturating_sub(self.most);
            self.waiting.drain(..over);
        }
        if self.waiting.try_reserve(rest.len()).is_ok() {
            self.waiting.extend(rest);
        }
        self.feed(stream)
    }

    /// Hand the stream what waits, as much as its ring takes.
    ///
    /// # Errors
    ///
    /// The stream's refusal; what waits is dropped with it, since a refused
    /// ring takes nothing more.
    pub fn feed(&mut self, stream: &mut impl Stream) -> Result<(), StreamError> {
        if self.waiting.is_empty() {
            return Ok(());
        }
        // Whole, so no frame is split across the two halves of the ring.
        let waiting = self.waiting.make_contiguous();
        match stream.append(waiting) {
            Ok(taken) => {
                let taken = taken.min(waiting.len());
                self.waiting.drain(..taken);
                Ok(())
            }
            Err(error) => {
                self.waiting.clear();
                Err(error)
            }
        }
    }

    /// The frames the stream and this hold between them, not yet played.
    pub fn fill(&self, stream: &mut impl Stream) -> u32 {
        let waiting = u32::try_from(self.waiting.len() / AUDIO_FRAME).unwrap_or(u32::MAX);
        stream.queued().saturating_add(waiting)
    }
}

#[cfg(test)]
#[path = "sound_tests.rs"]
mod tests;
