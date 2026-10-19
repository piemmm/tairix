//! `tairix-mkimage` — author flashable TAIRiX platform images.
//!
//! ```text
//! tairix-mkimage rpi \
//!     --kernel <tairix-kernel ELF> \
//!     --firmware <dir with the pinned blobs> \
//!     [--manifest <firmware.lock>] \
//!     [--profile debug|installer] \
//!     [--encrypt-root] \
//!     [--out images/tairix-aarch64-rpi-<profile>.img] \
//!     [--root-key-out <key file>]
//! ```
//!
//! The root volume is plaintext. `--encrypt-root`, a debug image's only,
//! encrypts it under a key **derived** from `tairix_mkimage::DEBUG_PASSPHRASE`,
//! with the unlock descriptor on the boot partition, and writes the derived
//! key for host-side mounting to `--root-key-out` (default `<out>.rootkey`).
//!
//! The normal entry point is `cargo xtask image --target aarch64-rpi`
//! (or `cargo xtask build --target aarch64-rpi`), which builds the kernel
//! and calls this crate as a library; the binary exists for direct,
//! scripted use. See `docs/src/install/raspberry_pi.md`.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use tairix_mkimage::firmware::FirmwareManifest;
use tairix_mkimage::{
    build_rpi_image, remove_stale_root_key, replace_file, write_root_key, HostEntropy, ImageKind,
    ImageProfile, VolumeProtection,
};

