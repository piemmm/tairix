//! A hart's supervisor-level IMSIC interrupt file: its registers reached
//! through the `siselect`/`sireg` indirect CSRs, its pending enabled
//! identities claimed through `stopei`, lowest-numbered first (RISC-V
//! Advanced Interrupt Architecture, chapter 3).

use crate::fdt::MAX_IDENTITIES;

/// The file's registers `siselect` names.
pub mod reg {
    /// Whether the file raises the hart's supervisor external interrupt.
    pub const EIDELIVERY: u32 = 0x70;
    /// The identity at or above which nothing is delivered; zero for none.
    pub const EITHRESHOLD: u32 = 0x72;
    /// The first pending-bit register.
    pub const EIP0: u32 = 0x80;
    /// The first enable-bit register.
    pub const EIE0: u32 = 0xC0;

    /// The register holding identity `id`'s bit among the 64-bit ones from
    /// `first`; a 64-bit hart has only the even-numbered ones.
    #[must_use]
    pub const fn holding(first: u32, id: u32) -> u32 {
        first + 2 * (id / 64)
    }
}

/// A file's registers as its own hart reaches them.
pub trait InterruptFile {
    /// Read `register`.
    fn read(&self, register: u32) -> u64;
    /// Write `register`.
    fn write(&self, register: u32, value: u64);
    /// Take the pending enabled identity of highest priority, the lowest
    /// numbered, clearing its pending bit: zero when there is none.
    fn claim(&self) -> u32;
}

/// A file the kernel has taken over.
pub struct Imsic<F> {
    file: F,
    ids: u32,
}

impl<F: InterruptFile> Imsic<F> {
    /// Take the file over: delivery off while every identity is cleared and
    /// enabled, no threshold, then delivery on. Every identity is open so no
    /// later change needs the file's own hart; one nothing was routed to
    /// raises nothing when claimed. [`None`] for an identity count outside
    /// `63..=2047`.
    pub fn take(file: F, ids: u32) -> Option<Self> {
        if !(63..=MAX_IDENTITIES).contains(&ids) {
            return None;
        }
        file.write(reg::EIDELIVERY, 0);
        for id in (0..=ids).step_by(64) {
            file.write(reg::holding(reg::EIP0, id), 0);
            file.write(reg::holding(reg::EIE0, id), u64::MAX);
        }
        file.write(reg::EITHRESHOLD, 0);
        file.write(reg::EIDELIVERY, 1);
        Some(Self { file, ids })
    }

    /// Its identities, `1..=ids`.
    #[must_use]
    pub const fn ids(&self) -> u32 {
        self.ids
    }

    /// Take the pending enabled identity of highest priority.
    pub fn claim(&self) -> Option<u32> {
        match self.file.claim() {
            0 => None,
            id => Some(id),
        }
    }
}

/// The running hart's own file, through its CSRs.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
pub struct HartFile;

#[cfg(all(target_arch = "riscv64", target_os = "none"))]
impl HartFile {
    /// Run `access` with `register` selected and the hart's interrupts off,
    /// so no handler reselects between the two.
    fn selected<T>(register: u32, access: impl FnOnce() -> T) -> T {
        let status: usize;
        // SAFETY: clears `sstatus.SIE` and selects a file register, touching
        // no memory; the previous enable is restored below.
        unsafe {
            core::arch::asm!(
                "csrrci {status}, sstatus, 2",
                "csrw 0x150, {register}",
                status = out(reg) status,
                register = in(reg) register as usize,
                options(nomem, nostack),
            );
        }
        let value = access();
        if status & 2 != 0 {
            // SAFETY: restores the interrupt enable found on entry.
            unsafe { core::arch::asm!("csrsi sstatus, 2", options(nomem, nostack)) };
        }
        value
    }
}

#[cfg(all(target_arch = "riscv64", target_os = "none"))]
impl InterruptFile for HartFile {
    fn read(&self, register: u32) -> u64 {
        Self::selected(register, || {
            let value: u64;
            // SAFETY: reads the register `selected` chose.
            unsafe { core::arch::asm!("csrr {}, 0x151", out(reg) value, options(nomem, nostack)) };
            value
        })
    }

    fn write(&self, register: u32, value: u64) {
        Self::selected(register, || {
            // SAFETY: writes the register `selected` chose.
            unsafe { core::arch::asm!("csrw 0x151, {}", in(reg) value, options(nomem, nostack)) };
        });
    }

    fn claim(&self) -> u32 {
        let top: u64;
        // SAFETY: a swap of `stopei` with zero reads the top pending enabled
        // identity and clears its pending bit, touching no memory.
        unsafe { core::arch::asm!("csrrw {}, 0x15C, zero", out(reg) top, options(nomem, nostack)) };
        ((top >> 16) & 0x7FF) as u32
    }
}

#[cfg(test)]
#[path = "imsic_tests.rs"]
mod tests;
