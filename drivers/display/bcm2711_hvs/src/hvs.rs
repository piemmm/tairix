//! The HVS: its global setup, its channels, the outputs they are muxed to,
//! and the display-list memory every list lives in.

use tairix_abi::driver::display::MAX_ACCEL_LAYERS;
use tairix_abi::driver::mmio::RegisterBlock;
use tairix_abi::DriverError;

use crate::dlist::{kernel, PlaneWords, END, KERNEL_WORDS, PLANE_WORDS_MAX};
use crate::regs::field;
use crate::regs::hvs as reg;

/// An HVS output a pixel valve reads, which a mux feeds from one channel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Output {
    /// Output 4, read by the pixel valve feeding HDMI0.
    Four,
    /// Output 5, read by the pixel valve feeding HDMI1.
    Five,
}

impl Output {
    /// The register holding the output's mux, and the field's shift.
    const fn mux(self) -> (usize, u32) {
        match self {
            Self::Four => (reg::DISPEOLN, reg::DISPEOLN_DSP4_MUX_SHIFT),
            Self::Five => (reg::DISPDITHER, reg::DISPDITHER_DSP5_MUX_SHIFT),
        }
    }
}

/// A list a channel can latch.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum List {
    /// Nothing: the background alone, black.
    Black,
    /// The boot console's surface.
    Console,
    /// A presented stack of layers, in one of the channel's two stack slots.
    Stack(usize),
}

/// Stack slots each channel has: the one it composites, and the one the next
/// present is written to, which a present in flight holds until it latches.
pub(crate) const STACKS: usize = 2;

/// Planes one stack holds: its layers, then the pointer.
pub(crate) const STACK_PLANES: usize = MAX_ACCEL_LAYERS + 1;

/// Words one list slot holds: a plane and the end word.
const SLOT_WORDS: usize = PLANE_WORDS_MAX + 1;

/// Words one stack slot holds: its planes and the end word.
const STACK_WORDS: usize = STACK_PLANES * PLANE_WORDS_MAX + 1;

/// Where the filter kernel lives: past the firmware's own lists.
pub(crate) const KERNEL_AT: usize = reg::BOOTLOADER_DLIST_END;

/// Words each channel's lists take.
const CHANNEL_WORDS: usize = 2 * SLOT_WORDS + STACKS * STACK_WORDS;

/// Where channel `channel`'s lists start, past the kernel.
const fn lists_at(channel: usize) -> usize {
    (KERNEL_AT + KERNEL_WORDS).next_multiple_of(SLOT_WORDS) + channel * CHANNEL_WORDS
}

const _: () = assert!(lists_at(reg::CHANNELS) <= reg::DLIST_WORDS);

/// The word offset of channel `channel`'s `list`.
pub(crate) const fn slot(channel: usize, list: List) -> usize {
    let base = lists_at(channel);
    match list {
        List::Black => base,
        List::Console => base + SLOT_WORDS,
        List::Stack(index) => base + 2 * SLOT_WORDS + index * STACK_WORDS,
    }
}

/// The list a channel's `DISPLACT` or `DISPLIST` names, if it is one of the
/// engine's.
pub(crate) const fn list_at(channel: usize, offset: u32) -> Option<List> {
    let offset = offset as usize;
    let base = lists_at(channel);
    if offset == base {
        return Some(List::Black);
    }
    if offset == base + SLOT_WORDS {
        return Some(List::Console);
    }
    let stacks = base + 2 * SLOT_WORDS;
    if offset < stacks || !(offset - stacks).is_multiple_of(STACK_WORDS) {
        return None;
    }
    let index = (offset - stacks) / STACK_WORDS;
    if index < STACKS {
        Some(List::Stack(index))
    } else {
        None
    }
}

/// The HVS behind its window.
pub(crate) struct Hvs<R> {
    regs: R,
}

