//! The transport-neutral SCSI engine (`plans/DEVICES.md` D5).
//!
//! USB mass storage carries SCSI commands over one of several wire
//! transports — Bulk-Only Transport ([`crate::bot`]), Control/Bulk/Interrupt
//! ([`crate::cbi`]), and USB Attached SCSI ([`crate::uas`]). The *commands*
//! are the same; only the framing differs. This module is the one home of
//! the command layer: it builds each CDB, validates every device-supplied
//! payload fail-closed, and maps sense data onto errors, generic over the
//! [`ScsiTransport`] seam each wire transport implements.
//!
//! The command *set* also varies by device: a disk speaks the SCSI
//! transparent set, a USB floppy speaks UFI (12-byte fixed CDBs and
//! `MODE SENSE(10)`, USB Mass Storage UFI 1.0), and an optical drive behind
//! an ATAPI bridge takes the transparent set in 12-byte packets.
//! [`CommandSet`] carries that per-device spelling so the logic exists once.
//! An optical drive's disc is read through the multimedia commands
//! ([`ScsiDevice::disc`], MMC-6).

use core::num::NonZeroU32;

use tairix_abi::blkio::{
    BlkDeviceName, BlkForm, BlkMedium, BlkReissue, BlkRemovability, BlkRotation,
    BlkRotationEvidence, BlkTransport, CommandBudget, CommandKind,
};
use tairix_abi::driver::block::{Block, BlockGeometry, DiscLayout};
use tairix_abi::driver::BufferClass;
use tairix_abi::{DriverError, Errno};

use crate::ready::Readiness;

/// One SCSI command execution seam: a wire transport (BOT, CBI, UAS) runs
/// the command block, moves the data phase, and reports the device's honest
/// verdict. A host test implements it as a scripted device.
pub trait ScsiTransport {
    /// Execute one SCSI command for `lun`: send `cdb`, move `data` within
    /// `exchange_ns`, and return whether the device reported success plus the
    /// bytes that actually moved (never more than the data phase's length).
    ///
    /// # Errors
    ///
    /// An [`Errno`] for a transport fault or protocol violation (the
    /// command's *failure* is an answer, not an error). Starting a new
    /// execution invalidates any in-band sense capture a previous command
    /// left behind ([`Self::take_sense`]), so stale sense can never
    /// explain a later failure.
    fn execute(
        &mut self,
        lun: u8,
        cdb: &[u8],
        data: DataPhase<'_>,
        exchange_ns: u64,
    ) -> Result<CommandOutcome, Errno>;

    /// Number of logical units the device exposes, by the transport's own
    /// discovery (BOT `GET MAX LUN`, UAS `REPORT LUNS`, CBI exactly one).
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a count outside the protocol bound, or a
    /// transport fault.
    fn lun_count(&mut self) -> Result<u8, Errno>;

    /// Take the sense data the transport captured with the last failed
    /// command, if its protocol delivers sense in-band (UAS autosense).
    /// `None` means the caller must issue `REQUEST SENSE`.
    fn take_sense(&mut self) -> Option<Sense>;

    /// Reset the transport so the next command starts in step with the
    /// device: BOT's reset recovery, CBI's Command Block Reset and bulk
    /// clears, UAS's four pipes cleared. A failed exchange runs this itself;
    /// it is offered for the recovery ladder's stronger rung.
    ///
    /// # Errors
    ///
    /// The transport's or device's refusal.
    fn reset(&mut self) -> Result<(), Errno>;

    /// Zero every byte of the transport's shared data window, so a
    /// sensitive payload does not outlive the block operation that moved
    /// it (the zero-on-free discipline applied to the bounce window).
    fn scrub(&mut self);

    /// Whether the reset recovery the last broken exchange ran put the
    /// transport back in step, taken: only then is the unit worth asking
    /// whether it is ready.
    fn take_resynced(&mut self) -> bool;
}

/// The command-set spelling a device speaks.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CommandSet {
    /// The SCSI transparent command set (interface sub-class `0x06`).
    Transparent,
    /// UFI, the USB floppy command set (sub-class `0x04`): every CDB is
    /// padded to 12 bytes, write protection is read with `MODE SENSE(10)`,
    /// and the set carries no `SYNCHRONIZE CACHE` (the medium is written
    /// through).
    Ufi,
    /// The transparent set carried as ATAPI packets (sub-class `0x02`, an
    /// optical drive behind an ATAPI bridge): every CDB is padded to the
    /// 12-byte packet, and write protection is read with `MODE SENSE(10)`,
    /// the only form ATAPI defines.
    Atapi,
}

impl CommandSet {
    /// Whether every command block is padded to 12 bytes.
    const fn pads(self) -> bool {
        matches!(self, Self::Ufi | Self::Atapi)
    }
}

/// Fixed UFI command-block length (UFI 1.0 §3.2: all commands are 12-byte
/// blocks, zero-padded).
const UFI_CDB_LEN: usize = 12;

/// The data phase of one SCSI command.
pub enum DataPhase<'a> {
    /// Device-to-host data of the given length.
    In(&'a mut [u8]),
    /// Host-to-device data.
    Out(&'a [u8]),
    /// No data phase.
    None,
}

impl DataPhase<'_> {
    /// Bytes the phase covers.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            DataPhase::In(buf) => buf.len(),
            DataPhase::Out(buf) => buf.len(),
            DataPhase::None => 0,
        }
    }

    /// Whether the phase moves no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the phase is device-to-host (a no-data phase counts as IN,
    /// matching the BOT CBW direction flag convention).
    #[must_use]
    pub const fn is_in(&self) -> bool {
        matches!(self, DataPhase::In(_) | DataPhase::None)
    }
}

/// The outcome of one executed command: whether the device reported success
/// and how many data-phase bytes actually moved.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CommandOutcome {
    /// The device reported the command passed.
    pub passed: bool,
    /// Data-phase bytes that actually moved.
    pub transferred: usize,
}

/// Fixed-format sense data (SPC-4 §4.5.3): the why of a failed command.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Sense {
    /// Sense key (low nibble of byte 2).
    pub key: u8,
    /// Additional sense code.
    pub asc: u8,
    /// Additional sense code qualifier.
    pub ascq: u8,
}

/// Sense key: a check condition that names no cause.
const SENSE_KEY_NO_SENSE: u8 = 0x00;
/// Sense key: the addressed unit is not ready.
const SENSE_KEY_NOT_READY: u8 = 0x02;
/// Sense key: the medium could not be read or written where addressed.
const SENSE_KEY_MEDIUM_ERROR: u8 = 0x03;
/// Sense key: the unit failed in hardware carrying out the command.
const SENSE_KEY_HARDWARE_ERROR: u8 = 0x04;
/// Sense key: the command was illegal for this device.
const SENSE_KEY_ILLEGAL_REQUEST: u8 = 0x05;
/// Sense key: unit attention (media change / reset notification).
const SENSE_KEY_UNIT_ATTENTION: u8 = 0x06;
/// Sense key: the medium is write-protected.
const SENSE_KEY_DATA_PROTECT: u8 = 0x07;
/// Sense key: a write-once medium holds nothing where addressed.
const SENSE_KEY_BLANK_CHECK: u8 = 0x08;
/// Sense key: the unit aborted the command; asked again, it may complete.
const SENSE_KEY_ABORTED_COMMAND: u8 = 0x0B;

