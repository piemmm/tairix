//! HostFS (`plans/RISCOS-EMULATOR.md` §6): the session's files as one filing
//! system, each of its roots a disc, the first the session's own where it has
//! one; and discs in the worker's memory: `Scrap`, and `Choices` for a
//! session with no disc of its own.
//!
//! The roots are held by the side that holds the session's files; a call
//! into HostFS crosses the relay to it as a [`protocol`] request and is
//! answered there by a [`server`]. The memory discs are answered by a server
//! of their own over an in-memory tree ([`scrap`]), so both kinds of disc are
//! served alike. Bytes read from a root are kept in a reclaimable read cache.
//!
//! - [`protocol`]: what crosses the relay.
//! - [`server`]: the side that holds the files, over `lib/fscopy`'s tree seam.
//! - [`scrap`]: the tree the memory discs are held in.

mod cache;
mod entries;
pub mod protocol;
pub mod scrap;
pub mod server;

use alloc::vec::Vec;

use tairix_fscopy::tree::Dir as _;
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::filing::fs::info;
use tairix_riscos_kernel::filing::UNSUPPORTED;
use tairix_riscos_kernel::module::{Builtin, Command, Resource, Services};
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{quietly, Fail, Os, Regs, Unwind, World};
use tairix_riscos_kernel::service::FS_REDECLARE;
use tairix_riscos_kernel::swi::os::{fs_control, FS_CONTROL};
use tairix_riscos_kernel::swi::X;

use crate::declared::Declared;
use crate::filecore::{code, numbered};
use protocol::{Refusal, Reply, Request};

/// HostFS's filing system number.
const NUMBER: u32 = tairix_riscos_kernel::cmos::HOSTFS as u32;

/// Its information word: its number, a host of image filing systems, told
/// of each flush, and loading and saving through its open files.
const INFO: u32 =
    NUMBER | info::IMAGES | info::FLUSH_NOTIFY | info::LOAD_BY_OPEN | info::SAVE_BY_OPEN;

/// Its messages, in the emulator's own words.
const MESSAGES: &[u8] = include_bytes!("../../resources/HostFS/Messages");
/// The filing system's name, its module's title and the command that
/// selects it.
pub const NAME: &str = "HostFS";

/// The first of its routines that are its entries, which are all it has.
const ENTRIES: usize = 0;

/// Its filing system.
static DECLARED: Declared = Declared {
    module: &HOST_FS,
    messages: OwnFile::named(b"Resources:$.Resources.HostFS.Messages\0"),
    name: NAME.as_bytes(),
    first_entry: ENTRIES,
    gbpb: false,
    info: INFO,
    extra: 0,
};

/// HostFS.
pub static HOST_FS: Builtin = Builtin {
    title: NAME,
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: None,
    services: Some(Services {
        numbers: &[FS_REDECLARE],
        handler: service,
    }),
    commands: &[Command::new(
        NAME,
        (0, 0),
        "*HostFS makes HostFS the current filing system.",
        Some("Syntax: *HostFS"),
        select,
    )],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[
        entries::open,
        entries::get_bytes,
        entries::put_bytes,
        entries::args,
        entries::close,
        entries::file,
        entries::func,
    ],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.HostFS.Messages",
        file_type: tairix_riscos_kernel::filing::filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// Where a disc's requests are answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Place {
    /// Across the relay, by the root of that number.
    Relay(u32),
    /// By the worker's own server of its memory discs, the root of that
    /// number.
    Memory(u32),
}

/// One of HostFS's discs; whether it may be written is its server's to
/// enforce.
struct Disc {
    name: Vec<u8>,
    place: Place,
}

/// A file HostFS holds open: its disc's place, the handle its server gave
/// it, the name its blocks are kept under in the read cache, none for a file
/// in the worker's own memory, and its path, which a stamp is read through.
#[derive(Clone)]
struct Opened {
    place: Place,
    handle: u32,
    cached: Option<cache::Name>,
    path: Vec<u8>,
}

