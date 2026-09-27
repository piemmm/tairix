//! aarch64 synchronous-exception syndrome decode.
//!
//! The EL1 exception vector ([`crate::exceptions`]) routes an IRQ to the
//! timer/IPI path and an EL0 `svc` to the syscall path. Every other exception
//! is decoded here — which class it is, whether it was a write, whether
//! `FAR_EL1` holds its address — and handed to the callbacks in
//! [`tairix_arch_api::fault`]: a user fault to the resolver or terminator, and
//! a kernel fault to the fatal handler, or with none installed to the port's
//! own report. Resuming a faulting instruction without a fix-up would re-trap
//! forever, so an unresolved kernel fault parks the CPU, never silently resets.
//!
//! The decode builds on the host, so its unit tests run under `cargo test`;
//! only the system-register reads that feed it are gated to the freestanding
//! target (in [`crate::exceptions`]).

/// Shift of the `ESR_ELx.EC` (exception class) field (bits `[31:26]`,
/// ARM ARM D17.2.37).
pub const ESR_EC_SHIFT: u64 = 26;

/// Mask of the `ESR_ELx.EC` field after shifting.
pub const ESR_EC_MASK: u64 = 0x3F;

/// `EC` for a Data Abort taken from a lower EL (e.g. EL0 reading an
/// unmapped user address). ARM ARM Table D17-2.
pub const EC_DATA_ABORT_LOWER: u64 = 0b10_0100;

/// `EC` for a Data Abort taken from the current EL (e.g. EL1 reading an
/// address the active translation regime does not map). The
/// memory-isolation vertical expects exactly this when the attacker
/// space reads the victim-only address.
pub const EC_DATA_ABORT_SAME: u64 = 0b10_0101;

/// `EC` for an Instruction Abort taken from a lower EL.
pub const EC_INSTRUCTION_ABORT_LOWER: u64 = 0b10_0000;

/// `EC` for an Instruction Abort taken from the current EL.
pub const EC_INSTRUCTION_ABORT_SAME: u64 = 0b10_0001;

/// Extract the exception class (`EC`) from a raw `ESR_EL1` value.
#[must_use]
pub const fn exception_class(esr: u64) -> u64 {
    (esr >> ESR_EC_SHIFT) & ESR_EC_MASK
}

/// `ESR_ELx.ISS.WnR` (bit 6) for a data abort: `1` = the abort was
/// raised by a write (or a cache-maintenance operation, which reports as
/// a write). ARM ARM D17.2.37, ISS encoding for a Data Abort.
pub const ESR_ISS_WNR: u64 = 1 << 6;

/// `true` iff `esr` denotes a data abort taken from a lower EL — an EL0
/// user access that could not be translated. This is the only exception
/// class the demand-paged file-mapping resolver may attempt to resolve:
/// a kernel-mode abort or an instruction abort is never file backing and
/// always takes the fatal path.
#[must_use]
pub const fn is_lower_el_data_abort(esr: u64) -> bool {
    exception_class(esr) == EC_DATA_ABORT_LOWER
}

/// `true` iff `esr` denotes a data abort taken from the **current** EL
/// — EL1 kernel code touching an address the active translation regime
/// refuses. The only recoverable shape is a fault inside the guarded
/// user-copy window (`crate::uaccess`), which the trap handler redirects
/// to the copy's fix-up; every other same-EL abort stays fatal.
#[must_use]
pub const fn is_current_el_data_abort(esr: u64) -> bool {
    exception_class(esr) == EC_DATA_ABORT_SAME
}

/// `true` iff a data abort's `esr` reports a **write** access (`WnR`
/// set). Only meaningful when [`is_lower_el_data_abort`] (or another
/// data-abort class check) already holds.
///
/// A write abort is never offered to the demand-paged file-mapping
/// resolver: a file mapping is read-only, so a store to it can never be
/// made valid — and once the target page is resident, resolving a write
/// fault as "already resident, retry" would re-execute the store into an
/// endless fault storm instead of killing the task. Write aborts always
/// take the fatal path.
#[must_use]
pub const fn is_write_data_abort(esr: u64) -> bool {
    esr & ESR_ISS_WNR != 0
}