impl Sense {
    /// Reconstruct fixed-format sense from a UFI command-completion
    /// interrupt block, which carries only the additional sense code and
    /// qualifier (USB Mass Storage CBI 1.1 §3.4.3.1.1) rather than the full
    /// sense triple. The sense key is recovered from the ASC using the SCSI
    /// additional-sense-code assignments (SPC-4 Annex F / SBC-3): each of
    /// these codes is only ever reported under one key — write protection
    /// under DATA PROTECT, a not-ready or no-medium state under NOT READY, a
    /// media-change or power-on/reset notification under UNIT ATTENTION, and
    /// a rejected command or CDB field under ILLEGAL REQUEST. Any other code
    /// carries no recovered key (`0`), so it surfaces as an unclassified
    /// device fault rather than a guessed category (fail closed).
    #[must_use]
    pub fn from_ufi_completion(asc: u8, ascq: u8) -> Self {
        let key = match asc {
            0x27 => SENSE_KEY_DATA_PROTECT,
            0x04 | 0x3A => SENSE_KEY_NOT_READY,
            0x28 | 0x29 => SENSE_KEY_UNIT_ATTENTION,
            0x20 | 0x21 | 0x24 | 0x26 => SENSE_KEY_ILLEGAL_REQUEST,
            _ => 0,
        };
        Self { key, asc, ascq }
    }
}

/// What a LUN's standard INQUIRY reported.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Inquiry {
    /// Peripheral device type (byte 0 low five bits); `0x00` is a
    /// direct-access block device (a disk).
    pub device_type: u8,
    /// Whether the medium is removable (byte 1 bit 7).
    pub removable: bool,
}

/// Peripheral device type of a direct-access block device.
pub const DEVICE_TYPE_DIRECT_ACCESS: u8 = 0x00;
/// Peripheral device type of a multimedia (optical) drive (MMC-6 §4.1).
pub const DEVICE_TYPE_MMC: u8 = 0x05;

impl Inquiry {
    /// Whether the unit is an optical drive.
    #[must_use]
    pub const fn is_optical(&self) -> bool {
        self.device_type == DEVICE_TYPE_MMC
    }
}

/// A logical unit's brought-up state: its geometry, write policy and what
/// it is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LunState {
    /// The unit's block geometry, read from `READ CAPACITY`.
    pub geometry: BlockGeometry,
    /// Whether the medium is write-protected (`MODE SENSE` WP bit);
    /// writes are refused driver-side before touching the device.
    pub write_protected: bool,
    /// What the unit reported of itself ([`ScsiDevice::probe`]).
    pub report: UnitReport,
    /// Where an optical drive's disc keeps its newest structures
    /// ([`ScsiDevice::disc`]); `None` for any other unit.
    pub disc_layout: Option<DiscLayout>,
}

/// What an optical drive reports of the disc it holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Disc {
    /// Where the disc's newest structures lie.
    pub layout: DiscLayout,
    /// The disc's blocks as far as it is recorded, which on a disc whose
    /// last session is still open runs past what `READ CAPACITY` counts.
    pub geometry: BlockGeometry,
    /// Whether any block may be rewritten in place: the drive's Random
    /// Writable feature is current for this disc (MMC-6 §5.3.13).
    pub random_writable: bool,
}

/// GET CONFIGURATION's Random Writable feature (MMC-6 §5.3.13).
const FEATURE_RANDOM_WRITABLE: u16 = 0x0020;
/// READ TRACK INFORMATION's byte 6 bit: the track is blank.
const TRACK_BLANK: u8 = 0x40;
/// READ TRACK INFORMATION's byte 7 bits: the next writable and the last
/// recorded addresses are valid.
const TRACK_NWA_VALID: u8 = 0x01;
const TRACK_LRA_VALID: u8 = 0x02;

/// What a unit reported of itself beyond its standard INQUIRY.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UnitReport {
    /// What its medium is, each fact with the evidence for it.
    pub medium: BlkMedium,
    /// The most blocks one command may move, when the unit states a limit
    /// (Block Limits VPD page, SBC-4 §6.6.4).
    pub max_transfer_blocks: Option<NonZeroU32>,
}

/// INQUIRY's EVPD bit (SPC-5 §6.7.1).
const EVPD: u8 = 0x01;
/// The Supported VPD Pages page.
const VPD_SUPPORTED: u8 = 0x00;
/// The Block Limits page.
const VPD_BLOCK_LIMITS: u8 = 0xB0;
/// The Block Device Characteristics page.
const VPD_CHARACTERISTICS: u8 = 0xB1;
/// Allocation for a VPD page: under 256, so a unit following SPC-2's one-byte
/// allocation field reads it the same, and enough for the pages read here.
const VPD_ALLOCATION: u8 = 64;
/// ATA PASS-THROUGH(16) (SAT-4 §12.2.2).
const ATA_PASS_THROUGH_16: u8 = 0x85;
/// PROTOCOL PIO Data-In, in byte 1's PROTOCOL field.
const ATA_PROTOCOL_PIO_IN: u8 = 4 << 1;
/// `T_DIR` from the device, `BYT_BLOK` in blocks, `T_LENGTH` in the COUNT field.
const ATA_TRANSFER_IN_BLOCKS_BY_COUNT: u8 = 0x08 | 0x04 | 0x02;
/// ATA IDENTIFY DEVICE (ACS-4 §7.12).
const ATA_IDENTIFY_DEVICE: u8 = 0xEC;
/// Bytes of IDENTIFY DEVICE data.
const IDENTIFY_LEN: usize = 512;
/// The byte of word 217, NOMINAL MEDIA ROTATION RATE.
const IDENTIFY_ROTATION_RATE: usize = 2 * 217;
/// The integrity word's signature byte (ACS-4 §7.12.6.88).
const IDENTIFY_CHECKSUM_SIGNATURE: u8 = 0xA5;

/// Most logical units a served device can expose (the BOT `GET MAX LUN`
/// bound, `0..=15`; UAS units above it are not served). A fixed protocol
/// bound, not a scalable capacity.
pub const MAX_LUNS: usize = 16;

/// Most bytes one SCSI READ/WRITE issued by [`LunBlock`] covers: the
/// block-service data window ([`tairix_abi::blkio::BLK_DATA_LEN`] — one
/// definition), so a Block call of any size splits into bounded commands
/// and per-device cost never scales with request length.
pub const MSD_MAX_TRANSFER_LEN: usize = tairix_abi::blkio::BLK_DATA_LEN;

/// One SCSI device: the command layer over a wire transport.
pub struct ScsiDevice<T: ScsiTransport> {
    transport: T,
    set: CommandSet,
    /// Each unit's command budget, from what its medium is known to be: until
    /// a probe says otherwise, that of a USB unit nothing is known of.
    budgets: [CommandBudget; MAX_LUNS],
    /// What each unit's last failed media command said of its readiness,
    /// until the serve loop takes it.
    readiness: [Option<Readiness>; MAX_LUNS],
    /// When each unit's last broken media command may come back, as its
    /// recovery found it, until the serve engine asks
    /// ([`Self::take_reissue`]).
    reissue: [BlkReissue; MAX_LUNS],
    /// Which units' medium can leave them.
    removable: [bool; MAX_LUNS],
    /// Which units' next reset attention is the one a transport reset this
    /// layer ran raised.
    expecting_reset: [bool; MAX_LUNS],
    /// The last command's exchange broke and the reset recovery it ran put
    /// the transport back in step.
    resynced: bool,
}

