//! The `InfoFrames` a source sends a sink (CTA-861-H §6; HDMI 1.4b §8.2.3),
//! packed as a transmitter's packet memory takes them: three header bytes,
//! the checksum, then the payload.

use crate::cta::hdmi_aspect;
use crate::{Aspect, VideoCode};

/// The longest packed `InfoFrame`: its header, checksum and payload.
pub const INFOFRAME_MAX: usize = 31;

/// A packed `InfoFrame`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Packed {
    bytes: [u8; INFOFRAME_MAX],
    len: usize,
}

impl Packed {
    /// The frame's type, version and payload, its checksum making every byte
    /// sum to zero.
    fn new<const N: usize>(kind: u8, version: u8, payload: &[u8; N]) -> Self {
        const { assert!(4 + N <= INFOFRAME_MAX, "a payload fits the frame") };
        let mut bytes = [0; INFOFRAME_MAX];
        let len = 4 + N;
        bytes[..3].copy_from_slice(&[kind, version, u8::try_from(N).unwrap_or(u8::MAX)]);
        bytes[4..len].copy_from_slice(payload);
        let sum = bytes[..len]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
        bytes[3] = 0u8.wrapping_sub(sum);
        Self { bytes, len }
    }

    /// The type the header names.
    #[must_use]
    pub const fn kind(&self) -> u8 {
        self.bytes[0]
    }

    /// Header, checksum and payload.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// The RGB range a source states it sends.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Quantization {
    /// Unstated: the range the timing's default is, the only answer a sink
    /// that cannot select one takes.
    Default,
    /// 16 to 235.
    Limited,
    /// 0 to 255.
    Full,
}

/// The auxiliary video information a source sends with RGB pictures.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Avi {
    /// The code naming the timing, when one does.
    pub code: Option<VideoCode>,
    /// The RGB range sent.
    pub quantization: Quantization,
}

const AVI_TYPE: u8 = 0x82;
const VENDOR_TYPE: u8 = 0x81;
const AUDIO_TYPE: u8 = 0x84;
/// The HDMI Licensing IEEE OUI, least significant byte first.
const HDMI_OUI: [u8; 3] = [0x03, 0x0C, 0x00];
/// The vendor-specific `InfoFrame`'s format: an HDMI 1.4 extended
/// resolution, its code following.
const HDMI_EXTENDED_RESOLUTION: u8 = 1 << 5;
/// Active format information present, the same as the picture's.
const ACTIVE_FORMAT_PRESENT: u8 = 1 << 4;
const ACTIVE_ASPECT_PICTURE: u8 = 8;

impl Avi {
    /// The AVI `InfoFrame`: version 3 for a code past version 2's seven bits.
    /// A timing named by an HDMI code carries none here, only its aspect.
    #[must_use]
    pub fn pack(&self) -> Packed {
        let (vic, aspect) = match self.code {
            Some(VideoCode::Cta { vic, aspect }) => (vic, aspect_bits(aspect)),
            Some(VideoCode::Hdmi(vic)) => (0, hdmi_aspect(vic).map_or(0, aspect_bits)),
            None => (0, 0),
        };
        let quantization = match self.quantization {
            Quantization::Default => 0,
            Quantization::Limited => 1,
            Quantization::Full => 2,
        };
        let mut payload = [0u8; 13];
        payload[0] = ACTIVE_FORMAT_PRESENT;
        payload[1] = aspect << 4 | ACTIVE_ASPECT_PICTURE;
        payload[2] = quantization << 2;
        payload[3] = vic;
        Packed::new(AVI_TYPE, if vic > 127 { 3 } else { 2 }, &payload)
    }
}

/// The audio `InfoFrame` for linear PCM: the channels each sample carries and
/// the speakers they play on, the rate and sample size left to the samples'
/// own channel status, as CTA-861 asks of a source.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AudioInfo {
    /// The channels each sample carries, two to eight.
    pub channels: u8,
    /// The CTA-861 speaker allocation they are placed by.
    pub allocation: u8,
}

impl AudioInfo {
    /// The `InfoFrame` type the frame carries.
    pub const KIND: u8 = AUDIO_TYPE;

    /// The audio `InfoFrame`, version 1.
    #[must_use]
    pub fn pack(&self) -> Packed {
        let mut payload = [0u8; 10];
        payload[0] = self.channels.saturating_sub(1) & 0x7;
        payload[3] = self.allocation;
        Packed::new(AUDIO_TYPE, 1, &payload)
    }
}

