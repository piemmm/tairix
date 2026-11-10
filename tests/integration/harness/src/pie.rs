//! The freestanding cross-compile targets, and how cargo is told to build
//! for one.
//!
//! Every freestanding build — the QEMU kernels, the target lint passes, the
//! image pipeline's program images, the kernel's embedded programs, and the
//! fixtures' nested builds — needs the same target-derived facts: the name
//! cargo knows the target by, the `--target` value that selects it, whether
//! the toolchain ships a sysroot for it, and the `CARGO_TARGET_<name>_RUSTFLAGS`
//! variable that scopes a link recipe to it alone. `PieArch` and
//! `cargo_target_args` are the one definition of each, so no builder can
//! spell one differently.
//!
//! x86_64 builds against a first-party target spec rather than rustc's
//! `x86_64-unknown-none`, which is soft-float: rustc refuses to change a
//! target's float ABI through `-C target-feature`. A JSON target has no
//! rustup sysroot, and cargo accepts it only by path.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The one PIE link script every program links against — the system's own
/// and every test fixture's — relative to the workspace root. It names only
/// architecture-neutral section classes, so one layout serves every target.
const RUN_LD_WORKSPACE_RELPATH: &str = "lib/rt/Run.ld";

/// The one PIE link script every program links against, as an absolute path:
/// every build names it by the same string, which cargo fingerprints.
#[must_use]
pub fn run_ld() -> PathBuf {
    workspace_root().join(RUN_LD_WORKSPACE_RELPATH)
}

/// A freestanding Tier-1 target the image pipeline can cross-compile a
/// position-independent `Run` binary for.
///
/// `wasm32` is deliberately absent: it is a `cdylib` host module, not a
/// bare-metal PIE image the kernel spawn path loads, so it is not a member of
/// this cross-compile vocabulary.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PieArch {
    /// `aarch64-unknown-none` — the aarch64 image and the aarch64 QEMU
    /// verticals boot this.
    Aarch64,
    /// `riscv64gc-unknown-none-elf` — the QEMU `virt` verticals.
    Riscv64,
    /// `x86_64-tairix-none` — the BIOS/UEFI PC image and the x86_64 QEMU
    /// verticals.
    X86_64,
}

/// The `-Z build-std` set every freestanding build compiles its sysroot
/// from: `alloc` is in it because the kernel and the programs name it.
pub const SYSROOT_BUILD_STD: &str = "build-std=core,compiler_builtins,alloc";

/// The first-party x86_64 target spec, relative to the workspace root.
///
/// Its file stem is the target's name, and the `-none` in it is
/// load-bearing: `compiler_builtins` compiles its `memcpy` family in for
/// any target whose name contains `-none`.
pub const X86_64_TARGET_SPEC_RELPATH: &str = ".cargo/x86_64-tairix-none.json";

/// The workspace root, from this crate's own place in the tree.
fn workspace_root() -> &'static Path {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    // `tests/integration/harness` is three levels below the root.
    manifest_dir.ancestors().nth(3).unwrap_or(manifest_dir)
}

/// The `--target` value cargo selects the freestanding target `name` with:
/// the absolute path of the first-party spec for x86_64, and `name` itself
/// for a target rustc builds in.
///
/// Every cargo invocation that shares a target directory must pass the same
/// string, or cargo treats the two as different targets and rebuilds.
#[must_use]
pub fn cargo_target_spec(name: &str) -> OsString {
    target_spec_file(name).map_or_else(|| OsString::from(name), PathBuf::into_os_string)
}

/// The first-party spec file the target `name` is defined by, or `None` for a
/// target rustc builds in.
#[must_use]
pub fn target_spec_file(name: &str) -> Option<PathBuf> {
    (name == PieArch::X86_64.target_triple())
        .then(|| workspace_root().join(X86_64_TARGET_SPEC_RELPATH))
}

/// The cargo profile a build compiles in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Profile {
    /// Cargo's `dev` profile: `debug_assertions` on.
    Dev,
    /// `--release`: optimised, `debug_assertions` off.
    Release,
}

impl Profile {
    /// The profile of a build script's own unit, from the `PROFILE` cargo
    /// hands it: `release` for any profile inheriting from release.
    #[must_use]
    pub fn of_build_script(profile: &str) -> Self {
        if profile == "release" {
            Self::Release
        } else {
            Self::Dev
        }
    }

    /// The `cargo build` arguments that select it.
    #[must_use]
    pub const fn args(self) -> &'static [&'static str] {
        match self {
            Self::Dev => &[],
            Self::Release => &["--release"],
        }
    }

    /// The directory beneath `<target-dir>/<triple>/` cargo writes its
    /// artefacts to.
    #[must_use]
    pub const fn dir(self) -> &'static str {
        match self {
            Self::Dev => "debug",
            Self::Release => "release",
        }
    }
}

