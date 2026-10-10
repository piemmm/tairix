//! Modelled codecs for the host tests: each answers the verbs the
//! specification defines from a description of its widgets, and keeps the
//! state a driver sets, so a test reads back what the engine programmed.

extern crate std;

use std::collections::BTreeMap;
use std::vec::Vec;

use tairix_abi::DriverError;

use crate::codec::Verbs;
use crate::verb::{self, param, Verb};

/// One modelled widget: what it states, and what has been set on it.
#[derive(Clone, Debug, Default)]
pub struct ModelWidget {
    pub nid: u8,
    pub caps: u32,
    pub sources: Vec<u8>,
    pub pin_caps: u32,
    pub config: u32,
    pub input_amp: u32,
    pub output_amp: u32,
    pub pcm: u32,
    pub streams: u32,
    pub select: u8,
    pub pin_control: u8,
    pub unsolicited: u8,
    pub eapd: u8,
    pub power: u8,
    pub format: u16,
    pub stream_channel: u8,
    pub digital: u8,
    pub channel_count: u8,
    /// Gain and mute by amplifier: output or input, index, left or right.
    pub amps: BTreeMap<(bool, u8, bool), u8>,
    pub present: bool,
    pub eld: Vec<u8>,
    pub dip: Vec<u8>,
    dip_index: u8,
}

impl ModelWidget {
    pub fn new(nid: u8, caps: u32) -> Self {
        Self {
            nid,
            caps,
            power: 3,
            ..Self::default()
        }
    }

    pub fn sources(mut self, sources: &[u8]) -> Self {
        self.sources = sources.to_vec();
        self.caps |= 1 << 8;
        self
    }

    pub fn pin(mut self, pin_caps: u32, config: u32) -> Self {
        self.pin_caps = pin_caps;
        self.config = config;
        self
    }

    pub fn amps(mut self, input: u32, output: u32) -> Self {
        self.input_amp = input;
        self.output_amp = output;
        self.caps |= 1 << 3;
        if input != 0 {
            self.caps |= 1 << 1;
        }
        if output != 0 {
            self.caps |= 1 << 2;
        }
        self
    }

    pub fn pcm(mut self, pcm: u32, streams: u32) -> Self {
        self.pcm = pcm;
        self.streams = streams;
        self.caps |= 1 << 4;
        self
    }

    /// The gain and mute byte an amplifier holds.
    pub fn amp(&self, output: bool, index: u8, left: bool) -> Option<u8> {
        self.amps.get(&(output, index, left)).copied()
    }
}

/// Widget capability words, by kind.
pub mod kind {
    pub const OUTPUT: u32 = 0 << 20;
    pub const INPUT: u32 = 1 << 20;
    pub const MIXER: u32 = 2 << 20;
    pub const SELECTOR: u32 = 3 << 20;
    pub const PIN: u32 = 4 << 20;
    pub const STEREO: u32 = 1;
    pub const DIGITAL: u32 = 1 << 9;
    pub const UNSOLICITED: u32 = 1 << 7;
}

/// Pin capability bits.
pub mod pin {
    pub const PRESENCE: u32 = 1 << 2;
    pub const HEADPHONE: u32 = 1 << 3;
    pub const OUT: u32 = 1 << 4;
    pub const IN: u32 = 1 << 5;
    pub const HDMI: u32 = 1 << 7;
    pub const VREF_80: u32 = 1 << 10;
    pub const EAPD: u32 = 1 << 16;
}

/// An ELD naming `monitor`: the four-byte header, the baseline block's fixed
/// sixteen bytes, and the name.
#[must_use]
pub fn eld(monitor: &str) -> Vec<u8> {
    let name = monitor.as_bytes();
    let mut eld = std::vec![0u8; 20 + name.len()];
    eld[0] = 2 << 3;
    eld[2] = u8::try_from((16 + name.len()).div_ceil(4)).unwrap_or(0);
    eld[4] = u8::try_from(name.len()).unwrap_or(0) & 0x1F;
    eld[20..].copy_from_slice(name);
    eld
}

