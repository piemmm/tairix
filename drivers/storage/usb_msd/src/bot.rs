//! The Bulk-Only Transport (BOT) wire transport (`plans/DEVICES.md` D2/D5).
//!
//! USB mass-storage "bulk-only" devices (USB Mass Storage Class Bulk-Only
//! Transport 1.0) wrap every SCSI command in a 31-byte Command Block
//! Wrapper (CBW) on the bulk-OUT endpoint, move the data phase over the
//! bulk pair, and answer with a 13-byte Command Status Wrapper (CSW) on
//! bulk-IN. [`Bot`] is that framing as a [`ScsiTransport`]: the shared
//! SCSI command layer ([`crate::scsi`]) drives it, generic over the
//! [`MsdTransport`] seam so it is proven host-side against a scripted
//! device and runs unchanged over the URB transport in the `Run` binary.
//!
//! Every device-supplied field — CSW signature/tag/residue/status, the LUN
//! count — is bounds-checked fail-closed: the device is hostile input. A
//! transfer that fails or outruns the exchange, a tag mismatch, a corrupt
//! CSW, or a phase error runs the spec's reset recovery and fails the
//! command rather than trusting the frame.

use tairix_abi::Errno;

use crate::scsi::{after_reset, CommandOutcome, DataPhase, ScsiTransport, Sense, MAX_LUNS};

/// One logical USB transfer seam the wire transports drive.
///
/// The `Run` binary implements it over the URB transport client (splitting
/// a transfer into per-URB chunks and bounce-copying through the shared
/// buffer); a host test implements it as a scripted device. A bulk transfer
/// moves *up to* `data.len()` bytes and reports the honest count (a short
/// bulk-IN ends the data phase early); a halted endpoint surfaces as
/// [`Errno::EndpointStalled`] with the endpoint already recovered below,
/// so the engine may transfer again immediately. BOT uses the control and
/// bulk operations; CBI ([`crate::cbi`]) additionally uses the control-OUT
/// data stage (its command channel) and the interrupt-IN endpoint (its
/// status channel). Every bulk and interrupt transfer is bounded by the
/// exchange [`begin_exchange`](Self::begin_exchange) last started, failing
/// [`Errno::TimedOut`] once it is spent.
pub trait MsdTransport {
    /// Run a control-IN transfer (SETUP + IN data stage) into `data`,
    /// returning the bytes the device delivered.
    ///
    /// # Errors
    ///
    /// An [`Errno`] from the transport/device.
    fn control_in(&mut self, setup: [u8; 8], data: &mut [u8]) -> Result<usize, Errno>;

    /// Run a control-OUT transfer (SETUP + OUT data stage carrying `data`).
    ///
    /// # Errors
    ///
    /// [`Errno::EndpointStalled`] when the device refused the request with
    /// a protocol STALL (the control endpoint recovers on the next SETUP),
    /// or any other transport [`Errno`].
    fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<(), Errno>;

    /// Run a no-data control transfer (SETUP + status stage only).
    ///
    /// # Errors
    ///
    /// An [`Errno`] from the transport/device.
    fn control_no_data(&mut self, setup: [u8; 8]) -> Result<(), Errno>;

    /// Read up to `data.len()` bytes from the bulk-IN endpoint, returning
    /// the bytes delivered (a short transfer ends the phase early).
    ///
    /// # Errors
    ///
    /// [`Errno::EndpointStalled`] for a device STALL (already recovered),
    /// or any other transport [`Errno`].
    fn bulk_in(&mut self, data: &mut [u8]) -> Result<usize, Errno>;

    /// Write `data` to the bulk-OUT endpoint, returning the bytes the
    /// device accepted.
    ///
    /// # Errors
    ///
    /// As [`Self::bulk_in`].
    fn bulk_out(&mut self, data: &[u8]) -> Result<usize, Errno>;

    /// Read one report from the interface's interrupt-IN endpoint into
    /// `data`, returning the bytes delivered — the CBI command-completion
    /// channel. The call blocks until the device raises the interrupt.
    ///
    /// # Errors
    ///
    /// An [`Errno`] from the transport/device.
    fn interrupt_in(&mut self, data: &mut [u8]) -> Result<usize, Errno>;

