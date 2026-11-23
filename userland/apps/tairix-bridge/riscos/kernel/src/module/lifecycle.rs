//! A module's life: linked once its first incarnation is initialised,
//! further incarnations made and killed, the last one's death freeing the
//! module. Initialisation expands a squeezed module and registers its SWI
//! chunk and commands with its first incarnation; a module that refuses to
//! die is put back as it was. A failed initialisation frees what it claimed,
//! and an incarnation that initialised but cannot be recorded for want of
//! memory is finalised again rather than left running unrecorded.

use super::header::{Field, BAD_FIELD};
use super::squeeze::Unsqueezed;
use super::{Incarnation, ModuleKey, Place, Shape};
use crate::error::Error;
use crate::kheap::KernelHeap;
use crate::os::{len32, Entry, Fail, Os, Regs, Target};

/// A squeezed module whose length reaches past the module given, as genuine
/// RISC OS numbers it.
const SQUEEZED_PAST_END: Error = Error {
    number: 0x101,
    token: "SqueezedModuleTruncated",
};

/// The postfix of the incarnation a module is made with.
pub(crate) const BASE: &[u8] = b"Base";

/// "The module is in use and cannot be killed".
pub(crate) const CANT_KILL: Error = Error {
    number: 0x104,
    token: "ModuleInUse",
};

/// An entry a module's life calls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Point {
    Init,
    Final,
}

/// The heap request `OS_Module` 6 makes for `bytes`: a block of a multiple
/// of 32 bytes, its header included.
pub(crate) const fn rma_request(bytes: u32) -> u32 {
    (bytes.saturating_add(35) & !31) - 4
}

