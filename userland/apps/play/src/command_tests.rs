use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use core::num::NonZeroU32;

use tairix_abi::audio::AudioGain;
use tairix_audio::target::AudioTarget;

use super::{parse, Command, Interface, Options, Passes, Span, UsageError};

fn play(args: &[&str]) -> Options {
    match parse(args) {
        Ok(Command::Play(options)) => options,
        other => panic!("{args:?} gave {other:?}"),
    }
}

fn level(millibel: i32) -> AudioGain {
    AudioGain::new(millibel).expect("attenuation")
}

#[test]
fn files_alone_play_once_on_the_default_sink_at_unity() {
    let options = play(&["a.wav", "b.au"]);
    assert_eq!(options.files, vec!["a.wav".to_string(), "b.au".to_string()]);
    assert_eq!(options.interface, Interface::Auto);
    assert_eq!(options.target, AudioTarget::DEFAULT_SINK);
    assert_eq!(options.gain, AudioGain::UNITY);
    assert_eq!(options.passes, Passes::ONCE);
    assert_eq!(options.start, Span::ZERO);
    assert_eq!(options.duration, None);
}

#[test]
fn every_spelling_of_a_value_reaches_the_same_option() {
    for args in [
        &["-g-6", "x"][..],
        &["-g", "-6", "x"],
        &["--gain=-6", "x"],
        &["--gain", "-6", "x"],
        &["-qg", "-6", "x"],
    ] {
        assert_eq!(play(args).gain, level(-600), "{args:?}");
    }
    assert_eq!(
        play(&["-d", "audio:sink/3", "x"]).target.device_id(),
        Some(3)
    );
    let kept = play(&["-d", "audio:sink/9f3a1c0042de7701.0", "x"]).target;
    assert_eq!(
        kept.device_id(),
        None,
        "a place is resolved when the session opens"
    );
    assert_eq!(
        play(&["--device=audio:sink/default", "x"]).target,
        AudioTarget::DEFAULT_SINK
    );
}

#[test]
fn short_options_cluster() {
    let options = play(&["-qv", "x"]);
    assert!(options.quiet && options.verbose);
    assert_eq!(options.interface, Interface::Off);
}

#[test]
fn the_last_word_on_the_interface_wins() {
    assert_eq!(play(&["-q", "--ui", "x"]).interface, Interface::Forced);
    assert_eq!(play(&["--ui", "--no-ui", "x"]).interface, Interface::Off);
}

#[test]
fn a_loop_count_is_attached_or_absent() {
    assert_eq!(play(&["-l", "x"]).passes, Passes::Forever);
    assert_eq!(play(&["--loop", "x"]).passes, Passes::Forever);
    let three = Passes::Times(NonZeroU32::new(3).expect("nonzero"));
    assert_eq!(play(&["-l3", "x"]).passes, three);
    assert_eq!(play(&["--loop=3", "x"]).passes, three);
    // A detached count is a file: the count is optional, so it never takes the
    // next argument.
    let options = play(&["-l", "3"]);
    assert_eq!(options.passes, Passes::Forever);
    assert_eq!(options.files, vec!["3".to_string()]);
    for bad in ["--loop=0", "--loop=", "-lx", "--loop=-1"] {
        assert!(
            matches!(
                parse(&[bad, "x"]),
                Err(UsageError::Invalid {
                    option: "--loop",
                    ..
                })
            ),
            "{bad}"
        );
    }
}

#[test]
fn times_are_read_to_the_nanosecond() {
    for (text, nanos) in [
        ("90", 90_000_000_000),
        ("1:30", 90_000_000_000),
        ("1:02:03", 3_723_000_000_000),
        ("12.5", 12_500_000_000),
        ("0.000000001", 1),
        ("100:00", 6_000_000_000_000),
    ] {
        assert_eq!(
            play(&["-s", text, "x"]).start,
            Span::from_nanos(nanos),
            "{text}"
        );
    }
    assert_eq!(
        play(&["--duration=2", "x"]).duration,
        Some(Span::from_nanos(2_000_000_000))
    );
    for bad in [
        "",
        "1:",
        ":5",
        "1::2",
        "1:60",
        "1:02:60",
        "1:2:3:4",
        "1.",
        "1.0000000001",
        "a",
        "-1",
    ] {
        assert!(
            matches!(
                parse(&["-s", bad, "x"]),
                Err(UsageError::Invalid {
                    option: "--start",
                    ..
                })
            ),
            "{bad:?}"
        );
    }
}

