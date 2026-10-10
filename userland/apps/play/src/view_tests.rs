use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::audio::AudioGain;
use tairix_abi::driver::audio::{ChannelMap, Rate, SampleFormat};
use tairix_curses::{Event, Size, Window};
use tairix_sound::{Encoding, SoundFormat, SoundInfo};

use super::{draw, key, Key};
use tairix_player::{Control, EntryId, Extent, List, Passes, Status, Transport};

fn files() -> List {
    let paths: Vec<String> = ["intro.wav", "song.wav", "outro.au"]
        .iter()
        .map(ToString::to_string)
        .collect();
    List::new(paths, Extent::default(), Passes::ONCE)
}

fn playing() -> Status {
    let info = SoundInfo {
        format: SoundFormat::Wav,
        encoding: Encoding::Linear { bits: 16 },
        rate: Rate::HZ_48000,
        channels: ChannelMap::STEREO,
        sample: SampleFormat::S16,
        frames: Some(48_000 * 200),
        seekable: true,
        data_length: None,
    };
    let mut peaks = [0u8; 8];
    peaks[0] = 255;
    Status {
        heard: Some((EntryId::new(1), info)),
        position: 48_000 * 83,
        transport: Transport::Playing,
        gain: AudioGain::new(-600).expect("attenuation"),
        peaks: (peaks, 2),
        ..Status::new(AudioGain::UNITY)
    }
}

fn rows(window: &Window) -> Vec<String> {
    (0..window.size().rows)
        .map(|row| {
            let cells = window.buffer().row(row).unwrap_or(&[]);
            cells
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn the_interface_shows_what_is_heard_where_and_how_loud() {
    let window = draw(&files(), &playing(), None, Size::new(16, 80));
    let rows = rows(&window);
    assert!(rows[0].starts_with(" play  2 of 3  pass 1"), "{rows:?}");
    assert!(rows[1].starts_with(" Playing  1:23 / 3:20  ["), "{rows:?}");
    assert_eq!(rows[2], " song.wav");
    assert_eq!(rows[3], " WAV, 16-bit PCM, 48000 Hz, stereo, 3:20");
    assert!(rows[4].starts_with(" Level -6.00 dB"), "{rows:?}");
    assert!(
        rows[4].contains(" L[#") && rows[4].contains(" R[ "),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row == " >   2  song.wav"), "{rows:?}");
    assert!(rows[15].contains("q quit"), "{rows:?}");
}

#[test]
fn a_notice_takes_the_line_above_the_keys() {
    let window = draw(
        &files(),
        &playing(),
        Some("outro.au was not played"),
        Size::new(16, 80),
    );
    assert_eq!(rows(&window)[14], " outro.au was not played");
}

/// A terminal narrower than the bars, or shorter than the list, is drawn
/// what fits rather than refused.
#[test]
fn a_small_terminal_is_drawn_what_fits() {
    for size in [Size::new(3, 10), Size::new(8, 20), Size::new(1, 1)] {
        let window = draw(&files(), &playing(), Some("x"), size);
        assert_eq!(window.size(), size);
    }
}

#[test]
fn before_anything_is_heard_the_list_is_shown() {
    let window = draw(
        &files(),
        &Status::new(AudioGain::UNITY),
        None,
        Size::new(12, 60),
    );
    let rows = rows(&window);
    assert_eq!(rows[0], " play  3 files");
    assert_eq!(rows[1], " Starting");
    assert_eq!(rows[6], "     1  intro.wav");
}

#[test]
fn keys_are_the_ones_the_footer_names() {
    for (event, wanted) in [
        (Event::Char(' '), Key::Control(Control::TogglePause)),
        (Event::Right, Key::Control(Control::Forward)),
        (Event::Left, Key::Control(Control::Back)),
        (Event::Char('n'), Key::Control(Control::Next)),
        (Event::Char('b'), Key::Control(Control::Previous)),
        (Event::Char('+'), Key::Control(Control::Louder)),
        (Event::Char('-'), Key::Control(Control::Quieter)),
        (Event::Char('q'), Key::Control(Control::Stop)),
        (Event::Ctrl('c'), Key::Control(Control::Stop)),
        (Event::Ctrl('z'), Key::Suspend),
    ] {
        assert_eq!(key(&event), Some(wanted), "{event:?}");
    }
    assert_eq!(key(&Event::Char('x')), None);
}
