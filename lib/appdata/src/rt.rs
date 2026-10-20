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
use tairix_appconf::Registry;
use tairix_cmdres::{bundle_candidates, CommandEnv};
use tairix_rt::sync::WorkerWake;
use tairix_rt::work::Worker;

use crate::{publish, AppDataHost, PublishAnswer, PublishDesk, PublishJob, Settings};

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

/// How a store job opens the application's own store.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Opening {
    /// With the bundle's shipped `DefaultSettings/` layer, found for the
    /// program the command word names.
    Defaults(&'static str),
    /// Without that layer: a bundle that ships none asks for no filesystem
    /// authority to look for it.
    Bare,
}

impl Opening {
    /// The store, opened this way through `host`.
    pub fn open(self, host: &mut RtHost) -> Settings<'_> {
        match self {
            Self::Defaults(word) => Settings::open(host, word),
            Self::Bare => Settings::open_without_defaults(host),
        }
    }
}

/// The worker a registry's writes are carried out on, off the loop that owes
/// a window its frames.
pub type PublishWorker<R> = Worker<Opening, PublishJob<R>, PublishAnswer<R>>;

/// A [`PublishWorker`] writing through `opening`, not yet started.
#[must_use]
pub fn publish_worker<R: Registry>(opening: Opening) -> PublishWorker<R> {
    Worker::new(publish_own::<R>, opening, WorkerWake::create())
}

/// Carry out `job` against the store `opening` opens: what a
/// [`PublishWorker`] runs.
///
/// A fresh handle per job, so the work keeps no state two threads could
/// disagree about, and opening one costs a single read.
fn publish_own<R: Registry>(opening: &mut Opening, job: &mut PublishJob<R>) -> PublishAnswer<R> {
    let mut host = RtHost;
    publish(&mut opening.open(&mut host), job)
}

impl<R: Registry> PublishDesk<R> for PublishWorker<R> {
    fn submit(&self, job: PublishJob<R>) -> bool {
        PublishWorker::<R>::submit(self, job)
    }

    fn collect(&self) -> Option<PublishAnswer<R>> {
        PublishWorker::<R>::collect(self)
    }

    fn wait(&self) -> Option<PublishAnswer<R>> {
        PublishWorker::<R>::wait(self)
    }
}
