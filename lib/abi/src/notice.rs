//! System notices: the one state-edge broadcast a process converges on
//! (`plans/NOTICE.md`).
//!
//! A notice is a **state edge**, not an occurrence. Each [`NoticeTopic`] names
//! one machine-wide value; a subscriber learns only that the value moved and
//! then reads the current one, so there is no queue, no history, nothing to
//! overflow, and nothing to drop. That is what makes it the right shape for
//! facts a process must *agree* with rather than witness: the desktop's
//! appearance and density, the mount table's composition, the memory-pressure
//! band, the boot seat's display lease, the display's output, whether the
//! machine is recording, which process is the cue authority.
//!
//! The mechanism is three parts:
//!
//! * [`WaitSourceKind::SystemNotice`](crate::WaitSourceKind::SystemNotice) —
//!   the wait-set member whose `id` is the topic, ready when the topic's
//!   generation differs from the one the member last observed.
//! * [`SyscallNumber::NOTICE_READ`](crate::SyscallNumber::NOTICE_READ) — the
//!   unprivileged, non-blocking read of a topic's current payload, so a woken
//!   subscriber has the value without an IPC round trip.
//! * [`SyscallNumber::NOTICE_PUBLISH`](crate::SyscallNumber::NOTICE_PUBLISH) —
//!   the publish, authorised per topic and fail-closed.
//!
//! # Payload shapes are exact, not merely bounded
//!
//! Every topic carries a payload of one fixed length ([`NoticeTopic::payload_len`]),
//! so a publish of any other length is refused rather than stored and handed
//! to a subscriber that would then have to defend against it. [`Notice`] is
//! the one encode/decode for all of them: the publisher writes through it and
//! the subscriber reads through it, so neither can spell a payload the other
//! would not accept.

use crate::desktop::DesktopInfo;
use crate::le::{read_u32, read_u64};
use crate::origin::{ProcId, PROC_ID_LEN};
use crate::seat::DisplayLease;
use crate::Errno;

/// Largest payload any topic carries — the buffer a subscriber sizes and the
/// kernel retains per topic.
///
/// A containment bound, not a capacity: it is what stops a topic's payload
/// growing into a channel. It is the widest topic's own record, derived from
/// the closed set so no topic can outgrow it; a topic needing more than a
/// small fixed record is carrying a document, not a state edge, and belongs on
/// an IPC endpoint.
pub const NOTICE_PAYLOAD_MAX: usize = {
    let mut widest = 0;
    let mut index = 0;
    while index < NoticeTopic::ALL.len() {
        let len = NoticeTopic::ALL[index].payload_len();
        if len > widest {
            widest = len;
        }
        index += 1;
    }
    widest
};

