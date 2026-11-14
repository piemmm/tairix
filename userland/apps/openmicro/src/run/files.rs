//! The file worker: every file a medium comes from, read and written back,
//! the application's documents folder and each machine's folder in it, and
//! each machine's sound stream, opened off the loop.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::audio::{OpenParams, StreamRole, Tenancy};
use tairix_abi::driver::audio::{ChannelMap, Frames, Rate, SampleFormat, StreamDirection};
use tairix_abi::fs::{FileKind, OpenFlags, UnlinkFlags, FS_NEW_DIR_MODE, FS_NEW_FILE_MODE};
use tairix_abi::Errno;
use tairix_audio::live::{LiveStream, RtAudio};
use tairix_fscopy::{DataCopy, Sparse};
use tairix_openmicro::front::folder::{self, MOST_NAMES};
use tairix_openmicro::front::media::Purpose;
use tairix_openmicro::front::pacing::Ring;
use tairix_openmicro::worker::protocol::{MAX_BULK, MAX_DISC, MAX_ROM, SAMPLE_RATE};
use tairix_rt::work::Worker;
use tairix_rt::{Dir, File};
use tairix_util::defer::JobQueue;
use tairix_window::app;

use crate::program::{APP_NAME, LATENCY_FRAMES, LEAST_RING};

/// The file worker's own state: the files the media came from and the
/// documents folder, once granted.
pub struct Held {
    next: u32,
    files: Vec<(u32, File)>,
    documents: Option<Dir>,
}

impl Held {
    /// No file held.
    pub const fn new() -> Self {
        Self {
            next: 0,
            files: Vec::new(),
            documents: None,
        }
    }
}

/// What the loop asks of the file worker.
pub enum FileJob {
    /// The application's documents folder, its delegation redeemed.
    Documents(u64),
    /// A file handed over for machine `id`, read whole and kept to be written
    /// back to while `writable`.
    Take {
        /// The machine.
        id: u32,
        /// The file's delegation.
        grant: u64,
        /// Whether it may be written.
        writable: bool,
        /// What it is called.
        name: String,
        /// What it is to be.
        purpose: Purpose,
    },
    /// A file the application was handed, for machine `id`.
    Handed {
        /// The machine.
        id: u32,
        /// The file, until it is taken.
        file: Option<File>,
        /// Whether it may be written.
        writable: bool,
        /// What it is called.
        name: String,
        /// What it is to be.
        purpose: Purpose,
    },
    /// An image written back over the file a medium came from.
    WriteBack {
        /// The medium.
        medium: u32,
        /// Its image.
        bytes: Vec<u8>,
    },
    /// Bytes written into a file chosen to hold them.
    SaveTo {
        /// The file's delegation.
        grant: u64,
        /// What it is to hold.
        bytes: Vec<u8>,
    },
    /// The file a medium came from let go.
    Forget(u32),
    /// A sound stream for machine `id`'s start `ticket`.
    Sound {
        /// The machine.
        id: u32,
        /// The start.
        ticket: u64,
    },
    /// A folder made for machine `id`, called after it.
    MakeFolder {
        /// The machine.
        id: u32,
        /// Its name.
        name: String,
    },
    /// Machine `id`'s folder listed.
    List {
        /// The machine.
        id: u32,
        /// Its folder.
        folder: String,
    },
    /// A blank disc or tape made in machine `id`'s folder, for `purpose`.
    New {
        /// The machine.
        id: u32,
        /// Its folder.
        folder: String,
        /// What it is to be.
        purpose: Purpose,
    },
    /// The file `name` of machine `id`'s folder put in as `purpose`; where
    /// `digest` is given, only as the file of a medium the machine already
    /// holds, and only while it still holds what the digest says.
    FromFolder {
        /// The machine.
        id: u32,
        /// Its folder.
        folder: String,
        /// The file.
        name: String,
        /// What it is to be.
        purpose: Purpose,
        /// What it held when the machine was suspended holding it.
        digest: Option<[u8; 32]>,
    },
    /// Machine `from`'s folder copied for machine `to`, called `name`.
    CopyFolder {
        /// The copy.
        to: u32,
        /// The folder copied.
        folder: String,
        /// The copy's name.
        name: String,
    },
}