/// What HostFS holds while it is active.
pub struct State {
    own: Own,
    workspace: u32,
    discs: Vec<Disc>,
    home: Home,
    memory: server::Server<scrap::Node>,
    files: Vec<Option<Opened>>,
    cache: cache::ReadCache,
    next_key: u64,
}

impl State {
    /// A name of an open's own in the read cache, never given before.
    fn mint(&mut self) -> cache::Name {
        self.next_key = self.next_key.wrapping_add(1);
        cache::Name::Opened(self.next_key)
    }
}

impl tairix_riscos_kernel::Reclaims for State {
    fn reclaim(&mut self) {
        self.cache.reclaim();
    }
}

/// The most HostFS holds of the host's memory beside a session of `ceiling`
/// bytes: its read cache and `Scrap`.
#[must_use]
pub fn host_bytes(ceiling: u64) -> u64 {
    cache::ReadCache::most(ceiling).saturating_add(scrap::bound(ceiling))
}

/// The names the memory discs have.
const SCRAP: &str = "Scrap";
const CHOICES: &str = "Choices";

/// Where a session keeps its own RISC OS files.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Home {
    /// The user's own RISC OS disc, the documents folder: the disc at that
    /// place in HostFS's discs.
    Own(usize),
    /// The memory disc at that place, which holds `Choices:` for a session
    /// with no disc of its own and ends with it.
    Memory(usize),
}

/// Where the session keeps its own files, as the root of its disc:
/// `HostFS::<disc>.$`, and whether it is the user's own disc.
///
/// # Errors
///
/// "Filing system not present" while HostFS is not active.
pub fn home(os: &mut Os<'_>) -> Result<(Vec<u8>, bool), Fail> {
    let found = with(os, |state| {
        let (at, own) = match state.home {
            Home::Own(at) => (at, true),
            Home::Memory(at) => (at, false),
        };
        state.discs.get(at).map(|disc| (root_of(&disc.name), own))
    })?;
    match found {
        Some((Some(root), own)) => Ok((root, own)),
        Some((None, _)) => Err(os.no_memory()),
        None => Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT)),
    }
}

/// HostFS's own message file in the world, while it is active.
fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

/// `work` on HostFS's state; "not present" while it has none, as only a
/// finalisation part-way through could leave.
fn with<T>(os: &mut Os<'_>, work: impl FnOnce(&mut State) -> T) -> Result<T, Fail> {
    match os.extension::<State>() {
        Some(state) => Ok(work(state)),
        None => Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT)),
    }
}

/// Its initialisation: its workspace, the roots the relay gives and `Scrap`
/// as its discs, and the filing system registered.
fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, messages) = DECLARED.made(os, regs.r[12])?;
    let Roots {
        seed,
        own,
        discs: roots,
    } = discs(os);
    let mut discs: Vec<Disc> = Vec::new();
    discs
        .try_reserve_exact(roots.len() + 2)
        .map_err(|_| os.no_memory())?;
    for (number, name) in (0..).zip(roots) {
        discs.push(Disc {
            name,
            place: Place::Relay(number),
        });
    }
    let tree = scrap::Tree::new(scrap::bound(os.ceiling_bytes()));
    let memory_discs: &[&str] = if own { &[SCRAP] } else { &[SCRAP, CHOICES] };
    let mut given = Vec::new();
    let mut home = Home::Own(0);
    let mut scrap_dir = None;
    for (number, &name) in (0..).zip(memory_discs) {
        let dir = tree
            .root()
            .make_dir(name, 0o777)
            .map_err(|_| os.no_memory())?;
        given.push(server::Given {
            dir,
            name: alloc::string::String::from(name),
            writable: true,
        });
        let disc = server::unique_disc(name, &discs.iter().map(|disc| disc.name.as_slice()));
        match name {
            SCRAP => scrap_dir = root_of(&disc),
            _ => home = Home::Memory(discs.len()),
        }
        discs.push(Disc {
            name: disc,
            place: Place::Memory(number),
        });
    }
    let memory = server::Server::new(None, given, seed, 0);
    let scrap_dir = scrap_dir.ok_or_else(|| os.no_memory())?;
    let (pressure, log) = os.cache_context();
    let cache = cache::ReadCache::new(os.ceiling_bytes(), seed, pressure, log);
    os.hold_cache_extension(State {
        own: messages,
        workspace,
        discs,
        home,
        memory,
        files: Vec::new(),
        cache,
        next_key: 0,
    })?;
    DECLARED.declare(os, workspace)?;
    os.set_string_variable(SCRAP_DIR, &scrap_dir)
}

