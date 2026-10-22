//! What a block device is (`docs/src/drivers/block.md`): the bus it is
//! reached over and, as separate facts, whether its medium rotates, whether
//! the medium can leave the unit, each with the evidence that established it,
//! and what form the medium takes where the unit names one.
//!
//! A fact nothing established is `Unknown`, and each consumer reads it in its
//! own conservative direction: a time budget as the slowest medium
//! ([`BlkMedium::class`]), a write-back window by what a command costs
//! ([`BlkMedium::command_cost`]), a picture or a description as no particular
//! medium. Nothing downstream is told more than the hardware reported.

use core::num::NonZeroU16;

use super::BlkDeviceClass;
use crate::le::{put_u16, read_u16};

/// Encoded length of a [`BlkMedium`] record ([`BlkMedium::encode`]).
pub const BLK_MEDIUM_LEN: usize = 8;

/// The transport byte of a record for a device whose driver declared nothing.
pub const BLK_TRANSPORT_UNDECLARED: u8 = 0xFF;
/// Kind byte: nothing established the fact. Its evidence byte is
/// [`BLK_EVIDENCE_NONE`].
pub const BLK_FACT_UNKNOWN: u8 = 0;
/// Rotation kind byte: the medium does not rotate.
pub const BLK_ROTATION_NON_ROTATING: u8 = 1;
/// Rotation kind byte: the medium rotates, at the record's nominal rate.
pub const BLK_ROTATION_ROTATING: u8 = 2;
/// Removability kind byte: the medium cannot leave the unit.
pub const BLK_REMOVABILITY_FIXED: u8 = 1;
/// Removability kind byte: the medium can leave the unit.
pub const BLK_REMOVABILITY_REMOVABLE: u8 = 2;
/// Evidence byte of a fact nothing established.
pub const BLK_EVIDENCE_NONE: u8 = 0;

const TRANSPORT_OFF: usize = 0;
const ROTATION_OFF: usize = 1;
const ROTATION_EVIDENCE_OFF: usize = 2;
const REMOVABILITY_OFF: usize = 3;
const REMOVABILITY_EVIDENCE_OFF: usize = 4;
const FORM_OFF: usize = 5;
const RPM_OFF: usize = 6;

/// The lowest and highest nominal rates the shared MEDIUM ROTATION RATE
/// encoding can state (SBC-4 §6.6.4, ACS-4 §9.11.7.4).
const NOMINAL_RPM: core::ops::RangeInclusive<u16> = 0x0401..=0xFFFE;
/// The rate field's "non-rotating medium" value.
const NON_ROTATING_FIELD: u16 = 0x0001;

/// The bus a block device is reached over: the one fact its driver always
/// knows for certain.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BlkTransport {
    /// An ATA disk on a SATA or PATA host controller.
    Ata = 0,
    /// An `NVMe` namespace.
    Nvme = 1,
    /// A USB mass-storage unit: a stick, a card reader, a disk in an
    /// enclosure.
    Usb = 2,
    /// An SD or MMC card.
    Sd = 3,
    /// A paravirtual or software-backed device.
    Virtual = 4,
}

impl BlkTransport {
    /// Every transport, so the C view is generated from the ABI.
    pub const ALL: [Self; 5] = [Self::Ata, Self::Nvme, Self::Usb, Self::Sd, Self::Virtual];

    /// The record byte for this transport.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The transport a record byte names, or `None` for any other byte.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Ata),
            1 => Some(Self::Nvme),
            2 => Some(Self::Usb),
            3 => Some(Self::Sd),
            4 => Some(Self::Virtual),
            _ => None,
        }
    }
}

/// What form a unit's medium takes, where the unit names one.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BlkForm {
    /// The unit names none: a disk, a stick, a card.
    Unspecified = 0,
    /// An optical disc: the unit is a multimedia drive (INQUIRY peripheral
    /// device type 05h, MMC-6).
    OpticalDisc = 1,
}

impl BlkForm {
    /// Every form, so the C view is generated from the ABI.
    pub const ALL: [Self; 2] = [Self::Unspecified, Self::OpticalDisc];

