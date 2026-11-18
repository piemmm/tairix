//! Software vectors (`plans/RISCOS-EMULATOR.md` §4.2): ninety-six chains of
//! claimants, the newest called first, each ending at the kernel's default.
//!
//! A claimant is called in a privileged mode with its R12, R14 the address
//! that passes the call on, and on the stack the address that claims it.
//! The default is a claimant like any other, on the host pages, which a
//! claimant may also release; called first, it runs without the processor.
//! Each claim holds a system heap node, so a guest's claims are bounded by
//! its own memory.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use crate::error::Error;
use crate::host::Routine;
use crate::memory::Bits;
use crate::module::{Code, ModuleKey};
use crate::os::{Entry, Fail, Os, Regs, Returned, Target, Unwind};

/// The vectors there are.
pub const COUNT: u32 = 96;

/// A vector's number.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Vector(pub u32);

/// `ErrorV`.
pub const ERROR: Vector = Vector(0x01);
/// `WrchV`.
pub const WRCH: Vector = Vector(0x03);
/// `RdchV`.
pub const RDCH: Vector = Vector(0x04);
/// `CliV`: `OS_CLI`.
pub const CLI: Vector = Vector(0x05);
/// `ByteV`.
pub const BYTE: Vector = Vector(0x06);
/// `WordV`.
pub const WORD: Vector = Vector(0x07);
/// `FileV`: `OS_File`.
pub const FILE: Vector = Vector(0x08);
/// `ArgsV`: `OS_Args`.
pub const ARGS: Vector = Vector(0x09);
/// `BGetV`: `OS_BGet`.
pub const BGET: Vector = Vector(0x0A);
/// `BPutV`: `OS_BPut`.
pub const BPUT: Vector = Vector(0x0B);
/// `GBPBV`: `OS_GBPB`.
pub const GBPB: Vector = Vector(0x0C);
/// `FindV`: `OS_Find`.
pub const FIND: Vector = Vector(0x0D);
/// `ReadLineV`.
pub const READ_LINE: Vector = Vector(0x0E);
/// `FSCV`: `OS_FSControl`.
pub const FSC: Vector = Vector(0x0F);
/// `EventV`.
pub const EVENT: Vector = Vector(0x10);
/// `KeyV`.
pub const KEY: Vector = Vector(0x13);
/// `InsV`.
pub const INS: Vector = Vector(0x14);
/// `RemV`.
pub const REM: Vector = Vector(0x15);
/// `CnpV`.
pub const CNP: Vector = Vector(0x16);
/// UKSWIV.
pub const UKSWI: Vector = Vector(0x18);
/// `MouseV`.
pub const MOUSE: Vector = Vector(0x1A);
/// VDUXV.
pub const VDUX: Vector = Vector(0x1B);
/// `TickerV`.
pub const TICKER: Vector = Vector(0x1C);
/// `UpCallV`.
pub const UPCALL: Vector = Vector(0x1D);
/// `ChangeEnvironmentV`.
pub const CHANGE_ENVIRONMENT: Vector = Vector(0x1E);
/// `SpriteV`: `OS_SpriteOp`.
pub const SPRITE: Vector = Vector(0x1F);
/// `SerialV`.
pub const SERIAL: Vector = Vector(0x24);
/// `GraphicsV`.
pub const GRAPHICS: Vector = Vector(0x2A);
/// `UnthreadV`.
pub const UNTHREAD: Vector = Vector(0x2B);
/// `SeriousErrorV`.
pub const SERIOUS_ERROR: Vector = Vector(0x2D);

/// A vector number out of range, and a release that names no claim.
pub(crate) const BAD_CLAIM_NUMBER: crate::error::Error = crate::error::Error {
    number: 0x1A0,
    token: "BadVectorNumber",
};
pub(crate) const NAFF_RELEASE: crate::error::Error = crate::error::Error {
    number: 0x1A1,
    token: "VectorClaimNotFound",
};

/// A claimant: its code, its R12, the profile it runs under, and the
/// system heap node its claim holds; none for the default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Claimant {
    address: u32,
    workspace: u32,
    bits: Bits,
    node: Option<u32>,
}

/// A vector default the kernel performs: the call's registers in and out,
/// V and R0 an error.
type Default<'o> = for<'a, 'c> fn(&'a mut Os<'o>, &'c mut Regs) -> Result<(), Fail>;

