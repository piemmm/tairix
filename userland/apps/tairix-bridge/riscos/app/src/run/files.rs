//! The file worker: every session's HostFS served over its grants, each
//! session's worker started, the documents folder, the zone store and the
//! files handed over redeemed. Each waits on the filesystem, the sysinfo
//! service or a spawn, so none runs on the window loop.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::num::NonZeroUsize;

use tairix_abi::origin::ProcId;
use tairix_abi::{Errno, LimitKind, RLIMIT_INFINITY, SPAWN_SELF};
use tairix_collections::OrderedMap;
use tairix_riscos_app::memory::Plan;
use tairix_riscos_app::provenance::Volume;
use tairix_riscos_filing::hostfs::server::{Given, Server};
use tairix_riscos_session::protocol::ZONE_STORE_MOST;
use tairix_riscos_session::ring::RING_BYTES;
use tairix_rt::work::Worker;
use tairix_rt::{Dir, File};
use tairix_sandbox::rt::RtSessionChannel;
use tairix_sandbox::WorkerMemory;
use tairix_util::defer::JobQueue;

use crate::front::SessionId;

/// The file worker.
pub type FileWorker = Worker<Held, Job, Done, JobQueue<Job, Done>>;

/// The jobs waiting at once: a relay and a delivery for each of a few
/// sessions, beside a start.
pub const JOBS: usize = 32;

/// The name the user's own disc has: the documents folder's.
const OWN_DISC: &str = "RISC OS";

/// What the worker keeps between jobs.
pub struct Held {
    servers: OrderedMap<SessionId, Server<Dir>>,
    /// The documents folder, until the shared session takes it.
    documents: Option<Dir>,
}

impl Held {
    /// Nothing held yet.
    pub const fn new() -> Self {
        Self {
            servers: OrderedMap::new(),
            documents: None,
        }
    }
}

/// A file handed over to run.
pub struct Handed {
    /// The file.
    pub file: File,
    /// Its TAIRiX name, whose suffix types it.
    pub name: String,
    /// The session may write it.
    pub writable: bool,
}

/// A session to start.
pub struct Start {
    /// Which.
    pub session: SessionId,
    /// It is the shared session, the documents folder its own disc.
    pub own: bool,
    /// The file its first launch runs.
    pub handed: Option<Handed>,
    /// The setting's memory ceiling.
    pub setting: Option<u64>,
}

/// A job.
pub enum Job {
    /// The documents folder granted as `handle`.
    Documents {
        /// Its delegation.
        handle: u64,
    },
    /// The zone store granted as `handle` by `grantor`.
    Zone {
        /// Its delegation.
        handle: u64,
        /// The desktop session that granted it.
        grantor: ProcId,
    },
    /// A session started.
    Start(Start),
    /// A HostFS request of `session`'s.
    Relay {
        /// Whose.
        session: SessionId,
        /// Its serial.
        serial: u32,
        /// The request.
        request: Vec<u8>,
    },
    /// A file granted as `handle` taken, with its name.
    Take {
        /// Its delegation.
        handle: u64,
        /// Its name.
        name: String,
        /// It may be written.
        writable: bool,
        /// The session asked for it by the window's own command.
        shared: bool,
    },
    /// A file already held, looked at for its volume: the job holds it
    /// until it is done.
    Look {
        /// The file.
        handed: Option<Handed>,
        /// The session was asked for by the window's own command.
        shared: bool,
    },
    /// `handed` delivered on `session`'s `Transfer`: the job holds it until
    /// it is done.
    Deliver {
        /// Into which.
        session: SessionId,
        /// The file.
        handed: Option<Handed>,
    },
    /// `session` ended: its server is let go.
    Ended {
        /// Which.
        session: SessionId,
    },
}

/// A started session.
pub struct Started {
    /// Its memory.
    pub plan: Plan,
    /// Its worker's channel.
    pub channel: RtSessionChannel,
    /// The line its first launch runs, where it was handed a file.
    pub line: Option<Vec<u8>>,
}

/// What a job came to.
pub enum Done {
    /// The documents folder's volume, or why it could not be taken.
    Documents(Result<Volume, String>),
    /// The zone store's bytes, read and found sound, or none.
    Zone(Option<Vec<u8>>),
    /// `session` started, or why not.
    Started {
        /// Which.
        session: SessionId,
        /// Its start.
        started: Result<Started, String>,
    },
    /// The reply to `session`'s relay `serial`, none where it could not be
    /// carried.
    Relayed {
        /// Whose.
        session: SessionId,
        /// Its serial.
        serial: u32,
        /// The reply.
        reply: Option<Vec<u8>>,
    },
    /// A file taken, and the volume it is on, or why not.
    Taken {
        /// The file and its volume.
        taken: Result<(Handed, Volume), String>,
        /// The session was asked for by the window's own command.
        shared: bool,
    },
    /// A file delivered into `session`: the line that runs it, or why not.
    Delivered {
        /// Into which.
        session: SessionId,
        /// The line.
        line: Result<Vec<u8>, String>,
    },
    /// An ended session's server let go.
    LetGo,
}