    /// The record byte for this form.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The form a record byte names, or `None` for any other byte.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Unspecified),
            1 => Some(Self::OpticalDisc),
            _ => None,
        }
    }
}

/// What established a [`BlkRotation`].
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BlkRotationEvidence {
    /// The transport admits one answer: `NVMe` and SD media do not rotate.
    /// It never establishes a rotating medium.
    Transport = 1,
    /// MEDIUM ROTATION RATE in the Block Device Characteristics VPD page
    /// (B1h, SBC-4 §6.6.4).
    RotationRateVpd = 2,
    /// IDENTIFY DEVICE word 217, read through ATA PASS-THROUGH (SAT-4,
    /// ACS-4 §9.11.7.4).
    AtaIdentify = 3,
}

impl BlkRotationEvidence {
    /// Every kind of evidence, so the C view is generated from the ABI.
    pub const ALL: [Self; 3] = [Self::Transport, Self::RotationRateVpd, Self::AtaIdentify];

    /// The record byte for this evidence.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The evidence a record byte names, or `None` for any other byte.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Transport),
            2 => Some(Self::RotationRateVpd),
            3 => Some(Self::AtaIdentify),
            _ => None,
        }
    }
}

/// Whether a device's medium rotates.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BlkRotation {
    /// Nothing established it: the device reported nothing, or refused to.
    Unknown,
    /// The medium does not rotate.
    NonRotating(BlkRotationEvidence),
    /// The medium rotates at a nominal `rpm` in the range the rate field can
    /// state; [`BlkRotationEvidence::Transport`] never establishes this.
    Rotating {
        /// The nominal rotation rate, in revolutions per minute.
        rpm: NonZeroU16,
        /// What reported the rate.
        evidence: BlkRotationEvidence,
    },
}

impl BlkRotation {
    /// Read the MEDIUM ROTATION RATE encoding SBC-4 and ACS-4 share, as
    /// `evidence` reported it: `0001h` is a non-rotating medium, `0401h` to
    /// `FFFEh` a nominal rate, and every other value states nothing.
    #[must_use]
    pub const fn from_rate_field(field: u16, evidence: BlkRotationEvidence) -> Self {
        if field == NON_ROTATING_FIELD {
            return Self::NonRotating(evidence);
        }
        if field < *NOMINAL_RPM.start() || field > *NOMINAL_RPM.end() {
            return Self::Unknown;
        }
        match NonZeroU16::new(field) {
            Some(rpm) => Self::Rotating { rpm, evidence },
            None => Self::Unknown,
        }
    }

    const fn is_consistent(self) -> bool {
        match self {
            Self::Unknown | Self::NonRotating(_) => true,
            Self::Rotating { rpm, evidence } => {
                let rpm = rpm.get();
                rpm >= *NOMINAL_RPM.start()
                    && rpm <= *NOMINAL_RPM.end()
                    && !matches!(evidence, BlkRotationEvidence::Transport)
            }
        }
    }
}

/// What established a [`BlkRemovability`].
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BlkRemovabilityEvidence {
    /// The transport or its host establishes it: a card leaves an SD slot,
    /// an `NVMe` medium never leaves its namespace.
    Transport = 1,
    /// INQUIRY's RMB bit (SPC-5 §6.7.2). Many USB bridges set it for a fixed
    /// disk, so it is evidence of removability only, never of what the
    /// medium is.
    Inquiry = 2,
    /// A NOT READY, MEDIUM NOT PRESENT answer (02/3A): only a removable
    /// medium can be absent, so it never establishes a fixed one.
    MediumAbsent = 3,
}

impl BlkRemovabilityEvidence {
    /// Every kind of evidence, so the C view is generated from the ABI.
    pub const ALL: [Self; 3] = [Self::Transport, Self::Inquiry, Self::MediumAbsent];

    /// The record byte for this evidence.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The evidence a record byte names, or `None` for any other byte.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Transport),
            2 => Some(Self::Inquiry),
            3 => Some(Self::MediumAbsent),
            _ => None,
        }
    }
}

