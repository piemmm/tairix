use alloc::vec::Vec;

use tairix_abi::audio::AudioGain;
use tairix_appconf::{Document, Registry};

use super::{resolve, CueSource, Resolution};
use crate::{Silence, SoundAsset, SoundEvent, SoundSettings, DEFAULT_THEME};

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
                            let gain = AudioGain::new(-300).expect("attenuation");
                            let from = |theme| Resolution::Play {
                                source: CueSource::Theme {
                                    theme,
                                    asset: SoundAsset::of(event),
                                },
                                gain,
                            };
                            let expected = if !master || !own_switch {
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
