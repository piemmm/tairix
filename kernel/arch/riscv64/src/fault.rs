//! riscv64 synchronous-exception cause decode.
//!
//! The S-mode trap vector ([`crate::trap`]) routes a U-mode `ecall` to the
//! syscall path and a supervisor timer / external interrupt to their
//! dispatchers. Every other synchronous exception is decoded here — which
//! page-fault class it is, whether `stval` holds its address — and handed to
//! the callbacks in [`tairix_arch_api::fault`]: one taken from U-mode is the
//! running task's fault and costs only that task, one taken from S-mode is
//! the kernel's own and goes to the fatal handler, or with none installed to
//! the port's own report, which parks the hart (never a silent reset).
//!
//! The decode builds on the host, so its unit tests run under `cargo test`;
//! only the CSR reads that feed it are gated to the freestanding target (in
//! [`crate::trap`]).

/// `scause` cause code for an Instruction page fault (privileged spec
/// table 4.2).
pub const SCAUSE_INSTRUCTION_PAGE_FAULT: u64 = 12;

/// `scause` cause code for a Load page fault — the cause raised when a
/// hart reads a virtual address that is unmapped (or lacks read
/// permission) in the active page table. The memory-isolation vertical
/// expects exactly this when the attacker reads the victim-only address.
pub const SCAUSE_LOAD_PAGE_FAULT: u64 = 13;

/// `scause` cause code for a Store/AMO page fault (privileged spec
/// table 4.2).
pub const SCAUSE_STORE_PAGE_FAULT: u64 = 15;

/// `true` iff `scause` denotes one of the three page-fault causes (the
/// interrupt bit is clear and the cause code is an instruction, load, or
/// store/AMO page fault).
#[must_use]
pub const fn is_page_fault(scause: u64) -> bool {
    if (scause & crate::trap::SCAUSE_INTERRUPT_BIT) != 0 {
        return false;
    }
    matches!(
        scause,
        SCAUSE_INSTRUCTION_PAGE_FAULT | SCAUSE_LOAD_PAGE_FAULT | SCAUSE_STORE_PAGE_FAULT
    )
}

/// `true` iff `scause` denotes a **load** page fault — the only class
/// the demand-paged file-mapping resolver may attempt to *resolve*. An
/// instruction page fault is never file backing (a file mapping is never
/// executable) and is not offered at all.
#[must_use]
pub const fn is_load_page_fault(scause: u64) -> bool {
    if (scause & crate::trap::SCAUSE_INTERRUPT_BIT) != 0 {
        return false;
    }
    scause == SCAUSE_LOAD_PAGE_FAULT
}

/// `true` iff `scause` denotes a **store/AMO** page fault. It is offered
/// to the resolver with `write = true`, which never resolves it: a file
/// mapping is read-only, so a store to it can never be made valid — and
/// once the target page is resident, resolving a store fault as "already
/// resident, retry" would re-execute the store into an endless fault
/// storm. The resolver kills the faulting task instead, so a store to a
/// read-only mapping (or any wild write) costs the task, never the hart.
#[must_use]
pub const fn is_store_page_fault(scause: u64) -> bool {
    if (scause & crate::trap::SCAUSE_INTERRUPT_BIT) != 0 {
        return false;
    }
    scause == SCAUSE_STORE_PAGE_FAULT
}

/// `true` iff `scause` denotes an **instruction** page fault. Offered to
/// the software A/D update path (an executable leaf whose Accessed bit was
/// cleared re-faults on the next fetch under Svade), never to the
/// demand-paged file resolver (a file mapping is never executable).
#[must_use]
pub const fn is_instruction_page_fault(scause: u64) -> bool {
    if (scause & crate::trap::SCAUSE_INTERRUPT_BIT) != 0 {
        return false;
    }
    scause == SCAUSE_INSTRUCTION_PAGE_FAULT
}

const SCAUSE_INSTRUCTION_MISALIGNED: u64 = 0;
const SCAUSE_INSTRUCTION_ACCESS_FAULT: u64 = 1;
const SCAUSE_BREAKPOINT: u64 = 3;
const SCAUSE_LOAD_MISALIGNED: u64 = 4;
const SCAUSE_LOAD_ACCESS_FAULT: u64 = 5;
const SCAUSE_STORE_MISALIGNED: u64 = 6;
const SCAUSE_STORE_ACCESS_FAULT: u64 = 7;

/// `true` iff the exception `scause` names left its faulting address in
/// `stval`: a misaligned access, an access fault, a page fault, or a
/// breakpoint (privileged spec, "Supervisor Trap Value Register"). An illegal
/// instruction leaves the instruction's bits there instead, and every other
/// cause, an interrupt included, leaves zero.
#[must_use]
pub const fn stval_is_address(scause: u64) -> bool {
    matches!(
        scause,
        SCAUSE_INSTRUCTION_MISALIGNED
            | SCAUSE_INSTRUCTION_ACCESS_FAULT
            | SCAUSE_BREAKPOINT
            | SCAUSE_LOAD_MISALIGNED
            | SCAUSE_LOAD_ACCESS_FAULT
            | SCAUSE_STORE_MISALIGNED
            | SCAUSE_STORE_ACCESS_FAULT
            | SCAUSE_INSTRUCTION_PAGE_FAULT
            | SCAUSE_LOAD_PAGE_FAULT
            | SCAUSE_STORE_PAGE_FAULT
    )
}