/// Whether a device's medium can leave the unit while the unit stays.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BlkRemovability {
    /// Nothing established it.
    Unknown,
    /// The medium cannot leave the unit;
    /// [`BlkRemovabilityEvidence::MediumAbsent`] never establishes this.
    Fixed(BlkRemovabilityEvidence),
    /// The medium can leave the unit.
    Removable(BlkRemovabilityEvidence),
}

impl BlkRemovability {
    /// What INQUIRY's RMB bit states.
    #[must_use]
    pub const fn from_rmb(removable: bool) -> Self {
        if removable {
            Self::Removable(BlkRemovabilityEvidence::Inquiry)
        } else {
            Self::Fixed(BlkRemovabilityEvidence::Inquiry)
        }
    }

    const fn is_consistent(self) -> bool {
        !matches!(self, Self::Fixed(BlkRemovabilityEvidence::MediumAbsent))
    }
}

/// What one device command costs, which a filesystem's write-back window
/// amortises (`lib/writeback`).
///
/// Kept apart from [`BlkDeviceClass`]: the class is how *patient* a consumer
/// is, read conservatively as the slowest medium when nothing is known, while
/// the cost is what batching buys, which an unknown rotation does not change
/// for a bridged unit.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BlkCommandCost {
    /// Each command crosses a bridge or a card protocol whose fixed cost
    /// dominates it: USB and SD units, whatever their medium.
    Bridged,
    /// A scattered command may cost a seek: a medium that rotates, or one not
    /// known not to.
    Seek,
    /// Commands are cheap: a non-rotating medium on a direct bus, or a
    /// paravirtual device.
    Cheap,
}

impl BlkCommandCost {
    /// The cost of a command to a device of `medium`. A device that declared
    /// nothing is taken as cheap, so it is given the window that holds the
    /// least unpublished work.
    #[must_use]
    pub const fn of(medium: Option<BlkMedium>) -> Self {
        match medium {
            Some(medium) => medium.command_cost(),
            None => Self::Cheap,
        }
    }
}

/// What a block device is, as the driver that binds it establishes it.
///
/// The patience the device is served with follows from it
/// ([`class`](Self::class)), as does the write-back cost
/// ([`command_cost`](Self::command_cost)), so what a device is said to be and
/// how it is served cannot disagree.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BlkMedium {
    /// The bus the device is reached over.
    pub transport: BlkTransport,
    /// Whether the medium rotates.
    pub rotation: BlkRotation,
    /// Whether the medium can leave the unit.
    pub removability: BlkRemovability,
    /// What form the medium takes.
    pub form: BlkForm,
}

impl BlkMedium {
    /// What `transport` alone establishes: an `NVMe` medium neither rotates
    /// nor leaves its namespace, an SD card does not rotate, and every other
    /// fact waits for the device to report it.
    #[must_use]
    pub const fn of_transport(transport: BlkTransport) -> Self {
        let (rotation, removability) = match transport {
            BlkTransport::Nvme => (
                BlkRotation::NonRotating(BlkRotationEvidence::Transport),
                BlkRemovability::Fixed(BlkRemovabilityEvidence::Transport),
            ),
            BlkTransport::Sd => (
                BlkRotation::NonRotating(BlkRotationEvidence::Transport),
                BlkRemovability::Unknown,
            ),
            BlkTransport::Ata | BlkTransport::Usb | BlkTransport::Virtual => {
                (BlkRotation::Unknown, BlkRemovability::Unknown)
            }
        };
        Self {
            transport,
            rotation,
            removability,
            form: BlkForm::Unspecified,
        }
    }

    /// This medium with its rotation established as `rotation`.
    #[must_use]
    pub const fn with_rotation(self, rotation: BlkRotation) -> Self {
        Self { rotation, ..self }
    }

    /// This medium with its removability established as `removability`.
    #[must_use]
    pub const fn with_removability(self, removability: BlkRemovability) -> Self {
        Self {
            removability,
            ..self
        }
    }

    /// This medium with its form established as `form`.
    #[must_use]
    pub const fn with_form(self, form: BlkForm) -> Self {
        Self { form, ..self }
    }