/// What the file worker answers.
pub enum FileDone {
    /// The documents folder is reachable, or why not.
    Documents(Result<(), String>),
    /// A file read for machine `id`.
    Taken {
        /// The machine.
        id: u32,
        /// What it is to be.
        purpose: Purpose,
        /// What it is called.
        name: String,
        /// What it holds.
        bytes: Vec<u8>,
        /// The medium it is written back through, while it may be.
        medium: Option<u32>,
        /// Whether it may be written.
        writable: bool,
        /// Whether it is of the machine's folder.
        of_folder: bool,
    },
    /// The file of a medium machine `id` holds from its snapshot, still what
    /// it was: `medium` it is written back through from now on.
    Rebound {
        /// The machine.
        id: u32,
        /// The drive's disc, or the tape.
        purpose: Purpose,
        /// The file's name.
        name: String,
        /// What it holds.
        bytes: Vec<u8>,
        /// The medium.
        medium: u32,
    },
    /// A write done, or why not.
    Written(Result<(), String>),
    /// Machine `id`'s start's sound stream, if one opened.
    Sound {
        /// The machine.
        id: u32,
        /// The start.
        ticket: u64,
        /// The stream.
        speaker: Option<Speaker>,
    },
    /// Machine `id`'s folder made, or why not.
    FolderMade {
        /// The machine.
        id: u32,
        /// The folder's name.
        folder: Result<String, String>,
    },
    /// Machine `id`'s folder's names, or why they could not be read.
    Listed {
        /// The machine.
        id: u32,
        /// The names.
        names: Result<Vec<String>, String>,
    },
    /// Machine `to`'s copy of a folder, or why there is none.
    FolderCopied {
        /// The copy.
        to: u32,
        /// The folder's name.
        folder: Result<String, String>,
    },
    /// Nothing to say.
    Nothing,
    /// A file job failed, and why.
    Failed(String),
}

/// The file worker.
pub type FileWorker = Worker<Held, FileJob, FileDone, JobQueue<FileJob, FileDone>>;

fn errno(err: i64) -> Errno {
    Errno::from_syscall(err)
}

/// Carry out `job`.
pub fn serve(held: &mut Held, job: &mut FileJob) -> FileDone {
    match job {
        FileJob::Documents(grant) => FileDone::Documents(match Dir::from_delegation(*grant) {
            Ok(dir) => {
                held.documents = Some(dir);
                Ok(())
            }
            Err(err) => Err(format!("its grant could not be taken ({})", errno(err))),
        }),
        FileJob::Take {
            id,
            grant,
            writable,
            name,
            purpose,
        } => match File::from_delegation(*grant) {
            Ok(file) => keep(
                held,
                *id,
                (file, *writable),
                core::mem::take(name),
                *purpose,
                false,
            ),
            Err(err) => FileDone::Failed(format!("{name} could not be taken ({})", errno(err))),
        },
        FileJob::Handed {
            id,
            file,
            writable,
            name,
            purpose,
        } => match file.take() {
            Some(file) => keep(
                held,
                *id,
                (file, *writable),
                core::mem::take(name),
                *purpose,
                false,
            ),
            None => FileDone::Nothing,
        },
        FileJob::WriteBack { medium, bytes } => FileDone::Written(
            held.files
                .iter()
                .find(|(id, _)| id == medium)
                .ok_or_else(|| String::from("its file is no longer held"))
                .and_then(|(_, file)| overwrite(file, bytes)),
        ),
        FileJob::SaveTo { grant, bytes } => FileDone::Written(
            File::from_delegation(*grant)
                .map_err(|err| format!("the file could not be taken ({})", errno(err)))
                .and_then(|file| overwrite(&file, bytes)),
        ),
        FileJob::Forget(medium) => {
            held.files.retain(|(id, _)| id != medium);
            FileDone::Nothing
        }
        FileJob::Sound { id, ticket } => FileDone::Sound {
            id: *id,
            ticket: *ticket,
            speaker: Speaker::open(),
        },
        FileJob::MakeFolder { id, name } => FileDone::FolderMade {
            id: *id,
            folder: make_folder(held, name),
        },
        FileJob::List { id, folder } => FileDone::Listed {
            id: *id,
            names: list(held, folder),
        },
        FileJob::New {
            id,
            folder,
            purpose,
        } => new_medium(held, *id, folder, *purpose),
        FileJob::FromFolder {
            id,
            folder,
            name,
            purpose,
            digest,
        } => from_folder(
            held,
            (*id, folder),
            core::mem::take(name),
            *purpose,
            *digest,
        ),
        FileJob::CopyFolder { to, folder, name } => FileDone::FolderCopied {
            to: *to,
            folder: copy_folder(held, folder, name),
        },
    }
}