impl<T: ScsiTransport> ScsiDevice<T> {
    /// The command layer over `transport`, speaking `set`.
    pub fn new(transport: T, set: CommandSet) -> Self {
        Self {
            transport,
            set,
            budgets: [unknown_unit_budget(); MAX_LUNS],
            readiness: [None; MAX_LUNS],
            reissue: [BlkReissue::Paced; MAX_LUNS],
            removable: [false; MAX_LUNS],
            expecting_reset: [false; MAX_LUNS],
            resynced: false,
        }
    }

    /// Serve `lun` with the budget its `medium` is given from now on.
    pub fn set_medium(&mut self, lun: u8, medium: BlkMedium) {
        let index = usize::from(lun);
        if let Some(budget) = self.budgets.get_mut(index) {
            *budget = medium.class().budget().command;
        }
        if let Some(removable) = self.removable.get_mut(index) {
            *removable = matches!(medium.removability, BlkRemovability::Removable(_));
        }
    }

    /// `lun` carried out a command: a reset attention it had yet to raise
    /// for a reset this layer ran, it never will.
    fn answered(&mut self, lun: u8) {
        if let Some(expecting) = self.expecting_reset.get_mut(usize::from(lun)) {
            *expecting = false;
        }
    }

    /// What `sense`, refusing a command `lun` was sent, says of the unit's
    /// readiness. A reset this layer did not run stands, on a unit whose
    /// medium can leave, for whatever change of medium its attention
    /// replaced, so the medium is read again.
    fn classify(&mut self, lun: u8, sense: Sense) -> Readiness {
        if !Readiness::is_reset(sense) {
            let readiness = Readiness::of_sense(sense);
            if readiness == Readiness::Ready {
                self.answered(lun);
            }
            return readiness;
        }
        let index = usize::from(lun);
        let expected = self
            .expecting_reset
            .get_mut(index)
            .is_some_and(core::mem::take);
        if !expected && self.removable.get(index).copied().unwrap_or(false) {
            Readiness::MediumChanged
        } else {
            Readiness::Attention
        }
    }

    /// The command budget `lun` is served with.
    #[must_use]
    pub fn budget(&self, lun: u8) -> CommandBudget {
        self.budgets
            .get(usize::from(lun))
            .copied()
            .unwrap_or_else(unknown_unit_budget)
    }

    /// What `lun`'s last failed media command said of its readiness, if it
    /// said anything, leaving it recorded.
    #[must_use]
    pub fn readiness(&self, lun: u8) -> Option<Readiness> {
        self.readiness.get(usize::from(lun)).copied().flatten()
    }

    /// Take what `lun`'s last failed media command said of its readiness.
    pub fn take_readiness(&mut self, lun: u8) -> Option<Readiness> {
        self.readiness
            .get_mut(usize::from(lun))
            .and_then(Option::take)
    }

    /// Take when `lun`'s last broken media command may come back:
    /// [`BlkReissue::Now`] once the reset its transport ran left the unit
    /// answering ready.
    pub fn take_reissue(&mut self, lun: u8) -> BlkReissue {
        self.reissue
            .get_mut(usize::from(lun))
            .map_or(BlkReissue::Paced, core::mem::take)
    }

    /// Number of logical units the device exposes.
    ///
    /// # Errors
    ///
    /// As [`ScsiTransport::lun_count`].
    pub fn lun_count(&mut self) -> Result<u8, Errno> {
        self.transport.lun_count()
    }

    /// Execute `cdb`, a command of `kind`, within its exchange budget, with
    /// the command set's spelling applied: a UFI device receives every
    /// command as a 12-byte zero-padded block.
    fn command(
        &mut self,
        lun: u8,
        cdb: &[u8],
        data: DataPhase<'_>,
        kind: CommandKind,
    ) -> Result<CommandOutcome, Errno> {
        if cdb.is_empty() || cdb.len() > 16 {
            return Err(Errno::OutOfRange);
        }
        let exchange_ns = self.budget(lun).exchange_ns(kind, data.len());
        let mut padded = [0u8; UFI_CDB_LEN];
        let cdb = if self.set.pads() && cdb.len() < UFI_CDB_LEN {
            padded[..cdb.len()].copy_from_slice(cdb);
            &padded[..]
        } else {
            cdb
        };
        let outcome = self.transport.execute(lun, cdb, data, exchange_ns);
        self.resynced = outcome.is_err() && self.transport.take_resynced();
        if self.resynced {
            // Whatever the command, every unit may answer the reset.
            self.expecting_reset = [true; MAX_LUNS];
        }
        outcome
    }

    /// The sense data explaining the last failed command: the transport's
    /// in-band capture where its protocol carries one (UAS), else a
    /// `REQUEST SENSE` round trip.
    fn sense_for_failure(&mut self, lun: u8) -> Result<Sense, Errno> {
        if let Some(sense) = self.transport.take_sense() {
            return Ok(sense);
        }
        self.request_sense(lun)
    }

