//! PC17 / BR26 fixture: a separately-linked pure-Rust user-mode program built
//! once and run in two argv-selected roles (`tairix_rt::arg(1)`), so the
//! consuming verticals (`tests/integration/code_install_qemu_{aarch64,riscv64,
//! x86_64}`) prove code installation end to end on every native port.
//!
//! * **`parent`** — holding `CAP_JIT_MAP_EXEC`, starts sandboxed `worker`s as
//!   a compiler and two targets, and a `worker` that is not sandboxed, each
//!   over a pipe pair. It has the compiler write functions returning values it
//!   chooses, installs each into a target with `code_install`, and has the
//!   target call it; it checks the compiler's pages left it, that the
//!   compiler serves only its first target, that the unsandboxed child takes
//!   no part, and that the target releases its code with `mem_unmap`. Exits
//!   `0` only when every check held, else a distinct code per check.
//! * **`worker`** — answers each request on fd 0 with a reply on fd 1 until
//!   the request pipe ends: assemble a function into a fresh page, call the
//!   function at an address, or unmap a page.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::{Errno, SpawnAttach, STDIN, STDOUT};

    /// The page the workers' requests are measured in.
    const PAGE: usize = 4096;

    /// The registry path the parent starts every worker from.
    const PATH_WORKER: &[u8] = b"/bin/ci-worker";

    /// The address space a sandboxed worker is held to.
    const WORKER_CEILING: u64 = 8 * 1024 * 1024;

    /// Request: assemble a function returning the argument into a fresh page,
    /// replying the page's address.
    const OP_WRITE: u8 = b'w';
    /// Request: call the function at the argument, replying what it returns.
    const OP_CALL: u8 = b'c';
    /// Request: unmap the page at the argument, replying the result.
    const OP_UNMAP: u8 = b'u';

    /// Exit code: an unknown or absent role.
    const FAIL_ROLE: i32 = 21;
    /// Exit code: a worker could not be started.
    const FAIL_SPAWN: i32 = 22;
    /// Exit code: a worker did not answer.
    const FAIL_ANSWER: i32 = 23;
    /// Exit code: an install that should have been admitted was refused.
    const FAIL_INSTALL: i32 = 24;
    /// Exit code: installed code did not return what the compiler wrote.
    const FAIL_RUN: i32 = 25;
    /// Exit code: the compiler still held a page it had handed over.
    const FAIL_LEFT: i32 = 26;
    /// Exit code: a compiler installed into a second target.
    const FAIL_PAIR: i32 = 27;
    /// Exit code: a child that is not sandboxed took part.
    const FAIL_OPEN: i32 = 28;
    /// Exit code: the target could not release its code, or released it twice.
    const FAIL_RELEASE: i32 = 29;
    /// Exit code: a worker did not exit cleanly once its pipe ended.
    const FAIL_EXIT: i32 = 30;
    /// Exit code: the worker could not map or write a page.
    const FAIL_WORKER: i32 = 31;

    /// The function returning `value`, as this instruction set spells it:
    /// `mov eax, imm32; ret`.
    #[cfg(code_install_x86_64)]
    fn returning(value: u32) -> [u8; 6] {
        let [a, b, c, d] = value.to_le_bytes();
        [0xB8, a, b, c, d, 0xC3]
    }

    /// The function returning `value`, as this instruction set spells it:
    /// `movz x0, #low; movk x0, #high, lsl #16; ret`.
    #[cfg(code_install_aarch64)]
    fn returning(value: u32) -> [u8; 12] {
        let movz = 0xD280_0000 | ((value & 0xFFFF) << 5);
        let movk = 0xF2A0_0000 | ((value >> 16) << 5);
        words([movz, movk, 0xD65F_03C0])
    }

    /// The function returning `value`, below `0x8000_0000` so `lui` does not
    /// sign-extend it, as this instruction set spells it: `lui a0, upper;
    /// addi a0, a0, low; ret`, `upper` rounded so the sign-extended `low`
    /// lands exactly.
    #[cfg(code_install_riscv64)]
    fn returning(value: u32) -> [u8; 12] {
        let upper = value.wrapping_add(0x800) & 0xFFFF_F000;
        let lui = upper | (10 << 7) | 0x37;
        let addi = ((value & 0xFFF) << 20) | (10 << 15) | (10 << 7) | 0x13;
        words([lui, addi, 0x0000_8067])
    }

    /// Three instruction words in memory order.
    #[cfg(any(code_install_aarch64, code_install_riscv64))]
    fn words(words: [u32; 3]) -> [u8; 12] {
        let mut bytes = [0; 12];
        for (chunk, word) in bytes.chunks_exact_mut(4).zip(words) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    /// Read exactly `buf.len()` bytes from `fd`, or answer `false` at its end.
    fn read_whole(fd: u32, buf: &mut [u8]) -> bool {
        let mut filled = 0;
        while filled < buf.len() {
            match tairix_rt::fs_read(fd, 0, &mut buf[filled..]) {
                Ok(0) | Err(_) => return false,
                Ok(read) => filled += read,
            }
        }
        true
    }

    /// One worker as the parent holds it: its pid and its pipes.
    struct Worker {
        pid: i64,
        requests: u32,
        replies: u32,
    }

    impl Worker {
        /// Start a worker, sandboxed when `sandboxed`.
        fn start(sandboxed: bool) -> Option<Self> {
            let args: [&[u8]; 2] = [PATH_WORKER, b"worker"];
            let (pid, requests, replies) = tairix_rt::spawn_piped(PATH_WORKER, &args, |wires| {
                if sandboxed {
                    SpawnAttach::sandbox(wires, WORKER_CEILING)
                } else {
                    SpawnAttach {
                        wires,
                        ..SpawnAttach::INHERIT
                    }
                }
            })
            .ok()?;
            Some(Self {
                pid,
                requests,
                replies,
            })
        }

        /// Send `op` with `arg` and read the reply.
        fn ask(&self, op: u8, arg: u64) -> Option<u64> {
            let mut request = [0u8; 9];
            request[0] = op;
            request[1..].copy_from_slice(&arg.to_le_bytes());
            if tairix_rt::fs_write(self.requests, 0, &request) != Ok(request.len()) {
                return None;
            }
            let mut reply = [0u8; 8];
            read_whole(self.replies, &mut reply).then(|| u64::from_le_bytes(reply))
        }

        /// End the worker's request pipe and reap it, answering whether it
        /// exited cleanly.
        fn finish(self) -> bool {
            let _ = tairix_rt::fs_close(self.requests);
            let _ = tairix_rt::fs_close(self.replies);
            let mut code = -1;
            tairix_rt::wait_exit(self.pid, &mut code) >= 0 && code == 0
        }
    }

    /// The reply a worker gives for a call answering `-errno`.
    fn refusal(errno: Errno) -> u64 {
        (-i64::from(errno as u32)).cast_unsigned()
    }

    /// Have `compiler` write a function returning `value`, install it into
    /// `target`, and answer where it landed.
    fn install(compiler: &Worker, target: &Worker, value: u32) -> Result<u64, i32> {
        let base = compiler
            .ask(OP_WRITE, u64::from(value))
            .ok_or(FAIL_ANSWER)?;
        tairix_rt::code_install(compiler.pid, target.pid, base, PAGE).map_err(|_| FAIL_INSTALL)
    }

    /// Require `target` to run the function at `at` and get `value`.
    fn runs(target: &Worker, at: u64, value: u32) -> Result<(), i32> {
        match target.ask(OP_CALL, at) {
            Some(got) if got == u64::from(value) => Ok(()),
            Some(_) => Err(FAIL_RUN),
            None => Err(FAIL_ANSWER),
        }
    }

    /// The parent role: every check, in order, through the real syscalls.
    fn checks(
        compiler: &Worker,
        target: &Worker,
        other: &Worker,
        open: &Worker,
    ) -> Result<(), i32> {
        let first = 0x0C0D_E001;
        let at = install(compiler, target, first)?;
        runs(target, at, first)?;

        let base = compiler.ask(OP_WRITE, 0x0C0D_E002).ok_or(FAIL_ANSWER)?;
        if tairix_rt::code_install(compiler.pid, other.pid, base, PAGE)
            != Err(Errno::PermissionDenied)
        {
            return Err(FAIL_PAIR);
        }
        let second = tairix_rt::code_install(compiler.pid, target.pid, base, PAGE)
            .map_err(|_| FAIL_INSTALL)?;
        if second == at {
            return Err(FAIL_INSTALL);
        }
        runs(target, second, 0x0C0D_E002)?;
        if compiler.ask(OP_UNMAP, base) != Some(refusal(Errno::NotFound)) {
            return Err(FAIL_LEFT);
        }

        let base = compiler.ask(OP_WRITE, 0x0C0D_E003).ok_or(FAIL_ANSWER)?;
        for (from, to) in [(open, target), (compiler, open)] {
            if tairix_rt::code_install(from.pid, to.pid, base, PAGE) != Err(Errno::PermissionDenied)
            {
                return Err(FAIL_OPEN);
            }
        }
        let third = tairix_rt::code_install(compiler.pid, target.pid, base, PAGE)
            .map_err(|_| FAIL_INSTALL)?;
        runs(target, third, 0x0C0D_E003)?;

        if target.ask(OP_UNMAP, at) != Some(0)
            || target.ask(OP_UNMAP, at) != Some(refusal(Errno::NotFound))
        {
            return Err(FAIL_RELEASE);
        }
        runs(target, second, 0x0C0D_E002)
    }

    /// The parent role: start the workers, run every check, and reap them.
    fn parent() -> i32 {
        let (Some(compiler), Some(target), Some(other), Some(open)) = (
            Worker::start(true),
            Worker::start(true),
            Worker::start(true),
            Worker::start(false),
        ) else {
            return FAIL_SPAWN;
        };
        let checked = checks(&compiler, &target, &other, &open);
        let finished = [compiler, target, other, open]
            .into_iter()
            .fold(true, |all, worker| worker.finish() && all);
        match checked {
            Err(code) => code,
            Ok(()) if !finished => FAIL_EXIT,
            Ok(()) => 0,
        }
    }

    /// Assemble a function returning `value` into a fresh page of this
    /// process's own anonymous memory, answering its address.
    fn assemble(value: u32) -> Option<u64> {
        tairix_rt::stage_code(&returning(value)).ok()
    }

    /// Call the function the kernel installed at `at`.
    fn call(at: u64) -> u64 {
        // SAFETY: the parent sends only an address `code_install` answered
        // for this process, where the kernel mapped read-execute a whole
        // function, written by `returning`, that takes nothing and returns in
        // the platform's C calling convention.
        let function: extern "C" fn() -> u64 = unsafe { core::mem::transmute(at as usize) };
        function()
    }

    /// The worker role: answer requests until the request pipe ends.
    fn worker() -> i32 {
        let mut request = [0u8; 9];
        while read_whole(STDIN, &mut request) {
            let mut arg = [0u8; 8];
            arg.copy_from_slice(&request[1..]);
            let arg = u64::from_le_bytes(arg);
            let reply = match request[0] {
                OP_WRITE => match u32::try_from(arg).ok().and_then(assemble) {
                    Some(base) => base,
                    None => return FAIL_WORKER,
                },
                OP_CALL => call(arg),
                OP_UNMAP => tairix_rt::mem_unmap(arg, PAGE).cast_unsigned(),
                _ => return FAIL_ROLE,
            };
            if tairix_rt::fs_write(STDOUT, 0, &reply.to_le_bytes()) != Ok(8) {
                return FAIL_WORKER;
            }
        }
        0
    }

    /// Program entry point: dispatch on the role the registry row pinned.
    fn main() -> i32 {
        match tairix_rt::arg(1) {
            Some(b"parent") => parent(),
            Some(b"worker") => worker(),
            _ => FAIL_ROLE,
        }
    }

    tairix_rt::entry!(main);
}

// On the host the freestanding entry path is not compiled; this inert `main`
// keeps the crate building under the host tooling.
#[cfg(not(freestanding))]
fn main() {}
