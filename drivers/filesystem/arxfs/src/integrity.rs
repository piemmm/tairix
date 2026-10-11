//! `ARXFS` per-data-block integrity primitives
//! (`docs/src/filesystem/arxfs-spec.md` §6, §8).
//!
//! Every file-data block carries two integrity layers beside its seal, in a
//! fixed trailer after the seal's own:
//!
//! ```text
//! [ sealed content ][ seal trailer ][ descriptor ][ logical hash ][ physical checksum ]
//!                     nonce + tag     5 B           32 B            4 B
//! ```
//!
//! - The **logical content hash** (`crate::crypto::ContentHash`) names the
//!   block's opened content: a raw record's plaintext, or the block's slice of
//!   a compressed cluster frame. Deduplication keys on it, and it catches a
//!   corruption the seal let through. On an encrypted volume it is keyed, so
//!   the stored hash says nothing of the plaintext to whoever holds the disk.
//! - The **physical checksum** ([`physical_checksum`]) is a CRC-32C
//!   (`lib/crc32c`) over the stored bytes, verified first on read, so media
//!   rot is caught before the seal is opened. It authenticates nothing: on an
//!   encrypted volume the AEAD and the metadata MAC do, and a plaintext
//!   volume's seal is a checksum too.

/// Length, in bytes, of a data block's logical content hash, keyed or not.
pub const LOGICAL_HASH_LEN: usize = tairix_crypto::SHA256_OUTPUT_LEN;

/// Length, in bytes, of a data block's fast physical checksum (CRC-32C is a
/// 32-bit checksum).
pub const PHYS_CHECKSUM_LEN: usize = 4;

/// Bytes of per-data-block integrity trailer appended after the crypto
/// trailer: the [`LOGICAL_HASH_LEN`]-byte logical content hash followed by the
/// [`PHYS_CHECKSUM_LEN`]-byte physical checksum.
pub const DATA_INTEGRITY_TRAILER: usize = LOGICAL_HASH_LEN + PHYS_CHECKSUM_LEN;

/// Bytes of the per-data-block **compression descriptor**
/// (`docs/src/filesystem/arxfs-spec.md` §8 — the data-record *compression
/// state* field). It sits between the crypto trailer and the logical hash, so
/// the fast physical checksum covers it (a corrupted descriptor is caught by
/// the first-layer check before the seal is opened). The layout is one state
/// byte followed by a little-endian `u32` whose meaning depends on the state
/// ([`StoredForm`]).
pub const COMPRESSION_DESCRIPTOR_LEN: usize = 1 + 4;

/// Descriptor state byte for a single-block record stored raw: the content
/// slot holds the logical block's plaintext directly
/// (`docs/src/filesystem/arxfs-spec.md` §10).
const STORED_RAW: u8 = 0;

/// Descriptor state byte for the **first** stored block of a compressed
/// cluster: its `u32` field carries the whole compressed frame's byte length.
const STORED_CLUSTER_HEAD: u8 = 1;

/// Descriptor state byte for a **continuation** stored block of a compressed
/// cluster: its `u32` field carries the block's 1-based position within the
/// cluster's stored run.
const STORED_CLUSTER_PART: u8 = 2;

/// How a data block's content slot stores its record
/// (`docs/src/filesystem/arxfs-spec.md` §10 compressed extents).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StoredForm {
    /// Single-block record: the content slot holds the logical block's
    /// plaintext raw.
    Raw,
    /// First stored block of a compressed cluster; `frame_len` is the byte
    /// length of the whole compressed frame spanning the cluster's stored
    /// blocks.
    ClusterHead {
        /// Byte length of the compressed frame across the stored run.
        frame_len: u32,
    },
    /// Continuation stored block of a compressed cluster; `index` is its
    /// 1-based position within the cluster's stored run.
    ClusterPart {
        /// 1-based position of this block within the stored run.
        index: u32,
    },
}

/// Serialise a [`StoredForm`] descriptor into the first
/// [`COMPRESSION_DESCRIPTOR_LEN`] bytes of `dst`. The caller guarantees
/// `dst.len() >= COMPRESSION_DESCRIPTOR_LEN`.
pub fn write_stored_form(dst: &mut [u8], form: StoredForm) {
    let (state, value) = match form {
        StoredForm::Raw => (STORED_RAW, 0),
        StoredForm::ClusterHead { frame_len } => (STORED_CLUSTER_HEAD, frame_len),
        StoredForm::ClusterPart { index } => (STORED_CLUSTER_PART, index),
    };
    dst[0] = state;
    dst[1..5].copy_from_slice(&value.to_le_bytes());
}