    /// Standard INQUIRY: what kind of unit this LUN is.
    ///
    /// # Errors
    ///
    /// [`Errno::NotImplemented`] if the device fails the command,
    /// [`Errno::LengthOutOfRange`] for a response too short to carry the
    /// identity bytes, or a transport error.
    pub fn inquiry(&mut self, lun: u8) -> Result<Inquiry, Errno> {
        // INQUIRY (SPC-4 §6.6): standard data, 36-byte allocation.
        let mut data = [0u8; 36];
        let cb = [0x12, 0, 0, 0, 36, 0];
        let outcome = self.command(lun, &cb, DataPhase::In(&mut data), CommandKind::Query)?;
        if !outcome.passed {
            return Err(Errno::NotImplemented);
        }
        if outcome.transferred < 2 {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(Inquiry {
            device_type: data[0] & 0x1F,
            removable: data[1] & 0x80 != 0,
        })
    }

    /// TEST UNIT READY: whether the LUN can accept media commands now, and
    /// if not, why ([`Readiness::of_sense`]).
    ///
    /// The sense of a refusal is read (and so cleared device-side) with the
    /// answer. A unit whose sense cannot be read is taken to be coming ready,
    /// which its ready budget bounds.
    ///
    /// # Errors
    ///
    /// A transport error.
    pub fn test_unit_ready(&mut self, lun: u8) -> Result<Readiness, Errno> {
        let cb = [0x00, 0, 0, 0, 0, 0];
        let outcome = self.command(lun, &cb, DataPhase::None, CommandKind::Query)?;
        self.readiness_of(lun, outcome)
    }

    /// START STOP UNIT with START and IMMED (SBC-4 §5.31): the initializing
    /// command a unit reporting "initializing command required" waits for.
    /// It returns at once; the unit's readiness is then tested on a timer.
    ///
    /// # Errors
    ///
    /// A transport error.
    pub fn start_unit(&mut self, lun: u8) -> Result<Readiness, Errno> {
        let cb = [0x1B, 0x01, 0, 0, 0x01, 0];
        let outcome = self.command(lun, &cb, DataPhase::None, CommandKind::Query)?;
        self.readiness_of(lun, outcome)
    }

    /// What `outcome` says of `lun`'s readiness.
    fn readiness_of(&mut self, lun: u8, outcome: CommandOutcome) -> Result<Readiness, Errno> {
        if outcome.passed {
            self.answered(lun);
            return Ok(Readiness::Ready);
        }
        match self.sense_for_failure(lun) {
            Ok(sense) => Ok(self.classify(lun, sense)),
            Err(Errno::NotImplemented | Errno::LengthOutOfRange) => Ok(Readiness::BecomingReady),
            Err(err) => Err(err),
        }
    }

    /// Learn what `lun`'s medium is and how much one command may move, from
    /// what the unit reports beyond `inquiry`: the rotation rate VPD page,
    /// else IDENTIFY DEVICE through ATA PASS-THROUGH, and the Block Limits
    /// page.
    ///
    /// Many bridges mishandle these, so a probe the unit refuses reports
    /// nothing, and one that breaks the exchange (its reset recovery already
    /// run) ends probing: each probe is sent once per attach, never retried.
    /// A UFI unit carries none of them.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] once the device is gone.
    pub fn probe(&mut self, lun: u8, inquiry: Inquiry) -> Result<UnitReport, Errno> {
        let mut report = UnitReport {
            medium: BlkMedium::of_transport(BlkTransport::Usb)
                .with_removability(BlkRemovability::from_rmb(inquiry.removable)),
            max_transfer_blocks: None,
        };
        if inquiry.is_optical() {
            // A disc spins at no rate a drive states, and the disk pages
            // mean nothing to it.
            report.medium = report.medium.with_form(BlkForm::OpticalDisc);
            return Ok(report);
        }
        if self.set == CommandSet::Ufi {
            return Ok(report);
        }
        let probed = (|| {
            let pages = self.vpd(lun, VPD_SUPPORTED)?;
            let supported = |page| pages.as_ref().is_some_and(|list| list.lists(page));
            if supported(VPD_BLOCK_LIMITS) {
                if let Some(page) = self.vpd(lun, VPD_BLOCK_LIMITS)? {
                    report.max_transfer_blocks = page.max_transfer_blocks();
                }
            }
            if supported(VPD_CHARACTERISTICS) {
                if let Some(page) = self.vpd(lun, VPD_CHARACTERISTICS)? {
                    report.medium = report.medium.with_rotation(page.rotation());
                }
            }
            if report.medium.rotation == BlkRotation::Unknown {
                if let Some(rotation) = self.identify_rotation(lun)? {
                    report.medium = report.medium.with_rotation(rotation);
                }
            }
            Ok(())
        })();
        match probed {
            Err(Errno::NotFound) => Err(Errno::NotFound),
            Ok(()) | Err(_) => Ok(report),
        }
    }

    /// One VPD page, or `None` for a page the unit refused or answered with
    /// something that is not that page.
    fn vpd(&mut self, lun: u8, page: u8) -> Result<Option<VpdPage>, Errno> {
        let mut data = [0u8; VPD_ALLOCATION as usize];
        let cb = [0x12, EVPD, page, 0, VPD_ALLOCATION, 0];
        let outcome = self.command(lun, &cb, DataPhase::In(&mut data), CommandKind::Query)?;
        if !outcome.passed {
            // Classified for the reset attention it may carry.
            let sense = self.sense_for_failure(lun)?;
            self.classify(lun, sense);
            return Ok(None);
        }
        Ok(VpdPage::parse(
            page,
            &data[..outcome.transferred.min(data.len())],
        ))
    }

    /// The rotation IDENTIFY DEVICE word 217 states, read through ATA
    /// PASS-THROUGH; `None` for a bridge that refused it or data that does
    /// not hold together.
    fn identify_rotation(&mut self, lun: u8) -> Result<Option<BlkRotation>, Errno> {
        let mut data = [0u8; IDENTIFY_LEN];
        let mut cb = [0u8; 16];
        cb[0] = ATA_PASS_THROUGH_16;
        cb[1] = ATA_PROTOCOL_PIO_IN;
        cb[2] = ATA_TRANSFER_IN_BLOCKS_BY_COUNT;
        cb[6] = 1;
        cb[14] = ATA_IDENTIFY_DEVICE;
        let outcome = self.command(lun, &cb, DataPhase::In(&mut data), CommandKind::Media)?;
        if !outcome.passed {
            // Classified for the reset attention it may carry.
            let sense = self.sense_for_failure(lun)?;
            self.classify(lun, sense);
            return Ok(None);
        }
        Ok(rotation_of_identify(
            &data[..outcome.transferred.min(IDENTIFY_LEN)],
        ))
    }

    /// REQUEST SENSE: why the previous command failed.
    ///
    /// # Errors
    ///
    /// [`Errno::NotImplemented`] if the device fails the command itself,
    /// [`Errno::LengthOutOfRange`] for a response too short to carry the
    /// key/ASC/ASCQ triple, or a transport error.
    pub fn request_sense(&mut self, lun: u8) -> Result<Sense, Errno> {
        // REQUEST SENSE (SPC-4 §6.39): fixed format, 18-byte allocation.
        let mut data = [0u8; 18];
        let cb = [0x03, 0, 0, 0, 18, 0];
        let outcome = self.command(lun, &cb, DataPhase::In(&mut data), CommandKind::Query)?;
        if !outcome.passed {
            return Err(Errno::NotImplemented);
        }
        if outcome.transferred < 14 {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(Sense {
            key: data[2] & 0x0F,
            asc: data[12],
            ascq: data[13],
        })
    }

    /// READ CAPACITY: the LUN's block geometry, via `READ CAPACITY(10)`
    /// and — for a unit past the 32-bit LBA horizon — `READ CAPACITY(16)`
    /// (SBC-3 §5.15/§5.16).
    ///
    /// Every device-supplied field is validated: the block size must be a
    /// power of two in `512..=4096` (so whole blocks tile the shared data
    /// window), and the block count must be non-zero and not overflow.
    ///
    /// # Errors
    ///
    /// [`Errno::NotImplemented`] if the device fails the command,
    /// [`Errno::OutOfRange`] / [`Errno::LengthOutOfRange`] for a geometry
    /// that cannot be honest, or a transport error.
    pub fn read_capacity(&mut self, lun: u8) -> Result<BlockGeometry, Errno> {
        let mut data = [0u8; 8];
        let cb = [0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let outcome = self.command(lun, &cb, DataPhase::In(&mut data), CommandKind::Media)?;
        if !outcome.passed {
            return Err(Errno::NotImplemented);
        }
        if outcome.transferred != 8 {
            return Err(Errno::LengthOutOfRange);
        }
        let max_lba_32 = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let block_size = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let (max_lba, block_size) = if max_lba_32 == u32::MAX {
            // The unit is larger than READ CAPACITY(10) can express; ask
            // with the 16-byte form.
            let mut data = [0u8; 32];
            let mut cb = [0u8; 16];
            cb[0] = 0x9E; // SERVICE ACTION IN(16)
            cb[1] = 0x10; // READ CAPACITY(16)
            cb[10..14].copy_from_slice(&32u32.to_be_bytes());
            let outcome = self.command(lun, &cb, DataPhase::In(&mut data), CommandKind::Media)?;
            if !outcome.passed {
                return Err(Errno::NotImplemented);
            }
            if outcome.transferred < 12 {
                return Err(Errno::LengthOutOfRange);
            }
            (
                u64::from_be_bytes([
                    data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
                ]),
                u32::from_be_bytes([data[8], data[9], data[10], data[11]]),
            )
        } else {
            (u64::from(max_lba_32), block_size)
        };
        if !block_size.is_power_of_two() || !(512..=4096).contains(&block_size) {
            return Err(Errno::OutOfRange);
        }
        let block_count = max_lba.checked_add(1).ok_or(Errno::OutOfRange)?;
        Ok(BlockGeometry {
            block_size,
            block_count,
        })
    }

    /// MODE SENSE: whether the medium is write-protected (the WP bit of
    /// the mode-parameter header's device-specific byte, SBC-3 §6.4.1 /
    /// UFI 1.0 §4.10).
    ///
    /// The transparent set asks with `MODE SENSE(6)`; UFI carries only the
    /// 10-byte form, whose header stores the device-specific byte at
    /// offset 3. A device that fails the command is reported write-enabled
    /// — the established meaning of the refusal (many devices implement no
    /// mode pages); the *enforcement* of the bit stays fail-closed in
    /// [`LunBlock::write_blocks`].
    ///
    /// # Errors
    ///
    /// A transport error.
    pub fn write_protected(&mut self, lun: u8) -> Result<bool, Errno> {
        let protected = match self.set {
            CommandSet::Transparent => {
                // MODE SENSE(6), all pages, header-only allocation.
                let mut data = [0u8; 4];
                let cb = [0x1A, 0, 0x3F, 0, 4, 0];
                let outcome =
                    self.command(lun, &cb, DataPhase::In(&mut data), CommandKind::Media)?;
                outcome.passed && outcome.transferred >= 3 && data[2] & 0x80 != 0
            }
            CommandSet::Ufi | CommandSet::Atapi => {
                // MODE SENSE(10), all pages, header-only allocation.
                let mut data = [0u8; 8];
                let cb = [0x5A, 0, 0x3F, 0, 0, 0, 0, 0, 8, 0];
                let outcome =
                    self.command(lun, &cb, DataPhase::In(&mut data), CommandKind::Media)?;
                outcome.passed && outcome.transferred >= 4 && data[3] & 0x80 != 0
            }
        };
        Ok(protected)
    }

    /// What the optical drive at `lun` reports of its disc, which `READ
    /// CAPACITY` measured as `geometry`: where its last session starts (`READ
    /// TOC/PMA/ATIP` format 1), the last block it recorded (`READ DISC
    /// INFORMATION`, then `READ TRACK INFORMATION` for the last session's
    /// last track), and whether it may be rewritten in place (`GET
    /// CONFIGURATION`). A query the drive refuses, or the first whose
    /// exchange breaks, leaves what is left at what a pressed disc of one
    /// session gives: the first session, every counted block recorded, read
    /// only.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] once the device is gone; [`Errno::WouldBlock`] for
    /// a disc not yet ready and [`Errno::DeviceOffline`] for one that left or
    /// changed while it was asked, so it is read again.
    pub fn disc(&mut self, lun: u8, geometry: BlockGeometry) -> Result<Disc, Errno> {
        let counted = geometry
            .block_count
            .checked_sub(1)
            .ok_or(Errno::OutOfRange)?;
        let mut disc = Disc {
            layout: DiscLayout {
                last_session_start: 0,
                last_recorded: Some(counted),
            },
            geometry,
            random_writable: false,
        };
        let mut feature = [0u8; 16];
        // One feature, Random Writable, after the feature header.
        let [high, low] = FEATURE_RANDOM_WRITABLE.to_be_bytes();
        let cb = [0x46, 0x02, high, low, 0, 0, 0, 0, 16, 0];
        let Some(returned) = self.mmc_query(lun, &cb, &mut feature)? else {
            return Ok(disc);
        };
        disc.random_writable = returned >= 12
            && u16::from_be_bytes([feature[8], feature[9]]) == FEATURE_RANDOM_WRITABLE
            && feature[10] & 0x01 != 0;
        let mut toc = [0u8; 12];
        let cb = [0x43, 0, 0x01, 0, 0, 0, 0, 0, 12, 0];
        let Some(returned) = self.mmc_query(lun, &cb, &mut toc)? else {
            return Ok(disc);
        };
        let session_start = (returned >= 12)
            .then(|| u64::from(u32::from_be_bytes([toc[8], toc[9], toc[10], toc[11]])));
        let recorded = self.last_recorded(lun)?.unwrap_or(counted);
        disc.geometry.block_count = recorded
            .max(counted)
            .checked_add(1)
            .ok_or(Errno::OutOfRange)?;
        disc.layout = DiscLayout {
            // A session begins where the disc holds something.
            last_session_start: session_start
                .filter(|&start| start <= recorded)
                .unwrap_or(0),
            last_recorded: Some(recorded),
        };
        Ok(disc)
    }

    /// The last block the disc at `lun` recorded, from its last session's
    /// last track, or `None` when the drive cannot say.
    fn last_recorded(&mut self, lun: u8) -> Result<Option<u64>, Errno> {
        let mut info = [0u8; 12];
        let cb = [0x51, 0, 0, 0, 0, 0, 0, 0, 12, 0];
        if self
            .mmc_query(lun, &cb, &mut info)?
            .is_none_or(|returned| returned < 12)
        {
            return Ok(None);
        }
        let track = u32::from(u16::from_be_bytes([info[11], info[6]]));
        let mut data = [0u8; 32];
        let [b2, b3, b4, b5] = track.to_be_bytes();
        let cb = [0x52, 0x01, b2, b3, b4, b5, 0, 0, 32, 0];
        let Some(returned) = self
            .mmc_query(lun, &cb, &mut data)?
            .filter(|&returned| returned >= 28)
        else {
            return Ok(None);
        };
        let word = |at: usize| {
            u64::from(u32::from_be_bytes([
                data[at],
                data[at + 1],
                data[at + 2],
                data[at + 3],
            ]))
        };
        let start = word(8);
        let end = start + word(24);
        // An older drive's 28-byte answer stops short of the last recorded
        // address its flag may still claim, and one outside the track is no
        // address of it.
        let recorded =
            if data[7] & TRACK_LRA_VALID != 0 && returned >= 32 && (start..end).contains(&word(28))
            {
                Some(word(28))
            } else if data[6] & TRACK_BLANK != 0 {
                start.checked_sub(1)
            } else if data[7] & TRACK_NWA_VALID != 0 {
                // The next writable address follows the last one written.
                word(12).checked_sub(1).filter(|&last| last >= start)
            } else {
                end.checked_sub(1)
            };
        Ok(recorded)
    }

    /// Run a multimedia query `cdb` into `data`: how many bytes it returned,
    /// none for a query the drive refused, and `None` once its exchange broke
    /// (the reset recovery already run), which ends the queries. An attention
    /// the query consumed is asked past once.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] once the device is gone; for a unit not ready, or
    /// whose disc left or changed, the error a media command answers
    /// ([`readiness_errno`]), so the disc is read again rather than taken for
    /// one its drive cannot describe.
    fn mmc_query(&mut self, lun: u8, cdb: &[u8], data: &mut [u8]) -> Result<Option<usize>, Errno> {
        let mut retried = false;
        loop {
            let outcome =
                match self.command(lun, cdb, DataPhase::In(&mut *data), CommandKind::Query) {
                    Ok(outcome) => outcome,
                    Err(Errno::NotFound) => return Err(Errno::NotFound),
                    Err(_) => return Ok(None),
                };
            if outcome.passed {
                self.answered(lun);
                return Ok(Some(outcome.transferred));
            }
            let sense = match self.sense_for_failure(lun) {
                Ok(sense) => sense,
                Err(Errno::NotImplemented | Errno::LengthOutOfRange) => return Ok(Some(0)),
                Err(Errno::NotFound) => return Err(Errno::NotFound),
                Err(_) => return Ok(None),
            };
            match self.classify(lun, sense) {
                Readiness::Ready => return Ok(Some(outcome.transferred)),
                Readiness::Refused(_) => return Ok(Some(0)),
                Readiness::Attention if !retried => retried = true,
                readiness => {
                    return Err(readiness_errno(readiness).unwrap_or_else(|| sense_errno(sense)))
                }
            }
        }
    }

    /// SYNCHRONIZE CACHE(10): commit every completed write to the medium.
    ///
    /// UFI carries no such command — the floppy medium is written through —
    /// so a UFI flush succeeds without a wire round trip. For the
    /// transparent set, a unit that answers ILLEGAL REQUEST has no cache to
    /// flush (the data is already on the medium), so that refusal is
    /// success; every other failure surfaces.
    ///
    /// # Errors
    ///
    /// The sense-mapped [`Errno`] of a genuine flush failure, or a
    /// transport error.
    pub fn synchronize_cache(&mut self, lun: u8) -> Result<(), Errno> {
        if self.set == CommandSet::Ufi {
            return Ok(());
        }
        let cb = [0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        match self.media(lun, |device| {
            device.command(lun, &cb, DataPhase::None, CommandKind::Flush)
        })? {
            Ok(_) => Ok(()),
            Err(sense) if sense.key == SENSE_KEY_ILLEGAL_REQUEST => Ok(()),
            Err(sense) => Err(self.refusal(lun, sense)),
        }
    }

    /// Run a media command (`run` issues it), once more after a unit
    /// attention it consumed, answering its outcome or the sense of its
    /// refusal.
    fn media(
        &mut self,
        lun: u8,
        mut run: impl FnMut(&mut Self) -> Result<CommandOutcome, Errno>,
    ) -> Result<Result<CommandOutcome, Sense>, Errno> {
        let mut retried = false;
        loop {
            let outcome = match run(self) {
                Ok(outcome) => outcome,
                Err(err) => return Err(self.after_broken_exchange(lun, err)),
            };
            if outcome.passed {
                self.answered(lun);
                return Ok(Ok(outcome));
            }
            let sense = self.sense_for_failure(lun)?;
            match self.classify(lun, sense) {
                Readiness::Ready => {
                    return Ok(Ok(CommandOutcome {
                        passed: true,
                        ..outcome
                    }));
                }
                Readiness::Attention if !retried => retried = true,
                Readiness::MediumChanged => {
                    self.record_readiness(lun, Readiness::MediumChanged);
                    return Ok(Err(sense));
                }
                _ => return Ok(Err(sense)),
            }
        }
    }

    /// What a media command whose exchange broke on `err` answers, once the
    /// unit has said whether the reset its transport ran left it ready:
    /// answering ready, its request may come back at once; coming ready, it
    /// is held; without its medium, the unit is retracted. A reset that did
    /// not put the transport back in step leaves nothing to ask.
    fn after_broken_exchange(&mut self, lun: u8, err: Errno) -> Errno {
        if !self.resynced {
            return err;
        }
        match self.test_unit_ready(lun) {
            Ok(Readiness::Ready | Readiness::Attention) => {
                if let Some(slot) = self.reissue.get_mut(usize::from(lun)) {
                    *slot = BlkReissue::Now;
                }
                err
            }
            Ok(readiness) => self.record_readiness(lun, readiness).unwrap_or(err),
            Err(_) => err,
        }
    }

    /// The error a media command refused with `sense` answers.
    fn refusal(&mut self, lun: u8, sense: Sense) -> Errno {
        let readiness = self.classify(lun, sense);
        self.record_readiness(lun, readiness)
            .unwrap_or_else(|| sense_errno(sense))
    }

    /// Record `readiness` for the serve loop when it acts on it, answering
    /// the error that tells the request so ([`readiness_errno`]).
    fn record_readiness(&mut self, lun: u8, readiness: Readiness) -> Option<Errno> {
        let errno = readiness_errno(readiness)?;
        if let Some(slot) = self.readiness.get_mut(usize::from(lun)) {
            *slot = Some(readiness);
        }
        Some(errno)
    }

    /// Read `buf.len()` bytes (a whole number of `block_size`-byte
    /// blocks) starting at `lba`. The caller ([`LunBlock`]) has validated
    /// the range and chunked the transfer; a short delivery is a failure,
    /// never a silent partial read.
    ///
    /// # Errors
    ///
    /// The sense-mapped [`Errno`] of a failed read, `NotImplemented` for a
    /// short delivery the device called a success, or a transport error.
    pub fn read(
        &mut self,
        lun: u8,
        lba: u64,
        block_size: u32,
        buf: &mut [u8],
    ) -> Result<(), Errno> {
        let blocks = blocks_spanned(lba, buf.len(), block_size)?;
        let cb = rw_command(0x28, 0x88, lba, blocks);
        let expected = buf.len();
        let outcome = match self.media(lun, |device| {
            device.command(
                lun,
                cb.bytes(),
                DataPhase::In(&mut *buf),
                CommandKind::Media,
            )
        })? {
            Ok(outcome) => outcome,
            Err(sense) => return Err(self.refusal(lun, sense)),
        };
        if outcome.transferred != expected {
            return Err(Errno::NotImplemented);
        }
        Ok(())
    }

    /// Write `buf` (a whole number of blocks) starting at `lba`. The
    /// write-protect policy is enforced by [`LunBlock`] before this is
    /// reached; a device-side DATA PROTECT still surfaces as
    /// [`Errno::PermissionDenied`].
    ///
    /// # Errors
    ///
    /// As [`Self::read`].
    pub fn write(&mut self, lun: u8, lba: u64, block_size: u32, buf: &[u8]) -> Result<(), Errno> {
        let blocks = blocks_spanned(lba, buf.len(), block_size)?;
        let cb = rw_command(0x2A, 0x8A, lba, blocks);
        let expected = buf.len();
        let outcome = match self.media(lun, |device| {
            device.command(lun, cb.bytes(), DataPhase::Out(buf), CommandKind::Media)
        })? {
            Ok(outcome) => outcome,
            Err(sense) => return Err(self.refusal(lun, sense)),
        };
        if outcome.transferred != expected {
            return Err(Errno::NotImplemented);
        }
        Ok(())
    }

    /// Zero the transport's shared data window (sensitive-payload hygiene).
    pub fn scrub_window(&mut self) {
        self.transport.scrub();
    }

    /// Reset the transport ([`ScsiTransport::reset`]): the data-path reset a
    /// recovering unit's ladder escalates to.
    ///
    /// # Errors
    ///
    /// The transport's or device's refusal.
    pub fn reset_transport(&mut self) -> Result<(), Errno> {
        self.expecting_reset = [true; MAX_LUNS];
        self.transport.reset()
    }

    /// Borrow the underlying transport, so the serve loop can observe
    /// transport-level state (e.g. that the interface disappeared and the
    /// per-LUN nodes must be retracted).
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }
}

/// The SCSI transfer length (in logical blocks of the unit's own block
/// size) a byte length spans, bounded by the per-command ceiling
/// ([`MSD_MAX_TRANSFER_LEN`]) and re-validated block-aligned so a caller
/// bug cannot issue a torn command.
fn blocks_spanned(lba: u64, len: usize, block_size: u32) -> Result<u32, Errno> {
    let block_size = usize::try_from(block_size).map_err(|_| Errno::OutOfRange)?;
    if block_size == 0 || len == 0 || len > MSD_MAX_TRANSFER_LEN || !len.is_multiple_of(block_size)
    {
        return Err(Errno::LengthOutOfRange);
    }
    let blocks = u32::try_from(len / block_size).map_err(|_| Errno::LengthOutOfRange)?;
    // Guard the end-of-range sum here too, so a caller bug cannot wrap.
    lba.checked_add(u64::from(blocks))
        .ok_or(Errno::LengthOutOfRange)?;
    Ok(blocks)
}

/// A READ/WRITE command block: the 10-byte form when the range fits it,
/// else the 16-byte form (SBC-3 §5.11/§5.13).
struct RwCommand {
    bytes: [u8; 16],
    len: usize,
}

impl RwCommand {
    fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Build the READ/WRITE command block for `lba`/`blocks` — `blocks` is the
/// SCSI transfer length in logical blocks, computed by
/// [`ScsiDevice::read`]/[`ScsiDevice::write`] from the byte length and the
/// unit's block size.
fn rw_command(op10: u8, op16: u8, lba: u64, blocks: u32) -> RwCommand {
    let mut bytes = [0u8; 16];
    if let (Ok(lba32), Ok(blocks16)) = (u32::try_from(lba), u16::try_from(blocks)) {
        bytes[0] = op10;
        bytes[2..6].copy_from_slice(&lba32.to_be_bytes());
        bytes[7..9].copy_from_slice(&blocks16.to_be_bytes());
        return RwCommand { bytes, len: 10 };
    }
    bytes[0] = op16;
    bytes[2..10].copy_from_slice(&lba.to_be_bytes());
    bytes[10..14].copy_from_slice(&blocks.to_be_bytes());
    RwCommand { bytes, len: 16 }
}

/// Map fixed-format sense data onto the [`Errno`] a failed media command
/// surfaces: write-protection is a permission refusal, a not-ready or
/// attention state is distinct from a hard medium fault.
fn sense_errno(sense: Sense) -> Errno {
    match sense.key {
        SENSE_KEY_DATA_PROTECT => Errno::PermissionDenied,
        // One request's blocks, not the unit: a scratch on a disc, a bad
        // sector, an unwritten stretch of a write-once disc, or the drive's
        // hardware failing to reach them.
        SENSE_KEY_MEDIUM_ERROR | SENSE_KEY_BLANK_CHECK | SENSE_KEY_HARDWARE_ERROR => {
            Errno::MediumError
        }
        SENSE_KEY_NO_SENSE
        | SENSE_KEY_NOT_READY
        | SENSE_KEY_UNIT_ATTENTION
        | SENSE_KEY_ABORTED_COMMAND => Errno::WouldBlock,
        SENSE_KEY_ILLEGAL_REQUEST => Errno::OutOfRange,
        _ => Errno::NotImplemented,
    }
}

/// The error a command answers for a unit whose `readiness` the serve loop
/// acts on: one coming ready has its request held, one without its medium is
/// retracted.
const fn readiness_errno(readiness: Readiness) -> Option<Errno> {
    match readiness {
        Readiness::BecomingReady | Readiness::NeedsStart => Some(Errno::WouldBlock),
        Readiness::NoMedium | Readiness::MediumChanged => Some(Errno::DeviceOffline),
        _ => None,
    }
}

/// Map a transport/engine [`Errno`] onto the [`DriverError`] the
/// [`Block`] surface reports.
fn driver_error(err: Errno) -> DriverError {
    match err {
        Errno::PermissionDenied => DriverError::PermissionDenied,
        Errno::LengthOutOfRange => DriverError::LengthOutOfRange,
        Errno::OutOfRange => DriverError::OutOfRange,
        Errno::BufferTooSmall => DriverError::BufferTooSmall,
        Errno::EndpointStalled => DriverError::EndpointStalled,
        Errno::NotFound => DriverError::NotFound,
        Errno::WouldBlock => DriverError::Busy,
        Errno::DeviceOffline => DriverError::DeviceOffline,
        Errno::TimedOut => DriverError::TimedOut,
        Errno::BadMagic => DriverError::BadMagic,
        Errno::MediumError => DriverError::MediumError,
        _ => DriverError::DeviceFault,
    }
}

/// What a command a transport reset ended answers its caller, given how the
/// reset went. The command never completed either way: a transport back in
/// step, or a reset the host controller could not take yet, makes it the
/// reissuable [`Errno::WouldBlock`]; a device gone is [`Errno::NotFound`]; one
/// that would not answer its own reset is [`Errno::TimedOut`], which the
/// unit's health rides out before failing it.
pub(crate) const fn after_reset(cause: Errno, reset: Result<(), Errno>) -> Errno {
    match (cause, reset) {
        (Errno::NotFound, _) | (_, Err(Errno::NotFound)) => Errno::NotFound,
        (_, Ok(()) | Err(Errno::WouldBlock)) => Errno::WouldBlock,
        (_, Err(_)) => Errno::TimedOut,
    }
}

/// What a transport knows of the reset its last broken exchange ran: whether
/// it put the transport back in step, which [`after_reset`]'s answer alone
/// does not tell from a reset the host controller deferred.
#[derive(Debug, Default)]
pub(crate) struct ResetRecord {
    resynced: bool,
}

impl ResetRecord {
    /// An exchange is beginning: no reset has run for it.
    pub(crate) fn begin(&mut self) {
        self.resynced = false;
    }

    /// What the exchange that broke on `cause` answers once its `reset`
    /// recovery ran ([`after_reset`]), noting whether the reset resynced the
    /// transport.
    pub(crate) fn after(&mut self, cause: Errno, reset: Result<(), Errno>) -> Errno {
        self.resynced = reset.is_ok();
        after_reset(cause, reset)
    }

    /// Whether the last broken exchange's reset resynced the transport,
    /// taken.
    pub(crate) fn take(&mut self) -> bool {
        core::mem::take(&mut self.resynced)
    }
}

/// The command budget of a USB unit nothing is yet known of: the slowest
/// medium's, until a probe says otherwise.
pub(crate) fn unknown_unit_budget() -> CommandBudget {
    BlkMedium::of_transport(BlkTransport::Usb)
        .class()
        .budget()
        .command
}

/// One VPD page's body, validated as the page asked for.
struct VpdPage {
    bytes: [u8; VPD_ALLOCATION as usize],
    len: usize,
}

impl VpdPage {
    /// `bytes` as page `page`, or `None` when the unit answered something
    /// else: a different page, a length the transfer does not hold, or (for
    /// the supported-pages list) a list that does not begin with itself, the
    /// mark of a unit that ignored EVPD and sent its standard INQUIRY data.
    fn parse(page: u8, bytes: &[u8]) -> Option<Self> {
        let header = bytes.get(..4)?;
        if header[1] != page {
            return None;
        }
        let len = usize::from(u16::from_be_bytes([header[2], header[3]]));
        let body = bytes.get(4..4 + len.min(bytes.len() - 4))?;
        if page == VPD_SUPPORTED && body.first() != Some(&VPD_SUPPORTED) {
            return None;
        }
        let mut page = Self {
            bytes: [0; VPD_ALLOCATION as usize],
            len: body.len(),
        };
        page.bytes[..body.len()].copy_from_slice(body);
        Some(page)
    }

    fn body(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Whether the supported-pages list names `page`.
    fn lists(&self, page: u8) -> bool {
        self.body().contains(&page)
    }

    /// The Block Limits page's MAXIMUM TRANSFER LENGTH, `None` when it states
    /// no limit.
    fn max_transfer_blocks(&self) -> Option<NonZeroU32> {
        let field = self.body().get(4..8)?;
        NonZeroU32::new(u32::from_be_bytes([field[0], field[1], field[2], field[3]]))
    }

    /// The Block Device Characteristics page's MEDIUM ROTATION RATE.
    fn rotation(&self) -> BlkRotation {
        let Some(field) = self.body().get(..2) else {
            return BlkRotation::Unknown;
        };
        BlkRotation::from_rate_field(
            u16::from_be_bytes([field[0], field[1]]),
            BlkRotationEvidence::RotationRateVpd,
        )
    }
}

/// The rotation IDENTIFY DEVICE `data` states in word 217, or `None` for data
/// that is not a whole ATA device's IDENTIFY or whose integrity word fails.
fn rotation_of_identify(data: &[u8]) -> Option<BlkRotation> {
    if data.len() != IDENTIFY_LEN || data[1] & 0x80 != 0 {
        return None;
    }
    if data[IDENTIFY_LEN - 2] == IDENTIFY_CHECKSUM_SIGNATURE
        && data.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)) != 0
    {
        return None;
    }
    let rate = u16::from_le_bytes([
        data[IDENTIFY_ROTATION_RATE],
        data[IDENTIFY_ROTATION_RATE + 1],
    ]);
    match BlkRotation::from_rate_field(rate, BlkRotationEvidence::AtaIdentify) {
        BlkRotation::Unknown => None,
        rotation => Some(rotation),
    }
}

/// One logical unit exposed as a [`Block`] device: the per-LUN view the
/// block service serves (`plans/DEVICES.md` D2 target shape).
pub struct LunBlock<'a, T: ScsiTransport> {
    device: &'a mut ScsiDevice<T>,
    lun: u8,
    state: LunState,
}

impl<'a, T: ScsiTransport> LunBlock<'a, T> {
    /// A [`Block`] view of `lun` with its brought-up `state`.
    pub fn new(device: &'a mut ScsiDevice<T>, lun: u8, state: LunState) -> Self {
        Self { device, lun, state }
    }

    /// Whether the medium is write-protected (the serve loop reports it
    /// on the geometry reply).
    #[must_use]
    pub fn write_protected(&self) -> bool {
        self.state.write_protected
    }

    /// The most bytes one command moves: the data window, or less where the
    /// unit's Block Limits state less.
    fn chunk_len(&self) -> usize {
        let block_size = self.state.geometry.block_size as usize;
        self.state
            .report
            .max_transfer_blocks
            .and_then(|blocks| usize::try_from(blocks.get()).ok())
            .map_or(MSD_MAX_TRANSFER_LEN, |blocks| {
                blocks.saturating_mul(block_size).min(MSD_MAX_TRANSFER_LEN)
            })
    }

    /// The [`DriverError`] a failed command answers: a unit coming ready is
    /// [`DriverError::NotReady`], held rather than answered.
    fn error(&self, err: Errno) -> DriverError {
        let coming_ready = matches!(
            self.device.readiness(self.lun),
            Some(Readiness::BecomingReady | Readiness::NeedsStart)
        );
        if err == Errno::WouldBlock && coming_ready {
            DriverError::NotReady
        } else {
            driver_error(err)
        }
    }

    /// Validate one block operation against the unit's geometry: the
    /// buffer must be a non-empty whole number of blocks and the range
    /// must lie inside the unit (mirrors the virtio-blk validation).
    fn validate(&self, lba: u64, len: usize) -> Result<(), DriverError> {
        let block_size = self.state.geometry.block_size as usize;
        if len == 0 || !len.is_multiple_of(block_size) {
            return Err(DriverError::BufferTooSmall);
        }
        let blocks = u64::try_from(len / block_size).map_err(|_| DriverError::LengthOutOfRange)?;
        let end = lba
            .checked_add(blocks)
            .ok_or(DriverError::LengthOutOfRange)?;
        if end > self.state.geometry.block_count {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok(())
    }
}

impl<T: ScsiTransport> Block for LunBlock<'_, T> {
    /// What the unit reported of its medium ([`ScsiDevice::probe`]).
    fn medium(&self) -> Option<BlkMedium> {
        Some(self.state.report.medium)
    }

    /// What an optical drive reported of its disc ([`ScsiDevice::disc`]).
    fn disc_layout(&self) -> Option<DiscLayout> {
        self.state.disc_layout
    }

    /// What the unit said once its transport's reset had run
    /// ([`ScsiDevice::take_reissue`]).
    fn reissue(&mut self) -> BlkReissue {
        self.device.take_reissue(self.lun)
    }

    /// A USB mass-storage logical unit, named by what it is: the standard
    /// INQUIRY this driver issues reports the unit's type and removability,
    /// not an identity that would distinguish one stick from another.
    fn device_name(&self) -> BlkDeviceName {
        BlkDeviceName::new("usb-msd")
    }

    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(self.state.geometry)
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        self.read_blocks_with_class(lba, buf, BufferClass::NonSensitive)
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.write_blocks_with_class(lba, buf, BufferClass::NonSensitive)
    }

    fn read_blocks_with_class(
        &mut self,
        lba: u64,
        buf: &mut [u8],
        class: BufferClass,
    ) -> Result<(), DriverError> {
        self.validate(lba, buf.len())?;
        let block_size = self.state.geometry.block_size as usize;
        let chunk_len = self.chunk_len();
        // Split into bounded per-command chunks so the device cost is
        // fixed, never a function of the request length.
        let result = (|| {
            let mut off = 0usize;
            let mut cur_lba = lba;
            while off < buf.len() {
                let chunk = (buf.len() - off).min(chunk_len);
                let read = self.device.read(
                    self.lun,
                    cur_lba,
                    self.state.geometry.block_size,
                    &mut buf[off..off + chunk],
                );
                read.map_err(|err| self.error(err))?;
                cur_lba += (chunk / block_size) as u64;
                off += chunk;
            }
            Ok(())
        })();
        // A sensitive payload never outlives the call in the shared
        // window, success or failure.
        if class == BufferClass::Sensitive {
            self.device.scrub_window();
        }
        result
    }

    fn write_blocks_with_class(
        &mut self,
        lba: u64,
        buf: &[u8],
        class: BufferClass,
    ) -> Result<(), DriverError> {
        // Enforce the write-protect policy driver-side, before any byte
        // reaches the device (the device's own DATA PROTECT would also
        // refuse, but the policy never depends on it).
        if self.state.write_protected {
            return Err(DriverError::PermissionDenied);
        }
        self.validate(lba, buf.len())?;
        let block_size = self.state.geometry.block_size as usize;
        let chunk_len = self.chunk_len();
        let result = (|| {
            let mut off = 0usize;
            let mut cur_lba = lba;
            while off < buf.len() {
                let chunk = (buf.len() - off).min(chunk_len);
                let written = self.device.write(
                    self.lun,
                    cur_lba,
                    self.state.geometry.block_size,
                    &buf[off..off + chunk],
                );
                written.map_err(|err| self.error(err))?;
                cur_lba += (chunk / block_size) as u64;
                off += chunk;
            }
            Ok(())
        })();
        if class == BufferClass::Sensitive {
            self.device.scrub_window();
        }
        result
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        // Commit the device's volatile write cache via SCSI SYNCHRONIZE
        // CACHE (a no-op on a UFI unit that has none). The block service
        // drives this for `BlkOp::Flush`.
        let flushed = self.device.synchronize_cache(self.lun);
        flushed.map_err(|err| self.error(err))
    }
}

#[cfg(test)]
#[path = "scsi_tests.rs"]
mod tests;
