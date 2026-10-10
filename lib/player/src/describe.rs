//! How a stream reads to people.

use alloc::format;
use alloc::string::String;

use tairix_sound::SoundInfo;

/// `info`'s format, encoding, rate and channels, as people read them.
#[must_use]
pub fn describe(info: &SoundInfo) -> String {
    let channels = match info.channels.channels() {
        1 => String::from("mono"),
        2 => String::from("stereo"),
        count => format!("{count} channels"),
    };
    format!(
        "{}, {}, {} Hz, {channels}",
        info.format,
        info.encoding,
        info.rate.hz()
    )
}

#[cfg(test)]
mod tests {
    use tairix_abi::driver::audio::{ChannelMap, Rate, SampleFormat};
    use tairix_sound::{Encoding, SoundFormat, SoundInfo};

    use super::describe;

    #[test]
    fn a_stream_reads_as_its_format_encoding_rate_and_channels() {
        let mut info = SoundInfo {
            format: SoundFormat::Wav,
            encoding: Encoding::Linear { bits: 16 },
            rate: Rate::HZ_48000,
            channels: ChannelMap::STEREO,
            sample: SampleFormat::S16,
            frames: Some(48_000),
            seekable: true,
            data_length: None,
        };
        assert_eq!(describe(&info), "WAV, 16-bit PCM, 48000 Hz, stereo");
        info.channels = ChannelMap::MONO;
        assert!(describe(&info).ends_with(", mono"));
    }
}
