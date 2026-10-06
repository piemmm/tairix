//! Per-task extended register state on x86_64: the x87/MMX file, the YMM and
//! ZMM upper halves and the AVX-512 opmask registers.
//!
//! Kernel code never writes any of it ([`crate::fpu`]), so it moves only at
//! three points of a user task's life:
//!
//! * **Park** (`park`, from `enter_cooperative_park`): saved with XSAVEOPT,
//!   XSAVE or FXSAVE64 — unless a load is still pending, in which case the
//!   area is already the latest copy and the registers hold another task's.
//! * **Resume** (`resume`, from `leave_cooperative_park`): a load is marked
//!   pending unless this CPU's registers still hold the task's state — this
//!   CPU's owner is the task's area *and* the area last lived on this CPU, the
//!   rule Linux's `fpregs_state_valid` states.
//! * **Return to ring 3**: `tairix_arch_x86_64_xstate_load`, called from each
//!   stub's naked exit path after all Rust has run, so no Rust code executes
//!   under a user's x87 or `MXCSR`.
//!
//! A restore on an AMD part before Zen 2 keeps the x87 last-instruction,
//! last-data and last-opcode pointers already in the registers unless the
//! image has an exception pending (CVE-2006-1056), which would let a task read
//! where the last one worked. On such a CPU every restore first points them
//! at a kernel constant ([`crate::xstate::keeps_x87_pointers`]).
//!
//! A task's first entry loads every component's initial state instead
//! (`crate::userentry`).
//!
//! # The area
//!
//! Each user task's area sits at the top of its kernel stack, directly above
//! `RSP0` (`rsp0_below`), so a ring-3 entry finds it at its frame's top with
//! no lookup: the `AreaHeader`, then the 64-byte-aligned image.
//!
//! XSAVEOPT may skip a component unchanged since the last XRSTOR from the
//! same address. That is sound only because the save is the image's sole
//! writer of component state: a first entry touches just the two headers —
//! the `AreaHeader` and, under XSAVE, the image's own, before any XRSTOR has
//! read it — and restores from `INIT_IMAGE`, which retargets the CPU's
//! tracking, so a reused stack address can never skip-save stale bytes.

use core::sync::atomic::{AtomicU64, Ordering};

/// `XCR0.X87`.
pub const X87: u64 = 1 << 0;
/// `XCR0.SSE`: `xmm0`–`xmm15` and `MXCSR`, which every entry frames instead.
pub const SSE: u64 = 1 << 1;
/// `XCR0.AVX`: the YMM upper halves.
pub const AVX: u64 = 1 << 2;
/// `XCR0` bits 5–7: the opmask registers, the ZMM upper halves of
/// `zmm0`–`zmm15`, and `zmm16`–`zmm31`. The architecture accepts them only
/// together.
pub const AVX512: u64 = 0b111 << 5;

/// The legacy FXSAVE image, which also begins every XSAVE image.
const FXSAVE_BYTES: u32 = 512;
/// The XSAVE header that follows the legacy image.
pub(crate) const XSAVE_HEADER_BYTES: usize = 64;
/// XSAVE requires its image 64-byte aligned.
const IMAGE_ALIGN: u64 = 64;

/// How this machine saves the extended state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavour {
    /// No XSAVE: FXSAVE64 saves x87 and SSE, which is all there is.
    Fxsave = 1,
    /// XSAVE without its optimised form.
    Xsave = 2,
    /// XSAVEOPT, which skips components in their initial state or unchanged
    /// since the last restore from the same address.
    Xsaveopt = 3,
}

/// The machine-wide extended-state configuration every CPU must share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    flavour: Flavour,
    xcr0: u64,
    image_bytes: u32,
    scrub_x87_pointers: bool,
}

/// Bit position of the flavour in a [`Config::packed`] word.
const PACKED_FLAVOUR_SHIFT: u32 = 48;
/// Bit position of the image size in a [`Config::packed`] word.
const PACKED_IMAGE_SHIFT: u32 = 32;
/// Bit of a [`Config::packed`] word set when every restore must scrub the x87
/// pointers first.
const PACKED_SCRUB_BIT: u32 = 56;

