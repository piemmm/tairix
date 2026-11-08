//! The PDF standard security handler's constructions (ISO 32000-2 §7.6.4):
//! the keys a PDF's password unlocks, and the RC4 and AES-CBC its strings and
//! streams were encrypted with — for reading files written elsewhere, never
//! for protecting data.
//!
//! Revisions 2 to 4 derive their keys through MD5 and encrypt through RC4,
//! both broken; neither is offered as a primitive, only within the handler's
//! own constructions over the inputs the file fixes. Revisions 5 and 6 derive
//! through SHA-2 and AES and decrypt through AES-256.

use core::ops::Range;

use aes::{Aes128, Aes256};
use cbc::cipher::{Block, BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use md5::digest::Output;
use md5::{Digest, Md5};
use rc4::{KeyInit as _, Rc4, StreamCipher as _};
use sha2::{Sha256, Sha384, Sha512};
use zeroize::Zeroize;

use crate::constant_time::ct_eq;

/// The bytes a password is padded with to 32, and the plaintext revisions 2
/// to 4 check a key against.
const PADDING: [u8; 32] = [
    0x28, 0xbf, 0x4e, 0x5e, 0x4e, 0x75, 0x8a, 0x41, 0x64, 0x00, 0x4e, 0x56, 0xff, 0xfa, 0x01, 0x08,
    0x2e, 0x2e, 0x00, 0xb6, 0xd0, 0x68, 0x3e, 0x80, 0x2f, 0x0c, 0xa9, 0xfe, 0x64, 0x53, 0x69, 0x7a,
];

/// The longest key the handler makes: AES-256's.
const MAX_KEY_LEN: usize = 32;

/// The longest password revisions 5 and 6 read: the rest is ignored.
const MODERN_PASSWORD_LEN: usize = 127;

/// An AES block, and the length of an AES-CBC stream's leading IV.
const BLOCK: usize = 16;

/// A key the handler made: a file's, or one object's. Wiped when dropped.
pub struct PdfKey {
    bytes: [u8; MAX_KEY_LEN],
    len: usize,
}

impl PdfKey {
    fn of(bytes: &[u8]) -> Self {
        let mut key = Self {
            bytes: [0; MAX_KEY_LEN],
            len: bytes.len().min(MAX_KEY_LEN),
        };
        key.bytes[..key.len].copy_from_slice(&bytes[..key.len]);
        key
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Encryption entries no password opens a file with: a revision the handler
/// does not define, a key length outside its range, or an `/O` or `/U` too
/// short to hold what it reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdfUnusable;

impl Drop for PdfKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

/// What revisions 2 to 4 of the handler derive a file's key from.
#[derive(Clone, Copy)]
pub struct PdfLegacyEntries<'a> {
    /// `/R`: 2, 3 or 4.
    pub revision: u8,
    /// The key's length in bytes, `/Length` over eight: 5 to 16.
    pub key_len: usize,
    /// `/O`.
    pub owner: &'a [u8],
    /// `/U`.
    pub user: &'a [u8],
    /// `/P`.
    pub permissions: i32,
    /// The first string of the trailer's `/ID`.
    pub id: &'a [u8],
    /// `/EncryptMetadata`.
    pub encrypt_metadata: bool,
}

/// The file key `password` unlocks under revisions 2 to 4 — as the user's
/// password, or as the owner's — or `None` where it unlocks neither.
///
/// # Errors
///
/// [`PdfUnusable`] for entries no password unlocks.
pub fn pdf_legacy_file_key(
    entries: &PdfLegacyEntries<'_>,
    password: &[u8],
) -> Result<Option<PdfKey>, PdfUnusable> {
    let (key_len, checked) = if entries.revision == 2 {
        (5, 32)
    } else {
        (entries.key_len, 16)
    };
    if !(2..=4).contains(&entries.revision)
        || !(5..=16).contains(&key_len)
        || entries.owner.len() < 32
        || entries.user.len() < checked
    {
        return Err(PdfUnusable);
    }
    let entries = PdfLegacyEntries {
        key_len,
        ..*entries
    };
    let as_user = legacy_key(&entries, password);
    if legacy_user_matches(&entries, &as_user) {
        return Ok(Some(as_user));
    }
    let mut user = legacy_user_password(&entries, password);
    let as_owner = legacy_key(&entries, &user);
    user.zeroize();
    Ok(legacy_user_matches(&entries, &as_owner).then_some(as_owner))
}

/// `password` padded or cut to 32 bytes.
fn padded(password: &[u8]) -> [u8; 32] {
    let mut out = PADDING;
    let take = password.len().min(32);
    out[..take].copy_from_slice(&password[..take]);
    out[take..].copy_from_slice(&PADDING[..32 - take]);
    out
}

/// Algorithm 2: the file key `password` makes.
fn legacy_key(entries: &PdfLegacyEntries<'_>, password: &[u8]) -> PdfKey {
    let mut padded = padded(password);
    let mut hash = Md5::new();
    hash.update(padded.as_slice());
    padded.zeroize();
    hash.update(&entries.owner[..32]);
    hash.update(entries.permissions.to_le_bytes());
    hash.update(entries.id);
    if entries.revision >= 4 && !entries.encrypt_metadata {
        hash.update([0xff; 4]);
    }
    let mut digest = [0u8; 16];
    hash.finalize_into((&mut digest).into());
    if entries.revision >= 3 {
        for _ in 0..50 {
            rehash_md5(&mut digest, entries.key_len);
        }
    }
    let key = PdfKey::of(&digest[..entries.key_len]);
    digest.zeroize();
    key
}

/// `digest` replaced by the MD5 of its first `len` bytes: written in place,
/// so no copy of the key it may be is left behind.
fn rehash_md5(digest: &mut [u8; 16], len: usize) {
    let mut hash = Md5::new();
    hash.update(&digest[..len]);
    hash.finalize_into(digest.into());
}

/// `data` run through RC4 once for each of `rounds`, under `key` with every
/// byte exclusive-or'd with the round.
fn rc4_rounds(key: &[u8], data: &mut [u8], rounds: impl Iterator<Item = u8>) {
    let key = &key[..key.len().min(MAX_KEY_LEN)];
    let mut turned = [0u8; MAX_KEY_LEN];
    for round in rounds {
        for (turned, &byte) in turned.iter_mut().zip(key) {
            *turned = byte ^ round;
        }
        if let Ok(mut cipher) = Rc4::new_from_slice(&turned[..key.len()]) {
            cipher.apply_keystream(data);
        }
    }
    turned.zeroize();
}

/// Algorithms 4 to 6: whether `key` is the one the file's `/U` was made with.
fn legacy_user_matches(entries: &PdfLegacyEntries<'_>, key: &PdfKey) -> bool {
    if entries.revision == 2 {
        let mut check = PADDING;
        rc4_rounds(key.as_bytes(), &mut check, core::iter::once(0));
        return entries
            .user
            .get(..32)
            .is_some_and(|user| ct_eq(&check, user));
    }
    let mut hash = Md5::new();
    hash.update(PADDING);
    hash.update(entries.id);
    let mut check = [0u8; 16];
    hash.finalize_into((&mut check).into());
    rc4_rounds(key.as_bytes(), &mut check, 0..=19);
    entries
        .user
        .get(..16)
        .is_some_and(|user| ct_eq(&check, user))
}

/// Algorithm 7: the user's password, padded, the file's `/O` holds under
/// owner password `password`.
fn legacy_user_password(entries: &PdfLegacyEntries<'_>, password: &[u8]) -> [u8; 32] {
    let mut padded = padded(password);
    let mut hash = Md5::new();
    hash.update(padded.as_slice());
    padded.zeroize();
    let mut digest = [0u8; 16];
    hash.finalize_into((&mut digest).into());
    if entries.revision >= 3 {
        for _ in 0..50 {
            rehash_md5(&mut digest, 16);
        }
    }
    let mut user = [0u8; 32];
    user.copy_from_slice(&entries.owner[..32]);
    let key = &digest[..entries.key_len];
    if entries.revision == 2 {
        rc4_rounds(key, &mut user, core::iter::once(0));
    } else {
        rc4_rounds(key, &mut user, (0..=19).rev());
    }
    digest.zeroize();
    user
}

/// Algorithm 1: the key object `num` of generation `gen` is encrypted with
/// under revisions 2 to 4, AES's when `aes`.
#[must_use]
pub fn pdf_object_key(file: &PdfKey, num: u32, gen: u16, aes: bool) -> PdfKey {
    let mut hash = Md5::new();
    hash.update(file.as_bytes());
    hash.update(&num.to_le_bytes()[..3]);
    hash.update(gen.to_le_bytes());
    if aes {
        hash.update(b"sAlT");
    }
    let mut digest = [0u8; 16];
    hash.finalize_into((&mut digest).into());
    let key = PdfKey::of(&digest[..(file.len + 5).min(16)]);
    digest.zeroize();
    key
}

/// What revisions 5 and 6 of the handler derive a file's key from.
#[derive(Clone, Copy)]
pub struct PdfModernEntries<'a> {
    /// `/R`: 5 or 6.
    pub revision: u8,
    /// `/O`: its hash, then its validation and key salts.
    pub owner: &'a [u8; 48],
    /// `/U`: likewise.
    pub user: &'a [u8; 48],
    /// `/OE`: the file key, encrypted under the owner's.
    pub owner_key: &'a [u8; 32],
    /// `/UE`: the file key, encrypted under the user's.
    pub user_key: &'a [u8; 32],
}

/// Algorithm 2.A: the file key `password`, UTF-8 and cut to 127 bytes,
/// unlocks under revisions 5 and 6 — as the owner's password or the user's —
/// or `None` where it unlocks neither.
///
/// # Errors
///
/// [`PdfUnusable`] for a revision other than 5 or 6.
pub fn pdf_modern_file_key(
    entries: &PdfModernEntries<'_>,
    password: &[u8],
) -> Result<Option<PdfKey>, PdfUnusable> {
    if !matches!(entries.revision, 5 | 6) {
        return Err(PdfUnusable);
    }
    let password = &password[..password.len().min(MODERN_PASSWORD_LEN)];
    let (owner, user) = (entries.owner, entries.user);
    let mut check = modern_hash(entries.revision, password, &owner[32..40], user);
    let (salt, udata, encrypted): (&[u8], &[u8], &[u8; 32]) = if ct_eq(&check, &owner[..32]) {
        (&owner[40..48], user, entries.owner_key)
    } else {
        check.zeroize();
        check = modern_hash(entries.revision, password, &user[32..40], &[]);
        if !ct_eq(&check, &user[..32]) {
            check.zeroize();
            return Ok(None);
        }
        (&user[40..48], &[], entries.user_key)
    };
    check.zeroize();
    let mut intermediate = modern_hash(entries.revision, password, salt, udata);
    let mut key = *encrypted;
    let mut decryptor =
        cbc::Decryptor::<Aes256>::new((&intermediate).into(), (&[0u8; BLOCK]).into());
    intermediate.zeroize();
    decryptor.decrypt_blocks(Block::<Aes256>::slice_as_chunks_mut(&mut key).0);
    let made = PdfKey::of(&key);
    key.zeroize();
    Ok(Some(made))
}

/// The hash revision 5 or 6 checks a password with and makes its
/// intermediate keys with: SHA-256 for revision 5, and for revision 6
/// Algorithm 2.B's rounds of AES-128-CBC and SHA-2.
fn modern_hash(revision: u8, password: &[u8], salt: &[u8], udata: &[u8]) -> [u8; 32] {
    let mut first = Sha256::new();
    first.update(password);
    first.update(salt);
    first.update(udata);
    let mut k = [0u8; 64];
    let mut k_len = finished_into(first, &mut k);
    if revision == 5 {
        return out_of(&mut k);
    }
    let mut round = 0usize;
    loop {
        let last = rehash(password, &mut k, &mut k_len, udata);
        round += 1;
        if round >= 64 && usize::from(last) + 32 <= round {
            return out_of(&mut k);
        }
    }
}

/// `hash` finished into the front of `k`, answering how many bytes it
/// wrote: in place, so no copy of the key it may be is left behind.
fn finished_into<D: Digest>(hash: D, k: &mut [u8; 64]) -> usize {
    let size = <D as Digest>::output_size();
    match k.get_mut(..size).and_then(Output::<D>::slice_as_mut_array) {
        Some(out) => {
            hash.finalize_into(out);
            size
        }
        None => 0,
    }
}

/// The first 32 bytes of `k`, which is wiped.
fn out_of(k: &mut [u8; 64]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&k[..32]);
    k.zeroize();
    out
}

