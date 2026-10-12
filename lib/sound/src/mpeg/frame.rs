//! The header every MPEG audio frame opens with (ISO/IEC 11172-3 §2.4.2.3,
//! extended to the lower rates by ISO/IEC 13818-3 and MPEG-2.5).

use super::tables::{LSF_BITRATES, MPEG1_BITRATES, SAMPLE_RATES};

/// Bytes of a header.
pub(super) const HEADER_LEN: usize = 4;

/// Bytes of the CRC a protected frame carries after its header.
pub(super) const CRC_LEN: usize = 2;

/// The longest frame any layer's bitrate and rate make: Layer II at
/// 160 kbit/s and 8 kHz, padded. A free-format frame is held to it too.
pub(super) const MAX_FRAME_LEN: usize = 2881;

/// Which of the format's three families the frame belongs to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Version {
    Mpeg1,
    Mpeg2,
    Mpeg25,
}

/// How the frame codes its samples.
#[allow(
    clippy::upper_case_acronyms,
    reason = "the standard numbers its layers in Roman numerals"
)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Layer {
    I,
    II,
    III,
}

/// How the frame's channels are coded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Mode {
    Stereo,
    JointStereo,
    DualChannel,
    Mono,
}

/// One frame's header.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Header {
    pub(super) version: Version,
    pub(super) layer: Layer,
    /// Whether a CRC follows the header.
    pub(super) protected: bool,
    /// Zero for free format.
    pub(super) bitrate_index: u8,
    pub(super) rate_index: u8,
    pub(super) padding: bool,
    pub(super) mode: Mode,
    pub(super) mode_extension: u8,
}

impl Header {
    /// The header `bytes` opens with, or `None` where they spell none the
    /// format allows: no sync, a reserved version, layer or rate, the
    /// forbidden bitrate, or the reserved emphasis.
    pub(super) fn parse(bytes: &[u8]) -> Option<Self> {
        let &[a, b, c, d, ..] = bytes else {
            return None;
        };
        if a != 0xFF || b & 0xE0 != 0xE0 {
            return None;
        }
        let version = match (b >> 3) & 3 {
            0 => Version::Mpeg25,
            2 => Version::Mpeg2,
            3 => Version::Mpeg1,
            _ => return None,
        };
        let layer = match (b >> 1) & 3 {
            1 => Layer::III,
            2 => Layer::II,
            3 => Layer::I,
            _ => return None,
        };
        let bitrate_index = c >> 4;
        let rate_index = (c >> 2) & 3;
        if bitrate_index == 15 || rate_index == 3 || d & 3 == 2 {
            return None;
        }
        let mode = match d >> 6 {
            0 => Mode::Stereo,
            1 => Mode::JointStereo,
            2 => Mode::DualChannel,
            _ => Mode::Mono,
        };
        Some(Self {
            version,
            layer,
            protected: b & 1 == 0,
            bitrate_index,
            rate_index,
            padding: (c >> 1) & 1 == 1,
            mode,
            mode_extension: (d >> 4) & 3,
        })
    }

    /// Whether `other` can follow this header in one stream: the same
    /// family, layer and rate, and free format on both or on neither. The
    /// channel mode may change from frame to frame.
    pub(super) fn continues(self, other: Self) -> bool {
        self.version == other.version
            && self.layer == other.layer
            && self.rate_index == other.rate_index
            && (self.bitrate_index == 0) == (other.bitrate_index == 0)
    }

    /// Whether the frame is at one of the lower rates.
    pub(super) fn lsf(self) -> bool {
        self.version != Version::Mpeg1
    }

    /// Channels the frame codes.
    pub(super) fn channels(self) -> u8 {
        if self.mode == Mode::Mono {
            1
        } else {
            2
        }
    }

    /// Frames a second.
    pub(super) fn rate(self) -> u32 {
        let family = match self.version {
            Version::Mpeg1 => 0,
            Version::Mpeg2 => 1,
            Version::Mpeg25 => 2,
        };
        SAMPLE_RATES[family][usize::from(self.rate_index)]
    }

    /// Samples the frame holds per channel.
    pub(super) fn samples(self) -> u32 {
        match (self.layer, self.lsf()) {
            (Layer::I, _) => 384,
            (Layer::III, true) => 576,
            _ => 1152,
        }
    }

    /// The stated bitrate in bit/s; `None` for free format.
    pub(super) fn bitrate(self) -> Option<u32> {
        if self.bitrate_index == 0 {
            return None;
        }
        let at = usize::from(self.bitrate_index);
        let kbps = match (self.lsf(), self.layer) {
            (false, Layer::I) => MPEG1_BITRATES[0][at],
            (false, Layer::II) => MPEG1_BITRATES[1][at],
            (false, Layer::III) => MPEG1_BITRATES[2][at],
            (true, Layer::I) => LSF_BITRATES[0][at],
            (true, _) => LSF_BITRATES[1][at],
        };
        Some(u32::from(kbps) * 1000)
    }