impl Config {
    /// The configuration CPUID describes, `image_bytes` being CPUID
    /// leaf 0xD's `EBX` once `xcr0` is set, or ignored for FXSAVE.
    #[must_use]
    pub const fn new(flavour: Flavour, xcr0: u64, image_bytes: u32) -> Self {
        let image_bytes = match flavour {
            Flavour::Fxsave => FXSAVE_BYTES,
            Flavour::Xsave | Flavour::Xsaveopt => image_bytes,
        };
        Self {
            flavour,
            xcr0,
            image_bytes,
            scrub_x87_pointers: false,
        }
    }

    /// This configuration on a CPU that does or does not need the x87 pointers
    /// scrubbed before every restore ([`keeps_x87_pointers`]).
    #[must_use]
    pub const fn scrubbing_x87_pointers(self, scrub: bool) -> Self {
        Self {
            scrub_x87_pointers: scrub,
            ..self
        }
    }

    /// Whether every restore scrubs the x87 pointers first.
    #[must_use]
    pub const fn scrubs_x87_pointers(self) -> bool {
        self.scrub_x87_pointers
    }

    /// The save instruction.
    #[must_use]
    pub const fn flavour(self) -> Flavour {
        self.flavour
    }

    /// The enabled state components.
    #[must_use]
    pub const fn xcr0(self) -> u64 {
        self.xcr0
    }

    /// The components a park saves and a return to ring 3 restores: every
    /// enabled one but SSE, which the entry frame holds.
    #[must_use]
    pub const fn park_mask(self) -> u64 {
        self.xcr0 & !SSE
    }

    /// Bytes a task's area occupies at the top of its kernel stack.
    #[must_use]
    pub const fn area_bytes(self) -> u64 {
        HEADER_BYTES + (self.image_bytes as u64).next_multiple_of(IMAGE_ALIGN)
    }

    /// The configuration as one word, published and compared atomically:
    /// `xcr0` in the low half, the image size above it, the flavour's byte
    /// above that, and the scrub bit on top. Never zero, so zero means "not
    /// yet published".
    const fn packed(self) -> u64 {
        (self.xcr0 & 0xFFFF_FFFF)
            | ((self.image_bytes as u64) << PACKED_IMAGE_SHIFT)
            | ((self.flavour as u64) << PACKED_FLAVOUR_SHIFT)
            | ((self.scrub_x87_pointers as u64) << PACKED_SCRUB_BIT)
    }

    /// The inverse of [`Self::packed`], `None` for the unpublished zero.
    const fn unpacked(word: u64) -> Option<Self> {
        let flavour = match (word >> PACKED_FLAVOUR_SHIFT) & 0xFF {
            1 => Flavour::Fxsave,
            2 => Flavour::Xsave,
            3 => Flavour::Xsaveopt,
            _ => return None,
        };
        // Sixteen bits by the mask, so the narrowing is exact.
        #[allow(clippy::cast_possible_truncation)]
        let image_bytes = ((word >> PACKED_IMAGE_SHIFT) & 0xFFFF) as u32;
        Some(Self {
            flavour,
            xcr0: word & 0xFFFF_FFFF,
            image_bytes,
            scrub_x87_pointers: (word >> PACKED_SCRUB_BIT) & 1 == 1,
        })
    }
}

/// `CPUID.8000_0008H:EBX.XSaveErPtr`: every save form stores the x87 pointers
/// whether or not an exception is pending.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
const EXT8_EBX_XSAVE_ERPTR: u32 = 1 << 2;

/// Whether a CPU's saves and restores carry the x87 last-instruction,
/// last-data and last-opcode pointers when no exception is pending, so a
/// restore replaces the last task's. Only Intel, or a part reporting
/// `XSaveErPtr`, is trusted to: AMD before Zen 2 does not, and an unknown
/// vendor is scrubbed rather than assumed safe.
#[must_use]
pub fn keeps_x87_pointers(vendor: Option<&str>, xsave_erptr: bool) -> bool {
    xsave_erptr || vendor == Some("Intel")
}

