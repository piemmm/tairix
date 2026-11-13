//! The clean-room ROM images `beeb.app` carries, compiled into `Run` so the
//! front end needs no filesystem authority to fit them. `cargo xtask beeb
//! --write-roms` writes them into this crate's `firmware/` from the sources
//! in `rom`, and the `beeb-roms` gate refuses one the tree does not build.

/// The crate's directory the images are committed in.
pub const DIR: &str = "firmware";
/// The MOS's file.
pub const MOS: &str = "mos.rom";
/// BBC BASIC's file.
pub const BASIC: &str = "basic.rom";
/// The DFS's file.
pub const DFS: &str = "dfs.rom";

/// The MOS, the machine's operating system.
pub const MOS_IMAGE: &[u8] = include_bytes!("../firmware/mos.rom");
/// BBC BASIC, the language in the top sideways slot.
pub const BASIC_IMAGE: &[u8] = include_bytes!("../firmware/basic.rom");
/// The disc filing system, in the slot below BASIC's.
pub const DFS_IMAGE: &[u8] = include_bytes!("../firmware/dfs.rom");

/// Each image by its file's name, the one list the gate and the tests read.
pub const IMAGES: [(&str, &[u8]); 3] = [(MOS, MOS_IMAGE), (BASIC, BASIC_IMAGE), (DFS, DFS_IMAGE)];