/// A pin configuration default: connectivity, location, device, colour,
/// association and sequence.
#[must_use]
pub const fn config(
    connectivity: u32,
    location: u32,
    device: u32,
    colour: u32,
    association: u32,
    sequence: u32,
) -> u32 {
    (connectivity << 30)
        | (location << 24)
        | (device << 20)
        | (colour << 12)
        | (association << 4)
        | sequence
}

/// QEMU's amplifier: mutable, 74 steps of 1 dB, 0 dB at the top.
pub const QEMU_AMP: u32 = (1 << 31) | (3 << 16) | (0x4A << 8) | 0x4A;

/// 16-bit PCM at 16 to 96 kHz, as QEMU states it.
pub const QEMU_PCM: u32 = (1 << 17) | 0x1FC;

/// One modelled codec.
#[derive(Clone, Debug, Default)]
pub struct ModelCodec {
    pub vendor: u32,
    pub afg: u8,
    pub afg_pcm: u32,
    pub afg_streams: u32,
    pub afg_input_amp: u32,
    pub afg_output_amp: u32,
    pub afg_power: u8,
    pub widgets: Vec<ModelWidget>,
    /// Every verb that set something, in order.
    pub sets: Vec<Verb>,
}

impl ModelCodec {
    /// QEMU's `hda-output`: one converter feeding one line-out jack.
    pub fn qemu_output() -> Self {
        Self {
            vendor: 0x1AF4_0012,
            afg: 1,
            afg_pcm: QEMU_PCM,
            afg_streams: 1,
            widgets: std::vec![
                ModelWidget::new(2, kind::OUTPUT | kind::STEREO)
                    .pcm(QEMU_PCM, 1)
                    .amps(0, QEMU_AMP),
                ModelWidget::new(3, kind::PIN | kind::STEREO)
                    .sources(&[2])
                    .pin(1 << 4, config(0, 0, 0x0, 0x4, 1, 0)),
            ],
            ..Self::default()
        }
    }

    /// A desktop codec: four converters behind a seven-point-one rear panel,
    /// a front headphone jack that finds none of its own, a loopback mixer
    /// that makes the graph cyclic, S/PDIF, and three inputs behind two
    /// selectors.
    pub fn desktop() -> Self {
        let dac = |nid| ModelWidget::new(nid, kind::OUTPUT | kind::STEREO).amps(0, QEMU_AMP);
        let mixer = |nid, sources: &[u8]| {
            ModelWidget::new(nid, kind::MIXER | kind::STEREO)
                .sources(sources)
                .amps(1 << 31, 0)
        };
        let output = |nid, sources: &[u8], location, device, colour, association, sequence| {
            ModelWidget::new(nid, kind::PIN | kind::STEREO | kind::UNSOLICITED)
                .sources(sources)
                .pin(
                    pin::OUT | pin::PRESENCE | pin::EAPD,
                    config(0, location, device, colour, association, sequence),
                )
        };
        let input = |nid, connectivity, location, device, colour| {
            ModelWidget::new(nid, kind::PIN | kind::STEREO)
                .pin(
                    pin::IN | pin::VREF_80,
                    config(connectivity, location, device, colour, 3, 0),
                )
                .amps(0x0003_0003, 0)
        };
        let adc = |nid, sources: &[u8]| {
            ModelWidget::new(nid, kind::INPUT | kind::STEREO)
                .sources(sources)
                .pcm(QEMU_PCM, 1)
                .amps(QEMU_AMP, 0)
        };
        Self {
            vendor: 0x10EC_0887,
            afg: 1,
            afg_pcm: QEMU_PCM,
            afg_streams: 1,
            widgets: std::vec![
                dac(0x02),
                dac(0x03),
                dac(0x04),
                dac(0x05),
                ModelWidget::new(0x06, kind::OUTPUT | kind::STEREO | kind::DIGITAL),
                adc(0x08, &[0x23]),
                adc(0x09, &[0x22]),
                mixer(0x0B, &[0x18, 0x1A, 0x0C]),
                mixer(0x0C, &[0x02, 0x0B]),
                mixer(0x0D, &[0x03]),
                mixer(0x0E, &[0x04]),
                mixer(0x0F, &[0x05]),
                input(0x12, 2, 0x10, 0xA, 0),
                output(0x14, &[0x0C], 0x01, 0x0, 0x4, 1, 0),
                output(0x15, &[0x0D], 0x01, 0x0, 0x1, 1, 2),
                output(0x16, &[0x0E], 0x01, 0x0, 0x6, 1, 1),
                output(0x17, &[0x0F], 0x01, 0x0, 0x2, 1, 4),
                input(0x18, 0, 0x01, 0xA, 0x9),
                ModelWidget::new(0x19, kind::PIN).pin(pin::OUT, config(1, 0, 0x0, 0, 0xF, 0)),
                input(0x1A, 0, 0x01, 0x8, 0x3),
                output(0x1B, &[0x0C, 0x0D], 0x02, 0x2, 0x1, 2, 0),
                ModelWidget::new(0x1E, kind::PIN | kind::DIGITAL)
                    .sources(&[0x06])
                    .pin(pin::OUT, config(0, 0x01, 0x4, 0, 4, 0)),
                ModelWidget::new(0x22, kind::SELECTOR | kind::STEREO).sources(&[0x18, 0x1A, 0x12]),
                ModelWidget::new(0x23, kind::SELECTOR | kind::STEREO).sources(&[0x18, 0x1A, 0x12]),
            ],
            ..Self::default()
        }
    }

