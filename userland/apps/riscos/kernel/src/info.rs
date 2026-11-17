//! What the kernel says about the machine (`plans/RISCOS-EMULATOR.md`
//! §4.5): `OS_ReadSysInfo`, `OS_PlatformFeatures` and the memory SWIs, for
//! a machine with no hardware platform and the processor of the calling
//! code's profile.
//!
//! `OS_PlatformFeatures` derives its answers from the processor's own ID
//! registers — for the 26-bit profile, from fixed values standing in for a
//! processor too old to have them — so it reports exactly what the profile
//! implements.

use tairix_riscos_arm::mmu::PAGE_SIZE;
use tairix_riscos_arm::psr;

use crate::cmos;
use crate::error::{Error, BAD_ADDRESS};
use crate::memory::check;
use crate::memory::layout::{APPLICATION, COMPATIBILITY, HIGH_ZERO_PAGE, SCRATCH, ZERO_PAGE_SIZE};
use crate::memory::Bits;
use crate::memory::{ReserveError, CANT_GET_PHYSICAL, PAGE_RESERVED};
use crate::os::{len32, Fail, Os, Regs};
use crate::zeropage;

/// "Unknown `OS_ReadSysInfo` call".
const BAD_READ_SYS_INFO: Error = Error {
    number: 0x1EC,
    token: "UnknownReadSysInfoReason",
};
/// "Unknown `OS_PlatformFeatures` reason code".
const BAD_PLATFORM_REASON: Error = Error {
    number: 0x1F0,
    token: "UnknownPlatformFeaturesReason",
};
/// "Invalid parameters".
pub(crate) const BAD_PARAMETERS: Error =
    crate::error::obey_error(tairix_obey::Error::BadParameters);
/// "No space left to map the I/O region".
const NO_ROOM_FOR_IO: Error = Error {
    number: 0x1D3,
    token: "NoIoSpace",
};
/// "No chunk is free".
const NO_CHUNK: Error = Error {
    number: 0x1D2,
    token: "NoFreeChunk",
};
/// `OS_Memory` 12's flag: R4 to R7 bound the physical addresses.
const RANGE_GIVEN: u32 = 1 << 1;
/// "No access privilege matches".
const NO_ACCESS_PRIVILEGE: Error = Error {
    number: 0x1D4,
    token: "NoSuchAccessPrivilege",
};
/// "Not a valid key handler".
const BAD_KEY_HANDLER: Error = Error {
    number: 0x1F4,
    token: "InvalidKeyHandler",
};
/// "Bad `OS_Hardware` reason code".
const HW_BAD_REASON: Error = Error {
    number: 0xC60,
    token: "UnknownHardwareReason",
};
/// "That hardware call is not provided".
const HW_BAD_ENTRY: Error = Error {
    number: 0xC61,
    token: "HardwareCallMissing",
};

/// `Service_Hardware`.
const SERVICE_HARDWARE: u32 = 0xD9;
/// Where a HAL device's version word is: its major version in the top half.
const DEVICE_VERSION: u32 = 8;

/// `OS_ReadSysInfo` 8's class: a machine described by a HAL, as every
/// RISC OS 5 machine is.
const PLATFORM_HAL: u32 = 5;
/// `OS_ReadSysInfo` 8's flags the emulator answers for: all seven.
const PLATFORM_FLAGS: u32 = 0x7F;
/// The OS relies on rotated loads: the 26-bit profile's processor makes
/// them. The 32-bit profile faults misaligned accesses, so RISC OS there
/// makes only aligned ones and sets neither this nor the unaligned flag.
const ROTATED_LOADS: u32 = 1 << 5;
/// `OS_ReadSysInfo` 2's first word: no special functions chip, and the I/O,
/// memory and video controllers each "unspecified", there being none.
const NO_CHIPS: u32 = 0xFFFF_FF00;

/// `ProcessorFlags`' bits.
mod flag {
    pub(super) const SYNCHRONISE: u32 = 1 << 0;
    pub(super) const IRQ_DELAY: u32 = 1 << 1;
    pub(super) const STORES_PC_PLUS_8: u32 = 1 << 3;
    pub(super) const BASE_RESTORED: u32 = 1 << 4;
    pub(super) const SPLIT_CACHE: u32 = 1 << 5;
    pub(super) const OS_32BIT: u32 = 1 << 6;
    pub(super) const NO_26BIT: u32 = 1 << 7;
    pub(super) const LONG_MULTIPLY: u32 = 1 << 8;
    pub(super) const THUMB: u32 = 1 << 9;
    pub(super) const DSP: u32 = 1 << 10;
    pub(super) const NO_SWP: u32 = 1 << 11;
    pub(super) const EXCLUSIVE: u32 = 1 << 12;
    pub(super) const EXCLUSIVE_ALL: u32 = 1 << 13;
    pub(super) const HIGH_VECTORS: u32 = 1 << 20;
    pub(super) const FIXED: u32 = 1 << 31;
}