    /// Drop every interrupt the interface has buffered and not yet read, so
    /// the next [`Self::interrupt_in`] is answered by one the device raises
    /// after this. A CBI completion carries no tag, so one an earlier command
    /// raised late is otherwise indistinguishable from this command's own.
    ///
    /// # Errors
    ///
    /// An [`Errno`] from the transport.
    fn discard_interrupts(&mut self) -> Result<(), Errno>;

    /// Start one command exchange: every transfer until the next begins is
    /// bounded by what remains of the exchange's budget, so a device that
    /// stops answering mid-command fails the command rather than holding it.
    fn begin_exchange(&mut self);

    /// Clear the halt of both bulk endpoints — bulk-IN, then bulk-OUT — on
    /// the device and in the controller, so both pipes restart empty at
    /// `DATA0`: the clears a BOT reset recovery and a CBI Command Block Reset
    /// owe (BOT 1.0 §5.3.4, CBI 1.1 §2.2).
    ///
    /// # Errors
    ///
    /// An [`Errno`] from the transport/device.
    fn clear_halts(&mut self) -> Result<(), Errno>;

    /// Zero every byte of the transport's shared data window, so a
    /// sensitive payload does not outlive the block operation that moved
    /// it (the zero-on-free discipline applied to the bounce window).
    fn scrub(&mut self);
}

/// CBW length (USB MSC BOT 1.0 §5.1).
pub const CBW_LEN: usize = 31;
/// CBW signature `USBC` (little-endian).
const CBW_SIGNATURE: u32 = 0x4342_5355;
/// CBW flags: data phase from device to host.
const CBW_FLAGS_IN: u8 = 0x80;
/// CSW length (BOT §5.2).
pub const CSW_LEN: usize = 13;
/// CSW signature `USBS` (little-endian).
const CSW_SIGNATURE: u32 = 0x5342_5355;
/// CSW status: command passed.
const CSW_STATUS_PASSED: u8 = 0;
/// CSW status: command failed (sense data pending).
const CSW_STATUS_FAILED: u8 = 1;
/// CSW status: phase error — the transport is out of step; reset it.
const CSW_STATUS_PHASE_ERROR: u8 = 2;

/// One bulk-only mass-storage device: the BOT framing over a transport.
pub struct Bot<T: MsdTransport> {
    transport: T,
    /// `bInterfaceNumber` — the `wIndex` of the class requests.
    interface_number: u8,
    /// The next CBW tag; incremented per command so a stale CSW can never
    /// match the command in flight.
    next_tag: u32,
}

impl<T: MsdTransport> Bot<T> {
    /// Wrap a transport for the mass-storage interface `interface_number`.
    pub fn new(transport: T, interface_number: u8) -> Self {
        Self {
            transport,
            interface_number,
            next_tag: 1,
        }
    }

    /// Borrow the underlying transport (the serve loop observes
    /// transport-level state through it).
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// The spec's reset recovery (BOT 1.0 §5.3.4): a Bulk-Only Mass Storage
    /// Reset, then the halt of each bulk pipe cleared. The reset alone leaves
    /// both pipes' halts and data toggles as they were, so the next CBW would
    /// still meet a pipe out of step.
    fn reset_recovery(&mut self) -> Result<(), Errno> {
        // Class-specific OUT, bRequest 0xFF, no data.
        let setup = [0x21, 0xFF, 0, 0, self.interface_number, 0, 0, 0];
        self.transport.control_no_data(setup)?;
        self.transport.clear_halts()
    }

    /// End an exchange that broke on `cause` with the reset recovery it
    /// owes, answering what the caller may do about it.
    fn fail(&mut self, cause: Errno) -> Errno {
        if cause == Errno::NotFound {
            return cause;
        }
        after_reset(cause, self.reset_recovery())
    }
}

impl<T: MsdTransport> ScsiTransport for Bot<T> {
    /// Run one BOT command: CBW, data phase, CSW, with the spec's
    /// stall/retry/reset handling and every device field validated.
    fn execute(
        &mut self,
        lun: u8,
        cb: &[u8],
        mut data: DataPhase<'_>,
    ) -> Result<CommandOutcome, Errno> {
        if cb.is_empty() || cb.len() > 16 || lun as usize >= MAX_LUNS {
            return Err(Errno::OutOfRange);
        }
        let data_len = u32::try_from(data.len()).map_err(|_| Errno::LengthOutOfRange)?;
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1);
        self.transport.begin_exchange();