    /// A display codec: one digital converter feeding one HDMI connector,
    /// with `monitor` attached when one is named.
    pub fn display(monitor: Option<&str>) -> Self {
        let mut connector = ModelWidget::new(
            0x03,
            kind::PIN | kind::STEREO | kind::DIGITAL | kind::UNSOLICITED,
        )
        .sources(&[0x02])
        .pin(
            pin::OUT | pin::PRESENCE | pin::HDMI,
            config(0, 0x18, 0x5, 0, 1, 0),
        );
        if let Some(monitor) = monitor {
            connector.present = true;
            connector.eld = eld(monitor);
        }
        Self {
            vendor: 0x8086_2812,
            afg: 1,
            afg_pcm: QEMU_PCM,
            afg_streams: 1,
            widgets: std::vec![
                ModelWidget::new(0x02, kind::OUTPUT | kind::STEREO | kind::DIGITAL),
                connector,
            ],
            ..Self::default()
        }
    }

    pub fn widget(&self, nid: u8) -> &ModelWidget {
        self.widgets
            .iter()
            .find(|widget| widget.nid == nid)
            .expect("modelled")
    }

    pub fn widget_mut(&mut self, nid: u8) -> &mut ModelWidget {
        self.widgets
            .iter_mut()
            .find(|widget| widget.nid == nid)
            .expect("modelled")
    }

    fn function_parameter(&self, parameter: u8) -> u32 {
        let first = self
            .widgets
            .iter()
            .map(|widget| widget.nid)
            .min()
            .unwrap_or(0);
        let last = self
            .widgets
            .iter()
            .map(|widget| widget.nid)
            .max()
            .unwrap_or(0);
        match parameter {
            param::NODE_COUNT => (u32::from(first) << 16) | u32::from(last - first + 1),
            param::FUNCTION_GROUP => verb::AUDIO_FUNCTION_GROUP | (1 << 8),
            param::PCM => self.afg_pcm,
            param::STREAM_FORMATS => self.afg_streams,
            param::INPUT_AMP => self.afg_input_amp,
            param::OUTPUT_AMP => self.afg_output_amp,
            _ => 0,
        }
    }

    /// The answer to `verb`.
    pub fn answer(&mut self, verb: Verb) -> u32 {
        let nid = verb.nid();
        let body = verb.body();
        let short = ((body >> 8) & 0xFFF) as u16;
        let payload = (body & 0xFF) as u8;
        if nid == 0 {
            return match (short, payload) {
                (verb::GET_PARAMETER, param::VENDOR_ID) => self.vendor,
                (verb::GET_PARAMETER, param::NODE_COUNT) => (u32::from(self.afg) << 16) | 1,
                _ => 0,
            };
        }
        if nid == self.afg {
            if short == verb::GET_PARAMETER {
                return self.function_parameter(payload);
            }
            self.sets.push(verb);
            if short == verb::SET_POWER_STATE {
                self.afg_power = payload;
            }
            return 0;
        }
        self.widget_answer(verb, short, payload)
    }

