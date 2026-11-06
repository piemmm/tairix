use alloc::vec::Vec;

use tairix_abi::audio::AudioGain;
use tairix_appconf::{Document, Registry};

use super::{resolve, CueSource, Resolution};
use crate::{is_effect, Silence, SoundAsset, SoundEvent, SoundSettings, DEFAULT_THEME};

fn settings(text: &str) -> SoundSettings {
    let (settings, refused) = SoundSettings::load(&Document::parse(text).expect("a document"));
    assert!(refused.is_empty(), "{text}");
    settings
}

/// Which themes hold which assets in a scenario.
#[derive(Copy, Clone, Debug)]
struct Shelf {
    active: bool,
    shipped: bool,
}

const ACTIVE: &str = "Bright";

/// The whole cross-product of event × theme × switches × own file × what the
/// themes hold, against the order the plan fixes.
#[test]
fn every_cue_resolves_in_the_one_order() {
    let shelves = [
        Shelf {
            active: true,
            shipped: true,
        },
        Shelf {
            active: true,
            shipped: false,
        },
        Shelf {
            active: false,
            shipped: true,
        },
        Shelf {
            active: false,
            shipped: false,
        },
    ];
    for event in SoundEvent::ALL {
        for theme in [ACTIVE, DEFAULT_THEME] {
            for master in [true, false] {
                for own_switch in [true, false] {
                    let text = alloc::format!(
                        "sound.enabled = {master}\nsound.theme = {theme}\nsound.gain = -3dB\n\
                         sound.{}.enabled = {own_switch}\n",
                        event.name()
                    );
                    let settings = settings(&text);
                    for own in [true, false] {
                        for shelf in shelves {
                            let mut asked = Vec::new();
                            let decided = resolve(event, &settings, own, |held, asset| {
                                assert_eq!(asset, SoundAsset::of(event));
                                asked.push(alloc::string::String::from(held));
                                if held == ACTIVE {
                                    shelf.active
                                } else {
                                    shelf.shipped
                                }
                            });
                            let effect = is_effect(event);
                            let gain = if effect {
                                AudioGain::new(-300).expect("attenuation")
                            } else {
                                AudioGain::UNITY
                            };
                            let from = |theme| Resolution::Play {
                                source: CueSource::Theme {
                                    theme,
                                    asset: SoundAsset::of(event),
                                },
                                gain,
                            };
                            let expected = if (effect && !master) || !own_switch {
                                Resolution::Silent(Silence::Disabled)
                            } else if own {
                                Resolution::Play {
                                    source: CueSource::Own,
                                    gain,
                                }
                            } else if theme == ACTIVE && shelf.active {
                                from(ACTIVE)
                            } else if shelf.shipped {
                                from(DEFAULT_THEME)
                            } else {
                                Resolution::Silent(Silence::NoAsset)
                            };
                            assert_eq!(
                                decided, expected,
                                "{event:?} {theme} master {master} switch {own_switch} \
                                 own {own} {shelf:?}"
                            );
                            assert!(asked.len() <= 2, "each theme is asked once");
                            if asked.len() == 2 {
                                assert_eq!(asked, [ACTIVE, DEFAULT_THEME]);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Nothing stored plays every event from the shipped theme at unity, which is
/// what keeps the shipped cue bit-exact.
#[test]
fn nothing_stored_plays_the_shipped_asset_at_unity() {
    let settings = SoundSettings::default();
    for event in SoundEvent::ALL {
        assert_eq!(
            resolve(event, &settings, false, |theme, _| theme == DEFAULT_THEME),
            Resolution::Play {
                source: CueSource::Theme {
                    theme: DEFAULT_THEME,
                    asset: SoundAsset::of(event),
                },
                gain: AudioGain::UNITY,
            }
        );
    }
}

/// What the volume's feedback resolves to under `settings`, with the shipped
/// theme holding every asset.
fn feedback(settings: &SoundSettings) -> Resolution<'_> {
    resolve(SoundEvent::VolumeFeedback, settings, false, |theme, _| {
        theme == DEFAULT_THEME
    })
}

/// The volume's feedback tells the listener how loud the output now is, so
/// neither the effects' switch nor their gain reaches it: it plays at unity
/// under effects turned down 12 dB, or off, and only its own switch silences
/// it.
#[test]
fn the_volumes_feedback_answers_to_its_own_switch_at_full_level() {
    let heard = Resolution::Play {
        source: CueSource::Theme {
            theme: DEFAULT_THEME,
            asset: SoundAsset::Volume,
        },
        gain: AudioGain::UNITY,
    };
    for text in [
        "sound.gain = -12dB\n",
        "sound.enabled = false\nsound.gain = -12dB\n",
    ] {
        assert_eq!(feedback(&settings(text)), heard, "{text}");
    }
    assert_eq!(
        feedback(&settings("sound.volume_feedback.enabled = false\n")),
        Resolution::Silent(Silence::Disabled)
    );
    let turned_down = settings("sound.gain = -12dB\n");
    assert_eq!(
        resolve(SoundEvent::Info, &turned_down, false, |_, _| true),
        Resolution::Play {
            source: CueSource::Theme {
                theme: DEFAULT_THEME,
                asset: SoundAsset::Info,
            },
            gain: AudioGain::new(-1_200).expect("attenuation"),
        },
        "an effect is turned down"
    );
}
