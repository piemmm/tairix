//! The `Run` entry point of the `images` fixture: the library's steps over
//! the running system.
//!
//! On the host it is an inert stub so the workspace tooling still covers the
//! file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;

    use tairix_abi::{Errno, OpenFlags, CONSOLE_INHERIT, SPAWN_UID_INHERIT};
    use tairix_rt::io::{write_stderr_line, Stdout, Write};
    use tairix_rt::File;
    use tairix_test_images::{pass_line, read_whole, run_steps, System, READ_CAP};

    /// The running system, as the session user.
    struct Rt;

    impl System for Rt {
        fn run(&mut self, path: &str, args: &[&str]) -> Result<i32, String> {
            let words: Vec<&[u8]> = args.iter().map(|arg| arg.as_bytes()).collect();
            let pid = tairix_rt::spawn_with(
                path.as_bytes(),
                CONSOLE_INHERIT,
                SPAWN_UID_INHERIT,
                &words,
                &[],
            );
            if pid < 0 {
                return Err(format!("images: {path} could not be spawned ({pid})"));
            }
            let mut status = 0i32;
            if tairix_rt::wait_exit(pid, &mut status) < 0 {
                return Err(format!("images: {path} could not be waited for"));
            }
            Ok(status)
        }

        fn read(&mut self, path: &str) -> Result<Vec<u8>, Errno> {
            tairix_rt::read_path_to_end(path.as_bytes(), READ_CAP)
                .map_err(Errno::from_syscall)
                .and_then(read_whole)
        }

        fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno> {
            let flags = OpenFlags::WRITE.union(OpenFlags::TRUNCATE);
            let file = File::create(path.as_bytes(), flags, 0o644).map_err(Errno::from_syscall)?;
            let mut at = 0;
            while at < bytes.len() {
                let wrote = file
                    .write_at(at as u64, &bytes[at..])
                    .map_err(Errno::from_syscall)?;
                if wrote == 0 {
                    return Err(Errno::NoSpace);
                }
                at += wrote;
            }
            Ok(())
        }

        fn exists(&mut self, path: &str) -> bool {
            File::open(path.as_bytes(), OpenFlags::empty()).is_ok()
        }
    }

    /// Report `reason` and park, so the vertical's end, armed on this
    /// program's exit, never fires; exit only if even the park is refused.
    fn fail(reason: &str) -> i32 {
        write_stderr_line(reason);
        let _ = tairix_rt::park_forever();
        write_stderr_line("images: parking was refused");
        1
    }

    fn main() -> i32 {
        if let Err(reason) = run_steps(&mut Rt) {
            return fail(&reason);
        }
        if Stdout.write_all(pass_line().as_bytes()).is_err() {
            return fail("images: the pass could not be reported");
        }
        0
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
