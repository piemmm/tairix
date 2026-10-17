//! The build's signing key set: the three secret Ed25519 seeds a TAIRiX build
//! signs its drivers, its first-party applications and their publisher
//! certificate with.
//!
//! Whoever holds the key set can sign drivers and programs that every kernel
//! built from it admits, so it is an input to the build and never part of the
//! source. It lives outside the workspace, readable by its owner alone, and the
//! kernel embeds only the public anchors derived from it. A build without one
//! fails rather than falling back to a key anybody could read; `cargo xtask
//! keys init` creates one.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use tairix_util::conf::setting_line;
use tairix_util::hex;
use tairix_util::secret::{Wiped, WipedBuf};

/// The environment variable naming the key-set file, by absolute path.
pub const ENV: &str = "TAIRIX_SIGNING_KEYS";

/// Every environment variable [`location`] reads: a build script that loads
/// the key set reruns when one changes.
pub const LOCATION_VARS: [&str; 3] = [ENV, "XDG_DATA_HOME", "HOME"];

/// Bytes in one seed.
pub const SEED_LEN: usize = 32;

/// The longest file read as a key set: three seed lines and their comment.
const MAX_FILE_LEN: u64 = 4096;

/// The comment a created key set opens with.
const FILE_HEADER: &str = "# TAIRiX signing key set. Secret: whoever holds it can sign drivers and\n\
                           # programs every kernel built with it admits. Never commit or share it.\n";

/// Seeds once published in the source tree, which anybody can sign with.
const PUBLISHED_SEEDS: [[u8; SEED_LEN]; 3] = [
    *b"tairix-kernel-driver-signing/v1!",
    *b"tairix-system-app-signing/v1!\0\0\0",
    *b"tairix-system-app-publisher/v1!\0",
];

/// What one seed signs for. Each is a trust domain of its own, so authority
/// over one is never authority over another.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Domain {
    /// Driver manifests, admitted by the kernel's driver-load anchor.
    DriverSigning,
    /// System application manifests, admitted by the kernel's application
    /// anchor.
    AppSigning,
    /// The first-party publisher, which certifies the application-signing
    /// key and owns the applications' stored data.
    AppPublisher,
}

impl Domain {
    /// Every domain, in the order a key-set file lists them.
    pub const ALL: [Self; 3] = [Self::DriverSigning, Self::AppSigning, Self::AppPublisher];

    /// The key naming this domain's seed in a key-set file.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::DriverSigning => "driver-signing",
            Self::AppSigning => "app-signing",
            Self::AppPublisher => "app-publisher",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::DriverSigning => 0,
            Self::AppSigning => 1,
            Self::AppPublisher => 2,
        }
    }
}

/// A build's three secret seeds, erased when dropped.
pub struct SigningKeys {
    seeds: [Wiped<SEED_LEN>; 3],
}

impl SigningKeys {
    /// The seed signing for `domain`.
    #[must_use]
    pub fn seed(&self, domain: Domain) -> &[u8; SEED_LEN] {
        &self.seeds[domain.index()]
    }

    /// A fresh key set, every seed drawn from `entropy`.
    ///
    /// # Errors
    ///
    /// The entropy source's failure, or why what it gave is no key set.
    pub fn generate(
        entropy: &mut dyn FnMut(&mut [u8]) -> io::Result<()>,
    ) -> Result<Self, KeyError> {
        let mut seeds = [Wiped::new(), Wiped::new(), Wiped::new()];
        for seed in &mut seeds {
            entropy(&mut seed[..]).map_err(KeyError::Entropy)?;
        }
        Self::checked(seeds)
    }

