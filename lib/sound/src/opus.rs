//! Opus (RFC 6716, with the RFC 8251 corrections): packets of one to
//! forty-eight frames, each coded by SILK, CELT or both, decoded at 48 kHz.
//!
//! The codec knows packets and nothing of the container they arrive in.

mod celt;
mod decoder;
mod multistream;
mod range;
mod silk;

pub(crate) use self::multistream::{Layout, Multistream, SILENT};

use crate::DecodeError;

/// The rate Opus is decoded at.
pub(crate) const RATE: u32 = 48_000;

/// The most samples a channel one packet holds: 120 ms.
pub(crate) const MAX_PACKET_SAMPLES: usize = 5760;

/// The most bytes one frame may take.
const MAX_FRAME_BYTES: usize = 1275;

/// The most frames one packet may hold: 120 ms of 2.5 ms frames.
const MAX_FRAMES: usize = 48;

/// Which layers code a frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mode {
    Silk,
    Hybrid,
    Celt,
}

/// The audio bandwidth a frame codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Bandwidth {
    Narrow,
    Medium,
    Wide,
    SuperWide,
    Full,
}

impl Bandwidth {
    /// The CELT band the bandwidth ends before.
    pub(crate) const fn end_band(self) -> usize {
        match self {
            Self::Narrow => 13,
            Self::Medium | Self::Wide => 17,
            Self::SuperWide => 19,
            Self::Full => 21,
        }
    }
}

/// A packet's table-of-contents byte (RFC 6716, section 3.1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Toc {
    pub(crate) mode: Mode,
    pub(crate) bandwidth: Bandwidth,
    /// Each frame's samples at 48 kHz.
    pub(crate) frame_size: usize,
    pub(crate) stereo: bool,
}

impl Toc {
    pub(crate) const fn of(byte: u8) -> Self {
        let config = byte >> 3;
        let (mode, bandwidth, frame_size) = match config {
            0..=11 => {
                let bandwidth = match config >> 2 {
                    0 => Bandwidth::Narrow,
                    1 => Bandwidth::Medium,
                    _ => Bandwidth::Wide,
                };
                let size = match config & 3 {
                    0 => 480,
                    1 => 960,
                    2 => 1920,
                    _ => 2880,
                };
                (Mode::Silk, bandwidth, size)
            }
            12..=15 => {
                let bandwidth = if config < 14 {
                    Bandwidth::SuperWide
                } else {
                    Bandwidth::Full
                };
                (
                    Mode::Hybrid,
                    bandwidth,
                    if config & 1 == 0 { 480 } else { 960 },
                )
            }
            _ => {
                let bandwidth = match (config - 16) >> 2 {
                    0 => Bandwidth::Narrow,
                    1 => Bandwidth::Wide,
                    2 => Bandwidth::SuperWide,
                    _ => Bandwidth::Full,
                };
                (Mode::Celt, bandwidth, 120 << (config & 3))
            }
        };
        Self {
            mode,
            bandwidth,
            frame_size,
            stereo: byte & 0x04 != 0,
        }
    }
}

/// A packet split into its frames.
pub(crate) struct Packet<'p> {
    pub(crate) toc: Toc,
    frames: [&'p [u8]; MAX_FRAMES],
    count: usize,
    /// The bytes the packet took, padding included: a self-delimited packet
    /// is followed by another's.
    pub(crate) len: usize,
}

/// The length a frame states in one or two bytes, and the bytes it took.
fn frame_length(bytes: &[u8]) -> Option<(usize, usize)> {
    match *bytes {
        [first, ..] if first < 252 => Some((usize::from(first), 1)),
        [first, second, ..] => Some((4 * usize::from(second) + usize::from(first), 2)),
        _ => None,
    }
}