/// One round of Algorithm 2.B: `password`, `k`'s first `k_len` bytes and
/// `udata` repeated 64 times, encrypted with AES-128-CBC under `k`'s first
/// half as key and second as IV, then hashed with SHA-256, SHA-384 or SHA-512
/// as the first block's bytes sum to modulo three — the whole streamed a
/// block at a time. `k` and `k_len` become the hash; answers the last byte
/// encrypted.
fn rehash(password: &[u8], k: &mut [u8; 64], k_len: &mut usize, udata: &[u8]) -> u8 {
    let mut key = [0u8; BLOCK];
    let mut iv = [0u8; BLOCK];
    key.copy_from_slice(&k[..BLOCK]);
    iv.copy_from_slice(&k[BLOCK..2 * BLOCK]);
    let mut encryptor = cbc::Encryptor::<Aes128>::new((&key).into(), (&iv).into());
    key.zeroize();
    iv.zeroize();
    let part = password.len() + *k_len + udata.len();
    let total = part * 64;
    let mut sequence = password
        .iter()
        .chain(&k[..*k_len])
        .chain(udata)
        .copied()
        .cycle();
    let mut hasher = Rehasher::None;
    let mut last = 0;
    let mut block = [0u8; BLOCK];
    for _ in 0..total / BLOCK {
        for (slot, byte) in block.iter_mut().zip(&mut sequence) {
            *slot = byte;
        }
        let mut cipher = Block::<Aes128>::from(block);
        encryptor.encrypt_block(&mut cipher);
        if matches!(hasher, Rehasher::None) {
            hasher = Rehasher::chosen(&cipher);
        }
        hasher.update(&cipher);
        last = cipher[BLOCK - 1];
        cipher.zeroize();
    }
    block.zeroize();
    *k_len = hasher.finish(k);
    last
}

