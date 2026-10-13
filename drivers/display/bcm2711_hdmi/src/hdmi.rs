//! One HDMI transmitter as an `encoder-v1` encoder: its connector found from
//! the hotplug lines and the sink's EDID, and a mode set's steps programmed as
//! Linux's `vc4_hdmi` takes a BCM2711 block through them.

use tairix_abi::driver::clock;
use tairix_abi::driver::display::{Polarity, Scan, Timing};
use tairix_abi::driver::encoder::{Connector, Encoder, Phase, PipeLimits};
use tairix_abi::driver::mmio::RegisterBlock;
use tairix_abi::{Delay, DriverError, Errno, CALL_DEADLINE_NONE};
use tairix_audiochan::cyclic::DmaPort;
use tairix_drvrt::Recorder;
use tairix_edid::{
    hdmi_vendor, Avi, Edid, Packed, Quantization, VideoCode, BLOCK_LEN, INFOFRAME_MAX, MAX_BLOCKS,
};
use tairix_linkclient::{ClockClient, IrqLineClient, LinkCall};

use crate::audio::AudioPath;
use crate::mode::{choose, Chosen, Transmitter};
use crate::phy::{self, LaneMap};
use crate::regs::{core, csc, dvp, field, hd, packet};
use crate::sink::{self, Ddc};

/// The state machine clock runs at least this fast while the block is used.
pub(crate) const HSM_MIN_HZ: u64 = tairix_vcmailbox::FirmwareClock::M2mc.least_hz();

/// How long the scheduler takes to change mode, in microseconds.
const SCHEDULER_BUDGET_US: u64 = 1_000_000;
/// How long a packet slot takes to empty or fill.
const PACKET_BUDGET_US: u64 = 100_000;
/// How long the FIFO takes to recentre.
const RECENTER_BUDGET_US: u64 = 1_000;
/// The pause a poll of one of those takes between reads.
const POLL_US: u32 = 1_000;
/// How long a sink is given to see the link scrambled before it is told
/// again, as Linux's `vc4_hdmi_scrambling_wq` waits.
const SCRAMBLER_CHECK_US: u64 = 1_000_000;

/// The bytes one packet slot holds: its words alternate three bytes and four,
/// as `vc4_hdmi_write_infoframe` lays a frame out.
const SLOT_BYTES: usize = {
    let mut bytes = 0;
    let mut word = 0;
    while word < packet::STRIDE / 4 {
        bytes += if word % 2 == 0 { 3 } else { 4 };
        word += 1;
    }
    bytes
};
const _: () = assert!(INFOFRAME_MAX <= SLOT_BYTES, "every InfoFrame fits a slot");

/// A decision the transmitter records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// A sink is connected whose EDID could not be read: it is given the
    /// fallback mode, as DVI.
    Unreadable,
    /// The sink forgot the scrambling its running link needs.
    LinkLost {
        /// The connector's link generation now.
        link: u32,
    },
    /// The sink has not seen the link scrambled; it is told again.
    Unscrambled,
}

/// One transmitter of the BCM2711, as its matched bind key names it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Variant {
    /// What its audio endpoint is called.
    pub name: &'static str,
    /// Its fastest TMDS character rate, in kHz.
    pub max_tmds_khz: u32,
    /// How its PHY's lanes are wired.
    pub lanes: LaneMap,
    /// Its video control register's offset in the shared `hd` block.
    pub vid_ctl: usize,
    /// Its MAI registers' base in the shared `hd` block.
    pub mai: usize,
}

impl Variant {
    /// Where in the `hd` block its MAI's data register lies, which its audio
    /// DMA channel writes.
    #[must_use]
    pub const fn mai_data(&self) -> usize {
        self.mai + hd::MAI_DATA
    }
}

/// The register windows a transmitter is programmed through.
pub struct Windows<R> {
    /// The transmitter core.
    pub core: R,
    /// The display video port.
    pub dvp: R,
    /// The TMDS PHY.
    pub phy: R,
    /// The rate manager.
    pub rm: R,
    /// The packet RAM.
    pub packet: R,
    /// The colour space converter.
    pub csc: R,
    /// The block both transmitters' video controls share.
    pub hd: R,
}

