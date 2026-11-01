//! `tools/ci/soak.sh` driven against a stand-in `cargo`, in an environment of
//! its own: it plans and launches from every registry's listing, and refuses
//! what it cannot soak before any job starts.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tairix_fuzzseed::scratch::Scratch;

/// Lists `STUB_TARGETS` targets for each registry (one by default), fails the
/// registries `STUB_FAILS` names and lists none for those `STUB_EMPTY` names.
/// Every other call drains its stdin, as a job reading it would, and is
/// recorded.
const STUB_CARGO: &str = r#"#!/bin/sh
if [ "$1" = xtask ] && [ "$3" = --list ]; then
    case " ${STUB_FAILS-} " in *" $2 "*) exit 3 ;; esac
    case " ${STUB_EMPTY-} " in *" $2 "*) exit 0 ;; esac
    n=0
    while [ "$n" -lt "${STUB_TARGETS:-1}" ]; do
        n=$((n + 1))
        echo "$2-target$n  one $2 target  [a-package]"
    done
    exit 0
fi
cat >/dev/null
echo "$*" >>"$STUB_CALLS"
"#;

/// A scratch checkout, log directory and `cargo` for one run of the script.
struct Fixture {
    scratch: Scratch,
}

impl Fixture {
    fn new() -> Self {
        let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "soak-script");
        for dir in ["bin", "repo"] {
            std::fs::create_dir(scratch.path().join(dir)).expect("a fixture directory");
        }
        let cargo = scratch.path().join("bin/cargo");
        std::fs::write(&cargo, STUB_CARGO).expect("the stand-in cargo");
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755))
            .expect("the stand-in cargo runs");
        Self { scratch }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.scratch.path().join(name)
    }

    /// `soak.sh args`, with `env` beside the fixture's own environment.
    fn soak(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ci/soak.sh");
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("bash");
        cmd.arg(script)
            .args(args)
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.scratch.path())
            .env("CARGO_HOME", self.scratch.path())
            .env("TAIRIX_CI_REPO", self.path("repo"))
            .env("TAIRIX_CI_LOGDIR", self.path("logs"))
            .env("TAIRIX_CI_SYNC", "0")
            .env("CARGO_TARGET_DIR", self.path("target"))
            .env("STUB_CALLS", self.path("calls"))
            .envs(env.iter().copied())
            .stdin(Stdio::null());
        cmd.output().expect("bash runs the soak script")
    }

    /// The calls the stand-in recorded, a job's launch among them.
    fn calls(&self) -> String {
        std::fs::read_to_string(self.path("calls")).unwrap_or_default()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The control the refusals below lean on: the stand-in reaches the planner,
/// so a refusal is the script's and not a missing tool's.
#[test]
fn a_soak_plans_one_job_per_listed_target() {
    let fixture = Fixture::new();
    let out = fixture.soak(&["both", "--secs", "20", "--dry-run"], &[]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
    assert!(stdout.contains("fuzz-fuzz-target1"), "{stdout}");
    assert!(stdout.contains("proptest-proptest-target1"), "{stdout}");
    assert!(stdout.contains("2 job(s) planned"), "{stdout}");
}

#[test]
fn a_listing_that_fails_stops_the_soak_before_any_job_starts() {
    let fixture = Fixture::new();
    let out = fixture.soak(&["both", "--secs", "20"], &[("STUB_FAILS", "proptest")]);
    let stderr = text(&out.stderr);
    assert!(!out.status.success(), "{}", text(&out.stdout));
    assert!(stderr.contains("cargo xtask proptest --list"), "{stderr}");
    assert_eq!(fixture.calls(), "", "no job was started");
}

#[test]
fn a_listing_that_names_no_target_is_refused() {
    let fixture = Fixture::new();
    let out = fixture.soak(&["fuzz", "--dry-run"], &[("STUB_EMPTY", "fuzz")]);
    let stderr = text(&out.stderr);
    assert!(!out.status.success(), "{}", text(&out.stdout));
    assert!(stderr.contains("named no target"), "{stderr}");
}

#[test]
fn a_duration_that_is_not_whole_seconds_is_refused() {
    let fixture = Fixture::new();
    for flag in ["--secs", "--window"] {
        for value in ["", "5x", "-1", "1234567890123456789"] {
            let out = fixture.soak(&["fuzz", flag, value, "--dry-run"], &[]);
            let stderr = text(&out.stderr);
            assert_eq!(out.status.code(), Some(2), "{flag} {value:?}: {stderr}");
            assert!(stderr.contains(flag), "{flag} {value:?}: {stderr}");
        }
    }
    assert_eq!(fixture.calls(), "");
}

/// A sequential job's stdin is not the rest of the listing, so a job that
/// reads its stdin cannot swallow the targets after it.
#[test]
fn a_sequential_job_cannot_swallow_the_rest_of_the_listing() {
    let fixture = Fixture::new();
    let out = fixture.soak(
        &["fuzz", "--sequential", "--secs", "20"],
        &[("STUB_TARGETS", "2")],
    );
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
    let calls = fixture.calls();
    for target in ["fuzz-target1", "fuzz-target2"] {
        assert!(calls.contains(&format!("--target {target}")), "{calls}");
    }
    assert!(stdout.contains("2 job(s), 0 failed"), "{stdout}");
}

/// The seconds of soak.sh's one definition of the scheduled window.
fn scheduled_window() -> u64 {
    let script =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../ci/soak.sh"))
            .expect("soak.sh is readable");
    script
        .lines()
        .find_map(|line| line.strip_prefix("scheduled_window="))
        .and_then(|value| value.parse().ok())
        .expect("soak.sh defines the scheduled window")
}

/// A scheduled soak runs the one scheduled window, or a shorter window a
/// dispatch asks for, and never a longer one its schedulers are not sized to.
#[test]
fn a_scheduled_soak_runs_its_window_and_refuses_a_longer_one() {
    let fixture = Fixture::new();
    let window = scheduled_window();
    let out = fixture.soak(&["fuzz", "--scheduled", "--dry-run"], &[]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
    assert!(stdout.contains(&format!("{window} s window")), "{stdout}");

    let shorter = (window - 1).to_string();
    let out = fixture.soak(
        &["fuzz", "--scheduled", "--window", &shorter, "--dry-run"],
        &[],
    );
    assert!(text(&out.stdout).contains(&format!("{shorter} s window")));

    let longer = (window + 1).to_string();
    let out = fixture.soak(
        &["fuzz", "--scheduled", "--window", &longer, "--dry-run"],
        &[],
    );
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("scheduled window"), "{stderr}");
    assert_eq!(fixture.calls(), "");
}
