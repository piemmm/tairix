//! The controller: its reset, the command and response rings a codec is
//! spoken to through, the DMA position buffer, and its stream descriptors
//! (Intel High Definition Audio Specification 1.0a, sections 3 and 4).
//!
//! A codec command is answered through the response ring's interrupt: the
//! engine parks on the controller's line until the response lands, never
//! polling. A park can also be woken by a stream's period; the status that
//! stream raised is kept for the engine rather than lost, and cleared in the
//! hardware so a message-signalled line raises again.

use tairix_abi::driver::dma::{DmaHost, DmaReach, DmaSlab};
use tairix_abi::{Delay, DriverError};
use tairix_dma_barrier::{dma_rmb, dma_wmb};

use crate::codec::Verbs;
use crate::regs::{self, descriptor, gcap, gctl, intr, ring_size, rirbsts, sd, Registers};
use crate::verb::{answered, response_source, unsolicited_tag, Verb};

/// How the engine waits on the controller's interrupt.
pub trait Wait {
    /// Park until the controller's interrupt fires or `budget_ns` passes.
    ///
    /// # Errors
    ///
    /// The kernel's refusal to park, which no later park would lift.
    fn park(&self, budget_ns: u64) -> Result<(), DriverError>;
}

/// Stream descriptors one controller may have: one interrupt bit each.
pub const MAX_DESCRIPTORS: usize = 30;

/// Codec addresses on one link: the four-bit address field, less the one the
/// controller itself answers broadcasts from.
pub const MAX_CODECS: usize = 15;

/// How long a codec has to answer a command. Codecs answer within a frame
/// or two; this bounds a codec that never will.
const COMMAND_TIMEOUT_US: u64 = 100_000;

/// How long a reset handshake may take before the controller is given up.
const HANDSHAKE_BUDGET_US: u64 = 100_000;

/// The step a handshake rechecks at.
const HANDSHAKE_STEP_US: u32 = 100;

/// How long codecs take to announce themselves after the link leaves reset:
/// the specification's 521 µs, rounded up.
const CODEC_SETTLE_US: u32 = 1_000;

/// How long the link must be held in reset.
const RESET_HOLD_US: u32 = 100;

/// The controller memory: the command ring, the response ring and the DMA
/// position buffer, each at a 128-byte boundary as the controller requires.
const CORB_OFFSET: usize = 0;
const RIRB_OFFSET: usize = 1_024;
const POSITIONS_OFFSET: usize = 3_072;
const RINGS_LEN: usize = POSITIONS_OFFSET + MAX_DESCRIPTORS * POSITION_STRIDE;

/// Bytes per command, per response, and per descriptor's position.
const CORB_ENTRY: usize = 4;
const RIRB_ENTRY: usize = 8;
const POSITION_STRIDE: usize = 8;

/// The alignment every controller-visible structure needs.
pub const DMA_ALIGN: u64 = 128;

/// What `GCAP` says the controller has.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Capabilities {
    /// Input stream descriptors, first in order.
    pub inputs: u8,
    /// Output stream descriptors, after the inputs.
    pub outputs: u8,
    /// Bidirectional descriptors, after the outputs.
    pub bidirectional: u8,
    /// The controller drives 64-bit addresses.
    pub wide: bool,
}

impl Capabilities {
    const fn of(gcap: u16) -> Self {
        Self {
            inputs: ((gcap >> gcap::ISS_SHIFT) & gcap::STREAMS_MASK) as u8,
            outputs: ((gcap >> gcap::OSS_SHIFT) & gcap::STREAMS_MASK) as u8,
            bidirectional: ((gcap >> gcap::BSS_SHIFT) & gcap::BSS_MASK) as u8,
            wide: gcap & gcap::OK64 != 0,
        }
    }

    /// Every descriptor the controller has.
    #[must_use]
    pub const fn descriptors(self) -> u8 {
        self.inputs + self.outputs + self.bidirectional
    }
}

/// What a descriptor carries.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Flow {
    /// Samples to a codec.
    Out,
    /// Samples from a codec.
    In,
}