/// The `XCR0` this kernel enables given the components the CPU supports
/// (CPUID leaf 0xD sub-leaf 0, `EDX:EAX`): x87 and SSE always, AVX when
/// supported, and the AVX-512 trio only whole and only on top of AVX. AMX,
/// MPX, PKRU and every supervisor component stay off: nothing here would save
/// them.
#[must_use]
pub const fn xcr0_for(supported: u64) -> u64 {
    let mut xcr0 = X87 | SSE;
    if supported & AVX != 0 {
        xcr0 |= AVX;
        if supported & AVX512 == AVX512 {
            xcr0 |= AVX512;
        }
    }
    xcr0
}

/// `CPUID.1:ECX.XSAVE`.
const LEAF1_ECX_XSAVE: u32 = 1 << 26;
/// `CPUID.(0xD,1):EAX.XSAVEOPT`.
const LEAF_D1_EAX_XSAVEOPT: u32 = 1 << 0;

/// The save instruction a CPU offers: XSAVEOPT, else XSAVE, else FXSAVE64,
/// which every x86-64 CPU has.
#[must_use]
pub const fn flavour_for(leaf1_ecx: u32, leaf_d1_eax: u32) -> Flavour {
    if leaf1_ecx & LEAF1_ECX_XSAVE == 0 {
        Flavour::Fxsave
    } else if leaf_d1_eax & LEAF_D1_EAX_XSAVEOPT != 0 {
        Flavour::Xsaveopt
    } else {
        Flavour::Xsave
    }
}

/// The published [`Config::packed`] word, zero until the boot CPU sets up
/// its state. Written once; every later CPU must match it.
pub(crate) static PUBLISHED: AtomicU64 = AtomicU64::new(0);

/// A CPU whose extended-state configuration differs from the boot CPU's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mismatch;

/// Publish `config` as the machine's, or confirm it matches the one the first
/// CPU published.
///
/// # Errors
///
/// [`Mismatch`] when an earlier CPU published a different configuration: a
/// task migrated between the two would have its state saved in one layout
/// and restored in another.
pub fn publish(config: Config) -> Result<(), Mismatch> {
    let word = config.packed();
    match PUBLISHED.compare_exchange(0, word, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => Ok(()),
        Err(existing) if existing == word => Ok(()),
        Err(_) => Err(Mismatch),
    }
}

/// The machine's configuration, once the boot CPU has published it.
#[must_use]
pub fn published() -> Option<Config> {
    Config::unpacked(PUBLISHED.load(Ordering::Acquire))
}

#[cfg(test)]
pub(crate) fn reset_published_for_tests() {
    PUBLISHED.store(0, Ordering::Release);
}

/// The `RSP0` for the kernel stack whose top is `stack_top`: the area's base,
/// directly below the top.
///
/// A ring-3 entry's frame ends at `RSP0` — the CPU aligns it to 16 bytes
/// before pushing, and it already is — so each stub finds the area at its
/// frame top.
///
/// # Errors
///
/// [`crate::percpu::InitError::NotInitialised`] before the configuration is
/// published, and [`crate::percpu::InitError::InvalidKernelStackPointer`]
/// for a stack top that is misaligned, outside the kernel half, or too low
/// to hold an area.
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
pub(crate) fn rsp0_below(stack_top: u64) -> Result<u64, crate::percpu::InitError> {
    use crate::percpu::InitError;
    crate::syscall_entry::validate_kernel_rsp0(stack_top)?;
    let config = published().ok_or(InitError::NotInitialised)?;
    let rsp0 = stack_top
        .checked_sub(config.area_bytes())
        .ok_or(InitError::InvalidKernelStackPointer)?
        & !(IMAGE_ALIGN - 1);
    crate::syscall_entry::validate_kernel_rsp0(rsp0)?;
    Ok(rsp0)
}

// --- The area --------------------------------------------------------------

/// Bytes of `AreaHeader` ahead of the image.
pub const HEADER_BYTES: u64 = 64;

/// Offset from an area's base of its image's XSAVE header. XSAVE writes none
/// of it but `XSTATE_BV`, and XRSTOR faults on reserved bytes left as the
/// stack found them, so a first entry zeroes it.
#[cfg(all(target_arch = "x86_64", target_os = "none", feature = "sched-arch"))]
pub(crate) const XSAVE_HEADER_OFFSET: u64 = HEADER_BYTES + FXSAVE_BYTES as u64;