/// The `ID_ISAR0` to `ID_ISAR4` standing in for an ARMv4 processor, which has
/// none: what the 26-bit profile implements; `ID_ISAR5` is zero.
const ARMV4_ISARS: [u32; 6] = [
    0x0001_0001,
    0x0000_0010,
    0x0111_1000,
    0x0000_0100,
    0x0000_0141,
    0,
];

/// `OS_PlatformFeatures` 33's cache type for separate instruction and data
/// caches.
const SPLIT_CACHES: u32 = 3;

/// The feature bits of `OS_PlatformFeatures` 34, defined to 64.
const FEATURES: u32 = 65;

/// The instruction set attribute registers and the rest a profile's
/// features are read from.
struct Ids {
    isar: [u32; 6],
    pfr1: u32,
    multiprocessor: bool,
    armv7: bool,
}

/// The mode, monitor type and sync an automatic setting comes to when no
/// driver describes the display: a VGA-class monitor showing 640 by 480 in
/// 256 colours, with separate syncs.
const AUTO_DISPLAY: [u32; 3] = [28, 3, 0];

impl Os<'_> {
    /// The current processor's identity.
    fn ids(&mut self) -> Ids {
        if self.world.processor.bits() == Bits::Bit26 {
            return Ids {
                isar: ARMV4_ISARS,
                pfr1: 0,
                multiprocessor: false,
                armv7: false,
            };
        }
        let cpu = self.world.processor.cpu();
        let read = |r: [u8; 4]| cpu.system_register(r).unwrap_or(0);
        let mut isar = [0; 6];
        for (i, slot) in (0u8..).zip(isar.iter_mut()) {
            *slot = read([0, 0, 2, i]);
        }
        Ids {
            isar,
            pfr1: read([0, 0, 1, 1]),
            multiprocessor: read([0, 0, 0, 5]) >> 31 != 0,
            armv7: true,
        }
    }

    /// `ProcessorFlags` for the current processor, from its identity; a
    /// 26-bit OS's for 26-bit code.
    fn processor_flags(&mut self) -> u32 {
        let ids = self.ids();
        let field = |register: u32, low: u32| (register >> low) & 0xF;
        if !ids.armv7 {
            return flag::SYNCHRONISE
                | flag::SPLIT_CACHE
                | flag::BASE_RESTORED
                | flag::STORES_PC_PLUS_8
                | flag::LONG_MULTIPLY
                | flag::IRQ_DELAY
                | flag::FIXED;
        }
        let cpu = self.world.processor.cpu();
        let pfr0 = cpu.system_register([0, 0, 1, 0]).unwrap_or(0);
        let clidr = cpu.system_register([1, 0, 0, 1]).unwrap_or(0);
        let mut flags = flag::STORES_PC_PLUS_8
            | flag::BASE_RESTORED
            | flag::OS_32BIT
            | flag::NO_26BIT
            | flag::HIGH_VECTORS
            | flag::FIXED;
        if clidr & 7 == 3 {
            flags |= flag::SYNCHRONISE | flag::SPLIT_CACHE;
        }
        if (ids.isar[2] >> 16) & 0xFF != 0 {
            flags |= flag::LONG_MULTIPLY;
        }
        if field(pfr0, 4) != 0 {
            flags |= flag::THUMB;
        }
        if field(ids.isar[3], 0) != 0 {
            flags |= flag::DSP;
        }
        if field(ids.isar[0], 0) == 0 && field(ids.isar[4], 28) == 0 {
            flags |= flag::NO_SWP;
        }
        match (field(ids.isar[3], 12), field(ids.isar[4], 20)) {
            (1, 0) => flags |= flag::EXCLUSIVE,
            (1, 3) | (2, _) => flags |= flag::EXCLUSIVE | flag::EXCLUSIVE_ALL,
            _ => {}
        }
        flags
    }

    /// `OS_PlatformFeatures` 34's feature bits for the current processor.
    #[expect(
        clippy::too_many_lines,
        reason = "one rule a line, in the order RISC OS applies them"
    )]
    fn features(&mut self) -> [u32; 3] {
        let ids = self.ids();
        let flags = self.processor_flags();
        let mut bits = [0u32; 3];
        let mut set = |n: u32, on: bool| {
            let (word, bit) = ((n / 32) as usize, n % 32);
            if on {
                bits[word] |= 1 << bit;
            } else {
                bits[word] &= !(1 << bit);
            }
        };
        let f = |register: u32, low: u32| (register >> low) & 0xF;
        let [isar0, isar1, isar2, isar3, isar4, isar5] = ids.isar;
        set(54, f(isar0, 24) >= 2);
        set(2, f(isar0, 20) >= 1);
        set(1, f(isar0, 8) >= 1);
        set(6, f(isar0, 4) >= 1);
        set(47, f(isar0, 0) >= 1);
        set(4, f(isar1, 24) >= 1);
        set(3, f(isar1, 24) >= 2);
        set(11, f(isar1, 24) >= 3);
        set(24, f(isar1, 20) >= 1);
        set(51, f(isar1, 12) >= 1);
        if f(isar1, 12) >= 2 {
            set(49, true);
            set(50, true);
        }
        set(45, f(isar1, 8) >= 1);
        set(37, f(isar2, 28) >= 1);
        set(36, f(isar2, 28) >= 2);
        set(25, f(isar2, 24) >= 1);
        set(56, f(isar2, 20) >= 1);
        set(55, f(isar2, 20) >= 2);
        set(44, f(isar2, 16) >= 1);
        if f(isar2, 16) >= 2 {
            set(42, true);
            set(34, true);
        }
        set(43, f(isar2, 16) >= 3);
        set(23, f(isar2, 12) >= 2);
        set(14, true);
        if f(isar2, 8) >= 1 {
            set(14, false);
            set(15, true);
        }
        if f(isar2, 8) >= 2 {
            set(15, false);
            set(13, true);
        }
        set(30, f(isar2, 4) >= 1);
        set(32, f(isar2, 4) >= 3);
        set(31, f(isar2, 4) >= 4);
        set(16, f(isar2, 0) >= 1);
        set(12, f(isar2, 0) >= 2);
        set(28, f(isar3, 24) >= 1);
        if f(isar3, 12) >= 1 {
            set(18, true);
            set(5, true);
        }
        if f(isar4, 20) < 3 {
            set(5, false);
        }
        if f(isar3, 12) >= 2 {
            set(5, true);
            set(17, true);
        }
        if f(isar3, 4) >= 1 {
            set(46, true);
            set(34, true);
        }
        if f(isar3, 4) >= 3 {
            set(29, true);
            set(33, true);
        } else {
            set(50, false);
        }
        if f(isar3, 0) >= 1 {
            set(35, true);
            set(34, true);
        }
        set(48, f(isar4, 28) >= 1);
        if f(isar0, 0) >= 1 {
            set(48, false);
        }
        set(8, f(isar4, 16) >= 1);
        set(41, f(isar4, 12) >= 1);
        set(19, f(isar4, 0) >= 2);
        set(7, f(isar5, 16) >= 1);
        set(40, f(isar5, 12) >= 1);
        set(39, f(isar5, 8) >= 1);
        set(0, f(isar5, 4) >= 1);
        set(38, f(isar5, 0) >= 1);
        set(53, flags & flag::NO_26BIT == 0);
        for n in [21, 52, 63, 20] {
            set(n, true);
        }
        if ids.armv7 {
            for n in [59, 61, 62, 64, 60] {
                set(n, true);
            }
        } else {
            for n in [27, 26, 22, 58] {
                set(n, true);
            }
        }
        set(57, ids.multiprocessor);
        if f(ids.pfr1, 12) != 0 {
            set(10, true);
            set(9, true);
        } else if f(ids.pfr1, 24) != 0 {
            set(9, true);
        }
        bits
    }

    /// `OS_PlatformFeatures`.
    pub(crate) fn platform_features(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        match regs.r[0] {
            0 => {
                let flags = self.processor_flags();
                regs.r[0] = flags;
                regs.r[1] = if flags & flag::IRQ_DELAY != 0 {
                    self.world.routine_address(crate::host::Routine::Return)
                } else {
                    0
                };
            }
            32 => {
                regs.r[0] = self.vector_base();
                regs.r[1] = 256;
            }
            33 => {
                let answer = match self.world.processor.cpu().cache(regs.r[1]) {
                    Some(cache) => [SPLIT_CACHES, cache.line, cache.size, cache.line, cache.size],
                    None => [0; 5],
                };
                regs.r[..5].copy_from_slice(&answer);
            }
            34 => {
                let features = self.features();
                let wanted = regs.r[1];
                match wanted.cast_signed() {
                    0.. if wanted < FEATURES => {
                        regs.r[0] = (features[(wanted / 32) as usize] >> (wanted % 32)) & 1;
                    }
                    0.. => regs.r[0] = u32::MAX,
                    -1 => {
                        regs.r[..3].copy_from_slice(&features);
                        regs.r[3] = 0;
                        regs.r[4] = u32::MAX;
                        regs.r[5] = u32::MAX;
                        regs.r[6] = 1;
                        regs.r[7] = 0;
                    }
                    _ => {
                        for r in [0, 1, 3, 4, 5, 7] {
                            regs.r[r] = 0;
                        }
                    }
                }
            }
            35 => {
                regs.r[1] = if self.caller_bits() == Bits::Bit32 {
                    self.world
                        .routine_address(crate::host::Routine::ClearExclusive)
                } else {
                    0
                };
            }
            _ => return Err(self.error(BAD_PLATFORM_REASON)),
        }
        Ok(())
    }

    /// `OS_ReadSysInfo`.
    pub(crate) fn read_sys_info(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        match regs.r[0] {
            0 => {
                let pages = u32::from(self.read_cmos(cmos::SCREEN_SIZE)? & 0x7F);
                let mut bytes = pages * PAGE_SIZE;
                if bytes == 0 {
                    bytes = 160 * 1024;
                }
                while bytes < 20 * 1024 {
                    bytes += PAGE_SIZE;
                }
                regs.r[0] = bytes.min(480 * 1024);
            }
            1 => self.configured_display(regs)?,
            2 => regs.r[..5].copy_from_slice(&[NO_CHIPS, 0, 0, 0, 0]),
            3..=5 => {
                regs.r[0] = 0;
                regs.r[1] = 0;
            }
            6 => self.kernel_values(regs)?,
            7 => {
                let abort = self.world.last_abort;
                regs.r[1] = abort.pc;
                regs.r[2] = abort.psr;
                regs.r[3] = abort.address;
            }
            8 => {
                regs.r[0] = PLATFORM_HAL;
                regs.r[1] = match self.caller_bits() {
                    Bits::Bit26 => ROTATED_LOADS,
                    Bits::Bit32 => 0,
                };
                regs.r[2] = PLATFORM_FLAGS;
            }
            9 => {
                regs.r[0] = match regs.r[1] {
                    0 | 6 => self.world.strings.os_name,
                    2 => self.world.strings.build_date,
                    7 => self.world.strings.platform,
                    _ => 0,
                };
            }
            // The debug channel's routines wrap HAL calls the machine lacks.
            11 => return Err(self.error(HW_BAD_ENTRY)),
            12 | 14 => regs.r[0] = 0,
            13 => {
                let handler = regs.r[1];
                // The flags word is there only where the size says so.
                if handler != 0 && self.read_word(handler.wrapping_add(4))? >> 31 != 0 {
                    let flags = self.read_word(handler.wrapping_add(32))?;
                    if flags & !1 != 0 {
                        return Err(self.error(BAD_KEY_HANDLER));
                    }
                }
                regs.r[0] = 1;
            }
            15 => {
                regs.r[1] = 0;
                regs.r[0] = 15;
            }
            _ => return Err(self.error(BAD_READ_SYS_INFO)),
        }
        Ok(())
    }

    /// `OS_ReadSysInfo` 1: the configured mode, monitor type and sync, an
    /// automatic one as the display's driver describes the display, else as
    /// [`AUTO_DISPLAY`].
    fn configured_display(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        regs.r[0] = self.read_spread(&cmos::MODE)?;
        regs.r[1] = self.read_spread(&cmos::MONITOR_TYPE)?;
        regs.r[2] = self.read_spread(&cmos::SYNC)?;
        if regs.r[..3].contains(&u32::MAX) {
            let mut lead = Regs::new([0, 0x76, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            self.service(&mut lead)?;
            let described = if lead.r[1] == 0 {
                [lead.r[3], lead.r[4], lead.r[5]]
            } else {
                AUTO_DISPLAY
            };
            for (slot, value) in regs.r[..3].iter_mut().zip(described) {
                if *slot == u32::MAX {
                    *slot = value;
                }
            }
        }
        Ok(())
    }

    /// One `OS_ReadSysInfo` 6 value: an address the kernel has, a count, or
    /// zero for what this kernel does not hold.
    fn kernel_value(&mut self, item: u32) -> u32 {
        let zero = self.vector_base();
        let layout = self.memory.layout();
        match item {
            2 => 0x8000,
            15 => layout.stacks.undefined.end,
            16 => layout.stacks.supervisor.end,
            17 => layout.system_heap.start,
            21 | 25 | 67 => layout.stacks.irq.end,
            23 | 69 => zero + zeropage::IRQSEMA,
            24 | 86 => crate::vectors::COUNT,
            70 => zero + zeropage::DOMAIN_ID,
            71 => zero + crate::byte::VARIABLES - 0xA6,
            74 => zero + zeropage::DEBUGGER_SPACE,
            75 => zeropage::DEBUGGER_SPACE_SIZE,
            76 => zero + zeropage::CANNOT_RESET,
            77 => zero + zeropage::METROGNOME,
            78 => zero + zeropage::CLIB_COUNTER,
            79 => zero + zeropage::RISCOS_LIB_WORD,
            80 => zero + zeropage::CLIB_WORD,
            81 => zero + zeropage::FPE_ANCHOR,
            82 => zero + zeropage::ESC_STATUS,
            87 | 89 => 1,
            88 => layout.stacks.abort.end,
            _ => 0,
        }
    }

    /// `OS_ReadSysInfo` 6: one value, or a list of them.
    fn kernel_values(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        if regs.r[1] == 0 {
            regs.r[2] = self.kernel_value(regs.r[2]);
            return Ok(());
        }
        let (mut list, mut out) = (regs.r[1], regs.r[2]);
        loop {
            let item = self.read_word(list)?;
            if item == u32::MAX {
                break;
            }
            let value = self.kernel_value(item);
            self.write_word(out, value)?;
            list = list.wrapping_add(4);
            out = out.wrapping_add(4);
            self.yield_point()?;
        }
        Ok(())
    }

    /// `OS_ReadMemMapInfo`.
    #[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
    pub(crate) fn read_mem_map_info(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        regs.r[0] = PAGE_SIZE;
        regs.r[1] = self.memory.ceiling_pages();
        Ok(())
    }

    /// `OS_ReadMemMapEntries`: each listed page's address and flags, to the
    /// first number past the last page.
    pub(crate) fn read_mem_map_entries(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let pages = self.memory.ceiling_pages();
        let mut at = regs.r[0];
        loop {
            let page = self.read_word(at)?;
            if page >= pages {
                break;
            }
            let (logical, flags) = self.memory.page_map(page).unwrap_or((u32::MAX, 0));
            self.write_word(at.wrapping_add(4), logical)?;
            self.write_word(at.wrapping_add(8), flags)?;
            at = at.wrapping_add(12);
            self.yield_point()?;
        }
        Ok(())
    }

    /// `OS_FindMemMapEntries`: each address's page and flags, -1 and 3 for
    /// one no page holds.
    pub(crate) fn find_mem_map_entries(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let mut at = regs.r[0];
        loop {
            if self.read_word(at)? == u32::MAX {
                break;
            }
            let logical = self.read_word(at.wrapping_add(4))?;
            let (page, flags) = match self.memory.page_at(logical) {
                Some(page) => (
                    page,
                    self.memory.page_map(page).map_or(0, |(_, flags)| flags),
                ),
                None => (u32::MAX, 3),
            };
            self.write_word(at, page)?;
            self.write_word(at.wrapping_add(8), flags)?;
            at = at.wrapping_add(12);
            self.yield_point()?;
        }
        Ok(())
    }

    /// `OS_SetMemMapEntries`: pages moved, as RISC OS 5 moves them, without
    /// checks — here only within the areas that own them.
    pub(crate) fn set_mem_map_entries(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let pages = self.memory.ceiling_pages();
        let mut at = regs.r[0];
        loop {
            let page = self.read_word(at)?;
            if page >= pages {
                break;
            }
            let logical = self.read_word(at.wrapping_add(4))?;
            let flags = self.read_word(at.wrapping_add(8))?;
            self.memory.set_page_map(page, logical, flags);
            at = at.wrapping_add(12);
            self.yield_point()?;
        }
        Ok(())
    }

    /// `OS_ValidateAddress`: C clear if R0 to R1 lies in memory a program may
    /// use, or a module claims it does.
    pub(crate) fn validate_address(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (start, end) = (regs.r[0], u64::from(regs.r[1]));
        let flags = self.memory.check_access(start, end);
        let complete =
            check::USER_READ | check::USER_WRITE | check::PRIVILEGED_READ | check::PRIVILEGED_WRITE;
        let mut valid = flags & complete != 0;
        if !valid {
            let mut service = Regs::new([0, 0x6D, start, regs.r[1], 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            self.service(&mut service)?;
            valid = service.r[1] == 0;
        }
        regs.set_flag(psr::C, !valid);
        Ok(())
    }

    /// `OS_ReadRAMFsLimits`: the RAM disc's area, dynamic area 5.
    pub(crate) fn read_ramfs_limits(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let mut area = Regs::new([5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.read_dynamic_area(&mut area)?;
        regs.r[0] = area.r[0];
        regs.r[1] = area.r[0] + area.r[1];
        Ok(())
    }

    /// `OS_SynchroniseCodeAreas`: no instruction is held anywhere a store
    /// could leave stale, so there is nothing to make agree.
    #[expect(
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "a SWI handler's signature"
    )]
    pub(crate) fn synchronise_code_areas(&mut self, _regs: &mut Regs) -> Result<(), Fail> {
        Ok(())
    }

    /// `OS_MMUControl`: the control register's soft copy, cache and TLB
    /// maintenance — which, with no cache to keep, does nothing — and the
    /// addresses of the `ARMop` routines.
    pub(crate) fn mmu_control(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        match regs.r[0] & 0xFF {
            0 => {
                let old = self.world.mmu_control;
                self.world.mmu_control = (old & regs.r[2]) ^ regs.r[1];
                regs.r[1] = old;
                regs.r[2] = self.world.mmu_control;
            }
            1 => {}
            2 => {
                let index = (regs.r[0] >> 8) & 0xFF;
                if index >= crate::host::ARM_OPS {
                    return Err(self.error(crate::kheap::UNKNOWN_REASON));
                }
                regs.r[0] = self
                    .world
                    .routine_address(crate::host::Routine::ArmOp(index));
            }
            _ => return Err(self.error(crate::kheap::UNKNOWN_REASON)),
        }
        Ok(())
    }

    /// `OS_Hardware`: no HAL entry exists; the kernel's device table, with
    /// none of its own, takes the devices modules add.
    pub(crate) fn hardware(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        match regs.r[8] & 0xFF {
            0 | 1 => Err(self.error(HW_BAD_ENTRY)),
            2 => {
                let device = regs.r[0];
                if self.world.devices.contains(&device) {
                    self.remove_device(device)?;
                }
                if self.world.devices.try_reserve(1).is_err() {
                    return Err(self.no_memory());
                }
                self.world.devices.push(device);
                let mut service =
                    Regs::new([0, SERVICE_HARDWARE, device, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                self.service(&mut service)?;
                Ok(())
            }
            3 => {
                let device = regs.r[0];
                if self.world.devices.contains(&device) {
                    self.remove_device(device)?;
                }
                Ok(())
            }
            reason @ (4 | 5) => self.enumerate_devices(regs, reason == 4),
            _ => Err(self.error(HW_BAD_REASON)),
        }
    }

    /// Removes a registered device, unless a `Service_Hardware` 1 claimant
    /// objects with its error.
    fn remove_device(&mut self, device: u32) -> Result<(), Fail> {
        let mut service = Regs::new([1, SERVICE_HARDWARE, device, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.service(&mut service)?;
        if service.r[1] == 0 {
            return Err(Fail::Error(service.r[0]));
        }
        self.world.devices.retain(|&d| d != device);
        Ok(())
    }

    /// `OS_Hardware` 4 and 5: the next device past R1 of those seen, in
    /// `newest_first` order, of R0's type and of a major version up to R0's;
    /// R1 counts the devices passed, -1 once none is left.
    fn enumerate_devices(&mut self, regs: &mut Regs, newest_first: bool) -> Result<(), Fail> {
        let (kind, highest) = (regs.r[0] & 0xFFFF, regs.r[0] >> 16);
        let count = self.world.devices.len();
        for place in regs.r[1] as usize..count {
            let index = if newest_first {
                count - 1 - place
            } else {
                place
            };
            let device = self.world.devices[index];
            let kind_of = self.read_word(device)? & 0xFFFF;
            let major = self.read_word(device.wrapping_add(DEVICE_VERSION))? >> 16;
            if kind_of == kind && major <= highest {
                regs.r[1] = len32(place + 1);
                regs.r[2] = device;
                return Ok(());
            }
            self.yield_point()?;
        }
        regs.r[1] = u32::MAX;
        Ok(())
    }

    /// `OS_IICOp`: there is no IIC bus.
    pub(crate) fn iic_op(&mut self, _regs: &mut Regs) -> Result<(), Fail> {
        Err(self.error(BAD_PARAMETERS))
    }

    /// `OS_TaskControl` 0: the address of the routine that empties the
    /// privileged stacks.
    pub(crate) fn task_control(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        if regs.r[0] != 0 {
            return Err(self.error(BAD_PARAMETERS));
        }
        regs.r[0] = self
            .world
            .routine_address(crate::host::Routine::ResetStacks);
        Ok(())
    }

    /// `OS_Reset`: `Service_PreReset`, then a hard reset, which ends the
    /// session's program.
    pub(crate) fn reset(&mut self, _regs: &mut Regs) -> Result<(), Fail> {
        let mut service = Regs::new([0, 0x45, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.service(&mut service)?;
        Err(Fail::Unwind(crate::os::Unwind::Ended(
            crate::os::Ended::Reset {
                hard: true,
                reverse_boot: false,
            },
        )))
    }

    /// `OS_UpdateMEMC`: the control register's soft copy; its video bit
    /// blanks or unblanks the display through `GraphicsV`.
    pub(crate) fn update_memc(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let old = self.world.memc;
        let new = ((old & !regs.r[1]) | (regs.r[0] & regs.r[1])) & 0x000F_FFFF | 0x0360_0000;
        self.world.memc = new;
        regs.r[0] = old;
        if (old ^ new) & (1 << 10) != 0 {
            let blank = u32::from(new & (1 << 10) == 0);
            let mut graphics =
                Regs::new([blank, 0, 0, 0, GRAPHICS_SET_BLANK, 0, 0, 0, 0, 0, 0, 0, 0]);
            self.call_vector(
                crate::vectors::GRAPHICS,
                &mut graphics,
                crate::os::Entry::SupervisorNoIrq,
            )?;
        }
        Ok(())
    }

    /// `OS_Memory`.
    pub(crate) fn os_memory(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let reason = regs.r[0] & 0xFF;
        let flags = regs.r[0] >> 8;
        // A reason unknown is refused as such whatever its flags, and which
        // known reasons read or ignore flags and which refuse them is what
        // genuine RISC OS 5.30 is observed to do.
        let known = matches!(reason, 0 | 6..=9 | 12..=24 | 64 | 65);
        let takes_flags = matches!(reason, 0 | 6..=9 | 12..=16 | 19 | 21..=23 | 64);
        if known && flags != 0 && !takes_flags {
            return Err(self.error(BAD_PARAMETERS));
        }
        match reason {
            0 | 64 => self.memory_convert(regs, reason == 64),
            6 => {
                regs.r[1] = self.memory.ceiling_pages().div_ceil(2);
                regs.r[2] = PAGE_SIZE;
                Ok(())
            }
            7 => self.memory_arrangement(regs),
            8 => {
                let pages = match flags & 0xF {
                    1 => self.memory.ceiling_pages(),
                    2..=5 => 0,
                    _ => return Err(self.error(BAD_PARAMETERS)),
                };
                if flags >> 4 != 0 {
                    return Err(self.error(BAD_PARAMETERS));
                }
                regs.r[1] = pages;
                regs.r[2] = PAGE_SIZE;
                Ok(())
            }
            9 => {
                regs.r[1] = 0;
                Ok(())
            }
            12 => self.memory_recommend(regs, flags),
            13 | 21 => Err(self.error(NO_ROOM_FOR_IO)),
            14 | 22 => Err(self.error(CANT_GET_PHYSICAL)),
            15 => Ok(()),
            16 => self.memory_area_info(regs, flags & 0xFF),
            17 => {
                let (number, rights) = crate::memory::privilege_rights(regs.r[1]);
                regs.r[1] = number;
                regs.r[2] = rights;
                Ok(())
            }
            18 => {
                let (wanted, mask) = (regs.r[1], regs.r[2]);
                if wanted & !mask != 0 || mask & !0x3F != 0 {
                    return Err(self.error(BAD_PARAMETERS));
                }
                match crate::memory::find_access_privilege(wanted, mask) {
                    Some((number, rights)) => {
                        regs.r[0] = number;
                        regs.r[2] = rights;
                        Ok(())
                    }
                    None => Err(self.error(NO_ACCESS_PRIVILEGE)),
                }
            }
            19 => self.memory_dma(regs),
            20 => self.memory_compatibility(regs),
            23 => match self
                .memory
                .reserve_pages(regs.r[1], regs.r[2], flags & 1 == 0)
            {
                Ok(()) => Ok(()),
                Err(ReserveError::Range) => Err(self.error(BAD_PARAMETERS)),
                Err(ReserveError::Reserved) => Err(self.error(PAGE_RESERVED)),
                Err(ReserveError::NoMemory) => Err(self.no_memory()),
            },
            24 => {
                regs.r[1] = self.memory.check_access(regs.r[1], u64::from(regs.r[2]));
                Ok(())
            }
            65 => self.memory_physical(regs),
            _ => Err(self.error(crate::kheap::UNKNOWN_REASON)),
        }
    }

    /// `OS_Memory` 12: the first of R1's bytes of free contiguous pages at
    /// R2's alignment, within R4 to R7's physical range where the flags
    /// give one.
    fn memory_recommend(&mut self, regs: &mut Regs, flags: u32) -> Result<(), Fail> {
        let pages = regs.r[1].div_ceil(PAGE_SIZE);
        let wide = |low: u32, high: u32| u64::from(low) | u64::from(high) << 32;
        let (low, high) = if flags & RANGE_GIVEN != 0 {
            (wide(regs.r[4], regs.r[5]), wide(regs.r[6], regs.r[7]))
        } else {
            (0, u64::from(u32::MAX))
        };
        if let Some(first) = self.memory.contiguous_free(pages, regs.r[2], low, high) {
            regs.r[3] = first;
            Ok(())
        } else {
            regs.r[3] = 0;
            Err(self.error(NO_CHUNK))
        }
    }

    /// `OS_Memory` 20: the compatibility page removed, restored or read for
    /// every 32-bit program; a 26-bit one, whose zero page is at zero, has
    /// none.
    fn memory_compatibility(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let action = regs.r[1];
        if !matches!(action, 0 | 1 | u32::MAX) {
            return Err(self.error(BAD_PARAMETERS));
        }
        regs.r[1] = match self.caller_bits() {
            Bits::Bit26 => u32::MAX,
            Bits::Bit32 => {
                if action != u32::MAX && self.memory.set_compatibility_page(action == 1).is_err() {
                    return Err(self.no_memory());
                }
                u32::from(self.memory.compatibility_page())
            }
        };
        Ok(())
    }

    /// `OS_Memory` 65: the physical address of the logical address R1, its
    /// low and high halves.
    fn memory_physical(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let address = regs.r[1];
        regs.r[2] = PAGE_SIZE;
        let page = self
            .memory
            .page_at(address)
            .ok_or_else(|| self.error(BAD_ADDRESS))?;
        let physical = u64::from(page) * u64::from(PAGE_SIZE) + u64::from(address % PAGE_SIZE);
        regs.r[0] = (physical & 0xFFFF_FFFF) as u32;
        regs.r[1] = (physical >> 32) as u32;
        Ok(())
    }

    /// `OS_Memory` 0 and 64: page numbers, logical and physical addresses
    /// filled in from whichever each entry gives; caching asked for is
    /// granted, there being no cache.
    fn memory_convert(&mut self, regs: &mut Regs, wide: bool) -> Result<(), Fail> {
        let flags = regs.r[0];
        let given = (flags >> 8) & 7;
        if given == 0 || regs.r[2] == 0 || (wide && flags >> 16 != 0) {
            return Err(self.error(BAD_PARAMETERS));
        }
        let wanted = (flags >> 11) & 7;
        let entry = if wide { 20 } else { 12 };
        let mut at = regs.r[1];
        for _ in 0..regs.r[2] {
            let page_field = self.read_word(at)?;
            let logical_field = self.read_word(at.wrapping_add(4))?;
            let physical_low = self.read_word(at.wrapping_add(if wide { 12 } else { 8 }))?;
            let physical_high = if wide {
                self.read_word(at.wrapping_add(16))?
            } else {
                0
            };
            let physical = u64::from(physical_low) | u64::from(physical_high) << 32;
            // A logical address past 32 bits names no page of a 32-bit OS.
            let logical_high = if wide && given & 2 != 0 {
                self.read_word(at.wrapping_add(8))?
            } else {
                0
            };
            let page = if given & 1 != 0 {
                Some(page_field)
            } else if given & 2 != 0 {
                (logical_high == 0)
                    .then(|| self.memory.page_at(logical_field))
                    .flatten()
            } else {
                u32::try_from(physical / u64::from(PAGE_SIZE)).ok()
            };
            let Some(page) = page.filter(|&p| p < self.memory.ceiling_pages()) else {
                return Err(self.error(BAD_ADDRESS));
            };
            let offset = if given & 4 != 0 {
                (physical & 0xFFFF_FFFF) as u32 % PAGE_SIZE
            } else if given & 2 != 0 {
                logical_field % PAGE_SIZE
            } else {
                0
            };
            if wanted & 1 != 0 && given & 1 == 0 {
                self.write_word(at, page)?;
            }
            if wanted & 2 != 0 && given & 2 == 0 {
                let (logical, _) = self
                    .memory
                    .page_map(page)
                    .filter(|&(logical, _)| logical != u32::MAX)
                    .ok_or_else(|| self.error(BAD_ADDRESS))?;
                self.write_word(at.wrapping_add(4), logical.wrapping_add(offset))?;
                if wide {
                    self.write_word(at.wrapping_add(8), 0)?;
                }
            }
            if wanted & 4 != 0 && given & 4 == 0 {
                let physical = u64::from(page) * u64::from(PAGE_SIZE) + u64::from(offset);
                if !wide && physical >> 32 != 0 {
                    return Err(self.error(BAD_ADDRESS));
                }
                self.write_word(
                    at.wrapping_add(if wide { 12 } else { 8 }),
                    (physical & 0xFFFF_FFFF) as u32,
                )?;
                if wide {
                    self.write_word(at.wrapping_add(16), (physical >> 32) as u32)?;
                }
            }
            at = at.wrapping_add(entry);
            self.yield_point()?;
        }
        Ok(())
    }

    /// `OS_Memory` 7: a nibble per physical page — DRAM, or DRAM not
    /// available where a page is claimed exclusively.
    fn memory_arrangement(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let pages = self.memory.ceiling_pages();
        for pair in 0..pages.div_ceil(2) {
            let mut byte = 0;
            for half in 0..2 {
                let page = pair * 2 + half;
                if page < pages {
                    let nibble = if self.memory.page_unavailable(page) {
                        9
                    } else {
                        1
                    };
                    byte |= nibble << (4 * half);
                }
            }
            self.write_byte(regs.r[1].wrapping_add(pair), byte)?;
            self.yield_point()?;
        }
        Ok(())
    }

    /// `OS_Memory` 16: where an area of the kernel's lies, as much as it
    /// has, and how much it uses.
    fn memory_area_info(&mut self, regs: &mut Regs, area: u32) -> Result<(), Fail> {
        let layout = self.memory.layout();
        let stacks = &layout.stacks;
        let span = |range: &core::ops::Range<u32>| (range.start, range.end - range.start);
        let (base, size) = match area {
            2 => span(&stacks.irq),
            3 => span(&stacks.supervisor),
            4 => span(&stacks.abort),
            5 => span(&stacks.undefined),
            12 => (self.vector_base(), ZERO_PAGE_SIZE),
            15 => span(&SCRATCH),
            16 if self.caller_bits() == Bits::Bit32 => {
                let used = if self.memory.compatibility_page() {
                    PAGE_SIZE
                } else {
                    0
                };
                regs.r[1..4].copy_from_slice(&[COMPATIBILITY.start, PAGE_SIZE, used]);
                return Ok(());
            }
            1 | 6..=11 | 13 | 14 | 16 | 17 => (0, 0),
            _ => return Err(self.error(BAD_PARAMETERS)),
        };
        regs.r[1] = base;
        regs.r[2] = size;
        regs.r[3] = size;
        Ok(())
    }

    /// Where the caller's processor vectors and zero page are: at zero for
    /// 26-bit code, at &FFFF0000 for 32-bit, as RISC OS 5 relocates them on
    /// the processors that profile models.
    pub(crate) fn vector_base(&mut self) -> u32 {
        match self.caller_bits() {
            Bits::Bit26 => 0,
            Bits::Bit32 => HIGH_ZERO_PAGE,
        }
    }

    /// The top of the current task's application space.
    pub(crate) fn application_top(&self) -> u32 {
        self.memory
            .current_slot()
            .and_then(|slot| self.memory.slot_size(slot))
            .map_or(APPLICATION, |size| APPLICATION + size)
    }
}

/// `GraphicsV`'s `SetBlank` reason.
const GRAPHICS_SET_BLANK: u32 = 4;

mod dma;

#[cfg(test)]
mod tests;