/// A command or response ring's size: entries, and the `SIZE` encoding.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct RingSize {
    entries: u16,
    select: u8,
}

impl RingSize {
    /// The largest size `capability` admits.
    const fn largest(capability: u8) -> Option<Self> {
        if capability & ring_size::CAP_256 != 0 {
            Some(Self {
                entries: 256,
                select: ring_size::SELECT_256,
            })
        } else if capability & ring_size::CAP_16 != 0 {
            Some(Self {
                entries: 16,
                select: ring_size::SELECT_16,
            })
        } else if capability & ring_size::CAP_2 != 0 {
            Some(Self {
                entries: 2,
                select: ring_size::SELECT_2,
            })
        } else {
            None
        }
    }
}

/// The controller, out of reset with its rings running.
pub struct Controller<R: Registers, W: Wait, D: Delay> {
    regs: R,
    wait: W,
    delay: D,
    caps: Capabilities,
    rings: DmaSlab,
    corb: RingSize,
    rirb: RingSize,
    corb_write: u16,
    rirb_read: u16,
    /// Codecs that announced themselves at reset.
    codecs: u16,
    /// Each descriptor's status, gathered and cleared, not yet taken.
    status: [u8; MAX_DESCRIPTORS],
    /// Unsolicited tags each codec has raised, not yet taken.
    unsolicited: [u64; MAX_CODECS],
    /// Stream interrupts enabled, by descriptor.
    stream_interrupts: u32,
}

impl<R: Registers, W: Wait, D: Delay> Controller<R, W, D> {
    /// Reset the controller, start its rings, and find its codecs.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for a controller that does not leave or
    /// enter reset, has no ring size, or a register outside the window;
    /// [`DriverError::Unsupported`] for memory the controller cannot address;
    /// or the DMA host's refusal.
    pub fn open(mut regs: R, wait: W, delay: D, dma: &dyn DmaHost) -> Result<Self, DriverError> {
        let caps = Capabilities::of(regs.read16(regs::GCAP)?);
        if usize::from(caps.descriptors()) > MAX_DESCRIPTORS {
            return Err(DriverError::DeviceFault);
        }
        // A controller a previous instance left running stops before reset,
        // so nothing it was doing outlives this one.
        regs.write8(regs::CORBCTL, 0)?;
        regs.write8(regs::RIRBCTL, 0)?;
        for index in 0..caps.descriptors() {
            regs.write8(descriptor(index) + sd::CTL, 0)?;
        }
        regs.write32(regs::INTCTL, 0)?;
        let mut controller_reset = Handshake {
            regs: &mut regs,
            delay: &delay,
        };
        controller_reset.set32(regs::GCTL, gctl::CRST, false)?;
        delay.delay_us(RESET_HOLD_US);
        let mut controller_reset = Handshake {
            regs: &mut regs,
            delay: &delay,
        };
        controller_reset.set32(regs::GCTL, gctl::CRST, true)?;
        delay.delay_us(CODEC_SETTLE_US);
        dma.device_quiesced();
        if !caps.wide {
            dma.narrow_dma_reach(DmaReach::of::<32>())?;
        }
        let corb =
            RingSize::largest(regs.read8(regs::CORBSIZE)?).ok_or(DriverError::DeviceFault)?;
        let rirb =
            RingSize::largest(regs.read8(regs::RIRBSIZE)?).ok_or(DriverError::DeviceFault)?;
        let rings = dma.alloc_dma_zeroed(RINGS_LEN)?;
        let base = rings.device_addr();
        if base % DMA_ALIGN != 0 || (!caps.wide && base + RINGS_LEN as u64 > 1 << 32) {
            return Err(DriverError::Unsupported);
        }
        let codecs = regs.read16(regs::STATESTS)? & ((1 << MAX_CODECS) - 1);
        regs.write16(regs::STATESTS, codecs)?;
        let mut controller = Self {
            regs,
            wait,
            delay,
            caps,
            rings,
            corb,
            rirb,
            corb_write: 0,
            rirb_read: 0,
            codecs,
            status: [0; MAX_DESCRIPTORS],
            unsolicited: [0; MAX_CODECS],
            stream_interrupts: 0,
        };
        controller.start_rings()?;
        let control = controller.regs.read32(regs::GCTL)?;
        controller.regs.write32(regs::GCTL, control | gctl::UNSOL)?;
        controller
            .regs
            .write32(regs::INTCTL, intr::GLOBAL | intr::CONTROLLER)?;
        Ok(controller)
    }