/// One job done.
pub fn serve(held: &mut Held, job: &mut Job) -> Done {
    match job {
        Job::Documents { handle } => Done::Documents(documents(held, *handle)),
        Job::Zone { handle, grantor } => Done::Zone(zone(*handle, *grantor)),
        Job::Start(start) => Done::Started {
            session: start.session,
            started: started(held, start),
        },
        Job::Relay {
            session,
            serial,
            request,
        } => Done::Relayed {
            session: *session,
            serial: *serial,
            reply: held.servers.get_mut(session).map(|server| {
                let mut reply = Vec::new();
                server.serve(request, &mut reply);
                reply
            }),
        },
        Job::Take {
            handle,
            name,
            writable,
            shared,
        } => Done::Taken {
            taken: take(*handle, core::mem::take(name), *writable),
            shared: *shared,
        },
        Job::Look { handed, shared } => Done::Taken {
            taken: handed
                .take()
                .ok_or_else(|| String::from("nothing was handed over"))
                .and_then(|handed| {
                    let on = volume(&handed.file)?;
                    Ok((handed, on))
                }),
            shared: *shared,
        },
        Job::Deliver { session, handed } => Done::Delivered {
            session: *session,
            line: handed
                .take()
                .ok_or_else(|| String::from("nothing was handed over"))
                .and_then(|handed| deliver(held, *session, handed)),
        },
        Job::Ended { session } => {
            held.servers.remove(session);
            Done::LetGo
        }
    }
}

/// The line that runs the file `server` delivered as `handed`.
fn run_line(server: &mut Server<Dir>, handed: Handed) -> Result<Vec<u8>, String> {
    let name = server
        .deliver(&handed.name, handed.file, handed.writable)
        .map_err(|refusal| format!("it could not be handed over ({refusal:?})"))?;
    let mut line = Vec::new();
    line.try_reserve_exact(RUN.len() + name.len())
        .map_err(|_| String::from("there was no memory to hand it over"))?;
    line.extend_from_slice(RUN);
    line.extend_from_slice(&name);
    Ok(line)
}

/// What a delivered file is run with.
const RUN: &[u8] = b"Run ";

/// A file's volume.
fn volume(file: &File) -> Result<Volume, String> {
    file.stat()
        .map(|stat| stat.id.volume)
        .map_err(|err| format!("it could not be looked at ({})", Errno::from_syscall(err)))
}

fn documents(held: &mut Held, handle: u64) -> Result<Volume, String> {
    let dir = Dir::from_delegation(handle)
        .map_err(|err| format!("it could not be taken ({})", Errno::from_syscall(err)))?;
    let volume = volume(dir.as_file())?;
    held.documents = Some(dir);
    Ok(volume)
}

fn zone(handle: u64, grantor: ProcId) -> Option<Vec<u8>> {
    let file = File::from_delegation_by(handle, grantor).ok()?;
    let bytes = tairix_rt::read_fd_to_end(file.fd(), ZONE_STORE_MOST).ok()?;
    tairix_tz::ZoneInfo::parse(&bytes).ok()?;
    Some(bytes)
}

fn take(handle: u64, name: String, writable: bool) -> Result<(Handed, Volume), String> {
    let file = File::from_delegation(handle)
        .map_err(|err| format!("it could not be taken ({})", Errno::from_syscall(err)))?;
    let volume = volume(&file)?;
    Ok((
        Handed {
            file,
            name,
            writable,
        },
        volume,
    ))
}

fn deliver(held: &mut Held, session: SessionId, handed: Handed) -> Result<Vec<u8>, String> {
    let server = held
        .servers
        .get_mut(&session)
        .ok_or_else(|| String::from("its session has ended"))?;
    run_line(server, handed)
}

/// The machine's usable RAM and the user's address-space limit, where known.
fn bounds() -> (Option<u64>, Option<u64>) {
    let usable = tairix_procinfo::memory_total_bytes(&tairix_procinfo::IpcTransport).ok();
    let limit = tairix_rt::rlimit_get(LimitKind::AddressSpaceBytes)
        .ok()
        .map(|limit| limit.soft)
        .filter(|&soft| soft != RLIMIT_INFINITY);
    (usable, limit)
}

fn started(held: &mut Held, start: &mut Start) -> Result<Started, String> {
    let (usable, limit) = bounds();
    let plan = Plan::of(usable, limit, start.setting)
        .ok_or_else(|| String::from("there is too little memory for a RISC OS session"))?;
    let own = if start.own {
        let dir = held
            .documents
            .take()
            .ok_or_else(|| String::from("the documents folder was not granted"))?;
        Some(Given {
            dir,
            name: String::from(OWN_DISC),
            writable: true,
        })
    } else {
        None
    };
    let mut seed = [0; 16];
    tairix_rt::random_fill(&mut seed)
        .map_err(|_| String::from("no secure random bytes could be had"))?;
    let epoch = u32::try_from(start.session).unwrap_or(u32::MAX);
    let mut server = Server::new(own, Vec::new(), seed, epoch);
    let line = match start.handed.take() {
        Some(handed) => Some(run_line(&mut server, handed)?),
        None => None,
    };
    let share = NonZeroUsize::new(RING_BYTES as usize);
    let memory = WorkerMemory::Machine {
        bytes: plan.worker_bytes,
    };
    let channel = RtSessionChannel::launch(SPAWN_SELF, memory, share)
        .map_err(|err| format!("its worker could not be started ({err})"))?;
    held.servers
        .try_insert(start.session, server)
        .map_err(|_| String::from("there was no memory to keep its files"))?;
    Ok(Started {
        plan,
        channel,
        line,
    })
}