/// One machine-wide value a process may converge on.
///
/// The set is closed and deliberately small: each topic widens what every
/// subscriber, reviewer, and audit must reason about, and a topic exists only
/// where a *state* has to be agreed rather than an event delivered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum NoticeTopic {
    /// The desktop the foreground session composites: its screen extent, UI
    /// density, and appearance, as a [`DesktopInfo`]. Published by the holder
    /// of a seat's live display lease — the one principal that owns what is on
    /// screen — and read by every windowed application.
    Desktop = 0,
    /// What the mount table reports changed: a mount point was attached,
    /// re-backed, or removed, or the volume behind one came online — an
    /// unlocked root — went, or became unavailable. Payload-less — the
    /// generation *is* the news, and what is mounted is then read through the
    /// System Information API under the caller's own authority. Published by
    /// the kernel alone.
    Mounts = 1,
    /// The machine's memory-pressure band, as its depth. Published by the
    /// kernel alone, from the gauge whatever was spending memory refreshed.
    ///
    /// The band is a five-level, hysteresis-damped, machine-wide indicator and
    /// carries no per-process, per-user, or byte-level figure, so reading it
    /// needs no capability — exactly as reading the load average does.
    MemoryPressure = 2,
    /// The boot seat's display lease, as a [`DisplayLease`]: moved by every
    /// acquire, release, revocation, and dead owner's reclaim, and by the
    /// text console taking back a handover. Published by the kernel alone.
    ///
    /// Read by the services that follow the seat: the display service, which
    /// must never keep a configuration — or a switched-off screen — past the
    /// lease that asked for it, the audio service, which mixes only the
    /// holder's session into the seat's speakers, and the cue authority,
    /// which applies only the holder's sound choices. Nothing else may observe
    /// it, because the lease's history is the seat inventory's, which is
    /// privileged.
    DisplayLease = 3,
    /// How many capture streams are moving frames on this machine, as a
    /// `u32`: the recording indicator's whole input. Published by the audio
    /// service — the process the kernel attests bound the reserved audio
    /// rendezvous — so no program can hide its own recording from it. Readable
    /// by any process, as whether a sound device is in use is on every system.
    AudioCapture = 4,
    /// The sound devices moved: one was bound or lost, or a default, a level
    /// or a mute changed. The payload is the audio service's own count of such
    /// changes, as a `u64`, so every change is a new value; what moved is then
    /// read from the service itself. Published by the audio service alone,
    /// the process the kernel attests bound the reserved audio rendezvous,
    /// and readable by any process, as the devices themselves are.
    AudioDevices = 5,
    /// The machine's network interfaces moved: one appeared or went, its link
    /// changed, or its addresses or an address's state did. The payload is the
    /// network stack's own count of such changes, as a `u64`. Published by the
    /// network stack alone, the process the kernel attests bound the reserved
    /// stack rendezvous, and readable by any process: it says only that
    /// something moved, and what moved is read under the reader's own
    /// authority.
    Network = 6,
    /// The display's output moved: a sink arrived or left, or the output
    /// runs another mode. The payload is the display service's own count of
    /// such moves, as a `u64`. Published by the display service alone, the
    /// process the kernel attests bound the reserved display rendezvous, and
    /// readable by any process: it says only that the output moved, and what
    /// it runs now is read under the lease-gated display query.
    DisplayOutput = 7,
    /// The cue authority's process instance, as a [`ProcId`]: the one a
    /// session hands its chosen sound files to, and, as a new value, the news
    /// that a restarted authority holds no session's choices. Published by the
    /// process the kernel attests bound the reserved cue rendezvous, naming
    /// itself and nothing else, and readable by any process.
    CueAuthority = 8,
}

impl NoticeTopic {
    /// The wire value for this topic.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Recover a topic from its wire value.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `value` is not a known topic (fail closed on a
    /// malformed argument).
    pub const fn from_u32(value: u32) -> Result<Self, Errno> {
        match value {
            0 => Ok(Self::Desktop),
            1 => Ok(Self::Mounts),
            2 => Ok(Self::MemoryPressure),
            3 => Ok(Self::DisplayLease),
            4 => Ok(Self::AudioCapture),
            5 => Ok(Self::AudioDevices),
            6 => Ok(Self::Network),
            7 => Ok(Self::DisplayOutput),
            8 => Ok(Self::CueAuthority),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// Recover a topic from the `id` a wait-set member or syscall argument
    /// carries.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a value outside the topic set, including one
    /// whose high bits are set — a wide argument is refused, never truncated
    /// into a topic it is not.
    pub const fn from_u64(value: u64) -> Result<Self, Errno> {
        if value > u32::MAX as u64 {
            return Err(Errno::OutOfRange);
        }
        #[allow(clippy::cast_possible_truncation)] // Bounded above.
        Self::from_u32(value as u32)
    }

    /// The exact payload length this topic carries; never above
    /// [`NOTICE_PAYLOAD_MAX`].
    #[must_use]
    pub const fn payload_len(self) -> usize {
        match self {
            Self::Desktop => DesktopInfo::WIRE_LEN,
            Self::Mounts => 0,
            Self::MemoryPressure => 1,
            Self::DisplayLease => DisplayLease::WIRE_LEN,
            Self::AudioCapture => 4,
            Self::AudioDevices | Self::Network | Self::DisplayOutput => 8,
            Self::CueAuthority => PROC_ID_LEN,
        }
    }

    /// Every topic, for a caller that must cover them all.
    pub const ALL: [Self; 9] = [
        Self::Desktop,
        Self::Mounts,
        Self::MemoryPressure,
        Self::DisplayLease,
        Self::AudioCapture,
        Self::AudioDevices,
        Self::Network,
        Self::DisplayOutput,
        Self::CueAuthority,
    ];
}

/// A topic's payload, decoded.
///
/// One definition for both directions, so a publisher cannot write a shape a
/// subscriber would refuse.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Notice {
    /// [`NoticeTopic::Desktop`]: the desktop as the session composites it.
    Desktop(DesktopInfo),
    /// [`NoticeTopic::Mounts`]: the mount table moved; read it to learn how.
    Mounts,
    /// [`NoticeTopic::MemoryPressure`]: the band's depth.
    MemoryPressure {
        /// The band depth, as `tairix_reclaim::PressureBand::depth` spells it.
        /// Validated by that type, not here — this layer carries the scalar.
        band: u8,
    },
    /// [`NoticeTopic::DisplayLease`]: the boot seat's display lease.
    DisplayLease(DisplayLease),
    /// [`NoticeTopic::AudioCapture`]: the capture streams moving frames.
    AudioCapture {
        /// How many there are; none means nothing is recording.
        live: u32,
    },
    /// [`NoticeTopic::AudioDevices`]: the sound devices moved; read them to
    /// learn how.
    AudioDevices {
        /// The audio service's count of changes so far.
        changes: u64,
    },
    /// [`NoticeTopic::Network`]: the interfaces moved; read them to learn
    /// how.
    Network {
        /// The network stack's count of changes so far.
        changes: u64,
    },
    /// [`NoticeTopic::DisplayOutput`]: the display's output moved; query it
    /// to learn what it runs now.
    DisplayOutput {
        /// The display service's count of output moves so far.
        changes: u64,
    },
    /// [`NoticeTopic::CueAuthority`]: the cue authority's instance.
    CueAuthority {
        /// The process instance serving the cue rendezvous.
        instance: ProcId,
    },
}

impl Notice {
    /// The topic this payload belongs to.
    #[must_use]
    pub const fn topic(&self) -> NoticeTopic {
        match self {
            Self::Desktop(_) => NoticeTopic::Desktop,
            Self::Mounts => NoticeTopic::Mounts,
            Self::MemoryPressure { .. } => NoticeTopic::MemoryPressure,
            Self::DisplayLease(_) => NoticeTopic::DisplayLease,
            Self::AudioCapture { .. } => NoticeTopic::AudioCapture,
            Self::AudioDevices { .. } => NoticeTopic::AudioDevices,
            Self::Network { .. } => NoticeTopic::Network,
            Self::DisplayOutput { .. } => NoticeTopic::DisplayOutput,
            Self::CueAuthority { .. } => NoticeTopic::CueAuthority,
        }
    }