    fn widget_answer(&mut self, verb: Verb, short: u16, payload: u8) -> u32 {
        let (nid, body) = (verb.nid(), verb.body());
        let Some(widget) = self.widgets.iter_mut().find(|widget| widget.nid == nid) else {
            // A node the codec numbers but does not describe here: a vendor's.
            return if short == verb::GET_PARAMETER && payload == param::WIDGET_CAPS {
                0xF << 20
            } else {
                0
            };
        };
        match ((body >> 16) & 0xF) as u8 {
            verb::SET_FORMAT => {
                widget.format = (body & 0xFFFF) as u16;
                self.sets.push(verb);
                return 0;
            }
            verb::SET_AMP => {
                let payload = (body & 0xFFFF) as u16;
                let byte = (payload & 0xFF) as u8;
                let index = ((payload >> verb::amp::INDEX_SHIFT) & 0xF) as u8;
                for (output, wanted) in [(true, verb::amp::OUTPUT), (false, verb::amp::INPUT)] {
                    for (left, side) in [(true, verb::amp::LEFT), (false, verb::amp::RIGHT)] {
                        if payload & wanted != 0 && payload & side != 0 {
                            widget.amps.insert((output, index, left), byte);
                        }
                    }
                }
                self.sets.push(verb);
                return 0;
            }
            _ => {}
        }
        match short {
            verb::GET_PARAMETER => match payload {
                param::WIDGET_CAPS => widget.caps,
                param::PCM => widget.pcm,
                param::STREAM_FORMATS => widget.streams,
                param::PIN_CAPS => widget.pin_caps,
                param::INPUT_AMP => widget.input_amp,
                param::OUTPUT_AMP => widget.output_amp,
                param::CONNECTION_LENGTH => u32::try_from(widget.sources.len()).unwrap_or(0),
                _ => 0,
            },
            verb::GET_CONNECTION_LIST => (0..4)
                .filter_map(|slot| {
                    widget
                        .sources
                        .get(usize::from(payload) + slot)
                        .map(|&source| u32::from(source) << (slot * 8))
                })
                .fold(0, |word, entry| word | entry),
            verb::GET_CONFIG_DEFAULT => widget.config,
            verb::GET_PIN_SENSE => {
                let mut sense = 0;
                if widget.present {
                    sense |= verb::pin_sense::PRESENT;
                    if !widget.eld.is_empty() {
                        sense |= verb::pin_sense::ELD_VALID;
                    }
                }
                sense
            }
            verb::GET_DIP_SIZE => u32::try_from(widget.eld.len()).unwrap_or(0),
            verb::GET_ELD_DATA => widget
                .eld
                .get(usize::from(payload))
                .map_or(0, |&byte| (1 << 31) | u32::from(byte)),
            _ => {
                self.sets.push(verb);
                match short {
                    verb::SET_CONNECTION_SELECT => widget.select = payload,
                    verb::SET_PIN_CONTROL => widget.pin_control = payload,
                    verb::SET_UNSOLICITED => widget.unsolicited = payload,
                    verb::SET_EAPD => widget.eapd = payload,
                    verb::SET_POWER_STATE => widget.power = payload,
                    verb::SET_STREAM_CHANNEL => widget.stream_channel = payload,
                    verb::SET_DIGITAL_CONTROL => widget.digital = payload,
                    verb::SET_CHANNEL_COUNT => widget.channel_count = payload,
                    verb::SET_DIP_INDEX => {
                        widget.dip_index = payload;
                        widget.dip.clear();
                    }
                    verb::SET_DIP_DATA => widget.dip.push(payload),
                    _ => {}
                }
                0
            }
        }
    }
}

/// A codec answering on one link address.
pub struct ModelLink {
    pub codecs: BTreeMap<u8, ModelCodec>,
}

impl Verbs for ModelLink {
    fn exchange(&mut self, address: u8, verb: Verb) -> Result<u32, DriverError> {
        self.codecs
            .get_mut(&address)
            .map(|codec| codec.answer(verb))
            .ok_or(DriverError::DeviceFault)
    }
}

use std::boxed::Box;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use core::ptr::NonNull;

use tairix_abi::driver::dma::{DmaHost, DmaSlab, PoolId};
use tairix_abi::time::MonotonicClock;
use tairix_abi::Delay;