    /// The patience a device of this medium is served with.
    ///
    /// Only positive evidence tightens it: a medium that rotates, or one not
    /// known not to, is served as the slowest medium the tree defines, so a
    /// disk spinning up is never punished for a guess. A non-rotating medium
    /// on a hot-pluggable bus is served as removable, one on a direct bus as
    /// solid-state.
    #[must_use]
    pub const fn class(self) -> BlkDeviceClass {
        match (self.transport, self.rotation) {
            (BlkTransport::Virtual, _) => BlkDeviceClass::Virtual,
            (_, BlkRotation::Unknown | BlkRotation::Rotating { .. }) => BlkDeviceClass::Rotational,
            (BlkTransport::Usb | BlkTransport::Sd, BlkRotation::NonRotating(_)) => {
                BlkDeviceClass::Removable
            }
            (BlkTransport::Ata | BlkTransport::Nvme, BlkRotation::NonRotating(_)) => {
                BlkDeviceClass::SolidState
            }
        }
    }

    /// What one command to a device of this medium costs.
    #[must_use]
    pub const fn command_cost(self) -> BlkCommandCost {
        match (self.transport, self.rotation) {
            (BlkTransport::Usb | BlkTransport::Sd, _) => BlkCommandCost::Bridged,
            (BlkTransport::Virtual, _)
            | (BlkTransport::Ata | BlkTransport::Nvme, BlkRotation::NonRotating(_)) => {
                BlkCommandCost::Cheap
            }
            (BlkTransport::Ata | BlkTransport::Nvme, _) => BlkCommandCost::Seek,
        }
    }

    /// The record for `medium`, or for a device that declared nothing.
    ///
    /// The one encoding every carrier shares: the block-service completion,
    /// the mount record and the hardware-tree property.
    #[must_use]
    pub fn encode(medium: Option<Self>) -> [u8; BLK_MEDIUM_LEN] {
        let mut record = [0u8; BLK_MEDIUM_LEN];
        let Some(medium) = medium else {
            record[TRANSPORT_OFF] = BLK_TRANSPORT_UNDECLARED;
            return record;
        };
        record[TRANSPORT_OFF] = medium.transport.as_u8();
        let (kind, evidence, rpm) = match medium.rotation {
            BlkRotation::Unknown => (BLK_FACT_UNKNOWN, BLK_EVIDENCE_NONE, 0),
            BlkRotation::NonRotating(evidence) => (BLK_ROTATION_NON_ROTATING, evidence.as_u8(), 0),
            BlkRotation::Rotating { rpm, evidence } => {
                (BLK_ROTATION_ROTATING, evidence.as_u8(), rpm.get())
            }
        };
        record[ROTATION_OFF] = kind;
        record[ROTATION_EVIDENCE_OFF] = evidence;
        put_u16(&mut record, RPM_OFF, rpm);
        let (kind, evidence) = match medium.removability {
            BlkRemovability::Unknown => (BLK_FACT_UNKNOWN, BLK_EVIDENCE_NONE),
            BlkRemovability::Fixed(evidence) => (BLK_REMOVABILITY_FIXED, evidence.as_u8()),
            BlkRemovability::Removable(evidence) => (BLK_REMOVABILITY_REMOVABLE, evidence.as_u8()),
        };
        record[REMOVABILITY_OFF] = kind;
        record[REMOVABILITY_EVIDENCE_OFF] = evidence;
        record[FORM_OFF] = medium.form.as_u8();
        record
    }

