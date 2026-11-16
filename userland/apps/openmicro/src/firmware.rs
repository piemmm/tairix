//! The clean-room ROM images `OpenMicro.app` carries, compiled into `Run` so the
//! front end needs no filesystem authority to fit them. `cargo xtask openmicro
//! --write-roms` writes them into this crate's `firmware/` from the sources
//! in `rom`, and the `openmicro-roms` gate refuses one the tree does not build.

/// The crate's directory the images are committed in.
pub const DIR: &str = "firmware";
/// The Model B's MOS's file.
pub const MOS: &str = "mos.rom";
/// The Electron's OS's file.
pub const ELECTRON_OS: &str = "electron.rom";
/// BBC BASIC's file.
pub const BASIC: &str = "basic.rom";
/// The DFS's file.
pub const DFS: &str = "dfs.rom";
/// The Plus 1 ROM's file.
pub const PLUS1: &str = "plus1.rom";
/// The Commodore 64's character ROM's file.
pub const C64_CHARACTER: &str = "c64-character.rom";
/// The Commodore 64's BASIC's file.
pub const C64_BASIC: &str = "c64-basic.rom";
/// The Commodore 64's KERNAL's file.
pub const C64_KERNAL: &str = "c64-kernal.rom";
/// The 1541's DOS's file.
pub const C1541_DOS: &str = "c1541-dos.rom";

/// The MOS, the Model B's operating system.
pub const MOS_IMAGE: &[u8] = include_bytes!("../firmware/mos.rom");
/// The Electron's operating system.
pub const ELECTRON_OS_IMAGE: &[u8] = include_bytes!("../firmware/electron.rom");
/// BBC BASIC, the language in the Model B's top sideways slot and the
/// Electron's 10 and 11.
pub const BASIC_IMAGE: &[u8] = include_bytes!("../firmware/basic.rom");
/// The disc filing system, in the slot below BASIC's.
pub const DFS_IMAGE: &[u8] = include_bytes!("../firmware/dfs.rom");
/// The Plus 1's ROM, in the Electron's slot 12.
pub const PLUS1_IMAGE: &[u8] = include_bytes!("../firmware/plus1.rom");
/// The Commodore 64's character ROM: both sets, each with its reverses.
pub const C64_CHARACTER_IMAGE: &[u8] = include_bytes!("../firmware/c64-character.rom");
/// The Commodore 64's BASIC, at `$A000`.
pub const C64_BASIC_IMAGE: &[u8] = include_bytes!("../firmware/c64-basic.rom");
/// The Commodore 64's KERNAL, at `$E000`.
pub const C64_KERNAL_IMAGE: &[u8] = include_bytes!("../firmware/c64-kernal.rom");
/// The 1541's DOS, at `$C000` in the drive.
pub const C1541_DOS_IMAGE: &[u8] = include_bytes!("../firmware/c1541-dos.rom");

/// Each image by its file's name, the one list the gate and the tests read.
pub const IMAGES: [(&str, &[u8]); 9] = [
    (MOS, MOS_IMAGE),
    (ELECTRON_OS, ELECTRON_OS_IMAGE),
    (BASIC, BASIC_IMAGE),
    (DFS, DFS_IMAGE),
    (PLUS1, PLUS1_IMAGE),
    (C64_CHARACTER, C64_CHARACTER_IMAGE),
    (C64_BASIC, C64_BASIC_IMAGE),
    (C64_KERNAL, C64_KERNAL_IMAGE),
    (C1541_DOS, C1541_DOS_IMAGE),
];
