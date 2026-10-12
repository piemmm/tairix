//! Synthetic sink descriptions for tests: base blocks, CTA-861 and
//! `DisplayID` extensions, and ELDs, each laid out as the standards place
//! every field and closed with its checksum.

use alloc::vec;
use alloc::vec::Vec;

/// Bytes of an EDID block.
pub const BLOCK: usize = 128;

/// Close `block` with the checksum that sums it to zero.
pub fn checksum(block: &mut [u8; BLOCK]) {
    let sum = block[..BLOCK - 1]
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    block[BLOCK - 1] = 0u8.wrapping_sub(sum);
}

/// A detailed timing descriptor: the clock in kHz, each axis as active,
/// front porch, sync and back porch, the flags byte, and the picture size.
#[must_use]
pub fn dtd(clock_khz: u32, h: [u16; 4], v: [u16; 4], flags: u8, size_mm: (u16, u16)) -> [u8; 18] {
    let [h_active, h_front, h_sync, h_back] = h;
    let [v_active, v_front, v_sync, v_back] = v;
    let h_blank = h_front + h_sync + h_back;
    let v_blank = v_front + v_sync + v_back;
    let clock = u16::try_from(clock_khz / 10).expect("a clock a descriptor holds");
    let low = |value: u16| value.to_le_bytes()[0];
    let nibble = |value: u16, shift: u16| u8::try_from((value >> shift) & 0xF).expect("a nibble");
    let mut d = [0u8; 18];
    d[0..2].copy_from_slice(&clock.to_le_bytes());
    d[2] = low(h_active);
    d[3] = low(h_blank);
    d[4] = nibble(h_active, 8) << 4 | nibble(h_blank, 8);
    d[5] = low(v_active);
    d[6] = low(v_blank);
    d[7] = nibble(v_active, 8) << 4 | nibble(v_blank, 8);
    d[8] = low(h_front);
    d[9] = low(h_sync);
    d[10] = nibble(v_front, 0) << 4 | nibble(v_sync, 0);
    let two = |value: u16, shift: u16| u8::try_from((value >> shift) & 0x3).expect("two bits");
    d[11] = two(h_front, 8) << 6 | two(h_sync, 8) << 4 | two(v_front, 4) << 2 | two(v_sync, 4);
    d[12] = low(size_mm.0);
    d[13] = low(size_mm.1);
    d[14] = nibble(size_mm.0, 8) << 4 | nibble(size_mm.1, 8);
    d[17] = flags;
    d
}

/// Separate digital sync, both pulses positive.
pub const SYNC_POSITIVE: u8 = 0x18 | 0x04 | 0x02;

/// 1920x1080 at 60 Hz, as CTA-861 code 16 states it.
#[must_use]
pub fn dtd_1080p() -> [u8; 18] {
    dtd(
        148_500,
        [1920, 88, 44, 148],
        [1080, 4, 5, 36],
        SYNC_POSITIVE,
        (600, 340),
    )
}

/// 1280x720 at 60 Hz, as CTA-861 code 4 states it.
#[must_use]
pub fn dtd_720p() -> [u8; 18] {
    dtd(
        74_250,
        [1280, 110, 40, 220],
        [720, 5, 5, 20],
        SYNC_POSITIVE,
        (0, 0),
    )
}

/// A base block under construction.
pub struct Base([u8; BLOCK]);

impl Base {
    /// An EDID 1.4 base block from manufacturer `TXS`, digital, 60 by 34 cm,
    /// with no timing yet.
    #[must_use]
    pub fn new() -> Self {
        let mut block = [0u8; BLOCK];
        block[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        let id: u16 =
            (u16::from(b'T' - b'@') << 10) | (u16::from(b'X' - b'@') << 5) | u16::from(b'S' - b'@');
        block[8..10].copy_from_slice(&id.to_be_bytes());
        block[10..12].copy_from_slice(&0x1234u16.to_le_bytes());
        block[12..16].copy_from_slice(&42u32.to_le_bytes());
        block[16] = 10;
        block[17] = 34;
        block[18] = 1;
        block[19] = 4;
        block[20] = 0x80;
        block[21] = 60;
        block[22] = 34;
        block[0x26..0x36].fill(0x01);
        Self(block)
    }

    /// Set the EDID 1 revision.
    #[must_use]
    pub fn revision(mut self, revision: u8) -> Self {
        self.0[19] = revision;
        self
    }

    /// Set the feature byte.
    #[must_use]
    pub fn features(mut self, features: u8) -> Self {
        self.0[24] = features;
        self
    }

    /// Put `descriptor` in slot `slot`, 0 to 3.
    #[must_use]
    pub fn descriptor(mut self, slot: usize, descriptor: [u8; 18]) -> Self {
        let at = 54 + slot * 18;
        self.0[at..at + 18].copy_from_slice(&descriptor);
        self
    }

    /// Put a display descriptor tagged `tag` holding `payload` in slot
    /// `slot`.
    #[must_use]
    pub fn display(self, slot: usize, tag: u8, payload: &[u8]) -> Self {
        let mut descriptor = [0u8; 18];
        descriptor[3] = tag;
        descriptor[5..5 + payload.len()].copy_from_slice(payload);
        self.descriptor(slot, descriptor)
    }

    /// Set the three established-timing bytes.
    #[must_use]
    pub fn established(mut self, bytes: [u8; 3]) -> Self {
        self.0[0x23..0x26].copy_from_slice(&bytes);
        self
    }

    /// Set the first standard timing codes.
    #[must_use]
    pub fn standard(mut self, codes: &[[u8; 2]]) -> Self {
        for (slot, code) in codes.iter().enumerate() {
            self.0[0x26 + slot * 2..0x28 + slot * 2].copy_from_slice(code);
        }
        self
    }

    /// Declare `count` extensions.
    #[must_use]
    pub fn extensions(mut self, count: u8) -> Self {
        self.0[126] = count;
        self
    }

    /// The block, checksummed.
    #[must_use]
    pub fn build(mut self) -> [u8; BLOCK] {
        checksum(&mut self.0);
        self.0
    }
}

impl Default for Base {
    fn default() -> Self {
        Self::new()
    }
}

/// A CTA-861 extension under construction.
pub struct Cta {
    revision: u8,
    flags: u8,
    collection: Vec<u8>,
    detailed: Vec<[u8; 18]>,
}

impl Cta {
    /// A revision 3 extension with nothing in it.
    #[must_use]
    pub fn new() -> Self {
        Self {
            revision: 3,
            flags: 0,
            collection: Vec::new(),
            detailed: Vec::new(),
        }
    }

