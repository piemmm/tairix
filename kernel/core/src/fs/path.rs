//! Path spellings and the top-level layout names.
//!
//! A [`Path`] is an *absolute*, normalised sequence of names: a mount point,
//! a boot-layout node, or what a one-driver reader asks for. A caller's own
//! spelling instead becomes steps (`parse_caller_path`) with every `..`
//! kept for the walk to apply to the directory it reaches, and a link's
//! stored target its own looser steps ([`parse_link_target`]).
//!
//! The top-level names are [`tairix_abi::ROOT_VIEW`]: data, not control
//! flow.

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::fs::{FS_NAME_MAX, FS_SYMLINK_MAX};
use tairix_abi::ROOT_VIEW;

use super::VfsError;

/// Maximum bytes in a single path component: the ABI's name bound, so the
/// VFS and every caller judge a name alike.
///
/// Bounding it keeps a hostile on-disk record from forcing an unbounded
/// component allocation.
pub const MAX_COMPONENT_LEN: usize = FS_NAME_MAX;

/// Maximum number of components in a single absolute path.
///
/// Bounds resolution work and stack depth. Raising it is a reviewed change
/// here, not a per-call override.
pub const MAX_PATH_COMPONENTS: usize = 64;

/// Resolve a machine-alias name to the top-level view component it roots at.
///
/// TAIRiX storage is a forest of named roots (`docs/src/filesystem/drives.md`):
/// a path names its root explicitly, and `System:` is a canonical first-class
/// root of which `/System` is merely the synthetic-view *projection*. The
/// **machine aliases** are therefore exactly [`ROOT_VIEW`] — one definition,
/// so the view and the alias namespace can never drift apart.
/// `System:/Kernel/x` and `/System/Kernel/x` name the same object.
///
/// Session and volume aliases are published by their owning services when
/// those land, never invented ahead of a live publisher, so an unknown name
/// resolves to `None` and the caller fails closed.
#[must_use]
pub fn resolve_machine_alias(name: &str) -> Option<&'static str> {
    ROOT_VIEW.iter().copied().find(|root| *root == name)
}

/// An absolute, normalised filesystem path.
///
/// Invariants, established at [`Path::parse`] and preserved by every
/// method:
///
/// * every component is a non-empty name of at most [`MAX_COMPONENT_LEN`]
///   bytes containing neither `/` nor a NUL byte,
/// * no component is `.` or `..`,
/// * there are at most [`MAX_PATH_COMPONENTS`] components.
///
/// The empty component list is the root (`/`).
#[derive(Clone, Debug, Eq, PartialEq, Hash, Default)]
pub struct Path {
    components: Vec<String>,
}

impl Path {
    /// The root path (`/`).
    #[must_use]
    pub fn root() -> Self {
        Self {
            components: Vec::new(),
        }
    }

    /// Parse an absolute path string into a normalised [`Path`].
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::InvalidPath`] if `text` is not absolute, has an
    /// empty/over-long component, contains a `.`/`..`/NUL token, or has
    /// more than [`MAX_PATH_COMPONENTS`] components.
    pub fn parse(text: &str) -> Result<Self, VfsError> {
        let Some(rest) = text.strip_prefix('/') else {
            return Err(VfsError::InvalidPath);
        };
        let mut components = Vec::new();
        for raw in rest.split('/') {
            if raw.is_empty() {
                // A trailing slash or a `//` run collapses to nothing; an
                // interior empty segment is the same as a redundant slash.
                continue;
            }
            validate_component(raw)?;
            if components.len() == MAX_PATH_COMPONENTS {
                return Err(VfsError::InvalidPath);
            }
            let name = owned_name(raw)?;
            components
                .try_reserve(1)
                .map_err(|_| VfsError::OutOfMemory)?;
            components.push(name);
        }
        Ok(Self { components })
    }

    /// The path's components, root-first. Empty for the root path.
    #[must_use]
    pub fn components(&self) -> &[String] {
        &self.components
    }

    /// The path's components, root-first, owned.
    #[must_use]
    pub fn into_components(self) -> Vec<String> {
        self.components
    }

    /// A copy of the path's components, root-first.
    ///
    /// # Errors
    ///
    /// [`VfsError::OutOfMemory`] when they cannot be held.
    pub fn owned_components(&self) -> Result<Vec<String>, VfsError> {
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(self.components.len())
            .map_err(|_| VfsError::OutOfMemory)?;
        for name in &self.components {
            owned.push(owned_name(name)?);
        }
        Ok(owned)
    }

    /// `true` if this is the root path (`/`).
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.components.is_empty()
    }

    /// Number of components (zero for the root).
    #[must_use]
    pub fn depth(&self) -> usize {
        self.components.len()
    }

    /// The first (top-level) component, or `None` for the root.
    #[must_use]
    pub fn top_level(&self) -> Option<&str> {
        self.components.first().map(String::as_str)
    }

    /// The final component (the file or directory name), or `None` for the
    /// root.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.components.last().map(String::as_str)
    }

    /// The parent path, or `None` for the root.
    #[must_use]
    pub fn parent(&self) -> Option<Path> {
        if self.components.is_empty() {
            return None;
        }
        let mut components = self.components.clone();
        components.pop();
        Some(Self { components })
    }

    /// `true` if `self` is a prefix of (or equal to) `other`, comparing
    /// component-by-component. The root is a prefix of every path.
    #[must_use]
    pub fn is_prefix_of(&self, other: &Path) -> bool {
        if self.components.len() > other.components.len() {
            return false;
        }
        self.components
            .iter()
            .zip(other.components.iter())
            .all(|(a, b)| a == b)
    }
}

