//! The workspace packages a feature set turns features on in.
//!
//! The debug image switches its kernel diagnostics on at one package, and what
//! that switches on elsewhere is whatever the manifests say, so the host lane
//! testing those features is read from the manifests rather than kept beside
//! them, where it would rot (`plans/OPEN-DEFECTS.md` D556).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::deps_check;

/// Each package's features, by package name.
pub(super) type Reach = BTreeMap<String, BTreeSet<String>>;

/// A workspace member's directory and manifest text, by package name.
type Manifests = BTreeMap<String, (String, String)>;

/// Every workspace package `features` of `package` turn a feature on in,
/// `package` included, with the features turned on there.
///
/// # Errors
///
/// A manifest that cannot be read, or an entry naming a feature its package
/// does not declare or a dependency it does not have: a lane that lost a
/// package would test less than the image ships.
pub(super) fn reach(root: &Path, package: &str, features: &[&str]) -> Result<Reach, String> {
    let mut manifests = Manifests::new();
    for dir in deps_check::workspace_members(root)? {
        let path = root.join(&dir).join("Cargo.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("feature lane: cannot read {}: {e}", path.display()))?;
        if let Some(name) = deps_check::package_name(&text) {
            manifests.insert(name, (dir, text));
        }
    }
    reach_in(&manifests, package, features)
}

fn reach_in(manifests: &Manifests, package: &str, features: &[&str]) -> Result<Reach, String> {
    let by_dir: BTreeMap<&str, &str> = manifests
        .iter()
        .map(|(name, (dir, _))| (dir.as_str(), name.as_str()))
        .collect();
    let mut reached = Reach::new();
    let mut pending: Vec<(String, String)> = features
        .iter()
        .map(|feature| (package.to_string(), (*feature).to_string()))
        .collect();
    while let Some((package, feature)) = pending.pop() {
        let (dir, manifest) = manifests
            .get(&package)
            .ok_or_else(|| format!("feature lane: `{package}` is not a workspace member"))?;
        if !reached
            .entry(package.clone())
            .or_default()
            .insert(feature.clone())
        {
            continue;
        }
        let entries = feature_entries(manifest, &feature)
            .ok_or_else(|| format!("feature lane: `{package}` declares no feature `{feature}`"))?;
        for entry in entries {
            if entry.starts_with("dep:") {
                continue;
            }
            let Some((dependency, feature)) = entry.split_once('/') else {
                pending.push((package.clone(), entry));
                continue;
            };
            let dependency = dependency.trim_end_matches('?');
            let source = dependency_source(manifest, dir, dependency).ok_or_else(|| {
                format!("feature lane: `{package}` has no dependency `{dependency}`")
            })?;
            // A dependency outside the workspace has no tests of ours to run.
            if let Source::Path(target) = source {
                if let Some(name) = by_dir.get(target.as_str()) {
                    pending.push(((*name).to_string(), feature.to_string()));
                }
            }
        }
    }
    Ok(reached)
}

/// The entries of `feature` in `manifest`'s `[features]` table; [`None`]
/// where it declares no such feature.
fn feature_entries(manifest: &str, feature: &str) -> Option<Vec<String>> {
    let mut in_features = false;
    let mut entries: Option<Vec<String>> = None;
    for line in manifest.lines() {
        let code = line.split('#').next().unwrap_or("").trim();
        if let Some(collected) = entries.as_mut() {
            if push_literals(code, collected) {
                return entries;
            }
            continue;
        }
        if code.starts_with('[') {
            in_features = code == "[features]";
            continue;
        }
        let Some((key, value)) = code.split_once('=').filter(|_| in_features) else {
            continue;
        };
        if key.trim() != feature {
            continue;
        }
        let mut collected = Vec::new();
        if push_literals(value.trim().strip_prefix('[')?, &mut collected) {
            return Some(collected);
        }
        entries = Some(collected);
    }
    None
}

/// Push each string literal of an array fragment, answering whether the
/// fragment closed the array.
fn push_literals(fragment: &str, entries: &mut Vec<String>) -> bool {
    let (body, closed) = fragment
        .find(']')
        .map_or((fragment, false), |at| (&fragment[..at], true));
    entries.extend(body.split(',').filter_map(deps_check::string_literal));
    closed
}

/// Where a declared dependency's sources are.
enum Source {
    /// The workspace-relative directory its `path` names.
    Path(String),
    /// A registry or git dependency: no `path`.
    Elsewhere,
}

/// Where dependency `key` of the manifest in `dir` is; [`None`] for no such
/// dependency.
fn dependency_source(manifest: &str, dir: &str, key: &str) -> Option<Source> {
    let mut in_dependencies = false;
    for line in manifest.lines() {
        let code = line.split('#').next().unwrap_or("").trim();
        if code.starts_with('[') {
            in_dependencies = code.trim_matches(['[', ']']).ends_with("dependencies");
            continue;
        }
        let Some((name, value)) = code.split_once('=').filter(|_| in_dependencies) else {
            continue;
        };
        if name.trim() == key {
            let path = deps_check::extract_path_value(value)
                .and_then(|path| deps_check::normalize_join(dir, &path));
            return Some(path.map_or(Source::Elsewhere, Source::Path));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifests(members: &[(&str, &str, &str)]) -> Manifests {
        members
            .iter()
            .map(|(name, dir, text)| {
                (
                    (*name).to_string(),
                    ((*dir).to_string(), (*text).to_string()),
                )
            })
            .collect()
    }

    const KERNEL: &str = r#"
[package]
name = "kernel"

[features]
# A comment naming [features] and "quoted/words" is not an entry.
diagnostics = [
    "core/diagnostics", # inline
    "arch?/diagnostics",
    "trace",
]
trace = []
optional = ["dep:arch"]

[dependencies]
core = { path = "../core" }
external = "1"

[target.'cfg(target_arch = "aarch64")'.dependencies]
arch = { path = "../arch", optional = true }
"#;

    const CORE: &str = r#"
[package]
name = "core"

[features]
diagnostics = ["sync/locks", "external/feature"]

[dependencies]
sync = { path = "../../lib/sync", version = "=0.0.0" }
external = "1"
"#;

    const SYNC: &str = "[package]\nname = \"sync\"\n\n[features]\nlocks = []\n";
    const ARCH: &str = "[package]\nname = \"arch\"\n\n[features]\ndiagnostics = []\n";

    fn workspace() -> Manifests {
        manifests(&[
            ("kernel", "kernel/kernel", KERNEL),
            ("core", "kernel/core", CORE),
            ("arch", "kernel/arch", ARCH),
            ("sync", "lib/sync", SYNC),
        ])
    }

    #[test]
    fn a_feature_reaches_every_package_its_entries_name() {
        let reach = reach_in(&workspace(), "kernel", &["diagnostics"]).expect("reach");
        let expected: Vec<(&str, Vec<&str>)> = vec![
            ("arch", vec!["diagnostics"]),
            ("core", vec!["diagnostics"]),
            ("kernel", vec!["diagnostics", "trace"]),
            ("sync", vec!["locks"]),
        ];
        let got: Vec<(&str, Vec<&str>)> = reach
            .iter()
            .map(|(name, features)| (name.as_str(), features.iter().map(String::as_str).collect()))
            .collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn an_optional_dependency_activation_names_no_feature() {
        let reach = reach_in(&workspace(), "kernel", &["optional"]).expect("reach");
        assert_eq!(
            reach.keys().map(String::as_str).collect::<Vec<_>>(),
            ["kernel"]
        );
    }

    #[test]
    fn an_undeclared_feature_or_dependency_fails_the_lane() {
        assert!(reach_in(&workspace(), "kernel", &["absent"]).is_err());
        let broken = manifests(&[(
            "kernel",
            "kernel/kernel",
            "[package]\nname = \"kernel\"\n[features]\nf = [\"missing/g\"]\n",
        )]);
        assert!(reach_in(&broken, "kernel", &["f"]).is_err());
        assert!(reach_in(&workspace(), "elsewhere", &["diagnostics"]).is_err());
    }

    #[test]
    fn a_single_line_array_closes_on_its_own_line() {
        assert_eq!(
            feature_entries("[features]\nf = [\"a\", \"b/c\"]\ng = [\"d\"]\n", "f"),
            Some(vec!["a".to_string(), "b/c".to_string()])
        );
        assert_eq!(
            feature_entries("[features]\nf = []\n", "f"),
            Some(Vec::new())
        );
        assert_eq!(feature_entries("[package]\nf = [\"a\"]\n", "f"), None);
    }

    /// The lane the gate runs: the debug image's kernel diagnostics reach the
    /// core, the locks and the heap it instruments.
    #[test]
    fn the_debug_image_diagnostics_reach_the_core_and_its_locks() {
        let ctx = crate::Context::discover().expect("workspace context");
        let features: Vec<&str> = super::super::KERNEL_DIAGNOSTICS_FEATURES
            .split(',')
            .collect();
        let reach =
            reach(&ctx.workspace_root, super::super::KERNEL_PACKAGE, &features).expect("reach");
        for (package, feature) in [
            ("tairix-kernel", "storage-trace"),
            ("tairix-kernel-core", "watchdog-diagnostics"),
            ("tairix-sync", "lock-diagnostics"),
            ("tairix-kalloc", "lock-diagnostics"),
        ] {
            assert!(
                reach
                    .get(package)
                    .is_some_and(|features| features.contains(feature)),
                "{package}/{feature} missing from {reach:?}"
            );
        }
    }
}
