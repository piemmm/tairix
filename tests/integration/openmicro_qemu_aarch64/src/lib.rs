//! The Open Micro vertical's shared contract: the bundle the guest attributes the
//! launch and the exit to, and the syscall the exit is audited as.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use tairix_abi::{SyscallNumber, SYSCALLS};

/// The bundle's name, which its store path and its processes carry.
pub const OPENMICRO_APP_NAME: &str = "OpenMicro";

/// The audited name of the syscall a process ends with.
pub const EXIT_SYSCALL: &str = SYSCALLS[SyscallNumber::EXIT.as_u16() as usize].name;

#[cfg(test)]
mod tests {
    use super::EXIT_SYSCALL;
    use tairix_abi::{spec_for, SyscallNumber};

    #[test]
    fn the_exit_witness_names_the_syscall_the_dispatcher_audits() {
        assert_eq!(
            Some(EXIT_SYSCALL),
            spec_for(SyscallNumber::EXIT).map(|spec| spec.name)
        );
    }
}