/// A kernel fault's words under the CSRs they came from, for the port's own
/// report.
#[cfg(any(test, all(target_arch = "riscv64", target_os = "none")))]
pub(crate) struct Decoded<'a>(pub(crate) &'a tairix_arch_api::fatal::KernelFault);

#[cfg(any(test, all(target_arch = "riscv64", target_os = "none")))]
impl core::fmt::Display for Decoded<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let fault = self.0;
        if let Some(scause) = fault.syndrome {
            write!(f, "scause {scause:#x}, ")?;
        }
        match fault.address {
            Some(stval) => write!(f, "stval {stval:#x}, ")?,
            None => f.write_str("no fault address, ")?,
        }
        write!(f, "sepc {:#x}", fault.pc)
    }
}

#[cfg(test)]
mod tests {
    use std::format;

    use tairix_arch_api::fatal::KernelFault;

    use super::*;
    use crate::trap::SCAUSE_INTERRUPT_BIT;

    #[test]
    fn a_fault_is_decoded_under_the_csrs_it_came_from() {
        let fault = KernelFault {
            syndrome: Some(13),
            address: Some(0xdead_0000),
            pc: 0x8020_1234,
            sp: Some(0x8040_0000),
        };
        assert_eq!(
            format!("{}", Decoded(&fault)),
            "scause 0xd, stval 0xdead0000, sepc 0x80201234"
        );
    }

    #[test]
    fn a_cause_with_no_address_says_so() {
        let illegal = KernelFault {
            syndrome: Some(2),
            address: None,
            pc: 0x8020_0000,
            sp: Some(0x8040_0000),
        };
        assert_eq!(
            format!("{}", Decoded(&illegal)),
            "scause 0x2, no fault address, sepc 0x80200000"
        );
    }

    #[test]
    fn page_fault_causes_are_recognised() {
        assert!(is_page_fault(SCAUSE_INSTRUCTION_PAGE_FAULT));
        assert!(is_page_fault(SCAUSE_LOAD_PAGE_FAULT));
        assert!(is_page_fault(SCAUSE_STORE_PAGE_FAULT));
    }

    #[test]
    fn interrupts_are_not_page_faults() {
        // The interrupt bit set with cause 13 is a (non-existent) async
        // cause, never the synchronous load page fault.
        assert!(!is_page_fault(
            SCAUSE_INTERRUPT_BIT | SCAUSE_LOAD_PAGE_FAULT
        ));
    }

    #[test]
    fn unrelated_exceptions_are_not_page_faults() {
        // Cause 2 is an illegal instruction; cause 8 is an ecall.
        assert!(!is_page_fault(2));
        assert!(!is_page_fault(8));
    }

    #[test]
    fn cause_codes_match_privileged_spec() {
        assert_eq!(SCAUSE_INSTRUCTION_PAGE_FAULT, 12);
        assert_eq!(SCAUSE_LOAD_PAGE_FAULT, 13);
        assert_eq!(SCAUSE_STORE_PAGE_FAULT, 15);
    }

    #[test]
    fn load_and_store_page_faults_are_classified_for_the_resolver() {
        // A load page fault is the resolvable class (demand-paged file
        // backing); a store/AMO page fault is offered with `write = true`
        // and is always fatal to the task (file mappings are read-only —
        // resolving a store against a resident page would retry it
        // forever, and before this classification a user store could park
        // the whole hart).
        assert!(is_load_page_fault(SCAUSE_LOAD_PAGE_FAULT));
        assert!(!is_load_page_fault(SCAUSE_STORE_PAGE_FAULT));
        assert!(is_store_page_fault(SCAUSE_STORE_PAGE_FAULT));
        assert!(!is_store_page_fault(SCAUSE_LOAD_PAGE_FAULT));
        // Instruction page faults, interrupts, and an `ecall` are never
        // offered to the user-fault resolver.
        assert!(!is_load_page_fault(SCAUSE_INSTRUCTION_PAGE_FAULT));
        assert!(!is_store_page_fault(SCAUSE_INSTRUCTION_PAGE_FAULT));
        assert!(!is_load_page_fault(
            crate::trap::SCAUSE_INTERRUPT_BIT | SCAUSE_LOAD_PAGE_FAULT
        ));
        assert!(!is_store_page_fault(
            crate::trap::SCAUSE_INTERRUPT_BIT | SCAUSE_STORE_PAGE_FAULT
        ));
        assert!(!is_load_page_fault(8));
        assert!(!is_store_page_fault(8));
    }

    #[test]
    fn stval_is_an_address_only_for_the_causes_that_write_one() {
        for cause in [0, 1, 3, 4, 5, 6, 7, 12, 13, 15] {
            assert!(stval_is_address(cause), "cause {cause}");
        }
        // An illegal instruction (its bits), an `ecall` from either mode, and
        // an interrupt with an address cause's number: no address.
        for cause in [2, 8, 9, 11] {
            assert!(!stval_is_address(cause), "cause {cause}");
        }
        assert!(!stval_is_address(
            SCAUSE_INTERRUPT_BIT | SCAUSE_LOAD_PAGE_FAULT
        ));
    }
}