impl Os<'_> {
    /// Links a module of `shape` at `place` once its first incarnation,
    /// `Base`, has been initialised with the environment string at `env`; a
    /// module that fails to initialise is freed.
    pub(crate) fn link_and_init(
        &mut self,
        shape: Shape,
        place: Place,
        env: u32,
    ) -> Result<ModuleKey, Fail> {
        let node = match self.incarnation_node(BASE) {
            Ok(node) => node,
            Err(fail) => {
                self.free_code(shape.code, shape.rom);
                return Err(fail);
            }
        };
        let Some(key) = self.world.modules.hold(shape) else {
            self.free_system_node(node);
            self.free_code(shape.code, shape.rom);
            return Err(self.no_memory());
        };
        let incarnation = Incarnation { node };
        if let Err(fail) = self.call_init(key, incarnation, env, 0) {
            if let Some(module) = self.world.modules.drop_module(key) {
                // Code an initialisation that never returned may still have
                // claims pointing into stays.
                if matches!(fail, Fail::Error(_)) {
                    self.free_code(module.shape.code, module.shape.rom);
                }
            }
            return Err(fail);
        }
        if !self.world.modules.link(key, place) {
            let _ = self.kill_and_free(key);
            return Err(self.no_memory());
        }
        Ok(key)
    }

    /// Makes a new incarnation of `key` postfixed `postfix`, initialised with
    /// the environment string at `env`, and makes it the preferred one.
    pub(crate) fn add_incarnation(
        &mut self,
        key: ModuleKey,
        postfix: &[u8],
        env: u32,
    ) -> Result<(), Fail> {
        let node = self.incarnation_node(postfix)?;
        let incarnation = Incarnation { node };
        self.call_init(key, incarnation, env, 0)
    }

    /// Finalises incarnation `index` of `key` and initialises it again with
    /// the environment string at `env`, at its place; the module goes if it
    /// fails and has no other incarnation.
    pub(crate) fn reinitialise(
        &mut self,
        key: ModuleKey,
        index: usize,
        env: u32,
    ) -> Result<(), Fail> {
        let incarnation = self.call_die(key, index)?;
        if let Err(fail) = self.call_init(key, incarnation, env, index) {
            if matches!(fail, Fail::Error(_)) && self.world.modules.empty(key) {
                self.free_module(key);
            }
            return Err(fail);
        }
        Ok(())
    }

    /// Kills incarnation `index` of `key`: finalised, its node freed, and
    /// the module with its last incarnation.
    pub(crate) fn kill_incarnation(&mut self, key: ModuleKey, index: usize) -> Result<(), Fail> {
        let incarnation = self.call_die(key, index)?;
        self.free_system_node(incarnation.node);
        if self.world.modules.empty(key) {
            self.free_module(key);
        }
        Ok(())
    }

    /// Kills every incarnation of `key`, the preferred first, stopping at
    /// the first that refuses.
    pub(crate) fn kill_and_free(&mut self, key: ModuleKey) -> Result<(), Fail> {
        while self.world.modules.get(key).is_some() {
            if self.world.modules.empty(key) {
                self.free_module(key);
                break;
            }
            self.kill_incarnation(key, 0)?;
        }
        Ok(())
    }

    /// Takes `key` out of the chain and frees its code, which a built-in's
    /// image in the host pages never is.
    pub(crate) fn free_module(&mut self, key: ModuleKey) {
        self.release_registrations(key);
        if let Some(module) = self.world.modules.drop_module(key) {
            self.free_code(module.shape.code, module.shape.rom);
        }
    }

    /// Frees a module's code, unless it is a built-in's image; a block the
    /// RMA does not hold is left alone.
    fn free_code(&mut self, code: u32, rom: Option<usize>) {
        if rom.is_none() {
            self.free_rma_quietly(code);
        }
    }

    /// Frees `block` of the RMA, if it is one: for a block a module gives
    /// back of its own, which the guest may have freed already.
    pub fn free_rma_quietly(&mut self, block: u32) {
        let _ = self.heap_free(KernelHeap::Rma, block);
    }

    /// A system heap node for an incarnation postfixed `postfix`: a zero
    /// word, its private word, zero, and the postfix and its terminator.
    pub(crate) fn incarnation_node(&mut self, postfix: &[u8]) -> Result<u32, Fail> {
        let node = self.system_node(8 + len32(postfix.len()) + 1)?;
        let written = self
            .write_word(node, 0)
            .and_then(|()| self.write_word(node + 4, 0))
            .and_then(|()| self.write_terminated(node + 8, postfix, 0, postfix.len() + 1));
        if let Err(fail) = written {
            self.free_system_node(node);
            return Err(fail);
        }
        Ok(node)
    }

    /// Initialises `incarnation` of `key`, R10 the environment string at
    /// `env`, and on success puts it at `at` in the module's list. An error
    /// leaves nothing of the incarnation behind; an
    /// unwind leaves its node and workspace to whatever the code that never
    /// returned holds of them.
    fn call_init(
        &mut self,
        key: ModuleKey,
        incarnation: Incarnation,
        env: u32,
        at: usize,
    ) -> Result<(), Fail> {
        let first = self.world.modules.empty(key);
        let mut outcome = self.initialise(key, incarnation, env, first);
        if outcome.is_ok() && !self.world.modules.attach(key, at, incarnation) {
            let mut regs = Regs::default();
            regs.r[10] = 1;
            regs.r[12] = incarnation.private();
            let _ = self.enter_module(key, Point::Final, &mut regs);
            outcome = Err(self.no_memory());
        }
        if let Err(fail) = outcome {
            if first {
                self.release_registrations(key);
            }
            if let Fail::Error(_) = fail {
                self.free_incarnation(incarnation);
            }
            return Err(fail);
        }
        Ok(())
    }

    /// Frees an incarnation's workspace, if it holds an RMA block, and its
    /// node.
    fn free_incarnation(&mut self, incarnation: Incarnation) {
        let workspace = self.peek_word(incarnation.private()).unwrap_or(0);
        if workspace != 0 {
            self.free_rma_quietly(workspace);
        }
        self.free_system_node(incarnation.node);
    }

    /// An initialisation's steps up to and including the module's own entry.
    fn initialise(
        &mut self,
        key: ModuleKey,
        incarnation: Incarnation,
        env: u32,
        first: bool,
    ) -> Result<(), Fail> {
        self.unsqueeze(key)?;
        let Some(module) = self.world.modules.get(key) else {
            return Err(self.no_memory());
        };
        let shape = module.shape;
        let others = len32(module.incarnations.len());
        let unread = module.services.is_none();
        if first {
            if let Some((chunk, _)) = shape.header.swis() {
                if !self.world.modules.register_chunk(chunk, key) {
                    return Err(self.no_memory());
                }
            }
            if let Some(builtin) = shape.rom.and_then(|rom| self.world.modules.builtin(rom)) {
                for default in builtin.vectors {
                    self.world.vectors.own(default.vector, key, default.code);
                }
            }
        }
        if unread {
            let handler = self.service_handler(&shape);
            if let Some(module) = self.world.modules.get_mut(key) {
                module.services = handler;
            }
        }
        if !self.world.modules.make_room(key) {
            return Err(self.no_memory());
        }
        let mut regs = Regs::default();
        regs.r[10] = env;
        regs.r[11] = if first { shape.hardware } else { others };
        regs.r[12] = incarnation.private();
        // The block may lie in the code an error frees.
        self.enter_module(key, Point::Init, &mut regs)
            .map_err(|fail| match fail {
                Fail::Error(block) => self.copy_error(block),
                Fail::Unwind(unwind) => Fail::Unwind(unwind),
            })
    }

    /// Finalises incarnation `index` of `key`, fatally: refused while the
    /// current active object is in the module's code or the incarnation's
    /// workspace. The incarnation leaves its list while the module's entry
    /// runs, and goes back if it refuses;
    /// once it has died its workspace is freed, and with the last one the
    /// module's SWI chunk.
    fn call_die(&mut self, key: ModuleKey, index: usize) -> Result<Incarnation, Fail> {
        let Some(module) = self.world.modules.get(key) else {
            return Err(self.no_memory());
        };
        let Some(&incarnation) = module.incarnations.get(index) else {
            return Err(self.no_memory());
        };
        let (code, size) = (module.shape.code, module.shape.header.size());
        let active = self.world.env.active_object();
        let workspace = self.peek_word(incarnation.private()).unwrap_or(0);
        let in_workspace = workspace != 0
            && self
                .heap_block_size(KernelHeap::Rma, workspace)
                .is_some_and(|block| active.wrapping_sub(workspace) < block.saturating_sub(4));
        if active.wrapping_sub(code) < size || in_workspace {
            return Err(self.error(CANT_KILL));
        }
        self.world.modules.detach(key, index);
        let mut regs = Regs::default();
        regs.r[10] = 1;
        regs.r[11] = len32(index);
        regs.r[12] = incarnation.private();
        if let Err(fail) = self.enter_module(key, Point::Final, &mut regs) {
            // A module that will not die runs on; should the memory to record
            // it again be gone, its code and workspace stay, called no more.
            self.world.modules.attach(key, index, incarnation);
            return Err(fail);
        }
        if self.world.modules.empty(key) {
            self.release_registrations(key);
        }
        let workspace = self.peek_word(incarnation.private()).unwrap_or(0);
        if workspace != 0 {
            self.free_rma_quietly(workspace);
            let _ = self.poke_word(incarnation.private(), 0);
        }
        Ok(incarnation)
    }

    /// Withdraws `key`'s registration for its SWI chunk and the vectors it is
    /// the default of, as its last incarnation goes.
    fn release_registrations(&mut self, key: ModuleKey) {
        self.world.vectors.disown(key);
        if let Some((chunk, _)) = self
            .world
            .modules
            .get(key)
            .and_then(|m| m.shape.header.swis())
        {
            self.world.modules.release_chunk(chunk, key);
        }
    }

    /// Calls `key`'s `point`: a built-in's Rust, or the module's code in
    /// Supervisor mode with interrupts enabled. A module without the entry
    /// succeeds.
    fn enter_module(&mut self, key: ModuleKey, point: Point, regs: &mut Regs) -> Result<(), Fail> {
        let Some(module) = self.world.modules.get(key) else {
            return Err(self.no_memory());
        };
        let shape = module.shape;
        if let Some(builtin) = shape.rom.and_then(|rom| self.world.modules.builtin(rom)) {
            let code = match point {
                Point::Init => builtin.init,
                Point::Final => builtin.finalise,
            };
            return code.map_or(Ok(()), |code| code(self, regs));
        }
        let offset = match point {
            Point::Init => shape.header.get(Field::Init),
            Point::Final => shape.header.final_entry(),
        };
        if offset == 0 {
            return Ok(());
        }
        let target = Target {
            address: shape.code.wrapping_add(offset),
            bits: shape.bits,
        };
        self.call(target, regs, Entry::SupervisorIrq)?;
        if regs.flag(tairix_riscos_arm::psr::V) {
            return Err(Fail::Error(regs.r[0]));
        }
        Ok(())
    }

    /// Expands `key`'s code into a new block of the RMA if it is squeezed,
    /// the squeezed block freed: an initialisation's first step.
    fn unsqueeze(&mut self, key: ModuleKey) -> Result<(), Fail> {
        let Some(shape) = self.world.modules.get(key).map(|m| m.shape) else {
            return Ok(());
        };
        if shape.header.squeezed().is_none() {
            return Ok(());
        }
        let squeezed = self.read_vec(shape.code, shape.header.size() as usize)?;
        let most = self.memory.layout().rma.len();
        let expanded = match super::squeeze::unsqueeze(&squeezed, most) {
            Ok(expanded) => expanded,
            Err(Unsqueezed::PastItsEnd) => return Err(self.error(SQUEEZED_PAST_END)),
            Err(Unsqueezed::Malformed) => return Err(self.error(BAD_FIELD)),
            Err(Unsqueezed::TooLarge) => return Err(self.error(KernelHeap::Rma.full())),
        };
        drop(squeezed);
        let size = len32(expanded.len());
        let block = self.heap_claim(KernelHeap::Rma, rma_request(size))?;
        let checked = self
            .write_bytes(block, &expanded)
            .and_then(|()| self.checked_header(block, size));
        let (header, bits) = match checked {
            Ok((header, bits)) if header.squeezed().is_none() => (header, bits),
            Ok(_) => {
                self.free_rma_quietly(block);
                return Err(self.error(BAD_FIELD));
            }
            Err(fail) => {
                self.free_rma_quietly(block);
                return Err(fail);
            }
        };
        self.free_code(shape.code, shape.rom);
        if let Some(module) = self.world.modules.get_mut(key) {
            module.shape.code = block;
            module.shape.header = header;
            module.shape.bits = bits;
        }
        Ok(())
    }

    /// The error block at `block` copied into the kernel's ring, so it
    /// outlives the code it may lie in; a block that cannot be read is
    /// passed on as it is, to be reported as one.
    pub(crate) fn copy_error(&mut self, block: u32) -> Fail {
        let Some(number) = self.peek_word(block) else {
            return Fail::Error(block);
        };
        match self.read_until(block.wrapping_add(4), crate::error::TEXT, |b| b == 0) {
            Ok((text, true)) => self.raw_error(number, &text),
            _ => Fail::Error(block),
        }
    }
}
