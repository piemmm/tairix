//! The one `PortRoom` wait-set member a loop keeps on the port it holds a
//! payload for.
//!
//! Room is level-triggered — a member is ready while its port has room — so a
//! member left armed on a port the loop is owed nothing for wakes it for ever.
//! The member therefore follows what the loop holds: on the port it waits on,
//! and on none once nothing waits.

use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};

use crate::waitset_ctl;

/// A loop's room member: on one port at most, armed on `set` under `token`.
#[derive(Debug)]
pub struct RoomMember {
    set: u64,
    token: u64,
    on: Option<u64>,
}

impl RoomMember {
    /// A member on no port yet, armed on wait-set `set` under `token` once one
    /// is wanted.
    #[must_use]
    pub const fn new(set: u64, token: u64) -> Self {
        Self {
            set,
            token,
            on: None,
        }
    }

    /// Keep the member on `wanted` and on no other port, answering whether it
    /// is armed there: `false` when the kernel would not arm it, so nothing
    /// will report that port's room and what waits on it needs a timed retry.
    pub fn keep_on(&mut self, wanted: Option<u64>) -> bool {
        if self.on == wanted {
            return true;
        }
        if let Some(port) = self.on.take() {
            let _ = waitset_ctl(
                self.set,
                WaitSetOp::Del,
                WaitSourceKind::PortRoom,
                port,
                self.token,
            );
        }
        let Some(port) = wanted else {
            return true;
        };
        let armed = waitset_ctl(
            self.set,
            WaitSetOp::Add,
            WaitSourceKind::PortRoom,
            port,
            self.token,
        ) == 0;
        if armed {
            self.on = Some(port);
        }
        armed
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
    use tairix_abi::Errno;
    use tairix_abi_trap::seam::{arm_script, calls, refusal};

    use super::RoomMember;
    use crate::NUM_WAITSET_CTL;

    const SET: u64 = 9;
    const TOKEN: u64 = 4;

    /// The membership changes the member made since the script was armed.
    fn changes() -> Vec<(WaitSetOp, u64)> {
        calls()
            .iter()
            .map(|(number, args)| {
                assert_eq!(*number, NUM_WAITSET_CTL);
                assert_eq!((args[0], args[2], args[4]), (SET, kind(), TOKEN));
                let op = if args[1] == u64::from(WaitSetOp::Add.as_u32()) {
                    WaitSetOp::Add
                } else {
                    WaitSetOp::Del
                };
                (op, args[3])
            })
            .collect()
    }

    fn kind() -> u64 {
        u64::from(WaitSourceKind::PortRoom.as_u32())
    }

    /// The member follows the port wanted: armed there once, moved with it,
    /// and taken off when none is wanted, never touching the set for a port
    /// it is already on.
    #[test]
    fn a_member_follows_the_port_wanted_and_no_other() {
        let mut room = RoomMember::new(SET, TOKEN);
        arm_script(&[], 0);
        assert!(room.keep_on(Some(5)));
        assert!(room.keep_on(Some(5)));
        assert!(room.keep_on(Some(6)));
        assert!(room.keep_on(None));
        assert!(room.keep_on(None));
        assert_eq!(
            changes(),
            [
                (WaitSetOp::Add, 5),
                (WaitSetOp::Del, 5),
                (WaitSetOp::Add, 6),
                (WaitSetOp::Del, 6),
            ]
        );
    }

    /// A member the kernel will not arm says so, is on no port, and is tried
    /// again the next time it is wanted.
    #[test]
    fn a_member_the_kernel_refuses_is_on_no_port_and_tried_again() {
        let mut room = RoomMember::new(SET, TOKEN);
        arm_script(&[refusal(Errno::LimitExceeded)], 0);
        assert!(!room.keep_on(Some(7)));
        assert!(room.keep_on(Some(7)), "tried again, it takes");
        assert_eq!(changes(), [(WaitSetOp::Add, 7), (WaitSetOp::Add, 7)]);
    }
}
