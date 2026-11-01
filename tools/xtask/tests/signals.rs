//! An interrupt to xtask reaches the step it is running, a group of its own,
//! and ends xtask with it.

#[path = "support/process.rs"]
mod process;
#[path = "support/xtask.rs"]
mod support;

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::process::Stdio;

use process::{ended_by, signal, Lines, WHILE_ITS_PARENT_RUNS};
use support::xtask;
use tairix_fuzzseed::scratch::Scratch;

#[test]
fn an_interrupt_reaches_the_running_step_and_ends_xtask_with_it() {
    // Under the build's own directory, which runs programs where a shared
    // temporary one may not.
    let scratch = Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "xtask-signals",
    );
    // Stands in for the `cargo` the step runs: it names itself, says when it
    // takes an interrupt and then dies of it, and otherwise lasts as long as
    // xtask does.
    let cargo = scratch.path().join("cargo");
    std::fs::write(
        &cargo,
        format!(
            "#!/bin/sh\n\
             trap 'echo took-int; trap - INT; kill -INT $$' INT\n\
             echo \"stub $$\"\n\
             {WHILE_ITS_PARENT_RUNS}\n"
        ),
    )
    .expect("the stub");
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).expect("executable");

    let mut cmd = xtask(&["fmt"]);
    cmd.env("CARGO", &cargo)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: runs in the forked child before it execs, and calls only
    // `signal`, `sigemptyset` and `sigprocmask`, all async-signal-safe, on a
    // set built in place.
    unsafe {
        cmd.pre_exec(|| {
            // An interrupt its runner ignored would stay ignored in xtask.
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            let mut set = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
            libc::sigemptyset(set.as_mut_ptr());
            libc::sigprocmask(libc::SIG_SETMASK, set.as_ptr(), std::ptr::null_mut());
            Ok(())
        });
    }
    let mut xtask = cmd.spawn().expect("xtask runs");
    let mut lines = Lines::new(xtask.stdout.take().expect("stdout is piped"));
    let stub: Option<u32> = lines.after("stub ").and_then(|pid| pid.parse().ok());
    signal(xtask.id(), libc::SIGINT);
    let took = stub.is_some() && lines.after("took-int").is_some();
    let ended = ended_by(xtask);
    assert!(stub.is_some(), "the step started");
    assert!(took, "the interrupt reached the step");
    assert_eq!(ended, Some(libc::SIGINT), "and xtask ended with it");
}
