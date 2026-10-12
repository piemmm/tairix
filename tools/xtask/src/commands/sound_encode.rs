//! `cargo xtask sound-encode`: turn authored WAV masters into a theme's
//! shipped FLAC assets (`plans/SOUND.md` §The shipped set).
//!
//! FLAC is lossless, so the asset written is the master from then on. Each
//! master is held to the authoring contract, encoded by the first-party
//! encoder, and decoded back sample for sample before anything is written.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use tairix_sound::flac_encode::{encode, Options, Params};
use tairix_sound::SoundFormat;
use tairix_soundtheme::{is_theme_name, SoundAsset, DEFAULT_THEME, MAX_SOUND_BYTES};
use tairix_syshelp::sound::{shipped_samples, SOUND_RATE_HZ};

use crate::Context;

/// Where the themes' shipped assets live in the tree.
const ASSETS_DIR: &str = "lib/soundtheme/assets";

const USAGE: &str = "usage: cargo xtask sound-encode [--theme <name>] <master.wav>...";

/// Encode every master `args` names into its theme's assets.
///
/// # Errors
///
/// A bad argument, or the first master that breaks the contract or cannot be
/// written; nothing past it is written.
pub fn run(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let (theme, masters) = parse(args)?;
    let dir = ctx.workspace_root.join(ASSETS_DIR).join(&theme);
    fs::create_dir_all(&dir).map_err(|err| format!("sound-encode: {}: {err}", dir.display()))?;
    for master in masters {
        let (asset, flac) = encode_master(&master)
            .map_err(|why| format!("sound-encode: {}: {why}", master.display()))?;
        let out = dir.join(format!("{}.flac", asset.name()));
        fs::write(&out, &flac).map_err(|err| format!("sound-encode: {}: {err}", out.display()))?;
        eprintln!(
            "xtask: [sound-encode] {} -> {} ({} bytes)",
            master.display(),
            out.display(),
            flac.len()
        );
    }
    Ok(())
}

fn parse(args: &[OsString]) -> Result<(String, Vec<PathBuf>), String> {
    let mut theme = String::from(DEFAULT_THEME);
    let mut masters = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--theme" {
            let name = args
                .next()
                .and_then(|name| name.to_str())
                .ok_or_else(|| format!("sound-encode: `--theme` needs a name; {USAGE}"))?;
            if !is_theme_name(name) {
                return Err(format!("sound-encode: `{name}` is not a legal theme name"));
            }
            name.clone_into(&mut theme);
        } else {
            masters.push(PathBuf::from(arg));
        }
    }
    if masters.is_empty() {
        return Err(format!("sound-encode: no masters given; {USAGE}"));
    }
    Ok((theme, masters))
}

/// The asset `master` is, and its FLAC.
fn encode_master(master: &Path) -> Result<(SoundAsset, Vec<u8>), String> {
    let name = master
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("has no usable name")?;
    let asset = SoundAsset::from_file_name(&format!("{name}.flac"))
        .ok_or_else(|| format!("`{name}` is no sound asset"))?;
    let bytes = fs::read(master).map_err(|err| err.to_string())?;
    let samples = shipped_samples(&bytes, SoundFormat::Wav)?;
    Ok((asset, encode_samples(&samples)?))
}

/// `samples` as a FLAC that decodes back to exactly them.
fn encode_samples(samples: &[i16]) -> Result<Vec<u8>, String> {
    let params = Params {
        rate: SOUND_RATE_HZ,
        channels: 2,
        bits: 16,
    };
    let wide: Vec<i32> = samples.iter().map(|&sample| i32::from(sample)).collect();
    let flac = encode(params, &wide, Options::default())
        .map_err(|err| format!("does not encode: {err:?}"))?
        .finish();
    if flac.len() > MAX_SOUND_BYTES {
        return Err(format!(
            "encodes to {} bytes, over the {MAX_SOUND_BYTES}-byte bound",
            flac.len()
        ));
    }
    if shipped_samples(&flac, SoundFormat::Flac)? != samples {
        return Err(String::from("its FLAC does not decode to its samples"));
    }
    Ok(flac)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{encode_samples, parse};
    use tairix_soundtheme::DEFAULT_THEME;

    #[test]
    fn the_masters_and_their_theme_are_read_from_the_arguments() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        let (theme, masters) = parse(&args(&["a.wav", "b.wav"])).expect("parses");
        assert_eq!((theme.as_str(), masters.len()), (DEFAULT_THEME, 2));
        let (theme, _) = parse(&args(&["--theme", "Bright", "a.wav"])).expect("parses");
        assert_eq!(theme, "Bright");
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["--theme"])).is_err());
        assert!(parse(&args(&["--theme", "../x", "a.wav"])).is_err());
    }

    #[test]
    fn a_master_encodes_to_a_flac_of_exactly_its_samples() {
        let mut samples = vec![0i16; 2];
        samples.extend((1..3_000).flat_map(|n| {
            let level = i16::try_from((n * 37) % 2_001).expect("fits") - 1_000;
            [level, -level]
        }));
        samples.extend([0, 0]);
        let flac = encode_samples(&samples).expect("encodes");
        assert!(flac.starts_with(b"fLaC"));
        assert!(encode_samples(&[0, 0, 7, 7]).is_err(), "it ends in a click");
    }
}
