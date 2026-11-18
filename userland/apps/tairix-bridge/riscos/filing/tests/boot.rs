//! The boot sequence on the user's own disc, served by HostFS: `!Boot` laid
//! out where the disc has none, the boot and choices variables set, then
//! `PreDesktop` obeyed and each object in `PreDesk` run, every failure kept
//! and the boot going on, as a program it starts exits.

mod common;

use common::served::{session, session_with};
use common::{run_out, variable, EXITS};

use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::boot;
use tairix_riscos_kernel::os::{Ended, Fail, Os, Outcome, Unwind};

/// The own disc's folder and the disc's name.
const HOME: (&str, &str) = ("/home", "Home");
const BOOT_DIR: &str = "/home/!Boot";
const PRE_DESK: &str = "/home/!Boot/Choices/Boot/PreDesk";

/// `name`'s value, as text.
fn value(os: &mut Os<'_>, name: &[u8]) -> Option<String> {
    variable(os, name).map(|value| String::from_utf8_lossy(&value).into_owned())
}

/// An own disc holding a boot application with an empty `PreDesk`.
fn booted_disc() -> Tree {
    Tree::new().dir(PRE_DESK)
}

#[test]
fn a_disc_with_no_boot_application_is_given_one_and_the_boot_variables() {
    let tree = Tree::new().dir("/home");
    let mut session = session_with(&tree, Some(HOME), &[]);
    let mut os = session.kernel.os();
    let report = boot::start(&mut os).expect("booted");
    assert_eq!(report.failures, []);
    assert_eq!(tree.names(BOOT_DIR), ["Choices", "Library", "Resources"]);
    assert_eq!(tree.names("/home/!Boot/Choices"), ["Boot"]);
    assert_eq!(tree.names("/home/!Boot/Choices/Boot"), ["PreDesk", "Tasks"]);
    for (name, expected) in [
        (&b"Boot$Dir"[..], "HostFS::Home.$.!Boot"),
        (b"Boot$Path", "HostFS::Home.$.!Boot."),
        (b"Boot$OSVersion", "530"),
        (b"Boot$State", "commands"),
        (b"Boot$Unique", "Local"),
        (b"Boot$ToBeLoaded", "Boot:Choices.Boot.PreDesk"),
        (b"Boot$ToBeTasks", "Boot:Choices.Boot.Tasks"),
        (b"Choices$Dir", "HostFS::Home.$.!Boot.Choices"),
        (b"Choices$Write", "HostFS::Home.$.!Boot.Choices"),
        (b"Choices$Path", "HostFS::Home.$.!Boot.Choices."),
    ] {
        assert_eq!(
            value(&mut os, name).as_deref(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(name)
        );
    }
    let run_path = value(&mut os, b"Run$Path").unwrap_or_default();
    assert!(run_path.ends_with(",Boot:Library."), "{run_path}");
}

#[test]
fn a_boot_application_the_disc_has_is_kept_as_it_is() {
    let tree = Tree::new()
        .dir(BOOT_DIR)
        .file("/home/!Boot/Mine,fff", b"kept");
    let mut session = session_with(&tree, Some(HOME), &[]);
    let mut os = session.kernel.os();
    assert_eq!(boot::start(&mut os).expect("booted").failures, []);
    assert_eq!(tree.names(BOOT_DIR), ["Mine,fff"], "nothing made beside it");
    assert_eq!(
        tree.bytes("/home/!Boot/Mine,fff").as_deref(),
        Some(&b"kept"[..])
    );
    assert_eq!(
        value(&mut os, b"Boot$Dir").as_deref(),
        Some("HostFS::Home.$.!Boot")
    );
}

#[test]
fn pre_desktop_is_obeyed_then_pre_desk_run_files_before_directories() {
    let tree = booted_disc()
        .file(
            "/home/!Boot/Choices/Boot/PreDesktop,feb",
            b"Set Probe$Log <Probe$Log>D\n",
        )
        .file(
            "/home/!Boot/Choices/Boot/PreDesk/B,feb",
            b"Set Probe$Log <Probe$Log>B\n",
        )
        .file(
            "/home/!Boot/Choices/Boot/PreDesk/C,feb",
            b"Set Probe$Log <Probe$Log>C\n",
        )
        .dir("/home/!Boot/Choices/Boot/PreDesk/!App")
        .file(
            "/home/!Boot/Choices/Boot/PreDesk/!App/!Run,feb",
            b"Set Probe$Log <Probe$Log>R\n",
        )
        .dir("/home/!Boot/Choices/Boot/PreDesk/NoRun");
    let mut session = session_with(&tree, Some(HOME), &[]);
    let mut os = session.kernel.os();
    assert_eq!(boot::start(&mut os).expect("booted").failures, []);
    assert_eq!(value(&mut os, b"Probe$Log").as_deref(), Some("DBCR"));
}

#[test]
fn each_failure_is_kept_and_the_boot_goes_on() {
    let tree = booted_disc()
        .file(
            "/home/!Boot/Choices/Boot/PreDesk/A,feb",
            b"Set Probe$Log <Probe$Log>a\nNoSuchCommandAnywhere\nSet Probe$Log <Probe$Log>x\n",
        )
        .file(
            "/home/!Boot/Choices/Boot/PreDesk/B,feb",
            b"Set Probe$Log <Probe$Log>b\n",
        );
    let mut session = session_with(&tree, Some(HOME), &[]);
    let mut os = session.kernel.os();
    let report = boot::start(&mut os).expect("booted");
    assert_eq!(
        value(&mut os, b"Probe$Log").as_deref(),
        Some("ab"),
        "the failing file ended there"
    );
    let [failure] = report.failures.as_slice() else {
        panic!("one failure: {:?}", report.failures);
    };
    assert_eq!(failure.line, b"Obey -c Boot:Choices.Boot.PreDesk.A");
    assert_ne!(failure.number, 0);
    assert!(!failure.text.is_empty());
}

#[test]
fn a_report_keeps_the_first_failures_and_counts_the_rest() {
    let mut tree = booted_disc();
    for n in 0..70 {
        tree = tree.file(
            &format!("{PRE_DESK}/F{n:02},feb"),
            b"NoSuchCommandAnywhere\n",
        );
    }
    let mut session = session_with(&tree, Some(HOME), &[]);
    let mut os = session.kernel.os();
    let report = boot::start(&mut os).expect("booted");
    assert_eq!(report.failures.len(), 64);
    assert_eq!(report.omitted, 6);
    assert_eq!(
        report.failures[0].line, b"Obey -c Boot:Choices.Boot.PreDesk.F00",
        "in the order met"
    );
}

#[test]
fn a_boot_file_is_run_in_place_of_a_boot_application() {
    let tree = Tree::new().file("/home/!Boot,feb", b"Set Probe$Ran 1\n");
    let mut session = session_with(&tree, Some(HOME), &[]);
    let mut os = session.kernel.os();
    assert_eq!(boot::start(&mut os).expect("booted").failures, []);
    assert_eq!(value(&mut os, b"Probe$Ran").as_deref(), Some("1"));
    assert_eq!(
        tree.names("/home"),
        ["!Boot,feb"],
        "no boot application made beside it"
    );
    assert_eq!(value(&mut os, b"Boot$Dir"), None);
}

#[test]
fn a_session_with_no_disc_of_its_own_boots_nothing_and_keeps_choices_in_memory() {
    let tree = Tree::new().dir("/apps");
    let mut session = session(&tree, &[("/apps", "Apps", false)]);
    let mut os = session.kernel.os();
    assert_eq!(boot::start(&mut os).expect("started").failures, []);
    assert_eq!(
        value(&mut os, b"Choices$Write").as_deref(),
        Some("HostFS::Choices.$")
    );
    assert_eq!(
        value(&mut os, b"Choices$Path").as_deref(),
        Some("HostFS::Choices.$.")
    );
    assert_eq!(value(&mut os, b"Boot$Dir"), None);
    assert_eq!(tree.names("/apps"), Vec::<String>::new());
}

#[test]
fn the_boot_goes_on_once_a_program_it_started_exits() {
    let tree = booted_disc()
        .file("/home/!Boot/Choices/Boot/PreDesk/A,ff8", &EXITS)
        .file(
            "/home/!Boot/Choices/Boot/PreDesk/B,feb",
            b"NoSuchCommandAnywhere\n",
        )
        .file(
            "/home/!Boot/Choices/Boot/PreDesk/C,feb",
            b"Set Probe$After 1\n",
        );
    let mut session = session_with(&tree, Some(HOME), &[]);
    common::session(&mut session.kernel, 0x1_0000);
    let mut os = session.kernel.os();
    let started = boot::start(&mut os);
    assert!(
        matches!(started, Err(Fail::Unwind(Unwind::Transfer))),
        "{started:?}"
    );
    assert_eq!(
        value(&mut os, b"Probe$After"),
        None,
        "not yet: the program runs first"
    );
    assert_eq!(
        run_out(&mut session.kernel),
        Outcome::Ended(Ended::Exit { code: 0 })
    );
    let mut os = session.kernel.os();
    assert_eq!(value(&mut os, b"Probe$After").as_deref(), Some("1"));
    let report = boot::reported(&mut os);
    let [failure] = report.failures.as_slice() else {
        panic!("one failure: {:?}", report.failures);
    };
    assert_eq!(failure.line, b"Obey -c Boot:Choices.Boot.PreDesk.B");
    assert_eq!(
        boot::reported(&mut os),
        boot::Report::default(),
        "taken once"
    );
}
