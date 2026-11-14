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
use core::fmt;

use tairix_abi::appdata_ipc::{BlobMode, APPDATA_ENDPOINT};
use tairix_abi::Errno;
use tairix_appconf::Registry;
use tairix_cmdres::{bundle_candidates, CommandEnv};
use tairix_rt::sync::WorkerWake;
use tairix_rt::work::Worker;
use tairix_rt::File;

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

/// Why a blob of the application's store could not be used, as its user is
/// told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobError {
    name: String,
    fault: BlobFault,
}

/// What went wrong with a blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobFault {
    /// The app-data service refused it.
    Refused(Errno),
    /// Its delegation could not be redeemed.
    Untaken(Errno),
    /// It holds more than it may.
    TooLarge,
    /// The heap would not hold it.
    OutOfMemory,
    /// A read of it failed.
    Unread(Errno),
    /// A write to it failed.
    Unwritten(Errno),
}

impl BlobError {
    /// What went wrong.
    #[must_use]
    pub const fn fault(&self) -> BlobFault {
        self.fault
    }

    fn of(name: &str, fault: BlobFault) -> Self {
        Self {
            name: String::from(name),
            fault,
        }
    }
}

impl fmt::Display for BlobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = &self.name;
        match self.fault {
            BlobFault::Refused(err) => write!(f, "the store refused {name} ({err})"),
            BlobFault::Untaken(err) => write!(f, "the grant of {name} could not be taken ({err})"),
            BlobFault::TooLarge => write!(f, "{name} is larger than it can be"),
            BlobFault::OutOfMemory => write!(f, "there is no memory to read {name}"),
            BlobFault::Unread(err) => write!(f, "{name} could not be read ({err})"),
            BlobFault::Unwritten(err) => write!(f, "{name} could not be written ({err})"),
        }
    }
}

impl From<BlobError> for String {
    fn from(err: BlobError) -> Self {
        alloc::format!("{err}")
    }
}

/// Blob `name` of the calling application's store, opened as `mode` says,
/// as a descriptor of its own.
///
/// # Errors
///
/// [`BlobFault::Refused`] or [`BlobFault::Untaken`].
pub fn open_blob(name: &str, mode: BlobMode) -> Result<File, BlobError> {
    let grant = crate::blobs::open(&mut RtHost, name, mode)
        .map_err(|err| BlobError::of(name, BlobFault::Refused(err)))?;
    File::from_delegation(grant)
        .map_err(|err| BlobError::of(name, BlobFault::Untaken(Errno::from_syscall(err))))
}

/// The whole of blob `name`, refused past `most` bytes.
///
/// # Errors
///
/// As [`open_blob`], [`BlobFault::TooLarge`], [`BlobFault::OutOfMemory`] or
/// [`BlobFault::Unread`].
pub fn read_blob(name: &str, most: usize) -> Result<Vec<u8>, BlobError> {
    let file = open_blob(name, BlobMode::Read)?;
    let bytes = tairix_rt::read_fd_to_end(file.fd(), most).map_err(|err| {
        BlobError::of(
            name,
            match Errno::from_syscall(err) {
                Errno::OutOfMemory => BlobFault::OutOfMemory,
                err => BlobFault::Unread(err),
            },
        )
    })?;
    if bytes.len() > most {
        return Err(BlobError::of(name, BlobFault::TooLarge));
    }
    Ok(bytes)
}

/// The bytes `span` of blob `name`, open as `file`, or as many of them as it
/// holds.
///
/// # Errors
///
/// [`BlobFault::OutOfMemory`] or [`BlobFault::Unread`].
pub fn read_blob_span(
    file: &File,
    name: &str,
    span: core::ops::Range<usize>,
) -> Result<Vec<u8>, BlobError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(span.len())
        .map_err(|_| BlobError::of(name, BlobFault::OutOfMemory))?;
    bytes.resize(span.len(), 0);
    let read = tairix_rt::fs_read_full(file.fd(), span.start as u64, &mut bytes)
        .map_err(|err| BlobError::of(name, BlobFault::Unread(err)))?;
    bytes.truncate(read);
    Ok(bytes)
}

/// Blob `name` made to hold exactly `bytes`, on the store before this
/// answers.
///
/// # Errors
///
/// As [`open_blob`], or [`BlobFault::Unwritten`].
pub fn write_blob(name: &str, bytes: &[u8]) -> Result<(), BlobError> {
    let file = open_blob(name, BlobMode::ReadWrite)?;
    let unwritten = |err| BlobError::of(name, BlobFault::Unwritten(err));
    file.truncate(0)
        .map_err(|err| unwritten(Errno::from_syscall(err)))?;
    tairix_rt::fs_write_all(file.fd(), 0, bytes).map_err(unwritten)?;
    file.sync()
        .map_err(|err| unwritten(Errno::from_syscall(err)))
}
