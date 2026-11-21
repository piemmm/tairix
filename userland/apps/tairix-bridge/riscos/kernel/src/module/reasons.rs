//! `OS_Module`'s reasons and `OS_ExitAndDie`, as RISC OS 5 performs them:
//! loading, entering, reinitialising and killing modules, the RMA's claims,
//! incarnations, the chain's enumeration and the ROM's.
//!
//! A guest module may take a built-in's place, as `*RMLoad` replaces a ROM
//! module, but for a bridge: without it the guest would have no way to reach
//! the desktop.

use alloc::vec::Vec;

use super::header::{Field, Header, TITLE_MOST};
use super::lifecycle::rma_request;
use super::name::{self, Given};
use super::{Missing, ModuleKey, Place, Shape};
use crate::error::Error;
use crate::filing::filetype;
use crate::filing::fs::{kind, stamp};
use crate::kheap::KernelHeap;
use crate::os::{len32, Fail, Os, Regs};
use crate::swi::os::file::{LOAD, MAKE_ERROR, READ_INFO};
use crate::swi::X;

/// The start offset's bits that make it a branch rather than an offset.
const BRANCH: u32 = 0xFF00_0003;

/// Where the start entry of a module at `code` is: `start` past it, or —
/// for a start that is no offset, unaligned or with bits in its top byte —
/// word 0, the instruction it is.
pub(super) const fn start_entry(code: u32, start: u32) -> u32 {
    if start & BRANCH == 0 {
        code.wrapping_add(start)
    } else {
        code
    }
}

/// The Podule manager's SWIs `OS_Module` 17 calls, which only a guest's own
/// Podule manager answers.
const PODULE_READ_CHUNK: u32 = 0x4_0283;
const PODULE_CHUNKS_WITH_INFO: u32 = 0x4_028A;
const PODULE_HARDWARE_ADDRESSES: u32 = 0x4_028B;
/// The chunk type of a relocatable module.
const CHUNK_MODULE: u32 = 0x81;

const fn error(number: u32, token: &'static str) -> Error {
    Error { number, token }
}

const NOT_MOD: Error = error(0x100, "NotAModule");
const BAD_REASON: Error = error(0x105, "UnknownModuleReason");
pub(crate) const NO_MORE_MODULES: Error = error(0x108, "ModuleListEnded");
const NO_MORE_INCARNATIONS: Error = error(0x109, "InstanceListEnded");
const POSTFIX_NEEDED: Error = error(0x10A, "InstanceNameMissing");
const INCARNATION_EXISTS: Error = error(0x10B, "InstanceExists");
const CHUNK_NOT_MODULE: Error = error(0x107, "ChunkNotAModule");
const ALIGNMENT: Error = error(0x117, "InvalidAlignment");
/// "Module … refuses to be replaced": a bridge refuses to die for a
/// replacement, as a module refusing finalisation does.
const CANT_REPLACE: Error = error(super::lifecycle::CANT_KILL.number, "ModuleNotReplaceable");

