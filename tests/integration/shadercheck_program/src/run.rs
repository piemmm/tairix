//! The `Run` entry-point binary of the `shadercheck` fixture
//! (`plans/GPU.md` GP8, MG3).
//!
//! It runs `gpucheck --shader` over every kernel its disk plants in the
//! account's home, reads what each answered and how it exited, and logs
//! [`tairix_test_shadercheck::PASSED`] once every one was answered as it
//! must be, or [`tairix_test_shadercheck::FAILED`] naming the first that was
//! not and what it answered. `gpucheck` runs as any program the account
//! starts, under its own manifest's authority.
//!
//! On the host it is an inert stub so the workspace build, clippy and fmt
//! still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
extern crate alloc;

#[cfg(all(freestanding, feature = "program"))]
mod program {
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;

    use tairix_abi::{FdWire, SpawnAttach, STDERR, STDOUT, STD_STREAM_COUNT};
    use tairix_log::{Event, Field, FieldValue, Sink};
    use tairix_rt::io::write_stderr_line;
    use tairix_rt::LogSink;
    use tairix_test_shadercheck::{
        Expect, AGREES, CASES, FAILED, FAILED_MESSAGE, FOLDER, LEVEL, LOST, PASSED, PASSED_MESSAGE,
    };

    /// The program each kernel is handed to.
    const GPUCHECK: &[u8] = b"/System/Commands/gpucheck.app/Run";

    /// The most of a run's answer read; a longer one is refused.
    const MAX_ANSWER: usize = 64 << 10;

    /// The most of an answer a failure record carries.
    const MAX_QUOTED: usize = 480;

    /// `gpucheck --shader path` run to its exit: its status, and what it
    /// wrote on both its streams, in order.
    fn gpucheck(path: &str) -> Result<(i32, String), String> {
        let (read, write) = tairix_rt::pipe_create().map_err(|raw| format!("no pipe: {raw}"))?;
        let mut wires = [FdWire::Closed; STD_STREAM_COUNT];
        wires[STDOUT as usize] = FdWire::Handle(write);
        wires[STDERR as usize] = FdWire::Handle(write);
        let attach = SpawnAttach {
            wires,
            ..SpawnAttach::INHERIT
        };
        let args: [&[u8]; 3] = [b"gpucheck", b"--shader", path.as_bytes()];
        let pid = tairix_rt::spawn_attached(GPUCHECK, &attach, &args, &[]);
        // The child holds its own counted clone, so the stream ends with it.
        let _ = tairix_rt::fs_close(write);
        if pid < 0 {
            let _ = tairix_rt::fs_close(read);
            return Err(format!("gpucheck could not be started: {pid}"));
        }
        let answer = tairix_rt::read_fd_to_end(read, MAX_ANSWER);
        let _ = tairix_rt::fs_close(read);
        let mut code = 0;
        if tairix_rt::wait_exit(pid, &mut code) < 0 {
            return Err(String::from("gpucheck could not be reaped"));
        }
        let answer = answer.map_err(|raw| format!("its answer could not be read: {raw}"))?;
        if answer.len() > MAX_ANSWER {
            return Err(String::from("its answer ran past what is read"));
        }
        let answer =
            String::from_utf8(answer).map_err(|_| String::from("its answer is not text"))?;
        Ok((code, answer))
    }

    /// Whether `answer`, with exit status `code`, is what `expect` demands.
    fn judge(expect: Expect, code: i32, answer: &str) -> Result<(), &'static str> {
        let gpus: Vec<&str> = answer
            .lines()
            .filter(|line| line.starts_with("gpu "))
            .collect();
        match expect {
            Expect::Agrees | Expect::Lost if code != 0 => Err("gpucheck exited other than 0"),
            Expect::Agrees | Expect::Lost if gpus.is_empty() => Err("no GPU was answered for"),
            Expect::Agrees | Expect::Lost if !gpus.iter().all(|line| line.contains(AGREES)) => {
                Err("a GPU did not agree with the software device")
            }
            Expect::Agrees if gpus.iter().any(|line| line.contains(LOST)) => {
                Err("a GPU lost a kernel that ends")
            }
            Expect::Lost if !gpus.iter().all(|line| line.contains(LOST)) => {
                Err("a GPU ran a kernel that never ends")
            }
            Expect::Refused(_) if code != 1 => Err("gpucheck did not exit 1 refusing a kernel"),
            Expect::Refused(_) if !gpus.is_empty() => Err("a refused kernel reached a GPU"),
            Expect::Refused(text) if !answer.contains(text) => {
                Err("the refusal did not name what was refused")
            }
            Expect::Agrees | Expect::Lost | Expect::Refused(_) => Ok(()),
        }
    }

    /// Log the failure of `file`, `why`, quoting what it answered; exit 1.
    fn fail(file: &str, why: &str, answer: &str) -> i32 {
        let mut end = answer.len().min(MAX_QUOTED);
        while !answer.is_char_boundary(end) {
            end -= 1;
        }
        let quoted = answer.get(..end).unwrap_or("");
        write_stderr_line(format!("shadercheck: {file}: {why}"));
        LogSink.write_event(&Event {
            level: LEVEL,
            id: FAILED,
            message: FAILED_MESSAGE,
            fields: &[
                Field {
                    key: "file",
                    value: FieldValue::Str(file),
                },
                Field {
                    key: "why",
                    value: FieldValue::Str(why),
                },
                Field {
                    key: "answer",
                    value: FieldValue::Str(quoted),
                },
            ],
        });
        1
    }

    /// Program entry point: every kernel handed to `gpucheck`, the first
    /// answered otherwise logged and the rest left unrun.
    fn main() -> i32 {
        let Some(home) = tairix_rt::env_var(b"HOME").and_then(|raw| core::str::from_utf8(raw).ok())
        else {
            return fail("-", "no home to read the kernels from", "");
        };
        for case in CASES {
            let path = format!("{home}/{}/{}/{}", FOLDER[0], FOLDER[1], case.file);
            match gpucheck(&path) {
                Err(why) => return fail(case.file, &why, ""),
                Ok((code, answer)) => {
                    if let Err(why) = judge(case.expect, code, &answer) {
                        return fail(case.file, why, &answer);
                    }
                }
            }
        }
        LogSink.write_event(&Event {
            level: LEVEL,
            id: PASSED,
            message: PASSED_MESSAGE,
            fields: &[],
        });
        0
    }

    tairix_rt::entry!(main);
}

// On the host the program's real entry is not compiled, so this inert `main`
// keeps the crate building under the host tooling.
#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