    /// The key set `text` spells: a `key value` line per domain, every seed in
    /// 64 hex digits, and `#` comments.
    ///
    /// # Errors
    ///
    /// Why `text` is not a key set a build may sign with.
    pub fn parse(text: &str) -> Result<Self, KeyError> {
        let mut seeds = [Wiped::new(), Wiped::new(), Wiped::new()];
        let mut seen = [false; 3];
        for (index, line) in text.lines().enumerate() {
            let Some(setting) = setting_line(line) else {
                continue;
            };
            let malformed = |why| KeyError::Malformed {
                line: index + 1,
                why,
            };
            let domain = Domain::ALL
                .into_iter()
                .find(|domain| domain.key() == setting.key)
                .ok_or_else(|| malformed("not a key-set field"))?;
            if core::mem::replace(&mut seen[domain.index()], true) {
                return Err(malformed("a seed given twice"));
            }
            let digits = setting.value.unwrap_or_default();
            if !hex::decode_into(digits, &mut seeds[domain.index()][..]) {
                return Err(malformed("a seed is 64 hex digits"));
            }
        }
        if let Some(domain) = Domain::ALL.into_iter().find(|domain| !seen[domain.index()]) {
            return Err(KeyError::MissingSeed(domain));
        }
        Self::checked(seeds)
    }

    /// `seeds` as a key set, unless no build should sign with them.
    fn checked(seeds: [Wiped<SEED_LEN>; 3]) -> Result<Self, KeyError> {
        let keys = Self { seeds };
        for domain in Domain::ALL {
            let seed = keys.seed(domain);
            if PUBLISHED_SEEDS.contains(seed) {
                return Err(KeyError::Published(domain));
            }
            if seed.iter().all(|&byte| byte == seed[0]) {
                return Err(KeyError::Constant(domain));
            }
        }
        let [driver, app, publisher] = Domain::ALL.map(|domain| keys.seed(domain));
        if driver == app || app == publisher || driver == publisher {
            return Err(KeyError::Shared);
        }
        Ok(keys)
    }

    /// The key set as its file holds it, erased when dropped.
    fn encode(&self) -> WipedBuf {
        let len = FILE_HEADER.len()
            + Domain::ALL
                .iter()
                .map(|domain| domain.key().len() + 2 * SEED_LEN + 2)
                .sum::<usize>();
        // Sized once, so the text never reallocates and leaves a copy behind.
        let mut text = Vec::with_capacity(len);
        text.extend_from_slice(FILE_HEADER.as_bytes());
        for domain in Domain::ALL {
            text.extend_from_slice(domain.key().as_bytes());
            text.push(b' ');
            text.extend(self.seed(domain).iter().flat_map(|&byte| hex::digits(byte)));
            text.push(b'\n');
        }
        WipedBuf::new(text)
    }
}