    /// The medium a record states, or `None` for a device that declared
    /// nothing and for every record this ABI does not define.
    ///
    /// Every malformed record collapses to the one `None`, so a driver cannot
    /// smuggle a value of its choosing through a relaying consumer, and an
    /// inconsistent claim (a rotating medium the transport established, a
    /// fixed one an absent medium established) is refused rather than
    /// believed.
    #[must_use]
    pub fn decode(record: &[u8; BLK_MEDIUM_LEN]) -> Option<Self> {
        let transport = BlkTransport::from_u8(record[TRANSPORT_OFF])?;
        let form = BlkForm::from_u8(record[FORM_OFF])?;
        let rpm = read_u16(record, RPM_OFF);
        let rotation = match (record[ROTATION_OFF], record[ROTATION_EVIDENCE_OFF], rpm) {
            (BLK_FACT_UNKNOWN, BLK_EVIDENCE_NONE, 0) => BlkRotation::Unknown,
            (BLK_ROTATION_NON_ROTATING, evidence, 0) => {
                BlkRotation::NonRotating(BlkRotationEvidence::from_u8(evidence)?)
            }
            (BLK_ROTATION_ROTATING, evidence, rpm) => BlkRotation::Rotating {
                rpm: NonZeroU16::new(rpm)?,
                evidence: BlkRotationEvidence::from_u8(evidence)?,
            },
            _ => return None,
        };
        let removability = match (record[REMOVABILITY_OFF], record[REMOVABILITY_EVIDENCE_OFF]) {
            (BLK_FACT_UNKNOWN, BLK_EVIDENCE_NONE) => BlkRemovability::Unknown,
            (BLK_REMOVABILITY_FIXED, evidence) => {
                BlkRemovability::Fixed(BlkRemovabilityEvidence::from_u8(evidence)?)
            }
            (BLK_REMOVABILITY_REMOVABLE, evidence) => {
                BlkRemovability::Removable(BlkRemovabilityEvidence::from_u8(evidence)?)
            }
            _ => return None,
        };
        (rotation.is_consistent() && removability.is_consistent()).then_some(Self {
            transport,
            rotation,
            removability,
            form,
        })
    }

    /// The medium of a composition over members of `media`: that of the
    /// member it must wait on longest, each member waited on as
    /// [`BlkDeviceClass::of`] its medium, so the composition is served the
    /// patience of its slowest member and reads as that member. When that
    /// member declared nothing the composition is unknown too, so a layer
    /// never asserts a medium no member declared. Equally patient members
    /// rank a declared medium first, then the greater record, so the answer
    /// does not depend on their order.
    #[must_use]
    pub fn of_members(media: impl IntoIterator<Item = Option<Self>>) -> Option<Self> {
        media
            .into_iter()
            .reduce(|held, offered| {
                if Self::outwaits(offered, held) {
                    offered
                } else {
                    held
                }
            })
            .flatten()
    }