    /// Point the rings at their memory, reset their pointers, and run them.
    fn start_rings(&mut self) -> Result<(), DriverError> {
        let base = self.rings.device_addr();
        let corb = base + CORB_OFFSET as u64;
        let rirb = base + RIRB_OFFSET as u64;
        let positions = base + POSITIONS_OFFSET as u64;
        self.write_address(regs::CORBLBASE, regs::CORBUBASE, corb)?;
        self.regs.write8(regs::CORBSIZE, self.corb.select)?;
        // The read pointer's reset is acknowledged by its bit reading back
        // set; some controllers instead clear the pointer at once.
        self.regs.write16(regs::CORBRP, regs::POINTER_RESET)?;
        let reset = Handshake {
            regs: &mut self.regs,
            delay: &self.delay,
        };
        reset.settle(|regs| {
            let pointer = regs.read16(regs::CORBRP)?;
            Ok(pointer & regs::POINTER_RESET != 0 || pointer == 0)
        })?;
        self.regs.write16(regs::CORBRP, 0)?;
        let reset = Handshake {
            regs: &mut self.regs,
            delay: &self.delay,
        };
        reset.settle(|regs| Ok(regs.read16(regs::CORBRP)? == 0))?;
        self.regs.write16(regs::CORBWP, 0)?;
        self.write_address(regs::RIRBLBASE, regs::RIRBUBASE, rirb)?;
        self.regs.write8(regs::RIRBSIZE, self.rirb.select)?;
        self.regs.write16(regs::RIRBWP, regs::POINTER_RESET)?;
        self.regs.write16(regs::RINTCNT, 1)?;
        self.write_address(
            regs::DPLBASE,
            regs::DPUBASE,
            positions | u64::from(regs::DMA_POSITION_ENABLE),
        )?;
        self.corb_write = 0;
        self.rirb_read = 0;
        self.regs.write8(regs::CORBCTL, regs::RING_RUN)?;
        self.regs.write8(
            regs::RIRBCTL,
            regs::RING_RUN | regs::RIRB_INTERRUPT | regs::RIRB_OVERRUN_INTERRUPT,
        )
    }

    /// Stop both rings and start them afresh, discarding every command and
    /// response in flight: after a command times out nothing later on the
    /// response ring can be told from its late answer.
    fn restart_rings(&mut self) -> Result<(), DriverError> {
        let mut stop = Handshake {
            regs: &mut self.regs,
            delay: &self.delay,
        };
        stop.set8(regs::CORBCTL, regs::RING_RUN, false)?;
        let mut stop = Handshake {
            regs: &mut self.regs,
            delay: &self.delay,
        };
        stop.set8(regs::RIRBCTL, regs::RING_RUN, false)?;
        self.start_rings()
    }

    fn write_address(
        &mut self,
        lower: usize,
        upper: usize,
        address: u64,
    ) -> Result<(), DriverError> {
        self.regs.write32(lower, (address & 0xFFFF_FFFF) as u32)?;
        self.regs.write32(upper, (address >> 32) as u32)
    }

    /// What the controller has.
    #[must_use]
    pub const fn capabilities(&self) -> Capabilities {
        self.caps
    }