/// The chains.
pub(crate) struct Vectors {
    /// Each vector's claimants, oldest first: the head is the last.
    chains: Vec<Vec<Claimant>>,
    /// The device vectors' claims, oldest first.
    devices: Vec<DeviceClaim>,
    /// Each vector's default where a built-in module is it, by number.
    owners: Vec<Option<Owner>>,
}

/// A built-in module that is a vector's default, and its Rust.
#[derive(Clone, Copy)]
struct Owner {
    module: ModuleKey,
    code: Code,
}

/// A claim's system heap node: room for what it records and a link.
const NODE: u32 = 12;

/// The device numbers the PRM gives, IOMD's and ARM7500's, and the two
/// expansion card devices whose claims also name a status and mask.
const DEVICES: u32 = 27;
const EXPANSION_CARD_DEVICES: [u32; 2] = [8, 13];
/// A device claim's node: its five registers and a link.
const DEVICE_NODE: u32 = 24;

/// "Device number out of range".
const BAD_DEVICE: Error = Error {
    number: 0x1A2,
    token: "BadDeviceNumber",
};
/// "No such claim on the device".
const BAD_DEVICE_RELEASE: Error = Error {
    number: 0x1A3,
    token: "DeviceClaimNotFound",
};

/// A device vector's claim: R0 to R2 of the claim, and R3 and R4 for an
/// expansion card. No device of this machine interrupts, so none is
/// called; the claims are kept as RISC OS keeps them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct DeviceClaim {
    registers: [u32; 5],
    node: u32,
}

impl Vectors {
    /// Every chain holding only its default, at `routine(vector)`.
    pub(crate) fn new(routine: impl Fn(u32) -> u32) -> Result<Self, crate::os::BootError> {
        let mut chains = Vec::new();
        chains
            .try_reserve_exact(COUNT as usize)
            .map_err(|_| crate::os::BootError::NoMemory)?;
        for vector in 0..COUNT {
            let mut chain = Vec::new();
            chain
                .try_reserve(4)
                .map_err(|_| crate::os::BootError::NoMemory)?;
            chain.push(Claimant {
                address: routine(vector),
                workspace: 0,
                bits: Bits::Bit32,
                node: None,
            });
            chains.push(chain);
        }
        let mut owners = Vec::new();
        owners
            .try_reserve_exact(COUNT as usize)
            .map_err(|_| crate::os::BootError::NoMemory)?;
        owners.resize(COUNT as usize, None);
        Ok(Self {
            chains,
            devices: Vec::new(),
            owners,
        })
    }

    /// Makes `code` `vector`'s default while `module` is active.
    pub(crate) fn own(&mut self, vector: Vector, module: ModuleKey, code: Code) {
        if let Some(slot) = self.owners.get_mut(vector.0 as usize) {
            *slot = Some(Owner { module, code });
        }
    }

    /// The kernel's defaults again for the vectors `module` was.
    pub(crate) fn disown(&mut self, module: ModuleKey) {
        for slot in &mut self.owners {
            if slot.is_some_and(|owner| owner.module == module) {
                *slot = None;
            }
        }
    }

    fn owner(&self, vector: Vector) -> Option<Code> {
        let owner = (*self.owners.get(vector.0 as usize)?)?;
        Some(owner.code)
    }
}

/// `ErrorV`'s default: the error handler entered with the error in R0.
fn default_error(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.enter_error_handler(regs.r[0])
}

/// The kernel's default of `vector`; none for a vector it has none for.
fn default_of<'o>(vector: Vector) -> Option<Default<'o>> {
    Some(match vector {
        ERROR => default_error,
        WRCH => Os::default_wrch,
        RDCH => Os::default_rdch,
        CLI => Os::default_cli,
        BYTE => Os::default_byte,
        WORD => Os::default_word,
        FILE => Os::default_file,
        ARGS => Os::default_args,
        BGET | BPUT => Os::default_byte_stream,
        GBPB => Os::default_gbpb,
        FIND => Os::default_find,
        FSC => Os::default_fs_control,
        READ_LINE => Os::default_read_line,
        EVENT => Os::default_event,
        KEY => Os::default_key,
        INS => Os::default_insert,
        REM => Os::default_remove,
        CNP => Os::default_count_purge,
        UKSWI => Os::default_unknown_swi,
        MOUSE => Os::default_mouse,
        UPCALL => Os::default_upcall,
        CHANGE_ENVIRONMENT => Os::adjust_environment,
        _ => return None,
    })
}

