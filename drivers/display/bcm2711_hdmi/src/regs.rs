//! The BCM2711 HDMI blocks' registers, window by window. No public document
//! describes them; the metal run confirms each (`plans/SOUND.md` `SND19q`).

/// A `width`-bit field holding `value` at `shift`.
pub(crate) const fn field(value: u32, shift: u32, width: u32) -> u32 {
    (value & ((1 << width) - 1)) << shift
}

/// The transmitter core (`hdmi`).
pub(crate) mod core {
    pub const FIFO_CTL: usize = 0x074;
    pub const FIFO_MASTER_SLAVE_N: u32 = 1;
    pub const FIFO_RECENTER: u32 = 1 << 6;
    pub const FIFO_RECENTER_DONE: u32 = 1 << 14;
    /// The FIFO control bits software may write back.
    pub const FIFO_VALID_WRITE: u32 = 0xEFFF;
    pub const MAI_CHANNEL_MAP: usize = 0x09C;
    pub const MAI_CONFIG: usize = 0x0A0;
    pub const MAI_CONFIG_BIT_REVERSE: u32 = 1 << 26;
    pub const MAI_CONFIG_FORMAT_REVERSE: u32 = 1 << 27;
    pub const AUDIO_PACKET_CONFIG: usize = 0x0B8;
    pub const AUDIO_PACKET_ZERO_INACTIVE: u32 = 1 << 24;
    pub const AUDIO_PACKET_ZERO_FLAT: u32 = 1 << 29;
    pub const RAM_PACKET_CONFIG: usize = 0x0BC;
    pub const RAM_PACKET_ENABLE: u32 = 1 << 16;
    pub const RAM_PACKET_STATUS: usize = 0x0C4;
    pub const CRP_CFG: usize = 0x0C8;
    pub const CRP_EXTERNAL_CTS: u32 = 1 << 24;
    pub const CTS_0: usize = 0x0CC;
    pub const CTS_1: usize = 0x0D0;
    pub const SCHEDULER_CONTROL: usize = 0x0E0;
    pub const SCHEDULER_MODE_HDMI: u32 = 1;
    pub const SCHEDULER_HDMI_ACTIVE: u32 = 1 << 1;
    pub const SCHEDULER_IGNORE_VSYNC_PREDICTS: u32 = 1 << 5;
    pub const SCHEDULER_MANUAL_FORMAT: u32 = 1 << 15;
    pub const HORZA: usize = 0x0E4;
    pub const HORZA_HPOS: u32 = 1 << 14;
    pub const HORZA_VPOS: u32 = 1 << 15;
    pub const HORZB: usize = 0x0E8;
    pub const VERTA0: usize = 0x0EC;
    pub const VERTB0: usize = 0x0F0;
    pub const VERTA1: usize = 0x0F4;
    pub const VERTB1: usize = 0x0F8;
    pub const MISC_CONTROL: usize = 0x100;
    pub const DEEP_COLOR_CONFIG_1: usize = 0x170;
    pub const GCP_CONFIG: usize = 0x178;
    pub const GCP_ENABLE: u32 = 1 << 31;
    pub const GCP_WORD_1: usize = 0x17C;
    pub const GCP_CLEAR_AVMUTE: u32 = 1 << 4;
    pub const HOTPLUG: usize = 0x1A8;
    pub const HOTPLUG_CONNECTED: u32 = 1;
    pub const SCRAMBLER_CTL: usize = 0x1C4;
    pub const SCRAMBLER_ENABLE: u32 = 1;
    /// Bytes of the window the driver reaches.
    pub const LEN: usize = SCRAMBLER_CTL + 4;
}

/// The display video port (`dvp`): the block's pixel clock gate and its
/// pixel interface.
pub(crate) mod dvp {
    pub const CLOCK_STOP: usize = 0x0BC;
    pub const CLOCK_STOP_PIXEL: u32 = 1 << 1;
    pub const VEC_INTERFACE_CFG: usize = 0x0EC;
    pub const VEC_INTERFACE_XBAR: usize = 0x0F0;
    /// RGB passed straight through the crossbar.
    pub const XBAR_RGB: u32 = 0x0035_4021;
    pub const LEN: usize = VEC_INTERFACE_XBAR + 4;
}