use crate::controller::Wait;
use crate::regs::{self, descriptor, gctl, intr, rirbsts, sd, Registers};

/// Where the modelled DMA host places its memory: above 4 GiB, as a 64-bit
/// controller may be handed it.
const DEVICE_BASE: u64 = 0x1_0000_0000;

/// The memory the modelled host handed out, found by device address.
#[derive(Default)]
pub struct ModelMemory {
    regions: RefCell<Vec<(u64, NonNull<u8>, usize)>>,
    next: Cell<u64>,
}

impl ModelMemory {
    fn locate(&self, address: u64, len: usize) -> NonNull<u8> {
        let regions = self.regions.borrow();
        let &(base, ptr, size) = regions
            .iter()
            .find(|&&(base, _, size)| address >= base && address + len as u64 <= base + size as u64)
            .expect("a device address the host handed out");
        let offset = usize::try_from(address - base).expect("in range");
        let _ = size;
        // SAFETY: `offset + len` lies within the region of `size` bytes `ptr`
        // was leaked with.
        unsafe { NonNull::new_unchecked(ptr.as_ptr().add(offset)) }
    }

    /// Copy `len` bytes at device address `address`.
    pub fn read(&self, address: u64, len: usize) -> Vec<u8> {
        let ptr = self.locate(address, len);
        // SAFETY: `locate` found `len` bytes of leaked storage at `ptr`, and no
        // reference into them is live while the model runs.
        unsafe { core::slice::from_raw_parts(ptr.as_ptr(), len) }.to_vec()
    }

    /// Store `bytes` at device address `address`.
    pub fn write(&self, address: u64, bytes: &[u8]) {
        let ptr = self.locate(address, bytes.len());
        // SAFETY: as for `read`.
        unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), bytes.len()) }
            .copy_from_slice(bytes);
    }

    fn word(&self, address: u64) -> u32 {
        let bytes = self.read(address, 4);
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }
}

/// A DMA host minting 128-byte-aligned slabs over leaked storage the model
/// reaches by device address.
pub struct ModelDma {
    pub memory: Rc<ModelMemory>,
    pub quiesced: Cell<usize>,
}

impl DmaHost for ModelDma {
    fn alloc_dma_zeroed(&self, size: usize) -> Result<DmaSlab, tairix_abi::DriverError> {
        let storage: Box<[u128]> = std::vec![0u128; size.div_ceil(16) + 8].into_boxed_slice();
        let base = Box::leak(storage).as_mut_ptr().cast::<u8>();
        let align = base.align_offset(128);
        // SAFETY: the storage holds 128 spare bytes past `size`, so the
        // aligned start and `size` bytes after it stay inside it.
        let ptr = NonNull::new(unsafe { base.add(align) }).expect("leaked storage is non-null");
        let address = DEVICE_BASE + self.memory.next.get();
        self.memory
            .next
            .set(self.memory.next.get() + (size as u64).div_ceil(4096) * 4096);
        self.memory.regions.borrow_mut().push((address, ptr, size));
        // SAFETY: `ptr` covers `size` zeroed bytes leaked for the whole test
        // process, reached otherwise only through the model while no slab
        // borrow is live; `address` is its device-visible base.
        Ok(unsafe { DmaSlab::from_leaked(address, ptr, size, PoolId::MOCK, 0) })
    }

    fn device_quiesced(&self) {
        self.quiesced.set(self.quiesced.get() + 1);
    }
}

/// Parks that count, and let a held response through; a park with nothing
/// to wake it sleeps its whole budget on the shared clock.
#[derive(Clone, Default)]
pub struct ModelWait {
    pub parks: Rc<Cell<u32>>,
    pub now_us: Rc<Cell<u64>>,
}

impl Wait for ModelWait {
    fn park(&self, budget_ns: u64) -> Result<(), tairix_abi::DriverError> {
        self.parks.set(self.parks.get() + 1);
        self.now_us
            .set(self.now_us.get() + budget_ns.div_ceil(1_000));
        Ok(())
    }
}

/// A clock that moves only when waited on, shared with the parks.
#[derive(Clone, Default)]
pub struct ModelDelay {
    pub now_us: Rc<Cell<u64>>,
}