    /// A counted `topic`'s payload carrying `changes`, or `None` for a topic
    /// whose payload is not a count.
    #[must_use]
    pub const fn counted(topic: NoticeTopic, changes: u64) -> Option<Self> {
        match topic {
            NoticeTopic::AudioDevices => Some(Self::AudioDevices { changes }),
            NoticeTopic::Network => Some(Self::Network { changes }),
            NoticeTopic::DisplayOutput => Some(Self::DisplayOutput { changes }),
            NoticeTopic::Desktop
            | NoticeTopic::Mounts
            | NoticeTopic::MemoryPressure
            | NoticeTopic::DisplayLease
            | NoticeTopic::AudioCapture
            | NoticeTopic::CueAuthority => None,
        }
    }

    /// The count a counted payload carries.
    #[must_use]
    pub const fn changes(&self) -> Option<u64> {
        match self {
            Self::AudioDevices { changes }
            | Self::Network { changes }
            | Self::DisplayOutput { changes } => Some(*changes),
            Self::Desktop(_)
            | Self::Mounts
            | Self::MemoryPressure { .. }
            | Self::DisplayLease(_)
            | Self::AudioCapture { .. }
            | Self::CueAuthority { .. } => None,
        }
    }

    /// Decode the payload held for `topic`.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] if `bytes` is not exactly the topic's
    /// [`payload_len`](NoticeTopic::payload_len), and whatever the payload's
    /// own decode refuses for a malformed body. A payload is never partially
    /// adopted.
    pub fn decode(topic: NoticeTopic, bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() != topic.payload_len() {
            return Err(Errno::LengthOutOfRange);
        }
        match topic {
            NoticeTopic::Desktop => DesktopInfo::from_bytes(bytes).map(Self::Desktop),
            NoticeTopic::Mounts => Ok(Self::Mounts),
            NoticeTopic::MemoryPressure => Ok(Self::MemoryPressure { band: bytes[0] }),
            NoticeTopic::DisplayLease => DisplayLease::from_bytes(bytes).map(Self::DisplayLease),
            NoticeTopic::AudioCapture => Ok(Self::AudioCapture {
                live: read_u32(bytes, 0),
            }),
            NoticeTopic::AudioDevices => Ok(Self::AudioDevices {
                changes: read_u64(bytes, 0),
            }),
            NoticeTopic::Network => Ok(Self::Network {
                changes: read_u64(bytes, 0),
            }),
            NoticeTopic::DisplayOutput => Ok(Self::DisplayOutput {
                changes: read_u64(bytes, 0),
            }),
            // No process is the kernel's sentinel, so no publish names it.
            NoticeTopic::CueAuthority => match ProcId::from_bytes(bytes)? {
                instance if instance.is_kernel() => Err(Errno::BadMagic),
                instance => Ok(Self::CueAuthority { instance }),
            },
        }
    }

