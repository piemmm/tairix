//! The store worker: the library's record and every machine's state, the ROM
//! images a machine is built from, and the images the user installs, read and
//! written off the loop.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::appdata_ipc::APPDATA_NAME_MAX;
use tairix_appdata::{read_blob, write_blob, RtHost};
use tairix_machines::record::Record;
use tairix_machines::state::{LastScreen, Suspended};
use tairix_machines::store::{self as kept, Resumable, Shelved};
use tairix_openmicro::front::first_screen;
use tairix_openmicro::front::images::{Images, Rom, NO_ROM};
use tairix_openmicro::front::library::{Fitting, Image, Machine, Socket, INSTALLED, STATE_BOUNDS};
use tairix_openmicro::machine::Model;
use tairix_openmicro::worker::protocol::MAX_ROM;
use tairix_rt::work::Worker;
use tairix_rt::File;
use tairix_util::defer::JobQueue;
use tairix_util::fallible;

/// Why a machine's state is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keeping {
    /// It was turned off: its last screen.
    Off,
    /// It resumed: its last screen, its snapshot set aside before it runs.
    SetAside,
    /// It was suspended: its last screen and its snapshot.
    Suspended,
}

/// What the loop asks of the store worker.
pub enum Job {
    /// Read the library, sweep what no machine owns, and draw every
    /// machine's last screen at `picture`.
    Load {
        /// The size the cards draw pictures at.
        picture: Option<(u32, u32)>,
    },
    /// Write a record of the library.
    Record(Record),
    /// Remove machine `id`'s state.
    Remove {
        /// The machine.
        id: u32,
    },
    /// Copy machine `from`'s state to machine `to`.
    Copy {
        /// The machine copied.
        from: u32,
        /// Its copy.
        to: u32,
    },
    /// Keep machine `id`'s last screen, and its snapshot while it is
    /// `suspended`, and draw the screen at `picture`; `why` it is kept.
    Keep {
        /// The machine.
        id: u32,
        /// Its last screen.
        screen: LastScreen,
        /// Its snapshot, while it is suspended.
        suspended: Option<Suspended>,
        /// The size the cards draw pictures at.
        picture: Option<(u32, u32)>,
        /// Why.
        why: Keeping,
    },
    /// Draw these machines' last screens at `picture`, a machine never yet
    /// run its model's boot screen.
    Pictures {
        /// The machines, and the model each is.
        machines: Vec<(u32, Model)>,
        /// The size.
        picture: (u32, u32),
    },
    /// Read the images `fitting` builds a machine from, and its snapshot
    /// where it is `suspended`, for the start `ticket` names.
    Start {
        /// The machine.
        id: u32,
        /// The start.
        ticket: u64,
        /// What it is fitted with.
        fitting: Box<Fitting>,
        /// Whether it resumes.
        suspended: bool,
    },
    /// Keep the ROM image the file `grant` hands over, called `name`, for
    /// `socket`.
    Install {
        /// The file.
        grant: u64,
        /// What it is called.
        name: String,
        /// The socket it was asked for.
        socket: Socket,
    },
}

/// What the store worker answers.
pub enum Done {
    /// The library as the store holds it, and the images installed.
    Loaded(Result<(Shelved<Machine>, Vec<String>), String>),
    /// A record of this generation written, or why not.
    Recorded(Result<u64, String>),
    /// A removal done, or why not.
    Removed(Result<(), String>),
    /// A copy done, or why not.
    Copied {
        /// The machine copied.
        from: u32,
        /// Its copy.
        to: u32,
        /// How it went.
        result: Result<(), String>,
    },
    /// A state kept, or why not, and its card's picture.
    Kept {
        /// The machine.
        id: u32,
        /// Its last screen drawn at the card's size.
        picture: Option<Vec<u8>>,
        /// Why it was kept.
        why: Keeping,
        /// How it went.
        result: Result<(), String>,
    },
    /// Machines' pictures.
    Pictured(Vec<(u32, Vec<u8>)>),
    /// A start's images, and what it resumes from.
    Started {
        /// The machine.
        id: u32,
        /// The start.
        ticket: u64,
        /// What was read.
        read: Result<(Box<Images>, Resumable), String>,
    },
    /// An image installed as blob `blob`, or why not.
    Installed {
        /// The socket it was asked for.
        socket: Socket,
        /// What came of it.
        blob: Result<String, String>,
    },
}

/// The store worker.
pub type StoreWorker = Worker<(), Job, Done, JobQueue<Job, Done>>;

/// Carry out `job`.
pub fn serve(_: &mut (), job: &mut Job) -> Done {
    match job {
        Job::Load { picture } => Done::Loaded(load(*picture)),
        Job::Record(record) => Done::Recorded(kept::write_record(record)),
        Job::Remove { id } => Done::Removed(kept::remove::<Machine>(*id)),
        Job::Copy { from, to } => Done::Copied {
            from: *from,
            to: *to,
            result: kept::copy_state(*from, *to),
        },
        Job::Keep {
            id,
            screen,
            suspended,
            picture,
            why,
        } => Done::Kept {
            id: *id,
            picture: picture.and_then(|size| {
                tairix_crt::screen::thumbnail(screen.width, screen.height, &screen.pixels, size)
            }),
            why: *why,
            result: kept::keep(*id, screen, suspended.as_ref(), &STATE_BOUNDS),
        },
        Job::Pictures { machines, picture } => {
            let ids: Vec<u32> = machines.iter().map(|&(id, _)| id).collect();
            let mut pictures = kept::pictures(&ids, *picture, &STATE_BOUNDS);
            first_screens(&mut pictures, machines.iter().copied(), *picture);
            Done::Pictured(pictures)
        }
        Job::Start {
            id,
            ticket,
            fitting,
            suspended,
        } => Done::Started {
            id: *id,
            ticket: *ticket,
            read: start(fitting, *suspended, *id),
        },
        Job::Install {
            grant,
            name,
            socket,
        } => Done::Installed {
            socket: *socket,
            blob: install(*grant, name),
        },
    }
}