/// Why there is no key set a build may sign with.
#[derive(Debug)]
pub enum KeyError {
    /// No key set has been created at the location.
    Missing(PathBuf),
    /// None of [`LOCATION_VARS`] names a location.
    Unlocated,
    /// `$TAIRIX_SIGNING_KEYS` is relative, so it would name a different file
    /// in every process that resolved it.
    Relative(PathBuf),
    /// The key set lies inside the source tree, where it could be committed.
    InsideWorkspace(PathBuf),
    /// The key set is readable by more than its owner.
    Exposed(PathBuf),
    /// A key set already exists where one was to be created.
    Exists(PathBuf),
    /// Line `line` (0 for the file as a whole) is not part of a key set.
    Malformed {
        /// The 1-based line, or 0 for the whole file.
        line: usize,
        /// What is wrong with it.
        why: &'static str,
    },
    /// The key set has no seed for a domain.
    MissingSeed(Domain),
    /// A seed is one once published in the source tree.
    Published(Domain),
    /// A seed is one byte repeated, which no working entropy source gives.
    Constant(Domain),
    /// Two domains share a seed, making one an authority over the other.
    Shared,
    /// The entropy source failed.
    Entropy(io::Error),
    /// Reading or writing the key set failed.
    Io(PathBuf, io::Error),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(path) => write!(
                f,
                "no signing key set at {}: create one with `cargo xtask keys init`, or name an \
                 existing one in {ENV}",
                path.display()
            ),
            Self::Unlocated => write!(
                f,
                "no signing key set location: set {ENV} to an absolute path, or HOME"
            ),
            Self::Relative(path) => {
                write!(f, "{ENV} must be an absolute path, not {}", path.display())
            }
            Self::InsideWorkspace(path) => write!(
                f,
                "the signing key set {} lies inside the source tree, where it could be \
                 committed: keep it outside",
                path.display()
            ),
            Self::Exposed(path) => write!(
                f,
                "the signing key set {} is readable by others: restrict it to its owner \
                 (chmod 600)",
                path.display()
            ),
            Self::Exists(path) => write!(
                f,
                "a signing key set already exists at {}, and is never replaced: the kernels \
                 built with it trust only what it signs",
                path.display()
            ),
            Self::Malformed { line: 0, why } => write!(f, "malformed signing key set: {why}"),
            Self::Malformed { line, why } => {
                write!(f, "malformed signing key set at line {line}: {why}")
            }
            Self::MissingSeed(domain) => {
                write!(f, "the signing key set has no {} seed", domain.key())
            }
            Self::Published(domain) => write!(
                f,
                "the signing key set's {} seed was once published in the source tree, so \
                 anybody can sign with it: create a new key set",
                domain.key()
            ),
            Self::Constant(domain) => write!(
                f,
                "the signing key set's {} seed is one byte repeated: create a new key set from a \
                 working entropy source",
                domain.key()
            ),
            Self::Shared => f.write_str(
                "two of the signing key set's seeds are the same, which would make one trust \
                 domain an authority over another",
            ),
            Self::Entropy(err) => write!(f, "the entropy source failed: {err}"),
            Self::Io(path, err) => write!(f, "{}: {err}", path.display()),
        }
    }
}

impl std::error::Error for KeyError {}

/// Where the key set lives: the absolute path `$TAIRIX_SIGNING_KEYS`, else
/// `$XDG_DATA_HOME/tairix/signing-keys`, else
/// `$HOME/.local/share/tairix/signing-keys`.
///
/// # Errors
///
/// [`KeyError::Relative`] for a relative `$TAIRIX_SIGNING_KEYS`, and
/// [`KeyError::Unlocated`] when no variable names a location.
pub fn location() -> Result<PathBuf, KeyError> {
    location_from(&|name| std::env::var_os(name))
}

fn location_from(var: &dyn Fn(&str) -> Option<OsString>) -> Result<PathBuf, KeyError> {
    let set = |name| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if let Some(path) = set(ENV) {
        return if path.is_absolute() {
            Ok(path)
        } else {
            Err(KeyError::Relative(path))
        };
    }
    // A relative XDG directory is invalid by that specification and ignored.
    if let Some(data) = set("XDG_DATA_HOME").filter(|path| path.is_absolute()) {
        return Ok(data.join("tairix").join("signing-keys"));
    }
    set("HOME")
        .filter(|path| path.is_absolute())
        .map(|home| home.join(".local/share/tairix/signing-keys"))
        .ok_or(KeyError::Unlocated)
}

/// The key set at [`location`], and the path it was read from.
///
/// # Errors
///
/// Why there is no key set a build may sign with: none was created, it lies
/// inside `workspace`, more than its owner can read it, or it is malformed.
pub fn load(workspace: &Path) -> Result<(PathBuf, SigningKeys), KeyError> {
    let path = location()?;
    let keys = load_at(&path, workspace)?;
    Ok((path, keys))
}