impl Delay for ModelDelay {
    fn delay_us(&self, us: u32) {
        self.now_us.set(self.now_us.get() + u64::from(us));
    }

    fn now_us(&self) -> u64 {
        self.now_us.get()
    }
}

/// A clock that ticks a microsecond each time it is read.
#[derive(Default)]
pub struct ModelClock {
    now_ns: Cell<u64>,
}

impl MonotonicClock for ModelClock {
    fn now_ns(&self) -> u64 {
        self.now_ns.set(self.now_ns.get() + 1_000);
        self.now_ns.get()
    }
}

/// One descriptor's DMA progress.
#[derive(Clone, Debug, Default)]
struct Dma {
    entry: u16,
    within: u32,
    position: u32,
}

/// The register-level controller: four input and four output descriptors as
/// QEMU's, 256-entry rings, 64-bit addressing, and the codecs on its link.
pub struct ModelController {
    file: [u8; 0x200],
    pub link: BTreeMap<u8, ModelCodec>,
    pub memory: Rc<ModelMemory>,
    corb_read: u16,
    rirb_write: Cell<u16>,
    rirb_status: Cell<u8>,
    dma: BTreeMap<u8, Dma>,
    /// What each output descriptor has played, in order.
    pub played: BTreeMap<u8, Vec<u8>>,
    /// The controller never leaves reset.
    pub stuck_in_reset: bool,
    /// Responses become visible only once the engine has parked
    /// [`Self::hold_parks`] times since they were sent.
    pub hold_responses: Option<Rc<Cell<u32>>>,
    pub hold_parks: u32,
    held: RefCell<Vec<(u32, u32, u32)>>,
}

const MODEL_GCAP: u16 = (4 << 12) | (4 << 8) | 1;
const DESCRIPTORS: u8 = 8;

impl ModelController {
    pub fn new(link: BTreeMap<u8, ModelCodec>, memory: Rc<ModelMemory>) -> Self {
        let mut model = Self {
            file: [0; 0x200],
            link,
            memory,
            corb_read: 0,
            rirb_write: Cell::new(0),
            rirb_status: Cell::new(0),
            dma: BTreeMap::new(),
            played: BTreeMap::new(),
            stuck_in_reset: false,
            hold_responses: None,
            hold_parks: 1,
            held: RefCell::new(Vec::new()),
        };
        model.store16(regs::GCAP, MODEL_GCAP);
        model.store8(regs::CORBSIZE, 0x42 | regs::ring_size::CAP_256);
        model.store8(regs::RIRBSIZE, 0x42 | regs::ring_size::CAP_256);
        model
    }

    fn load32(&self, offset: usize) -> u32 {
        u32::from_le_bytes([
            self.file[offset],
            self.file[offset + 1],
            self.file[offset + 2],
            self.file[offset + 3],
        ])
    }

