//! The Messages module (`plans/RISCOS-EMULATOR.md` §4.6): the ROM's
//! resource files — each built-in module's own and those shared — laid out
//! once as one ResourceFS block in the host pages, registered while it
//! lives and again whenever ResourceFS starts.

use alloc::vec::Vec;

use crate::filing::filetype;
use crate::memory::{Memory, MemoryError};
use crate::module::{Builtin, Resource, Services};
use crate::os::{Fail, Os, Regs, Unwind};
use crate::resourcefs::{self, DEREGISTER_FILES, REGISTER_FILES};
use crate::swi::X;

/// The resources no one built-in module owns: the messages every module
/// shares, and the Wimp's file-type sprites, drawn from the desktop's icon
/// masters by `cargo xtask riscos --write-sprites`.
static SHARED: [Resource; 2] = [
    Resource {
        name: "Resources.Global.Messages",
        file_type: filetype::TEXT,
        data: crate::msgtrans::GLOBAL,
    },
    Resource {
        name: "Resources.Wimp.Sprites22",
        file_type: filetype::SPRITE,
        data: include_bytes!("../resources/Wimp/Sprites22"),
    },
];

/// The Messages module.
pub(crate) static MESSAGES: Builtin = Builtin {
    title: "Messages",
    help_name: None,
    bridge: false,
    swis: None,
    services: Some(Services {
        numbers: &[crate::service::RESOURCE_FS_STARTING],
        handler: service,
    }),
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[],
    vectors: &[],
    resources: &SHARED,
    code: None,
};

/// One ResourceFS block of `resources`, in order: each entry its offset to
/// the next, its load and execution addresses, length and attributes, its
/// name padded to a word, its length again with four, then its data padded;
/// a zero word after the last.
fn block<'a>(
    resources: impl Iterator<Item = &'a Resource> + Clone,
    stamp: u64,
) -> Result<Vec<u32>, MemoryError> {
    let load_high = u32::try_from(stamp >> 32).unwrap_or(0);
    let exec = u32::try_from(stamp & 0xFFFF_FFFF).unwrap_or(0);
    let total = resources
        .clone()
        .map(|resource| resourcefs::entry_words(resource.name.as_bytes(), resource.data))
        .sum::<usize>()
        + 1;
    let mut words = Vec::new();
    words.try_reserve_exact(total)?;
    for resource in resources {
        let load = crate::filing::fs::stamp::typed(resource.file_type, load_high);
        resourcefs::push_entry(
            &mut words,
            resource.name.as_bytes(),
            resource.data,
            (load, exec),
        )?;
    }
    words.push(resourcefs::BLOCK_END);
    Ok(words)
}

/// Lays the resources of `builtins`, in their order, into the host pages as
/// one block: its address.
pub(crate) fn install(
    memory: &mut Memory,
    builtins: &[(usize, &'static Builtin)],
) -> Result<u32, MemoryError> {
    let resources = builtins.iter().flat_map(|&(_, builtin)| builtin.resources);
    let words = block(resources, crate::release::ROM_STAMP)?;
    memory.install_host(&words)
}

fn init(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[0] = os.world.rom_resources;
    os.perform(X | REGISTER_FILES, &mut regs)
}

fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[0] = os.world.rom_resources;
    os.perform_quietly(X | DEREGISTER_FILES, &mut regs)?;
    Ok(())
}

/// ResourceFS starting: the block registered through the routine it gives.
fn service(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    let mut register = Regs::default();
    register.r[0] = os.world.rom_resources;
    register.r[3] = regs.r[3];
    let target = crate::os::Target {
        address: regs.r[2],
        bits: crate::memory::Bits::Bit32,
    };
    os.call(target, &mut register, crate::os::Entry::Supervisor)?;
    Ok(())
}

#[cfg(test)]
mod tests;