impl<'p> Packet<'p> {
    /// Split `bytes` into its frames (RFC 6716, section 3.2, and appendix B
    /// for the self-delimited form a multistream packet's leading streams
    /// take), holding it to every requirement section 3.4 states.
    #[allow(
        clippy::too_many_lines,
        reason = "the reference's opus_packet_parse_impl, kept whole so the port can be read against it"
    )]
    pub(crate) fn parse(bytes: &'p [u8], self_delimited: bool) -> Result<Self, DecodeError> {
        let invalid = DecodeError::OpusPacketInvalid;
        let (&toc_byte, mut rest) = bytes.split_first().ok_or(invalid)?;
        let toc = Toc::of(toc_byte);
        let mut sizes = [0usize; MAX_FRAMES];
        let mut padding = 0;
        let count;
        let mut cbr = false;
        let mut last = rest.len();
        match toc_byte & 3 {
            0 => count = 1,
            1 => {
                count = 2;
                cbr = true;
                if !self_delimited {
                    if rest.len() % 2 != 0 {
                        return Err(invalid);
                    }
                    last = rest.len() / 2;
                    sizes[0] = last;
                }
            }
            2 => {
                count = 2;
                let (size, taken) = frame_length(rest).ok_or(invalid)?;
                rest = &rest[taken..];
                if size > rest.len() {
                    return Err(invalid);
                }
                sizes[0] = size;
                last = rest.len() - size;
            }
            _ => {
                let (&frame_count, after) = rest.split_first().ok_or(invalid)?;
                rest = after;
                count = usize::from(frame_count & 0x3F);
                if count == 0 || toc.frame_size * count > MAX_PACKET_SAMPLES {
                    return Err(invalid);
                }
                if frame_count & 0x40 != 0 {
                    // Cut from the window as declared, so no length or frame
                    // is read out of the padding.
                    loop {
                        let (&step, after) = rest.split_first().ok_or(invalid)?;
                        let pad = if step == 255 { 254 } else { usize::from(step) };
                        rest = after
                            .get(..after.len().checked_sub(pad).ok_or(invalid)?)
                            .ok_or(invalid)?;
                        padding += pad;
                        if step != 255 {
                            break;
                        }
                    }
                }
                cbr = frame_count & 0x80 == 0;
                if !cbr {
                    last = rest.len();
                    for size in &mut sizes[..count - 1] {
                        let (frame, taken) = frame_length(rest).ok_or(invalid)?;
                        rest = &rest[taken..];
                        if frame > rest.len() {
                            return Err(invalid);
                        }
                        *size = frame;
                        last = last.checked_sub(taken + frame).ok_or(invalid)?;
                    }
                } else if !self_delimited {
                    last = rest.len() / count;
                    if last * count != rest.len() {
                        return Err(invalid);
                    }
                    sizes[..count - 1].fill(last);
                }
            }
        }
        if self_delimited {
            let (size, taken) = frame_length(rest).ok_or(invalid)?;
            rest = &rest[taken..];
            if size > rest.len() {
                return Err(invalid);
            }
            if cbr {
                if size * count > rest.len() {
                    return Err(invalid);
                }
                sizes[..count].fill(size);
            } else {
                if taken + size > last {
                    return Err(invalid);
                }
                sizes[count - 1] = size;
            }
        } else {
            if last > MAX_FRAME_BYTES {
                return Err(invalid);
            }
            sizes[count - 1] = last;
        }
        let header = bytes.len() - padding - rest.len();
        let mut frames: [&[u8]; MAX_FRAMES] = [&[]; MAX_FRAMES];
        for (frame, &size) in frames.iter_mut().zip(&sizes[..count]) {
            let (this, after) = rest.split_at_checked(size).ok_or(invalid)?;
            *frame = this;
            rest = after;
        }
        let len = if self_delimited {
            header + sizes[..count].iter().sum::<usize>() + padding
        } else {
            bytes.len()
        };
        Ok(Self {
            toc,
            frames,
            count,
            len,
        })
    }

    /// The packet's frames, in order.
    pub(crate) fn frames(&self) -> &[&'p [u8]] {
        &self.frames[..self.count]
    }

    /// The samples a channel the packet decodes to.
    pub(crate) const fn samples(&self) -> usize {
        self.toc.frame_size * self.count
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec;
    use std::vec::Vec;

    use super::{Mode, Packet, MAX_FRAME_BYTES};
    use crate::DecodeError;

    /// A full-band CELT TOC of 2.5 ms frames, framed by `code`.
    const fn celt(code: u8) -> u8 {
        (28 << 3) | code
    }

    fn packet(head: &[u8], body: usize) -> Vec<u8> {
        let mut bytes = head.to_vec();
        bytes.extend((0..=u8::MAX).cycle().take(body));
        bytes
    }

    fn sizes(bytes: &[u8], self_delimited: bool) -> Result<(Vec<usize>, usize), DecodeError> {
        Packet::parse(bytes, self_delimited).map(|packet| {
            (
                packet.frames().iter().map(|frame| frame.len()).collect(),
                packet.len,
            )
        })
    }

    #[test]
    fn each_framing_code_splits_its_frames() {
        assert_eq!(sizes(&packet(&[celt(0)], 40), false), Ok((vec![40], 41)));
        assert_eq!(
            sizes(&packet(&[celt(1)], 40), false),
            Ok((vec![20, 20], 41))
        );
        assert_eq!(
            sizes(&packet(&[celt(2), 15], 40), false),
            Ok((vec![15, 25], 42))
        );
        assert_eq!(
            sizes(&packet(&[celt(2), 252, 1], 300), false),
            Ok((vec![256, 44], 303))
        );
        assert_eq!(
            sizes(&packet(&[celt(3), 3], 30), false),
            Ok((vec![10, 10, 10], 32))
        );
        assert_eq!(
            sizes(&packet(&[celt(3), 0x83, 5, 0], 30), false),
            Ok((vec![5, 0, 25], 34))
        );
        let padded = packet(&[celt(3), 0x42, 255, 3], 254 + 3 + 20);
        assert_eq!(sizes(&padded, false), Ok((vec![10, 10], padded.len())));
        assert_eq!(
            sizes(&packet(&[celt(0)], 0), false),
            Ok((vec![0], 1)),
            "a frame of nothing"
        );
        let packet = Packet::parse(&[(12 << 3) | 0x04 | 3, 2, 0, 0], false).expect("parses");
        assert_eq!(
            (packet.toc.mode, packet.toc.stereo, packet.samples()),
            (Mode::Hybrid, true, 960)
        );
    }

    #[test]
    fn a_self_delimited_packet_states_its_last_frame_and_ends_where_it_says() {
        let tail = |bytes: &mut Vec<u8>| bytes.extend_from_slice(&[0xEE; 9]);
        let mut one = packet(&[celt(0), 12], 12);
        tail(&mut one);
        assert_eq!(sizes(&one, true), Ok((vec![12], 14)));
        let mut pair = packet(&[celt(1), 7], 14);
        tail(&mut pair);
        assert_eq!(sizes(&pair, true), Ok((vec![7, 7], 16)));
        let mut uneven = packet(&[celt(2), 3, 6], 9);
        tail(&mut uneven);
        assert_eq!(sizes(&uneven, true), Ok((vec![3, 6], 12)));
        let mut cbr = packet(&[celt(3), 0x43, 2, 4], 12 + 2);
        tail(&mut cbr);
        assert_eq!(sizes(&cbr, true), Ok((vec![4, 4, 4], 4 + 12 + 2)));
        let mut vbr = packet(&[celt(3), 0xC2, 1, 5, 2], 7 + 1);
        tail(&mut vbr);
        assert_eq!(sizes(&vbr, true), Ok((vec![5, 2], 5 + 7 + 1)));
        assert_eq!(
            sizes(&packet(&[celt(0), 20], 12), true),
            Err(DecodeError::OpusPacketInvalid)
        );
        assert_eq!(
            sizes(&packet(&[celt(1), 7], 13), true),
            Err(DecodeError::OpusPacketInvalid)
        );
        assert_eq!(
            sizes(&packet(&[celt(2), 9, 6], 9), true),
            Err(DecodeError::OpusPacketInvalid)
        );
    }

    /// RFC 6716, section 3.4, R1 to R7.
    #[test]
    fn packets_breaking_the_framing_requirements_are_refused() {
        let refused = |bytes: &[u8], what: &str| {
            assert_eq!(
                sizes(bytes, false),
                Err(DecodeError::OpusPacketInvalid),
                "{what}"
            );
        };
        refused(&[], "R1: no TOC");
        assert!(sizes(&packet(&[celt(0)], MAX_FRAME_BYTES), false).is_ok());
        refused(
            &packet(&[celt(0)], MAX_FRAME_BYTES + 1),
            "R2: an implicit frame too long",
        );
        refused(
            &packet(&[celt(1)], 2 * MAX_FRAME_BYTES + 2),
            "R2: implicit frames too long",
        );
        refused(&packet(&[celt(1)], 41), "R3: frames of unequal halves");
        refused(&[celt(2)], "R4: no length");
        refused(&[celt(2), 252], "R4: half a length");
        refused(&packet(&[celt(2), 41], 40), "R4: a length past the packet");
        refused(&[celt(3)], "R5: no frame count");
        refused(&packet(&[celt(3), 0], 10), "R5: no frames");
        assert!(sizes(&packet(&[celt(3), 48], 48), false).is_ok());
        refused(&packet(&[celt(3), 49], 49), "R5: more than 120 ms");
        refused(&packet(&[(19 << 3) | 3, 7], 7), "R5: seven 20 ms frames");
        refused(&packet(&[celt(3), 3], 31), "R6: no whole frames");
        refused(
            &packet(&[celt(3), 0x41, 30], 29),
            "R6: padding past the packet",
        );
        refused(&packet(&[celt(3), 0x41], 0), "R6: padding of no length");
        refused(
            &packet(&[celt(3), 0x82, 30], 29),
            "R7: a length past the packet",
        );
        refused(
            &packet(&[celt(3), 0xC2, 5, 20], 20),
            "R7: lengths leaving no room for the padding",
        );
    }

    /// No length, padding step or frame is ever read out of a packet's
    /// padding, however the budget the padding leaves runs out.
    #[test]
    fn nothing_is_read_out_of_the_padding() {
        let mut exhausted = vec![celt(3), 0x41, 255];
        exhausted.extend_from_slice(&[255; 254]);
        assert_eq!(
            sizes(&exhausted, false),
            Err(DecodeError::OpusPacketInvalid),
            "a step past the budget"
        );
        let lengths = [celt(3), 0xC2, 3, 1, 1, 1];
        assert_eq!(
            sizes(&lengths, false),
            Err(DecodeError::OpusPacketInvalid),
            "a length in the padding"
        );
        let delimited = [celt(3), 0x41, 2, 1, 1];
        assert_eq!(
            sizes(&delimited, true),
            Err(DecodeError::OpusPacketInvalid),
            "a stated last frame in the padding"
        );
    }
}