/// The variable naming the directory a task leaves data it hands another in.
const SCRAP_DIR: &[u8] = b"Wimp$ScrapDir";

/// The root of the disc `disc`, `HostFS::<disc>.$`; none where there is no
/// room for it.
fn root_of(disc: &[u8]) -> Option<Vec<u8>> {
    let mut value = Vec::new();
    value
        .try_reserve_exact(NAME.len() + disc.len() + b"::.$".len())
        .ok()?;
    value.extend_from_slice(NAME.as_bytes());
    value.extend_from_slice(b"::");
    value.extend_from_slice(disc);
    value.extend_from_slice(b".$");
    Some(value)
}

/// What the relay says of the session's roots.
struct Roots {
    /// The read cache's seed.
    seed: [u8; 16],
    /// Whether the first is the session's own disc.
    own: bool,
    /// Their discs' names.
    discs: Vec<Vec<u8>>,
}

/// The session's roots: none where the session has no relay, or it answers
/// with anything but its roots.
fn discs(os: &mut Os<'_>) -> Roots {
    let none = Roots {
        seed: [0; 16],
        own: false,
        discs: Vec::new(),
    };
    let mut request = Vec::new();
    Request::Roots.encode(&mut request);
    let mut reply = Vec::new();
    if os.relay(&request, &mut reply, false) != Ok(true) {
        return none;
    }
    match Reply::decode(&reply, &Request::Roots) {
        Some(Reply::Roots { seed, own, discs }) => Roots { seed, own, discs },
        _ => none,
    }
}

/// Its service handler: `Service_FSRedeclare`, HostFS registered again.
fn service(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    if regs.r[1] != FS_REDECLARE {
        return Ok(());
    }
    quietly(with(os, |state| state.workspace).and_then(|at| DECLARED.declare(os, at)))
}

/// `*HostFS`: the filing system selected.
fn select(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let workspace = with(os, |state| state.workspace)?;
    let mut regs = Regs::default();
    regs.r[0] = fs_control::SELECT;
    regs.r[1] = workspace + DECLARED.name_at();
    os.perform(X | FS_CONTROL, &mut regs)
}

/// Its finalisation: the filing system removed, every file its server holds
/// let go with its state.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let workspace = with(os, |state| state.workspace)?;
    DECLARED.remove(os, workspace)?;
    os.close_own(own)?;
    os.release_extension::<State>();
    Ok(())
}

/// `request` answered for the disc `place`: its reply, read from the bytes
/// `buffer` is left holding.
///
/// # Errors
///
/// "Failed" where the relay could not carry it or answered with anything
/// that does not answer it.
fn exchange<'b>(
    os: &mut Os<'_>,
    place: Place,
    request: &Request<'_>,
    buffer: &'b mut Vec<u8>,
) -> Result<Reply<'b>, Fail> {
    let mut bytes = Vec::new();
    request.encode(&mut bytes);
    buffer.clear();
    let carried = match place {
        Place::Relay(_) => os.relay(&bytes, buffer, request.changes())?,
        Place::Memory(_) => with(os, |state| state.memory.serve(&bytes, buffer)).is_ok(),
    };
    let buffer: &'b Vec<u8> = buffer;
    match carried.then(|| Reply::decode(buffer, request)).flatten() {
        Some(reply) => Ok(reply),
        None => Err(fail(os, FAILED)),
    }
}