fn load_at(path: &Path, workspace: &Path) -> Result<SigningKeys, KeyError> {
    let io_error = |err| KeyError::Io(path.to_path_buf(), err);
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(KeyError::Missing(path.to_path_buf()))
        }
        Err(err) => return Err(io_error(err)),
    };
    refuse_inside(path, workspace)?;
    // The open file's own metadata, so what is checked is what is read.
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_LEN {
        return Err(KeyError::Malformed {
            line: 0,
            why: "not a key-set file",
        });
    }
    if !owner_only(&metadata) {
        return Err(KeyError::Exposed(path.to_path_buf()));
    }
    let len = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    let mut text = WipedBuf::with_capacity(len).map_err(|_| KeyError::Malformed {
        line: 0,
        why: "too large to hold",
    })?;
    let bytes = text.extend_zeroed(len).ok_or(KeyError::Malformed {
        line: 0,
        why: "too large to hold",
    })?;
    file.read_exact(bytes).map_err(io_error)?;
    let text = std::str::from_utf8(&text).map_err(|_| KeyError::Malformed {
        line: 0,
        why: "not text",
    })?;
    SigningKeys::parse(text)
}

/// Create a fresh key set at [`location`], every seed drawn from `entropy`,
/// readable by its owner alone, and answer where it was written. An existing
/// key set is never replaced.
///
/// # Errors
///
/// [`KeyError::Exists`] when there is one already, [`KeyError::InsideWorkspace`]
/// when the location lies inside `workspace`, or the failure to draw or write
/// it.
pub fn create(
    workspace: &Path,
    entropy: &mut dyn FnMut(&mut [u8]) -> io::Result<()>,
) -> Result<PathBuf, KeyError> {
    let path = location()?;
    create_at(&path, workspace, entropy)?;
    Ok(path)
}

fn create_at(
    path: &Path,
    workspace: &Path,
    entropy: &mut dyn FnMut(&mut [u8]) -> io::Result<()>,
) -> Result<(), KeyError> {
    refuse_inside(path, workspace)?;
    let keys = SigningKeys::generate(entropy)?;
    if let Some(directory) = path.parent() {
        let mut builder = DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
            .create(directory)
            .map_err(|err| KeyError::Io(directory.to_path_buf(), err))?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).map_err(|err| {
        if err.kind() == io::ErrorKind::AlreadyExists {
            KeyError::Exists(path.to_path_buf())
        } else {
            KeyError::Io(path.to_path_buf(), err)
        }
    })?;
    let written = file
        .write_all(&keys.encode())
        .and_then(|()| file.sync_all());
    if let Err(err) = written {
        // A partial key set is no key set; leave none behind.
        let _ = fs::remove_file(path);
        return Err(KeyError::Io(path.to_path_buf(), err));
    }
    Ok(())
}

/// Refuse a `path` that resolves inside `workspace`.
fn refuse_inside(path: &Path, workspace: &Path) -> Result<(), KeyError> {
    let real = resolved(path).map_err(|err| KeyError::Io(path.to_path_buf(), err))?;
    let root =
        fs::canonicalize(workspace).map_err(|err| KeyError::Io(workspace.to_path_buf(), err))?;
    if real.starts_with(&root) {
        Err(KeyError::InsideWorkspace(real))
    } else {
        Ok(())
    }
}

/// `path` with its existing part's links resolved and the rest appended, so a
/// file not yet created resolves where it would be.
fn resolved(path: &Path) -> io::Result<PathBuf> {
    let mut missing: Vec<&OsStr> = Vec::new();
    let mut existing = path;
    loop {
        match fs::canonicalize(existing) {
            Ok(real) => {
                return Ok(missing
                    .iter()
                    .rev()
                    .fold(real, |real, part| real.join(part)))
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
                    return Err(err);
                };
                missing.push(name);
                existing = parent;
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(unix)]
fn owner_only(metadata: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    const GROUP_OR_OTHER: u32 = 0o077;
    metadata.permissions().mode() & GROUP_OR_OTHER == 0
}

/// A host without Unix permission bits leaves the file's access to its own
/// controls.
#[cfg(not(unix))]
fn owner_only(_: &Metadata) -> bool {
    true
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
