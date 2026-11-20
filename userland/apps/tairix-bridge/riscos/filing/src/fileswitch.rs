//! FileSwitch (`plans/RISCOS-EMULATOR.md` §6): the default of the seven
//! filing vectors while it is active, over the filing systems registered
//! with it.
//!
//! A filing system is only ever handed an absolute path — its disc, if it
//! named one, the root and the rest — with a special field apart: FileSwitch
//! keeps each filing system's directories, resolves `@`, `^`, `&`, `%`, `\`
//! and wildcards itself, and buffers open files, as RISC OS 5's FileSwitch
//! is recorded doing (`tests/corpus/filing-protocol.scenarios`). Everything
//! it hands a filing system — a path, a special field, a block it reads into
//! — is built below the supervisor stack, so a filing system that calls
//! FileSwitch again finds its own untouched.
//!
//! - [`system`]: the registered filing systems and their entry points.
//! - [`name`]: names parsed and resolved to a filing system and a path.
//! - [`open`]: open files, their handles and buffers.
//! - The SWIs: `file` (`OS_File`), `find`, `args`, `bytes` (`OS_BGet`,
//!   `OS_BPut`), `gbpb` and `control` (`OS_FSControl`).

mod args;
mod bytes;
mod call;
mod commands;
mod control;
mod directory;
pub(crate) mod error;
pub(crate) mod file;
mod find;
mod gbpb;
mod image;
mod listing;
pub mod name;
pub mod open;
mod run;
pub mod system;
mod tree;
mod types;

use alloc::vec::Vec;

use tairix_riscos_kernel::module::{Builtin, Resource, Services, VectorDefault};
use tairix_riscos_kernel::msgtrans::{Own, OWN_DESCRIPTOR, REQUEST};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs, Unwind, World};
use tairix_riscos_kernel::vectors;

use name::Directories;
use open::Files;
use system::System;

/// FileSwitch's own messages: those the filing vectors' defaults do not
/// share with it.
const MESSAGES: &[u8] = include_bytes!("../resources/FileSwitch/Messages");
/// Its file's name, which its descriptor keeps.
const MESSAGES_NAME: &[u8] = b"Resources:$.Resources.FileSwitch.Messages\0";

/// Where its workspace keeps the messages descriptor, the file's name and an
/// error's request.
const OWN_AT: u32 = 0;
const NAME_AT: u32 = OWN_DESCRIPTOR;
const REQUEST_AT: u32 = (NAME_AT + len32(MESSAGES_NAME.len())).next_multiple_of(4);
const WORKSPACE: u32 = REQUEST_AT + REQUEST;

/// FileSwitch.
pub static FILE_SWITCH: Builtin = Builtin {
    title: "FileSwitch",
    help_name: None,
    bridge: true,
    swis: None,
    services: Some(Services {
        numbers: &[service::CLOSE_FILE],
        handler: serve,
    }),
    commands: &commands::COMMANDS,
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[crate::script::resumed],
    vectors: &[
        VectorDefault {
            vector: vectors::FILE,
            code: file::file,
        },
        VectorDefault {
            vector: vectors::ARGS,
            code: args::args,
        },
        VectorDefault {
            vector: vectors::BGET,
            code: bytes::bget,
        },
        VectorDefault {
            vector: vectors::BPUT,
            code: bytes::bput,
        },
        VectorDefault {
            vector: vectors::GBPB,
            code: gbpb::gbpb,
        },
        VectorDefault {
            vector: vectors::FIND,
            code: find::find,
        },
        VectorDefault {
            vector: vectors::FSC,
            code: control::fs_control,
        },
    ],
    resources: &[Resource {
        name: "Resources.FileSwitch.Messages",
        file_type: 0xFFF,
        data: MESSAGES,
    }],
    code: None,
};

/// What FileSwitch holds while it is active.
pub struct State {
    own: Own,
    /// The filing systems, in the order they were registered.
    systems: Vec<System>,
    /// The current filing system's number, if one is selected.
    current: Option<u8>,
    /// The filing system `OS_FSControl` 11 selected from a prefix, until 19
    /// gives the current one back; a prefix on a name serves that name alone,
    /// as RISC OS 5's does.
    prefixed: Option<u8>,
    /// The current filing system that was removed, current again should it
    /// return before another is selected.
    removed_current: Option<u8>,
    /// Each filing system's directories, by its number.
    directories: Vec<(u8, Directories)>,
    files: Files,
    /// The image filing systems, by the file types they read.
    images: Vec<system::ImageSystem>,
    /// The images open, each on its host.
    open_images: Vec<image::OpenImage>,
}

impl State {
    fn new(own: Own) -> Self {
        Self {
            own,
            systems: Vec::new(),
            current: None,
            prefixed: None,
            removed_current: None,
            directories: Vec::new(),
            files: Files::new(),
            images: Vec::new(),
            open_images: Vec::new(),
        }
    }

    /// The filing system numbered `number`.
    /// The temporary filing system: what a name with no prefix resolves on,
    /// and what the calls that read "the filing system" read — the one a
    /// prefix selected, else the current one.
    pub(crate) fn temporary(&self) -> Option<u8> {
        self.prefixed.or(self.current)
    }

    pub(crate) fn system(&self, number: u8) -> Option<&System> {
        self.systems.iter().find(|s| s.number == number)
    }
}

/// FileSwitch's own message file in the world, while it is active.
fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