/// The TMDS PHY (`phy`).
pub(crate) mod phy {
    pub const RESET_CTL: usize = 0x000;
    /// The data and clock lanes' resets.
    pub const RESET_LANES: u32 = 0x0F;
    /// The PLL's and its divider's resets, active low.
    pub const RESET_PLL: u32 = 3 << 4;
    pub const POWERDOWN_CTL: usize = 0x004;
    pub const POWERDOWN_ALL: u32 = 1 << 10;
    pub const POWERDOWN_RNDGEN: u32 = 1 << 4;
    pub const CTL_0: usize = 0x008;
    pub const CTL_1: usize = 0x00C;
    pub const CTL_2: usize = 0x010;
    pub const CTL_3: usize = 0x014;
    pub const PLL_CTL_0: usize = 0x01C;
    pub const PLL_ENA_VCO_CLK: u32 = 1 << 5;
    pub const PLL_VCO_CONT_EN: u32 = 1 << 6;
    pub const PLL_MASH11_MODE: u32 = 1 << 13;
    pub const PLL_CTL_1: usize = 0x020;
    pub const PLL_FREQ_DOUBLER_ENABLE: u32 = 1 << 13;
    pub const CLK_DIV: usize = 0x028;
    pub const PLL_CFG: usize = 0x034;
    pub const TMDS_CLK_WORD_SEL: usize = 0x044;
    pub const CHANNEL_SWAP: usize = 0x04C;
    pub const PLL_CALIBRATION_1: usize = 0x050;
    pub const PLL_CALIBRATION_2: usize = 0x054;
    pub const CALIBRATION_LIMIT: u32 = (1 << 28) - 1;
    pub const PLL_CALIBRATION_4: usize = 0x05C;
    pub const LEN: usize = PLL_CALIBRATION_4 + 4;
}

/// The rate manager (`rm`), which locks the PHY's PLL.
pub(crate) mod rm {
    pub const CONTROL: usize = 0x000;
    pub const CONTROL_FREE_RUN: u32 = 1 << 4;
    pub const CONTROL_LOAD_INTEGRATOR: u32 = 1 << 17;
    pub const CONTROL_FREEZE_COUNTERS: u32 = 1 << 19;
    pub const OFFSET: usize = 0x018;
    pub const OFFSET_ONLY: u32 = 1 << 31;
    pub const FORMAT: usize = 0x01C;
    pub const LEN: usize = FORMAT + 4;
}

/// The packet RAM (`packet`): one slot of words per packet type, the slot of
/// `InfoFrame` type `0x80 + n` at `n` strides.
pub(crate) mod packet {
    /// Bytes from one packet's slot to the next.
    pub const STRIDE: usize = 0x24;
    /// The slots of the `InfoFrame` types CTA-861 defines, `0x80` to `0x87`.
    pub const SLOTS: usize = 8;
    pub const LEN: usize = STRIDE * SLOTS;
}

/// The colour space converter (`csc`).
pub(crate) mod csc {
    pub const CTL: usize = 0x000;
    pub const CTL_ENABLE: u32 = 1 << 2;
    pub const CTL_MODE_CUSTOM: u32 = 3;
    /// The coefficient pairs, row by row.
    pub const COEFFICIENTS: [usize; 6] = [0x004, 0x008, 0x00C, 0x010, 0x014, 0x018];
    pub const CHANNEL_CTL: usize = 0x02C;
    pub const LEN: usize = CHANNEL_CTL + 4;
}

/// The block both HDMI transmitters' video and audio controls share (`hd`),
/// each at its own offsets.
pub(crate) mod hd {
    /// A transmitter's MAI registers, from its block's base.
    pub const MAI_CTL: usize = 0x00;
    pub const MAI_THR: usize = 0x04;
    pub const MAI_FMT: usize = 0x08;
    pub const MAI_DATA: usize = 0x0C;
    pub const MAI_SMP: usize = 0x10;
    pub const MAI_LEN: usize = MAI_SMP + 4;
    pub const MAI_RESET: u32 = 1;
    pub const MAI_ERRORF: u32 = 1 << 1;
    pub const MAI_ERRORE: u32 = 1 << 2;
    pub const MAI_ENABLE: u32 = 1 << 3;
    pub const MAI_FLUSH: u32 = 1 << 9;
    pub const MAI_WHOLSMP: u32 = 1 << 12;
    pub const MAI_CHALIGN: u32 = 1 << 13;
    pub const MAI_DLATE: u32 = 1 << 15;
    pub const MAI_FORMAT_PCM: u32 = 2;
    pub const VID_CTL_ENABLE: u32 = 1 << 31;
    pub const VID_CTL_UNDERFLOW_ENABLE: u32 = 1 << 30;
    pub const VID_CTL_FRAME_COUNTER_RESET: u32 = 1 << 29;
    pub const VID_CTL_VSYNC_LOW: u32 = 1 << 28;
    pub const VID_CTL_HSYNC_LOW: u32 = 1 << 27;
    pub const VID_CTL_CLRRGB: u32 = 1 << 23;
    pub const VID_CTL_BLANKPIX: u32 = 1 << 18;
    pub const VID_CTL_BLANK_INSERT_EN: u32 = 1 << 16;
}