    /// The codec addresses that announced themselves.
    pub fn codecs(&self) -> impl Iterator<Item = u8> + '_ {
        (0u8..15).filter(|&address| self.codecs & (1 << address) != 0)
    }

    /// The descriptors that carry `flow`, in order: that direction's own,
    /// then the bidirectional ones.
    pub fn descriptors(&self, flow: Flow) -> impl Iterator<Item = u8> {
        let own = match flow {
            Flow::In => 0..self.caps.inputs,
            Flow::Out => self.caps.inputs..self.caps.inputs + self.caps.outputs,
        };
        let shared = self.caps.inputs + self.caps.outputs..self.caps.descriptors();
        own.chain(shared)
    }

    /// Whether `index` is a bidirectional descriptor.
    const fn bidirectional(&self, index: u8) -> bool {
        index >= self.caps.inputs + self.caps.outputs
    }

    /// Queue `verb` for `address` on the command ring.
    fn send(&mut self, address: u8, verb: Verb) -> Result<(), DriverError> {
        let next = (self.corb_write + 1) % self.corb.entries;
        let at = CORB_OFFSET + usize::from(next) * CORB_ENTRY;
        let entry = self
            .rings
            .as_bytes_mut()
            .get_mut(at..at + CORB_ENTRY)
            .ok_or(DriverError::DeviceFault)?;
        entry.copy_from_slice(&verb.command(address).to_le_bytes());
        self.rings.sync_range(at, CORB_ENTRY);
        dma_wmb();
        self.regs.write16(regs::CORBWP, next)?;
        self.corb_write = next;
        Ok(())
    }

    /// The next response on the ring: its word and the extended word naming
    /// its codec and whether it was unsolicited.
    fn next_response(&mut self) -> Result<Option<(u32, u32)>, DriverError> {
        let written = self.regs.read16(regs::RIRBWP)? % self.rirb.entries;
        if written == self.rirb_read {
            return Ok(None);
        }
        dma_rmb();
        let next = (self.rirb_read + 1) % self.rirb.entries;
        let at = RIRB_OFFSET + usize::from(next) * RIRB_ENTRY;
        self.rings.sync_range(at, RIRB_ENTRY);
        let entry = self
            .rings
            .as_bytes()
            .get(at..at + RIRB_ENTRY)
            .ok_or(DriverError::DeviceFault)?;
        let word = |offset: usize| {
            u32::from_le_bytes([
                entry[offset],
                entry[offset + 1],
                entry[offset + 2],
                entry[offset + 3],
            ])
        };
        let response = (word(0), word(4));
        self.rirb_read = next;
        Ok(Some(response))
    }

    /// Take every response waiting, keeping the unsolicited ones' tags and
    /// answering the solicited one, if one is there.
    fn drain_responses(&mut self) -> Result<Option<(u8, u32)>, DriverError> {
        while let Some((response, extended)) = self.next_response()? {
            let (codec, unsolicited) = response_source(extended);
            if unsolicited {
                if let Some(tags) = self.unsolicited.get_mut(usize::from(codec)) {
                    *tags |= 1 << (unsolicited_tag(response) & 0x3F);
                }
            } else {
                return Ok(Some((codec, response)));
            }
        }
        Ok(None)
    }

    /// Read and clear the controller's causes, keeping each stream's status
    /// and every unsolicited tag for the engine.
    ///
    /// # Errors
    ///
    /// A register outside the window.
    pub fn gather(&mut self) -> Result<(), DriverError> {
        if !self.gather_causes()? {
            return Ok(());
        }
        if self.drain_responses()?.is_some() {
            // A solicited answer with no command outstanding is a codec
            // speaking out of turn; it answers nothing.
            return Err(DriverError::DeviceFault);
        }
        Ok(())
    }

    /// Each descriptor's gathered status, taken.
    pub fn take_status(&mut self) -> [u8; MAX_DESCRIPTORS] {
        core::mem::take(&mut self.status)
    }

    /// Each codec's unsolicited tags, taken.
    pub fn take_unsolicited(&mut self) -> [u64; MAX_CODECS] {
        core::mem::take(&mut self.unsolicited)
    }

    /// Reset descriptor `index`, leaving it idle with its registers cleared.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for a descriptor that does not
    /// acknowledge its reset.
    pub fn reset_stream(&mut self, index: u8) -> Result<(), DriverError> {
        let control = descriptor(index) + sd::CTL;
        let mut reset = Handshake {
            regs: &mut self.regs,
            delay: &self.delay,
        };
        reset.set8(control, sd::RUN, false)?;
        reset.set8(control, sd::SRST, true)?;
        reset.set8(control, sd::SRST, false)?;
        self.status[usize::from(index)] = 0;
        // The controller writes a stopped descriptor's position only once it
        // moves again, so the last stream's must not be read as this one's.
        let at = POSITIONS_OFFSET + usize::from(index) * POSITION_STRIDE;
        if let Some(position) = self.rings.as_bytes_mut().get_mut(at..at + POSITION_STRIDE) {
            position.fill(0);
        }
        self.rings.sync_range(at, POSITION_STRIDE);
        Ok(())
    }

    /// Program descriptor `index` to carry `flow` as stream `tag`, in
    /// `format`, over the buffer `bdl` describes, of `length` bytes in
    /// `entries` pieces. The descriptor must be reset.
    ///
    /// # Errors
    ///
    /// A register outside the window.
    pub fn program_stream(&mut self, index: u8, setup: &StreamSetup) -> Result<(), DriverError> {
        let base = descriptor(index);
        let mut stream = setup.tag << sd::STREAM_SHIFT;
        if self.bidirectional(index) && setup.flow == Flow::Out {
            stream |= sd::BIDIRECTIONAL_OUTPUT;
        }
        self.regs.write8(base + sd::CTL_STREAM, stream)?;
        self.regs.write32(base + sd::CBL, setup.length)?;
        self.regs
            .write16(base + sd::LVI, u16::from(setup.entries - 1))?;
        self.regs.write16(base + sd::FMT, setup.format)?;
        self.write_address(base + sd::BDPL, base + sd::BDPU, setup.bdl)?;
        self.regs
            .write8(base + sd::CTL, sd::IOCE | sd::FEIE | sd::DEIE)
    }

    /// Start or stop descriptor `index`; a stop returns once the descriptor
    /// reads stopped.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for a descriptor that does not stop.
    pub fn run_stream(&mut self, index: u8, run: bool) -> Result<(), DriverError> {
        let control = descriptor(index) + sd::CTL;
        if run {
            let value = self.regs.read8(control)?;
            dma_wmb();
            return self.regs.write8(control, value | sd::RUN);
        }
        Handshake {
            regs: &mut self.regs,
            delay: &self.delay,
        }
        .set8(control, sd::RUN, false)
    }

    /// Enable or disable descriptor `index`'s interrupt.
    ///
    /// # Errors
    ///
    /// A register outside the window.
    pub fn stream_interrupt(&mut self, index: u8, enabled: bool) -> Result<(), DriverError> {
        let bit = 1u32 << index;
        if enabled {
            self.stream_interrupts |= bit;
        } else {
            self.stream_interrupts &= !bit;
        }
        self.regs.write32(
            regs::INTCTL,
            intr::GLOBAL | intr::CONTROLLER | self.stream_interrupts,
        )
    }

    /// Where descriptor `index`'s DMA has reached in its buffer, in bytes.
    #[must_use]
    pub fn position(&self, index: u8) -> u32 {
        let at = POSITIONS_OFFSET + usize::from(index) * POSITION_STRIDE;
        self.rings.sync_range(at, 4);
        self.rings.as_bytes().get(at..at + 4).map_or(0, |bytes| {
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        })
    }

    /// The registers, for the test model to drive.
    #[cfg(test)]
    pub fn registers_mut(&mut self) -> &mut R {
        &mut self.regs
    }
}

