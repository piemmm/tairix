use alloc::string::String;

use tairix_abi::driver::audio::{ChannelMap, Rate, SampleFormat};
use tairix_sandbox::audiodecode::AudioRefusal;
use tairix_sound::{Encoding, SoundFormat, SoundInfo};

use super::{describe, left_out, naming, opened, summary};
use tairix_abi::audio::AudioGain;
use tairix_player::{Outcome, Skip, Status};

fn info(frames: Option<u64>) -> SoundInfo {
    SoundInfo {
        format: SoundFormat::Wav,
        encoding: Encoding::Linear { bits: 16 },
        rate: Rate::HZ_48000,
        channels: ChannelMap::STEREO,
        sample: SampleFormat::S16,
        frames,
        seekable: true,
        data_length: None,
    }
}

fn text(line: &[u8]) -> String {
    String::from_utf8(line.to_vec()).expect("a record is text")
}

#[test]
fn a_stream_is_described_in_one_line() {
    assert_eq!(
        describe(&info(Some(144_000))),
        "WAV, 16-bit PCM, 48000 Hz, stereo, 0:03"
    );
    assert_eq!(describe(&info(None)), "WAV, 16-bit PCM, 48000 Hz, stereo");
}

#[test]
fn the_schema_record_names_what_is_playing() {
    let line = text(&opened("a.wav", 0, &info(Some(144_000))));
    assert!(line.ends_with('\n'));
    assert!(line.contains("\"kind\":\"schema\""), "{line}");
    assert!(line.contains("\"code\":\"audio.playing\""), "{line}");
    assert!(
        line.contains(
            "\"ai\":{\"subject\":\"playback\",\"file\":\"a.wav\",\"link\":0,\"format\":\"wav\",\
             \"encoding\":\"pcm\",\"bits\":16,\"rate_hz\":48000,\"channels\":2,\"frames\":144000}"
        ),
        "{line}"
    );
    assert!(text(&opened("b.wav", 0, &info(None))).contains("\"frames\":null}"));
}

/// A chained file's later link is named by its place, counted from one for
/// people and from zero for tools.
#[test]
fn a_later_link_of_a_chain_is_named_by_its_place() {
    let line = text(&opened("radio.ogg", 2, &info(None)));
    assert!(line.contains("Playing radio.ogg (link 3): "), "{line}");
    assert!(line.contains("\"link\":2,"), "{line}");
    assert_eq!(naming("radio.ogg", 0), "radio.ogg");
}

/// A line names its file as every `name: reason` line does, quoted where a
/// shell needs it or where it holds a `:`, its hidden characters escaped.
#[test]
fn a_line_names_its_file_quoted_where_a_name_needs_it() {
    assert_eq!(naming("radio.ogg", 0), "radio.ogg");
    assert_eq!(naming("a:b.ogg", 1), "'a:b.ogg' (link 2)");
    assert_eq!(naming("e\u{1b}[2J.wav", 0), "'e'$'\\033''[2J.wav'");
    let line = text(&left_out("e\u{1b}[2J.wav", Skip::StartsPastEnd, true));
    assert!(
        line.contains("'e'$'\\\\033''[2J.wav' was not played"),
        "{line}"
    );
}

/// A file name is the user's text and may hold anything a name may.
#[test]
fn a_hostile_file_name_cannot_break_a_record() {
    let line = text(&left_out(
        "a\"},\"x\":{\"\u{1b}[2J.wav",
        Skip::StartsPastEnd,
        true,
    ));
    assert!(
        line.contains("\"file\":\"a\\\"},\\\"x\\\":{\\\"\\u001b[2J.wav\""),
        "{line}"
    );
    assert_eq!(line.matches('\n').count(), 1);
}

#[test]
fn a_file_left_out_or_cut_short_is_an_omission_with_its_reason() {
    let skipped = text(&left_out(
        "a.wav",
        Skip::Refused(AudioRefusal::WorkingSetExceeded),
        true,
    ));
    assert!(skipped.contains("\"kind\":\"omission\""), "{skipped}");
    assert!(
        skipped.contains("\"code\":\"audio.file_skipped\""),
        "{skipped}"
    );
    assert!(
        skipped.contains("\"reason\":\"decoder_refused\""),
        "{skipped}"
    );
    let cut = text(&left_out("a.wav", Skip::DecoderGaveUp, false));
    assert!(cut.contains("\"code\":\"audio.file_cut_short\""), "{cut}");
    assert!(cut.contains("\"whole_file\":false"), "{cut}");
}

#[test]
fn the_summary_counts_what_was_heard_and_what_ran_short() {
    let status = Status {
        heard_frames: 96_000,
        underruns: 1,
        lost_frames: 480,
        ..Status::new(AudioGain::UNITY)
    };
    let line = text(&summary(&status, Some(48_000), Outcome::Played));
    assert!(line.contains("\"kind\":\"summary\""), "{line}");
    assert!(line.contains("Played 0:02; 1 underrun."), "{line}");
    assert!(
        line.contains(
            "\"outcome\":\"played\",\"frames_heard\":96000,\"underruns\":1,\"lost_frames\":480"
        ),
        "{line}"
    );
}
