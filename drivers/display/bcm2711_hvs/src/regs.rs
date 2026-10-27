//! The fifth-generation HVS and the pixel valve registers the engine drives.
//! No public document describes them; the metal run confirms each
//! (`plans/SOUND.md` `SND19q`).

/// A `width`-bit field holding `value` at `shift`.
pub(crate) const fn field(value: u32, shift: u32, width: u32) -> u32 {
    (value & ((1 << width) - 1)) << shift
}

/// The HVS: its global and per-channel registers, and the display-list
/// memory behind them in the same window.
pub(crate) mod hvs {
    /// Bytes of the HVS window: its registers and its display-list memory.
    pub const WINDOW_LEN: usize = 0x8000;
    /// Channels the HVS composites.
    pub const CHANNELS: usize = 3;

    pub const DISPCTRL: usize = 0x00;
    pub const DISPCTRL_ENABLE: u32 = 1 << 31;
    /// Each channel's AXI panic level, two bits a channel from bit 24.
    pub const fn dispctrl_panic(channel: usize, lines: u32) -> u32 {
        (lines & 3) << (24 + 2 * channel)
    }
    pub const DISPCTRL_PANIC_MASK: u32 = 0x3F << 24;
    pub const DISPCTRL_DSP3_MUX_SHIFT: u32 = 18;
    pub const fn dispctrl_dspeislur(channel: usize) -> u32 {
        1 << (9 + 4 * channel)
    }
    pub const fn dispctrl_dspeieoln(channel: usize) -> u32 {
        1 << (8 + 4 * channel)
    }
    pub const fn dispctrl_dspeieof(channel: usize) -> u32 {
        1 << (7 + 4 * channel)
    }
    pub const DISPCTRL_SLVEIRQ: u32 = 1 << 5;
    pub const DISPCTRL_DMAEIRQ: u32 = 1 << 4;
    pub const fn dispctrl_dispeirq(channel: usize) -> u32 {
        1 << (1 + channel)
    }
    pub const DISPCTRL_SCLEIRQ: u32 = 1;

    pub const DISPSTAT: usize = 0x04;
    /// A channel finished compositing a frame; written as one to clear.
    pub const fn dispstat_eof(channel: usize) -> u32 {
        1 << (8 + 8 * channel)
    }

    pub const DISPECTRL: usize = 0x0C;
    pub const DISPECTRL_DSP2_MUX: u32 = 1 << 31;
    pub const DISPDITHER: usize = 0x14;
    pub const DISPDITHER_DSP5_MUX_SHIFT: u32 = 30;
    /// Channel `channel`'s dither: `mode` in its low two bits and the depth
    /// it dithers to in the three above, five bits every eighth from bit 0
    /// (Raspberry Pi forums, topic 324770).
    pub const fn dispdither(channel: usize, depth: u32, mode: u32) -> u32 {
        (((depth & 7) << 2) | (mode & 3)) << (8 * channel)
    }
    pub const fn dispdither_mask(channel: usize) -> u32 {
        dispdither(channel, 7, 3)
    }
    /// The dither depth of the 24-bit pixels the valves carry
    /// (`pv::CONTROL_FORMAT_24`).
    pub const DITHER_DEPTH_24: u32 = 0;
    /// Carry each pixel's rounding error along its line, with a ±½ LSB noise
    /// restarted every frame.
    pub const DITHER_ERROR_AND_NOISE: u32 = 3;
    pub const DISPEOLN: usize = 0x18;
    pub const DISPEOLN_DSP4_MUX_SHIFT: u32 = 30;
    /// The output mux value that feeds an output from no channel.
    pub const MUX_DISABLED: u32 = 3;

    /// The display list a channel latches at its next frame start.
    pub const fn displist(channel: usize) -> usize {
        0x20 + 4 * channel
    }
    /// The display list a channel composites now.
    pub const fn displact(channel: usize) -> usize {
        0x30 + 4 * channel
    }

    pub const fn dispctrlx(channel: usize) -> usize {
        0x40 + 0x10 * channel
    }
    pub const DISPCTRLX_ENABLE: u32 = 1 << 31;
    pub const DISPCTRLX_RESET: u32 = 1 << 30;
    pub const DISPCTRLX_WIDTH_SHIFT: u32 = 16;
    /// Bits of a channel's width and height fields.
    pub const DISPCTRLX_SIZE_BITS: u32 = 13;

    pub const fn dispbkgnd(channel: usize) -> usize {
        0x44 + 0x10 * channel
    }
    pub const DISPBKGND_BCK2BCK: u32 = 1 << 31;
    pub const DISPBKGND_INTERLACE: u32 = 1 << 30;
    pub const DISPBKGND_GAMMA: u32 = 1 << 29;
    /// Fill each line with the background colour, black, before compositing.
    pub const DISPBKGND_FILL: u32 = 1 << 24;
    pub const DISPBKGND_COLOUR: u32 = 0x00FF_FFFF;

    pub const fn dispbase(channel: usize) -> usize {
        0x4C + 0x10 * channel
    }

    /// The composite output buffer each channel's lines pass through: pixels,
    /// channel 0 taking the largest share for the 4K output it feeds.
    pub const COB: [(u32, u32); CHANNELS] = [(24_608, 44_416), (12_304, 24_592), (0, 12_288)];

    /// The display-list memory: byte offset in the window, and its words.
    pub const DLIST: usize = 0x4000;
    pub const DLIST_WORDS: usize = 0x1000;
    /// Words the firmware's own lists occupy at the start of the memory,
    /// left alone so its screen holds until the engine takes over.
    pub const BOOTLOADER_DLIST_END: usize = 32;
}

/// A pixel valve: the timing generator between an HVS output and an encoder.
pub(crate) mod pv {
    /// Bytes of a pixel valve's window.
    pub const WINDOW_LEN: usize = 0x100;

    pub const CONTROL: usize = 0x00;
    pub const CONTROL_FIFO_LEVEL_HIGH_SHIFT: u32 = 25;
    pub const CONTROL_FORMAT_24: u32 = 0;
    pub const CONTROL_FORMAT_SHIFT: u32 = 21;
    pub const CONTROL_FIFO_LEVEL_SHIFT: u32 = 15;
    pub const CONTROL_CLR_AT_START: u32 = 1 << 14;
    pub const CONTROL_TRIGGER_UNDERFLOW: u32 = 1 << 13;
    pub const CONTROL_WAIT_HSTART: u32 = 1 << 12;
    pub const CONTROL_FIFO_CLR: u32 = 1 << 1;
    pub const CONTROL_EN: u32 = 1;

    pub const V_CONTROL: usize = 0x04;
    pub const VCONTROL_ODD_DELAY_SHIFT: u32 = 6;
    pub const VCONTROL_INTERLACE: u32 = 1 << 4;
    pub const VCONTROL_CONTINUOUS: u32 = 1 << 1;
    pub const VCONTROL_VIDEN: u32 = 1;

    pub const VSYNCD_EVEN: usize = 0x08;
    pub const HORZA: usize = 0x0C;
    pub const HORZB: usize = 0x10;
    pub const VERTA: usize = 0x14;
    pub const VERTB: usize = 0x18;
    pub const VERTA_EVEN: usize = 0x1C;
    pub const VERTB_EVEN: usize = 0x20;
    pub const INTEN: usize = 0x24;
    pub const MUX_CFG: usize = 0x34;
    /// RGB pixels pass to the encoder unswapped.
    pub const MUX_CFG_NO_SWAP: u32 = 8 << 2;
}
