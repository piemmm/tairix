//! The one way a freestanding program is cross-compiled position-independent:
//! in a target directory shared by every build of the same configuration.
//!
//! A nested `cargo` building into its caller's own `OUT_DIR` compiled the
//! standard library and the runtime's whole dependency closure afresh for
//! every fixture and every kernel unit. The shared directory lies under the
//! cargo build root and is named for the configuration: the flags cargo is
//! given, and the content of the inputs it fingerprints only by path — the
//! link script and the x86_64 target spec. Builds differing in any of them
//! never share a directory; within one, cargo's own fingerprints decide what
//! is fresh, the values a guest reads through `env!` among them.
//!
//! A build holds its directory's lock from its `cargo` invocation to the last
//! read of what that produced, so no other build relinks an artefact in
//! between. A changed input names a new directory rather than wiping one
//! another build may be reading.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::hash::Hasher as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use tairix_hash::{HashSeed, SipHash13};

use crate::dep_info;
use crate::pie::{self, PieArch, Profile};

/// The directory beneath the build root the shared builds live in.
const SHARED_DIR: &str = "guest";

/// The key directory names are hashed under. Public: a name only has to keep
/// configurations apart, not withstand anyone choosing them.
const NAME_KEY: HashSeed = HashSeed::from_words(0x7461_6972_6978_2d70, 0x6965_2d62_7569_6c64);

/// What a build links.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Recipe {
    /// Executables linked against the one PIE script ([`pie::run_ld`]).
    Program,
    /// Static archives a foreign object links the runtime from: `memcpy` and
    /// its kin compiled in, and no `alloc`, since an archive registers no
    /// global allocator.
    Archive,
}

impl Recipe {
    /// The `-Z` arguments that build this recipe's sysroot.
    const fn build_std(self) -> &'static [&'static str] {
        match self {
            Self::Program => &["-Z", pie::SYSROOT_BUILD_STD],
            Self::Archive => &[
                "-Z",
                "build-std=core,compiler_builtins",
                "-Z",
                "build-std-features=compiler-builtins-mem",
            ],
        }
    }

    /// The link script this recipe's flags name, where it links.
    fn link_script(self) -> Option<PathBuf> {
        match self {
            Self::Program => Some(pie::run_ld()),
            Self::Archive => None,
        }
    }

    /// The file cargo writes for `name`: a program's binary, or an archive's
    /// package.
    fn artefact(self, name: &str) -> String {
        match self {
            Self::Program => name.to_string(),
            Self::Archive => format!("lib{}.a", name.replace('-', "_")),
        }
    }
}

/// One configuration of position-independent cross-compiles.
pub struct PieBuild<'a> {
    /// The cargo build root the shared directories live under: the target
    /// directory, or [`build_root`] from a build script.
    pub root: &'a Path,
    /// Where `cargo` runs: a directory inside this workspace.
    pub workspace: &'a Path,
    /// The freestanding target.
    pub arch: PieArch,
    /// What is linked.
    pub recipe: Recipe,
    /// The profile everything compiles in.
    pub profile: Profile,
    /// CPU-floor flags, ahead of the recipe's own; empty for none.
    pub floor: &'a [String],
    /// Environment the guests read through `env!`.
    pub env: &'a [(&'a str, String)],
}

impl PieBuild<'_> {
    /// The target-scoped flags every crate of the build compiles with. The
    /// link arguments reach only what links, but cargo fingerprints the whole
    /// string, so every recipe spells the one script alike.
    fn rustflags(&self) -> String {
        let mut flags = self.floor.join(" ");
        if !flags.is_empty() {
            flags.push(' ');
        }
        flags.push_str("-C relocation-model=pie");
        if let Some(script) = self.recipe.link_script() {
            let _ = write!(
                flags,
                " -C link-arg=-pie -C link-arg=-T{}",
                script.display()
            );
        }
        flags
    }

    /// The name of this configuration's shared directory.
    ///
    /// # Errors
    ///
    /// The link script or target spec, which the name covers, cannot be read.
    fn name(&self) -> Result<String, String> {
        let triple = self.arch.target_triple();
        let path_only = self
            .recipe
            .link_script()
            .into_iter()
            .chain(pie::target_spec_file(triple));
        let mut contents = Vec::new();
        for input in path_only {
            contents.push(fs::read(&input).map_err(|e| format!("read {}: {e}", input.display()))?);
        }
        Ok(directory_name(
            triple,
            &self.rustflags(),
            self.recipe.build_std(),
            &contents,
        ))
    }

    /// Take this configuration's shared directory, waiting while another
    /// build holds it.
    ///
    /// # Errors
    ///
    /// The directory's name cannot be formed, or its lock cannot be opened or
    /// taken.
    pub fn lock(&self) -> Result<Locked<'_>, String> {
        let shared = self.root.join(SHARED_DIR);
        fs::create_dir_all(&shared).map_err(|e| format!("create {}: {e}", shared.display()))?;
        let name = self.name()?;
        let path = shared.join(format!("{name}.lock"));
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        lock.lock()
            .map_err(|e| format!("lock {}: {e}", path.display()))?;
        Ok(Locked {
            build: self,
            dir: shared.join(name),
            _lock: lock,
        })
    }
}