/// The most a file is read to for `purpose`.
const fn most(purpose: Purpose) -> usize {
    match purpose {
        Purpose::Disc(_) => MAX_DISC,
        Purpose::Rom(_) => MAX_ROM,
        Purpose::Tape | Purpose::Snapshot => MAX_BULK,
    }
}

/// `file` read whole for machine `id`, kept to be written back to while
/// `writable` and a medium.
fn keep(
    held: &mut Held,
    id: u32,
    (file, writable): (File, bool),
    name: String,
    purpose: Purpose,
    of_folder: bool,
) -> FileDone {
    let bytes = match read(&file, most(purpose)) {
        Ok(bytes) => bytes,
        Err(why) => return FileDone::Failed(format!("{name} could not be read: {why}")),
    };
    let medium =
        (writable && matches!(purpose, Purpose::Disc(_) | Purpose::Tape)).then(|| hold(held, file));
    FileDone::Taken {
        id,
        purpose,
        name,
        bytes,
        medium,
        writable,
        of_folder,
    }
}

/// `file` held as a medium: its number.
fn hold(held: &mut Held, file: File) -> u32 {
    let medium = held.next;
    held.next = held.next.wrapping_add(1);
    held.files.push((medium, file));
    medium
}

/// `file` read whole, refused past `most` bytes.
fn read(file: &File, most: usize) -> Result<Vec<u8>, String> {
    let bytes =
        tairix_rt::read_fd_to_end(file.fd(), most).map_err(|err| format!("{}", errno(err)))?;
    if bytes.len() > most {
        return Err(format!("it is larger than {most} bytes"));
    }
    Ok(bytes)
}

/// `bytes` written over `file` and kept.
fn overwrite(file: &File, bytes: &[u8]) -> Result<(), String> {
    tairix_rt::fs_write_all(file.fd(), 0, bytes).map_err(|err| format!("{err}"))?;
    file.truncate(bytes.len() as u64)
        .map_err(|err| format!("{}", errno(err)))?;
    file.sync().map_err(|err| format!("{}", errno(err)))
}

const NO_DOCUMENTS: &str = "the documents folder cannot be reached";

/// The names of directory `dir`'s entries, `.` and `..` aside, its regular
/// files alone where `files`.
fn names_of(dir: &Dir, files: bool) -> Result<Vec<String>, String> {
    let stream = dir.read_all().map_err(|err| format!("{}", errno(err)))?;
    let mut names = Vec::new();
    for entry in tairix_abi::fs::DirEntries::new(&stream) {
        let entry = entry.map_err(|err| format!("{err}"))?;
        let Ok(name) = core::str::from_utf8(entry.name) else {
            continue;
        };
        let kept = name != "." && name != ".." && (!files || entry.kind == FileKind::Regular);
        if kept && names.try_reserve(1).is_ok() {
            names.push(String::from(name));
        }
    }
    Ok(names)
}

/// A folder made in the documents folder for a machine called `name`, named
/// after it and apart from what is there.
fn make_folder(held: &Held, name: &str) -> Result<String, String> {
    let documents = held.documents.as_ref().ok_or(NO_DOCUMENTS)?;
    let taken = names_of(documents, false)?;
    let folder = folder::folder_for(name, |candidate| taken.iter().any(|t| t == candidate))
        .ok_or_else(|| String::from("too many folders are called after it"))?;
    documents
        .mkdir(folder.as_bytes(), FS_NEW_DIR_MODE)
        .map_err(|err| format!("{folder} could not be made ({})", errno(err)))?;
    Ok(folder)
}

/// Machine folder `folder`, opened.
fn folder_dir(held: &Held, folder: &str) -> Result<Dir, String> {
    let documents = held.documents.as_ref().ok_or(NO_DOCUMENTS)?;
    documents
        .open_dir(folder.as_bytes())
        .map_err(|err| format!("{folder} could not be opened ({})", errno(err)))
}

fn list(held: &Held, folder: &str) -> Result<Vec<String>, String> {
    names_of(&folder_dir(held, folder)?, true)
}

