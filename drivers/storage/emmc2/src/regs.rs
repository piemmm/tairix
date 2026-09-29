//! BCM2711 EMMC2 (Arasan / SDHCI) register map and bit fields.
//!
//! Byte offsets and bit positions follow the SD Host Controller Simplified
//! Specification (v3.00) standard register block, which the Pi 4 EMMC2
//! controller implements. The BCM2711 takes 32-bit accesses only, so each
//! 16- or 8-bit field is named by the 32-bit register holding it and its
//! position there. Only the registers the driver drives are named.

/// SDHCI standard register block length, in bytes: the Pi 4 device tree's
/// `0x100`-byte window, up to and including the host version at `0xFC`.
pub const REGS_LEN_BYTES: usize = 0x100;

// --- Register byte offsets (SDHCI standard block) -------------------------

/// `ARG2`: the block count an Auto-`CMD23` sends (SDMA system address when
/// SDMA is in use, which this driver never selects).
pub const REG_ARG2: usize = 0x00;
/// `BLKSIZECNT`: block size `[15:0]` and block count `[31:16]`.
pub const REG_BLKSIZECNT: usize = 0x04;
/// `ARG1`: the 32-bit command argument.
pub const REG_ARG1: usize = 0x08;
/// `CMDTM`: transfer mode `[15:0]` and command `[31:16]`.
pub const REG_CMDTM: usize = 0x0C;
/// `RESP0`: command response word 0.
pub const REG_RESP0: usize = 0x10;
/// `RESP1`: command response word 1.
pub const REG_RESP1: usize = 0x14;
/// `RESP2`: command response word 2.
pub const REG_RESP2: usize = 0x18;
/// `RESP3`: command response word 3.
pub const REG_RESP3: usize = 0x1C;
/// `DATA`: the PIO buffer data port.
pub const REG_DATA: usize = 0x20;
/// `STATUS`: present-state register (line-busy flags, line levels).
pub const REG_STATUS: usize = 0x24;
/// `CONTROL0`: host control 1 `[7:0]`, power control `[15:8]`.
pub const REG_CONTROL0: usize = 0x28;
/// `CONTROL1`: clock control `[15:0]`, timeout `[19:16]`, reset `[26:24]`.
pub const REG_CONTROL1: usize = 0x2C;
/// `INTERRUPT`: normal interrupt status `[15:0]`, error status `[31:16]`
/// (write-1-to-clear).
pub const REG_INTERRUPT: usize = 0x30;
/// `IRPT_MASK`: interrupt-status enable bits.
pub const REG_IRPT_MASK: usize = 0x34;
/// `IRPT_EN`: interrupt-signal (to-CPU) enable bits.
pub const REG_IRPT_EN: usize = 0x38;
/// `CONTROL2`: auto-command error status `[15:0]` (read-only) and host
/// control 2 `[31:16]`.
pub const REG_CONTROL2: usize = 0x3C;
/// `CAPABILITIES`: the controller's capabilities, low word.
pub const REG_CAPABILITIES: usize = 0x40;
/// `CAPABILITIES_1`: the capabilities' high word (SDHCI 3.00 and later).
pub const REG_CAPABILITIES_1: usize = 0x44;
/// `MAX_CURRENT`: the maximum current the host supplies per voltage.
pub const REG_MAX_CURRENT: usize = 0x48;
/// `ADMA_ADDR` (low 32 bits): the device address of the 32-bit ADMA2
/// descriptor table the controller walks for a DMA transfer. The upper word
/// (`0x5C`) is left zero: 32-bit ADMA2 addresses fit the low word.
pub const REG_ADMA_ADDR: usize = 0x58;
/// `SLOTISR_VER`: slot interrupt status `[15:0]`, host controller version
/// `[31:16]`.
pub const REG_SLOTISR_VER: usize = 0xFC;

// --- `STATUS` (present state) bits ----------------------------------------

/// Command line is busy; a new command must not be issued.
pub const STATUS_CMD_INHIBIT: u32 = 1 << 0;
/// Data line is busy; a new data command must not be issued.
pub const STATUS_DAT_INHIBIT: u32 = 1 << 1;
/// `DAT[0]` line signal level: low while the card holds the line busy.
pub const STATUS_DAT0_LEVEL: u32 = 1 << 20;

// --- `CONTROL0` host-control 1 bits (byte `[7:0]`) ------------------------

/// Data Transfer Width = 4-bit (`CONTROL0[1]`); clear means the 1-bit bus.
pub const CONTROL0_DATA_WIDTH_4BIT: u32 = 1 << 1;
/// High Speed Enable (`CONTROL0[2]`): the host drives the bus with the
/// output timing of every mode faster than Default Speed.
pub const CONTROL0_HIGH_SPEED: u32 = 1 << 2;
/// DMA Select field (`CONTROL0[4:3]`) value `0b10`: 32-bit ADMA2.
pub const CONTROL0_DMA_SELECT_ADMA2: u32 = 0b10 << 3;
/// Mask of the whole 2-bit DMA Select field (`CONTROL0[4:3]`).
pub const CONTROL0_DMA_SELECT_MASK: u32 = 0b11 << 3;

// --- `CONTROL0` power-control bits (byte `[15:8]`) ------------------------

