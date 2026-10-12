//! The network-map vertical's shared contract (`plans/NETMAP.md` `NM2c`).
//!
//! The freestanding guest kernel (`src/main.rs`) and the host runner's
//! enrolment (`tools/xtask/src/commands/qemu_tests.rs`) both read these
//! definitions, so the gesture the host injects and the witness the guest
//! latches can never drift apart.
//!
//! # What the vertical proves that no host test can
//!
//! Every piece of the map is host-tested: the model and its bounds, the ingest
//! that projects interfaces, subnets, routers and the gateway into it, the
//! passive inference, the layout, the viewport and the renderer. What only a
//! running machine can show is that they are wired to the live system — that
//! `netmap`, launched from the desktop's program library, reads the real
//! interface state, neighbour cache and routing table through the capability-
//! checked System Information API of a booted machine whose one NIC has
//! autoconfigured against a router-advertising peer, and draws its interface,
//! subnet and gateway tiers on screen.
//!
//! # Who states what
//!
//! - The **host** reads the serial transcript, so it gates its gestures and its
//!   screendump on the desktop session's own announcements: the desktop
//!   revealed, the library shown, the map's window on screen, and — the honest
//!   "the map has real data drawn, not an empty first frame" signal — the
//!   window on screen wearing the title it takes once its first read lands
//!   (`WINDOW_RETITLED`).
//! - The **guest** kernel's audit sink sees kernel audit records only, so it
//!   gates on those: the bundle a load names, the window channel's create
//!   reply, and the map's own clean exit — the user's *Quit*, the last gesture
//!   the host injects, and the only one it injects after reading the dump.
//!
//! Neither side infers the other's facts, and neither counts a cumulative
//! event an unrelated subsystem's cadence could shift under it
//! (`plans/OPEN-DEFECTS.md` D19/D20).
//!
//! # Why the guest cannot exit early
//!
//! The exit is caused by the script's final gesture (*Quit* on the map's icon-
//! bar slot), and that gesture is sent only after the runner has read the
//! screendump back. A map that failed before then exits too, but the host
//! gates the dump on `WINDOW_RETITLED`, which a failed map never reaches, so no
//! dump is captured and the tier assertion fails the run — a premature exit can
//! never pass.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use tairix_abi::SyscallNumber;

/// Bare name of the network-map application the library row launches — the
/// bundle is `<system application store>/<name>.app`, composed from the shared
/// `lib/abi` spellings on both sides rather than written out here.
pub const NETMAP_APP_NAME: &str = "netmap";

/// Process name (`comm`) the kernel attests for the launched map — the
/// principal whose clean exit (the user's *Quit*) ends the run.
pub const NETMAP_COMM: &str = NETMAP_APP_NAME;

/// Name of the syscall a process ends through, as the audit `sc` field renders
/// it. Read from the `abi-v1` table so a rename moves the witness with it
/// instead of leaving a string here that matches nothing and times out.
pub const EXIT_SYSCALL: &str = syscall_name(SyscallNumber::EXIT);

/// The audited name of `number`, as the dispatcher's `sc` field renders it.
///
/// An unassigned number has no name; the empty string matches no record, so a
/// gate built on one never latches (fail closed) rather than on the wrong call.
const fn syscall_name(number: SyscallNumber) -> &'static str {
    match tairix_abi::spec_for(number) {
        Some(spec) => spec.name,
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::{syscall_name, EXIT_SYSCALL};
    use tairix_abi::SyscallNumber;

    /// The exit witness names the `abi-v1` table's own spelling, so a renamed
    /// syscall fails this test rather than silently leaving the guest's gate
    /// unmatchable and the run reported as a timeout.
    #[test]
    fn the_exit_witness_names_the_syscall_the_dispatcher_audits() {
        assert_eq!(EXIT_SYSCALL, syscall_name(SyscallNumber::EXIT));
        assert!(!EXIT_SYSCALL.is_empty(), "exit is an assigned syscall");
    }
}
