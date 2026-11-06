//! `WebM`: the Matroska subset browsers record and play, holding VP8 video
//! and Opus audio (RFC 8794's EBML, RFC 9559's Matroska and the `WebM`
//! container guidelines).
//!
//! [`Muxer`] writes a recording as it is made, through positional writes to a
//! file it never reads back. The Segment and each Cluster open with an unknown
//! size that closing them patches, so the file of a writer that died is a live
//! stream that plays as far as it got. The two tracks' frames are interleaved
//! by timestamp, and a Cluster opens at every video keyframe and wherever a
//! block's timestamp is further from its Cluster's than a block's signed
//! 16-bit offset reaches.
//!
//! [`Demuxer`] reads an untrusted file a frame at a time through
//! [`SoundInput`](tairix_sound::SoundInput), the random-access input the
//! sandbox's page cache serves, answering each frame's track, timestamp,
//! keyframe flag and where its payload lies. It reads element headers and the
//! few small elements it interprets, never a payload.
//!
//! # The seek index
//!
//! The index is written as a recording finishes, into space reserved at the
//! start. Finishing then needs no new space, so a recording a full disk stopped
//! still gets its index, and a reader starting at the front finds it before the
//! first Cluster. The reservation holds as many keyframes as the caller states;
//! an index that outgrows it is appended after the last Cluster, the seek head
//! pointing there, and one that cannot be appended either keeps every n-th
//! keyframe, so it still spans the whole recording.
//!
//! # Bounds and fail-closed policy
//!
//! Every element ID and size is held to EBML's rules, and every size to the
//! element holding it. Only the Segment and a Cluster may leave their size
//! unknown, and nothing is recursed into: the elements interpreted sit at the
//! depths the format fixes for them, and any other is stepped over by its size.
//! An interpreted element is read whole only up to
//! [`MAX_READ`](tairix_sound::MAX_READ) bytes, a frame holds at most
//! [`MAX_FRAME_BYTES`], a file declares at most [`MAX_TRACKS`] tracks, and an
//! index keeps at most [`MAX_INDEX_POINTS`] points. A malformed file is refused
//! with a [`DemuxError`], never a panic.
//!
//! # Streaming
//!
//! A demuxer call that meets bytes the input does not hold yet answers
//! [`DemuxError::InputUnavailable`]; asked again once they are there, it
//! answers as it would have. Opening reads the elements ahead of the first
//! Cluster afresh each time, and every later call keeps each element it has
//! read whole, so a retry gets further and needs one element's bytes at a time.

mod demux;
mod ebml;
mod id;
mod mux;

pub use self::demux::{Codec, DemuxError, Demuxer, Ending, Frame, Media, Track};
pub use self::mux::{Finished, Layout, MuxError, Muxer, OpusChannels, OpusTrack, Placement};

/// The four bytes every EBML document, and so every `WebM` file, begins with.
pub const MAGIC: [u8; 4] = id::EBML.to_be_bytes();

/// Most bytes one frame may hold: the muxer refuses a larger frame, and the
/// demuxer a block laying one out.
pub const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Most points a seek index holds: the most keyframes a muxer reserves room
/// for or names, and the most cue points or Clusters a demuxer keeps.
pub const MAX_INDEX_POINTS: u32 = 65_536;

/// Most tracks a file may declare.
pub const MAX_TRACKS: usize = 32;

/// Most bytes of a codec identifier a [`Track`] keeps.
pub const MAX_CODEC_ID: usize = 32;

/// Nanoseconds a tick of the timeline stands for where a file states none,
/// and the tick the muxer writes: a millisecond.
const DEFAULT_TIMESTAMP_SCALE: u64 = 1_000_000;

/// The document type a `WebM` file's EBML header names.
const DOC_TYPE: &str = "webm";

/// VP8's codec identifier.
const VP8_CODEC: &str = "V_VP8";

/// Opus's codec identifier.
const OPUS_CODEC: &str = "A_OPUS";

/// Matroska's track type for pictures.
const VIDEO_TYPE: u8 = 1;

/// Matroska's track type for sound.
const AUDIO_TYPE: u8 = 2;

/// The `SimpleBlock` flag marking a keyframe.
const KEYFRAME_FLAG: u8 = 0x80;

#[cfg(test)]
#[path = "webm_tests.rs"]
mod tests;