    /// Set the revision.
    #[must_use]
    pub fn revision(mut self, revision: u8) -> Self {
        self.revision = revision;
        self
    }

    /// Set the flags byte: underscan, basic audio, YCbCr, native count.
    #[must_use]
    pub fn flags(mut self, flags: u8) -> Self {
        self.flags = flags;
        self
    }

    /// Add a data block tagged `tag` holding `payload`.
    #[must_use]
    pub fn block(mut self, tag: u8, payload: &[u8]) -> Self {
        self.collection
            .push(tag << 5 | u8::try_from(payload.len()).expect("a short block"));
        self.collection.extend_from_slice(payload);
        self
    }

    /// Add the raw bytes `bytes` to the collection, header and all.
    #[must_use]
    pub fn raw(mut self, bytes: &[u8]) -> Self {
        self.collection.extend_from_slice(bytes);
        self
    }

    /// Add a detailed timing after the collection.
    #[must_use]
    pub fn detailed(mut self, descriptor: [u8; 18]) -> Self {
        self.detailed.push(descriptor);
        self
    }

    /// The block, checksummed.
    #[must_use]
    pub fn build(self) -> [u8; BLOCK] {
        let mut block = [0u8; BLOCK];
        block[0] = 0x02;
        block[1] = self.revision;
        let start = 4 + self.collection.len();
        block[2] = u8::try_from(start).expect("the collection fits");
        block[3] = self.flags;
        block[4..start].copy_from_slice(&self.collection);
        for (index, descriptor) in self.detailed.iter().enumerate() {
            let at = start + index * 18;
            block[at..at + 18].copy_from_slice(descriptor);
        }
        checksum(&mut block);
        block
    }
}

impl Default for Cta {
    fn default() -> Self {
        Self::new()
    }
}

/// A `DisplayID` 1.3 section in an extension block, its data blocks each a
/// tag and payload, both checksums closed.
#[must_use]
pub fn displayid(blocks: &[(u8, &[u8])]) -> [u8; BLOCK] {
    let mut block = [0u8; BLOCK];
    block[0] = 0x70;
    block[1] = 0x13;
    let mut at = 5;
    for (tag, payload) in blocks {
        block[at] = *tag;
        block[at + 1] = 0;
        block[at + 2] = u8::try_from(payload.len()).expect("a short block");
        block[at + 3..at + 3 + payload.len()].copy_from_slice(payload);
        at += 3 + payload.len();
    }
    block[2] = u8::try_from(at - 5).expect("the section fits");
    let sum = block[1..at]
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    block[at] = 0u8.wrapping_sub(sum);
    checksum(&mut block);
    block
}

/// A type I `DisplayID` timing: the clock in units of 10 kHz, each axis as
/// active, front porch, sync and back porch, positive sync, and the flags
/// byte.
#[must_use]
pub fn displayid_timing(clock: u32, h: [u16; 4], v: [u16; 4], flags: u8) -> [u8; 20] {
    let stored = |value: u32| (value - 1).to_le_bytes();
    let mut t = [0u8; 20];
    t[0..3].copy_from_slice(&stored(clock)[..3]);
    t[3] = flags;
    let field = |t: &mut [u8; 20], at: usize, value: u16, top: u8| {
        let bytes = (value - 1).to_le_bytes();
        t[at] = bytes[0];
        t[at + 1] = bytes[1] | top;
    };
    field(&mut t, 4, h[0], 0);
    field(&mut t, 6, h[1] + h[2] + h[3], 0);
    field(&mut t, 8, h[1], 0x80);
    field(&mut t, 10, h[2], 0);
    field(&mut t, 12, v[0], 0);
    field(&mut t, 14, v[1] + v[2] + v[3], 0);
    field(&mut t, 16, v[1], 0x80);
    field(&mut t, 18, v[2], 0);
    t
}

/// A version 2 ELD: the connection type, the monitor name, the short audio
/// descriptors and the speaker byte, its baseline padded to whole words.
#[must_use]
pub fn eld(connection: u8, name: &[u8], sads: &[[u8; 3]], speakers: u8) -> Vec<u8> {
    let mut baseline = vec![0u8; 16];
    baseline[0] = 0x40 | u8::try_from(name.len()).expect("a short name");
    baseline[1] = u8::try_from(sads.len()).expect("few descriptors") << 4 | connection << 2;
    baseline[3] = speakers;
    baseline.extend_from_slice(name);
    for sad in sads {
        baseline.extend_from_slice(sad);
    }
    while !baseline.len().is_multiple_of(4) {
        baseline.push(0);
    }
    let mut eld = vec![0x10, 0, u8::try_from(baseline.len() / 4).expect("short"), 0];
    eld.extend_from_slice(&baseline);
    eld
}