/// The directory name `triple`'s configuration of `rustflags`, `build_std`
/// and the `contents` of the inputs cargo fingerprints only by path is built
/// under, every field framed by its length so no two configurations render
/// alike. Each triple has its own, so builds for different triples never wait
/// on one another.
fn directory_name(
    triple: &str,
    rustflags: &str,
    build_std: &[&str],
    contents: &[Vec<u8>],
) -> String {
    let mut hasher = SipHash13::new(NAME_KEY);
    let fields = core::iter::once(rustflags.as_bytes())
        .chain(build_std.iter().map(|arg| arg.as_bytes()))
        .chain(contents.iter().map(Vec::as_slice));
    for field in fields {
        hasher.write_u64(u64::try_from(field.len()).unwrap_or(u64::MAX));
        hasher.write(field);
    }
    format!("{triple}-{:016x}", hasher.finish())
}

/// A configuration's shared directory, held until this is dropped.
pub struct Locked<'a> {
    build: &'a PieBuild<'a>,
    dir: PathBuf,
    /// Released when the file closes.
    _lock: File,
}

impl Locked<'_> {
    /// The shared target directory.
    #[cfg(test)]
    fn dir(&self) -> &Path {
        &self.dir
    }

    /// The `cargo build` of `packages`, only their `bins` where any are named,
    /// with `features`, for the caller to run however it runs commands.
    #[must_use]
    pub fn command(&self, packages: &[&str], bins: &[&str], features: &[&str]) -> Command {
        let build = self.build;
        let mut command = pie::nested_cargo(build.workspace);
        // Target-scoped, so the flags reach only the guests' freestanding
        // crates, never their host build scripts.
        command
            .env(build.arch.rustflags_env_var(), build.rustflags())
            // Incremental state would double the directory's size.
            .env("CARGO_INCREMENTAL", "0")
            // A build script must never rewrite the committed lockfile.
            .args(["build", "--locked", "--target"])
            .arg(build.arch.cargo_target_spec())
            .args(build.recipe.build_std())
            .args(build.profile.args())
            .arg("--target-dir")
            .arg(&self.dir);
        for package in packages {
            command.args(["-p", package]);
        }
        for bin in bins {
            command.args(["--bin", bin]);
        }
        if !features.is_empty() {
            command.arg("--features").arg(features.join(","));
        }
        command.envs(build.env.iter().map(|(key, value)| (key, value)));
        command
    }

    /// Where the artefact for `name` lies: a program's binary, or an
    /// archive's package.
    #[must_use]
    pub fn artefact(&self, name: &str) -> PathBuf {
        let build = self.build;
        self.dir
            .join(build.arch.target_triple())
            .join(build.profile.dir())
            .join(build.recipe.artefact(name))
    }

    /// Have the calling build script run again once any source `artefact`
    /// was built from changes.
    pub fn emit_reruns(&self, artefact: &Path) {
        dep_info::emit_dep_info_reruns(
            &artefact.with_extension("d"),
            self.build.workspace,
            &self.dir,
        );
    }
}

/// The cargo build root a build script's `out_dir` lies in, `target` the
/// triple its unit builds for: `out_dir` is
/// `<root>[/<target>]/<profile>/build/<unit>/out`.
///
/// # Errors
///
/// An `out_dir` outside that layout, or a root cargo did not tag as its own.
pub fn build_root(out_dir: &Path, target: &str) -> Result<PathBuf, String> {
    let outside = || {
        format!(
            "{} is not a build script's output directory",
            out_dir.display()
        )
    };
    let build = out_dir
        .parent()
        .and_then(Path::parent)
        .filter(|dir| dir.file_name() == Some("build".as_ref()))
        .ok_or_else(outside)?;
    let above = build.parent().and_then(Path::parent).ok_or_else(outside)?;
    let root = if above.file_name() == Some(target.as_ref()) {
        above.parent().ok_or_else(outside)?
    } else {
        above
    };
    if root.join("CACHEDIR.TAG").is_file() {
        Ok(root.to_path_buf())
    } else {
        Err(format!(
            "{} is not a cargo build root: it holds no cache tag",
            root.display()
        ))
    }
}

#[cfg(test)]
#[path = "pie_build_tests.rs"]
mod tests;