impl Os<'_> {
    /// `OS_Module`.
    pub(crate) fn os_module(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        match regs.r[0] {
            0 => self.module_run(regs),
            1 => self.module_load(regs.r[1]).map(|_| ()),
            2 => self.module_enter(regs),
            3 => self.module_reinit(regs.r[1]),
            4 => {
                let (key, index) = self.named(regs.r[1])?;
                self.kill_incarnation(key, index)
            }
            5 => {
                let (largest, free) = self.heap_describe(KernelHeap::Rma)?;
                regs.r[2] = largest;
                regs.r[3] = free;
                Ok(())
            }
            6 => {
                regs.r[2] = self.rma_claim(regs.r[3])?;
                Ok(())
            }
            7 => self.heap_free(KernelHeap::Rma, regs.r[2]),
            8 => Ok(()),
            9 => self.module_clear(),
            10 => self.module_add_area(regs.r[1]),
            11 => self.module_copy_area(regs.r[1], regs.r[2]),
            12 => self.module_enumerate(regs, false),
            13 => self.rma_extend(regs),
            14 => self.module_new_incarnation(regs.r[1]),
            15 => self.module_rename(regs.r[1], regs.r[2]),
            16 => {
                let (key, index) = self.named(regs.r[1])?;
                self.prefer_incarnation(key, index);
                Ok(())
            }
            17 => self.module_add_podule(regs),
            18 => self.module_lookup(regs),
            19 => self.rom_modules(regs, false),
            20 => self.rom_modules(regs, true),
            21 => self.rom_chain_end(regs),
            22 => self.module_enumerate(regs, true),
            24 => self.rma_claim_aligned(regs),
            _ => Err(self.error(BAD_REASON)),
        }
    }

    /// `OS_ExitAndDie`: the named module's incarnation killed, the current
    /// active object forgotten so it may be, then `OS_Exit` with R0 to R2.
    pub(crate) fn exit_and_die(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.world.env.set_active_object(0);
        let mut delete = Regs::default();
        delete.r[..2].copy_from_slice(&[4, regs.r[3]]);
        self.perform_quietly(X | crate::swi::os::MODULE, &mut delete)?;
        let mut exit = Regs::default();
        exit.r[..3].copy_from_slice(&regs.r[..3]);
        self.perform(X | crate::swi::os::EXIT, &mut exit)
    }

    /// The block `OS_Module` 6 claims for `bytes`, the RMA grown if it must.
    ///
    /// # Errors
    ///
    /// The RMA's "full" error where even growing it leaves no room.
    pub fn rma_claim(&mut self, bytes: u32) -> Result<u32, Fail> {
        self.heap_claim(KernelHeap::Rma, rma_request(bytes))
    }

    /// Frees `block` of the RMA, as `OS_Module` 7 does.
    ///
    /// # Errors
    ///
    /// The heap's error for a block it does not hold.
    pub fn rma_free(&mut self, block: u32) -> Result<(), Fail> {
        self.heap_free(KernelHeap::Rma, block)
    }

    /// `OS_Module` 1: the module file R1 names loaded into the RMA and
    /// linked, initialised with the parameters after its name.
    fn module_load(&mut self, name: u32) -> Result<ModuleKey, Fail> {
        let mut info = Regs::default();
        info.r[..2].copy_from_slice(&[READ_INFO, name]);
        self.perform(crate::swi::os::FILE, &mut info)?;
        if info.r[0] != kind::FILE {
            let mut make = Regs::default();
            make.r[..3].copy_from_slice(&[MAKE_ERROR, name, info.r[0]]);
            // FileSwitch's error for the object, or what it left in R0 if it
            // gave none.
            return Err(match self.perform(crate::swi::os::FILE, &mut make) {
                Ok(()) => Fail::Error(make.r[0]),
                Err(fail) => fail,
            });
        }
        if stamp::type_of(info.r[2]) != Some(filetype::MODULE) {
            return Err(self.error(NOT_MOD));
        }
        let block = self.rma_claim(info.r[4])?;
        let mut load = Regs::default();
        load.r[..4].copy_from_slice(&[LOAD, name, block, 0]);
        if let Err(fail) = self.perform(crate::swi::os::FILE, &mut load) {
            self.free_rma_quietly(block);
            return Err(fail);
        }
        let env = match self.past_name(name) {
            Ok(env) => env,
            Err(fail) => {
                self.free_rma_quietly(block);
                return Err(fail);
            }
        };
        self.link_new(block, env)
    }

    /// `OS_Module` 0: the module R1 names loaded, then entered.
    fn module_run(&mut self, regs: &Regs) -> Result<(), Fail> {
        let key = self.module_load(regs.r[1])?;
        let env = self.past_name(regs.r[1])?;
        self.enter_it(key, env)
    }

    /// `OS_Module` 2: the module R1 names entered with R2's parameters, a
    /// postfix making its incarnation the preferred one for good.
    fn module_enter(&mut self, regs: &Regs) -> Result<(), Fail> {
        let (key, index) = self.named(regs.r[1])?;
        self.prefer_incarnation(key, index);
        self.enter_it(key, regs.r[2])
    }

    /// Enters `key`'s preferred incarnation as the application, its start
    /// entry in User mode with interrupts enabled, R0 the command line —
    /// its title and the parameters at `env` — and R12 its private word;
    /// nothing for a module without one.
    fn enter_it(&mut self, key: ModuleKey, env: u32) -> Result<(), Fail> {
        let Some(module) = self.world.modules.get(key) else {
            return Ok(());
        };
        let shape = module.shape;
        let private = module
            .incarnations
            .first()
            .copied()
            .map_or(0, super::Incarnation::private);
        let start = shape.header.get(Field::Start);
        if start == 0 {
            return Ok(());
        }
        let title = shape.code.wrapping_add(shape.header.get(Field::Title));
        self.start_application(env, shape.code, title)?;
        let entry = start_entry(shape.code, start);
        let command = self.world.env.buffers.command;
        Err(self.enter_application(shape.bits, entry, |user| {
            user[0] = command;
            user[12] = private;
        }))
    }

    /// Makes incarnation `index` of `key` its preferred one.
    pub(crate) fn prefer_incarnation(&mut self, key: ModuleKey, index: usize) {
        if let Some(module) = self.world.modules.get_mut(key) {
            if index < module.incarnations.len() {
                let incarnation = module.incarnations.remove(index);
                module.incarnations.insert(0, incarnation);
            }
        }
    }

    /// `OS_Module` 3: the incarnation R1 names finalised and initialised
    /// again with the parameters after its name; a ROM module not active is
    /// plugged back in and started.
    fn module_reinit(&mut self, address: u32) -> Result<(), Fail> {
        match self.look_up_at(address)? {
            Ok((key, index)) => {
                let env = self.past_name(address)?;
                self.reinitialise(key, index, env)
            }
            Err(Missing::Module) => {
                let (text, _) = self.read_until(address, TITLE_MOST, |b| b <= b' ')?;
                match self.restart_rom(Given::parse(&text).title, true)? {
                    Some(_) => Ok(()),
                    None => Err(self.missing(Missing::Module, address)),
                }
            }
            Err(missing) => Err(self.missing(missing, address)),
        }
    }

    /// `OS_Module` 9: every module but the ROM's and those marked to stay
    /// killed, the last first; the last error, if one refused.
    fn module_clear(&mut self) -> Result<(), Fail> {
        let mut passed: Vec<ModuleKey> = Vec::new();
        let mut refused = None;
        loop {
            let next = self
                .world
                .modules
                .chain()
                .iter()
                .rev()
                .copied()
                .find(|key| !passed.contains(key));
            let Some(key) = next else {
                break;
            };
            let stays = self
                .world
                .modules
                .get(key)
                .is_some_and(|m| m.shape.rom.is_some() || m.shape.header.invincible());
            let passed_over = stays
                || match self.kill_and_free(key) {
                    Ok(()) => false,
                    Err(Fail::Error(block)) => {
                        refused = Some(block);
                        true
                    }
                    Err(unwind) => return Err(unwind),
                };
            if passed_over {
                if passed.try_reserve(1).is_err() {
                    return Err(self.no_memory());
                }
                passed.push(key);
            }
            self.yield_point()?;
        }
        refused.map_or(Ok(()), |block| Err(Fail::Error(block)))
    }

    /// `OS_Module` 10: the module at R1, in a block of the RMA, linked in
    /// the place of any it replaces.
    fn module_add_area(&mut self, code: u32) -> Result<(), Fail> {
        let empty = self.world.modules.empty;
        self.link_new(code, empty).map(|_| ())
    }

    /// `OS_Module` 11: a copy in the RMA of the module at R1, R2 bytes,
    /// linked last.
    fn module_copy_area(&mut self, code: u32, size: u32) -> Result<(), Fail> {
        let (header, bits) = self.checked_header(code, size)?;
        self.admissible(code, &header)?;
        let block = self.rma_claim(size)?;
        let words = size.div_ceil(4).max(1);
        if let Err(fail) = self.copy_bytes(code, block, 4 * words) {
            self.free_rma_quietly(block);
            return Err(fail);
        }
        let shape = Shape {
            code: block,
            header,
            bits,
            rom: None,
            hardware: 0,
        };
        let empty = self.world.modules.empty;
        self.link_and_init(shape, Place::End, empty).map(|_| ())
    }

    /// Links the module at `code`, its size the block's, in the place of
    /// any it replaces, initialised with the parameters at `env`; its block
    /// freed if anything refuses before it is linked.
    fn link_new(&mut self, code: u32, env: u32) -> Result<ModuleKey, Fail> {
        let size = self.peek_word(code.wrapping_sub(4)).unwrap_or(0);
        let prepared = self.checked_header(code, size).and_then(|(header, bits)| {
            let place = self.admissible(code, &header)?;
            let shape = Shape {
                code,
                header,
                bits,
                rom: None,
                hardware: 0,
            };
            Ok((shape, place))
        });
        match prepared {
            Ok((shape, place)) => self.link_and_init(shape, place, env),
            Err(fail) => {
                self.free_rma_quietly(code);
                Err(fail)
            }
        }
    }

    /// Whether the module at `code` may be linked: a title no bridge has,
    /// any namesake killed first; where its namesake was.
    fn admissible(&mut self, code: u32, header: &Header) -> Result<Place, Fail> {
        let title = self.title_at(code, header)?;
        let bridge = self
            .world
            .modules
            .roms
            .iter()
            .find(|rom| rom.builtin.bridge && name::names(&title, rom.builtin.title.as_bytes()))
            .map(|rom| rom.builtin.title);
        if let Some(bridge) = bridge {
            return Err(self.error_with(CANT_REPLACE, &[bridge.as_bytes()]));
        }
        let Some(old) = self.find_module(&title)? else {
            return Ok(Place::End);
        };
        let before = self.world.modules.before(old);
        self.kill_and_free(old)?;
        Ok(Place::After(before))
    }

    /// `OS_Module` 12 and 22: the module R1 places into the chain and its
    /// incarnation R2: its code, its private word's value (or address, for
    /// 22) and its postfix, R1 and R2 moved on to the next incarnation.
    fn module_enumerate(&mut self, regs: &mut Regs, address: bool) -> Result<(), Fail> {
        let place = |n: u32| if n.cast_signed() < 0 { 0 } else { n as usize };
        let (at, index) = (place(regs.r[1]), place(regs.r[2]));
        let Some(module) = self
            .world
            .modules
            .nth(at)
            .and_then(|key| self.world.modules.get(key))
        else {
            return Err(self.error(NO_MORE_MODULES));
        };
        let code = module.shape.code;
        let count = module.incarnations.len();
        let Some(&incarnation) = module.incarnations.get(index) else {
            if count == 0 && index == 0 {
                // A module between incarnations, in its own initialisation
                // or death.
                regs.r[1] = len32(at + 1);
                regs.r[2] = 0;
                regs.r[3] = code;
                regs.r[4] = if address { 0 } else { 0xDEAD_DEAD };
                return Ok(());
            }
            return Err(self.error(NO_MORE_INCARNATIONS));
        };
        regs.r[3] = code;
        regs.r[4] = if address {
            incarnation.private()
        } else {
            self.read_word(incarnation.private())?
        };
        regs.r[5] = incarnation.postfix();
        if index + 1 < count {
            regs.r[1] = len32(at);
            regs.r[2] = len32(index + 1);
        } else {
            regs.r[1] = len32(at + 1);
            regs.r[2] = 0;
        }
        Ok(())
    }

    /// `OS_Module` 13: the block R2 resized by R3, rounded to 32 bytes; a
    /// block grown on a 32-byte boundary moves.
    fn rma_extend(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (block, change) = (regs.r[2], regs.r[3].cast_signed());
        let rounded = change.wrapping_add(31) & !31;
        let Some(held) = self.heap_block_size(KernelHeap::Rma, block) else {
            return Err(self.error(crate::kheap::heap_error(crate::heap::HeapError::NotABlock)));
        };
        if rounded < 0 || !block.is_multiple_of(32) {
            let need = u64::from(held) + u64::from(rounded.max(0).unsigned_abs()) + 8;
            let extended = self.growing(KernelHeap::Rma, need, |os, heap| {
                heap.extend_block(os, block, rounded)
            })?;
            regs.r[2] = extended.unwrap_or(u32::MAX);
            return Ok(());
        }
        let usable = held.saturating_sub(4);
        let moved = self.rma_claim(usable.saturating_add(rounded.unsigned_abs()))?;
        if let Err(fail) = self.copy_bytes(block, moved, usable) {
            self.free_rma_quietly(moved);
            return Err(fail);
        }
        regs.r[2] = moved;
        self.heap_free(KernelHeap::Rma, block)
    }

    /// `OS_Module` 24: a block of R3 bytes aligned to R4, a power of two, at
    /// least 32.
    fn rma_claim_aligned(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let alignment = regs.r[4];
        if !alignment.is_power_of_two() {
            return Err(self.error(ALIGNMENT));
        }
        let alignment = alignment.max(32);
        let size = (regs.r[3].saturating_add(15) & !31) + 16;
        let skew = self.memory.layout().rma.start;
        let need = u64::from(size) + u64::from(alignment) + 8;
        regs.r[2] = self.growing(KernelHeap::Rma, need, |os, heap| {
            heap.get_aligned(os, size, alignment, 0, skew)
        })?;
        Ok(())
    }

    /// `OS_Module` 14: a new incarnation of the module R1 names by its
    /// postfix, initialised with the parameters after it, made the
    /// preferred one; a ROM module not active is started, its base
    /// incarnation given the postfix.
    fn module_new_incarnation(&mut self, address: u32) -> Result<(), Fail> {
        let (text, _) = self.read_until(address, TITLE_MOST, |b| b <= b' ')?;
        let given = Given::parse(&text);
        let Some(postfix) = given.postfix.filter(|postfix| !postfix.is_empty()) else {
            return Err(self.error(POSTFIX_NEEDED));
        };
        match self.find_module(given.title)? {
            Some(key) => {
                if self.find_incarnation(key, postfix)?.is_some() {
                    return Err(self.error(INCARNATION_EXISTS));
                }
                let env = self.past_name(address)?;
                self.add_incarnation(key, postfix, env)
            }
            None => match self.restart_rom(given.title, false)? {
                Some(key) => self.rename(key, 0, postfix),
                None => Err(self.missing(Missing::Module, address)),
            },
        }
    }

    /// `OS_Module` 15: the incarnation R1 names given R2's postfix.
    fn module_rename(&mut self, address: u32, postfix: u32) -> Result<(), Fail> {
        let (key, index) = self.named(address)?;
        let (postfix, _) = self.read_until(postfix, TITLE_MOST, |b| b <= b' ')?;
        if postfix.is_empty() {
            return Err(self.error(POSTFIX_NEEDED));
        }
        if self.find_incarnation(key, &postfix)?.is_some() {
            return Err(self.error(INCARNATION_EXISTS));
        }
        self.rename(key, index, &postfix)
    }

    /// Incarnation `index` of `key` postfixed `postfix`: in its node where
    /// the block holds it, otherwise in a new one, its private word moving
    /// with it.
    fn rename(&mut self, key: ModuleKey, index: usize, postfix: &[u8]) -> Result<(), Fail> {
        let Some(incarnation) = self
            .world
            .modules
            .get(key)
            .and_then(|m| m.incarnations.get(index))
            .copied()
        else {
            return Err(self.no_memory());
        };
        let room = self
            .heap_block_size(KernelHeap::System, incarnation.node)
            .map_or(0, |block| block.saturating_sub(4 + 8) as usize);
        if postfix.len() < room {
            return self.write_terminated(incarnation.postfix(), postfix, 0, room);
        }
        let node = self.incarnation_node(postfix)?;
        let word = self.read_word(incarnation.private())?;
        self.write_word(node + 4, word)?;
        let moved = super::Incarnation { node };
        self.world.modules.moved(key, index, moved);
        self.free_system_node(incarnation.node);
        Ok(())
    }

    /// `OS_Module` 17: the module in chunk R2 of the expansion card R3
    /// linked, R1 its environment string and R11 the card's hardware.
    /// Only a Podule manager the guest itself provides has cards to name.
    fn module_add_podule(&mut self, regs: &Regs) -> Result<(), Fail> {
        let (env, chunk, podule) = (regs.r[1], regs.r[2], regs.r[3]);
        let mut info = Regs::default();
        info.r[0] = chunk;
        info.r[3] = podule;
        self.perform(X | PODULE_CHUNKS_WITH_INFO, &mut info)?;
        if info.r[2] != CHUNK_MODULE {
            return Err(self.error(CHUNK_NOT_MODULE));
        }
        let in_place = info.r[6] != 0;
        let code = if in_place {
            info.r[6]
        } else {
            let block = self.rma_claim(info.r[1])?;
            let mut read = Regs::default();
            read.r[0] = chunk;
            read.r[2] = block;
            read.r[3] = podule;
            if let Err(fail) = self.perform(X | PODULE_READ_CHUNK, &mut read) {
                self.free_rma_quietly(block);
                return Err(fail);
            }
            block
        };
        let size = self.peek_word(code.wrapping_sub(4)).unwrap_or(0);
        let checked = self.checked_header(code, size).and_then(|(header, bits)| {
            self.admissible(code, &header)?;
            Ok((header, bits))
        });
        let (header, bits) = match checked {
            Ok(checked) => checked,
            Err(fail) => {
                if !in_place {
                    self.free_rma_quietly(code);
                }
                return Err(fail);
            }
        };
        let mut hardware = 0;
        if podule.cast_signed() >= 0 {
            let mut addresses = Regs::default();
            addresses.r[3] = podule;
            self.perform(X | PODULE_HARDWARE_ADDRESSES, &mut addresses)?;
            hardware = addresses.r[0];
        }
        let shape = Shape {
            code,
            header,
            bits,
            rom: None,
            hardware,
        };
        self.link_and_init(shape, Place::End, env).map(|_| ())
    }

    /// `OS_Module` 18: the module and incarnation R1 names: their places,
    /// the module's code, the private word's value and the postfix.
    fn module_lookup(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (key, index) = self.named(regs.r[1])?;
        let Some(module) = self.world.modules.get(key) else {
            return Err(self.no_memory());
        };
        let (code, incarnation) = (module.shape.code, module.incarnations.get(index).copied());
        let Some(incarnation) = incarnation else {
            return Err(self.no_memory());
        };
        regs.r[1] = self.world.modules.place_of(key).map_or(0, len32);
        regs.r[2] = len32(index);
        regs.r[3] = code;
        regs.r[4] = self.read_word(incarnation.private())?;
        regs.r[5] = incarnation.postfix();
        Ok(())
    }

    /// `OS_Module` 21: the word after the ROM chain, R1 -1 for the main ROM.
    fn rom_chain_end(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        if regs.r[1] != u32::MAX {
            return Err(self.error(crate::error::BAD_PARAMETERS));
        }
        regs.r[2] = self.world.modules.empty;
        Ok(())
    }
}
