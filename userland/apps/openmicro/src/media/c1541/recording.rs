//! A sector as the 1541's DOS records it — a SYNC, the header, a gap, a SYNC
//! and the data block, then a gap before the next — and the codes its
//! controller reports a job by, which a D64's error bytes carry.

/// A SYNC's byte.
pub const SYNC: u8 = 0xFF;
/// SYNC's bytes written before each block.
pub const SYNC_BYTES: u8 = 5;
/// A gap's byte.
pub const GAP: u8 = 0x55;
/// The gap after a header.
pub const HEADER_GAP: u8 = 9;
/// A header's first byte, before its checksum, sector, track and ID.
pub const HEADER_MARK: u8 = 0x08;
/// The two bytes that pad a header to eight.
pub const HEADER_PADDING: u8 = 0x0F;
/// A data block's first byte, before a sector's 256, their checksum and two
/// zeros.
pub const DATA_MARK: u8 = 0x07;
/// A header's eight bytes as GCR.
pub const HEADER_GCR: u8 = 10;
/// A data block's 260 bytes as GCR.
pub const DATA_GCR: u16 = 325;
/// A sector's recording but the gap after it.
pub const SECTOR_RECORDING: usize =
    2 * SYNC_BYTES as usize + HEADER_GCR as usize + HEADER_GAP as usize + DATA_GCR as usize;
/// A formatted data block's first byte.
pub const FILL_FIRST: u8 = 0x4B;
/// Track 1's, which a format opens with that instead.
pub const FILL_FIRST_TRACK_1: u8 = 0x00;
/// A formatted data block's other bytes.
pub const FILL: u8 = 0x01;

/// How a job ended; a D64's error byte for a sector is the code reading it
/// ended with. Each failure is reported as the error numbered beside it.
pub mod job {
    /// Done.
    pub const OK: u8 = 0x01;
    /// 20: no header for the sector.
    pub const NO_HEADER: u8 = 0x02;
    /// 21: no SYNC.
    pub const NO_SYNC: u8 = 0x03;
    /// 22: no data block after the header.
    pub const NO_DATA_BLOCK: u8 = 0x04;
    /// 23: the data block's checksum wrong.
    pub const DATA_CHECKSUM: u8 = 0x05;
    /// 24: a byte that is no GCR code's.
    pub const BYTE_DECODING: u8 = 0x06;
    /// 25: a block written that does not compare.
    pub const VERIFY: u8 = 0x07;
    /// 26: the disk write-protected.
    pub const WRITE_PROTECT: u8 = 0x08;
    /// 27: the header's checksum wrong.
    pub const HEADER_CHECKSUM: u8 = 0x09;
    /// 29: the header carrying another disk's ID.
    pub const ID_MISMATCH: u8 = 0x0B;
}