/// The kernel's bookkeeping at the base of a task's area.
///
/// A first entry zeroes it, which reads as "no load pending, no CPU".
#[repr(C, align(64))]
#[derive(Debug, Default)]
pub struct AreaHeader {
    /// Non-zero while the registers do not hold this task's state: the next
    /// return to ring 3 loads the image.
    pub load_pending: u64,
    /// The CPU whose registers last held this task's state, plus one; zero
    /// when none has since the first entry.
    pub last_cpu: u64,
    /// The owner slot of the CPU a pending load is for, where the load
    /// records the area.
    pub owner_slot: u64,
    reserved: [u64; 5],
}

/// Offset of [`AreaHeader::load_pending`], which each stub's exit tests.
pub const LOAD_PENDING_OFFSET: usize = core::mem::offset_of!(AreaHeader, load_pending);
/// Offset of [`AreaHeader::owner_slot`], which the load writes through.
pub const OWNER_SLOT_OFFSET: usize = core::mem::offset_of!(AreaHeader, owner_slot);

const _: () = {
    assert!(core::mem::size_of::<AreaHeader>() as u64 == HEADER_BYTES);
    // The stubs test the flag with one byte-sized compare at the area base.
    assert!(LOAD_PENDING_OFFSET == 0);
};

/// Record a park on `cpu`: whether the registers hold the task's state and
/// must be saved. The caller then records the area as `cpu`'s owner.
#[must_use]
pub fn park(header: &mut AreaHeader, cpu: u64) -> bool {
    if header.load_pending != 0 {
        return false;
    }
    header.last_cpu = cpu + 1;
    true
}

/// Record a resume on `cpu`, whose owner slot at `owner_slot` holds `owner`,
/// of the task whose area is at `area`: mark a load pending unless the
/// registers still hold the task's state.
///
/// A load already pending stays pending, so a task parked again before it
/// returned to ring 3 still gets its state back.
pub fn resume(header: &mut AreaHeader, area: u64, cpu: u64, owner: u64, owner_slot: u64) {
    if owner == area && header.last_cpu == cpu + 1 {
        return;
    }
    header.load_pending = 1;
    header.last_cpu = cpu + 1;
    header.owner_slot = owner_slot;
}

/// A new context's image: every component in its initial state
/// (`XSTATE_BV` zero) with the initial x87 control word and `MXCSR`, which
/// XRSTOR loads even for a component it initialises and FXRSTOR64 loads as
/// they stand.
#[repr(C, align(64))]
pub struct InitImage {
    legacy: [u8; FXSAVE_BYTES as usize],
    header: [u8; XSAVE_HEADER_BYTES],
}

/// The x87 control word FNINIT establishes: every exception masked,
/// 64-bit precision, round to nearest.
const FCW_INIT: u16 = 0x037F;
/// Offset of the x87 control word in the legacy image.
const LEGACY_FCW_OFFSET: usize = 0;
/// Offset of `MXCSR` in the legacy image.
const LEGACY_MXCSR_OFFSET: usize = 24;

/// The one initial image every first entry restores from.
pub static INIT_IMAGE: InitImage = {
    let mut legacy = [0u8; FXSAVE_BYTES as usize];
    let fcw = FCW_INIT.to_le_bytes();
    legacy[LEGACY_FCW_OFFSET] = fcw[0];
    legacy[LEGACY_FCW_OFFSET + 1] = fcw[1];
    let mxcsr = crate::fpu::MXCSR_DEFAULT.to_le_bytes();
    let mut i = 0;
    while i < mxcsr.len() {
        legacy[LEGACY_MXCSR_OFFSET + i] = mxcsr[i];
        i += 1;
    }
    InitImage {
        legacy,
        header: [0; XSAVE_HEADER_BYTES],
    }
};

// --- The calling CPU ---------------------------------------------------------

/// `CR4.OSXSAVE`: XSAVE, XRSTOR and XGETBV are enabled.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
const CR4_OSXSAVE: u64 = 1 << 18;