    /// Write this payload to `out`, answering how many bytes it took.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] if `out` is shorter than the topic's
    /// [`payload_len`](NoticeTopic::payload_len), and [`Errno::BadMagic`] for
    /// a payload [`Self::decode`] would refuse: a cue authority naming no
    /// process.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let len = self.topic().payload_len();
        let Some(slot) = out.get_mut(..len) else {
            return Err(Errno::LengthOutOfRange);
        };
        match self {
            Self::Desktop(info) => slot.copy_from_slice(&info.to_le_bytes()),
            Self::Mounts => {}
            Self::MemoryPressure { band } => slot[0] = *band,
            Self::DisplayLease(lease) => slot.copy_from_slice(&lease.to_le_bytes()),
            Self::AudioCapture { live } => slot.copy_from_slice(&live.to_le_bytes()),
            Self::AudioDevices { changes }
            | Self::Network { changes }
            | Self::DisplayOutput { changes } => {
                slot.copy_from_slice(&changes.to_le_bytes());
            }
            Self::CueAuthority { instance } if instance.is_kernel() => {
                return Err(Errno::BadMagic);
            }
            Self::CueAuthority { instance } => slot.copy_from_slice(instance.as_bytes()),
        }
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::{Notice, NoticeTopic, NOTICE_PAYLOAD_MAX};
    use crate::desktop::{Appearance, DesktopInfo};
    use crate::seat::DisplayLease;
    use crate::{Errno, ProcId};

    fn desktop() -> DesktopInfo {
        match DesktopInfo::new(1920, 1080, 150, Appearance::Light) {
            Ok(info) => info,
            Err(_) => unreachable!("a valid desktop"),
        }
    }

    #[test]
    fn topic_wire_values_are_frozen_and_unknown_is_refused() {
        assert_eq!(NoticeTopic::Desktop.as_u32(), 0);
        assert_eq!(NoticeTopic::Mounts.as_u32(), 1);
        assert_eq!(NoticeTopic::MemoryPressure.as_u32(), 2);
        assert_eq!(NoticeTopic::DisplayLease.as_u32(), 3);
        assert_eq!(NoticeTopic::AudioCapture.as_u32(), 4);
        assert_eq!(NoticeTopic::AudioDevices.as_u32(), 5);
        assert_eq!(NoticeTopic::Network.as_u32(), 6);
        assert_eq!(NoticeTopic::DisplayOutput.as_u32(), 7);
        assert_eq!(NoticeTopic::CueAuthority.as_u32(), 8);
        for topic in NoticeTopic::ALL {
            assert_eq!(NoticeTopic::from_u32(topic.as_u32()), Ok(topic));
            assert_eq!(NoticeTopic::from_u64(u64::from(topic.as_u32())), Ok(topic));
        }
        assert_eq!(NoticeTopic::from_u32(9), Err(Errno::OutOfRange));
        assert_eq!(NoticeTopic::from_u32(u32::MAX), Err(Errno::OutOfRange));
    }

    /// A wide `id` must not be truncated into a topic it is not: the low
    /// bits of `1 << 32` are `Desktop`'s wire value.
    #[test]
    fn a_wide_id_is_refused_not_truncated() {
        assert_eq!(NoticeTopic::from_u64(1 << 32), Err(Errno::OutOfRange));
        assert_eq!(NoticeTopic::from_u64(u64::MAX), Err(Errno::OutOfRange));
    }

    #[test]
    fn every_payload_fits_the_ceiling() {
        for topic in NoticeTopic::ALL {
            assert!(topic.payload_len() <= NOTICE_PAYLOAD_MAX, "{topic:?}");
        }
    }

