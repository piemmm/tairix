//! The system control coprocessor, CP15, as each profile has it.
//!
//! Translation is the bus's, so the registers that program a guest MMU (the
//! translation table bases, the domains, the memory attributes, address
//! translation) are refused by name rather than accepted and ignored. Cache
//! and TLB maintenance completes: no cache is modelled, so nothing is
//! stale, and an instruction fetch reads the page as it stands. The cache
//! enables may be changed for the same reason; any other change to the
//! control register is refused.

use crate::bus::Abort;
use crate::isa::{Feature, Isa};
use crate::profile::System;

/// What a CP15 access does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Outcome {
    /// It completes, a read with this value.
    Done(u32),
    /// It waits for an interrupt (the SA-110's idle).
    Wait,
    /// It takes the undefined instruction exception: no such register, or
    /// not from this privilege.
    Undefined,
    /// It names a register this processor does not model.
    Unmodelled,
}

/// A data or prefetch abort's cause, as the fault status registers record
/// it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Fault {
    /// A misaligned access the profile faults.
    Alignment,
    /// The bus's refusal.
    Bus(Abort),
    /// `BKPT`'s debug event.
    Debug,
}

/// One level of a processor's caches: separate instruction and data caches,
/// each of `size` bytes in lines of `line`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SplitCache {
    /// Each cache's size, in bytes.
    pub size: u32,
    /// Each cache's line, in bytes.
    pub line: u32,
}

/// The SA-110's caches, as its data sheet gives them.
const SA110_L1: SplitCache = SplitCache {
    size: 16 * 1024,
    line: 32,
};

/// The SA-110's identity: Digital's part `A10`, revision T.
const SA110_ID: u32 = 0x4401_A104;
/// The SA-110's control register as RISC OS 3.7 runs it: the MMU, the
/// caches and write buffer, 32-bit exception handling and data, late aborts,
/// system protection.
const SA110_CONTROL: u32 = 0x0000_117D;
/// The SA-110 control bits that enable a cache or the write buffer.
const SA110_CACHE_BITS: u32 = 1 << 2 | 1 << 3 | 1 << 12;

/// An ARMv7 implementer of zero, reserved for software, with the CPUID
/// identification scheme: a processor that is no particular silicon.
const ARMV7_ID: u32 = 0x000F_0000;
/// The ARMv7 profile's level 1 caches, of four ways each.
const ARMV7_L1: SplitCache = SplitCache {
    size: 32 * 1024,
    line: 64,
};
const ARMV7_L1_WAYS: u32 = 4;
/// `log2` of [`ARMV7_L1`]'s line in words, as the ID registers count it.
const ARMV7_LINE_WORDS: u32 = ARMV7_L1.line.trailing_zeros() - 2;
/// The cache type: the ARMv7 format with PIPT instruction caches, and the
/// level 1 line as the smallest line, reservation and write-back granule.
const ARMV7_CACHE_TYPE: u32 = 0x8000_C000
    | ARMV7_LINE_WORDS << 24
    | ARMV7_LINE_WORDS << 20
    | ARMV7_LINE_WORDS << 16
    | ARMV7_LINE_WORDS;
/// A uniprocessor in the multiprocessing extensions' format.
const ARMV7_MULTIPROCESSOR: u32 = 0xC000_0000;
/// The control register as RISC OS 5 runs it: the MMU, alignment faults,
/// the caches and branch prediction, high vectors, CP15 barriers enabled.
const ARMV7_CONTROL: u32 = 0x00C5_387F;
/// The ARMv7 control bits that enable a cache or branch prediction.
const ARMV7_CACHE_BITS: u32 = 1 << 2 | 1 << 11 | 1 << 12;
/// The control register's CP15 barrier enable.
const ARMV7_CP15_BARRIERS: u32 = 1 << 5;
/// Either level 1 cache's size register: write-back, read- and
/// write-allocate, [`ARMV7_L1`]'s sets, ways and line.
const ARMV7_CACHE_SIZE: u32 = 0x7 << 28
    | (ARMV7_L1.size / (ARMV7_L1_WAYS * ARMV7_L1.line) - 1) << 13
    | (ARMV7_L1_WAYS - 1) << 3
    | (ARMV7_LINE_WORDS - 2);
/// Separate level 1 instruction and data caches, the points of coherency
/// and unification at level 1.
const ARMV7_CACHE_LEVELS: u32 = 0x0920_0003;

