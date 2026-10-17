//! Shared scripted USB device double behind the [`MsdTransport`] seam,
//! driven by the BOT and CBI wire-transport tests.

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::Errno;

use crate::bot::{MsdTransport, CSW_LEN};

/// One scripted answer to a bulk-IN transfer.
pub(crate) enum InStep {
    /// Deliver these bytes (short if fewer than requested).
    Data(Vec<u8>),
    /// STALL the transfer (the transport reports the endpoint recovered).
    Stall,
    /// Fail the transfer other than by a STALL: a wire fault, or a deadline
    /// the controller abandoned it at.
    Fail(Errno),
}

/// A scripted mass-storage device behind the [`MsdTransport`] seam.
pub(crate) struct ScriptedDevice {
    /// Queued bulk-IN answers, consumed one per transfer.
    pub in_steps: VecDeque<InStep>,
    /// Every bulk-OUT payload the device accepted (CBWs and data).
    pub out_frames: Vec<Vec<u8>>,
    /// Whether the next bulk-OUT STALLs (consumed once).
    pub stall_next_out: bool,
    /// Recorded no-data control SETUPs (the BOT reset trail).
    pub resets: Vec<[u8; 8]>,
    /// Scripted `GET MAX LUN` answer; `None` STALLs the request.
    pub max_lun: Option<u8>,
    /// Window-scrub call count.
    pub scrubs: usize,
    /// Recorded control-OUT transfers `(setup, data)` — the CBI ADSC
    /// command channel (including the Command Block Reset).
    pub control_outs: Vec<([u8; 8], Vec<u8>)>,
    /// Whether the next control-OUT STALLs (consumed once) — the CBI
    /// "command not accepted" answer.
    pub stall_next_control_out: bool,
    /// Queued interrupt-IN answers — the CBI completion blocks.
    pub interrupts: VecDeque<Vec<u8>>,
    /// Interrupts captured before the command began — a late answer to an
    /// earlier one — read ahead of [`Self::interrupts`] unless discarded.
    pub stale_interrupts: VecDeque<Vec<u8>>,
    /// How the next bulk-OUT fails other than by a STALL (consumed once).
    pub fail_next_out: Option<Errno>,
    /// How the next no-data control request (the BOT reset) fails, if it does.
    pub reset_failure: Option<Errno>,
    /// The recovery steps in the order they ran: `"reset"` for the class
    /// reset, `"clear"` for the bulk clears.
    pub trail: Vec<&'static str>,
    /// Exchanges begun.
    pub exchanges: usize,
}

impl ScriptedDevice {
    pub fn new() -> Self {
        Self {
            in_steps: VecDeque::new(),
            out_frames: Vec::new(),
            stall_next_out: false,
            resets: Vec::new(),
            max_lun: Some(0),
            scrubs: 0,
            control_outs: Vec::new(),
            stall_next_control_out: false,
            interrupts: VecDeque::new(),
            stale_interrupts: VecDeque::new(),
            fail_next_out: None,
            reset_failure: None,
            trail: Vec::new(),
            exchanges: 0,
        }
    }

    /// Queue a CSW for `tag` with `status` and `residue`.
    pub fn queue_csw(&mut self, tag: u32, status: u8, residue: u32) {
        let mut csw = vec![0u8; CSW_LEN];
        csw[0..4].copy_from_slice(&0x5342_5355u32.to_le_bytes());
        csw[4..8].copy_from_slice(&tag.to_le_bytes());
        csw[8..12].copy_from_slice(&residue.to_le_bytes());
        csw[12] = status;
        self.in_steps.push_back(InStep::Data(csw));
    }
}

impl MsdTransport for ScriptedDevice {
    fn control_in(&mut self, setup: [u8; 8], data: &mut [u8]) -> Result<usize, Errno> {
        // The only control-IN the transports issue is GET MAX LUN.
        assert_eq!(setup[0], 0xA1);
        assert_eq!(setup[1], 0xFE);
        match self.max_lun {
            Some(value) => {
                data[0] = value;
                Ok(1)
            }
            None => Err(Errno::EndpointStalled),
        }
    }

    fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<(), Errno> {
        if self.stall_next_control_out {
            self.stall_next_control_out = false;
            return Err(Errno::EndpointStalled);
        }
        if data.first() == Some(&0x1D) {
            self.trail.push("reset");
        }
        self.control_outs.push((setup, data.to_vec()));
        Ok(())
    }

    fn control_no_data(&mut self, setup: [u8; 8]) -> Result<(), Errno> {
        self.resets.push(setup);
        self.trail.push("reset");
        self.reset_failure.take().map_or(Ok(()), Err)
    }

    fn bulk_in(&mut self, data: &mut [u8]) -> Result<usize, Errno> {
        match self.in_steps.pop_front() {
            Some(InStep::Data(bytes)) => {
                let n = bytes.len().min(data.len());
                data[..n].copy_from_slice(&bytes[..n]);
                Ok(n)
            }
            Some(InStep::Stall) => Err(Errno::EndpointStalled),
            Some(InStep::Fail(errno)) => Err(errno),
            None => Err(Errno::NotFound),
        }
    }

    fn bulk_out(&mut self, data: &[u8]) -> Result<usize, Errno> {
        if self.stall_next_out {
            self.stall_next_out = false;
            return Err(Errno::EndpointStalled);
        }
        if let Some(errno) = self.fail_next_out.take() {
            return Err(errno);
        }
        self.out_frames.push(data.to_vec());
        Ok(data.len())
    }

    fn interrupt_in(&mut self, data: &mut [u8]) -> Result<usize, Errno> {
        let next = self
            .stale_interrupts
            .pop_front()
            .or_else(|| self.interrupts.pop_front());
        match next {
            Some(bytes) => {
                let n = bytes.len().min(data.len());
                data[..n].copy_from_slice(&bytes[..n]);
                Ok(n)
            }
            None => Err(Errno::NotFound),
        }
    }

    fn discard_interrupts(&mut self) -> Result<(), Errno> {
        self.stale_interrupts.clear();
        Ok(())
    }

    fn begin_exchange(&mut self) {
        self.exchanges += 1;
    }

    fn clear_halts(&mut self) -> Result<(), Errno> {
        self.trail.push("clear");
        Ok(())
    }

    fn scrub(&mut self) {
        self.scrubs += 1;
    }
}