/// A blank disc or tape made afresh in `folder` for machine `id`, held as a
/// medium.
fn new_medium(held: &mut Held, id: u32, folder: &str, purpose: Purpose) -> FileDone {
    let made = (|| {
        let dir = folder_dir(held, folder)?;
        let taken = names_of(&dir, false)?;
        let (stem, ending, bytes) = match purpose {
            Purpose::Disc(_) => ("Disc", ".ssd", folder::blank_disc()),
            _ => ("Tape", ".uef", folder::blank_tape()),
        };
        let name = folder::fresh(stem, ending, |candidate| {
            taken.iter().any(|t| t == candidate)
        })
        .ok_or_else(|| format!("{MOST_NAMES} {stem}s are already there"))?;
        let flags = OpenFlags::READ
            .union(OpenFlags::WRITE)
            .union(OpenFlags::EXCLUSIVE);
        let file = dir
            .create_file(name.as_bytes(), flags, FS_NEW_FILE_MODE)
            .map_err(|err| format!("{name} could not be made ({})", errno(err)))?;
        overwrite(&file, &bytes)?;
        Ok::<_, String>((name, bytes, file))
    })();
    match made {
        Ok((name, bytes, file)) => FileDone::Taken {
            id,
            purpose,
            name,
            bytes,
            medium: Some(hold(held, file)),
            writable: true,
            of_folder: true,
        },
        Err(why) => FileDone::Failed(why),
    }
}

/// The file `name` of `folder` for machine `id`: read as `purpose`, opened
/// to be written back to where the user lets it be written; or, given a
/// `digest`, held as the file of the medium the machine has from its
/// snapshot while it still holds that, and only where it may be written.
fn from_folder(
    held: &mut Held,
    (id, folder): (u32, &str),
    name: String,
    purpose: Purpose,
    digest: Option<[u8; 32]>,
) -> FileDone {
    let opened = folder_dir(held, folder).and_then(|dir| {
        let read = OpenFlags::READ.union(OpenFlags::NO_FOLLOW);
        let failed = |err| format!("{name} could not be opened ({})", errno(err));
        match dir.open_file(name.as_bytes(), read.union(OpenFlags::WRITE)) {
            Ok(file) => Ok((file, true)),
            Err(err)
                if digest.is_none()
                    && matches!(errno(err), Errno::PermissionDenied | Errno::ReadOnly) =>
            {
                dir.open_file(name.as_bytes(), read)
                    .map(|file| (file, false))
                    .map_err(failed)
            }
            Err(err) => Err(failed(err)),
        }
    });
    let (file, writable) = match opened {
        Ok(opened) => opened,
        Err(why) => return FileDone::Failed(why),
    };
    let Some(digest) = digest else {
        return keep(held, id, (file, writable), name, purpose, true);
    };
    match read(&file, most(purpose)) {
        Ok(bytes) if folder::digest(&bytes) == digest => FileDone::Rebound {
            id,
            purpose,
            name,
            bytes,
            medium: hold(held, file),
        },
        _ => FileDone::Nothing,
    }
}

/// Bytes a file is copied through at a time.
const COPY_CHUNK: usize = 64 * 1024;

/// Machine folder `folder`'s files copied into a folder made for `name`; a
/// copy cut short takes away what it made.
fn copy_folder(held: &Held, folder: &str, name: &str) -> Result<String, String> {
    let from = folder_dir(held, folder)?;
    let to_name = make_folder(held, name)?;
    let to = folder_dir(held, &to_name)?;
    let mut chunk = Vec::new();
    chunk
        .try_reserve_exact(COPY_CHUNK)
        .map_err(|_| String::from("there is no memory to copy it through"))?;
    chunk.resize(COPY_CHUNK, 0);
    let mut made = Vec::new();
    let copied = names_of(&from, true).and_then(|files| {
        for file in files {
            copy_file((&from, &to), &file, &mut chunk)?;
            if made.try_reserve(1).is_ok() {
                made.push(file);
            }
        }
        Ok(())
    });
    if let Err(why) = copied {
        for file in made {
            let _ = to.unlink(file.as_bytes(), UnlinkFlags::empty(), None);
        }
        if let Some(documents) = held.documents.as_ref() {
            let _ = documents.unlink(to_name.as_bytes(), UnlinkFlags::DIRECTORY, None);
        }
        return Err(why);
    }
    Ok(to_name)
}

