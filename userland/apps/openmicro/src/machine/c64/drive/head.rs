//! The 1541's head and the electronics behind it: the stepper, the disk
//! turning under the head at 300 rpm, and the read and write clock, timed
//! from the drive's 16 MHz crystal.
//!
//! Reading, a counter (UE7) divides the crystal by 16 less the zone the
//! drive selects, a second (UF4) counts its carries, and every fourth carry
//! clocks a bit into the shift register: a one at the first after a flux
//! reversal, which restarts both counters, zeros after. Ten ones in a row
//! are SYNC, and the eighth bit after it completes a byte, which BYTE READY
//! announces. With no reversal for long the electronics read noise.
//! Writing, the same clock shifts the drive's byte out onto the disk.

use crate::media::c1541::{Surface, HALF_TRACKS, MAX_TRACK_BYTES};
use crate::snapshot;

/// Crystal cycles a drive cycle.
pub const CRYSTAL_PER_CYCLE: u64 = 16;
/// Crystal cycles a revolution at 300 rpm.
pub const REVOLUTION: u64 = 16_000_000 / 5;
/// UF4 counts a cell takes.
const COUNTS_PER_CELL: u8 = 4;

/// Crystal cycles a UF4 count takes at `zone`, whose two bits are all the
/// electronics see: UE7 divides the crystal by 16 less the zone.
const fn carry_period(zone: u8) -> u64 {
    16 - (zone & 3) as u64
}

/// Crystal cycles a cell takes at `zone`.
#[must_use]
pub const fn cell_cycles(zone: u8) -> u64 {
    COUNTS_PER_CELL as u64 * carry_period(zone)
}

/// A reversal is accepted this long after its cell begins.
const FILTER_DELAY: u64 = 1;
/// BYTE READY follows a byte's last bit by at least this many crystal
/// cycles, on the next drive cycle's edge.
const BYTE_READY_DELAY: u64 = 10;
/// The half-track the head rests on at power-on: track 18's.
const HOME: u8 = 34;

/// How the drive's VIA2 sets the electronics up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    /// The spindle motor runs.
    pub motor: bool,
    /// The stepper's phase, 0 to 3.
    pub phase: u8,
    /// The zone the clock divides for, 0 to 3.
    pub zone: u8,
    /// Reading rather than writing.
    pub reading: bool,
    /// BYTE READY reaches SO and VIA2's CA1.
    pub byte_ready: bool,
    /// The byte the drive writes next.
    pub write_value: u8,
}

/// The next flux reversal under the head, once looked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reversal {
    /// Not looked for since the head or its settings last changed.
    Unknown,
    /// None on this track.
    Never,
    /// At this crystal cycle.
    At(u64),
}

impl Reversal {
    const fn at(self) -> Option<u64> {
        match self {
            Self::At(at) => Some(at),
            Self::Unknown | Self::Never => None,
        }
    }
}

impl snapshot::Save for Reversal {
    fn save(&self, w: &mut snapshot::Writer) {
        let found: Option<Option<u64>> = match *self {
            Self::Unknown => None,
            Self::Never => Some(None),
            Self::At(at) => Some(Some(at)),
        };
        w.put(&found);
    }
}

impl snapshot::Load for Reversal {
    fn load(r: &mut snapshot::Reader<'_>) -> Result<Self, snapshot::SnapshotError> {
        Ok(match r.get::<Option<Option<u64>>>()? {
            None => Self::Unknown,
            Some(None) => Self::Never,
            Some(Some(at)) => Self::At(at),
        })
    }
}

/// The head and its electronics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    half_track: u8,
    /// Crystal cycles elapsed.
    now: u64,
    /// The disk's angle under the head, in crystal cycles of a revolution.
    angle: u64,
    controls: Controls,
    /// UF4, and the crystal cycle it counts next.
    uf4: u8,
    next_carry: u64,
    next_reversal: Reversal,
    /// When noise stands in for a reversal, and the noise generator.
    noise_at: u64,
    noise: u32,
    shift: u16,
    bit_count: u8,
    /// The byte the shift register last completed, on VIA2's port A.
    latched: u8,
    write_shift: u8,
    /// Where on the track the next bit is written: from the head's place
    /// when writing begins, a bit on for each bit written, so a block keeps
    /// every bit whatever the ring's length.
    write_at: Option<usize>,
    /// The drive cycle BYTE READY pulses in next.
    byte_ready_at: Option<u64>,
}