/// The picture aspect field: version 2 names 4:3 and 16:9; wider ones are
/// the code's alone.
const fn aspect_bits(aspect: Aspect) -> u8 {
    match aspect {
        Aspect::FourThree => 1,
        Aspect::SixteenNine => 2,
        Aspect::SixtyFourTwentySeven | Aspect::TwoFiftySixOneThirtyFive => 0,
    }
}

/// The HDMI vendor-specific `InfoFrame` naming an HDMI 1.4 video code, which
/// a timing listed by one is sent with, its AVI code left zero.
#[must_use]
pub fn hdmi_vendor(code: VideoCode) -> Option<Packed> {
    let VideoCode::Hdmi(vic) = code else {
        return None;
    };
    let payload = [
        HDMI_OUI[0],
        HDMI_OUI[1],
        HDMI_OUI[2],
        HDMI_EXTENDED_RESOLUTION,
        vic,
    ];
    Some(Packed::new(VENDOR_TYPE, 1, &payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sums_to_zero(frame: &Packed) -> bool {
        frame
            .as_bytes()
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
            == 0
    }

    #[test]
    fn an_avi_frame_names_the_code_its_aspect_and_its_range() {
        let frame = Avi {
            code: Some(VideoCode::Cta {
                vic: 16,
                aspect: Aspect::SixteenNine,
            }),
            quantization: Quantization::Default,
        }
        .pack();
        assert_eq!(&frame.as_bytes()[..3], [0x82, 2, 13]);
        assert_eq!(frame.as_bytes()[4..9], [0x10, 0x28, 0, 16, 0]);
        assert!(sums_to_zero(&frame));
        let limited = Avi {
            code: None,
            quantization: Quantization::Limited,
        }
        .pack();
        assert_eq!(limited.as_bytes()[6], 1 << 2);
        assert_eq!(limited.as_bytes()[7], 0, "no code");
    }

    #[test]
    fn a_code_past_seven_bits_takes_version_three() {
        let frame = Avi {
            code: Some(VideoCode::Cta {
                vic: 200,
                aspect: Aspect::SixtyFourTwentySeven,
            }),
            quantization: Quantization::Full,
        }
        .pack();
        assert_eq!(frame.as_bytes()[1], 3);
        assert_eq!(
            frame.as_bytes()[5],
            ACTIVE_ASPECT_PICTURE,
            "aspect the code's"
        );
        assert!(sums_to_zero(&frame));
    }

    #[test]
    fn an_hdmi_code_is_sent_in_the_vendor_frame() {
        let frame = hdmi_vendor(VideoCode::Hdmi(1)).expect("an HDMI code");
        assert_eq!(frame.as_bytes(), {
            let mut bytes = [0x81, 1, 5, 0, 0x03, 0x0C, 0x00, 0x20, 1];
            bytes[3] = 0u8.wrapping_sub(bytes.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)));
            bytes
        });
        assert!(hdmi_vendor(VideoCode::Cta {
            vic: 1,
            aspect: Aspect::FourThree
        })
        .is_none());
    }

    #[test]
    fn an_audio_frame_states_the_channels_and_their_placement_and_nothing_else() {
        let frame = AudioInfo {
            channels: 6,
            allocation: 0x0B,
        }
        .pack();
        assert_eq!(&frame.as_bytes()[..3], [0x84, 1, 10]);
        assert_eq!(frame.as_bytes()[4..], [5, 0, 0, 0x0B, 0, 0, 0, 0, 0, 0]);
        assert!(sums_to_zero(&frame));
    }

    #[test]
    fn an_avi_frame_beside_an_hdmi_code_carries_its_aspect_but_no_code() {
        let uhd = Avi {
            code: Some(VideoCode::Hdmi(1)),
            quantization: Quantization::Limited,
        }
        .pack();
        assert_eq!(uhd.as_bytes()[5], 2 << 4 | ACTIVE_ASPECT_PICTURE, "16:9");
        assert_eq!(uhd.as_bytes()[7], 0);
        let cinema = Avi {
            code: Some(VideoCode::Hdmi(4)),
            quantization: Quantization::Limited,
        }
        .pack();
        assert_eq!(
            cinema.as_bytes()[5],
            ACTIVE_ASPECT_PICTURE,
            "256:135 is the code's alone"
        );
    }
}
