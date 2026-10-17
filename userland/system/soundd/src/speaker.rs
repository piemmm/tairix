//! The authority's `audio-v1` streams, one per sounding cue, through an
//! injected seam so the authority is host-tested with no mixer.

use tairix_abi::audio::{AudioGain, AudioNotify, OpenParams};
use tairix_abi::driver::audio::Frames;
use tairix_abi::Errno;

/// The audio service, as the authority reaches it.
pub trait Speakers {
    /// One open stream.
    type Stream;

    /// Open a stream on `params`, its ring attached.
    ///
    /// # Errors
    ///
    /// The service's refusal — [`Errno::SeatNotOwner`] for a room the cue is
    /// not admitted to, [`Errno::DeviceOffline`] for a machine with no sink —
    /// or the transport's.
    fn open(&mut self, params: &OpenParams) -> Result<Self::Stream, Errno>;

    /// The mailbox `stream`'s notifications arrive in, for the wait-set.
    fn mailbox(&self, stream: &Self::Stream) -> u64;

    /// The id the service granted `stream`, which every notification about
    /// it carries.
    fn stream_id(&self, stream: &Self::Stream) -> u64;

    /// Publish `samples` from frame `at`, answering how many frames the ring
    /// took.
    ///
    /// # Errors
    ///
    /// The ring's refusal.
    fn write(
        &mut self,
        stream: &mut Self::Stream,
        at: Frames,
        samples: &[u8],
    ) -> Result<u32, Errno>;

    /// Set the stream's level.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    fn set_gain(&mut self, stream: &mut Self::Stream, gain: AudioGain) -> Result<(), Errno>;

    /// Start the stream at its first frame.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    fn start(&mut self, stream: &mut Self::Stream) -> Result<(), Errno>;

    /// Play out what is queued, then stop.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    fn drain(&mut self, stream: &mut Self::Stream) -> Result<(), Errno>;

    /// The next notification waiting for `stream`, never parking.
    ///
    /// # Errors
    ///
    /// The transport's refusal, or a malformed notification.
    fn next_notify(&mut self, stream: &mut Self::Stream) -> Result<Option<AudioNotify>, Errno>;

    /// Close the stream.
    fn close(&mut self, stream: Self::Stream);
}