/// The clients of the clocks, the DDC bus and the hotplug lines a
/// transmitter's node links to, and those its sound needs.
pub struct Links<C: LinkCall, P, M> {
    /// The state machine clock.
    pub hsm: ClockClient<C>,
    /// The pixel path's clock.
    pub bvb: ClockClient<C>,
    /// The DDC bus.
    pub ddc: P,
    /// The lines raised when a sink arrives and when it leaves.
    pub hotplug: [IrqLineClient<C>; 2],
    /// The links its sound needs, or `None` for a transmitter that carries
    /// none: sound is the video's companion, never its precondition.
    pub audio: Option<AudioLinks<C, M>>,
}

/// The links a transmitter's sound needs.
pub struct AudioLinks<C: LinkCall, M> {
    /// The MAI's sample clock.
    pub clock: ClockClient<C>,
    /// The channel feeding the MAI.
    pub dma: M,
}

/// An HDMI transmitter: the encoder its display engine drives and, where it
/// carries sound, the audio endpoint its sink plays.
pub struct Hdmi<R, C: LinkCall, P, D, L, M: DmaPort> {
    pub(crate) regs: Windows<R>,
    pub(crate) variant: Variant,
    hsm: ClockClient<C>,
    bvb: ClockClient<C>,
    ddc: P,
    hotplug: [IrqLineClient<C>; 2],
    pub(crate) delay: D,
    log: L,
    transmitter: Transmitter,
    pub(crate) edid: Option<Edid>,
    limits: PipeLimits,
    pub(crate) connected: bool,
    pub(crate) chosen: Option<Chosen>,
    link: u32,
    found: Phase,
    pub(crate) holds: Holds,
    /// The state machine clock's floor as last stated: [`None`] while it is
    /// released, and no register may be reached.
    hsm_floor: Option<u64>,
    /// The output sends video, so its packets reach the sink.
    pub(crate) sending: bool,
    /// When the sink is next asked whether it sees the link scrambled.
    scrambler_check_us: Option<u64>,
    pub(crate) audio: Option<AudioPath<M, C>>,
    scratch: [u8; MAX_BLOCKS * BLOCK_LEN],
}

/// What holds the state machine clock up.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Holds {
    /// The output is up — set up by a configure, or left running — until it
    /// is powered down, needing the clock at this rate.
    pub(crate) video: Option<u64>,
    /// The pixel path's clock is floored.
    pub(crate) bvb: bool,
    /// The audio path has a stream configured.
    pub(crate) audio: bool,
    /// The connector is being read.
    pub(crate) probe: bool,
}

impl Holds {
    /// The floor the state machine clock is held at, while anything holds it.
    fn floor(self) -> Option<u64> {
        let held = self.video.is_some() || self.audio || self.probe;
        held.then(|| self.video.unwrap_or(HSM_MIN_HZ).max(HSM_MIN_HZ))
    }
}

/// No bound beyond the transmitter's own, until the engine states its pipe's.
const UNBOUNDED: PipeLimits = PipeLimits {
    max_clock_khz: u32::MAX,
    max_active_khz: u32::MAX,
};

fn modify(
    regs: &dyn RegisterBlock,
    offset: usize,
    clear: u32,
    set: u32,
) -> Result<(), DriverError> {
    let value = regs.read32(offset)?;
    regs.write32(offset, value & !clear | set)
}

fn floored(result: Result<u64, Errno>) -> Result<(), DriverError> {
    result.map(|_| ()).map_err(clock::refusal)
}

/// The BVB clock a TMDS rate needs.
const fn bvb_hz(tmds_hz: u64) -> u64 {
    if tmds_hz > 297_000_000 {
        300_000_000
    } else if tmds_hz > 148_500_000 {
        150_000_000
    } else {
        75_000_000
    }
}

/// The colour conversion's coefficients, row by row: unity for full range,
/// 0.8594 and an offset of 16 squeezing it into 16 to 235.
const fn csc_rows(limited: bool) -> [[u32; 4]; 3] {
    if limited {
        [
            [0x1B80, 0, 0, 0x0400],
            [0, 0x1B80, 0, 0x0400],
            [0, 0, 0x1B80, 0x0400],
        ]
    } else {
        [[0x2000, 0, 0, 0], [0, 0x2000, 0, 0], [0, 0, 0x2000, 0]]
    }
}