/// Whether `name` is a component [`Path::parse`] admits.
pub(crate) fn is_component(name: &str) -> bool {
    tairix_abi::fs::is_component(name.as_bytes())
}

/// Validate a single path component against the [`Path`] invariants.
fn validate_component(name: &str) -> Result<(), VfsError> {
    if is_component(name) {
        Ok(())
    } else {
        Err(VfsError::InvalidPath)
    }
}

/// The spelling every name must have, whoever supplied it.
///
/// Shared by [`Path::parse`] — which additionally forbids the `.`/`..`
/// tokens — and by [`parse_link_target`], which permits them.
fn validate_name(name: &str) -> Result<(), VfsError> {
    if tairix_abi::fs::is_name(name.as_bytes()) {
        Ok(())
    } else {
        Err(VfsError::InvalidPath)
    }
}

/// One step of a path: a caller's or a symbolic link's target's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetStep {
    /// Descend into the named child.
    Name(String),
    /// Ascend from the directory the walk stands in.
    Up,
}

/// One step of a resolution and who supplied it: a caller's `..` may leave
/// a projection, a link's never does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Step {
    /// The step.
    pub(crate) kind: TargetStep,
    /// Whether a symbolic link's stored target supplied it.
    pub(crate) from_link: bool,
}

/// A caller's path as the walk takes it: whether it starts at the view root,
/// and its steps, each the caller's own.
pub(crate) struct CallerPath {
    /// Whether the path starts at the view root rather than where the caller
    /// stands.
    pub(crate) absolute: bool,
    /// The steps, in order.
    pub(crate) steps: VecDeque<Step>,
}

/// Parse a caller's `/`-view or relative path through the one shared
/// grammar: `.` dropped, `..` kept for the walk to apply to the directory it
/// reaches rather than to the text before it.
///
/// # Errors
///
/// [`VfsError::InvalidPath`] for a spelling the grammar refuses, for more
/// than [`MAX_PATH_COMPONENTS`] steps, or for an alias or `id::` spelling,
/// which the syscall layer turns into the view location it names before a
/// path reaches the VFS; [`VfsError::OutOfMemory`] when the steps cannot be
/// held.
pub(crate) fn parse_caller_path(text: &str) -> Result<CallerPath, VfsError> {
    let (root, spelled) = tairix_path::steps(text).map_err(|_| VfsError::InvalidPath)?;
    let absolute = match root {
        tairix_path::Root::View => true,
        tairix_path::Root::Relative => false,
        tairix_path::Root::Alias(_) | tairix_path::Root::VolumeId(_) => {
            return Err(VfsError::InvalidPath)
        }
    };
    let mut steps = VecDeque::new();
    for step in spelled {
        if steps.len() == MAX_PATH_COMPONENTS {
            return Err(VfsError::InvalidPath);
        }
        let kind = match step.map_err(|_| VfsError::InvalidPath)? {
            tairix_path::Step::Up => TargetStep::Up,
            tairix_path::Step::Name(name) => TargetStep::Name(owned_name(name)?),
        };
        steps.try_reserve(1).map_err(|_| VfsError::OutOfMemory)?;
        steps.push_back(Step {
            kind,
            from_link: false,
        });
    }
    Ok(CallerPath { absolute, steps })
}

/// `name`, owned.
///
/// # Errors
///
/// [`VfsError::OutOfMemory`] when it cannot be held.
pub(crate) fn owned_name(name: &str) -> Result<String, VfsError> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(name.len())
        .map_err(|_| VfsError::OutOfMemory)?;
    owned.push_str(name);
    Ok(owned)
}

/// Spell a normalised `/`-view path from its components: `/` for the root,
/// otherwise the `/`-joined component names.
///
/// The inverse of [`Path::parse`], and the one spelling every path the
/// kernel reports takes, so it is spelled exactly as a path it accepts.
///
/// # Errors
///
/// [`VfsError::OutOfMemory`] when the spelling cannot be held.
pub fn spell(components: &[String]) -> Result<String, VfsError> {
    let len = components.iter().map(|c| c.len() + 1).sum::<usize>();
    let mut out = String::new();
    out.try_reserve_exact(len.max(1))
        .map_err(|_| VfsError::OutOfMemory)?;
    if components.is_empty() {
        out.push('/');
    }
    for component in components {
        out.push('/');
        out.push_str(component);
    }
    Ok(out)
}

