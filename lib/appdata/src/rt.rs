//! The production, `tairix-rt`-backed [`AppDataHost`] (feature `rt`).
//!
//! The client engine itself is I/O-free so it stays host-testable, so this is
//! where its three syscalls actually land: the `ipc_call` to the app-data
//! endpoint, the bounded whole-file read of a bundle's shipped defaults, and
//! the session-dependent half of command-word resolution (`HOME` and `PATH`,
//! which only the running process can see).
//!
//! Feature-gated so a host test injects its own host instead of linking the
//! userland runtime.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::appdata_ipc::APPDATA_ENDPOINT;
use tairix_abi::Errno;
use tairix_cmdres::{bundle_candidates, CommandEnv};

use crate::AppDataHost;

/// The app-data client's syscall host: `ipc_call`, `fs_*`, and the session's
/// own `HOME`/`PATH`.
pub struct RtHost;

impl AppDataHost for RtHost {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        tairix_rt::ipc_call(APPDATA_ENDPOINT, request, reply).map_err(Errno::from_syscall)
    }

    fn read_file(&mut self, path: &str, cap: usize) -> Result<Vec<u8>, Errno> {
        let bytes =
            tairix_rt::read_path_to_end(path.as_bytes(), cap).map_err(Errno::from_syscall)?;
        // Refused whole rather than truncated into a store that means
        // something else.
        if bytes.len() > cap {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(bytes)
    }

    fn bundle_candidates(&mut self, word: &str) -> Vec<String> {
        let home = tairix_rt::env_var(b"HOME").and_then(|value| core::str::from_utf8(value).ok());
        let path_var =
            tairix_rt::env_var(b"PATH").and_then(|value| core::str::from_utf8(value).ok());
        bundle_candidates(word, CommandEnv { home, path_var })
    }
}
