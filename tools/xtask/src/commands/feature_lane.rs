//! The workspace packages a feature set turns features on in.
//!
//! The debug image switches its kernel diagnostics on at one package, and what
//! that switches on elsewhere is whatever the manifests say, so the host lane
//! testing those features is read from the manifests rather than kept beside
//! them, where it would rot (`plans/OPEN-DEFECTS.md` D556). The scene tier is
//! read the same way: whichever packages declare its feature.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::deps_check;

/// Each package's features, by package name.
pub(super) type Reach = BTreeMap<String, BTreeSet<String>>;

/// A workspace member's directory and manifest text, by package name.
pub(super) type Manifests = BTreeMap<String, (String, String)>;

/// Every workspace package `features` of `package` turn a feature on in,
/// `package` included, with the features turned on there.
///
/// # Errors
///
/// A manifest that cannot be read, or an entry naming a feature its package
/// does not declare or a dependency it does not have: a lane that lost a
/// package would test less than the image ships.
pub(super) fn reach(root: &Path, package: &str, features: &[&str]) -> Result<Reach, String> {
    reach_in(&workspace_manifests(root)?, package, features)
}

/// Every workspace package declaring `feature`, with what turning it on there
/// reaches.
///
/// # Errors
///
/// As [`reach`].
pub(super) fn declared(root: &Path, feature: &str) -> Result<Reach, String> {
    declared_in(&workspace_manifests(root)?, feature)
}