/// The library, what no machine owns swept, and the ROM images still
/// fitted to a machine; one no machine is fitted with goes, so the store
/// keeps only what the library uses.
fn load(picture: Option<(u32, u32)>) -> Result<(Shelved<Machine>, Vec<String>), String> {
    let mut shelved = kept::load::<Machine>(picture, &STATE_BOUNDS)?;
    if let Some(picture) = picture {
        let machines = shelved.loaded.library.machines();
        let models = machines.iter().map(|machine| (machine.id, machine.model()));
        first_screens(&mut shelved.pictures, models, picture);
    }
    let listing = tairix_appdata::blobs::list(&mut RtHost)
        .map_err(|err| format!("the store could not be listed ({err})"))?;
    let fitted = |blob: &str| {
        shelved.loaded.library.machines().iter().any(|machine| {
            machine
                .fitting
                .sockets()
                .any(|(_, image)| matches!(image, Image::Installed(held) if held == blob))
        })
    };
    let mut installed = Vec::new();
    for (name, _) in listing
        .iter()
        .filter(|(name, _)| name.starts_with(INSTALLED))
    {
        let kept = fitted(name) || tairix_appdata::blobs::remove(&mut RtHost, name).is_err();
        if kept && installed.try_reserve(1).is_ok() {
            installed.push(name.clone());
        }
    }
    Ok((shelved, installed))
}

/// Add to `pictures` the boot screen of each of `machines` that has no last
/// screen to show, drawn at `size`: a machine never yet run.
fn first_screens(
    pictures: &mut Vec<(u32, Vec<u8>)>,
    machines: impl Iterator<Item = (u32, Model)>,
    size: (u32, u32),
) {
    let mut drawn: [Option<Option<Vec<u8>>>; Model::ALL.len()] = Default::default();
    for (id, model) in machines {
        if pictures.iter().any(|&(pictured, _)| pictured == id) {
            continue;
        }
        let Some(at) = Model::ALL.iter().position(|&each| each == model) else {
            continue;
        };
        let picture = drawn[at].get_or_insert_with(|| first_screen::picture(model, size));
        // Short of memory, the card shows no picture rather than the app ending.
        if let Some(copy) = picture.as_deref().and_then(fallible::copied) {
            let _ = fallible::push(pictures, (id, copy));
        }
    }
}

/// What a start of a machine fitted so reads: every image, and its snapshot
/// where it is suspended.
fn start(fitting: &Fitting, suspended: bool, id: u32) -> Result<(Box<Images>, Resumable), String> {
    let images = Box::new(images(fitting)?);
    let resume = if suspended {
        kept::resumable(id, &STATE_BOUNDS)?
    } else {
        Resumable::Nothing
    };
    Ok((images, resume))
}

/// The images `fitting` builds a machine from: each `Run` carries, or one
/// read from the store.
fn images(fitting: &Fitting) -> Result<Images, String> {
    Images::of(fitting, &mut |image| match image {
        Image::Installed(blob) => read_blob(blob, MAX_ROM)
            .map(Rom::Read)
            .map_err(|err| format!("{err}")),
        Image::Shipped(shipped) => Ok(Rom::Carried(shipped.image())),
        Image::Empty | Image::Ram => Err(String::from(NO_ROM)),
    })
}

/// The ROM image the file `grant` hands over kept in the store, its blob
/// named from `name` and made unique there.
fn install(grant: u64, name: &str) -> Result<String, String> {
    let file = File::from_delegation(grant).map_err(|err| {
        format!(
            "{name} could not be taken ({})",
            tairix_abi::Errno::from_syscall(err)
        )
    })?;
    let bytes = tairix_rt::read_fd_to_end(file.fd(), MAX_ROM).map_err(|err| {
        format!(
            "{name} could not be read ({})",
            tairix_abi::Errno::from_syscall(err)
        )
    })?;
    if bytes.is_empty() || bytes.len() > MAX_ROM {
        return Err(format!("{name} is not a ROM image of at most 16 KB"));
    }
    let stem: String = name
        .rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .chars()
        .filter_map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9') => Some(c),
            ' ' | '-' | '_' | '.' => Some('-'),
            _ => None,
        })
        .take(APPDATA_NAME_MAX - INSTALLED.len() - 4)
        .collect();
    let stem = stem.trim_matches('-');
    let stem = if stem.is_empty() { "image" } else { stem };
    let listing = tairix_appdata::blobs::list(&mut RtHost)
        .map_err(|err| format!("the store could not be listed ({err})"))?;
    let blob = (1..=99)
        .map(|n| {
            if n == 1 {
                format!("{INSTALLED}{stem}")
            } else {
                format!("{INSTALLED}{stem}-{n}")
            }
        })
        .find(|blob| !listing.iter().any(|(held, _)| held == blob))
        .ok_or_else(|| format!("too many images are called {stem}"))?;
    write_blob(&blob, &bytes)?;
    Ok(blob)
}