/// The SHA-2 a round of Algorithm 2.B hashes with.
enum Rehasher {
    None,
    Sha256(Sha256),
    Sha384(Sha384),
    Sha512(Sha512),
}

impl Rehasher {
    /// The hash a round whose first block encrypted to `first` uses.
    fn chosen(first: &[u8]) -> Self {
        let sum: u32 = first.iter().map(|&byte| u32::from(byte)).sum();
        match sum % 3 {
            0 => Self::Sha256(Sha256::new()),
            1 => Self::Sha384(Sha384::new()),
            _ => Self::Sha512(Sha512::new()),
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Self::None => {}
            Self::Sha256(hash) => hash.update(data),
            Self::Sha384(hash) => hash.update(data),
            Self::Sha512(hash) => hash.update(data),
        }
    }

    /// The hash written into `k`, answering its length.
    fn finish(self, k: &mut [u8; 64]) -> usize {
        match self {
            Self::None => 0,
            Self::Sha256(hash) => finished_into(hash, k),
            Self::Sha384(hash) => finished_into(hash, k),
            Self::Sha512(hash) => finished_into(hash, k),
        }
    }
}

/// `data` decrypted in place with RC4 under `key`.
pub fn pdf_rc4_decrypt(key: &PdfKey, data: &mut [u8]) {
    rc4_rounds(key.as_bytes(), data, core::iter::once(0));
}