impl<R: Registers, W: Wait, D: Delay> Drop for Controller<R, W, D> {
    /// Stop everything the controller reaches memory for before that memory
    /// goes back; memory a controller that will not stop may still write is
    /// never handed to anyone else.
    fn drop(&mut self) {
        let mut stopped = self.regs.write32(regs::INTCTL, 0).is_ok();
        for index in 0..self.caps.descriptors() {
            stopped &= self.regs.write8(descriptor(index) + sd::CTL, 0).is_ok();
        }
        stopped &= self.regs.write8(regs::CORBCTL, 0).is_ok()
            && self.regs.write8(regs::RIRBCTL, 0).is_ok()
            && self.regs.write32(regs::DPLBASE, 0).is_ok();
        if !stopped {
            self.rings.withhold();
        }
    }
}

/// What a descriptor is programmed with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StreamSetup {
    /// Which way its samples flow.
    pub flow: Flow,
    /// The stream number its converters listen for, one to fifteen.
    pub tag: u8,
    /// The stream format word.
    pub format: u16,
    /// The buffer descriptor list's address.
    pub bdl: u64,
    /// The buffer's bytes.
    pub length: u32,
    /// The pieces the buffer descriptor list holds.
    pub entries: u8,
}

impl<R: Registers, W: Wait, D: Delay> Verbs for Controller<R, W, D> {
    fn exchange(&mut self, address: u8, verb: Verb) -> Result<u32, DriverError> {
        // Anything already waiting is unsolicited or owed; it is not this
        // command's answer.
        if let Some((_, _)) = self.drain_responses()? {
            return Err(DriverError::DeviceFault);
        }
        self.send(address, verb)?;
        let deadline = self.delay.now_us().saturating_add(COMMAND_TIMEOUT_US);
        loop {
            if let Some((codec, response)) = self.drain_responses()? {
                return if codec == address {
                    answered(response)
                } else {
                    Err(DriverError::DeviceFault)
                };
            }
            let now = self.delay.now_us();
            if now >= deadline {
                self.restart_rings()?;
                return Err(DriverError::DeviceFault);
            }
            self.wait.park((deadline - now).saturating_mul(1_000))?;
            self.gather_causes()?;
        }
    }
}