    fn load16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.file[offset], self.file[offset + 1]])
    }

    fn store32(&mut self, offset: usize, value: u32) {
        self.file[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn store16(&mut self, offset: usize, value: u16) {
        self.file[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn store8(&mut self, offset: usize, value: u8) {
        self.file[offset] = value;
    }

    fn address(&self, lower: usize, upper: usize) -> u64 {
        u64::from(self.load32(lower)) | (u64::from(self.load32(upper)) << 32)
    }

    /// Answer every command queued up to the write pointer.
    fn run_commands(&mut self) {
        let base = self.address(regs::CORBLBASE, regs::CORBUBASE);
        let written = self.load16(regs::CORBWP) & 0xFF;
        while self.corb_read != written {
            self.corb_read = (self.corb_read + 1) % 256;
            let command = self.memory.word(base + u64::from(self.corb_read) * 4);
            let (address, _, verb) = Verb::from_command(command);
            let response = self.link.get_mut(&address).map(|codec| codec.answer(verb));
            if let Some(response) = response {
                let parks = self.hold_responses.as_ref().map_or(0, |parks| parks.get());
                self.held
                    .borrow_mut()
                    .push((response, u32::from(address), parks));
            }
        }
        self.release_responses();
    }

    /// Post the responses whose hold has passed: at once, or once the engine
    /// has parked since they were sent.
    fn release_responses(&self) {
        let parks = self.hold_responses.as_ref().map(|parks| parks.get());
        let ready: Vec<_> = {
            let mut held = self.held.borrow_mut();
            let (ready, waiting): (Vec<_>, Vec<_>) = held
                .drain(..)
                .partition(|&(_, _, sent)| parks.is_none_or(|now| now >= sent + self.hold_parks));
            *held = waiting;
            ready
        };
        for (response, extended, _) in ready {
            self.post(response, extended);
        }
    }

    /// Post one response.
    fn post(&self, response: u32, extended: u32) {
        let base = self.address(regs::RIRBLBASE, regs::RIRBUBASE);
        let write = (self.rirb_write.get() + 1) % 256;
        self.rirb_write.set(write);
        let at = base + u64::from(write) * 8;
        self.memory.write(at, &response.to_le_bytes());
        self.memory.write(at + 4, &extended.to_le_bytes());
        self.rirb_status
            .set(self.rirb_status.get() | rirbsts::RESPONSE);
    }

    /// Post an unsolicited response tagged `tag` from the codec at `address`.
    pub fn unsolicited(&mut self, address: u8, tag: u8) {
        self.post(u32::from(tag) << 26, u32::from(address) | (1 << 4));
    }

    fn interrupt_status(&self) -> u32 {
        let mut status = 0;
        if self.rirb_status.get() & (rirbsts::RESPONSE | rirbsts::OVERRUN) != 0 {
            status |= intr::CONTROLLER;
        }
        for index in 0..DESCRIPTORS {
            if self.file[descriptor(index) + sd::STS] & sd::STATUS_MASK != 0 {
                status |= 1 << index;
            }
        }
        if status != 0 {
            status |= intr::GLOBAL;
        }
        status
    }

    /// Move `bytes` through every running descriptor: an output's buffer to
    /// what it played, an input's buffer filled with a count of the bytes it
    /// has captured.
    pub fn tick(&mut self, bytes: u32) {
        for index in 0..DESCRIPTORS {
            let base = descriptor(index);
            if self.file[base + sd::CTL] & sd::RUN == 0 {
                continue;
            }
            let output = index >= 4;
            let bdl = self.address(base + sd::BDPL, base + sd::BDPU);
            let last = self.load16(base + sd::LVI);
            let length = self.load32(base + sd::CBL);
            let mut state = self.dma.remove(&index).unwrap_or_default();
            let mut left = bytes;
            while left > 0 {
                let entry = bdl + u64::from(state.entry) * 16;
                let address = u64::from(self.memory.word(entry))
                    | (u64::from(self.memory.word(entry + 4)) << 32);
                let size = self.memory.word(entry + 8);
                let ioc = self.memory.word(entry + 12) & 1 != 0;
                let step = left.min(size - state.within);
                let at = address + u64::from(state.within);
                if output {
                    let samples = self.memory.read(at, step as usize);
                    self.played.entry(index).or_default().extend(samples);
                } else {
                    let counted: Vec<u8> = (0..step)
                        .map(|offset| ((state.position + offset) & 0xFF) as u8)
                        .collect();
                    self.memory.write(at, &counted);
                }
                state.within += step;
                state.position = (state.position + step) % length;
                left -= step;
                if state.within == size {
                    state.within = 0;
                    state.entry = if state.entry >= last {
                        0
                    } else {
                        state.entry + 1
                    };
                    if ioc {
                        self.file[base + sd::STS] |= sd::BCIS;
                    }
                }
            }
            if self.load32(regs::DPLBASE) & regs::DMA_POSITION_ENABLE != 0 {
                let positions = self.address(regs::DPLBASE, regs::DPUBASE) & !0x7F;
                self.memory.write(
                    positions + u64::from(index) * 8,
                    &state.position.to_le_bytes(),
                );
            }
            self.dma.insert(index, state);
        }
    }

    /// Raise a FIFO error on descriptor `index`.
    pub fn starve(&mut self, index: u8) {
        self.file[descriptor(index) + sd::STS] |= sd::FIFOE;
    }

    /// Descriptor `index`'s register at `offset` within it.
    pub fn descriptor_register(&self, index: u8, offset: usize) -> u32 {
        self.load32(descriptor(index) + offset)
    }

    /// Whether descriptor `index` runs.
    pub fn running(&self, index: u8) -> bool {
        self.file[descriptor(index) + sd::CTL] & sd::RUN != 0
    }
}

impl Registers for ModelController {
    fn read8(&self, offset: usize) -> Result<u8, tairix_abi::DriverError> {
        match offset {
            regs::INTSTS => Ok((self.interrupt_status() & 0xFF) as u8),
            regs::RIRBSTS => Ok(self.rirb_status.get()),
            _ => Ok(self.file[offset]),
        }
    }

    fn read16(&self, offset: usize) -> Result<u16, tairix_abi::DriverError> {
        if offset == regs::RIRBWP {
            self.release_responses();
            return Ok(self.rirb_write.get());
        }
        Ok(self.load16(offset))
    }

    fn read32(&self, offset: usize) -> Result<u32, tairix_abi::DriverError> {
        if offset == regs::INTSTS {
            return Ok(self.interrupt_status());
        }
        Ok(self.load32(offset))
    }

    fn write8(&mut self, offset: usize, value: u8) -> Result<(), tairix_abi::DriverError> {
        if offset == regs::CORBCTL && value & regs::RING_RUN == 0 {
            self.held.borrow_mut().clear();
        }
        if offset == regs::RIRBSTS {
            self.rirb_status.set(self.rirb_status.get() & !value);
            return Ok(());
        }
        for index in 0..DESCRIPTORS {
            let base = descriptor(index);
            if offset == base + sd::STS {
                self.file[offset] &= !value;
                return Ok(());
            }
            if offset == base + sd::CTL {
                if value & sd::SRST != 0 {
                    self.file[base..base + regs::SD_STRIDE].fill(0);
                    self.dma.remove(&index);
                }
                self.file[offset] = value;
                return Ok(());
            }
        }
        self.file[offset] = value;
        Ok(())
    }

    fn write16(&mut self, offset: usize, value: u16) -> Result<(), tairix_abi::DriverError> {
        match offset {
            regs::STATESTS => {
                let current = self.load16(offset);
                self.store16(offset, current & !value);
            }
            regs::CORBRP => {
                if value & regs::POINTER_RESET != 0 {
                    self.corb_read = 0;
                }
                self.store16(offset, value & regs::POINTER_RESET);
            }
            regs::RIRBWP => {
                if value & regs::POINTER_RESET != 0 {
                    self.rirb_write.set(0);
                }
            }
            regs::CORBWP => {
                self.store16(offset, value);
                self.run_commands();
            }
            _ => self.store16(offset, value),
        }
        Ok(())
    }

    fn write32(&mut self, offset: usize, value: u32) -> Result<(), tairix_abi::DriverError> {
        if offset == regs::GCTL {
            let reset_left = value & gctl::CRST != 0 && !self.stuck_in_reset;
            self.store32(
                offset,
                if reset_left {
                    value
                } else {
                    value & !gctl::CRST
                },
            );
            if reset_left {
                let present = self
                    .link
                    .keys()
                    .fold(0u16, |mask, &address| mask | (1 << address));
                self.store16(regs::STATESTS, present);
            }
            return Ok(());
        }
        self.store32(offset, value);
        Ok(())
    }
}

/// A controller model and everything the engine opens it with.
pub struct Bench {
    pub dma: ModelDma,
    pub clock: ModelClock,
    pub wait: ModelWait,
    pub delay: ModelDelay,
}

impl Bench {
    pub fn new() -> Self {
        let wait = ModelWait::default();
        let delay = ModelDelay {
            now_us: Rc::clone(&wait.now_us),
        };
        Self {
            dma: ModelDma {
                memory: Rc::new(ModelMemory::default()),
                quiesced: Cell::new(0),
            },
            clock: ModelClock::default(),
            wait,
            delay,
        }
    }

    /// A controller with `codecs` on its link, sharing this bench's memory.
    pub fn controller(
        &self,
        codecs: impl IntoIterator<Item = (u8, ModelCodec)>,
    ) -> ModelController {
        ModelController::new(codecs.into_iter().collect(), Rc::clone(&self.dma.memory))
    }
}