/// SD Bus Power: the standard register block gates command and data
/// activity on it; a full host-controller reset clears it.
pub const CONTROL0_BUS_POWER: u32 = 1 << 8;
/// SD Bus Voltage Select = 3.3 V, the card's supply (`[11:9]`). Signalling
/// is chosen separately, in [`CONTROL2_1V8_SIGNALLING`].
pub const CONTROL0_BUS_VOLTAGE_3V3: u32 = 0b111 << 9;

// --- `CONTROL1` bits ------------------------------------------------------

/// Internal clock enable.
pub const CONTROL1_CLK_INTLEN: u32 = 1 << 0;
/// Internal clock stable.
pub const CONTROL1_CLK_STABLE: u32 = 1 << 1;
/// SD clock enable.
pub const CONTROL1_CLK_EN: u32 = 1 << 2;
/// Reset the complete host controller.
pub const CONTROL1_SRST_HC: u32 = 1 << 24;
/// Reset the command line.
pub const CONTROL1_SRST_CMD: u32 = 1 << 25;
/// Reset the data line, which also halts the DMA engine.
pub const CONTROL1_SRST_DATA: u32 = 1 << 26;

/// Bit offset of the data-timeout field (`[19:16]`).
pub const CONTROL1_TIMEOUT_SHIFT: u32 = 16;

// --- `CONTROL2` host-control 2 bits (half `[31:16]`) ----------------------

/// UHS Mode Select field (`CONTROL2[18:16]`).
pub const CONTROL2_UHS_MODE_MASK: u32 = 0b111 << 16;
/// Bit offset of the UHS Mode Select field.
pub const CONTROL2_UHS_MODE_SHIFT: u32 = 16;
/// 1.8 V Signaling Enable (`CONTROL2[19]`).
pub const CONTROL2_1V8_SIGNALLING: u32 = 1 << 19;

// --- `INTERRUPT` bits (normal status, low half) ---------------------------

/// Command complete.
pub const INT_CMD_DONE: u32 = 1 << 0;
/// Data transfer complete, and the end of an R1b command's busy.
pub const INT_DATA_DONE: u32 = 1 << 1;
/// Buffer write ready: the data port can accept a block.
pub const INT_WRITE_RDY: u32 = 1 << 4;
/// Buffer read ready: a block is available at the data port.
pub const INT_READ_RDY: u32 = 1 << 5;
/// An error interrupt is asserted; the error half `[31:16]` is set.
pub const INT_ERROR: u32 = 1 << 15;

/// Mask covering every error bit (the upper half of `INTERRUPT`).
pub const INT_ERROR_MASK: u32 = 0xFFFF_0000;

/// The `IRPT_EN` signal-enable mask: the controller asserts its CPU
/// interrupt line for exactly the sources the engine parks on — command
/// complete, transfer complete, the PIO buffer-ready events — and every
/// error bit, so a faulted transfer also wakes the parked task.
pub const INT_SIGNAL_ENABLE: u32 =
    INT_CMD_DONE | INT_DATA_DONE | INT_WRITE_RDY | INT_READ_RDY | INT_ERROR_MASK;

/// Every bit set: clears the whole `INTERRUPT` register (write-1-to-clear)
/// and unmasks every status bit.
pub const INT_ALL: u32 = 0xFFFF_FFFF;

// --- `CMDTM` command-register fields (upper half) -------------------------

/// Bit offset of the 6-bit command index (`[29:24]`).
pub const CMD_INDEX_SHIFT: u32 = 24;
/// Bit offset of the 2-bit response-type-select field (`[17:16]`).
pub const CMD_RESP_TYPE_SHIFT: u32 = 16;
/// Command uses CRC checking on its response (`[19]`).
pub const CMD_CRCCHK_EN: u32 = 1 << 19;
/// Command uses index checking on its response (`[20]`).
pub const CMD_IXCHK_EN: u32 = 1 << 20;
/// Command transfers data on the DAT lines (`[21]`).
pub const CMD_IS_DATA: u32 = 1 << 21;
/// Command type = Abort (`[23:22]` = `0b11`): the command stops the data
/// transfer in progress.
pub const CMD_TYPE_ABORT: u32 = 0b11 << 22;

/// Response-type select: no response.
pub const RESP_NONE: u32 = 0b00;
/// Response-type select: 136-bit response (R2).
pub const RESP_136: u32 = 0b01;
/// Response-type select: 48-bit response (R1/R3/R6/R7).
pub const RESP_48: u32 = 0b10;
/// Response-type select: 48-bit response with busy (R1b).
pub const RESP_48_BUSY: u32 = 0b11;

// --- `CMDTM` transfer-mode fields (lower half) ----------------------------

/// DMA-enable (`[0]`): the data phase is mastered by the controller's DMA
/// engine instead of the programmed-I/O buffer data port.
pub const TM_DMA_EN: u32 = 1 << 0;
/// Block-count-enable (multi-block transfers).
pub const TM_BLKCNT_EN: u32 = 1 << 1;
/// Auto-CMD12 (`[3:2]` = `0b01`): the controller ends an open multi-block
/// transfer with `STOP_TRANSMISSION`.
pub const TM_AUTO_CMD12: u32 = 0b01 << 2;
/// Auto-CMD23 (`[3:2]` = `0b10`): the controller precedes the multi-block
/// command with `SET_BLOCK_COUNT` carrying [`REG_ARG2`].
pub const TM_AUTO_CMD23: u32 = 0b10 << 2;
/// Data direction: card-to-host (read).
pub const TM_DAT_DIR_READ: u32 = 1 << 4;
/// Multi-block transfer.
pub const TM_MULTI_BLOCK: u32 = 1 << 5;
