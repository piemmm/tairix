//! The `Run` entry point of the `optical` fixture: the library's check over
//! the running system's files, after waiting for the disc's volume and
//! copying it with `cp`, and again over the disc that replaces it.
//!
//! On the host it is an inert stub so the workspace tooling still covers the
//! file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::vec::Vec;

    use tairix_abi::{
        Errno, NoticeTopic, OpenFlags, WaitSourceKind, CONSOLE_INHERIT, SPAWN_UID_INHERIT,
    };
    use tairix_rt::io::{write_stderr_line, Stdout, Write};
    use tairix_rt::{File, WaitSet};
    use tairix_test_iso9660_image::{RISCOS_FILES, SECOND_FILES};
    use tairix_test_optical::{
        check, pass_line, swap_line, Files, COPY, CP, MOUNT_DEADLINE_NS, READ_CAP, SECOND_SOURCE,
        SOURCE,
    };

    /// The system's files, read as the session user.
    struct Rt;

    impl Files for Rt {
        fn contents(&mut self, path: &str) -> Result<Vec<u8>, Errno> {
            tairix_rt::read_path_to_end(path.as_bytes(), READ_CAP).map_err(Errno::from_syscall)
        }

        fn attr(&mut self, path: &str, key: &str) -> Result<Vec<u8>, Errno> {
            tairix_rt::fs_attr_value(path.as_bytes(), key.as_bytes()).map_err(Errno::from_syscall)
        }
    }

    /// Whether a volume is attached at `path`.
    fn mounted(path: &str) -> bool {
        File::open(path.as_bytes(), OpenFlags::empty()).is_ok()
    }

    /// Park on the mount table's notice until `ready` holds, or say `why`
    /// once the deadline passes.
    fn await_mounts(ready: impl Fn() -> bool, why: &'static str) -> Result<(), &'static str> {
        let deadline = tairix_rt::clock_get().saturating_add(MOUNT_DEADLINE_NS);
        let set = WaitSet::create().map_err(|_| "optical: no wait set")?;
        set.add(
            WaitSourceKind::SystemNotice,
            u64::from(NoticeTopic::Mounts.as_u32()),
            0,
        )
        .map_err(|_| "optical: the mount table's notice was refused")?;
        // Looked at after the watch is armed, so a change between the two
        // still wakes the wait.
        while !ready() {
            match set.wait_until(deadline) {
                Ok(Some(_)) => {}
                Ok(None) => return Err(why),
                Err(_) => return Err("optical: the wait on the mount table failed"),
            }
        }
        Ok(())
    }

    /// Copy the disc's volume to [`COPY`] with `cp -r`.
    fn copy() -> Result<(), &'static str> {
        let args: [&[u8]; 4] = [b"cp", b"-r", SOURCE.as_bytes(), COPY.as_bytes()];
        let pid = tairix_rt::spawn_with(CP, CONSOLE_INHERIT, SPAWN_UID_INHERIT, &args, &[]);
        if pid < 0 {
            return Err("optical: cp could not be spawned");
        }
        let mut status = 0i32;
        if tairix_rt::wait_exit(pid, &mut status) < 0 {
            return Err("optical: cp could not be waited for");
        }
        if status != 0 {
            return Err("optical: cp exited non-zero");
        }
        Ok(())
    }

    /// Report `reason` and park, so the vertical's end, armed on this
    /// program's exit, never fires; exit only if even the park is refused.
    fn fail(reason: &str) -> i32 {
        write_stderr_line(reason);
        let _ = tairix_rt::park_forever();
        write_stderr_line("optical: parking was refused");
        1
    }

    fn main() -> i32 {
        let first = || mounted(SOURCE);
        if let Err(reason) =
            await_mounts(first, "optical: the disc's volume never appeared").and_then(|()| copy())
        {
            return fail(reason);
        }
        for dir in [SOURCE, COPY] {
            if let Err(reason) = check(&mut Rt, dir, &RISCOS_FILES) {
                return fail(&reason);
            }
        }
        if Stdout.write_all(swap_line().as_bytes()).is_err() {
            return fail("optical: the change of disc could not be asked for");
        }
        let changed = || !mounted(SOURCE) && mounted(SECOND_SOURCE);
        if let Err(reason) =
            await_mounts(changed, "optical: the next disc never replaced the first")
        {
            return fail(reason);
        }
        if let Err(reason) = check(&mut Rt, SECOND_SOURCE, &SECOND_FILES) {
            return fail(&reason);
        }
        if Stdout.write_all(pass_line().as_bytes()).is_err() {
            return fail("optical: the pass could not be reported");
        }
        0
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