/// The services FileSwitch issues and takes.
mod service {
    /// `Service_CloseFile`: an object and everything beneath it to be closed.
    pub(super) const CLOSE_FILE: u32 = 0x68;
    /// `Service_LookupFileType`, offered a type no variable names.
    pub(super) const LOOKUP_FILE_TYPE: u32 = 0x42;
}

/// The variables FileSwitch gives a value where none is set, as RISC OS 5
/// starts with them: the file type names, the run and load actions of the
/// types FileSwitch does not run itself, and the options `*Copy`, `*Wipe` and
/// `*Count` take where they are given none.
const DEFAULTS: &[(&[u8], &[u8])] = &[
    (b"Alias$@RunType_FFF", b"Type %0 "),
    (b"Alias$@RunType_FFE", b"Exec %0 "),
    (b"Alias$@RunType_FFB", b"Basic -quit \"%0\" %*1"),
    (b"Alias$@RunType_FFA", b"RMRun %*0"),
    (b"Alias$@RunType_FF9", b"ScreenLoad %0 "),
    (b"Alias$@RunType_FEB", b"Obey %0 "),
    (b"Alias$@RunType_FEA", b"Desktop -file %*0"),
    (
        b"Alias$@RunType_FD7",
        b"TaskWindow \"Obey %*0\" -name \"Task Obey\" -quit",
    ),
    (
        b"Alias$@RunType_FD6",
        b"TaskWindow \"Exec %*0\" -name \"Task Exec\" -display",
    ),
    (b"Alias$@LoadType_FFB", b"Basic -load \"%0\" %*1"),
    (b"Alias$@LoadType_FFA", b"RMLoad %*0"),
    (b"Alias$@LoadType_FF9", b"SLoad %0 "),
    (b"File$Type_FFF", b"Text"),
    (b"File$Type_FFE", b"Command"),
    (b"File$Type_FFD", b"Data"),
    (b"File$Type_FFC", b"Utility"),
    (b"File$Type_FFB", b"BASIC"),
    (b"File$Type_FFA", b"Module"),
    (b"File$Type_FF9", b"Sprite"),
    (b"File$Type_FF8", b"Absolute"),
    (b"File$Type_FF7", b"BBC font"),
    (b"File$Type_FF6", b"Font"),
    (b"File$Type_FF5", b"PoScript"),
    (b"File$Type_FF4", b"Printout"),
    (b"File$Type_FF2", b"Config"),
    (b"File$Type_FED", b"Palette"),
    (b"File$Type_FEC", b"Template"),
    (b"File$Type_FEB", b"Obey"),
    (b"File$Type_FEA", b"Desktop"),
    (b"File$Type_FE4", b"DOS"),
    (b"File$Type_FD7", b"TaskObey"),
    (b"File$Type_FD6", b"TaskExec"),
    (b"File$Type_FD1", b"BASICTxt"),
    (b"File$Type_AFF", b"DrawFile"),
    (b"File$Type_BBC", b"BBC ROM"),
    (b"Copy$Options", COPY_OPTIONS),
    (b"Wipe$Options", WIPE_OPTIONS),
    (b"Count$Options", COUNT_OPTIONS),
];

/// The options `*Copy`, `*Wipe` and `*Count` take where their variables are
/// unset, as RISC OS 5 sets the variables.
const COPY_OPTIONS: &[u8] = b"A C ~D ~F ~L ~N ~P ~Q ~R ~S ~T V";
const WIPE_OPTIONS: &[u8] = b"C ~F ~R V";
const COUNT_OPTIONS: &[u8] = b"~C R ~V";

/// `work` on FileSwitch's state: "no filing system" while it has none, as
/// only a finalisation part-way through could leave.
pub(crate) fn with<T>(os: &mut Os<'_>, work: impl FnOnce(&mut State) -> T) -> Result<T, Fail> {
    match os.extension::<State>() {
        Some(state) => Ok(work(state)),
        None => Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT)),
    }
}

/// Its initialisation: its workspace, its state with no filing system, the
/// variables it starts with, and every filing system told to register.
fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(WORKSPACE)?;
    os.write_word(regs.r[12], workspace)?;
    os.write_bytes(workspace + NAME_AT, MESSAGES_NAME)?;
    let own = Own::at(
        workspace + OWN_AT,
        workspace + NAME_AT,
        workspace + REQUEST_AT,
    );
    os.hold_extension(State::new(own))?;
    for &(name, value) in DEFAULTS {
        if os.variable_text(name)?.is_none() {
            os.set_string_variable(name, value)?;
        }
    }
    let mut redeclare = Regs::default();
    redeclare.r[1] = tairix_riscos_kernel::filing::fs::REDECLARE;
    os.service(&mut redeclare)?;
    Ok(())
}

/// Its service handler: `Service_CloseFile`, the object R2's canonical name
/// names closed, and every open file beneath it, R3 counting them.
fn serve(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    match open::close_beneath(os, regs.r[2]) {
        Ok(closed) => regs.r[3] = regs.r[3].wrapping_add(closed),
        Err(Fail::Unwind(unwind)) => return Err(unwind),
        Err(Fail::Error(_)) => {}
    }
    Ok(())
}

/// Its finalisation: every open file closed, its messages closed, and its
/// state let go of.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    open::close_all(os, None)?;
    os.close_own(own)?;
    os.release_extension::<State>();
    Ok(())
}