/// Whether an HDMI sink takes `chosen` as limited-range RGB: every consumer
/// electronics timing but 640x480 is by default; IT and VESA ones are not.
fn limited_range(chosen: &Chosen) -> bool {
    chosen.hdmi
        && match chosen.code {
            Some(VideoCode::Cta { vic, .. }) => vic > 1,
            Some(VideoCode::Hdmi(_)) => true,
            None => false,
        }
}

/// The first failure of a teardown that runs every step whatever fails.
fn first(errors: impl IntoIterator<Item = Result<(), DriverError>>) -> Result<(), DriverError> {
    errors.into_iter().fold(Ok(()), Result::and)
}

impl<R, C, P, D, L, M> Hdmi<R, C, P, D, L, M>
where
    R: RegisterBlock,
    C: LinkCall,
    P: Ddc,
    D: Delay,
    L: Recorder<Record>,
    M: DmaPort,
{
    /// The transmitter `variant` behind `regs`, its hotplug lines watched and
    /// its connector read, its state machine clock kept only while an output
    /// the firmware left running needs it; it records to `log`.
    ///
    /// # Errors
    ///
    /// The state machine clock's refusal, a hotplug line that cannot be
    /// watched, or a window too short for its registers.
    pub fn open(
        regs: Windows<R>,
        variant: Variant,
        links: Links<C, P, M>,
        delay: D,
        wifi_coexistence: bool,
        log: L,
    ) -> Result<Self, DriverError> {
        let windows = [
            (&regs.core, core::LEN),
            (&regs.dvp, dvp::LEN),
            (&regs.phy, crate::regs::phy::LEN),
            (&regs.rm, crate::regs::rm::LEN),
            (&regs.packet, packet::LEN),
            (&regs.csc, csc::LEN),
            (
                &regs.hd,
                (variant.vid_ctl + 4).max(variant.mai + hd::MAI_LEN),
            ),
        ];
        if windows
            .iter()
            .any(|(window, len)| window.block_len() < *len)
        {
            return Err(DriverError::LengthOutOfRange);
        }
        let Links {
            mut hsm,
            bvb,
            ddc,
            mut hotplug,
            audio,
        } = links;
        // A running output keeps the rate the firmware gave it.
        let running_hz = hsm.describe().map_err(clock::refusal)?.hz;
        let floor = HSM_MIN_HZ.max(running_hz);
        floored(hsm.floor(floor))?;
        for line in &mut hotplug {
            line.post_wait(CALL_DEADLINE_NONE)
                .map_err(DriverError::from_errno)?;
        }
        let running = regs.hd.read32(variant.vid_ctl)? & hd::VID_CTL_ENABLE != 0;
        let mut hdmi = Self {
            regs,
            variant,
            hsm,
            bvb,
            ddc,
            hotplug,
            delay,
            log,
            transmitter: Transmitter {
                max_tmds_khz: variant.max_tmds_khz,
                wifi_coexistence,
            },
            edid: None,
            limits: UNBOUNDED,
            connected: false,
            chosen: None,
            link: 0,
            found: if running { Phase::Enabled } else { Phase::Off },
            holds: Holds {
                video: running.then_some(floor),
                probe: true,
                ..Holds::default()
            },
            hsm_floor: Some(floor),
            sending: running,
            scrambler_check_us: None,
            audio: audio.map(|links| AudioPath::new(links.clock, links.dma)),
            scratch: [0; MAX_BLOCKS * BLOCK_LEN],
        };
        hdmi.read_connector()?;
        hdmi.holds.probe = false;
        hdmi.settle_hsm()?;
        Ok(hdmi)
    }

    /// Whether a register may be reached: the state machine clock is held.
    pub(crate) const fn reachable(&self) -> bool {
        self.hsm_floor.is_some()
    }

    /// State the state machine clock's floor its holders need, releasing it
    /// once none does.
    pub(crate) fn settle_hsm(&mut self) -> Result<(), DriverError> {
        let needed = self.holds.floor();
        if needed == self.hsm_floor {
            return Ok(());
        }
        match needed {
            Some(hz) => floored(self.hsm.floor(hz))?,
            None => self.hsm.release().map_err(clock::refusal)?,
        }
        self.hsm_floor = needed;
        Ok(())
    }

    /// Read the connector again, the state machine clock held for the reads;
    /// answers whether it moved.
    fn probe(&mut self) -> Result<bool, DriverError> {
        self.holds.probe = true;
        let moved = self.settle_hsm().and_then(|()| self.read_connector());
        self.holds.probe = false;
        let settled = self.settle_hsm();
        let moved = moved?;
        settled?;
        Ok(moved)
    }

    /// Read whether a sink is connected and, when it is, its description;
    /// answers whether the connector moved. A scrambled link the sink no
    /// longer runs scrambled is lost, as Linux's `vc4_hdmi_reset_link` finds.
    fn read_connector(&mut self) -> Result<bool, DriverError> {
        let before = self.connector();
        self.connected = self.regs.core.read32(core::HOTPLUG)? & core::HOTPLUG_CONNECTED != 0;
        self.edid = if self.connected {
            sink::read_edid(&mut self.ddc, &self.delay, &mut self.scratch)
        } else {
            None
        };
        if self.connected && self.edid.is_none() {
            self.log.record(Record::Unreadable);
        }
        if let Some(audio) = self.audio.as_mut() {
            audio.follow(self.connected, self.edid.as_ref());
        }
        self.choose();
        // A sink that does not answer is not counted lost.
        if self.connected
            && self.scrambling()?
            && !sink::scrambling_set(&mut self.ddc).unwrap_or(true)
        {
            self.link = self.link.wrapping_add(1);
            self.log.record(Record::LinkLost { link: self.link });
        }
        Ok(self.connector() != before)
    }

    fn choose(&mut self) {
        self.chosen = if self.connected {
            choose(self.transmitter, self.edid.as_ref(), self.limits)
        } else {
            None
        };
    }

    /// Whether the transmitter scrambles what it sends.
    fn scrambling(&self) -> Result<bool, DriverError> {
        Ok(self.holds.video.is_some()
            && self.regs.core.read32(core::SCRAMBLER_CTL)? & core::SCRAMBLER_ENABLE != 0)
    }

    /// Poll `ready` once a millisecond for up to `budget_us`, sleeping
    /// between reads: a bounded handshake that fails closed.
    fn poll(
        &self,
        budget_us: u64,
        ready: impl Fn() -> Result<bool, DriverError>,
    ) -> Result<(), DriverError> {
        let started = self.delay.now_us();
        loop {
            if ready()? {
                return Ok(());
            }
            if self.delay.now_us().saturating_sub(started) >= budget_us {
                return Err(DriverError::DeviceFault);
            }
            self.delay.delay_us(POLL_US);
        }
    }

    fn chosen(&self) -> Result<Chosen, DriverError> {
        self.chosen.ok_or(DriverError::Stale)
    }

    /// The mode the output is set up for, once its state machine clock runs:
    /// a register reached without it stalls the bus.
    fn powered_chosen(&self) -> Result<Chosen, DriverError> {
        if self.holds.video.is_none() {
            return Err(DriverError::Stale);
        }
        self.chosen()
    }

    /// The clocks, PHY and timing for `chosen`.
    fn set_up(&mut self, chosen: &Chosen) -> Result<(), DriverError> {
        let tmds_hz = u64::from(chosen.timing.clock_khz) * 1_000;
        // A state machine clock infinitesimally faster than the pixel clock
        // is the firmware's rule; Linux keeps a margin of one percent.
        self.holds.video = Some(tmds_hz / 100 * 101);
        self.settle_hsm()?;
        floored(self.bvb.floor(bvb_hz(tmds_hz)))?;
        self.holds.bvb = true;
        phy::init(
            &self.regs.phy,
            &self.regs.rm,
            tmds_hz,
            chosen.scrambled,
            self.variant.lanes,
        )?;
        modify(
            &self.regs.core,
            core::SCHEDULER_CONTROL,
            0,
            core::SCHEDULER_MANUAL_FORMAT | core::SCHEDULER_IGNORE_VSYNC_PREDICTS,
        )?;
        self.set_timings(&chosen.timing)
    }

    /// The timing registers, as `vc5_hdmi_set_timings` lays them out for an
    /// RGB link of eight bits a channel.
    fn set_timings(&self, timing: &Timing) -> Result<(), DriverError> {
        let core_regs = &self.regs.core;
        let line = timing.horizontal;
        let [active, front, sync, back] =
            [line.active, line.front, line.sync, line.back].map(u32::from);
        let positive = |polarity| polarity == Polarity::Positive;
        core_regs.write32(
            core::HORZA,
            if positive(timing.vsync) {
                core::HORZA_VPOS
            } else {
                0
            } | if positive(timing.hsync) {
                core::HORZA_HPOS
            } else {
                0
            } | field(active, 0, 14)
                | field(front, 16, 13),
        )?;
        core_regs.write32(core::HORZB, field(back, 16, 11) | field(sync, 0, 11))?;
        let frame = timing.vertical;
        let halve = u32::from(timing.scan == Scan::Interlaced);
        let [v_active, v_sync_start, v_sync_end, v_total] = [
            u32::from(frame.active),
            u32::from(frame.active) + u32::from(frame.front),
            u32::from(frame.active) + u32::from(frame.front) + u32::from(frame.sync),
            frame.total(),
        ]
        .map(|edge| edge >> halve);
        let verta = field(v_sync_end - v_sync_start, 24, 5)
            | field(v_sync_start - v_active, 16, 7)
            | field(v_active, 0, 13);
        let back_porch = v_total - v_sync_end;
        core_regs.write32(core::VERTA0, verta)?;
        core_regs.write32(core::VERTA1, verta)?;
        core_regs.write32(core::VERTB0, field(back_porch, 0, 9))?;
        core_regs.write32(
            core::VERTB1,
            field(line.total() >> 1, 16, 14) | field(back_porch + halve, 0, 9),
        )?;
        // Eight bits a channel: no deep colour, the packing phase at its
        // reset, and the sink's mute released.
        modify(core_regs, core::DEEP_COLOR_CONFIG_1, 0x70F, field(2, 8, 3))?;
        modify(core_regs, core::GCP_WORD_1, 0xFFFF, core::GCP_CLEAR_AVMUTE)?;
        modify(core_regs, core::GCP_CONFIG, 0, core::GCP_ENABLE)?;
        modify(core_regs, core::MISC_CONTROL, 0xF, 0)?;
        self.regs.dvp.write32(dvp::CLOCK_STOP, 0)
    }

    /// Write `frame` into its packet slot and have the transmitter send it.
    pub(crate) fn send_packet(&self, frame: &Packed) -> Result<(), DriverError> {
        let id = usize::from(frame.kind())
            .checked_sub(0x80)
            .filter(|&id| id < packet::SLOTS)
            .ok_or(DriverError::OutOfRange)?;
        let bit = 1 << id;
        modify(&self.regs.core, core::RAM_PACKET_CONFIG, bit, 0)?;
        self.poll(PACKET_BUDGET_US, || {
            Ok(self.regs.core.read32(core::RAM_PACKET_STATUS)? & bit == 0)
        })?;
        // The rest of the slot is part of the frame a sink checks, so it is
        // written as zeros.
        let mut rest = frame.as_bytes();
        for word in 0..packet::STRIDE / 4 {
            let take = rest.len().min(if word % 2 == 0 { 3 } else { 4 });
            let (bytes, tail) = rest.split_at(take);
            rest = tail;
            let mut value = [0u8; 4];
            value[..take].copy_from_slice(bytes);
            self.regs
                .packet
                .write32(id * packet::STRIDE + word * 4, u32::from_le_bytes(value))?;
        }
        modify(&self.regs.core, core::RAM_PACKET_CONFIG, 0, bit)?;
        self.poll(PACKET_BUDGET_US, || {
            Ok(self.regs.core.read32(core::RAM_PACKET_STATUS)? & bit != 0)
        })
    }

    /// The `InfoFrames` an HDMI sink is sent with `chosen`. The range is
    /// always stated: it is the timing's default, which HDMI 2.0 recommends
    /// stating even to a sink that cannot select one.
    fn send_infoframes(&self, chosen: &Chosen) -> Result<(), DriverError> {
        let quantization = if limited_range(chosen) {
            Quantization::Limited
        } else {
            Quantization::Full
        };
        self.send_packet(
            &Avi {
                code: chosen.code,
                quantization,
            }
            .pack(),
        )?;
        if let Some(vendor) = chosen.code.and_then(hdmi_vendor) {
            self.send_packet(&vendor)?;
        }
        Ok(())
    }

    /// Recentre the FIFO between the pixel valve and the transmitter, twice
    /// with a pause between, as the block needs once its video runs.
    fn recenter_fifo(&self) -> Result<(), DriverError> {
        let fifo = &self.regs.core;
        let drift = fifo.read32(core::FIFO_CTL)? & core::FIFO_VALID_WRITE;
        let recenter = || -> Result<(), DriverError> {
            fifo.write32(core::FIFO_CTL, drift & !core::FIFO_RECENTER)?;
            fifo.write32(core::FIFO_CTL, drift | core::FIFO_RECENTER)
        };
        recenter()?;
        self.delay.delay_us(POLL_US);
        recenter()?;
        self.poll(RECENTER_BUDGET_US, || {
            Ok(fifo.read32(core::FIFO_CTL)? & core::FIFO_RECENTER_DONE != 0)
        })
    }

    fn start(&mut self, chosen: &Chosen) -> Result<(), DriverError> {
        let timing = chosen.timing;
        let vid_ctl = self.variant.vid_ctl;
        let low = |polarity, bit| {
            if polarity == Polarity::Positive {
                0
            } else {
                bit
            }
        };
        modify(
            &self.regs.hd,
            vid_ctl,
            hd::VID_CTL_VSYNC_LOW | hd::VID_CTL_HSYNC_LOW,
            hd::VID_CTL_ENABLE
                | hd::VID_CTL_CLRRGB
                | hd::VID_CTL_UNDERFLOW_ENABLE
                | hd::VID_CTL_FRAME_COUNTER_RESET
                | hd::VID_CTL_BLANK_INSERT_EN
                | low(timing.vsync, hd::VID_CTL_VSYNC_LOW)
                | low(timing.hsync, hd::VID_CTL_HSYNC_LOW),
        )?;
        modify(&self.regs.hd, vid_ctl, hd::VID_CTL_BLANKPIX, 0)?;
        let scheduler = &self.regs.core;
        if chosen.hdmi {
            modify(
                scheduler,
                core::SCHEDULER_CONTROL,
                0,
                core::SCHEDULER_MODE_HDMI,
            )?;
            self.poll(SCHEDULER_BUDGET_US, || {
                Ok(scheduler.read32(core::SCHEDULER_CONTROL)? & core::SCHEDULER_HDMI_ACTIVE != 0)
            })?;
            scheduler.write32(core::RAM_PACKET_CONFIG, core::RAM_PACKET_ENABLE)?;
            self.send_infoframes(chosen)?;
            self.audio_link_up(chosen)?;
        } else {
            modify(
                scheduler,
                core::RAM_PACKET_CONFIG,
                core::RAM_PACKET_ENABLE,
                0,
            )?;
            modify(
                scheduler,
                core::SCHEDULER_CONTROL,
                core::SCHEDULER_MODE_HDMI,
                0,
            )?;
            self.poll(SCHEDULER_BUDGET_US, || {
                Ok(scheduler.read32(core::SCHEDULER_CONTROL)? & core::SCHEDULER_HDMI_ACTIVE == 0)
            })?;
        }
        self.recenter_fifo()?;
        if chosen.scrambled {
            sink::set_scrambling(&mut self.ddc, true)?;
            modify(
                &self.regs.core,
                core::SCRAMBLER_CTL,
                0,
                core::SCRAMBLER_ENABLE,
            )?;
            self.scrambler_check_us = Some(self.delay.now_us() + SCRAMBLER_CHECK_US);
        }
        self.sending = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), DriverError> {
        self.scrambler_check_us = None;
        self.sending = false;
        if self.holds.video.is_none() {
            return Ok(());
        }
        let vid_ctl = self.variant.vid_ctl;
        let packets = self.regs.core.write32(core::RAM_PACKET_CONFIG, 0);
        let cleared = modify(&self.regs.hd, vid_ctl, 0, hd::VID_CTL_CLRRGB);
        self.delay.delay_us(POLL_US);
        let stopped = modify(&self.regs.hd, vid_ctl, hd::VID_CTL_ENABLE, 0);
        let unscrambled = match self.scrambling() {
            Ok(false) => Ok(()),
            Ok(true) => {
                let disabled = modify(
                    &self.regs.core,
                    core::SCRAMBLER_CTL,
                    core::SCRAMBLER_ENABLE,
                    0,
                );
                // A sink already gone cannot be told; it resets its own state.
                let _ = sink::set_scrambling(&mut self.ddc, false);
                disabled
            }
            Err(err) => Err(err),
        };
        first([packets, cleared, stopped, unscrambled])
    }

    fn shut_down(&mut self) -> Result<(), DriverError> {
        self.scrambler_check_us = None;
        self.sending = false;
        if self.holds.video.is_none() {
            return Ok(());
        }
        let blanked = modify(&self.regs.hd, self.variant.vid_ctl, 0, hd::VID_CTL_BLANKPIX);
        let reset = phy::reset(&self.regs.phy);
        let gated = modify(&self.regs.dvp, dvp::CLOCK_STOP, 0, dvp::CLOCK_STOP_PIXEL);
        let bvb = if self.holds.bvb {
            self.holds.bvb = false;
            self.bvb.release().map_err(clock::refusal)
        } else {
            Ok(())
        };
        self.holds.video = None;
        let hsm = self.settle_hsm();
        first([blanked, reset, gated, bvb, hsm])
    }
}