/// `true` iff `esr` denotes a data or instruction abort (a page fault),
/// taken from either the current or a lower EL.
#[must_use]
pub const fn is_abort(esr: u64) -> bool {
    matches!(
        exception_class(esr),
        EC_DATA_ABORT_LOWER
            | EC_DATA_ABORT_SAME
            | EC_INSTRUCTION_ABORT_LOWER
            | EC_INSTRUCTION_ABORT_SAME
    )
}

/// Mask of the fault-status-code field (`DFSC` for a data abort, `IFSC`
/// for an instruction abort) in `ESR_ELx.ISS` — bits `[5:0]`. ARM ARM
/// D17.2.37 (Data Abort) / D17.2.38 (Instruction Abort).
pub const ESR_ISS_FSC_MASK: u64 = 0b11_1111;

/// The fault-status-code value for an **Access Flag fault**, minus the
/// low two bits that carry the translation *level* (`0b001000`..`0b001011`
/// for levels 0–3). ARM ARM Table D17-3 / D17-4: an access to a valid
/// leaf whose Access Flag (AF, bit 10) is clear raises this fault on a PE
/// without ARMv8.1 HAFDBS hardware AF management (cortex-a57/a72, the
/// default QEMU CPU). It is the software referenced-bit mechanism the
/// cold-page scanner drives.
pub const FSC_ACCESS_FLAG_BASE: u64 = 0b00_1000;

/// `true` iff a data or instruction abort's `esr` reports an **Access
/// Flag fault** at any translation level (`FSC == 0b0010xx`).
///
/// The synchronous-exception path resolves this by setting AF back on the
/// faulting leaf ([`crate::paging::set_accessed_flag_in_active`]) and
/// retrying, rather than treating it as a fatal translation fault. Only
/// meaningful when [`is_abort`] already holds.
#[must_use]
pub const fn is_access_flag_fault(esr: u64) -> bool {
    (esr & ESR_ISS_FSC_MASK) & !0b11 == FSC_ACCESS_FLAG_BASE
}

/// `ESR_ELx.ISS.FnV` (bit 10) for an instruction abort, a data abort, or a
/// watchpoint: set when `FAR_EL1` does not hold the faulting address, as for
/// a synchronous external abort. ARM ARM D17.2.37.
pub const ESR_ISS_FNV: u64 = 1 << 10;

/// `EC` for a PC alignment fault: `FAR_EL1` holds the misaligned PC.
pub const EC_PC_ALIGNMENT: u64 = 0b10_0010;

/// `EC` for a watchpoint taken from a lower EL.
pub const EC_WATCHPOINT_LOWER: u64 = 0b11_0100;

/// `EC` for a watchpoint taken from the current EL.
pub const EC_WATCHPOINT_SAME: u64 = 0b11_0101;

/// `true` iff the exception `esr` describes left its address in `FAR_EL1`.
///
/// Only an instruction abort, a PC alignment fault, a data abort and a
/// watchpoint write it, and an abort or a watchpoint with `FnV` set still does
/// not; for every other class the register is UNKNOWN, and reading it would
/// report whatever the last fault left there (ARM ARM D17.2.40).
#[must_use]
pub const fn far_is_valid(esr: u64) -> bool {
    match exception_class(esr) {
        EC_PC_ALIGNMENT => true,
        EC_INSTRUCTION_ABORT_LOWER
        | EC_INSTRUCTION_ABORT_SAME
        | EC_DATA_ABORT_LOWER
        | EC_DATA_ABORT_SAME
        | EC_WATCHPOINT_LOWER
        | EC_WATCHPOINT_SAME => esr & ESR_ISS_FNV == 0,
        _ => false,
    }
}

/// A kernel fault's words under the registers they came from, for the port's
/// own report.
#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
pub(crate) struct Decoded<'a>(pub(crate) &'a tairix_arch_api::fatal::KernelFault);

#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
impl core::fmt::Display for Decoded<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let fault = self.0;
        match fault.syndrome {
            Some(esr) => write!(f, "ESR_EL1 {esr:#x} (EC {:#04x}), ", exception_class(esr))?,
            None => f.write_str("no syndrome (an FIQ, or an IRQ from AArch32), ")?,
        }
        match fault.address {
            Some(far) => write!(f, "FAR_EL1 {far:#x}, ")?,
            None => f.write_str("no fault address, ")?,
        }
        write!(f, "ELR_EL1 {:#x}", fault.pc)
    }
}

