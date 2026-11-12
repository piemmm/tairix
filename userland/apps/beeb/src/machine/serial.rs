//! The serial ULA: the 6850's transmit and receive clocks, the switch that
//! gives the 6850's lines to the cassette or to RS423, and the cassette motor
//! relay.

/// The rate each three-bit field selects, assuming the 6850 divides its
/// clock by 64 (the Advanced User Guide, 20.9).
pub const BAUD: [u64; 8] = [19_200, 1_200, 4_800, 150, 9_600, 300, 2_400, 75];

/// The clock the data separator gives the 6850's receiver from a playing
/// cassette: eight times the 2400 Hz carrier, 300 baud's rate.
const CASSETTE_RECEIVE_CLOCK: u64 = 64 * 300;

/// RS423 rather than the cassette has the 6850.
pub const RS423: u8 = 0x40;
/// The cassette motor relay is closed.
pub const MOTOR: u8 = 0x80;
/// A rate field: the transmit rate's at bit 0, the receive rate's at
/// `RECEIVE_SHIFT`.
pub const RATE: u8 = 0x07;
/// Where the receive rate's field starts.
pub const RECEIVE_SHIFT: u8 = 3;

/// The chip's single write-only register.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialUla {
    control: u8,
}

impl SerialUla {
    /// A chip at power-on: cassette selected, motor off, 19200 baud.
    #[must_use]
    pub const fn new() -> Self {
        Self { control: 0 }
    }

    /// The CPU writes the control register.
    pub fn write(&mut self, value: u8) {
        self.control = value;
    }

    /// The control register.
    #[must_use]
    pub const fn control(self) -> u8 {
        self.control
    }

    /// The clock given to the 6850's transmitter, in Hz.
    #[must_use]
    pub fn transmit_clock(self) -> u64 {
        64 * BAUD[usize::from(self.control & RATE)]
    }

    /// The clock given to the 6850's receiver, in Hz: for RS423 the rate
    /// bits 3–5 select; for the cassette the data separator's, whatever they
    /// say, as the OS leaves them while it loads.
    #[must_use]
    pub fn receive_clock(self) -> u64 {
        if self.rs423() {
            64 * BAUD[usize::from((self.control >> RECEIVE_SHIFT) & RATE)]
        } else {
            CASSETTE_RECEIVE_CLOCK
        }
    }

    /// Whether RS423, rather than the cassette, has the 6850.
    #[must_use]
    pub const fn rs423(self) -> bool {
        self.control & RS423 != 0
    }

    /// Whether the cassette motor relay is closed.
    #[must_use]
    pub const fn motor(self) -> bool {
        self.control & MOTOR != 0
    }
}

crate::snapshot::fields!(SerialUla { control });

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_three_bit_field_picks_its_direction_s_rs423_rate() {
        let mut ula = SerialUla::new();
        ula.write(RS423);
        assert_eq!(
            (ula.transmit_clock(), ula.receive_clock()),
            (64 * 19_200, 64 * 19_200)
        );
        // Transmit 300 (5), receive 1200 (1).
        ula.write(RS423 | 0x0D);
        assert_eq!(
            (ula.transmit_clock(), ula.receive_clock()),
            (64 * 300, 64 * 1_200)
        );
        ula.write(RS423 | 0x3F);
        assert_eq!(
            (ula.transmit_clock(), ula.receive_clock()),
            (64 * 75, 64 * 75)
        );
    }

    #[test]
    fn a_playing_cassette_clocks_the_receiver_at_300_baud_s_rate_whatever_the_bits() {
        let mut ula = SerialUla::new();
        // The OS loads with &85: motor, transmit 300, receive bits 000.
        ula.write(0x85);
        assert_eq!(ula.receive_clock(), 64 * 300);
        assert_eq!(ula.transmit_clock(), 64 * 300);
    }

    #[test]
    fn bit_6_hands_the_6850_to_rs423_and_bit_7_closes_the_motor_relay() {
        let mut ula = SerialUla::new();
        assert!(!ula.rs423() && !ula.motor());
        ula.write(0x40);
        assert!(ula.rs423() && !ula.motor());
        ula.write(0x80);
        assert!(!ula.rs423() && ula.motor());
        assert_eq!(ula.control(), 0x80);
    }
}