impl<R, C, P, D, L, M> Encoder for Hdmi<R, C, P, D, L, M>
where
    R: RegisterBlock,
    C: LinkCall,
    P: Ddc,
    D: Delay,
    L: Recorder<Record>,
    M: DmaPort,
{
    fn connector(&self) -> Connector {
        Connector {
            connected: self.connected,
            mode: self.chosen.map(|chosen| chosen.timing),
            link: self.link,
        }
    }

    fn phase(&self) -> Phase {
        self.found
    }

    fn limit(&mut self, limits: &PipeLimits) -> Result<bool, DriverError> {
        let before = self.connector();
        self.limits = *limits;
        self.choose();
        Ok(self.connector() != before)
    }

    fn service_event(&mut self, _index: usize) -> Result<bool, DriverError> {
        let mut fired = false;
        for line in &mut self.hotplug {
            if !line.reap_wait().map_err(DriverError::from_errno)? {
                continue;
            }
            fired = true;
            line.post_wait(CALL_DEADLINE_NONE)
                .map_err(DriverError::from_errno)?;
        }
        if !fired {
            return Ok(false);
        }
        self.probe()
    }

    fn deadline_us(&self) -> Option<u64> {
        self.scrambler_check_us
    }

    /// Ask the sink whether it sees the link scrambled, and tell it again
    /// until it does, as Linux's `vc4_hdmi_scrambling_wq` does.
    fn expired(&mut self) -> Result<bool, DriverError> {
        self.scrambler_check_us = None;
        if !self.scrambling()? {
            return Ok(false);
        }
        // A sink that does not answer is asked again; one that left is
        // its hotplug's to report.
        if !sink::scrambling_seen(&mut self.ddc).unwrap_or(false) {
            self.log.record(Record::Unscrambled);
            let _ = sink::set_scrambling(&mut self.ddc, true);
            self.scrambler_check_us = Some(self.delay.now_us() + SCRAMBLER_CHECK_US);
        }
        Ok(false)
    }

    fn configure(&mut self, timing: &Timing) -> Result<(), DriverError> {
        let chosen = self.chosen()?;
        if chosen.timing != *timing {
            return Err(DriverError::Stale);
        }
        self.set_up(&chosen).inspect_err(|_| {
            let _ = self.shut_down();
        })
    }

    fn prepare(&mut self) -> Result<(), DriverError> {
        let chosen = self.powered_chosen()?;
        let csc_regs = &self.regs.csc;
        for (pair, row) in csc::COEFFICIENTS
            .chunks(2)
            .zip(csc_rows(limited_range(&chosen)))
        {
            csc_regs.write32(pair[0], row[1] << 16 | row[0])?;
            csc_regs.write32(pair[1], row[3] << 16 | row[2])?;
        }
        self.regs.dvp.write32(dvp::VEC_INTERFACE_CFG, 0)?;
        self.regs
            .dvp
            .write32(dvp::VEC_INTERFACE_XBAR, dvp::XBAR_RGB)?;
        csc_regs.write32(csc::CHANNEL_CTL, 0)?;
        csc_regs.write32(csc::CTL, csc::CTL_ENABLE | csc::CTL_MODE_CUSTOM)?;
        self.regs
            .core
            .write32(core::FIFO_CTL, core::FIFO_MASTER_SLAVE_N)
    }

    fn enable(&mut self) -> Result<(), DriverError> {
        let chosen = self.powered_chosen()?;
        self.start(&chosen).inspect_err(|_| {
            let _ = self.stop();
        })
    }

    fn disable(&mut self) -> Result<(), DriverError> {
        self.stop()
    }

    fn power_down(&mut self) -> Result<(), DriverError> {
        self.shut_down()
    }
}