/// `data` — a 16-byte IV, then AES-CBC ciphertext under `key` — decrypted
/// in place, answering where the plaintext lies: its padding left out where
/// well formed and kept where not, a final part block dropped, and nothing
/// for data shorter than its IV — as other readers decrypt a damaged
/// stream. `None` for a key of a length AES does not take.
#[must_use]
pub fn pdf_aes_decrypt(key: &PdfKey, data: &mut [u8]) -> Option<Range<usize>> {
    if !matches!(key.len, 16 | 32) {
        return None;
    }
    let Some((iv, body)) = data.split_first_chunk_mut::<BLOCK>() else {
        return Some(0..0);
    };
    let whole = body.len() - body.len() % BLOCK;
    let body = &mut body[..whole];
    if key.len == 16 {
        let mut decryptor = cbc::Decryptor::<Aes128>::new_from_slices(key.as_bytes(), iv).ok()?;
        decryptor.decrypt_blocks(Block::<Aes128>::slice_as_chunks_mut(body).0);
    } else {
        let mut decryptor = cbc::Decryptor::<Aes256>::new_from_slices(key.as_bytes(), iv).ok()?;
        decryptor.decrypt_blocks(Block::<Aes256>::slice_as_chunks_mut(body).0);
    }
    let pad = body.last().map_or(0, |&pad| usize::from(pad));
    let well_formed = (1..=BLOCK).contains(&pad)
        && pad <= whole
        && body[whole - pad..]
            .iter()
            .all(|&byte| usize::from(byte) == pad);
    Some(BLOCK..BLOCK + if well_formed { whole - pad } else { whole })
}

#[cfg(test)]
#[path = "pdf_tests.rs"]
mod tests;