#[cfg(test)]
mod tests {
    use std::format;

    use tairix_arch_api::fatal::KernelFault;

    use super::*;

    #[test]
    fn a_fault_is_decoded_under_the_registers_it_came_from() {
        let abort = KernelFault {
            syndrome: Some(0x9600_0045),
            address: Some(0xdead_0000),
            pc: 0x4008_1234,
            sp: Some(0x4010_0000),
        };
        assert_eq!(
            format!("{}", Decoded(&abort)),
            "ESR_EL1 0x96000045 (EC 0x25), FAR_EL1 0xdead0000, ELR_EL1 0x40081234"
        );
    }

    #[test]
    fn a_word_the_cpu_did_not_give_is_said_to_be_absent() {
        let fiq = KernelFault {
            syndrome: None,
            address: None,
            pc: 0x4008_0000,
            sp: None,
        };
        assert_eq!(
            format!("{}", Decoded(&fiq)),
            "no syndrome (an FIQ, or an IRQ from AArch32), no fault address, ELR_EL1 0x40080000"
        );
    }

    #[test]
    fn aborts_are_recognised() {
        assert!(is_abort(EC_DATA_ABORT_SAME << ESR_EC_SHIFT));
        assert!(is_abort(EC_DATA_ABORT_LOWER << ESR_EC_SHIFT));
        assert!(is_abort(EC_INSTRUCTION_ABORT_SAME << ESR_EC_SHIFT));
        assert!(is_abort(EC_INSTRUCTION_ABORT_LOWER << ESR_EC_SHIFT));
    }

    #[test]
    fn non_aborts_are_not_recognised() {
        // EC 0b010101 is an SVC from AArch64 (a syscall), not an abort.
        assert!(!is_abort(0b01_0101 << ESR_EC_SHIFT));
        // EC 0 is "unknown reason".
        assert!(!is_abort(0));
    }

    #[test]
    fn exception_class_extracts_the_ec_field() {
        let esr = (EC_DATA_ABORT_SAME << ESR_EC_SHIFT) | 0x37; // ISS noise
        assert_eq!(exception_class(esr), EC_DATA_ABORT_SAME);
    }

    #[test]
    fn write_aborts_are_distinguished_from_reads() {
        let read_abort = EC_DATA_ABORT_LOWER << ESR_EC_SHIFT;
        let write_abort = read_abort | ESR_ISS_WNR;
        // A store to a read-only file mapping must never be offered to
        // the resolver (it would retry forever against a resident page);
        // a read abort remains resolvable.
        assert!(is_write_data_abort(write_abort));
        assert!(!is_write_data_abort(read_abort));
        // WnR is ISS bit 6 (ARM ARM D17.2.37).
        assert_eq!(ESR_ISS_WNR, 1 << 6);
    }

    #[test]
    fn access_flag_faults_are_recognised_at_every_level() {
        // FSC `0b0010LL` (0x08..=0x0B) is an Access Flag fault at levels
        // 0–3; a 4 KiB leaf faults at level 3 (0x0B). All four are the
        // software referenced-bit mechanism.
        for level in 0..=3u64 {
            let esr = (EC_DATA_ABORT_LOWER << ESR_EC_SHIFT) | (FSC_ACCESS_FLAG_BASE + level);
            assert!(is_access_flag_fault(esr), "level {level} data abort");
            let iesr =
                (EC_INSTRUCTION_ABORT_LOWER << ESR_EC_SHIFT) | (FSC_ACCESS_FLAG_BASE + level);
            assert!(is_access_flag_fault(iesr), "level {level} instr abort");
        }
    }

