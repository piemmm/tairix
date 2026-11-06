//! The `Run` entry point of the `twins` fixture: the library's steps over the
//! running system, once both sticks' volumes are attached.
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

    use tairix_abi::sysinfo::MountRecord;
    use tairix_abi::volume::{DetachMode, VolumeDetachRequest, VOLUME_DETACH_LEN, VOLUME_ID_LEN};
    use tairix_abi::{Errno, FileId, NoticeTopic, OpenFlags, WaitSourceKind};
    use tairix_procinfo::{for_each_mount, IpcTransport, WalkStep};
    use tairix_rt::io::{write_stderr_line, Stdout, Write};
    use tairix_rt::{File, WaitSet};
    use tairix_test_twins::{
        attached, mount_point, run_steps, System, MOUNT_DEADLINE_NS, NAME, READ_CAP, TWIN_NAME,
    };

    /// The running system, as the session user.
    struct Rt;

    impl System for Rt {
        fn mounts(&mut self) -> Result<Vec<MountRecord>, String> {
            let mut records = Vec::new();
            for_each_mount(&IpcTransport, |record| {
                records.push(*record);
                Ok(WalkStep::Continue)
            })
            .map_err(|err| format!("twins: the mount table could not be read: {err:?}"))?;
            Ok(records)
        }

        fn file_id(&mut self, path: &str) -> Result<FileId, Errno> {
            File::open(path.as_bytes(), OpenFlags::empty())
                .and_then(|node| node.stat())
                .map(|stat| stat.id)
                .map_err(Errno::from_syscall)
        }

        fn read(&mut self, path: &str) -> Result<Vec<u8>, Errno> {
            tairix_rt::read_path_to_end(path.as_bytes(), READ_CAP).map_err(Errno::from_syscall)
        }

        fn write(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno> {
            let flags = OpenFlags::WRITE.union(OpenFlags::TRUNCATE);
            let file = File::create(path.as_bytes(), flags, 0o644).map_err(Errno::from_syscall)?;
            let wrote = file.write_at(0, bytes).map_err(Errno::from_syscall)?;
            if wrote == bytes.len() {
                Ok(())
            } else {
                Err(Errno::NoSpace)
            }
        }

        fn detach(&mut self, volume_id: [u8; VOLUME_ID_LEN]) -> Result<(), Errno> {
            let request = VolumeDetachRequest {
                volume_id,
                mode: DetachMode::Plain,
            };
            let mut frame = [0u8; VOLUME_DETACH_LEN];
            let len = request.encode(&mut frame)?;
            match tairix_rt::volume_detach(&frame[..len]) {
                0 => Ok(()),
                ret => Err(Errno::from_syscall(ret)),
            }
        }

        fn say(&mut self, line: &str) -> Result<(), String> {
            Stdout
                .write_all(line.as_bytes())
                .map_err(|_| String::from("twins: a step could not be reported"))
        }
    }

    /// Park on the mount table's notice until both twins are attached,
    /// answering their records, or say why not once the deadline passes.
    fn await_twins(system: &mut Rt) -> Result<(MountRecord, MountRecord), String> {
        let deadline = tairix_rt::clock_get().saturating_add(MOUNT_DEADLINE_NS);
        let set = WaitSet::create().map_err(|_| String::from("twins: no wait set"))?;
        set.add(
            WaitSourceKind::SystemNotice,
            u64::from(NoticeTopic::Mounts.as_u32()),
            0,
        )
        .map_err(|_| String::from("twins: the mount table's notice was refused"))?;
        // Read after the watch is armed, so an attach between the two still
        // wakes the wait.
        loop {
            if let Some(twins) = attached(&system.mounts()?) {
                return Ok(twins);
            }
            match set.wait_until(deadline) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err(format!(
                        "twins: {} and {} were never both attached",
                        mount_point(NAME),
                        mount_point(TWIN_NAME)
                    ))
                }
                Err(_) => return Err(String::from("twins: the wait on the mount table failed")),
            }
        }
    }

    /// Report `reason` and park, so the vertical's end, armed on this
    /// program's exit, never fires; exit only if even the park is refused.
    fn fail(reason: &str) -> i32 {
        write_stderr_line(reason);
        let _ = tairix_rt::park_forever();
        write_stderr_line("twins: parking was refused");
        1
    }

    fn main() -> i32 {
        let mut system = Rt;
        let steps = await_twins(&mut system)
            .and_then(|(first, twin)| run_steps(&mut system, &first, &twin));
        match steps {
            Ok(()) => 0,
            Err(reason) => fail(&reason),
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
