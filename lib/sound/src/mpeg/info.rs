//! The tags an encoder writes into a stream's first frame in place of
//! audio: Xing's (`Xing`, or `Info` for a constant bitrate) with LAME's
//! extension, and Fraunhofer's VBRI.

use super::frame::{Header, HEADER_LEN};
use crate::crc::crc16_reflected;

/// Bytes of LAME's extension; its last two are a CRC over the frame up to
/// them.
const LAME_LEN: usize = 36;

/// Samples a Layer III decoder delays its output by: the IMDCT's half
/// block and the filterbank's (LAME's documented 528 + 1).
pub(super) const DECODER_DELAY: u64 = 529;

/// What a stream's tag frame says about it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct StreamInfo {
    /// Audio frames, the tag frame not among them.
    pub(super) frames: Option<u64>,
    /// Samples of silence the encoder put before the sound and after it.
    pub(super) gapless: Option<(u64, u64)>,
}

/// The tag `frame` — the stream's first frame, whose header is `header` —
/// carries in place of audio, or `None` where it is audio.
pub(super) fn parse(header: Header, frame: &[u8]) -> Option<StreamInfo> {
    xing(header, frame).or_else(|| vbri(frame))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    let field = bytes.get(at..at + 4)?;
    Some(u32::from_be_bytes([field[0], field[1], field[2], field[3]]))
}

fn xing(header: Header, frame: &[u8]) -> Option<StreamInfo> {
    let at = header.data_start();
    let tag = frame.get(at..at + 4)?;
    if tag != b"Xing" && tag != b"Info" {
        return None;
    }
    let flags = be32(frame, at + 4)?;
    let mut field = at + 8;
    let mut info = StreamInfo::default();
    if flags & 1 != 0 {
        info.frames = Some(u64::from(be32(frame, field)?));
        field += 4;
    }
    for (flag, len) in [(2, 4), (4, 100), (8, 4)] {
        if flags & flag != 0 {
            field += len;
        }
    }
    // A tag whose fields run past its frame is no tag, and the frame is
    // audio.
    if field > frame.len() {
        return None;
    }
    info.gapless = lame(frame, field);
    Some(info)
}

/// LAME's encoder delay and padding, where its extension is present and its
/// own CRC vouches for it.
fn lame(frame: &[u8], at: usize) -> Option<(u64, u64)> {
    let tag = frame.get(at..at + LAME_LEN)?;
    if !tag.starts_with(b"LAME") && !tag.starts_with(b"Lavc") && !tag.starts_with(b"Lavf") {
        return None;
    }
    let stated = u16::from_be_bytes([tag[34], tag[35]]);
    if crc16_reflected(0, &frame[..at + LAME_LEN - 2]) != stated {
        return None;
    }
    let delay = u64::from(tag[21]) << 4 | u64::from(tag[22] >> 4);
    let padding = u64::from(tag[22] & 0xF) << 8 | u64::from(tag[23]);
    Some((delay, padding))
}

fn vbri(frame: &[u8]) -> Option<StreamInfo> {
    let at = HEADER_LEN + 32;
    if frame.get(at..at + 4)? != b"VBRI" {
        return None;
    }
    Some(StreamInfo {
        frames: Some(u64::from(be32(frame, at + 14)?)),
        gapless: None,
    })
}
