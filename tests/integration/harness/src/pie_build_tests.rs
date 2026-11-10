use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};

use super::{build_root, directory_name, PieBuild, Recipe};
use crate::pie::{self, PieArch, Profile};

/// A fresh scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tairix-pie-build-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// A cargo build root: a directory holding cargo's cache tag.
fn cargo_root(name: &str) -> PathBuf {
    let root = scratch(name);
    fs::write(
        root.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .expect("tag the root");
    root
}

fn program<'a>(root: &'a Path, floor: &'a [String]) -> PieBuild<'a> {
    PieBuild {
        root,
        workspace: Path::new("."),
        arch: PieArch::Aarch64,
        recipe: Recipe::Program,
        profile: Profile::Dev,
        floor,
        env: &[],
    }
}

/// Every build script of a freestanding target — any fixture, any kernel
/// unit — finds the one root its units share, so all of them build in the
/// same shared directory rather than one each.
#[test]
fn every_build_script_of_a_target_finds_the_one_root() {
    let root = cargo_root("root");
    let triple = PieArch::Riscv64.target_triple();
    for unit in ["tairix-kernel-0123", "tairix-test-wait-qemu-riscv64-4567"] {
        for profile in ["debug", "release"] {
            let out = root
                .join(triple)
                .join(profile)
                .join("build")
                .join(unit)
                .join("out");
            assert_eq!(
                build_root(&out, triple),
                Ok(root.clone()),
                "{}",
                out.display()
            );
        }
    }
    let host = root
        .join("debug")
        .join("build")
        .join("xtask-89ab")
        .join("out");
    assert_eq!(
        build_root(&host, "x86_64-unknown-linux-gnu"),
        Ok(root.clone())
    );
    let _ = fs::remove_dir_all(&root);
}

/// A directory outside cargo's layout, or a root cargo never tagged, is
/// refused rather than guessed at.
#[test]
fn a_root_outside_cargos_layout_or_untagged_is_refused() {
    let untagged = scratch("untagged");
    let triple = PieArch::Aarch64.target_triple();
    let out = untagged
        .join(triple)
        .join("debug")
        .join("build")
        .join("unit")
        .join("out");
    assert!(build_root(&out, triple).is_err());
    let elsewhere = untagged
        .join(triple)
        .join("debug")
        .join("deps")
        .join("unit")
        .join("out");
    assert!(build_root(&elsewhere, triple).is_err());
    assert!(build_root(Path::new("/"), triple).is_err());
    let _ = fs::remove_dir_all(&untagged);
}

/// The kernel's embedded programs, the fixtures' and the image pipeline's on
/// an empty floor are one configuration, so they share one directory; any
/// flag, sysroot or unfingerprinted content that differs names another.
#[test]
fn a_directory_is_named_for_its_whole_configuration() {
    let root = Path::new("/target");
    let named = |build: &PieBuild<'_>| build.name().expect("the script and spec are readable");
    let base = named(&program(root, &[]));
    assert_eq!(base, named(&program(root, &[])));
    let hash = base
        .strip_prefix("aarch64-unknown-none-")
        .expect("named after its triple");
    assert!(hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let floor = [String::from("-C"), String::from("target-cpu=x86-64")];
    assert_ne!(base, named(&program(root, &floor)));
    let archive = PieBuild {
        recipe: Recipe::Archive,
        ..program(root, &[])
    };
    assert_ne!(base, named(&archive));
    for arch in [PieArch::Riscv64, PieArch::X86_64] {
        let other = PieBuild {
            arch,
            ..program(root, &[])
        };
        assert_ne!(
            base,
            named(&other),
            "{arch:?} builds in a directory of its own"
        );
    }
    let release = PieBuild {
        profile: Profile::Release,
        ..program(root, &[])
    };
    assert_eq!(
        base,
        named(&release),
        "cargo keeps each profile apart itself"
    );
}

/// Content cargo fingerprints only by path — a link script edited in place —
/// names a new directory, and the framing keeps two fields from running into
/// one another.
#[test]
fn unfingerprinted_content_and_field_boundaries_name_new_directories() {
    let script = |content: &[u8]| vec![content.to_vec()];
    let flags = "-C relocation-model=pie";
    let std = ["-Z", "build-std=core"];
    let name = |std: &[&str], contents: &[Vec<u8>]| directory_name("t", flags, std, contents);
    let base = name(&std, &script(b"SECTIONS {}"));
    assert_eq!(base, name(&std, &script(b"SECTIONS {}")));
    assert_ne!(base, name(&std, &script(b"SECTIONS { . = 0x10000; }")));
    assert_ne!(
        name(&["-Zbuild", "-std=core"], &[]),
        name(&["-Zbuild-", "std=core"], &[])
    );
}

/// The lock is exclusive: while one build holds a configuration's directory,
/// another cannot take it, and dropping the hold lets it in.
#[test]
fn a_held_directory_cannot_be_taken_until_it_is_let_go() {
    let root = scratch("lock");
    let build = program(&root, &[]);
    let held = build.lock().expect("take the directory");
    let lock = root
        .join("guest")
        .join(format!("{}.lock", build.name().expect("named")));
    let other = File::options()
        .write(true)
        .open(&lock)
        .expect("open the lock");
    assert!(matches!(other.try_lock(), Err(TryLockError::WouldBlock)));
    drop(held);
    assert!(other.try_lock().is_ok(), "let go, it can be taken");
    let _ = fs::remove_dir_all(&root);
}

/// The command builds in the shared directory, locked to the lockfile, for
/// the target and sysroot the recipe needs, with the recipe's flags scoped to
/// the target and the guests' environment.
#[test]
fn the_command_builds_in_the_shared_directory_with_the_recipes_flags() {
    let root = scratch("command");
    let env = [("TAIRIX_PROBE_ROUNDS", String::from("3"))];
    let build = PieBuild {
        env: &env,
        ..program(&root, &[])
    };
    let held = build.lock().expect("take the directory");
    let command = held.command(&["tairix-a", "tairix-b"], &["a-run", "b-run"], &["x", "y"]);
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let dir = held.dir().to_string_lossy().into_owned();
    for expected in [
        &["build", "--locked", "--target", "aarch64-unknown-none"][..],
        &["-Z", pie::SYSROOT_BUILD_STD],
        &["--target-dir", &dir],
        &[
            "-p", "tairix-a", "-p", "tairix-b", "--bin", "a-run", "--bin", "b-run",
        ],
        &["--features", "x,y"],
    ] {
        assert!(
            args.windows(expected.len())
                .any(|window| window == expected),
            "{expected:?} in {args:?}"
        );
    }
    assert!(!args.iter().any(|arg| arg == "--release"));
    let envs: Vec<_> = command.get_envs().collect();
    let flags = envs
        .iter()
        .find(|(key, _)| *key == PieArch::Aarch64.rustflags_env_var())
        .and_then(|(_, value)| *value)
        .expect("the target-scoped flags")
        .to_string_lossy()
        .into_owned();
    let script = format!("-C link-arg=-T{}", pie::run_ld().display());
    assert!(
        flags.starts_with("-C relocation-model=pie") && flags.ends_with(&script),
        "{flags}"
    );
    let set = |name: &str, to: &str| {
        envs.iter()
            .any(|(key, value)| *key == name && value.is_some_and(|value| value == to))
    };
    assert!(set("TAIRIX_PROBE_ROUNDS", "3"));
    assert!(set("CARGO_INCREMENTAL", "0"));
    assert!(dir.starts_with(&root.join("guest").to_string_lossy().into_owned()));
    drop(held);
    let _ = fs::remove_dir_all(&root);
}

/// A program is its binary and an archive its package's static library, each
/// beneath the target and profile cargo writes it under.
#[test]
fn an_artefact_lies_where_cargo_writes_it() {
    let root = scratch("artefact");
    let build = PieBuild {
        profile: Profile::Release,
        ..program(&root, &[])
    };
    let held = build.lock().expect("take the directory");
    assert_eq!(
        held.artefact("tairix-init-run"),
        held.dir()
            .join("aarch64-unknown-none")
            .join("release")
            .join("tairix-init-run")
    );
    drop(held);
    let archive = PieBuild {
        recipe: Recipe::Archive,
        ..program(&root, &[])
    };
    let held = archive.lock().expect("take the directory");
    assert_eq!(
        held.artefact("tairix-test-cc5-program"),
        held.dir()
            .join("aarch64-unknown-none")
            .join("debug")
            .join("libtairix_test_cc5_program.a")
    );
    drop(held);
    let _ = fs::remove_dir_all(&root);
}

/// No build script runs a cross-compile of its own: every one builds through
/// [`PieBuild`], so none compiles the sysroot into a tree of its own.
#[test]
fn no_build_script_builds_into_a_target_directory_of_its_own() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("the workspace root");
    let mut scripts: Vec<PathBuf> = fs::read_dir(workspace.join("tests").join("integration"))
        .expect("read tests/integration")
        .filter_map(|entry| Some(entry.ok()?.path().join("build.rs")))
        .filter(|script| script.is_file())
        .collect();
    scripts.push(
        workspace
            .join("kernel")
            .join("tairix-kernel")
            .join("build.rs"),
    );
    let offenders: Vec<String> = scripts
        .iter()
        .filter(|script| {
            fs::read_to_string(script)
                .is_ok_and(|source| source.contains("--target-dir") || source.contains("build-std"))
        })
        .map(|script| script.display().to_string())
        .collect();
    assert!(scripts.len() > 1, "the build scripts were found");
    assert!(
        offenders.is_empty(),
        "these build scripts cross-compile outside PieBuild: {offenders:#?}"
    );
}