#[test]
fn a_gain_is_attenuation_in_hundredths_of_a_decibel() {
    for (text, millibel) in [
        ("-6", -600),
        ("-3.5", -350),
        ("-3.25", -325),
        ("0", 0),
        ("-0", 0),
    ] {
        assert_eq!(play(&["-g", text, "x"]).gain, level(millibel), "{text}");
    }
    assert_eq!(
        parse(&["-g", "+3", "x"]),
        Err(UsageError::Invalid {
            option: "--gain",
            why: "a stream can only be attenuated: 0 dB or below",
        })
    );
    for bad in ["", "-", "-3.", "-3.333", "x", "--6", "-99999999999"] {
        assert!(
            matches!(
                parse(&["-g", bad, "x"]),
                Err(UsageError::Invalid {
                    option: "--gain",
                    ..
                })
            ),
            "{bad:?}"
        );
    }
}

#[test]
fn a_device_must_be_a_sink() {
    assert!(matches!(
        parse(&["-d", "audio:source/default", "x"]),
        Err(UsageError::Invalid {
            option: "--device",
            ..
        })
    ));
    assert!(matches!(
        parse(&["-d", "speakers", "x"]),
        Err(UsageError::Invalid {
            option: "--device",
            ..
        })
    ));
}

#[test]
fn help_and_version_answer_whatever_else_is_said() {
    for args in [&["-h"][..], &["-?"], &["--help"], &["x", "-h", "--bogus"]] {
        assert_eq!(parse(args), Ok(Command::Help), "{args:?}");
    }
    assert_eq!(parse(&["--version"]), Ok(Command::Version));
    assert_eq!(parse(&["--list-devices"]), Ok(Command::ListDevices));
}

#[test]
fn what_is_not_an_option_is_refused_by_name() {
    assert_eq!(
        parse(&["-x", "f"]),
        Err(UsageError::Unknown("-x".to_string()))
    );
    assert_eq!(
        parse(&["--colour", "f"]),
        Err(UsageError::Unknown("--colour".to_string()))
    );
    assert_eq!(
        parse(&["--quiet=yes", "f"]),
        Err(UsageError::Unwanted("--quiet"))
    );
    assert_eq!(parse(&["f", "-g"]), Err(UsageError::Missing("--gain")));
    assert_eq!(parse(&[]), Err(UsageError::NoFiles));
    assert_eq!(parse(&["-"]), Err(UsageError::StandardInput));
}

#[test]
fn a_double_dash_ends_the_options() {
    let options = play(&["--", "-q", "--ui"]);
    assert_eq!(options.files, vec!["-q".to_string(), "--ui".to_string()]);
    assert_eq!(options.interface, Interface::Auto);
}

/// Every locale's `OPTIONS` documents exactly the switches this parser takes,
/// read from the bundle's own `Help/` tree — the one source the image builder
/// plants — and each documented switch is one the parser accepts.
#[test]
fn help_documents_the_parser_switches() {
    extern crate std;
    use alloc::format;
    use std::fs;

    const SWITCHES: [(&str, &[&str]); 11] = [
        ("`-q, --quiet`", &["-q", "--quiet"]),
        ("`-v, --verbose`", &["-v", "--verbose"]),
        ("`--ui, --no-ui`", &["--ui", "--no-ui"]),
        (
            "`-d, --device <sink>`",
            &["-daudio:sink/1", "--device=audio:sink/1"],
        ),
        ("`-g, --gain <dB>`", &["-g-3", "--gain=-3"]),
        ("`-s, --start <time>`", &["-s1:00", "--start=1:00"]),
        ("`-t, --duration <time>`", &["-t5", "--duration=5"]),
        ("`-l, --loop[=N]`", &["-l", "-l2", "--loop", "--loop=2"]),
        ("`--list-devices`", &["--list-devices"]),
        ("`-h, -?, --help`", &["-h", "-?", "--help"]),
        ("`--version`", &["--version"]),
    ];
    for (_, spellings) in SWITCHES {
        for spelling in spellings {
            assert!(parse(&[spelling, "x.wav"]).is_ok(), "{spelling}");
        }
    }
    let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
    for locale in tairix_help::REQUIRED_LOCALES {
        let path = format!("{help_root}/{locale}/play.md");
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let documented: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("- `-"))
            .filter_map(|line| line.split(" — ").next())
            .map(|key| key.trim_start_matches("- "))
            .collect();
        let pinned: Vec<&str> = SWITCHES.iter().map(|(key, _)| *key).collect();
        assert_eq!(documented, pinned, "{locale}/play.md");
    }
}