        // Build and send the CBW. Any failure here, a STALL included, means
        // the device did not take the wrapper: reset the transport and fail.
        let mut cbw = [0u8; CBW_LEN];
        cbw[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&data_len.to_le_bytes());
        cbw[12] = if data.is_in() { CBW_FLAGS_IN } else { 0 };
        cbw[13] = lun;
        cbw[14] = u8::try_from(cb.len()).map_err(|_| Errno::OutOfRange)?;
        cbw[15..15 + cb.len()].copy_from_slice(cb);
        match self.transport.bulk_out(&cbw) {
            Ok(CBW_LEN) => {}
            Ok(_) => return Err(self.fail(Errno::NotImplemented)),
            Err(err) => return Err(self.fail(err)),
        }

        // Data phase. A STALL ends the phase early (the device will report
        // the residue in the CSW); any other failure left the device
        // mid-command, out of step with the host.
        let mut transferred = 0usize;
        let moved = match &mut data {
            DataPhase::In(buf) if !buf.is_empty() => Some(self.transport.bulk_in(buf)),
            DataPhase::Out(buf) if !buf.is_empty() => Some(self.transport.bulk_out(buf)),
            _ => None,
        };
        match moved {
            Some(Ok(n)) => transferred = n,
            Some(Err(Errno::EndpointStalled)) | None => {}
            Some(Err(err)) => return Err(self.fail(err)),
        }

        // CSW, retried once across a STALL (BOT §6.7.2 figure 2), then
        // validated field by field before anything is believed.
        let mut csw = [0u8; CSW_LEN];
        let received = match self.transport.bulk_in(&mut csw) {
            Err(Errno::EndpointStalled) => self.transport.bulk_in(&mut csw),
            first => first,
        }
        .map_err(|err| self.fail(err))?;
        if received != CSW_LEN {
            return Err(self.fail(Errno::LengthOutOfRange));
        }
        let signature = u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]);
        let csw_tag = u32::from_le_bytes([csw[4], csw[5], csw[6], csw[7]]);
        let residue = u32::from_le_bytes([csw[8], csw[9], csw[10], csw[11]]);
        let status = csw[12];
        if signature != CSW_SIGNATURE || csw_tag != tag || residue > data_len {
            return Err(self.fail(Errno::BadMagic));
        }
        // The device's honest data count: never more than the bytes that
        // actually moved, never more than the CSW says remained unmoved.
        let reported = (data_len - residue) as usize;
        let transferred = transferred.min(reported);
        match status {
            CSW_STATUS_PASSED => Ok(CommandOutcome {
                passed: true,
                transferred,
            }),
            CSW_STATUS_FAILED => Ok(CommandOutcome {
                passed: false,
                transferred,
            }),
            CSW_STATUS_PHASE_ERROR => Err(self.fail(Errno::NotImplemented)),
            _ => Err(self.fail(Errno::BadMagic)),
        }
    }

    fn reset(&mut self) -> Result<(), Errno> {
        self.reset_recovery()
    }

    /// Number of logical units the device exposes (`GET MAX LUN + 1`).
    ///
    /// A device that refuses the request (many single-LUN sticks STALL it,
    /// BOT §3.2) reports one LUN — the spec's meaning of the refusal. A
    /// delivered value above 15 is a protocol violation and fails closed.
    fn lun_count(&mut self) -> Result<u8, Errno> {
        // GET MAX LUN: class-specific IN, bRequest 0xFE, one data byte.
        let setup = [0xA1, 0xFE, 0, 0, self.interface_number, 0, 1, 0];
        let mut data = [0u8; 1];
        match self.transport.control_in(setup, &mut data) {
            Ok(1) => {
                if data[0] as usize >= MAX_LUNS {
                    return Err(Errno::OutOfRange);
                }
                Ok(data[0] + 1)
            }
            // A refusal (or a zero-byte answer) means "no multi-LUN
            // support": exactly one LUN, per the BOT spec.
            Ok(_) | Err(_) => Ok(1),
        }
    }

    /// BOT delivers no in-band sense; the command layer issues
    /// `REQUEST SENSE`.
    fn take_sense(&mut self) -> Option<Sense> {
        None
    }

    fn scrub(&mut self) {
        self.transport.scrub();
    }
}

#[cfg(test)]
#[path = "bot_tests.rs"]
mod tests;