/// File `name` of `from` copied afresh into `to`, its holes kept.
fn copy_file((from, to): (&Dir, &Dir), name: &str, chunk: &mut [u8]) -> Result<(), String> {
    let mut source = from
        .open_file(name.as_bytes(), OpenFlags::READ.union(OpenFlags::NO_FOLLOW))
        .map_err(|err| format!("{name} could not be read ({})", errno(err)))?;
    let len = source
        .regular_len()
        .ok()
        .flatten()
        .ok_or_else(|| format!("{name} could not be measured"))?;
    let flags = OpenFlags::WRITE.union(OpenFlags::EXCLUSIVE);
    let mut target = to
        .create_file(name.as_bytes(), flags, FS_NEW_FILE_MODE)
        .map_err(|err| format!("{name} could not be copied ({})", errno(err)))?;
    DataCopy::new(len, Sparse::Always)
        .run(&mut source, &mut target, chunk)
        .map_err(|err| format!("{name} could not be copied ({err:?})"))?;
    target
        .sync()
        .map_err(|err| format!("{name} could not be copied ({})", errno(err)))
}

/// An open sound stream, and how full its ring is kept.
pub struct Speaker {
    audio: RtAudio,
    stream: LiveStream,
    target: u32,
}

impl Speaker {
    /// A 48 kHz 16-bit mono stream, started; none, said, where the seat has
    /// none to give.
    fn open() -> Option<Self> {
        let mut audio = RtAudio::new();
        let params = OpenParams {
            device_id: 0,
            direction: StreamDirection::Playback,
            format: SampleFormat::S16,
            rate: Rate::new(SAMPLE_RATE).ok()?,
            channel_map: ChannelMap::MONO,
            role: StreamRole::Media,
            latency_target_frames: LATENCY_FRAMES,
            tenancy: Tenancy::Caller,
        };
        let mut stream = match LiveStream::open(&mut audio, &params) {
            Ok(stream) => stream,
            Err(why) => {
                app::report(
                    APP_NAME,
                    format_args!("no sound ({why:?}); the machine is silent"),
                );
                return None;
            }
        };
        let grant = stream.client().grant();
        if grant.format != SampleFormat::S16
            || grant.channel_map != ChannelMap::MONO
            || grant.rate.hz() != SAMPLE_RATE
        {
            app::report(
                APP_NAME,
                "the sound stream is not 48 kHz 16-bit mono; the machine is silent",
            );
            let _ = stream.close(&mut audio);
            return None;
        }
        let target = grant
            .granted_latency_frames
            .max(LEAST_RING)
            .min(grant.ring_frames);
        if let Err(err) = stream.client_mut().start(&mut audio, Frames::new(0)) {
            app::report(
                APP_NAME,
                format_args!("the sound stream would not start ({err})"),
            );
            let _ = stream.close(&mut audio);
            return None;
        }
        Some(Self {
            audio,
            stream,
            target,
        })
    }

    /// The port the stream's notices arrive on.
    pub fn port(&self) -> u64 {
        self.stream.notify_port()
    }

    /// How full the ring is, and how full it is kept.
    pub fn ring(&mut self) -> Option<Ring> {
        let queued = self.stream.ring().ok()?.readable_frames().ok()?;
        Some(Ring {
            queued,
            target: self.target,
        })
    }

    /// `samples` queued after what is already queued, at `volume`
    /// thousandths of their level.
    pub fn play(
        &mut self,
        samples: &[u8],
        volume: u16,
        scratch: &mut Vec<u8>,
    ) -> Result<(), Errno> {
        let volume = i32::from(volume);
        scratch.clear();
        scratch.extend(samples.as_chunks::<2>().0.iter().flat_map(|&pair| {
            let level = i32::from(i16::from_le_bytes(pair)) * volume / 1000;
            i16::try_from(level).unwrap_or(0).to_le_bytes()
        }));
        let at = self.stream.ring()?.producer_position()?;
        self.stream.write_at(at, scratch).map(drop)
    }

    /// Every notice taken, so the port's readiness is spent.
    pub fn drain(&mut self) {
        while let Ok(Some(_)) = self.stream.take_notify(&mut self.audio) {}
    }

    /// The stream closed.
    pub fn close(mut self) {
        let _ = self.stream.close(&mut self.audio);
    }
}