/// The reply `request` gets for `place`, which carries no bytes of a file,
/// or the server's refusal as a value.
fn ask(
    os: &mut Os<'_>,
    place: Place,
    request: &Request<'_>,
) -> Result<Result<Reply<'static>, Refusal>, Fail> {
    let mut buffer = Vec::new();
    let answer = match exchange(os, place, request, &mut buffer)? {
        Reply::Refused(refusal) => return Ok(Err(refusal)),
        Reply::Roots { seed, own, discs } => Reply::Roots { seed, own, discs },
        Reply::Object(object) => Reply::Object(object),
        Reply::Entries { next, entries } => Reply::Entries { next, entries },
        Reply::Opened {
            handle,
            extent,
            writable,
            version,
        } => Reply::Opened {
            handle,
            extent,
            writable,
            version,
        },
        Reply::Wrote(count) => Reply::Wrote(count),
        Reply::Done => Reply::Done,
        Reply::Space { free, total } => Reply::Space { free, total },
        Reply::Bytes(_) => return Err(fail(os, FAILED)),
    };
    Ok(Ok(answer))
}

/// As [`ask`], a refusal the error RISC OS gives for it.
fn must(os: &mut Os<'_>, place: Place, request: &Request<'_>) -> Result<Reply<'static>, Fail> {
    match ask(os, place, request)? {
        Ok(reply) => Ok(reply),
        Err(refusal) => Err(refused(os, refusal)),
    }
}

/// HostFS's own errors, numbered beside its filing system's number as RISC
/// OS's HostFS numbers them, each by the code FileCore gives its kind.
const LOCKED: Error = own_error(code::LOCKED, "Locked");
const EXISTS: Error = own_error(code::EXISTS, "Exists");
const DISC_FULL: Error = own_error(code::DISC_FULL, "DiscFull");
const PROTECTED: Error = own_error(code::WRITE_PROTECTED, "Protected");
const BAD_NAME: Error = own_error(code::BAD_NAME, "BadName");
const AMBIGUOUS: Error = own_error(code::BAD_NAME, "Ambiguous");
const NOT_EMPTY: Error = own_error(code::DIR_NOT_EMPTY, "NotEmpty");
const ACCESS: Error = own_error(code::ACCESS, "Access");
const NO_DISC: Error = own_error(code::DISC_NOT_FOUND, "NoDisc");

/// HostFS's error `token`, of FileCore's kind `code`.
const fn own_error(code: u8, token: &'static str) -> Error {
    Error {
        number: numbered(NUMBER, code),
        token,
    }
}
const TOO_LARGE: Error = Error {
    number: UNSUPPORTED.number,
    token: "TooLarge",
};
const UNKEPT: Error = Error {
    number: UNSUPPORTED.number,
    token: "Unkept",
};
const FAILED: Error = Error {
    number: UNSUPPORTED.number,
    token: "Failed",
};

/// `error` from HostFS's own file.
fn fail(os: &mut Os<'_>, error: Error) -> Fail {
    os.own_error(own, error, &[])
}

/// The error RISC OS gives for what the server refused.
fn refused(os: &mut Os<'_>, refusal: Refusal) -> Fail {
    use tairix_riscos_kernel::filing::{IS_A_DIRECTORY, NOT_FOUND, NO_DIRECTORY};
    match refusal {
        Refusal::NotFound => os.error(NOT_FOUND),
        Refusal::IsADirectory => os.error(IS_A_DIRECTORY),
        Refusal::NotADirectory => os.error(NO_DIRECTORY),
        Refusal::Locked => fail(os, LOCKED),
        Refusal::Exists => fail(os, EXISTS),
        Refusal::DiscFull => fail(os, DISC_FULL),
        Refusal::WriteProtected => fail(os, PROTECTED),
        Refusal::BadName => fail(os, BAD_NAME),
        Refusal::Ambiguous => fail(os, AMBIGUOUS),
        Refusal::NotEmpty => fail(os, NOT_EMPTY),
        Refusal::AccessViolation => fail(os, ACCESS),
        Refusal::TooLarge => fail(os, TOO_LARGE),
        Refusal::Unrepresentable => fail(os, UNKEPT),
        Refusal::Unknown | Refusal::Failed => fail(os, FAILED),
    }
}
