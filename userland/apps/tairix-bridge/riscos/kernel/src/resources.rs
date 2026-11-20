//! The Messages module (`plans/RISCOS-EMULATOR.md` §4.6): the ROM's
//! resource files — each built-in module's own and those shared — laid out
//! once as one ResourceFS block in the host pages, registered while it
//! lives and again whenever ResourceFS starts.

use alloc::vec::Vec;

use crate::memory::{Memory, MemoryError};
use crate::module::{Builtin, Resource, Services};
use crate::os::{len32, Fail, Os, Regs, Unwind};
use crate::resourcefs::{DEREGISTER_FILES, REGISTER_FILES};
use crate::swi::X;

/// `Service_ResourceFSStarting`: R2 the registration to call, R3 its R3.
const STARTING: u32 = 0x60;
/// A resource's attributes: readable and writable by its owner.
const ATTRIBUTES: u32 = 3;

/// The resources no one built-in module owns: the messages every module
/// shares, and the Wimp's file-type sprites, drawn from the desktop's icon
/// masters by `cargo xtask riscos --write-sprites`.
static SHARED: [Resource; 2] = [
    Resource {
        name: "Resources.Global.Messages",
        file_type: 0xFFF,
        data: crate::msgtrans::GLOBAL,
    },
    Resource {
        name: "Resources.Wimp.Sprites22",
        file_type: 0xFF9,
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
        numbers: &[STARTING],
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
    let words_of = |bytes: usize| bytes.div_ceil(4);
    let total = resources
        .clone()
        .map(|resource| 6 + words_of(resource.name.len() + 1) + words_of(resource.data.len()))
        .sum::<usize>()
        + 1;
    let mut words = Vec::new();
    words.try_reserve_exact(total)?;
    for resource in resources {
        let name_words = words_of(resource.name.len() + 1);
        let entry = 4 * (6 + name_words + words_of(resource.data.len()));
        let length = len32(resource.data.len());
        let load = crate::filing::fs::stamp::typed(u32::from(resource.file_type), load_high);
        words.extend_from_slice(&[len32(entry), load, exec, length, ATTRIBUTES]);
        push_padded(&mut words, resource.name.as_bytes(), name_words);
        words.push(length + 4);
        push_padded(&mut words, resource.data, words_of(resource.data.len()));
    }
    words.push(0);
    Ok(words)
}

/// `bytes` as little-endian words, the last padded with noughts, then
/// nought words to `count`.
fn push_padded(words: &mut Vec<u32>, bytes: &[u8], count: usize) {
    let (whole, tail) = bytes.as_chunks::<4>();
    words.extend(whole.iter().map(|word| u32::from_le_bytes(*word)));
    let mut pushed = whole.len();
    if !tail.is_empty() {
        let mut last = [0; 4];
        last[..tail.len()].copy_from_slice(tail);
        words.push(u32::from_le_bytes(last));
        pushed += 1;
    }
    words.extend(core::iter::repeat_n(0, count.saturating_sub(pushed)));
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