    #[test]
    fn each_topic_round_trips_through_its_exact_length() {
        for notice in [
            Notice::Desktop(desktop()),
            Notice::Mounts,
            Notice::MemoryPressure { band: 3 },
            Notice::DisplayLease(DisplayLease::held(4, ProcId::from_raw([0x51; 16]))),
            Notice::AudioCapture { live: 2 },
            Notice::AudioDevices { changes: 9 },
            Notice::Network { changes: 11 },
            Notice::DisplayOutput { changes: 13 },
            Notice::CueAuthority {
                instance: ProcId::from_raw([0x5d; 16]),
            },
        ] {
            let mut buf = [0xAAu8; NOTICE_PAYLOAD_MAX];
            let topic = notice.topic();
            let len = notice.encode(&mut buf).expect("encodes");
            assert_eq!(len, topic.payload_len());
            assert_eq!(Notice::decode(topic, &buf[..len]), Ok(notice));
            // Nothing beyond the payload is touched.
            assert!(buf[len..].iter().all(|&b| b == 0xAA), "{topic:?}");
        }
    }

    #[test]
    fn only_the_counted_topics_carry_a_count() {
        for topic in NoticeTopic::ALL {
            let counted = Notice::counted(topic, 7);
            let is_counted = matches!(
                topic,
                NoticeTopic::AudioDevices | NoticeTopic::Network | NoticeTopic::DisplayOutput
            );
            assert_eq!(counted.is_some(), is_counted, "{topic:?}");
            if let Some(notice) = counted {
                assert_eq!(notice.topic(), topic);
                assert_eq!(notice.changes(), Some(7));
            }
        }
        assert_eq!(Notice::Mounts.changes(), None);
        assert_eq!(Notice::AudioCapture { live: 2 }.changes(), None);
    }

    #[test]
    fn a_payload_of_the_wrong_length_is_refused() {
        let good = Notice::Desktop(desktop());
        let mut buf = [0u8; NOTICE_PAYLOAD_MAX];
        let len = good.encode(&mut buf).expect("encodes");
        assert_eq!(
            Notice::decode(NoticeTopic::Desktop, &buf[..len - 1]),
            Err(Errno::LengthOutOfRange)
        );
        // A payload longer than the topic's is refused, not read as a
        // prefix.
        let over = [0u8; DesktopInfo::WIRE_LEN + 1];
        assert_eq!(
            Notice::decode(NoticeTopic::Desktop, &over),
            Err(Errno::LengthOutOfRange)
        );
        assert_eq!(
            Notice::decode(NoticeTopic::Mounts, &buf[..1]),
            Err(Errno::LengthOutOfRange)
        );
        assert_eq!(
            Notice::decode(NoticeTopic::MemoryPressure, &[]),
            Err(Errno::LengthOutOfRange)
        );
        assert_eq!(
            Notice::decode(
                NoticeTopic::DisplayLease,
                &buf[..DisplayLease::WIRE_LEN - 1]
            ),
            Err(Errno::LengthOutOfRange)
        );
    }

    #[test]
    fn encoding_into_a_short_buffer_is_refused() {
        let mut buf = [0u8; DesktopInfo::WIRE_LEN - 1];
        assert_eq!(
            Notice::Desktop(desktop()).encode(&mut buf),
            Err(Errno::LengthOutOfRange)
        );
        assert_eq!(Notice::Mounts.encode(&mut []), Ok(0));
    }

    /// A malformed desktop body is refused rather than adopted: the payload
    /// crosses a trust boundary like any other.
    #[test]
    fn a_malformed_desktop_payload_is_refused() {
        let zeroed = [0u8; DesktopInfo::WIRE_LEN];
        assert!(Notice::decode(NoticeTopic::Desktop, &zeroed).is_err());
    }

    #[test]
    fn a_cue_authority_naming_no_process_is_refused() {
        assert_eq!(
            Notice::decode(NoticeTopic::CueAuthority, ProcId::KERNEL.as_bytes()),
            Err(Errno::BadMagic)
        );
        let mut out = [0u8; NOTICE_PAYLOAD_MAX];
        assert_eq!(
            Notice::CueAuthority {
                instance: ProcId::KERNEL
            }
            .encode(&mut out),
            Err(Errno::BadMagic),
            "nor written"
        );
        assert_eq!(
            Notice::decode(NoticeTopic::CueAuthority, &[7u8; 15]),
            Err(Errno::LengthOutOfRange)
        );
    }
}