/// The processor feature, debug, memory model and instruction set
/// attribute registers, `c0, c1, 0` to `c0, c2, 5`.
const ARMV7_FEATURES: [u32; 14] = [
    // ID_PFR0: ARM, Thumb-2, trivial Jazelle, no ThumbEE.
    0x0000_0131,
    // ID_PFR1: the ARMv4 programmers' model; no Security or Virtualization
    // Extensions and no generic timer.
    0x0000_0001,
    // ID_DFR0: no debug architecture.
    0,
    // ID_AFR0.
    0,
    // ID_MMFR0: VMSAv7, outer shareable, an auxiliary control register.
    0x0010_0103,
    // ID_MMFR1: branch predictor maintenance not required.
    0x4000_0000,
    // ID_MMFR2: WFI, the CP15 barriers.
    0x0120_0000,
    // ID_MMFR3: maintenance by address and by set and way, branch predictor
    // maintenance, broadcast.
    0x0000_1211,
    // ID_ISAR0: no SWP; CLZ, bitfields, CBZ, every coprocessor form, BKPT,
    // divide in both instruction sets.
    0x0214_1110,
    // ID_ISAR1: SETEND, the exception returns, SRS/RFE/CPS, every extend,
    // IT, MOVW/MOVT, interworking ALU writes to the PC, BXJ.
    0x1311_2111,
    // ID_ISAR2: load-acquire, every preload, uninterruptible LDM/STM, MLS,
    // every signed and unsigned multiply, MRS/MSR, RBIT.
    0x2123_2042,
    // ID_ISAR3: saturation, the media instructions, SVC, the exclusives,
    // TBB/TBH, Thumb MOV, true NOP.
    0x0111_2131,
    // ID_ISAR4: the unprivileged transfers, shifts in addressing,
    // writeback, the barriers; no SMC and no SWP.
    0x0001_0142,
    // ID_ISAR5: SEVL and CRC32, and the Cryptographic Extension's
    // instructions where the set has them.
    0x0001_0001,
];

/// `ID_ISAR5`'s AES with the 64-bit polynomial multiply, SHA-1 and SHA-256.
const ARMV7_CRYPTO: u32 = 0x0000_1120;

/// CPACR's access fields for coprocessors 10 and 11.
const VFP_ACCESS: u32 = 0x00F0_0000;

/// The coprocessor's state.
#[derive(Clone, Debug)]
pub(crate) struct Cp15 {
    system: System,
    control: u32,
    data_status: u32,
    data_address: u32,
    instruction_status: u32,
    instruction_address: u32,
    cache_selection: u32,
    context: u32,
    /// TPIDRURW, TPIDRURO and TPIDRPRW.
    thread: [u32; 3],
    /// CPACR.
    access: u32,
    /// The CPACR bits the processor's coprocessors have.
    access_mask: u32,
    /// `ID_ISAR5`.
    isar5: u32,
}

impl Cp15 {
    /// The coprocessor at reset.
    pub(crate) const fn new(system: System, isa: Isa) -> Self {
        Self {
            system,
            control: match system {
                System::Sa110 => SA110_CONTROL,
                System::Armv7 => ARMV7_CONTROL,
            },
            data_status: 0,
            data_address: 0,
            instruction_status: 0,
            instruction_address: 0,
            cache_selection: 0,
            context: 0,
            thread: [0; 3],
            access: 0,
            access_mask: if isa.has(Feature::Vfp) { VFP_ACCESS } else { 0 },
            isar5: ARMV7_FEATURES[13]
                | if isa.has(Feature::Crypto) {
                    ARMV7_CRYPTO
                } else {
                    0
                },
        }
    }

    /// The caches at `level`, from zero; none past the last.
    pub(crate) const fn cache(&self, level: u32) -> Option<SplitCache> {
        match (self.system, level) {
            (System::Sa110, 0) => Some(SA110_L1),
            (System::Armv7, 0) => Some(ARMV7_L1),
            _ => None,
        }
    }

    /// CPACR's two-bit access field for coprocessor `cp`: none, privileged
    /// only, reserved, or full.
    pub(crate) const fn coprocessor_access(&self, cp: u32) -> u32 {
        (self.access >> (2 * (cp & 15))) & 3
    }

    /// CPACR's fields for coprocessors 10 and 11 both made `field`, where
    /// the processor has them.
    pub(crate) const fn set_simdfp_access(&mut self, field: u32) {
        let field = field & 3;
        self.access = (field << 20 | field << 22) & self.access_mask;
    }