    /// Whether a member of `offered` must be waited on longer than one of
    /// `held`: a total order, so a fold over it is order-independent.
    fn outwaits(offered: Option<Self>, held: Option<Self>) -> bool {
        let (wider, narrower) = (BlkDeviceClass::of(offered), BlkDeviceClass::of(held));
        if wider != narrower {
            return wider.most_patient(narrower) == wider;
        }
        match (offered, held) {
            (Some(_), Some(_)) => Self::encode(offered) > Self::encode(held),
            (Some(_), None) => true,
            (None, _) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    fn rotations() -> Vec<BlkRotation> {
        let rpm = |v| NonZeroU16::new(v).expect("non-zero");
        let mut every = vec![BlkRotation::Unknown];
        for evidence in BlkRotationEvidence::ALL {
            every.push(BlkRotation::NonRotating(evidence));
            if evidence != BlkRotationEvidence::Transport {
                for value in [0x0401, 5400, 7200, 15_000, 0xFFFE] {
                    every.push(BlkRotation::Rotating {
                        rpm: rpm(value),
                        evidence,
                    });
                }
            }
        }
        every
    }

    fn removabilities() -> Vec<BlkRemovability> {
        let mut every = vec![BlkRemovability::Unknown];
        for evidence in BlkRemovabilityEvidence::ALL {
            every.push(BlkRemovability::Removable(evidence));
            if evidence != BlkRemovabilityEvidence::MediumAbsent {
                every.push(BlkRemovability::Fixed(evidence));
            }
        }
        every
    }

    fn every_medium() -> Vec<BlkMedium> {
        let mut every = Vec::new();
        for transport in BlkTransport::ALL {
            for rotation in rotations() {
                for removability in removabilities() {
                    for form in BlkForm::ALL {
                        every.push(BlkMedium {
                            transport,
                            rotation,
                            removability,
                            form,
                        });
                    }
                }
            }
        }
        every
    }

    #[test]
    fn every_consistent_medium_round_trips_its_record() {
        for medium in every_medium() {
            assert_eq!(
                BlkMedium::decode(&BlkMedium::encode(Some(medium))),
                Some(medium)
            );
        }
        assert_eq!(BlkMedium::decode(&BlkMedium::encode(None)), None);
    }

    #[test]
    fn a_malformed_or_inconsistent_record_is_no_medium_at_all() {
        let valid = BlkMedium::encode(Some(
            BlkMedium::of_transport(BlkTransport::Usb)
                .with_rotation(BlkRotation::from_rate_field(
                    7200,
                    BlkRotationEvidence::RotationRateVpd,
                ))
                .with_removability(BlkRemovability::from_rmb(true)),
        ));
        assert!(BlkMedium::decode(&valid).is_some());
        // Every single-byte corruption either still names a consistent
        // medium, which then re-encodes to exactly the corrupted bytes, or
        // names none: nothing decodes to a value the record does not spell.
        for at in 0..BLK_MEDIUM_LEN {
            for value in 0..=u8::MAX {
                let mut record = valid;
                record[at] = value;
                if let Some(medium) = BlkMedium::decode(&record) {
                    assert_eq!(
                        BlkMedium::encode(Some(medium)),
                        record,
                        "byte {at} = {value}"
                    );
                }
            }
        }
        let inconsistent = [
            // A rate the transport claims to have established.
            BlkMedium {
                transport: BlkTransport::Ata,
                rotation: BlkRotation::Rotating {
                    rpm: NonZeroU16::new(7200).expect("non-zero"),
                    evidence: BlkRotationEvidence::Transport,
                },
                removability: BlkRemovability::Unknown,
                form: BlkForm::Unspecified,
            },
            // A fixed medium an absent one established.
            BlkMedium {
                transport: BlkTransport::Usb,
                rotation: BlkRotation::Unknown,
                removability: BlkRemovability::Fixed(BlkRemovabilityEvidence::MediumAbsent),
                form: BlkForm::Unspecified,
            },
            // A rate below what the field can state.
            BlkMedium {
                transport: BlkTransport::Usb,
                rotation: BlkRotation::Rotating {
                    rpm: NonZeroU16::new(2).expect("non-zero"),
                    evidence: BlkRotationEvidence::AtaIdentify,
                },
                removability: BlkRemovability::Unknown,
                form: BlkForm::OpticalDisc,
            },
        ];
        for medium in inconsistent {
            assert_eq!(
                BlkMedium::decode(&BlkMedium::encode(Some(medium))),
                None,
                "{medium:?}"
            );
        }
        let mut unknown_form = valid;
        unknown_form[FORM_OFF] = 2;
        assert_eq!(
            BlkMedium::decode(&unknown_form),
            None,
            "a form this ABI does not define"
        );
    }

    #[test]
    fn the_rate_field_reads_as_sbc_and_acs_define_it() {
        let vpd = BlkRotationEvidence::RotationRateVpd;
        assert_eq!(
            BlkRotation::from_rate_field(0x0000, vpd),
            BlkRotation::Unknown
        );
        assert_eq!(
            BlkRotation::from_rate_field(0x0001, vpd),
            BlkRotation::NonRotating(vpd)
        );
        for reserved in [0x0002, 0x0400, 0xFFFF] {
            assert_eq!(
                BlkRotation::from_rate_field(reserved, vpd),
                BlkRotation::Unknown
            );
        }
        assert_eq!(
            BlkRotation::from_rate_field(7200, vpd),
            BlkRotation::Rotating {
                rpm: NonZeroU16::new(7200).expect("non-zero"),
                evidence: vpd,
            }
        );
    }

    #[test]
    fn only_positive_evidence_tightens_the_patience() {
        let usb = BlkMedium::of_transport(BlkTransport::Usb);
        let ata = BlkMedium::of_transport(BlkTransport::Ata);
        let flash = BlkRotation::NonRotating(BlkRotationEvidence::RotationRateVpd);
        let spinning = BlkRotation::from_rate_field(7200, BlkRotationEvidence::AtaIdentify);
        assert_eq!(usb.class(), BlkDeviceClass::Rotational);
        assert_eq!(
            usb.with_rotation(spinning).class(),
            BlkDeviceClass::Rotational
        );
        assert_eq!(usb.with_rotation(flash).class(), BlkDeviceClass::Removable);
        assert_eq!(
            usb.with_removability(BlkRemovability::from_rmb(true))
                .class(),
            BlkDeviceClass::Rotational,
            "a removable bit says nothing of the medium"
        );
        assert_eq!(ata.class(), BlkDeviceClass::Rotational);
        assert_eq!(ata.with_rotation(flash).class(), BlkDeviceClass::SolidState);
        assert_eq!(
            BlkMedium::of_transport(BlkTransport::Nvme).class(),
            BlkDeviceClass::SolidState
        );
        assert_eq!(
            BlkMedium::of_transport(BlkTransport::Sd).class(),
            BlkDeviceClass::Removable
        );
        for medium in every_medium() {
            if medium.transport == BlkTransport::Virtual {
                assert_eq!(medium.class(), BlkDeviceClass::Virtual);
            }
        }
        assert_eq!(BlkDeviceClass::of(None), BlkDeviceClass::Virtual);
    }

    #[test]
    fn a_bridged_unit_costs_the_same_whatever_its_medium() {
        for medium in every_medium() {
            let cost = medium.command_cost();
            match medium.transport {
                BlkTransport::Usb | BlkTransport::Sd => assert_eq!(cost, BlkCommandCost::Bridged),
                BlkTransport::Virtual => assert_eq!(cost, BlkCommandCost::Cheap),
                BlkTransport::Ata | BlkTransport::Nvme => match medium.rotation {
                    BlkRotation::NonRotating(_) => assert_eq!(cost, BlkCommandCost::Cheap),
                    _ => assert_eq!(cost, BlkCommandCost::Seek),
                },
            }
        }
        assert_eq!(BlkCommandCost::of(None), BlkCommandCost::Cheap);
    }

    #[test]
    fn a_composition_reads_as_its_slowest_member_in_any_order() {
        let nvme = Some(BlkMedium::of_transport(BlkTransport::Nvme));
        let disk = Some(BlkMedium::of_transport(BlkTransport::Ata).with_rotation(
            BlkRotation::from_rate_field(7200, BlkRotationEvidence::AtaIdentify),
        ));
        assert_eq!(BlkMedium::of_members([nvme, disk]), disk);
        assert_eq!(BlkMedium::of_members([disk, nvme]), disk);
        assert_eq!(
            BlkMedium::of_members([nvme, None, disk]),
            disk,
            "a disk is waited on longer than an undeclared member"
        );
        assert_eq!(BlkMedium::of_members([None, None]), None);
        assert_eq!(BlkMedium::of_members([]), None);
    }

    #[test]
    fn an_undeclared_member_is_waited_on_as_paravirtual() {
        // The paravirtual envelope outwaits a solid-state member's, so the
        // array waits as long as the undeclared member may take, and reads as
        // unknown, which it is.
        let nvme = Some(BlkMedium::of_transport(BlkTransport::Nvme));
        assert_eq!(BlkMedium::of_members([nvme, None]), None);
        assert_eq!(BlkMedium::of_members([None, nvme]), None);
        let virtio = Some(BlkMedium::of_transport(BlkTransport::Virtual));
        assert_eq!(
            BlkMedium::of_members([None, virtio]),
            virtio,
            "an equally patient declared member names the array"
        );
        let card = Some(BlkMedium::of_transport(BlkTransport::Sd));
        assert_eq!(
            BlkMedium::of_members([None, card]),
            card,
            "a removable member outwaits the paravirtual envelope"
        );
    }

    #[test]
    fn a_composition_answers_the_same_in_either_order() {
        let sample: Vec<Option<BlkMedium>> = every_medium()
            .into_iter()
            .step_by(7)
            .map(Some)
            .chain([None])
            .collect();
        for a in &sample {
            for b in &sample {
                assert_eq!(
                    BlkMedium::of_members([*a, *b]),
                    BlkMedium::of_members([*b, *a]),
                    "{a:?} / {b:?}"
                );
            }
        }
    }
}