impl<R: RegisterBlock> Hvs<R> {
    /// The HVS behind `regs`.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a window short of the
    /// display-list memory.
    pub fn new(regs: R) -> Result<Self, DriverError> {
        if regs.block_len() < reg::WINDOW_LEN {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok(Self { regs })
    }

    fn modify(&self, offset: usize, clear: u32, set: u32) -> Result<(), DriverError> {
        let value = self.regs.read32(offset)?;
        self.regs.write32(offset, value & !clear | set)
    }

    /// The channel that feeds `output` now, as the firmware or an earlier
    /// instance left it.
    pub fn muxed(&self, output: Output) -> Result<Option<usize>, DriverError> {
        let (register, shift) = output.mux();
        let mux = (self.regs.read32(register)? >> shift) & 3;
        Ok((mux != reg::MUX_DISABLED).then_some(mux as usize))
    }

    /// Feed `output` from `channel`, or from none.
    pub fn mux(&self, output: Output, channel: Option<usize>) -> Result<(), DriverError> {
        let (register, shift) = output.mux();
        let mux = match channel {
            Some(channel) => u32::try_from(channel).map_err(|_| DriverError::OutOfRange)?,
            None => reg::MUX_DISABLED,
        };
        self.modify(register, 3 << shift, field(mux, shift, 2))
    }

    /// Stop `channel`, its FIFO emptied; a stopped channel is left alone.
    pub fn stop(&self, channel: usize) -> Result<(), DriverError> {
        if self.regs.read32(reg::dispctrlx(channel))? & reg::DISPCTRLX_ENABLE == 0 {
            return Ok(());
        }
        self.regs
            .write32(reg::dispctrlx(channel), reg::DISPCTRLX_RESET)?;
        self.regs.write32(reg::dispctrlx(channel), 0)
    }

    /// Take the HVS over: every channel stopped and every output unfed, the
    /// interrupts the engine does not use masked, the composite output
    /// buffer divided and the filter kernel in place.
    pub fn take_over(&self) -> Result<(), DriverError> {
        for channel in 0..reg::CHANNELS {
            self.stop(channel)?;
        }
        self.modify(reg::DISPECTRL, reg::DISPECTRL_DSP2_MUX, 0)?;
        self.modify(
            reg::DISPCTRL,
            3 << reg::DISPCTRL_DSP3_MUX_SHIFT,
            reg::MUX_DISABLED << reg::DISPCTRL_DSP3_MUX_SHIFT,
        )?;
        for output in [Output::Four, Output::Five] {
            self.mux(output, None)?;
        }
        let mut sources = reg::DISPCTRL_DMAEIRQ
            | reg::DISPCTRL_SLVEIRQ
            | reg::DISPCTRL_SCLEIRQ
            | reg::DISPCTRL_PANIC_MASK;
        let mut enables = reg::DISPCTRL_ENABLE;
        for channel in 0..reg::CHANNELS {
            sources |= reg::dispctrl_dspeieof(channel)
                | reg::dispctrl_dspeieoln(channel)
                | reg::dispctrl_dspeislur(channel);
            // Raise the AXI panic before a channel's FIFO runs dry.
            enables |= reg::dispctrl_dispeirq(channel) | reg::dispctrl_panic(channel, 2);
        }
        self.modify(reg::DISPCTRL, sources, enables)?;
        for (channel, (base, top)) in reg::COB.into_iter().enumerate() {
            self.regs.write32(
                reg::dispbase(channel),
                field(top, 16, 16) | field(base, 0, 16),
            )?;
        }
        self.write_list(KERNEL_AT, &kernel())
    }

    /// Start `channel` compositing `width` by `height`, black behind its
    /// planes, at the next frame start its pixel valve gives.
    pub fn start(
        &self,
        channel: usize,
        width: u32,
        height: u32,
        interlaced: bool,
    ) -> Result<(), DriverError> {
        let size = (1 << reg::DISPCTRLX_SIZE_BITS) - 1;
        if width == 0 || height == 0 || width > size || height > size {
            return Err(DriverError::OutOfRange);
        }
        let control = reg::dispctrlx(channel);
        self.regs.write32(control, 0)?;
        self.regs.write32(control, reg::DISPCTRLX_RESET)?;
        self.regs.write32(control, 0)?;
        self.regs.write32(
            control,
            reg::DISPCTRLX_ENABLE
                | field(width, reg::DISPCTRLX_WIDTH_SHIFT, reg::DISPCTRLX_SIZE_BITS)
                | height,
        )?;
        let interlace = if interlaced {
            reg::DISPBKGND_INTERLACE
        } else {
            0
        };
        self.modify(
            reg::dispbkgnd(channel),
            reg::DISPBKGND_BCK2BCK
                | reg::DISPBKGND_GAMMA
                | reg::DISPBKGND_INTERLACE
                | reg::DISPBKGND_COLOUR,
            reg::DISPBKGND_FILL | interlace,
        )
    }

    /// The word at `at` of display-list memory.
    pub fn read_list(&self, at: usize) -> Result<u32, DriverError> {
        if at >= reg::DLIST_WORDS {
            return Err(DriverError::OutOfRange);
        }
        self.regs.read32(reg::DLIST + 4 * at)
    }

    /// Write `words` into display-list memory at word `at`.
    pub fn write_list(&self, at: usize, words: &[u32]) -> Result<(), DriverError> {
        if at + words.len() > reg::DLIST_WORDS {
            return Err(DriverError::OutOfRange);
        }
        for (index, &word) in words.iter().enumerate() {
            self.regs.write32(reg::DLIST + 4 * (at + index), word)?;
        }
        Ok(())
    }

    /// Write channel `channel`'s `list`: `plane`, if any, then the end.
    pub fn write_slot(&self, channel: usize, list: List, plane: &[u32]) -> Result<(), DriverError> {
        if plane.len() >= SLOT_WORDS {
            return Err(DriverError::OutOfRange);
        }
        let at = slot(channel, list);
        self.write_list(at, plane)?;
        self.write_list(at + plane.len(), &[END])
    }

    /// Write channel `channel`'s stack slot `index`: `planes` in order, then
    /// the end, answering the word offset each plane starts at.
    pub fn write_stack(
        &self,
        channel: usize,
        index: usize,
        planes: &[PlaneWords],
    ) -> Result<tairix_inline::ArrayVec<usize, STACK_PLANES>, DriverError> {
        if index >= STACKS || planes.len() > STACK_PLANES {
            return Err(DriverError::OutOfRange);
        }
        let mut at = slot(channel, List::Stack(index));
        let mut starts = tairix_inline::ArrayVec::new();
        for words in planes {
            starts.try_push(at).map_err(|_| DriverError::OutOfRange)?;
            self.write_list(at, words)?;
            at += words.len();
        }
        self.write_list(at, &[END])?;
        Ok(starts)
    }

    /// Raise an interrupt at the end of each of `channel`'s frames, or stop.
    pub fn watch_frames(&self, channel: usize, on: bool) -> Result<(), DriverError> {
        let source = reg::dispctrl_dspeieof(channel);
        self.modify(reg::DISPCTRL, source, if on { source } else { 0 })
    }

    /// Clear and answer the channels that finished a frame since last asked.
    pub fn take_frame_ends(&self) -> Result<[bool; reg::CHANNELS], DriverError> {
        let status = self.regs.read32(reg::DISPSTAT)?;
        let mut ended = [false; reg::CHANNELS];
        let mut clear = 0;
        for (channel, end) in ended.iter_mut().enumerate() {
            let bit = reg::dispstat_eof(channel);
            *end = status & bit != 0;
            clear |= status & bit;
        }
        if clear != 0 {
            self.regs.write32(reg::DISPSTAT, clear)?;
        }
        Ok(ended)
    }

    /// Have `channel` latch `list` at its next frame start.
    pub fn latch(&self, channel: usize, list: List) -> Result<(), DriverError> {
        let at = u32::try_from(slot(channel, list)).map_err(|_| DriverError::OutOfRange)?;
        self.regs.write32(reg::displist(channel), at)
    }

    /// The lists `channel` reads: the one it composites, and the one it
    /// latches next.
    pub fn lists(&self, channel: usize) -> Result<[Option<List>; 2], DriverError> {
        Ok([
            list_at(channel, self.regs.read32(reg::displact(channel))?),
            list_at(channel, self.regs.read32(reg::displist(channel))?),
        ])
    }
}