fn main() -> ExitCode {
    let argv: Vec<OsString> = env::args_os().skip(1).collect();
    match run(&argv) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("tairix-mkimage: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Parsed `rpi` subcommand arguments.
struct RpiArgs {
    kernel: PathBuf,
    firmware_dir: PathBuf,
    manifest: PathBuf,
    kind: ImageKind,
    out: PathBuf,
    root_key_out: Option<PathBuf>,
}

fn run(argv: &[OsString]) -> Result<(), String> {
    let Some((subcommand, rest)) = argv.split_first() else {
        return Err(usage());
    };
    if subcommand != "rpi" {
        return Err(usage());
    }
    let rpi = parse_rpi_args(rest)?;

    let manifest_text = std::fs::read_to_string(&rpi.manifest)
        .map_err(|e| format!("cannot read manifest {}: {e}", rpi.manifest.display()))?;
    let manifest = FirmwareManifest::parse(&manifest_text).map_err(|e| e.to_string())?;
    let firmware = manifest
        .load_dir(&rpi.firmware_dir)
        .map_err(|e| e.to_string())?;

    let kernel_elf = std::fs::read(&rpi.kernel)
        .map_err(|e| format!("cannot read kernel ELF {}: {e}", rpi.kernel.display()))?;

    // The low-level CLI installs no autoloaded driver bundles and no
    // application bundles: cross-compiling and signing the `/System/Drivers/`
    // and the `/System` program/service stores is the orchestrator's job
    // (it needs to drive `cargo` for the freestanding builds), so the
    // canonical `cargo xtask image` path supplies them. A directly-scripted
    // CLI image therefore ships empty stores (the kernel leaves every node
    // unbound, fail-closed), exactly as before — and, planting no NIC driver,
    // the canonical empty `network.conf` ("no managed interfaces beyond
    // loopback") with them.
    let network_conf = tairix_netconfig::NetworkConfig::default().render();
    let built = build_rpi_image(
        &kernel_elf,
        &firmware,
        &mut HostEntropy,
        rpi.kind,
        &[],
        &[],
        &network_conf,
    )
    .map_err(|e| e.to_string())?;

    if let Some(parent) = rpi.out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
    }
    replace_file(&rpi.out, &built.image)
        .map_err(|e| format!("cannot write image {}: {e}", rpi.out.display()))?;

    let key_out = rpi
        .root_key_out
        .unwrap_or_else(|| rpi.out.with_extension("rootkey"));
    let Some(unlock) = &built.unlock else {
        let stale = remove_stale_root_key(&key_out).map_err(|e| {
            format!(
                "cannot remove stale root-key file {}: {e}",
                key_out.display()
            )
        })?;
        println!(
            "wrote {} ({} bytes), root volume plaintext{}",
            rpi.out.display(),
            built.image.len(),
            if stale {
                "; removed the earlier build's root-key file"
            } else {
                ""
            }
        );
        return Ok(());
    };
    write_root_key(&key_out, unlock)
        .map_err(|e| format!("cannot write root-key file {}: {e}", key_out.display()))?;
    println!(
        "wrote {} ({} bytes) and root volume key {}",
        rpi.out.display(),
        built.image.len(),
        key_out.display()
    );
    Ok(())
}

fn parse_rpi_args(rest: &[OsString]) -> Result<RpiArgs, String> {
    let mut kernel = None;
    let mut firmware_dir = None;
    let mut manifest = None;
    let mut profile = None;
    let mut out = None;
    let mut root_key_out = None;
    let mut encrypt_root = false;

    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let Some(flag) = flag.to_str() else {
            return Err("arguments must be valid UTF-8".into());
        };
        let mut value = |name: &str| -> Result<PathBuf, String> {
            it.next()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match flag {
            "--kernel" => kernel = Some(value("--kernel")?),
            "--firmware" => firmware_dir = Some(value("--firmware")?),
            "--manifest" => manifest = Some(value("--manifest")?),
            "--profile" => {
                let name = value("--profile")?;
                let name = name
                    .to_str()
                    .ok_or_else(|| "--profile value must be valid UTF-8".to_string())?;
                profile = Some(ImageProfile::from_label(name).ok_or_else(|| {
                    format!("unknown profile {name:?}; expected `debug` or `installer`")
                })?);
            }
            "--out" => out = Some(value("--out")?),
            "--encrypt-root" => encrypt_root = true,
            "--root-key-out" => root_key_out = Some(value("--root-key-out")?),
            other => return Err(format!("unknown argument {other}\n{}", usage())),
        }
    }

    let profile = profile.unwrap_or(ImageProfile::Debug);
    let root = if encrypt_root {
        VolumeProtection::Encrypted
    } else {
        VolumeProtection::Plaintext
    };
    let kind =
        ImageKind::new(profile, root).map_err(|refused| format!("--encrypt-root: {refused}"))?;
    if root_key_out.is_some() && !encrypt_root {
        return Err("--root-key-out needs --encrypt-root: a plaintext root has no key".into());
    }
    Ok(RpiArgs {
        kernel: kernel.ok_or_else(|| format!("--kernel is required\n{}", usage()))?,
        firmware_dir: firmware_dir.ok_or_else(|| format!("--firmware is required\n{}", usage()))?,
        manifest: manifest.unwrap_or_else(|| {
            PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/firmware.lock"))
        }),
        kind,
        out: out.unwrap_or_else(|| {
            PathBuf::from(format!("images/tairix-aarch64-rpi-{}.img", profile.label()))
        }),
        root_key_out,
    })
}

fn usage() -> String {
    "usage: tairix-mkimage rpi --kernel <elf> --firmware <dir> \
     [--manifest <firmware.lock>] [--profile debug|installer] [--encrypt-root] \
     [--out <img>] [--root-key-out <file>]"
        .into()
}

#[cfg(test)]
mod tests {
    use super::{parse_rpi_args, VolumeProtection};
    use std::ffi::OsString;

    fn parse(args: &[&str]) -> Result<super::RpiArgs, String> {
        let args: Vec<OsString> = ["--kernel", "k.elf", "--firmware", "fw"]
            .iter()
            .chain(args)
            .map(OsString::from)
            .collect();
        parse_rpi_args(&args)
    }

    #[test]
    fn the_root_is_plaintext_unless_a_debug_image_asks_for_encryption() {
        let plain = parse(&[]).expect("parses");
        assert_eq!(plain.kind.root(), VolumeProtection::Plaintext);
        assert_eq!(plain.root_key_out, None);
        let encrypted = parse(&["--encrypt-root", "--root-key-out", "k"]).expect("parses");
        assert_eq!(encrypted.kind.root(), VolumeProtection::Encrypted);
        assert!(parse(&["--profile", "installer", "--encrypt-root"])
            .is_err_and(|why| why.contains("--encrypt-root")));
    }

    #[test]
    fn a_root_key_file_is_refused_for_a_root_that_has_no_key() {
        assert!(parse(&["--root-key-out", "k"]).is_err_and(|why| why.contains("--encrypt-root")));
    }
}