impl Os<'_> {
    /// `OS_Claim`: every identical claim removed, then this one made the
    /// head.
    pub(crate) fn claim(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.add_claimant(regs, true)
    }

    /// `OS_AddToVector`: this claim made the head, any identical one left.
    pub(crate) fn add_to_vector(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.add_claimant(regs, false)
    }

    fn add_claimant(&mut self, regs: &Regs, unique: bool) -> Result<(), Fail> {
        let [vector, address, workspace] = [regs.r[0], regs.r[1], regs.r[2]];
        if vector >= COUNT {
            return Err(self.error(BAD_CLAIM_NUMBER));
        }
        if unique {
            loop {
                let chain = &mut self.world.vectors.chains[vector as usize];
                let Some(at) = chain
                    .iter()
                    .position(|c| c.address == address && c.workspace == workspace)
                else {
                    break;
                };
                let removed = chain.remove(at);
                self.free_node(removed.node);
            }
        }
        if self.world.vectors.chains[vector as usize]
            .try_reserve(1)
            .is_err()
        {
            return Err(self.error(crate::error::SYSTEM_HEAP_FULL));
        }
        let node = self.system_node(NODE)?;
        let bits = self.caller_bits();
        self.world.vectors.chains[vector as usize].push(Claimant {
            address,
            workspace,
            bits,
            node: Some(node),
        });
        Ok(())
    }

    /// `OS_ClaimDeviceVector`: the driver claims device R0, a claim the same
    /// as one already there taken off first.
    pub(crate) fn claim_device_vector(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let registers = self.device_claim(regs)?;
        self.drop_device_claim(registers);
        if self.world.vectors.devices.try_reserve(1).is_err() {
            return Err(self.error(crate::error::SYSTEM_HEAP_FULL));
        }
        let node = self.system_node(DEVICE_NODE)?;
        self.world
            .vectors
            .devices
            .push(DeviceClaim { registers, node });
        Ok(())
    }

    /// `OS_ReleaseDeviceVector`: the claim the registers name removed.
    pub(crate) fn release_device_vector(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let registers = self.device_claim(regs)?;
        if self.drop_device_claim(registers) {
            Ok(())
        } else {
            Err(self.error(BAD_DEVICE_RELEASE))
        }
    }

    /// The registers that name a device claim: R0 to R2, and R3 and R4 for
    /// an expansion card.
    fn device_claim(&mut self, regs: &Regs) -> Result<[u32; 5], Fail> {
        let device = regs.r[0];
        if device >= DEVICES {
            return Err(self.error(BAD_DEVICE));
        }
        let mut registers = [device, regs.r[1], regs.r[2], 0, 0];
        if EXPANSION_CARD_DEVICES.contains(&device) {
            registers[3..].copy_from_slice(&regs.r[3..5]);
        }
        Ok(registers)
    }

    /// The newest claim of `registers` taken off; whether there was one.
    fn drop_device_claim(&mut self, registers: [u32; 5]) -> bool {
        let devices = &mut self.world.vectors.devices;
        let Some(at) = devices
            .iter()
            .rposition(|claim| claim.registers == registers)
        else {
            return false;
        };
        let claim = devices.remove(at);
        self.free_system_node(claim.node);
        true
    }

    /// `OS_Release`: the newest claim matching removed.
    pub(crate) fn release(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let [vector, address, workspace] = [regs.r[0], regs.r[1], regs.r[2]];
        let Some(chain) = self.world.vectors.chains.get_mut(vector as usize) else {
            return Err(self.error(NAFF_RELEASE));
        };
        let Some(at) = chain
            .iter()
            .rposition(|c| c.address == address && c.workspace == workspace)
        else {
            return Err(self.error(NAFF_RELEASE));
        };
        let removed = chain.remove(at);
        self.free_node(removed.node);
        Ok(())
    }

    /// Whether anything but the kernel's default answers `vector`.
    pub(crate) fn vector_claimed(&self, vector: Vector) -> bool {
        let default = self.world.routine_address(Routine::Vector(vector.0));
        match self
            .world
            .vectors
            .chains
            .get(vector.0 as usize)
            .map(Vec::as_slice)
        {
            Some([only]) => only.address != default || only.node.is_some(),
            _ => true,
        }
    }

    /// Frees a claim's system heap node.
    fn free_node(&mut self, node: Option<u32>) {
        if let Some(node) = node {
            self.free_system_node(node);
        }
    }

    /// `OS_CallAVector`: the vector in R9 called with R0 to R8 and the
    /// caller's flags, all four flags handed back.
    pub(crate) fn call_a_vector(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.call_vector(Vector(regs.r[9]), regs, Entry::Supervisor)?;
        if regs.flag(psr::V) {
            return Err(Fail::Error(regs.r[0]));
        }
        Ok(())
    }

    /// Calls `vector` with `regs`, claimant by claimant from the head until
    /// one claims the call; `regs` holds what the last left, V and R0 its
    /// error. A vector past the last returns at once.
    pub fn call_vector(
        &mut self,
        vector: Vector,
        regs: &mut Regs,
        entry: Entry,
    ) -> Result<(), Unwind> {
        let Some(chain) = self.world.vectors.chains.get(vector.0 as usize) else {
            return Ok(());
        };
        let default_address = self.world.routine_address(Routine::Vector(vector.0));
        let mut position = chain.len();
        let mut stack: Option<u32> = None;
        while position > 0 {
            let Some(&claimant) = self.world.vectors.chains[vector.0 as usize].get(position - 1)
            else {
                break;
            };
            if claimant.address == default_address && claimant.node.is_none() {
                return self.perform_default(vector, regs);
            }
            let sp = match stack {
                Some(sp) => sp,
                None => self.push_claimed(entry)?,
            };
            let target = Target {
                address: claimant.address,
                bits: claimant.bits,
            };
            let mut call = *regs;
            call.r[12] = claimant.workspace;
            let (returned, left) = self.call_claimant(target, &mut call, entry, sp)?;
            regs.r[..10].copy_from_slice(&call.r[..10]);
            regs.flags = call.flags;
            if returned == Returned::Claimed {
                return Ok(());
            }
            stack = Some(left);
            let chain = &self.world.vectors.chains[vector.0 as usize];
            position = match chain.iter().rposition(|c| *c == claimant) {
                Some(at) => at,
                None => position.min(chain.len() + 1) - 1,
            };
        }
        Ok(())
    }

    /// Pushes the address that claims a call onto the supervisor stack, as
    /// the vector's caller stacks it: the stack pointer after.
    fn push_claimed(&mut self, entry: Entry) -> Result<u32, Unwind> {
        let state = self.world.processor.state();
        let sp = state.banked[1][0].wrapping_sub(4);
        let mut word = self.world.routine_address(Routine::Claimed);
        if self.world.processor.bits() == Bits::Bit26 {
            let entry_psr = (state.cpsr & psr::NZCV)
                | entry.masked(state.cpsr)
                | tairix_riscos_arm::psr::Mode::Supervisor26.bits();
            word = (word & psr::R15_PC) | psr::r15_psr(entry_psr);
        }
        match self.write_word(sp, word) {
            Ok(()) => Ok(sp),
            Err(Fail::Unwind(unwind)) => Err(unwind),
            Err(Fail::Error(_)) => Err(Unwind::Ended(crate::os::Ended::Fault)),
        }
    }

    /// The default of `vector`: the kernel's, or none, which claims the
    /// call and clears V.
    fn perform_default(&mut self, vector: Vector, regs: &mut Regs) -> Result<(), Unwind> {
        let owned: Option<Default<'_>> = self.world.vectors.owner(vector).map(|code| code as _);
        let Some(default) = owned.or_else(|| default_of(vector)) else {
            regs.set_flag(psr::V, false);
            return Ok(());
        };
        match default(self, regs) {
            Ok(()) => {
                regs.set_flag(psr::V, false);
                Ok(())
            }
            Err(Fail::Error(block)) => {
                regs.r[0] = block;
                regs.set_flag(psr::V, true);
                Ok(())
            }
            Err(Fail::Unwind(unwind)) => Err(unwind),
        }
    }

    /// A vector's default entered from guest code, at its host routine: as a
    /// claimant that claims, it performs the default and returns through
    /// the address stacked for it.
    pub(crate) fn vector_default_entered(&mut self, vector: u32) -> Result<(), Unwind> {
        let cpu = self.world.processor.cpu();
        let mut regs = Regs::default();
        for (n, slot) in (0..).zip(regs.r.iter_mut()) {
            *slot = cpu.reg(tairix_riscos_arm::reg::Reg::field(n));
        }
        regs.flags = cpu.cpsr() & psr::NZCV;
        let sp = cpu.reg(tairix_riscos_arm::reg::Reg::SP);
        self.perform_default(Vector(vector), &mut regs)?;
        let back = self
            .peek_word(sp)
            .ok_or(Unwind::Ended(crate::os::Ended::Fault))?;
        let cpu = self.world.processor.cpu();
        for (n, &value) in (0..).zip(&regs.r[..10]) {
            cpu.set_reg(tairix_riscos_arm::reg::Reg::field(n), value);
        }
        cpu.set_nzcv(regs.flags);
        cpu.set_reg(tairix_riscos_arm::reg::Reg::SP, sp.wrapping_add(4));
        let target = if cpu.mode().is_26bit() {
            back & psr::R15_PC
        } else {
            back
        };
        cpu.set_reg(tairix_riscos_arm::reg::Reg::PC, target);
        Ok(())
    }

    /// A SWI that is its vector: the vector called with the caller's flags
    /// and V clear; N and Z come back as the caller's, C as `carry` says,
    /// and V set makes R0 the SWI's error.
    pub(crate) fn vectored(
        &mut self,
        vector: Vector,
        regs: &mut Regs,
        entry: Entry,
        carry: Carry,
    ) -> Result<(), Fail> {
        let caller = regs.flags;
        regs.set_flag(psr::V, false);
        self.call_vector(vector, regs, entry)?;
        let failed = regs.flag(psr::V);
        regs.flags = match carry {
            Carry::Vector => (caller & (psr::N | psr::Z)) | (regs.flags & psr::C),
            Carry::Caller => caller & (psr::N | psr::Z | psr::C),
            Carry::All => regs.flags & (psr::N | psr::Z | psr::C),
        };
        if failed {
            return Err(Fail::Error(regs.r[0]));
        }
        Ok(())
    }

    /// `OS_DelinkApplication`: each claimant in application space taken off
    /// its vector and written to the buffer, highest vector first, then -1.
    /// The end word's room comes off R1 before anything is written, and R1
    /// returns what is left, even below zero. A claimant with no room ends
    /// the list there with Buffer overflow and stays on its vector — the end
    /// word and the error, not the PRM's single byte and zero R1, are what
    /// RISC OS answers.
    pub(crate) fn delink_application(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let mut at = regs.r[0];
        let given = regs.r[1];
        regs.r[1] = given.wrapping_sub(4);
        if given < 4 {
            return Err(self.error(crate::error::BUFFER_OVERFLOW));
        }
        let top = self.application_top();
        let in_application =
            |address: u32| (crate::memory::layout::APPLICATION..=top).contains(&address);
        for vector in (0..COUNT).rev() {
            while let Some(position) = self.world.vectors.chains[vector as usize]
                .iter()
                .rposition(|c| in_application(c.address))
            {
                if regs.r[1] < DELINKED {
                    self.write_word(at, DELINKED_END)?;
                    return Err(self.error(crate::error::BUFFER_OVERFLOW));
                }
                let claimant = self.world.vectors.chains[vector as usize][position];
                for (i, word) in (0..).zip([vector, claimant.address, claimant.workspace]) {
                    self.write_word(at.wrapping_add(4 * i), word)?;
                }
                self.world.vectors.chains[vector as usize].remove(position);
                self.free_node(claimant.node);
                at = at.wrapping_add(DELINKED);
                regs.r[1] -= DELINKED;
            }
        }
        self.write_word(at, DELINKED_END)
    }

    /// `OS_RelinkApplication`: the claimants one delinking wrote added back,
    /// the last written first.
    pub(crate) fn relink_application(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let start = regs.r[0];
        let mut end = start;
        while self.read_word(end)? != DELINKED_END {
            end = end.checked_add(DELINKED).ok_or_else(|| self.aborted(end))?;
        }
        while end > start {
            end -= DELINKED;
            let mut add = Regs::default();
            for (i, slot) in (0..).zip(&mut add.r[..3]) {
                *slot = self.read_word(end + 4 * i)?;
            }
            self.add_to_vector(&mut add)?;
        }
        Ok(())
    }
}

/// A claimant `OS_DelinkApplication` writes: its vector, its code and its
/// R12.
const DELINKED: u32 = 12;
/// What ends a delinked list.
const DELINKED_END: u32 = u32::MAX;

/// Which carry a vectored SWI hands back.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Carry {
    /// The vector's.
    Vector,
    /// The caller's.
    Caller,
    /// N, Z and C all the vector's.
    All,
}

#[cfg(test)]
mod tests;