/// A parsed symbolic-link target.
///
/// Unlike a [`Path`], a target is *not* required to be absolute and *may*
/// carry `.`/`..`: it is a POSIX link body, so it is whatever the link's
/// author stored — including one authored by a foreign system on a foreign
/// volume. The grammar is therefore looser than [`Path::parse`], and the
/// safety comes from *resolution* rather than from spelling: a `..` is
/// applied to the **walked node stack** (physically), never by collapsing
/// text before the walk, so a `..` after a link cannot be tricked into
/// naming a directory the walk never visited.
///
/// A `.` step is dropped at parse time: it is a no-op under both lexical
/// and physical resolution, so representing it would only add a variant
/// every consumer must handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkTarget {
    absolute: bool,
    steps: Vec<TargetStep>,
}

impl LinkTarget {
    /// Whether the target restarts from the floor of the link's own
    /// projection.
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        self.absolute
    }

    /// The target's steps, in order, owned.
    #[must_use]
    pub fn into_steps(self) -> Vec<TargetStep> {
        self.steps
    }
}

/// Parse a symbolic link's stored target.
///
/// The bytes are untrusted on-disk data, so every step is bounds- and
/// shape-checked and the whole target is refused on any failure — a
/// partially-parsed target is never resolved.
///
/// # Errors
///
/// Returns [`VfsError::InvalidPath`] if `text` is empty, longer than
/// [`tairix_abi::fs::FS_SYMLINK_MAX`], carries an over-long name, a NUL, or
/// more than [`MAX_PATH_COMPONENTS`] steps.
pub fn parse_link_target(text: &str) -> Result<LinkTarget, VfsError> {
    if text.is_empty() || text.len() > FS_SYMLINK_MAX {
        return Err(VfsError::InvalidPath);
    }
    let (absolute, rest) = match text.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let mut steps = Vec::new();
    for raw in rest.split('/') {
        // A trailing slash, a `//` run, and a `.` are all no-ops.
        if raw.is_empty() || raw == "." {
            continue;
        }
        if steps.len() == MAX_PATH_COMPONENTS {
            return Err(VfsError::InvalidPath);
        }
        if raw == ".." {
            steps.push(TargetStep::Up);
            continue;
        }
        validate_name(raw)?;
        steps.push(TargetStep::Name(raw.to_string()));
    }
    // `/` alone is a legal target (the root); a bare empty relative target
    // is not, and was rejected above.
    if steps.is_empty() && !absolute {
        return Err(VfsError::InvalidPath);
    }
    Ok(LinkTarget { absolute, steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_parses_and_is_root() {
        let p = Path::parse("/").expect("root parses");
        assert!(p.is_root());
        assert_eq!(p.depth(), 0);
        assert_eq!(p.top_level(), None);
        assert_eq!(p.file_name(), None);
        assert_eq!(p.parent(), None);
    }

    #[test]
    fn redundant_slashes_collapse() {
        let p = Path::parse("//System///Logs/").expect("collapses");
        assert_eq!(p.components(), &["System".to_string(), "Logs".to_string()]);
        assert_eq!(p.top_level(), Some("System"));
        assert_eq!(p.file_name(), Some("Logs"));
    }

    #[test]
    fn relative_path_is_rejected() {
        assert_eq!(Path::parse("System/x"), Err(VfsError::InvalidPath));
        assert_eq!(Path::parse(""), Err(VfsError::InvalidPath));
    }

    #[test]
    fn dot_and_dotdot_are_rejected() {
        assert_eq!(Path::parse("/System/.."), Err(VfsError::InvalidPath));
        assert_eq!(Path::parse("/./System"), Err(VfsError::InvalidPath));
    }

    #[test]
    fn nul_byte_component_is_rejected() {
        assert_eq!(Path::parse("/Sys\0tem"), Err(VfsError::InvalidPath));
    }

    #[test]
    fn over_long_component_is_rejected() {
        let mut s = String::from("/");
        s.push_str(&"a".repeat(MAX_COMPONENT_LEN + 1));
        assert_eq!(Path::parse(&s), Err(VfsError::InvalidPath));
    }

    #[test]
    fn too_many_components_is_rejected() {
        let mut s = String::new();
        for _ in 0..=MAX_PATH_COMPONENTS {
            s.push_str("/a");
        }
        assert_eq!(Path::parse(&s), Err(VfsError::InvalidPath));
    }

    #[test]
    fn parent_drops_the_last_component() {
        let p = Path::parse("/System/Logs/boot").expect("parses");
        let parent = p.parent().expect("has parent");
        assert_eq!(parent, Path::parse("/System/Logs").expect("parses"));
    }

    #[test]
    fn link_target_keeps_dot_dot_and_drops_dot() {
        let t = parse_link_target("a/./b/../c").expect("legal target");
        assert!(!t.is_absolute());
        assert_eq!(
            t.into_steps(),
            [
                TargetStep::Name("a".into()),
                TargetStep::Name("b".into()),
                TargetStep::Up,
                TargetStep::Name("c".into()),
            ]
        );
    }

    #[test]
    fn link_target_records_absoluteness() {
        assert!(parse_link_target("/System/x").expect("legal").is_absolute());
        assert!(!parse_link_target("System/x").expect("legal").is_absolute());
        // `/` alone is the root and is legal; a bare relative nothing is not.
        let root = parse_link_target("/").expect("root is a legal target");
        assert!(root.is_absolute() && root.into_steps().is_empty());
        assert_eq!(parse_link_target(""), Err(VfsError::InvalidPath));
        assert_eq!(parse_link_target("."), Err(VfsError::InvalidPath));
    }

    #[test]
    fn link_target_refuses_malformed_and_oversized_input() {
        // Untrusted on-disk bytes: every bound refuses the whole target.
        let long_name = "a".repeat(MAX_COMPONENT_LEN + 1);
        assert_eq!(parse_link_target(&long_name), Err(VfsError::InvalidPath));
        assert_eq!(parse_link_target("a\0b"), Err(VfsError::InvalidPath));
        let too_deep = (0..=MAX_PATH_COMPONENTS)
            .map(|_| "d")
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(parse_link_target(&too_deep), Err(VfsError::InvalidPath));
        let too_long = "/".to_string() + &"a/".repeat(FS_SYMLINK_MAX);
        assert_eq!(parse_link_target(&too_long), Err(VfsError::InvalidPath));
    }

    #[test]
    fn caller_supplied_paths_still_refuse_dot_tokens() {
        // The looser target grammar must not have relaxed the syscall
        // boundary: a caller's own path is as strict as it ever was.
        assert_eq!(Path::parse("/a/../b"), Err(VfsError::InvalidPath));
        assert_eq!(Path::parse("/a/./b"), Err(VfsError::InvalidPath));
        assert_eq!(Path::parse("a/b"), Err(VfsError::InvalidPath));
    }

    #[test]
    fn a_callers_path_keeps_each_dot_dot_as_a_step() {
        let path = parse_caller_path("/a/./b/../c").expect("parses");
        assert!(path.absolute);
        let steps: Vec<_> = path
            .steps
            .iter()
            .map(|step| (step.kind.clone(), step.from_link))
            .collect();
        assert_eq!(
            steps,
            [
                (TargetStep::Name("a".into()), false),
                (TargetStep::Name("b".into()), false),
                (TargetStep::Up, false),
                (TargetStep::Name("c".into()), false),
            ]
        );
        assert!(!parse_caller_path("../x").expect("relative").absolute);
        assert!(parse_caller_path(".").expect("here").steps.is_empty());
        for refused in [
            "System:/x",
            "alias::System/x",
            "id::b7f2e4e6-8d7a-4ef8-a13e-d3b84d4e8001/x",
            "a//b",
            "",
            "a/b:c",
        ] {
            assert_eq!(
                parse_caller_path(refused).map(|_| ()),
                Err(VfsError::InvalidPath),
                "{refused}"
            );
        }
        let deep = "a/".repeat(MAX_PATH_COMPONENTS + 1);
        assert_eq!(
            parse_caller_path(&deep).map(|_| ()),
            Err(VfsError::InvalidPath)
        );
    }

    #[test]
    fn prefix_is_component_wise() {
        let sys = Path::parse("/System").expect("parses");
        let logs = Path::parse("/System/Logs").expect("parses");
        let syslike = Path::parse("/SystemX").expect("parses");
        assert!(sys.is_prefix_of(&logs));
        assert!(Path::root().is_prefix_of(&logs));
        assert!(!logs.is_prefix_of(&sys));
        assert!(!sys.is_prefix_of(&syslike));
    }

    #[test]
    fn every_machine_alias_roots_at_its_own_top_level_component() {
        for name in ROOT_VIEW {
            assert_eq!(resolve_machine_alias(name), Some(name));
        }
    }

    #[test]
    fn unknown_machine_alias_fails_closed() {
        assert_eq!(resolve_machine_alias("Apps"), None, "there is no /Apps");
        assert_eq!(resolve_machine_alias("Home"), None);
        assert_eq!(resolve_machine_alias("system"), None);
        assert_eq!(resolve_machine_alias(""), None);
        assert_eq!(resolve_machine_alias("Storage2"), None);
    }
}
