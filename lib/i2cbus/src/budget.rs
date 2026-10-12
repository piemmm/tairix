//! How long a controller waits on its bus before giving a transaction up.

/// The longest a target may hold the bus still before its controller gives
/// the transaction up: the Linux I²C core's default adapter timeout.
pub const STALL_ALLOWANCE_NS: u64 = 1_000_000_000;

/// Bit periods one byte costs on the wire: eight data bits and the
/// acknowledge.
const BITS_PER_BYTE: u64 = 9;

/// Byte periods a message's START and address byte, and a STOP, add.
const FRAMING_BYTES: u64 = 2;

const NS_PER_S: u64 = 1_000_000_000;

/// How long `bytes` bytes and one message's framing take on a bus clocked at
/// `scl_hz`; a stopped clock is taken as the slowest one.
#[must_use]
pub fn wire_ns(bytes: u64, scl_hz: u64) -> u64 {
    bytes
        .saturating_add(FRAMING_BYTES)
        .saturating_mul(BITS_PER_BYTE)
        .saturating_mul(NS_PER_S)
        .div_ceil(scl_hz.max(1))
}

/// How long a step moving `bytes` bytes may go without progress before the
/// controller gives the transaction up: its time on the wire and the stall
/// allowance.
#[must_use]
pub fn step_budget_ns(bytes: u64, scl_hz: u64) -> u64 {
    wire_ns(bytes, scl_hz).saturating_add(STALL_ALLOWANCE_NS)
}

#[cfg(test)]
mod tests {
    use super::{step_budget_ns, wire_ns, STALL_ALLOWANCE_NS};

    #[test]
    fn a_step_is_its_bytes_and_framing_on_the_wire_and_the_stall_allowance() {
        assert_eq!(wire_ns(16, 100_000), 1_620_000, "18 bytes of 9 bits");
        assert_eq!(step_budget_ns(16, 100_000), 1_620_000 + STALL_ALLOWANCE_NS);
        assert!(step_budget_ns(16, 1_000) > step_budget_ns(16, 400_000));
        assert!(wire_ns(32, 97_500) > wire_ns(1, 97_500));
        assert_eq!(
            wire_ns(1, 0),
            wire_ns(1, 1),
            "no division by a stopped clock"
        );
        assert_eq!(
            wire_ns(u64::MAX, 1),
            u64::MAX,
            "saturates rather than wraps"
        );
    }
}