/// Enable the extended state this kernel manages on the calling CPU and
/// publish the configuration, or check it against the one published.
///
/// Must precede this CPU's feature detection, whose AVX bits follow the
/// state enabled here, and any user task on it.
///
/// # Errors
///
/// [`Mismatch`] when this CPU's configuration differs from the boot CPU's.
///
/// # Safety
///
/// At CPL 0 on the CPU being brought up, with interrupts disabled.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub(crate) unsafe fn init_cpu() -> Result<(), Mismatch> {
    use core::arch::x86_64::{__cpuid, __cpuid_count};
    let leaf0 = __cpuid(0);
    let vendor = crate::cpuname::vendor_from_leaf0(leaf0.ebx, leaf0.edx, leaf0.ecx);
    let xsave_erptr = __cpuid(0x8000_0000).eax >= 0x8000_0008
        && __cpuid(0x8000_0008).ebx & EXT8_EBX_XSAVE_ERPTR != 0;
    let leaf1_ecx = __cpuid(1).ecx;
    let config = if leaf1_ecx & LEAF1_ECX_XSAVE == 0 {
        Config::new(Flavour::Fxsave, X87 | SSE, 0)
    } else {
        let leaf_d0 = __cpuid_count(0xD, 0);
        let xcr0 = xcr0_for((u64::from(leaf_d0.edx) << 32) | u64::from(leaf_d0.eax));
        let (lo, hi) = crate::msr::halves(xcr0);
        // SAFETY: CPUID advertises XSAVE, so `CR4.OSXSAVE` exists; with it set,
        // XSETBV accepts an `XCR0` built from the supported components under
        // the architecture's rules, which `xcr0_for` keeps.
        unsafe {
            core::arch::asm!(
                "mov {t}, cr4",
                "or {t}, {bit}",
                "mov cr4, {t}",
                t = out(reg) _,
                bit = in(reg) CR4_OSXSAVE,
                options(nostack, preserves_flags),
            );
            core::arch::asm!(
                "xsetbv",
                in("ecx") 0u32,
                in("eax") lo,
                in("edx") hi,
                options(nomem, nostack, preserves_flags),
            );
        }
        // The size is for the components now enabled, so it is read after.
        let image_bytes = __cpuid_count(0xD, 0).ebx;
        Config::new(
            flavour_for(leaf1_ecx, __cpuid_count(0xD, 1).eax),
            xcr0,
            image_bytes,
        )
    };
    publish(config.scrubbing_x87_pointers(!keeps_x87_pointers(vendor, xsave_erptr)))
}

/// Save the running task's extended state before it parks, unless a load is
/// still pending.
///
/// # Safety
///
/// On the parking user task's own control flow in the in-handler GS
/// convention, with this CPU's `RSP0` the task's.
#[cfg(all(target_arch = "x86_64", target_os = "none", feature = "sched-arch"))]
pub(crate) unsafe fn park_current() {
    let Some(config) = published() else {
        // SAFETY-INVARIANT: a task parks only after entering ring 3, which
        // refuses until the layout is published; parking unsaved would hand
        // the task's registers to the next one.
        crate::panic::refuse("a user task parked before the extended-state layout was published");
    };
    // SAFETY: the in-handler convention puts this CPU's TLS block in GS.
    let tls = unsafe { crate::syscall_entry::this_cpu_tls() };
    // SAFETY: `tls` is this CPU's registered block, and its `RSP0` is the
    // running task's area base, carved by `rsp0_below`.
    unsafe {
        let area = (*tls).kernel_rsp0;
        if !park(&mut *(area as *mut AreaHeader), (*tls).cpu_index) {
            return;
        }
        save(config, area + HEADER_BYTES);
        (*tls).xstate_owner = area;
    }
}

/// Mark the resumed task's extended state for loading on its way back to
/// ring 3, unless this CPU's registers still hold it.
///
/// # Safety
///
/// As `park_current`, on the resumed task's control flow.
#[cfg(all(target_arch = "x86_64", target_os = "none", feature = "sched-arch"))]
pub(crate) unsafe fn resume_current() {
    // SAFETY: as in `park_current`.
    unsafe {
        let tls = crate::syscall_entry::this_cpu_tls();
        let area = (*tls).kernel_rsp0;
        let owner_slot = core::ptr::addr_of_mut!((*tls).xstate_owner);
        resume(
            &mut *(area as *mut AreaHeader),
            area,
            (*tls).cpu_index,
            *owner_slot,
            owner_slot as u64,
        );
    }
}

