//! Host tests for the player's painting: a paint draws only from state, and
//! only where it was asked to.

use alloc::string::{String, ToString};
use alloc::vec;

use tairix_abi::audio::AudioGain;
use tairix_abi::driver::audio::{ChannelMap, Rate, SampleFormat};
use tairix_abi::time::Duration64;
use tairix_controls::damage;
use tairix_font::BitmapFont;
use tairix_geometry::Scale;
use tairix_player::{Status, Transport};
use tairix_raster::{Color, Pixel, Surface};
use tairix_sound::{Encoding, Metadata, SoundFormat, SoundInfo, Tag, TagKind};
use tairix_theme::{TextRole, ThemeRegistry};

use super::{paint, status_line, subtitle};
use crate::layout::Layout;
use crate::view::{Player, Saved};

const DOUBLE_CLICK: Duration64 = Duration64::from_millis(500);

fn info(seconds: u64) -> SoundInfo {
    SoundInfo {
        format: SoundFormat::Wav,
        encoding: Encoding::Linear { bits: 16 },
        rate: Rate::HZ_48000,
        channels: ChannelMap::STEREO,
        sample: SampleFormat::S16,
        frames: Some(48_000 * seconds),
        seekable: true,
        data_length: None,
    }
}

fn setting() -> (Layout, ThemeRegistry) {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, Scale::ONE);
    (
        Layout::for_window(720, 520, theme, Scale::ONE, font),
        registry,
    )
}

fn tagged(pairs: &[(TagKind, &str)]) -> Metadata {
    let mut metadata = Metadata::default();
    for (kind, value) in pairs {
        metadata.tags.push(Tag {
            kind: *kind,
            value: (*value).to_string(),
        });
    }
    metadata
}

#[test]
fn a_paint_scoped_to_one_part_changes_no_pixel_outside_it() {
    let (layout, registry) = setting();
    let theme = registry.active();
    let mut player = Player::new(Saved::default(), 1, DOUBLE_CLICK);
    let (added, _) = player.add(
        vec![String::from("a.wav"), String::from("b.wav")],
        &layout,
        &mut damage::sink(),
    );
    let mut levels = [0u8; 8];
    levels[0] = 180;
    player.adopt(
        Status {
            heard: Some((added[0], info(60))),
            position: 48_000 * 5,
            transport: Transport::Playing,
            peaks: (levels, 2),
            ..Status::new(AudioGain::UNITY)
        },
        &layout,
        &mut damage::sink(),
    );
    let window = layout.window();
    let mut whole = Surface::new(window.width, window.height).expect("a surface");
    paint(
        &mut whole,
        &player,
        &layout,
        None,
        Scale::ONE,
        theme,
        window,
    );
    let before = whole.clone();
    let meters = layout.meters();
    levels[0] = 20;
    player.adopt(
        Status {
            heard: Some((added[0], info(60))),
            position: 48_000 * 5,
            transport: Transport::Playing,
            peaks: (levels, 2),
            ..Status::new(AudioGain::UNITY)
        },
        &layout,
        &mut damage::sink(),
    );
    paint(
        &mut whole,
        &player,
        &layout,
        None,
        Scale::ONE,
        theme,
        meters,
    );
    let mut changed_inside = false;
    for y in 0..window.height {
        for x in 0..window.width {
            let differs = whole.get(x, y) != before.get(x, y);
            let inside = meters.contains(tairix_geometry::Point::new(
                i32::try_from(x).expect("small"),
                i32::try_from(y).expect("small"),
            ));
            assert!(!differs || inside, "({x}, {y}) changed outside the meters");
            changed_inside |= differs && inside;
        }
    }
    assert!(changed_inside, "the meters moved");
}

#[test]
fn a_cover_that_has_arrived_is_drawn_where_the_art_goes() {
    let (layout, registry) = setting();
    let theme = registry.active();
    let mut player = Player::new(Saved::default(), 1, DOUBLE_CLICK);
    let (added, _) = player.add(vec![String::from("a.wav")], &layout, &mut damage::sink());
    player.adopt(
        Status {
            heard: Some((added[0], info(60))),
            transport: Transport::Playing,
            ..Status::new(AudioGain::UNITY)
        },
        &layout,
        &mut damage::sink(),
    );
    let art = layout.art();
    let mut cover = Surface::new(art.width, art.height).expect("a cover");
    let red = Color::rgb(200, 10, 10);
    cover.fill_rect(0, 0, art.width, art.height, red);
    let window = layout.window();
    let mut surface = Surface::new(window.width, window.height).expect("a surface");
    paint(
        &mut surface,
        &player,
        &layout,
        Some(&cover),
        Scale::ONE,
        theme,
        window,
    );
    let middle = (
        u32::try_from(art.left()).expect("on screen") + art.width / 2,
        u32::try_from(art.top()).expect("on screen") + art.height / 2,
    );
    assert_eq!(
        surface.get(middle.0, middle.1),
        Some(Pixel {
            r: 200,
            g: 10,
            b: 10,
            a: 255
        })
    );
}

#[test]
fn the_status_line_states_the_lists_length_or_how_to_fill_it() {
    let (layout, _registry) = setting();
    let mut player = Player::new(Saved::default(), 1, DOUBLE_CLICK);
    assert!(status_line(&player).contains("Ctrl+O"));
    let (added, _) = player.add(
        vec![String::from("a.wav"), String::from("b.wav")],
        &layout,
        &mut damage::sink(),
    );
    for (entry, seconds) in added.iter().zip([90, 150]) {
        player.probed(
            *entry,
            Some((info(seconds), &Metadata::default())),
            &layout,
            &mut damage::sink(),
        );
    }
    assert_eq!(status_line(&player), "2 tracks, 4:00");
    player.notify(
        String::from("2 files left out"),
        &layout,
        &mut damage::sink(),
    );
    assert_eq!(status_line(&player), "2 files left out");
}

#[test]
fn a_tracks_second_line_names_what_its_tags_name() {
    let (layout, _registry) = setting();
    let mut player = Player::new(Saved::default(), 1, DOUBLE_CLICK);
    let (added, _) = player.add(
        vec![String::from("a.wav"), String::from("b.wav")],
        &layout,
        &mut damage::sink(),
    );
    let both = tagged(&[(TagKind::Artist, "Holst"), (TagKind::Album, "The Planets")]);
    player.probed(
        added[0],
        Some((info(60), &both)),
        &layout,
        &mut damage::sink(),
    );
    let row = player.playlist().get(added[0]).expect("a row");
    assert_eq!(subtitle(row), "Holst \u{2014} The Planets");
    let untagged = player.playlist().get(added[1]).expect("a row");
    assert_eq!(
        subtitle(untagged),
        "b.wav",
        "the file's own name until it is read"
    );
}