    /// Bytes per slot, and slots a frame of one bit/s takes per second of
    /// rate: the frame length is `per_bit × bitrate / rate` slots, padded by
    /// one.
    fn slots(self) -> (usize, u32) {
        match (self.layer, self.lsf()) {
            (Layer::I, _) => (4, 12),
            (Layer::III, true) => (1, 72),
            _ => (1, 144),
        }
    }

    /// Bytes the frame occupies, header included, at `bitrate` bit/s.
    pub(super) fn len_at(self, bitrate: u32) -> usize {
        let (slot, per_bit) = self.slots();
        let unpadded = u64::from(per_bit) * u64::from(bitrate) / u64::from(self.rate());
        (usize::try_from(unpadded).unwrap_or(usize::MAX) + usize::from(self.padding))
            .saturating_mul(slot)
    }

    /// The bitrate a free-format frame of `len` bytes, its own padding
    /// included, implies.
    pub(super) fn bitrate_of(self, len: usize) -> u32 {
        let (slot, per_bit) = self.slots();
        let unpadded = (len / slot).saturating_sub(usize::from(self.padding));
        let bits = u64::try_from(unpadded).unwrap_or(u64::MAX) * u64::from(self.rate());
        u32::try_from(bits / u64::from(per_bit)).unwrap_or(u32::MAX)
    }

    /// Bytes of Layer III side information after the header and any CRC.
    pub(super) fn side_info_len(self) -> usize {
        match (self.lsf(), self.mode == Mode::Mono) {
            (false, false) => 32,
            (false, true) | (true, false) => 17,
            (true, true) => 9,
        }
    }

    /// Bytes from the frame's start to its audio data: the header, the CRC
    /// where one is carried, and Layer III's side information.
    pub(super) fn data_start(self) -> usize {
        let crc = if self.protected { CRC_LEN } else { 0 };
        let side = if self.layer == Layer::III {
            self.side_info_len()
        } else {
            0
        };
        HEADER_LEN + crc + side
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(bytes: [u8; 4]) -> Header {
        Header::parse(&bytes).expect("a header")
    }

    #[test]
    fn a_header_states_its_frame_and_refuses_what_the_format_forbids() {
        // MPEG-1 Layer III, 128 kbit/s, 44.1 kHz, padded, joint stereo.
        let mp3 = header([0xFF, 0xFB, 0x92, 0x40]);
        assert_eq!(mp3.version, Version::Mpeg1);
        assert_eq!(mp3.layer, Layer::III);
        assert!(!mp3.protected);
        assert_eq!(mp3.mode, Mode::JointStereo);
        assert_eq!(mp3.bitrate(), Some(128_000));
        assert_eq!(mp3.rate(), 44_100);
        assert_eq!(mp3.len_at(128_000), 418);
        assert_eq!(mp3.samples(), 1152);
        assert_eq!(mp3.data_start(), 36);
        for forbidden in [
            [0xFF, 0xEB, 0x90, 0x00], // reserved version
            [0xFF, 0xF9, 0x90, 0x00], // reserved layer
            [0xFF, 0xFB, 0xF0, 0x00], // bitrate index 15
            [0xFF, 0xFB, 0x9C, 0x00], // reserved rate
            [0xFF, 0xFB, 0x90, 0x02], // reserved emphasis
            [0xFF, 0x7B, 0x90, 0x00], // no sync
        ] {
            assert_eq!(Header::parse(&forbidden), None, "{forbidden:02x?}");
        }
    }

    #[test]
    fn each_layer_and_family_lays_its_frame_out_by_its_own_rule() {
        // Layer I, MPEG-1, 32 kbit/s at 32 kHz: 12 slots of four bytes.
        assert_eq!(header([0xFF, 0xFF, 0x18, 0xC0]).len_at(32_000), 48);
        // Layer II, MPEG-2, 160 kbit/s at 16 kHz, padded: 1441 bytes.
        let layer2 = header([0xFF, 0xF5, 0xEA, 0xC0]);
        assert_eq!(layer2.len_at(160_000), 1441);
        assert_eq!(layer2.samples(), 1152);
        // Layer III, MPEG-2.5, 8 kbit/s at 8 kHz: 72 bytes, 576 samples.
        let low = header([0xFF, 0xE3, 0x18, 0xC0]);
        assert_eq!(low.rate(), 8_000);
        assert_eq!(low.len_at(8_000), 72);
        assert_eq!(low.samples(), 576);
        assert_eq!(low.side_info_len(), 9);
    }

    #[test]
    fn a_free_format_frame_states_its_bitrate_by_its_length() {
        // 48 kHz, where 640 kbit/s divides into whole bytes.
        let free = header([0xFF, 0xFB, 0x04, 0x00]);
        assert_eq!(free.bitrate(), None);
        assert_eq!(free.len_at(640_000), 1920);
        assert_eq!(free.bitrate_of(1920), 640_000);
    }
}