    /// Records a data abort at `address`.
    pub(crate) fn data_fault(&mut self, fault: Fault, address: u32, write: bool) {
        self.data_status = self.status(fault)
            | match self.system {
                System::Armv7 if write => 1 << 11,
                _ => 0,
            };
        self.data_address = address;
    }

    /// The last data abort's fault status and address.
    pub(crate) const fn last_data_fault(&self) -> (u32, u32) {
        (self.data_status, self.data_address)
    }

    /// Records a prefetch abort at `address`. The SA-110 records none.
    pub(crate) fn prefetch_fault(&mut self, fault: Fault, address: u32) {
        if self.system == System::Armv7 {
            self.instruction_status = self.status(fault);
            self.instruction_address = address;
        }
    }

    /// The fault status encoding of `fault`, at page level in domain 0.
    const fn status(&self, fault: Fault) -> u32 {
        match (self.system, fault) {
            (_, Fault::Alignment) => 0b0001,
            (_, Fault::Bus(Abort::Translation)) => 0b0111,
            (_, Fault::Bus(Abort::Permission)) => 0b1111,
            (System::Sa110, Fault::Bus(Abort::External)) => 0b1010,
            (System::Armv7, Fault::Bus(Abort::External)) => 0b1000,
            (_, Fault::Debug) => 0b0010,
        }
    }

    /// An `MRC` of the register `opc1, crn, crm, opc2`.
    pub(crate) fn read(&self, register: [u8; 4], privileged: bool) -> Outcome {
        match self.system {
            System::Sa110 if !privileged => Outcome::Undefined,
            System::Sa110 => self.sa110_read(register),
            System::Armv7 => self.armv7_read(register, privileged),
        }
    }

    /// An `MCR` of `value` to the register `opc1, crn, crm, opc2`.
    pub(crate) fn write(&mut self, register: [u8; 4], value: u32, privileged: bool) -> Outcome {
        match self.system {
            System::Sa110 if !privileged => Outcome::Undefined,
            System::Sa110 => self.sa110_write(register, value),
            System::Armv7 => self.armv7_write(register, value, privileged),
        }
    }

    fn sa110_read(&self, [opc1, crn, _, _]: [u8; 4]) -> Outcome {
        match (opc1, crn) {
            (0, 0) => Outcome::Done(SA110_ID),
            (0, 1) => Outcome::Done(self.control),
            (0, 5) => Outcome::Done(self.data_status),
            (0, 6) => Outcome::Done(self.data_address),
            (0, 2 | 3) => Outcome::Unmodelled,
            _ => Outcome::Undefined,
        }
    }

    fn sa110_write(&mut self, [opc1, crn, crm, opc2]: [u8; 4], value: u32) -> Outcome {
        match (opc1, crn, crm, opc2) {
            (0, 1, _, _) => self.write_control(value, SA110_CACHE_BITS),
            (0, 5, _, _) => {
                self.data_status = value & 0xFF;
                Outcome::Done(0)
            }
            (0, 6, _, _) => {
                self.data_address = value;
                Outcome::Done(0)
            }
            (0, 2 | 3, _, _) => Outcome::Unmodelled,
            // Cache, TLB and read-buffer maintenance, and clock switching on
            // and off.
            (0, 7..=9, _, _) | (0, 15, 1 | 2, 2) => Outcome::Done(0),
            (0, 15, 8, 2) => Outcome::Wait,
            _ => Outcome::Undefined,
        }
    }

    fn write_control(&mut self, value: u32, cache_bits: u32) -> Outcome {
        if (value ^ self.control) & !cache_bits != 0 {
            return Outcome::Unmodelled;
        }
        self.control = value;
        Outcome::Done(0)
    }