/// The arguments that make a cargo invocation build for `name` when it brings
/// no sysroot of its own: `--target` with [`cargo_target_spec`], plus
/// [`SYSROOT_BUILD_STD`] where the toolchain ships none (the first-party
/// spec).
///
/// A build that compiles its sysroot anyway — every position-independent
/// program build does — passes `--target` with [`cargo_target_spec`] and its
/// own `-Z build-std` instead.
#[must_use]
pub fn cargo_target_args(name: &str) -> Vec<OsString> {
    let mut args = vec![OsString::from("--target"), cargo_target_spec(name)];
    if name == PieArch::X86_64.target_triple() {
        args.push(OsString::from("-Z"));
        args.push(OsString::from(SYSROOT_BUILD_STD));
    }
    args
}

impl PieArch {
    /// Every architecture in this vocabulary, in a stable order, so a caller
    /// (or a test) can iterate the whole set without hard-coding the members.
    pub const ALL: &'static [PieArch] = &[PieArch::Aarch64, PieArch::Riscv64, PieArch::X86_64];

    /// The name cargo knows this architecture's target by: the `TARGET` a
    /// build script sees, the `target/<name>/` directory, the
    /// `[target.<name>]` config table, and the infix of every
    /// `CARGO_TARGET_<NAME>_*` variable. Pass [`Self::cargo_target_spec`] as
    /// `--target`, not this.
    #[must_use]
    pub const fn target_triple(self) -> &'static str {
        match self {
            PieArch::Aarch64 => "aarch64-unknown-none",
            PieArch::Riscv64 => "riscv64gc-unknown-none-elf",
            PieArch::X86_64 => "x86_64-tairix-none",
        }
    }

    /// The `--target` value that selects this architecture's target
    /// ([`cargo_target_spec`]).
    #[must_use]
    pub fn cargo_target_spec(self) -> OsString {
        cargo_target_spec(self.target_triple())
    }

    /// This architecture's stable index into a per-arch table (its position
    /// in [`Self::ALL`]), so a builder can memoise one composed artefact per
    /// target in a fixed-size `[_; PieArch::COUNT]` array without a runtime
    /// map. Kept in lockstep with [`Self::ALL`] by
    /// `index_matches_all_position`.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            PieArch::Aarch64 => 0,
            PieArch::Riscv64 => 1,
            PieArch::X86_64 => 2,
        }
    }

    /// The number of architectures in this vocabulary — the length a per-arch
    /// memo table indexed by [`Self::index`] must have.
    pub const COUNT: usize = 3;

    /// The architecture whose freestanding target triple is `triple`, or
    /// `None` for a triple this vocabulary does not name. The inverse of
    /// [`Self::target_triple`], so the image pipeline can recover the arch a
    /// QEMU enrolment's `target` string selects without a second table.
    #[must_use]
    pub fn from_target_triple(triple: &str) -> Option<PieArch> {
        PieArch::ALL
            .iter()
            .copied()
            .find(|a| a.target_triple() == triple)
    }

    /// The `CARGO_TARGET_<triple>_RUSTFLAGS` variable that scopes the PIE
    /// link recipe to [`Self::target_triple`].
    ///
    /// Cargo derives this variable from the triple by upper-casing it and
    /// replacing every character that is not a letter or digit with `_`; the
    /// `target_scoped_rustflags_var_matches_cargos_mangling` test pins each
    /// value against that rule so a typo cannot ship.
    #[must_use]
    pub const fn rustflags_env_var(self) -> &'static str {
        match self {
            PieArch::Aarch64 => "CARGO_TARGET_AARCH64_UNKNOWN_NONE_RUSTFLAGS",
            PieArch::Riscv64 => "CARGO_TARGET_RISCV64GC_UNKNOWN_NONE_ELF_RUSTFLAGS",
            PieArch::X86_64 => "CARGO_TARGET_X86_64_TAIRIX_NONE_RUSTFLAGS",
        }
    }
}