/// Save every component the park covers into the image at `image`.
///
/// # Safety
///
/// `image` must be a 64-byte-aligned area image of `config`'s size.
#[cfg(all(target_arch = "x86_64", target_os = "none", feature = "sched-arch"))]
unsafe fn save(config: Config, image: u64) {
    let (lo, hi) = crate::msr::halves(config.park_mask());
    // SAFETY: each form writes at most `config`'s image size at the aligned
    // `image` and reads the registers without changing them.
    unsafe {
        match config.flavour() {
            Flavour::Xsaveopt => core::arch::asm!(
                "xsaveopt64 [{image}]",
                image = in(reg) image,
                in("eax") lo,
                in("edx") hi,
                options(nostack, preserves_flags),
            ),
            Flavour::Xsave => core::arch::asm!(
                "xsave64 [{image}]",
                image = in(reg) image,
                in("eax") lo,
                in("edx") hi,
                options(nostack, preserves_flags),
            ),
            Flavour::Fxsave => core::arch::asm!(
                "fxsave64 [{image}]",
                image = in(reg) image,
                options(nostack, preserves_flags),
            ),
        }
    }
}

/// Load a pending image on the way back to ring 3 and record the area as this
/// CPU's owner. `%rdi` is the area; clobbers `%rax` and `%rdx`.
///
/// Each returning stub tests the pending flag itself and calls this only
/// when it is set, after all Rust has run and before its SSE frame is
/// restored: FXRSTOR64 loads the `xmm` registers too, and XRSTOR loads
/// `MXCSR` — the kernel's, from the park — which the frame then replaces with
/// the task's.
///
/// # Safety
///
/// Only the entry stubs call it, with `%rdi` a pending area.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn tairix_arch_x86_64_xstate_load() {
    core::arch::naked_asm!(
        "movq $0, {pending}(%rdi)",
        "movq {owner_slot}(%rdi), %rax",
        "movq %rdi, (%rax)",
        "movq {published}(%rip), %rdx",
        "btq ${scrub_bit}, %rdx",
        "jnc 1f",
        "call {scrub}",
        "1:",
        "movl %edx, %eax",
        "btrl ${sse_bit}, %eax",
        "shrq ${flavour_shift}, %rdx",
        "cmpb ${fxsave}, %dl",
        "je 2f",
        "xorl %edx, %edx",
        "xrstor64 {image}(%rdi)",
        "ret",
        "2:",
        "fxrstor64 {image}(%rdi)",
        "ret",
        pending = const LOAD_PENDING_OFFSET,
        owner_slot = const OWNER_SLOT_OFFSET,
        published = sym PUBLISHED,
        scrub_bit = const PACKED_SCRUB_BIT,
        scrub = sym tairix_arch_x86_64_x87_scrub,
        sse_bit = const SSE.trailing_zeros(),
        flavour_shift = const PACKED_FLAVOUR_SHIFT,
        fxsave = const Flavour::Fxsave as u32,
        image = const HEADER_BYTES,
        options(att_syntax),
    )
}

/// The value [`tairix_arch_x86_64_x87_scrub`] loads, so the x87 data pointer
/// names this constant rather than anything a task touched.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
static X87_SCRUB_OPERAND: i32 = 0;

/// Point the x87 last-instruction, last-data and last-opcode registers at a
/// kernel constant ahead of a restore that would otherwise keep the last
/// task's. Clears any pending exception first so the load cannot fault, and
/// frees every register so it cannot overflow the stack.
///
/// # Safety
///
/// Only immediately ahead of an x87 restore, which replaces the stack entry
/// this leaves.
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn tairix_arch_x86_64_x87_scrub() {
    core::arch::naked_asm!(
        "fnclex",
        "emms",
        "fildl {operand}(%rip)",
        "ret",
        operand = sym X87_SCRUB_OPERAND,
        options(att_syntax),
    )
}

#[cfg(test)]
#[path = "xstate_tests.rs"]
mod tests;
