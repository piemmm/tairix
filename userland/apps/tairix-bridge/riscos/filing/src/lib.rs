//! TAIRiX Bridge's filing (`plans/RISCOS-EMULATOR.md` §6):
//! FileSwitch, which every filing SWI reaches through the filing vectors,
//! and the filing systems it calls through their entry points.
//!
//! - [`fileswitch`]: FileSwitch — the registered filing systems, names and
//!   the open files — the default of the filing vectors while it is active.
//! - [`hostfs`]: HostFS, the session's files as one filing system.
//! - [`devicefs`]: DeviceFS, the devices drivers register and the streams
//!   opened on them.
//! - [`systemdevices`]: SystemDevices, the screen, keyboard, printer and
//!   null device as filing systems.
//! - [`pipefs`]: PipeFS, named pipes between a reader and a writer.
//! - [`filecore`]: FileCore, each client's discs as a filing system.
//! - [`ramfs`]: RamFS, the RAM disc as FileCore's client.
//! - [`obey`]: Obey, a file of commands run a line at a time.
//! - [`bootcommands`]: `BootCommands`, the boot sequence's commands.
//! - [`boot`]: the boot sequence on the user's own RISC OS disc.
//!
//! A filing system is called through the entry protocol FileSwitch defines,
//! whether it is guest code in the RMA or one of the emulator's built-in
//! modules, whose entries are Rust behind its module image.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod boot;
pub mod bootcommands;
pub(crate) mod declared;
pub mod devicefs;
pub mod filecore;
pub mod fileswitch;
pub mod hostfs;
pub mod mimemap;
pub(crate) mod names;
pub mod obey;
pub mod pipefs;
pub mod ramfs;
pub(crate) mod script;
#[cfg(feature = "test-support")]
pub mod session;
pub mod systemdevices;

use tairix_riscos_kernel::module::Builtin;

/// The built-in modules this crate provides, for the session's
/// `Config::modules`.
pub static MODULES: [&Builtin; 10] = [
    &fileswitch::FILE_SWITCH,
    &hostfs::HOST_FS,
    &filecore::FILE_CORE,
    &ramfs::RAM_FS,
    &obey::OBEY,
    &bootcommands::BOOT_COMMANDS,
    &mimemap::MIME_MAP,
    &devicefs::DEVICE_FS,
    &systemdevices::SYSTEM_DEVICES,
    &pipefs::PIPE_FS,
];
