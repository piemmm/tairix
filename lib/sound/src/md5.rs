//! MD5 (RFC 1321), as FLAC's `STREAMINFO` uses it: the digest of a stream's
//! unencoded samples, against which a decode checks its own arithmetic.
//!
//! An interop digest, not a cryptographic hash. MD5 is broken for that, and
//! nothing here may stand for integrity against an adversary: it lives in
//! this crate, beside the one format that carries it, and no other crate
//! reaches it.

/// Per-round shift amounts.
const SHIFTS: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// The integer parts of `abs(sin(i + 1)) * 2^32`.
const SINES: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

/// A running MD5 digest.
#[derive(Clone, Debug)]
pub(crate) struct Md5 {
    state: [u32; 4],
    pending: [u8; 64],
    pending_len: usize,
    total: u64,
}

impl Md5 {
    pub(crate) const fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            pending: [0; 64],
            pending_len: 0,
            total: 0,
        }
    }

    pub(crate) fn update(&mut self, mut bytes: &[u8]) {
        self.total = self.total.wrapping_add(bytes.len() as u64);
        if self.pending_len > 0 {
            let take = bytes.len().min(64 - self.pending_len);
            self.pending[self.pending_len..self.pending_len + take].copy_from_slice(&bytes[..take]);
            self.pending_len += take;
            bytes = &bytes[take..];
            if self.pending_len < 64 {
                return;
            }
            let block = self.pending;
            self.compress(&block);
            self.pending_len = 0;
        }
        let (blocks, rest) = bytes.as_chunks::<64>();
        for block in blocks {
            self.compress(block);
        }
        self.pending[..rest.len()].copy_from_slice(rest);
        self.pending_len = rest.len();
    }

    pub(crate) fn finish(mut self) -> [u8; 16] {
        let bits = self.total.wrapping_mul(8);
        let mut tail = [0u8; 72];
        tail[0] = 0x80;
        let pad = if self.pending_len < 56 {
            56 - self.pending_len
        } else {
            120 - self.pending_len
        };
        tail[pad..pad + 8].copy_from_slice(&bits.to_le_bytes());
        let total = self.total;
        self.update(&tail[..pad + 8]);
        self.total = total;
        let mut digest = [0u8; 16];
        for (word, out) in self.state.iter().zip(digest.as_chunks_mut::<4>().0) {
            *out = word.to_le_bytes();
        }
        digest
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut words = [0u32; 16];
        for (word, bytes) in words.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*bytes);
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for round in 0..64 {
            let (mix, index) = match round / 16 {
                0 => ((b & c) | (!b & d), round),
                1 => ((d & b) | (!d & c), (5 * round + 1) % 16),
                2 => (b ^ c ^ d, (3 * round + 5) % 16),
                _ => (c ^ (b | !d), (7 * round) % 16),
            };
            let rotated = a
                .wrapping_add(mix)
                .wrapping_add(SINES[round])
                .wrapping_add(words[index])
                .rotate_left(SHIFTS[round]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated);
        }
        for (state, add) in self.state.iter_mut().zip([a, b, c, d]) {
            *state = state.wrapping_add(add);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Md5;

    fn hex(digest: [u8; 16]) -> alloc::string::String {
        use core::fmt::Write as _;
        digest
            .iter()
            .fold(alloc::string::String::new(), |mut text, byte| {
                let _ = write!(text, "{byte:02x}");
                text
            })
    }

    fn digest(bytes: &[u8]) -> alloc::string::String {
        let mut md5 = Md5::new();
        md5.update(bytes);
        hex(md5.finish())
    }

    /// RFC 1321's own test suite.
    #[test]
    fn the_rfcs_test_suite_digests_as_published() {
        let suite: [(&[u8], &str); 7] = [
            (b"", "d41d8cd98f00b204e9800998ecf8427e"),
            (b"a", "0cc175b9c0f1b6a831c399e269772661"),
            (b"abc", "900150983cd24fb0d6963f7d28e17f72"),
            (b"message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                b"abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ];
        for (input, expected) in suite {
            assert_eq!(digest(input), expected);
        }
    }

    #[test]
    fn fed_in_pieces_it_digests_as_fed_whole() {
        let bytes: alloc::vec::Vec<u8> = (0..1_000u32).map(|n| (n * 7 % 251) as u8).collect();
        let whole = digest(&bytes);
        for split in [1, 55, 56, 63, 64, 65, 127, 999] {
            let mut md5 = Md5::new();
            md5.update(&bytes[..split]);
            md5.update(&bytes[split..]);
            assert_eq!(hex(md5.finish()), whole, "split at {split}");
        }
    }
}