/// A `cargo` command for a build script's nested build, run from
/// `manifest_dir`, cleared of what the outer build would otherwise impose on
/// it: the outer flags, which outrank the target-scoped ones the nested build
/// is given, and a lint run's wrapper, which would lint the nested crates
/// under the outer run's settings and fail the build script on findings that
/// run reports itself.
#[must_use]
pub(crate) fn nested_cargo(manifest_dir: impl AsRef<Path>) -> Command {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .current_dir(manifest_dir)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CLIPPY_ARGS");
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cargo's own rule for turning a target triple into the infix of its
    /// per-target environment variables: upper-case, and every byte that is
    /// not ASCII-alphanumeric becomes `_`.
    fn cargo_env_infix(triple: &str) -> String {
        triple
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect()
    }

    #[test]
    fn target_triples_are_the_tier_one_freestanding_triples() {
        assert_eq!(PieArch::Aarch64.target_triple(), "aarch64-unknown-none");
        assert_eq!(
            PieArch::Riscv64.target_triple(),
            "riscv64gc-unknown-none-elf"
        );
        assert_eq!(PieArch::X86_64.target_triple(), "x86_64-tairix-none");
    }

    /// The x86_64 spec is named by its file stem, which is the name every
    /// other fact derives from, and it is a real file in the tree; no other
    /// target has one.
    #[test]
    fn the_x86_64_spec_is_the_file_its_name_derives_from() {
        let spec = PathBuf::from(PieArch::X86_64.cargo_target_spec());
        assert!(spec.is_absolute(), "{}", spec.display());
        assert!(spec.is_file(), "{} is not a file", spec.display());
        assert_eq!(
            spec.file_stem().and_then(std::ffi::OsStr::to_str),
            Some(PieArch::X86_64.target_triple())
        );
        assert!(PieArch::X86_64.target_triple().contains("-none"));
        assert_eq!(
            target_spec_file(PieArch::X86_64.target_triple()),
            Some(spec)
        );
        for arch in [PieArch::Aarch64, PieArch::Riscv64] {
            assert_eq!(target_spec_file(arch.target_triple()), None, "{arch:?}");
        }
    }

    /// The one link script is a real file, named by an absolute path every
    /// build spells alike.
    #[test]
    fn the_link_script_is_one_absolute_file() {
        let script = run_ld();
        assert!(script.is_absolute(), "{}", script.display());
        assert!(script.is_file(), "{} is not a file", script.display());
        assert!(!script
            .components()
            .any(|part| part == std::path::Component::ParentDir));
    }

    /// The profile a build script reads selects cargo's own arguments and
    /// directory for it.
    #[test]
    fn a_profile_selects_cargos_arguments_and_directory() {
        assert_eq!(Profile::of_build_script("release"), Profile::Release);
        assert_eq!(Profile::of_build_script("debug"), Profile::Dev);
        assert!(Profile::Dev.args().is_empty());
        assert_eq!(Profile::Dev.dir(), "debug");
        assert_eq!(Profile::Release.args(), ["--release"]);
        assert_eq!(Profile::Release.dir(), "release");
    }

    /// A builtin target is selected by name and brings its own sysroot; the
    /// first-party spec is selected by path and needs one built.
    #[test]
    fn only_the_first_party_spec_builds_its_own_sysroot() {
        for &arch in PieArch::ALL {
            let args = cargo_target_args(arch.target_triple());
            assert_eq!(args[0], "--target");
            assert_eq!(args[1], arch.cargo_target_spec());
            let builds_std = args[2..] == [OsString::from("-Z"), OsString::from(SYSROOT_BUILD_STD)];
            assert_eq!(builds_std, arch == PieArch::X86_64, "{arch:?}");
            if arch != PieArch::X86_64 {
                assert_eq!(args.len(), 2, "{arch:?}");
                assert_eq!(arch.cargo_target_spec(), arch.target_triple());
            }
        }
        assert_eq!(
            cargo_target_args("wasm32-unknown-unknown"),
            [
                OsString::from("--target"),
                OsString::from("wasm32-unknown-unknown")
            ]
        );
    }

    #[test]
    fn target_scoped_rustflags_var_matches_cargos_mangling() {
        for &arch in PieArch::ALL {
            let expected = format!(
                "CARGO_TARGET_{}_RUSTFLAGS",
                cargo_env_infix(arch.target_triple())
            );
            assert_eq!(
                arch.rustflags_env_var(),
                expected,
                "{arch:?} rustflags var must match cargo's triple mangling",
            );
        }
    }

    #[test]
    fn all_lists_each_arch_once() {
        let mut seen = PieArch::ALL.to_vec();
        seen.sort_by_key(|a| format!("{a:?}"));
        seen.dedup();
        assert_eq!(
            seen.len(),
            PieArch::ALL.len(),
            "ALL must not repeat an arch"
        );
    }

    #[test]
    fn index_matches_all_position() {
        assert_eq!(PieArch::ALL.len(), PieArch::COUNT);
        for (position, &arch) in PieArch::ALL.iter().enumerate() {
            assert_eq!(
                arch.index(),
                position,
                "{arch:?} index must be its position in ALL"
            );
            assert!(arch.index() < PieArch::COUNT);
        }
    }

    #[test]
    fn from_target_triple_inverts_target_triple() {
        for &arch in PieArch::ALL {
            assert_eq!(
                PieArch::from_target_triple(arch.target_triple()),
                Some(arch)
            );
        }
        assert_eq!(PieArch::from_target_triple("wasm32-unknown-unknown"), None);
        assert_eq!(PieArch::from_target_triple(""), None);
    }

    /// A nested build runs clear of the outer flags and of a lint run's
    /// wrapper: under `cargo clippy` the wrapper otherwise lints the nested
    /// program crates with the outer run's denials and fails the build script.
    #[test]
    fn a_nested_cargo_clears_the_outer_flags_and_lint_wrapper() {
        let command = super::nested_cargo("/tmp");
        let cleared: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_owned())
            .collect();
        for key in [
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTFLAGS",
            "RUSTC_WORKSPACE_WRAPPER",
            "CLIPPY_ARGS",
        ] {
            assert!(
                cleared.iter().any(|cleared| cleared == key),
                "{key} is cleared"
            );
        }
        assert_eq!(
            command.get_current_dir(),
            Some(std::path::Path::new("/tmp"))
        );
    }
}