    fn armv7_read(&self, register: [u8; 4], privileged: bool) -> Outcome {
        let value = match register {
            [0, 13, 0, 2] => self.thread[0],
            [0, 13, 0, 3] => self.thread[1],
            _ if !privileged => return Outcome::Undefined,
            [0, 0, 0, 0 | 4 | 7] => ARMV7_ID,
            [0, 0, 0, 1] => ARMV7_CACHE_TYPE,
            [0, 0, 0, 2 | 3 | 6]
            | [1, 0, 0, 7]
            | [0, 1, 0, 1]
            | [0, 5, 1, 0 | 1]
            | [0, 13, 0, 0] => 0,
            [0, 1, 0, 2] => self.access,
            [0, 0, 2, 5] => self.isar5,
            [0, 0, 0, 5] => ARMV7_MULTIPROCESSOR,
            [0, 0, crm @ 1..=2, opc2 @ 0..=7] => {
                let index = usize::from((crm - 1) * 8 + opc2);
                ARMV7_FEATURES.get(index).copied().unwrap_or(0)
            }
            [1, 0, 0, 0] => {
                if self.cache_selection & 0xE == 0 {
                    ARMV7_CACHE_SIZE
                } else {
                    0
                }
            }
            [1, 0, 0, 1] => ARMV7_CACHE_LEVELS,
            [2, 0, 0, 0] => self.cache_selection,
            [0, 1, 0, 0] => self.control,
            [0, 2, 0, 0..=2] | [0, 3, 0, 0] | [0, 7, 4, 0] | [0, 10, 2, 0 | 1] => {
                return Outcome::Unmodelled
            }
            [0, 5, 0, 0] => self.data_status,
            [0, 5, 0, 1] => self.instruction_status,
            [0, 6, 0, 0] => self.data_address,
            [0, 6, 0, 2] => self.instruction_address,
            [0, 13, 0, 1] => self.context,
            [0, 13, 0, 4] => self.thread[2],
            _ => return Outcome::Undefined,
        };
        Outcome::Done(value)
    }