impl<R: Registers, W: Wait, D: Delay> Controller<R, W, D> {
    /// Read and clear the interrupt causes: each stream's status kept, the
    /// response ring's cleared so the next response raises the line again.
    /// Answers whether the response ring raised one.
    fn gather_causes(&mut self) -> Result<bool, DriverError> {
        let causes = self.regs.read32(regs::INTSTS)?;
        let responses = causes & intr::CONTROLLER != 0;
        if responses {
            let ring = self.regs.read8(regs::RIRBSTS)?;
            self.regs
                .write8(regs::RIRBSTS, ring & (rirbsts::RESPONSE | rirbsts::OVERRUN))?;
        }
        for index in 0..self.caps.descriptors() {
            if causes & intr::STREAMS & (1 << index) == 0 {
                continue;
            }
            let at = descriptor(index) + sd::STS;
            let status = self.regs.read8(at)? & sd::STATUS_MASK;
            if status != 0 {
                self.regs.write8(at, status)?;
                self.status[usize::from(index)] |= status;
            }
        }
        Ok(responses)
    }
}

/// A bit a register must be driven to and read back at, within a bounded
/// budget: the controller acknowledges resets this way, and has no
/// interrupt for it.
struct Handshake<'a, R: Registers, D: Delay> {
    regs: &'a mut R,
    delay: &'a D,
}

impl<R: Registers, D: Delay> Handshake<'_, R, D> {
    fn settle(
        &self,
        mut reached: impl FnMut(&R) -> Result<bool, DriverError>,
    ) -> Result<(), DriverError> {
        let start = self.delay.now_us();
        loop {
            if reached(&*self.regs)? {
                return Ok(());
            }
            if self.delay.now_us().saturating_sub(start) >= HANDSHAKE_BUDGET_US {
                return Err(DriverError::DeviceFault);
            }
            self.delay.delay_us(HANDSHAKE_STEP_US);
        }
    }

    fn set32(&mut self, offset: usize, bit: u32, on: bool) -> Result<(), DriverError> {
        let value = self.regs.read32(offset)?;
        self.regs
            .write32(offset, if on { value | bit } else { value & !bit })?;
        self.settle(|regs| Ok((regs.read32(offset)? & bit != 0) == on))
    }

    fn set8(&mut self, offset: usize, bit: u8, on: bool) -> Result<(), DriverError> {
        let value = self.regs.read8(offset)?;
        self.regs
            .write8(offset, if on { value | bit } else { value & !bit })?;
        self.settle(|regs| Ok((regs.read8(offset)? & bit != 0) == on))
    }
}