/// Every workspace member's directory and manifest, by package name.
pub(super) fn workspace_manifests(root: &Path) -> Result<Manifests, String> {
    let mut manifests = Manifests::new();
    for dir in deps_check::workspace_members(root)? {
        let path = root.join(&dir).join("Cargo.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("feature lane: cannot read {}: {e}", path.display()))?;
        if let Some(name) = deps_check::package_name(&text) {
            manifests.insert(name, (dir, text));
        }
    }
    Ok(manifests)
}

fn declared_in(manifests: &Manifests, feature: &str) -> Result<Reach, String> {
    let mut reached = Reach::new();
    for (package, (_, manifest)) in manifests {
        if feature_entries(manifest, feature).is_none() {
            continue;
        }
        for (name, features) in reach_in(manifests, package, &[feature])? {
            reached.entry(name).or_default().extend(features);
        }
    }
    Ok(reached)
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

    /// The features every package's defaults turn on, as far as they reach,
    /// each credited to the package it is turned on in.
    fn defaults_reach(manifests: &Manifests) -> Reach {
        let mut reached = Reach::new();
        for (package, (_, manifest)) in manifests {
            if feature_entries(manifest, "default").is_none() {
                continue;
            }
            for (package, features) in
                reach_in(manifests, package, &["default"]).expect("defaults reach")
            {
                reached.entry(package).or_default().extend(features);
            }
        }
        reached
    }

    /// The features a host pass builds each package with: its defaults, as
    /// far as they reach, and whatever a host lane turns on in it.
    fn built_features(root: &Path, manifests: &Manifests) -> Reach {
        let mut built = defaults_reach(manifests);
        let ctx = crate::Context {
            workspace_root: root.to_path_buf(),
            cargo: std::ffi::OsString::from("cargo"),
            children: std::sync::Arc::new(crate::children::Children::new(None)),
        };
        for lane in super::super::HostLane::ALL {
            for (package, features) in lane.reach(&ctx).expect("a lane's reach") {
                built.entry(package).or_default().extend(features);
            }
        }
        built
    }

    /// The root source of a target table in the crate at `dir`, in either of
    /// the layouts cargo finds an unnamed path in.
    fn target_root(
        dir: &Path,
        table: &deps_check::TargetTable,
        package: &str,
    ) -> std::path::PathBuf {
        if let Some(path) = &table.path {
            return dir.join(path);
        }
        let name = table.name.as_deref().unwrap_or(package);
        let base = match table.kind.as_str() {
            "test" => dir.join("tests"),
            "example" => dir.join("examples"),
            "bench" => dir.join("benches"),
            _ if name == package => return dir.join("src").join("main.rs"),
            _ => dir.join("src").join("bin"),
        };
        let flat = base.join(format!("{name}.rs"));
        if flat.exists() {
            flat
        } else {
            base.join(name).join("main.rs")
        }
    }

    /// The `#[path]` an attribute line names, if it is one.
    fn path_attribute(code: &str) -> Option<String> {
        code.strip_prefix("#[")?
            .split_once(']')?
            .0
            .trim()
            .strip_prefix("path")?
            .trim_start()
            .strip_prefix('=')
            .and_then(deps_check::string_literal)
    }

    /// The module a `mod name;` line declares, past its attributes and
    /// visibility, with a `#[path]` on the same line if any.
    fn declared_module(line: &str) -> Option<(&str, Option<String>)> {
        let mut code = line.trim_start();
        let mut path = None;
        while code.starts_with("#[") {
            path = path_attribute(code).or(path);
            code = code.split_once(']')?.1.trim_start();
        }
        if let Some(rest) = code.strip_prefix("pub") {
            code = rest.trim_start();
            if let Some(scoped) = code.strip_prefix('(') {
                code = scoped.split_once(')')?.1.trim_start();
            }
        }
        let name = code.strip_prefix("mod ")?.strip_suffix(';')?.trim();
        name.chars()
            .all(|c| c.is_alphanumeric() || c == '_')
            .then_some((name, path))
    }

    /// Whether the module tree rooted at `file` holds a `#[test]`: `file`, and
    /// each `mod name;` it declares, followed as the compiler follows it,
    /// `#[path]` on its own line included.
    ///
    /// A file it cannot read is an error, not an answer: the guard must not
    /// pass on a tree it never looked at.
    fn holds_a_test(file: &Path, is_root: bool) -> Result<bool, String> {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        if text.lines().any(|line| {
            let code = line.trim_start();
            code.starts_with('#') && code.contains("#[test]")
        }) {
            return Ok(true);
        }
        let stem = file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("");
        let parent = file
            .parent()
            .ok_or_else(|| format!("{}: no directory", file.display()))?;
        let children = if is_root || stem == "mod" {
            parent.to_path_buf()
        } else {
            parent.join(stem)
        };
        let mut pending = None;
        for line in text.lines() {
            let code = line.trim_start();
            if let Some((name, path)) = declared_module(code) {
                let source = if let Some(path) = path.or_else(|| pending.take()) {
                    parent.join(path)
                } else {
                    let flat = children.join(format!("{name}.rs"));
                    if flat.exists() {
                        flat
                    } else {
                        children.join(name).join("mod.rs")
                    }
                };
                if holds_a_test(&source, false)? {
                    return Ok(true);
                }
            } else if code.starts_with("#[") {
                pending = path_attribute(code).or(pending);
            } else if !code.is_empty() && !code.starts_with("//") {
                pending = None;
            }
        }
        Ok(false)
    }

    /// A target only a feature no host pass turns on builds is skipped by
    /// every one, so it may hold no test: one would run nowhere.
    #[test]
    fn a_target_no_host_pass_builds_holds_no_test() {
        let root = crate::workspace_root_from_manifest_dir().expect("the workspace root");
        let manifests = workspace_manifests(&root).expect("the workspace manifests");
        let built = built_features(&root, &manifests);
        let mut unbuilt = Vec::new();
        for (package, (dir, text)) in &manifests {
            let crate_dir = root.join(dir);
            for table in deps_check::target_tables(text) {
                let builds = table
                    .required_features
                    .iter()
                    .all(|feature| built.get(package).is_some_and(|on| on.contains(feature)));
                if builds {
                    continue;
                }
                match holds_a_test(&target_root(&crate_dir, &table, package), true) {
                    Ok(false) => {}
                    Ok(true) => unbuilt.push(format!("{dir}: {} {:?}", table.kind, table.name)),
                    Err(why) => unbuilt.push(format!(
                        "{dir}: {} {:?} unread: {why}",
                        table.kind, table.name
                    )),
                }
            }
        }
        assert!(unbuilt.is_empty(), "tests no host pass builds: {unbuilt:?}");
    }

    #[test]
    fn a_tests_module_tree_is_followed_from_its_root() {
        let scratch =
            tairix_fuzzseed::scratch::Scratch::under(&std::env::temp_dir(), "tairix-lane-tree");
        let src = scratch.path().join("src");
        std::fs::create_dir_all(src.join("deep")).expect("dirs");
        std::fs::write(src.join("main.rs"), "mod deep;\nfn main() {}\n").expect("root");
        std::fs::write(src.join("deep").join("mod.rs"), "pub(crate) mod leaf;\n").expect("mod");
        std::fs::write(src.join("deep").join("leaf.rs"), "fn f() {}\n").expect("leaf");
        assert_eq!(holds_a_test(&src.join("main.rs"), true), Ok(false));
        std::fs::write(src.join("deep").join("leaf.rs"), "#[test]\nfn t() {}\n").expect("leaf");
        assert_eq!(holds_a_test(&src.join("main.rs"), true), Ok(true));

        std::fs::write(
            src.join("elsewhere.rs"),
            "#[cfg(test)] #[test]\nfn t() {}\n",
        )
        .expect("away");
        std::fs::write(
            src.join("lib.rs"),
            "#[cfg(test)]\n#[path = \"elsewhere.rs\"]\nmod moved;\n",
        )
        .expect("lib");
        assert_eq!(
            holds_a_test(&src.join("lib.rs"), true),
            Ok(true),
            "#[path] on its own line"
        );

        std::fs::write(src.join("gone.rs"), "mod absent;\n").expect("dangling");
        assert!(
            holds_a_test(&src.join("gone.rs"), true).is_err(),
            "unread is not none"
        );
    }

    /// A dependency's feature a package's defaults turn on is credited to the
    /// dependency, never to the package that asked for it.
    #[test]
    fn a_default_reaching_a_dependency_credits_the_dependency() {
        let manifests = manifests(&[
            ("p", "lib/p", "[features]\ndefault = [\"q/extra\"]\nextra = []\n\n[dependencies]\nq = { path = \"../q\" }\n"),
            ("q", "lib/q", "[features]\nextra = []\n"),
        ]);
        let built = defaults_reach(&manifests);
        assert!(!built["p"].contains("extra"), "p's own extra stays off");
        assert!(built["q"].contains("extra"), "q's is on");
    }

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

    fn listed(reach: &Reach) -> Vec<(&str, Vec<&str>)> {
        reach
            .iter()
            .map(|(name, features)| (name.as_str(), features.iter().map(String::as_str).collect()))
            .collect()
    }

    /// What the kernel's `diagnostics` turns on in the synthetic workspace.
    const KERNEL_DIAGNOSTICS: [(&str, &[&str]); 4] = [
        ("arch", &["diagnostics"]),
        ("core", &["diagnostics"]),
        ("kernel", &["diagnostics", "trace"]),
        ("sync", &["locks"]),
    ];

    #[test]
    fn a_feature_reaches_every_package_its_entries_name() {
        let reach = reach_in(&workspace(), "kernel", &["diagnostics"]).expect("reach");
        assert_eq!(
            listed(&reach),
            KERNEL_DIAGNOSTICS.map(|(n, f)| (n, f.to_vec()))
        );
    }

    /// Every package declaring a feature joins its lane, with what it turns
    /// on elsewhere; a feature no package declares makes an empty lane.
    #[test]
    fn a_declared_feature_gathers_every_package_declaring_it() {
        let declared = declared_in(&workspace(), "diagnostics").expect("declared");
        assert_eq!(
            listed(&declared),
            KERNEL_DIAGNOSTICS.map(|(n, f)| (n, f.to_vec()))
        );
        let locks = declared_in(&workspace(), "locks").expect("declared");
        assert_eq!(listed(&locks), [("sync", vec!["locks"])]);
        assert!(declared_in(&workspace(), "absent")
            .expect("declared")
            .is_empty());
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
        let ctx = crate::Context::discover(None).expect("workspace context");
        let features: Vec<&str> = super::super::KERNEL_DIAGNOSTICS_FEATURES
            .split(',')
            .collect();
        let reach =
            reach(&ctx.workspace_root, super::super::KERNEL_PACKAGE, &features).expect("reach");
        for (package, feature) in [
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

    /// The scene tier `--scenes` runs: the ray tracer's whole-scene tests and
    /// its screensaver's.
    #[test]
    fn the_scene_tier_holds_the_ray_tracer_and_its_screensaver() {
        let ctx = crate::Context::discover(None).expect("workspace context");
        let feature = super::super::SCENE_TESTS_FEATURE;
        let tier = declared(&ctx.workspace_root, feature).expect("declared");
        for package in ["tairix-raytrace", "tairix-saver-raytrace"] {
            assert!(
                tier.get(package)
                    .is_some_and(|features| features.contains(feature)),
                "{package}/{feature} missing from {tier:?}"
            );
        }
    }
}