/// Parse a [`StoredForm`] descriptor from the first
/// [`COMPRESSION_DESCRIPTOR_LEN`] bytes of `src`. An unknown state byte, a
/// non-zero raw field, a zero-length frame, or a zero part index is rejected
/// as corruption (fail closed). The caller guarantees
/// `src.len() >= COMPRESSION_DESCRIPTOR_LEN`.
pub fn read_stored_form(src: &[u8]) -> Result<StoredForm, DataFault> {
    let value = u32::from_le_bytes([src[1], src[2], src[3], src[4]]);
    match src[0] {
        STORED_RAW if value == 0 => Ok(StoredForm::Raw),
        STORED_CLUSTER_HEAD if value != 0 => Ok(StoredForm::ClusterHead { frame_len: value }),
        STORED_CLUSTER_PART if value != 0 => Ok(StoredForm::ClusterPart { index: value }),
        _ => Err(DataFault::Logical),
    }
}

/// Which integrity layer rejected a data block. Surfaced to the caller as a
/// single [`tairix_abi::DriverError::DeviceFault`] (the `abi-v1` error surface
/// is frozen), but kept distinct internally so a media /
/// transport corruption (the fast [`Physical`](Self::Physical) checksum) is
/// not confused with a failed region seal ([`Seal`](Self::Seal)) or content
/// corruption that survived it ([`Logical`](Self::Logical)).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DataFault {
    /// The fast physical checksum over the at-rest block did not match:
    /// media or transport corruption.
    Physical,
    /// The region seal failed: an encrypted block's AEAD tag, or a plaintext
    /// block's checksum or the address it binds.
    Seal,
    /// The decrypted plaintext did not match its stored logical hash.
    Logical,
}

/// A fast, non-cryptographic checksum over the at-rest `bytes` of a data
/// block: **CRC-32C** (Castagnoli), little-endian. Cheap to verify on every
/// read; detects bit rot, torn writes, and misdirected reads in the stored
/// representation.
///
/// Routed through the shared `lib/crc32c`, so on a core with the `crc32c*`
/// (aarch64) / SSE4.2 `crc32` (x86_64) instruction it runs in one
/// general-purpose-register instruction per word; on every other core it is
/// the portable table baseline. Which one is used is selected once at driver
/// bring-up (`tairix_crc32c::resolve`); until then, and on any core without
/// the instruction, it is the baseline — always the same value (the hardware
/// path is self-verified bit-identical to the reference before it can be
/// selected).
#[must_use]
pub fn physical_checksum(bytes: &[u8]) -> [u8; PHYS_CHECKSUM_LEN] {
    tairix_crc32c::checksum(bytes).to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::{
        physical_checksum, read_stored_form, write_stored_form, DataFault, StoredForm,
        COMPRESSION_DESCRIPTOR_LEN, PHYS_CHECKSUM_LEN,
    };

    #[test]
    fn stored_form_round_trips_and_rejects_undefined_descriptors() {
        let forms = [
            StoredForm::Raw,
            StoredForm::ClusterHead { frame_len: 12_345 },
            StoredForm::ClusterPart { index: 7 },
        ];
        for form in forms {
            let mut buf = [0u8; COMPRESSION_DESCRIPTOR_LEN];
            write_stored_form(&mut buf, form);
            assert_eq!(read_stored_form(&buf), Ok(form));
        }
        // Unknown state byte.
        assert_eq!(read_stored_form(&[3, 0, 0, 0, 0]), Err(DataFault::Logical));
        // A raw descriptor never carries a value.
        assert_eq!(read_stored_form(&[0, 1, 0, 0, 0]), Err(DataFault::Logical));
        // A zero frame length or part index is meaningless.
        assert_eq!(read_stored_form(&[1, 0, 0, 0, 0]), Err(DataFault::Logical));
        assert_eq!(read_stored_form(&[2, 0, 0, 0, 0]), Err(DataFault::Logical));
    }

    #[test]
    fn physical_checksum_detects_single_bit_flips() {
        let a = [0x5au8; 256];
        let mut b = a;
        b[0] ^= 0x01;
        assert_eq!(physical_checksum(&a), physical_checksum(&a));
        assert_ne!(physical_checksum(&a), physical_checksum(&b));
        assert_eq!(physical_checksum(&a).len(), PHYS_CHECKSUM_LEN);
    }

    #[test]
    fn empty_input_hashes_and_checksums_deterministically() {
        // CRC-32C of the empty string is zero (init ^ final-XOR cancel).
        assert_eq!(physical_checksum(&[]), 0u32.to_le_bytes());
        // The canonical CRC-32C check value pins the algorithm end to end.
        assert_eq!(
            physical_checksum(b"123456789"),
            0xE306_9283u32.to_le_bytes()
        );
    }
}