    #[test]
    fn non_access_flag_faults_are_not_misread() {
        // A translation fault (FSC `0b0001LL`, 0x04..=0x07) and a
        // permission fault (FSC `0b0011LL`, 0x0C..=0x0F) must not be taken
        // for the referenced-bit mechanism — resolving them by setting AF
        // would mask a genuine fault (fail closed).
        for fsc in [
            0x04u64, 0x05, 0x06, 0x07, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x00,
        ] {
            let esr = (EC_DATA_ABORT_LOWER << ESR_EC_SHIFT) | fsc;
            assert!(!is_access_flag_fault(esr), "FSC {fsc:#x} must not match");
        }
        // The FSC field is exactly bits [5:0]; ISS noise above it (e.g.
        // WnR at bit 6) must not perturb the classification.
        let esr = (EC_DATA_ABORT_LOWER << ESR_EC_SHIFT) | ESR_ISS_WNR | (FSC_ACCESS_FLAG_BASE + 3);
        assert!(is_access_flag_fault(esr));
        assert_eq!(ESR_ISS_FSC_MASK, 0x3F);
        assert_eq!(FSC_ACCESS_FLAG_BASE, 0x08);
    }

    #[test]
    fn ec_codes_match_arm_arm() {
        assert_eq!(EC_DATA_ABORT_LOWER, 0x24);
        assert_eq!(EC_DATA_ABORT_SAME, 0x25);
        assert_eq!(EC_INSTRUCTION_ABORT_LOWER, 0x20);
        assert_eq!(EC_INSTRUCTION_ABORT_SAME, 0x21);
    }

    #[test]
    fn lower_el_data_aborts_are_distinguished() {
        assert!(is_lower_el_data_abort(EC_DATA_ABORT_LOWER << ESR_EC_SHIFT));
        // Same-EL data aborts, instruction aborts, and an EL0 `svc` are
        // never offered to the user-fault resolver.
        assert!(!is_lower_el_data_abort(EC_DATA_ABORT_SAME << ESR_EC_SHIFT));
        assert!(!is_lower_el_data_abort(
            EC_INSTRUCTION_ABORT_LOWER << ESR_EC_SHIFT
        ));
        assert!(!is_lower_el_data_abort(0b01_0101 << ESR_EC_SHIFT));
    }

    #[test]
    fn current_el_data_aborts_are_distinguished() {
        let same = EC_DATA_ABORT_SAME << ESR_EC_SHIFT;
        let lower = EC_DATA_ABORT_LOWER << ESR_EC_SHIFT;
        let instr_same = EC_INSTRUCTION_ABORT_SAME << ESR_EC_SHIFT;
        assert!(is_current_el_data_abort(same));
        assert!(!is_current_el_data_abort(lower));
        assert!(!is_current_el_data_abort(instr_same));
        assert!(!is_current_el_data_abort(0));
    }

    #[test]
    fn far_is_read_only_for_the_classes_that_write_it() {
        for ec in [
            EC_INSTRUCTION_ABORT_LOWER,
            EC_INSTRUCTION_ABORT_SAME,
            EC_PC_ALIGNMENT,
            EC_DATA_ABORT_LOWER,
            EC_DATA_ABORT_SAME,
            EC_WATCHPOINT_LOWER,
            EC_WATCHPOINT_SAME,
        ] {
            assert!(far_is_valid(ec << ESR_EC_SHIFT), "EC {ec:#x}");
        }
        // Unknown reason, an `svc`, an SP alignment fault, a `brk`, an SError:
        // `FAR_EL1` is UNKNOWN for each.
        for ec in [0x00u64, 0x15, 0x26, 0x3c, 0x2f] {
            assert!(!far_is_valid(ec << ESR_EC_SHIFT), "EC {ec:#x}");
        }
    }

    /// An abort whose `FnV` is set — a synchronous external abort that could
    /// not name its address — has no fault address to report.
    #[test]
    fn an_abort_with_fnv_set_names_no_address() {
        for ec in [
            EC_DATA_ABORT_SAME,
            EC_DATA_ABORT_LOWER,
            EC_INSTRUCTION_ABORT_SAME,
            EC_WATCHPOINT_SAME,
        ] {
            assert!(
                !far_is_valid((ec << ESR_EC_SHIFT) | ESR_ISS_FNV),
                "EC {ec:#x}"
            );
        }
        assert_eq!(ESR_ISS_FNV, 1 << 10);
        assert_eq!(EC_PC_ALIGNMENT, 0x22);
        assert_eq!(EC_WATCHPOINT_LOWER, 0x34);
        assert_eq!(EC_WATCHPOINT_SAME, 0x35);
    }
}
