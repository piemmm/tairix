//! The `SharedCLibrary` module: its SWIs, the routines its clients' vectors
//! reach, and its errors.

use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::filing::filetype;
use tairix_riscos_kernel::module::{Builtin, Resource, Swis};
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{Fail, Os, Regs, World};
use tairix_riscos_kernel::swi::chunk;

use crate::chunk::ROUTINES;
use crate::libinit::{self, Form, Standard};

const NAMES: &[&str] = &[
    "LibInitAPCS_A",
    "LibInitAPCS_R",
    "LibInitModule",
    "LibInitAPCS_32",
    "LibInitModuleAPCS_32",
];

/// The SWIs by their offset in the chunk.
mod call {
    pub(super) const LIB_INIT_APCS_A: u32 = 0;
    pub(super) const LIB_INIT_APCS_R: u32 = 1;
    pub(super) const LIB_INIT_MODULE: u32 = 2;
    pub(super) const LIB_INIT_APCS_32: u32 = 3;
    pub(super) const LIB_INIT_MODULE_APCS_32: u32 = 4;
}

const MESSAGES: &[u8] = include_bytes!("../resources/Messages");
const MESSAGES_FILE: OwnFile = OwnFile::named(b"Resources:$.Resources.CLib.Messages\0");

pub(crate) static SHARED_C_LIBRARY: Builtin = Builtin {
    title: "SharedCLibrary",
    help_name: Some("C Library"),
    help_suffix: None,
    bridge: false,
    swis: Some(Swis {
        chunk: chunk::SHARED_C_LIBRARY,
        prefix: "SharedCLibrary",
        names: NAMES,
        handler: perform,
    }),
    services: None,
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &ROUTINES,
    vectors: &[],
    resources: &[Resource {
        name: "Resources.CLib.Messages",
        file_type: filetype::TEXT,
        data: MESSAGES,
    }],
    code: Some(crate::chunk::emit),
};

/// What the module keeps: its messages. Everything a client's library holds
/// lies in the client's own memory.
struct State {
    own: Own,
}

fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(MESSAGES_FILE.room())?;
    os.as_caller().write_word(regs.r[12], workspace)?;
    let own = MESSAGES_FILE.place(os, workspace)?;
    os.hold_extension(State { own })
}

fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.close_own(own)?;
    os.release_extension::<State>();
    Ok(())
}

fn perform(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        call::LIB_INIT_APCS_A => Err(raise(os, error::NO_APCS_A, &[])),
        call::LIB_INIT_APCS_R => libinit::lib_init(os, regs, Form::Application, Standard::Apcs26),
        call::LIB_INIT_MODULE => libinit::lib_init(os, regs, Form::Module, Standard::Apcs26),
        call::LIB_INIT_APCS_32 => libinit::lib_init(os, regs, Form::Application, Standard::Apcs32),
        call::LIB_INIT_MODULE_APCS_32 => {
            libinit::lib_init(os, regs, Form::Module, Standard::Apcs32)
        }
        _ => Err(raise(
            os,
            error::SWI_RANGE,
            &[SHARED_C_LIBRARY.title.as_bytes()],
        )),
    }
}

/// The errors the library raises, numbered as RISC OS 5.30's.
pub(crate) mod error {
    use super::Error;

    const fn error(number: u32, token: &'static str) -> Error {
        Error { number, token }
    }

    pub(crate) const UNCAUGHT_TRAP: Error = error(0x80_0E01, "UncaughtTrap");
    pub(crate) const NO_MAIN: Error = error(0x80_0E02, "NoMain");
    pub(crate) const NO_MEMORY: Error = error(0x80_0E80, "NoMemory");
    pub(crate) const UNKNOWN_CHUNK: Error = error(0x80_0E81, "UnknownChunk");
    pub(crate) const STATIC_SIZE: Error = error(0x80_0E83, "StaticSize");
    pub(crate) const SWI_RANGE: Error = error(0x80_0E85, "SwiRange");
    pub(crate) const NOT_THIRTY_TWO: Error = error(0x80_0E86, "NotThirtyTwo");
    pub(crate) const NO_APCS_A: Error = error(0x80_0E86, "NoApcsA");
    pub(crate) const OUT_OF_DATE: Error = error(0x80_0E91, "OutOfDate");
}

/// The text `token` stands for in the library's messages.
pub(crate) fn message(os: &mut Os<'_>, token: &str) -> Result<alloc::vec::Vec<u8>, Fail> {
    message_with(os, token, &[])
}

/// The text `token` stands for in the library's messages, `arguments` in
/// it.
pub(crate) fn message_with(
    os: &mut Os<'_>,
    token: &str,
    arguments: &[&[u8]],
) -> Result<alloc::vec::Vec<u8>, Fail> {
    os.own_text(own, token, arguments)
}

/// The error `error` raises, its text from the library's messages with
/// `arguments` in it.
pub(crate) fn raise(os: &mut Os<'_>, error: Error, arguments: &[&[u8]]) -> Fail {
    os.own_error(own, error, arguments)
}