snapshot::fields!(
    Controls {
        motor,
        phase,
        zone,
        reading,
        byte_ready,
        write_value
    },
    check | c | c.phase < 4 && c.zone < 4
);

snapshot::fields!(
    Head {
        half_track,
        now,
        angle,
        controls,
        uf4,
        next_carry,
        next_reversal,
        noise_at,
        noise,
        shift,
        bit_count,
        latched,
        write_shift,
        write_at,
        byte_ready_at
    },
    check | h | usize::from(h.half_track) < HALF_TRACKS
        && h.angle < REVOLUTION
        && h.uf4 < 16
        && h.next_carry > h.now
        && h.next_reversal.at().is_none_or(|at| at > h.now)
        && h.noise_at > h.now
        && h.noise != 0
        && h.write_at.is_none_or(|at| at < MAX_TRACK_BYTES * 8)
        && h.shift < 0x400
        && h.bit_count < 8
);

impl Default for Head {
    fn default() -> Self {
        Self::new()
    }
}

impl Head {
    /// The head at power-on, resting on track 18, the motor off.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            half_track: HOME,
            now: 0,
            angle: 0,
            controls: Controls {
                motor: false,
                phase: HOME & 3,
                zone: 0,
                reading: true,
                byte_ready: false,
                write_value: 0,
            },
            uf4: 0,
            next_carry: 16,
            next_reversal: Reversal::Unknown,
            noise_at: u64::MAX,
            noise: 0x2468_ACE1,
            shift: 0,
            bit_count: 0,
            latched: 0,
            write_shift: 0,
            write_at: None,
            byte_ready_at: None,
        }
    }

    /// The crystal cycle it has been brought up to.
    #[must_use]
    pub const fn now(&self) -> u64 {
        self.now
    }

    /// The half-track under the head.
    #[must_use]
    pub const fn half_track(&self) -> u8 {
        self.half_track
    }

    /// The byte on VIA2's port A.
    #[must_use]
    pub const fn latched(&self) -> u8 {
        self.latched
    }

    /// Whether SYNC is seen: reading, ten ones in the shift register.
    #[must_use]
    pub const fn sync(&self) -> bool {
        self.controls.reading && self.shift == 0x3FF
    }

    /// Whether BYTE READY pulses in drive cycle `cycle`.
    #[must_use]
    pub fn byte_ready(&self, cycle: u64) -> bool {
        self.byte_ready_at == Some(cycle)
    }

    /// Take VIA2's new settings: a phase one step round moves the head a
    /// half-track while the motor runs, two steps round pulls both ways and
    /// moves it not at all. A write goes on from the last bit written until
    /// the head, its mode, the motor or the zone changes: the drive hands
    /// the head its settings at every write to VIA2, the next byte's
    /// included.
    pub fn set_controls(&mut self, controls: Controls) {
        let from = self.half_track;
        if controls.motor {
            match (controls.phase.wrapping_sub(self.half_track & 3)) & 3 {
                1 => {
                    self.half_track =
                        (self.half_track + 1).min(u8::try_from(HALF_TRACKS - 1).unwrap_or(u8::MAX));
                }
                3 => self.half_track = self.half_track.saturating_sub(1),
                _ => {}
            }
        }
        let mode_changed =
            controls.reading != self.controls.reading || controls.motor != self.controls.motor;
        if mode_changed {
            // Noise waits on the next real reversal, as at power-on.
            self.bit_count = 0;
            self.noise_at = u64::MAX;
        }
        let stepped = self.half_track != from;
        if mode_changed || stepped || controls.zone != self.controls.zone {
            self.write_at = None;
        }
        if mode_changed || stepped {
            self.next_reversal = Reversal::Unknown;
        }
        self.controls = controls;
    }

    /// A disk put in or taken out: nothing known of its recording, and no
    /// write going on.
    pub fn disk_changed(&mut self) {
        self.next_reversal = Reversal::Unknown;
        self.write_at = None;
    }

    fn random(&mut self) -> u32 {
        self.noise ^= self.noise << 13;
        self.noise ^= self.noise >> 17;
        self.noise ^= self.noise << 5;
        self.noise
    }

    /// The crystal cycle of the next flux reversal on the track under the
    /// head, if the track records any: the current cell's while it is still
    /// ahead of the head.
    fn find_reversal(&self, disk: Option<&Surface>) -> Option<u64> {
        let track = disk?.track(usize::from(self.half_track))?;
        let len = u64::try_from(track.len()).ok()?;
        let reversal_at = |cell: u64| cell * REVOLUTION / len + FILTER_DELAY;
        let cell = self.angle * len / REVOLUTION;
        let from = if reversal_at(cell) > self.angle {
            cell
        } else {
            cell + 1
        };
        let ahead = track.cells_to_set_bit(usize::try_from(from % len).ok()?)?;
        let index = from + u64::try_from(ahead).ok()?;
        Some(self.now + (reversal_at(index) - self.angle))
    }

    /// Run the electronics up to the start of drive cycle `cycle`.
    pub fn advance(&mut self, cycle: u64, disk: &mut Option<Surface>) {
        let until = cycle * CRYSTAL_PER_CYCLE;
        if !self.controls.motor {
            self.now = until.max(self.now);
            self.next_carry = self.next_carry.max(self.now + 1);
            return;
        }
        while self.now < until {
            let reversal = if self.controls.reading {
                if self.next_reversal == Reversal::Unknown {
                    self.next_reversal = self
                        .find_reversal(disk.as_ref())
                        .map_or(Reversal::Never, Reversal::At);
                }
                self.next_reversal
                    .at()
                    .unwrap_or(u64::MAX)
                    .min(self.noise_at)
            } else {
                u64::MAX
            };
            // A deadline left behind falls due at once.
            let next = reversal.min(self.next_carry).min(until).max(self.now);
            self.angle = (self.angle + (next - self.now)) % REVOLUTION;
            self.now = next;
            if next >= reversal {
                self.reverse();
            } else if next >= self.next_carry {
                self.carry(disk);
            }
        }
    }

    /// A flux reversal, real or noise: both counters restart.
    fn reverse(&mut self) {
        let real = self.next_reversal.at().is_some_and(|at| at <= self.now);
        if real {
            self.next_reversal = Reversal::Unknown;
        }
        let wait = if real {
            289 + u64::from(self.random() % 31)
        } else {
            33 + u64::from(self.random() % 367)
        };
        self.noise_at = self.now + wait;
        self.uf4 = 0;
        self.next_carry = self.now + carry_period(self.controls.zone);
    }

    /// UF4 counts; at every fourth count a bit is read or written.
    fn carry(&mut self, disk: &mut Option<Surface>) {
        self.uf4 = (self.uf4 + 1) & 0x0F;
        self.next_carry = self.now + carry_period(self.controls.zone);
        if self.uf4 % COUNTS_PER_CELL != 2 {
            return;
        }
        if self.controls.reading {
            self.read_bit(self.uf4 == 2);
        } else {
            self.write_bit(disk);
        }
    }

    fn read_bit(&mut self, bit: bool) {
        self.shift = ((self.shift << 1) | u16::from(bit)) & 0x3FF;
        if self.shift == 0x3FF {
            self.bit_count = 0;
            return;
        }
        self.bit_count += 1;
        if self.bit_count == 8 {
            self.bit_count = 0;
            self.latched = self.shift.to_le_bytes()[0];
            self.write_shift = self.latched;
            self.announce_byte();
        }
    }

    fn write_bit(&mut self, disk: &mut Option<Surface>) {
        let bit = self.write_shift & 0x80 != 0;
        self.write_shift <<= 1;
        if let Some(surface) = disk.as_mut().filter(|surface| !surface.write_protected) {
            if let Some(track) =
                surface.track_to_write(usize::from(self.half_track), self.controls.zone)
            {
                let len = track.len();
                let angle = self.angle;
                let index = *self.write_at.get_or_insert_with(|| {
                    let at = angle * u64::try_from(len).unwrap_or(1) / REVOLUTION;
                    usize::try_from(at).unwrap_or(0)
                });
                track.set_bit(index, bit);
                self.write_at = Some((index % len + 1) % len);
            }
        }
        self.bit_count += 1;
        if self.bit_count == 8 {
            self.bit_count = 0;
            self.write_shift = self.controls.write_value;
            self.announce_byte();
        }
    }

    /// BYTE READY, if enabled, on the first drive cycle edge far enough on.
    fn announce_byte(&mut self) {
        if self.controls.byte_ready {
            let at = self.now + BYTE_READY_DELAY;
            self.byte_ready_at = Some(at.div_ceil(CRYSTAL_PER_CYCLE));
        }
    }
}

#[cfg(test)]
#[path = "head_tests.rs"]
mod tests;