    fn armv7_write(&mut self, register: [u8; 4], value: u32, privileged: bool) -> Outcome {
        let barriers = self.control & ARMV7_CP15_BARRIERS != 0;
        match register {
            [0, 13, 0, 2] => self.thread[0] = value,
            [0, 7, 10, 4 | 5] | [0, 7, 5, 4] if privileged || barriers => {}
            _ if !privileged => return Outcome::Undefined,
            [0, 13, 0, 3] => self.thread[1] = value,
            [0, 13, 0, 4] => self.thread[2] = value,
            [0, 13, 0, 1] => self.context = value,
            // RAZ/WI registers; cache and branch predictor maintenance by
            // address, by set and way and in whole; TLB maintenance.
            [0, 1, 0, 2] => self.access = value & self.access_mask,
            [0, 13, 0, 0]
            | [0, 5, 1, 0 | 1]
            | [0, 7, 1, 0 | 6]
            | [0, 7, 5, 0 | 1 | 6 | 7]
            | [0, 7, 6 | 10 | 14, 1 | 2]
            | [0, 7, 11, 1]
            | [0, 8, 3 | 5..=7, 0..=3] => {}
            [2, 0, 0, 0] => self.cache_selection = value & 0xF,
            [0, 1, 0, 0] => return self.write_control(value, ARMV7_CACHE_BITS),
            [0, 5, 0, 0] => self.data_status = value & 0x1C0F,
            [0, 5, 0, 1] => self.instruction_status = value & 0x140F,
            [0, 6, 0, 0] => self.data_address = value,
            [0, 6, 0, 2] => self.instruction_address = value,
            [0, 1, 0, 1]
            | [0, 2, 0, 0..=2]
            | [0, 3, 0, 0]
            | [0, 7, 4, 0]
            | [0, 7, 8, 0..=3]
            | [0, 10, 2, 0 | 1] => return Outcome::Unmodelled,
            _ => return Outcome::Undefined,
        }
        Outcome::Done(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{BIT26, BIT32};

    #[test]
    fn user_mode_reaches_only_the_thread_registers_and_the_barriers() {
        let mut cp15 = Cp15::new(System::Armv7, BIT32.isa);
        assert_eq!(cp15.read([0, 0, 0, 0], false), Outcome::Undefined);
        assert_eq!(cp15.write([0, 13, 0, 2], 7, false), Outcome::Done(0));
        assert_eq!(cp15.read([0, 13, 0, 2], false), Outcome::Done(7));
        assert_eq!(cp15.write([0, 13, 0, 3], 9, false), Outcome::Undefined);
        assert_eq!(cp15.write([0, 13, 0, 3], 9, true), Outcome::Done(0));
        assert_eq!(cp15.read([0, 13, 0, 3], false), Outcome::Done(9));
        assert_eq!(cp15.write([0, 7, 10, 5], 0, false), Outcome::Done(0));
        let mut sa110 = Cp15::new(System::Sa110, BIT26.isa);
        assert_eq!(sa110.read([0, 0, 0, 0], false), Outcome::Undefined);
        assert_eq!(sa110.write([0, 7, 7, 0], 0, false), Outcome::Undefined);
    }

    #[test]
    fn the_identity_and_features_are_the_profile_s() {
        let cp15 = Cp15::new(System::Armv7, BIT32.isa);
        assert_eq!(cp15.read([0, 0, 0, 0], true), Outcome::Done(ARMV7_ID));
        assert_eq!(cp15.read([0, 0, 2, 5], true), Outcome::Done(0x0001_1121));
        assert_eq!(cp15.read([0, 0, 2, 0], true), Outcome::Done(0x0214_1110));
        let sa110 = Cp15::new(System::Sa110, BIT26.isa);
        assert_eq!(sa110.read([0, 0, 0, 0], true), Outcome::Done(SA110_ID));
    }

    #[test]
    fn the_cache_registers_describe_the_profile_s_caches() {
        let cp15 = Cp15::new(System::Armv7, BIT32.isa);
        assert_eq!(cp15.read([0, 0, 0, 1], true), Outcome::Done(0x8444_C004));
        assert_eq!(cp15.read([1, 0, 0, 0], true), Outcome::Done(0x700F_E01A));
        assert_eq!(
            cp15.cache(0),
            Some(SplitCache {
                size: 32 * 1024,
                line: 64
            })
        );
        assert_eq!(cp15.cache(1), None);
        let sa110 = Cp15::new(System::Sa110, BIT26.isa);
        assert_eq!(
            sa110.cache(0),
            Some(SplitCache {
                size: 16 * 1024,
                line: 32
            })
        );
    }

    #[test]
    fn only_the_cache_enables_of_the_control_register_change() {
        let mut cp15 = Cp15::new(System::Armv7, BIT32.isa);
        assert_eq!(
            cp15.write([0, 1, 0, 0], ARMV7_CONTROL & !(1 << 12), true),
            Outcome::Done(0)
        );
        assert_eq!(
            cp15.read([0, 1, 0, 0], true),
            Outcome::Done(ARMV7_CONTROL & !(1 << 12))
        );
        assert_eq!(
            cp15.write([0, 1, 0, 0], ARMV7_CONTROL & !1, true),
            Outcome::Unmodelled
        );
        assert_eq!(cp15.write([0, 2, 0, 0], 0x4000, true), Outcome::Unmodelled);
        let mut sa110 = Cp15::new(System::Sa110, BIT26.isa);
        assert_eq!(
            sa110.write([0, 1, 0, 0], SA110_CONTROL | 2, true),
            Outcome::Unmodelled
        );
        assert_eq!(sa110.write([0, 15, 8, 2], 0, true), Outcome::Wait);
    }

    #[test]
    fn faults_record_their_status_and_address() {
        let mut cp15 = Cp15::new(System::Armv7, BIT32.isa);
        cp15.data_fault(Fault::Bus(Abort::Permission), 0x1234, true);
        assert_eq!(cp15.read([0, 5, 0, 0], true), Outcome::Done(0x80F));
        assert_eq!(cp15.read([0, 6, 0, 0], true), Outcome::Done(0x1234));
        cp15.prefetch_fault(Fault::Debug, 0x8000);
        assert_eq!(cp15.read([0, 5, 0, 1], true), Outcome::Done(0b0010));
        assert_eq!(cp15.read([0, 6, 0, 2], true), Outcome::Done(0x8000));
        let mut sa110 = Cp15::new(System::Sa110, BIT26.isa);
        sa110.data_fault(Fault::Alignment, 0x8001, false);
        assert_eq!(sa110.read([0, 5, 0, 0], true), Outcome::Done(1));
    }
    #[test]
    fn cpacr_holds_only_the_vfp_access_fields_and_isar5_the_crypto_extension() {
        let mut cp15 = Cp15::new(System::Armv7, BIT32.isa);
        assert_eq!(cp15.write([0, 1, 0, 2], u32::MAX, true), Outcome::Done(0));
        assert_eq!(cp15.read([0, 1, 0, 2], true), Outcome::Done(VFP_ACCESS));
        assert_eq!(cp15.coprocessor_access(10), 3);
        assert_eq!(cp15.coprocessor_access(11), 3);
        assert_eq!(cp15.coprocessor_access(1), 0);
        assert_eq!(cp15.read([0, 1, 0, 2], false), Outcome::Undefined);
        assert_eq!(cp15.read([0, 0, 2, 5], true), Outcome::Done(0x0001_1121));
        let bare = Cp15::new(System::Armv7, crate::isa::Isa::of(&[]));
        assert_eq!(bare.read([0, 0, 2, 5], true), Outcome::Done(0x0001_0001));
        let mut no_vfp = bare;
        assert_eq!(no_vfp.write([0, 1, 0, 2], u32::MAX, true), Outcome::Done(0));
        assert_eq!(no_vfp.read([0, 1, 0, 2], true), Outcome::Done(0));
    }
}
