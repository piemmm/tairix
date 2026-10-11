//! `ARXFS` at-rest protection (`docs/src/filesystem/arxfs-spec.md` §5, §7).
//!
//! A volume is **encrypted** or **plaintext**, as its superblock declares
//! ([`VolumeProtection`]). The mount's [`Protection`] is the one path by which
//! any block is sealed or opened, so no read or write path chooses a primitive
//! itself.
//!
//! An encrypted volume grows its key hierarchy from a caller-supplied
//! [`VolumeKey`]:
//!
//! ```text
//! volume key (caller-supplied)
//!   -> wrapping key  (KDF) ----- unwraps -----> master key (on disk, AEAD-wrapped)
//!                                                  -> metadata-authentication key (HMAC)
//!                                                  -> filename key (AEAD)
//!                                                  -> content  key (AEAD)
//! ```
//!
//! The master key is never stored unwrapped: only its AEAD-sealed form lives in
//! the plaintext discovery region of every superblock slot. Opening derives the
//! wrapping key from the supplied key and unseals the master key; a wrong key
//! fails the AEAD authentication and the mount is refused.
//!
//! A plaintext volume has no key. Every metadata block carries a CRC-32C in its
//! authenticator field, and every name and content region a CRC-32C over its
//! physical address and bytes. That catches a torn, rotted or misdirected write
//! as surely as the keyed seal, but not a deliberate rewrite, which only
//! encryption can refuse.
//!
//! All primitives come through `lib/crypto` and `lib/crc32c`. The master key,
//! its wrapping salt and the wrap nonce are drawn at format time from an
//! injected [`EntropySource`], the seam onto the platform RNG, so the master key
//! is independent of the volume key and a failed draw fails the format.

use tairix_abi::driver::DriverError;
use tairix_crypto::{
    derive_key, hmac_sha256, hmac_sha256_parts, open, seal, sha256, AeadError, AeadKey, AeadNonce,
    AeadTag, HmacSha256Key, HmacSha256Tag, AEAD_NONCE_LEN, HMAC_SHA256_TAG_LEN,
};

use crate::integrity::LOGICAL_HASH_LEN;
use zeroize::Zeroize;

/// Length, in bytes, of a volume key — the caller-supplied key that unwraps a
/// `ARXFS` volume. A 256-bit key matches the AEAD and MAC key widths.
pub const VOLUME_KEY_LEN: usize = 32;

/// A caller-supplied volume key: the installer's, recovery flow's, or storage
/// policy service's key material that unlocks a `ARXFS` volume. `ARXFS`
/// never persists it; only the master key it unwraps is stored, and that only
/// in AEAD-sealed form.
pub type VolumeKey = [u8; VOLUME_KEY_LEN];

/// Source of cryptographic randomness for a fresh volume's key material (the
/// master key, the wrapping salt, and the wrap nonce) and its UUID.
///
/// This is `ARXFS`'s seam onto the platform RNG: the composition root
/// injects the cryptographically secure generator (`lib/rng`'s `CsRng`, the
/// generator the charter mandates for `ARXFS` keys), so the driver
/// itself never names or reaches for a global RNG and stays
/// architecture-neutral. It mirrors the injection seam `kernel/mem`'s
/// encrypted swap, `init`'s `Spawner`, and `login`'s `Authenticator` use.
pub trait EntropySource {
    /// Fill the whole of `out` with cryptographically secure random bytes.
    ///
    /// # Errors
    ///
    /// Returns a [`DriverError`] (the implementation's fail-closed code) if
    /// randomness is unavailable. `ARXFS` fails closed: no
    /// key, salt, nonce, or UUID is derived from a failed draw, so a volume is
    /// never provisioned with predictable key material. On error the contents
    /// of `out` are unspecified and must not be used.
    fn fill(&mut self, out: &mut [u8]) -> Result<(), DriverError>;
}

/// How a volume's blocks are protected at rest, as its superblock declares it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VolumeProtection {
    /// ChaCha20-Poly1305 names and content and HMAC-SHA256 metadata, under the
    /// key hierarchy a [`VolumeKey`] unwraps.
    Encrypted,
    /// No key: a CRC-32C on every block, which detects corruption but not
    /// tampering.
    Plaintext,
}

impl VolumeProtection {
    /// The superblock's spelling of the protection.
    pub(crate) const fn word(self) -> u32 {
        match self {
            Self::Encrypted => 1,
            Self::Plaintext => 2,
        }
    }

    /// The protection a superblock's `word` names: `None` for any other word,
    /// zero included, so an unwritten or foreign field never reads as either.
    pub(crate) const fn from_word(word: u32) -> Option<Self> {
        match word {
            1 => Some(Self::Encrypted),
            2 => Some(Self::Plaintext),
            _ => None,
        }
    }
}

/// The key a volume is formatted or opened with: none for a plaintext volume,
/// its [`VolumeKey`] for an encrypted one.
///
/// An open refuses the other kind of volume rather than mounting it: an
/// encrypted volume opened with no key is
/// [`PermissionDenied`](DriverError::PermissionDenied), as a wrong key is, and a
/// plaintext volume opened with a key is
/// [`Unsupported`](DriverError::Unsupported) — a caller expecting protection is
/// never handed a volume that has none.
#[derive(Copy, Clone)]
pub enum Keying<'a> {
    /// No key: the volume is plaintext.
    Plaintext,
    /// The volume is encrypted under this key.
    Key(&'a VolumeKey),
}

/// What authenticates a volume's metadata blocks. A key it holds is zeroed
/// when it is dropped.
#[derive(Clone)]
pub(crate) enum MetaSeal {
    /// HMAC-SHA256 under the volume's metadata-authentication key.
    Keyed(HmacSha256Key),
    /// A plaintext volume's CRC-32C.
    Checksum,
}

impl MetaSeal {
    /// The protection a volume sealed this way has.
    pub(crate) const fn protection(&self) -> VolumeProtection {
        match self {
            Self::Keyed(_) => VolumeProtection::Encrypted,
            Self::Checksum => VolumeProtection::Plaintext,
        }
    }

    /// The authenticator for `parts` taken as one message: the HMAC, or the
    /// CRC-32C in its first four bytes with the rest zero, so a plaintext tag
    /// is checked whole just as a keyed one is.
    pub(crate) fn tag(&self, parts: &[&[u8]]) -> HmacSha256Tag {
        match self {
            Self::Keyed(key) => hmac_sha256_parts(key, parts),
            Self::Checksum => {
                let state = parts.iter().fold(0xFFFF_FFFF, |state, part| {
                    tairix_crc32c::update(state, part)
                });
                let mut tag = [0u8; HMAC_SHA256_TAG_LEN];
                tag[..4].copy_from_slice(&(!state).to_le_bytes());
                tag
            }
        }
    }
}

/// Bytes of per-block crypto trailer appended to every data and directory
/// block, of either protection: a [`tairix_crypto::AEAD_NONCE_LEN`]-byte nonce
/// followed by a [`tairix_crypto::AEAD_TAG_LEN`]-byte authentication tag, or on
/// a plaintext volume a zero nonce, the region's CRC-32C and zeroes.
pub const CRYPTO_TRAILER: usize = tairix_crypto::AEAD_NONCE_LEN + tairix_crypto::AEAD_TAG_LEN;

/// What seals a volume's name or content regions. A key it holds is zeroed
/// when it is dropped.
#[derive(Clone)]
pub(crate) enum RegionSeal {
    /// ChaCha20-Poly1305 under one of the volume's region keys.
    Aead(AeadKey),
    /// A plaintext volume's CRC-32C over the region and where it is stored.
    Checksum,
}

impl RegionSeal {
    /// Seal `region` for the block at `phys` written by generation `gen`:
    /// encrypt it in place, or checksum it, recording either in `trailer`.
    pub(crate) fn seal(
        &self,
        region: &mut [u8],
        trailer: &mut [u8],
        phys: u64,
        gen: u64,
    ) -> Result<(), DriverError> {
        let trailer = trailer
            .get_mut(..CRYPTO_TRAILER)
            .ok_or(DriverError::DeviceFault)?;
        match self {
            Self::Aead(key) => encrypt_region(key, region, trailer, phys, gen)
                .map_err(|_| DriverError::DeviceFault),
            Self::Checksum => {
                trailer.copy_from_slice(&checksum_trailer(region, phys));
                Ok(())
            }
        }
    }

    /// Open `region` sealed for the block at `phys`: authenticate and decrypt
    /// it in place, or check its checksum and that the rest of the trailer is
    /// zero.
    pub(crate) fn open(
        &self,
        region: &mut [u8],
        trailer: &[u8],
        phys: u64,
    ) -> Result<(), AeadError> {
        let trailer = trailer
            .get(..CRYPTO_TRAILER)
            .ok_or(AeadError::Authentication)?;
        match self {
            Self::Aead(key) => decrypt_region(key, region, trailer, phys),
            Self::Checksum if trailer == checksum_trailer(region, phys) => Ok(()),
            Self::Checksum => Err(AeadError::Authentication),
        }
    }
}

/// What names a data block's content in its integrity trailer and the
/// deduplication index. A key it holds is zeroed when it is dropped.
#[derive(Clone)]
pub(crate) enum ContentHash {
    /// HMAC-SHA256 under the volume's content-hash key: without the key a
    /// stored hash confirms no guessed plaintext, so a low-entropy file
    /// cannot be searched for offline.
    Keyed(HmacSha256Key),
    /// A plaintext volume's SHA-256, which hides nothing the block does not.
    Plain,
}

impl ContentHash {
    /// The hash of `content`.
    pub(crate) fn of(&self, content: &[u8]) -> [u8; LOGICAL_HASH_LEN] {
        match self {
            Self::Keyed(key) => hmac_sha256(key, content),
            Self::Plain => sha256(content),
        }
    }
}

impl Drop for ContentHash {
    fn drop(&mut self) {
        if let Self::Keyed(key) = self {
            key.zeroize();
        }
    }
}

impl Drop for MetaSeal {
    fn drop(&mut self) {
        if let Self::Keyed(key) = self {
            key.zeroize();
        }
    }
}

impl Drop for RegionSeal {
    fn drop(&mut self) {
        if let Self::Aead(key) = self {
            key.zeroize();
        }
    }
}

/// A plaintext region's trailer: a zero nonce, then the CRC-32C of `phys` and
/// `region`, then zeroes. Binding `phys` makes a region read back at another
/// address fail even with every byte intact.
fn checksum_trailer(region: &[u8], phys: u64) -> [u8; CRYPTO_TRAILER] {
    let state = tairix_crc32c::update(0xFFFF_FFFF, &phys.to_le_bytes());
    let checksum = !tairix_crc32c::update(state, region);
    let mut trailer = [0u8; CRYPTO_TRAILER];
    trailer[AEAD_NONCE_LEN..AEAD_NONCE_LEN + 4].copy_from_slice(&checksum.to_le_bytes());
    trailer
}

/// What a volume is opened under: its UUID, its protection, and the discovery
/// header every commit re-publishes, zero on a plaintext volume. A volume is
/// built only from one, so no block is ever sealed before it is established.
pub(crate) struct Established {
    pub(crate) fs_uuid: u128,
    pub(crate) protection: Protection,
    pub(crate) crypto_header: [u8; CRYPTO_HEADER_LEN],
}

impl Established {
    /// A plaintext volume's.
    pub(crate) fn plaintext(fs_uuid: u128) -> Self {
        Self {
            fs_uuid,
            protection: Protection::plaintext(fs_uuid),
            crypto_header: [0u8; CRYPTO_HEADER_LEN],
        }
    }
}

/// How a mounted volume seals and opens every block, set once at format or
/// mount from its [`VolumeProtection`].
pub(crate) struct Protection {
    pub(crate) meta: MetaSeal,
    pub(crate) names: RegionSeal,
    pub(crate) content: RegionSeal,
    pub(crate) content_hash: ContentHash,
    /// Stable per-volume deduplication-domain identifier, carried in every
    /// chunk record and index key; dedupe never crosses it. An identifier, not
    /// a secret.
    pub(crate) dedupe_domain: u64,
}

impl Protection {
    /// An encrypted volume's protection, from its unwrapped working keys.
    pub(crate) fn encrypted(keys: &VolumeKeys) -> Self {
        Self {
            meta: MetaSeal::Keyed(keys.mac_key),
            names: RegionSeal::Aead(keys.filename_key),
            content: RegionSeal::Aead(keys.content_key),
            content_hash: ContentHash::Keyed(keys.content_hash_key),
            dedupe_domain: keys.dedupe_domain,
        }
    }

    /// A plaintext volume's protection; its dedupe domain is drawn from the
    /// volume's UUID, as an encrypted volume's is from its master key.
    pub(crate) fn plaintext(fs_uuid: u128) -> Self {
        let mut material = [0u8; CTX_DEDUPE.len() + 16];
        material[..CTX_DEDUPE.len()].copy_from_slice(CTX_DEDUPE);
        material[CTX_DEDUPE.len()..].copy_from_slice(&fs_uuid.to_le_bytes());
        let digest = sha256(&material);
        let mut domain = [0u8; 8];
        domain.copy_from_slice(&digest[..8]);
        Self {
            meta: MetaSeal::Checksum,
            names: RegionSeal::Checksum,
            content: RegionSeal::Checksum,
            content_hash: ContentHash::Plain,
            dedupe_domain: u64::from_le_bytes(domain),
        }
    }

    /// The protection these seals give: the metadata seal's, which every
    /// other seal of the volume shares.
    pub(crate) const fn kind(&self) -> VolumeProtection {
        self.meta.protection()
    }
}

/// On-disk size of the crypto discovery header stored in a superblock slot's
/// plaintext payload region: a 16-byte salt, the 32-byte AEAD-wrapped master
/// key, the wrap nonce, and the wrap tag.
pub const CRYPTO_HEADER_LEN: usize =
    SALT_LEN + VOLUME_KEY_LEN + tairix_crypto::AEAD_NONCE_LEN + tairix_crypto::AEAD_TAG_LEN;

/// Length, in bytes, of the per-volume wrapping salt.
const SALT_LEN: usize = 16;

// Domain-separating KDF context labels. Each derived key gets its own stable
// label so no two uses of a parent key ever collide (`tairix_crypto::kdf`).
const CTX_WRAP: &[u8] = b"arxfs/wrap-key";
const CTX_META: &[u8] = b"arxfs/meta-mac";
const CTX_FILENAME: &[u8] = b"arxfs/filename";
const CTX_CONTENT: &[u8] = b"arxfs/content";
const CTX_CONTENT_HASH: &[u8] = b"arxfs/content-hash";
const CTX_DEDUPE: &[u8] = b"arxfs/dedupe-domain";

/// The fully-derived set of working keys for a mounted encrypted volume.
///
/// Every field is a key, so the shared `key` suffix is intentional and names
/// each key's role rather than repeating the type (`clippy::struct_field_names`
/// is silenced for exactly that reason).
#[derive(Clone)]
#[allow(clippy::struct_field_names)]
pub(crate) struct VolumeKeys {
    /// Keyed authenticator for every metadata block (HMAC-SHA256).
    pub(crate) mac_key: HmacSha256Key,
    /// AEAD key encrypting directory-entry names at rest.
    pub(crate) filename_key: AeadKey,
    /// AEAD key encrypting file data at rest.
    pub(crate) content_key: AeadKey,
    /// Keyed hash naming file data (HMAC-SHA256).
    pub(crate) content_hash_key: HmacSha256Key,
    /// The volume's deduplication domain, derived from the master key.
    pub(crate) dedupe_domain: u64,
}

impl Drop for VolumeKeys {
    fn drop(&mut self) {
        self.mac_key.zeroize();
        self.filename_key.zeroize();
        self.content_key.zeroize();
        self.content_hash_key.zeroize();
    }
}

/// The plaintext crypto discovery header an encrypted volume stores in every
/// superblock slot; a plaintext volume's slot holds zeroes in its place.
#[derive(Copy, Clone)]
pub(crate) struct CryptoHeader {
    salt: [u8; SALT_LEN],
    wrapped_master: [u8; VOLUME_KEY_LEN],
    wrap_nonce: AeadNonce,
    wrap_tag: AeadTag,
}

/// Derive a 256-bit subkey from `secret`, a domain-separating `label`, and the
/// per-volume `salt`, all through the `lib/crypto` KDF.
fn derive_with_salt(secret: &[u8; 32], label: &[u8], salt: &[u8; SALT_LEN]) -> [u8; 32] {
    let mut ctx = [0u8; 64];
    let l = label.len();
    ctx[..l].copy_from_slice(label);
    ctx[l..l + SALT_LEN].copy_from_slice(salt);
    derive_key(secret, &ctx[..l + SALT_LEN])
}

/// The wrapping key that seals the master key on disk, derived from the
/// caller's volume key and the per-volume salt. It is a deterministic KDF of
/// the volume key so [`unwrap`] can recompute it on mount; the random salt
/// (stored in the clear) makes it unique per volume.
fn wrapping_key(volume_key: &VolumeKey, salt: &[u8; SALT_LEN]) -> [u8; 32] {
    derive_with_salt(volume_key, CTX_WRAP, salt)
}

/// Grow the working key set from the master key.
fn derive_volume_keys(master: &[u8; 32]) -> VolumeKeys {
    let mut domain_material = derive_key(master, CTX_DEDUPE);
    let mut domain_bytes = [0u8; 8];
    domain_bytes.copy_from_slice(&domain_material[..8]);
    domain_material.zeroize();
    VolumeKeys {
        mac_key: derive_key(master, CTX_META),
        filename_key: derive_key(master, CTX_FILENAME),
        content_key: derive_key(master, CTX_CONTENT),
        content_hash_key: derive_key(master, CTX_CONTENT_HASH),
        dedupe_domain: u64::from_le_bytes(domain_bytes),
    }
}

impl CryptoHeader {
    /// Encode the header into the first [`CRYPTO_HEADER_LEN`] bytes of `out`.
    ///
    /// # Errors
    ///
    /// [`AeadError::Authentication`] is never produced here; the function is
    /// infallible for a correctly-sized buffer and panics are avoided by the
    /// caller always passing a full payload region.
    pub(crate) fn encode(&self, out: &mut [u8]) {
        let mut off = 0;
        out[off..off + SALT_LEN].copy_from_slice(&self.salt);
        off += SALT_LEN;
        out[off..off + VOLUME_KEY_LEN].copy_from_slice(&self.wrapped_master);
        off += VOLUME_KEY_LEN;
        out[off..off + AEAD_NONCE_LEN].copy_from_slice(&self.wrap_nonce);
        off += AEAD_NONCE_LEN;
        out[off..off + self.wrap_tag.len()].copy_from_slice(&self.wrap_tag);
    }

    /// Decode a header from the first [`CRYPTO_HEADER_LEN`] bytes of `bytes`.
    /// Returns `None` if `bytes` is too short. The bytes are plaintext
    /// discovery material; their authenticity is established by a successful
    /// [`unwrap`].
    #[must_use]
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < CRYPTO_HEADER_LEN {
            return None;
        }
        let mut off = 0;
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&bytes[off..off + SALT_LEN]);
        off += SALT_LEN;
        let mut wrapped_master = [0u8; VOLUME_KEY_LEN];
        wrapped_master.copy_from_slice(&bytes[off..off + VOLUME_KEY_LEN]);
        off += VOLUME_KEY_LEN;
        let mut wrap_nonce = [0u8; AEAD_NONCE_LEN];
        wrap_nonce.copy_from_slice(&bytes[off..off + AEAD_NONCE_LEN]);
        off += AEAD_NONCE_LEN;
        let mut wrap_tag = [0u8; tairix_crypto::AEAD_TAG_LEN];
        wrap_tag.copy_from_slice(&bytes[off..off + tairix_crypto::AEAD_TAG_LEN]);
        Some(Self {
            salt,
            wrapped_master,
            wrap_nonce,
            wrap_tag,
        })
    }
}

/// Provision a fresh key hierarchy for a new volume at format time.
///
/// Draws the per-volume wrapping salt, the master key, and the wrap nonce from
/// `entropy` (the platform RNG seam), derives the wrapping key from the
/// caller-supplied `volume_key` and the random salt, AEAD-seals the master key
/// under it, and returns the on-disk [`CryptoHeader`] plus the working
/// [`VolumeKeys`]. The master key is independent of `volume_key`, so the
/// volume can be re-wrapped on a key change without rewriting its data. No
/// plaintext key is ever returned for storage; the header carries only the
/// wrapped master key.
///
/// # Errors
///
/// * [`DriverError`] (the entropy source's fail-closed code) if any random
///   draw is unavailable: provisioning aborts before sealing, so a volume is
///   never created with predictable key material.
/// * [`DriverError::DeviceFault`] if sealing the master key fails (unreachable
///   for the fixed-size buffer, but surfaced rather than panicked).
pub(crate) fn provision(
    volume_key: &VolumeKey,
    entropy: &mut dyn EntropySource,
) -> Result<(CryptoHeader, VolumeKeys), DriverError> {
    let mut salt = [0u8; SALT_LEN];
    entropy.fill(&mut salt)?;
    let mut master = [0u8; 32];
    entropy.fill(&mut master)?;
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    entropy.fill(&mut nonce)?;

    let mut wrapping = wrapping_key(volume_key, &salt);
    let mut wrapped_master = master;
    let sealed = seal(&wrapping, &nonce, &salt, &mut wrapped_master);
    wrapping.zeroize();
    let keys = derive_volume_keys(&master);
    master.zeroize();
    let wrap_tag = sealed.map_err(|_| DriverError::DeviceFault)?;
    Ok((
        CryptoHeader {
            salt,
            wrapped_master,
            wrap_nonce: nonce,
            wrap_tag,
        },
        keys,
    ))
}

/// Recover the working key set by unwrapping the master key with the
/// caller-supplied `volume_key` at mount time.
///
/// # Errors
///
/// [`AeadError::Authentication`] if `volume_key` is wrong (or the wrapped
/// blob is corrupt): the AEAD tag fails to verify, so the caller refuses the
/// mount, fail-closed.
pub(crate) fn unwrap(
    volume_key: &VolumeKey,
    header: &CryptoHeader,
) -> Result<VolumeKeys, AeadError> {
    let mut wrapping = wrapping_key(volume_key, &header.salt);
    let mut master = header.wrapped_master;
    let opened = open(
        &wrapping,
        &header.wrap_nonce,
        &header.salt,
        &mut master,
        &header.wrap_tag,
    );
    wrapping.zeroize();
    let keys = opened.map(|()| derive_volume_keys(&master));
    master.zeroize();
    keys
}

/// The deterministic AEAD nonce for the block at physical address `phys`
/// written by transaction generation `gen`. Unique per persisted write
/// because copy-on-write gives a fresh `(phys, gen)` to every distinct stored
/// ciphertext, and the nonce is also stored in the block so reads never
/// recompute it.
fn block_nonce(key: &AeadKey, phys: u64, gen: u64) -> AeadNonce {
    let mut ctx = [0u8; 16];
    ctx[..8].copy_from_slice(&phys.to_le_bytes());
    ctx[8..].copy_from_slice(&gen.to_le_bytes());
    let full = derive_key(key, &ctx);
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    nonce.copy_from_slice(&full[..AEAD_NONCE_LEN]);
    nonce
}

/// Encrypt `region` in place under `key`, writing the nonce and tag into
/// `trailer` (which must be exactly [`CRYPTO_TRAILER`] bytes). `phys` and
/// `gen` make the nonce unique; `phys` is also bound as associated data so a
/// block decrypted at the wrong physical address fails.
///
/// # Errors
///
/// [`AeadError`] if the AEAD seal fails (unreachable for the fixed key/nonce
/// widths, surfaced rather than panicked).
fn encrypt_region(
    key: &AeadKey,
    region: &mut [u8],
    trailer: &mut [u8],
    phys: u64,
    gen: u64,
) -> Result<(), AeadError> {
    let nonce = block_nonce(key, phys, gen);
    let tag = seal(key, &nonce, &phys.to_le_bytes(), region)?;
    trailer[..AEAD_NONCE_LEN].copy_from_slice(&nonce);
    trailer[AEAD_NONCE_LEN..CRYPTO_TRAILER].copy_from_slice(&tag);
    Ok(())
}

/// Decrypt `region` in place under `key`, reading the nonce and tag from
/// `trailer`. Authenticates before it yields plaintext: a bit-flip in the
/// ciphertext, trailer, or `phys` binding fails the tag and the caller fails
/// closed, never returning mis-decrypted bytes.
///
/// # Errors
///
/// [`AeadError::Authentication`] if authentication fails.
fn decrypt_region(
    key: &AeadKey,
    region: &mut [u8],
    trailer: &[u8],
    phys: u64,
) -> Result<(), AeadError> {
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    nonce.copy_from_slice(&trailer[..AEAD_NONCE_LEN]);
    let mut tag = [0u8; tairix_crypto::AEAD_TAG_LEN];
    tag.copy_from_slice(&trailer[AEAD_NONCE_LEN..CRYPTO_TRAILER]);
    open(key, &nonce, &phys.to_le_bytes(), region, &tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VK: VolumeKey = [0x11; VOLUME_KEY_LEN];

    /// A deterministic stand-in for the platform RNG: a byte counter starting
    /// at `seed`, so each fill is distinct and reproducible and different seeds
    /// produce different streams. It is test scaffolding, never a production
    /// entropy source.
    struct CountingEntropy {
        next: u8,
    }

    impl CountingEntropy {
        fn new(seed: u8) -> Self {
            Self { next: seed }
        }
    }

    impl EntropySource for CountingEntropy {
        fn fill(&mut self, out: &mut [u8]) -> Result<(), DriverError> {
            for byte in out.iter_mut() {
                *byte = self.next;
                self.next = self.next.wrapping_add(1);
            }
            Ok(())
        }
    }

    /// An entropy source that always fails, to drive the fail-closed path.
    struct DeadEntropy;

    impl EntropySource for DeadEntropy {
        fn fill(&mut self, _out: &mut [u8]) -> Result<(), DriverError> {
            Err(DriverError::DeviceFault)
        }
    }

    #[test]
    fn provision_then_unwrap_round_trips_keys() {
        let (header, keys) = provision(&VK, &mut CountingEntropy::new(1)).expect("provision");
        let recovered = unwrap(&VK, &header).expect("unwrap");
        assert_eq!(keys.mac_key, recovered.mac_key);
        assert_eq!(keys.filename_key, recovered.filename_key);
        assert_eq!(keys.content_key, recovered.content_key);
    }

    #[test]
    fn wrong_volume_key_fails_to_unwrap() {
        let (header, _) = provision(&VK, &mut CountingEntropy::new(2)).expect("provision");
        let mut wrong = VK;
        wrong[0] ^= 0x01;
        assert!(unwrap(&wrong, &header).is_err());
    }

    #[test]
    fn derived_keys_are_independent() {
        let (_, keys) = provision(&VK, &mut CountingEntropy::new(3)).expect("provision");
        assert_ne!(keys.mac_key, keys.filename_key);
        assert_ne!(keys.mac_key, keys.content_key);
        assert_ne!(keys.filename_key, keys.content_key);
    }

    #[test]
    fn distinct_entropy_yields_distinct_master_keys() {
        // Two volumes formatted with the same volume key but independent RNG
        // streams get independent key hierarchies: the master key is drawn
        // from the RNG, not derived from the volume key.
        let (_, a) = provision(&VK, &mut CountingEntropy::new(10)).expect("provision a");
        let (_, b) = provision(&VK, &mut CountingEntropy::new(20)).expect("provision b");
        assert_ne!(a.content_key, b.content_key);
        assert_ne!(a.mac_key, b.mac_key);
        assert_ne!(a.dedupe_domain, b.dedupe_domain);
    }

    #[test]
    fn provision_fails_closed_when_entropy_is_unavailable() {
        assert_eq!(
            provision(&VK, &mut DeadEntropy).err(),
            Some(DriverError::DeviceFault)
        );
    }

    #[test]
    fn header_encode_decode_round_trips() {
        let (header, _) = provision(&VK, &mut CountingEntropy::new(4)).expect("provision");
        let mut bytes = [0u8; CRYPTO_HEADER_LEN];
        header.encode(&mut bytes);
        let decoded = CryptoHeader::decode(&bytes).expect("decode");
        let keys = unwrap(&VK, &decoded).expect("unwrap decoded");
        let direct = unwrap(&VK, &header).expect("unwrap direct");
        assert_eq!(keys.content_key, direct.content_key);
    }

    #[test]
    fn region_round_trips_and_detects_tampering() {
        let (_, keys) = provision(&VK, &mut CountingEntropy::new(5)).expect("provision");
        let plain = *b"a directory entry name or file data block content!!";
        let mut region = plain;
        let mut trailer = [0u8; CRYPTO_TRAILER];
        encrypt_region(&keys.content_key, &mut region, &mut trailer, 42, 7).expect("encrypt");
        assert_ne!(region, plain, "ciphertext must differ from plaintext");
        let mut ok = region;
        decrypt_region(&keys.content_key, &mut ok, &trailer, 42).expect("decrypt");
        assert_eq!(ok, plain);
        // A flipped ciphertext byte is rejected.
        let mut bad = region;
        bad[0] ^= 0x01;
        assert!(decrypt_region(&keys.content_key, &mut bad, &trailer, 42).is_err());
        // The wrong physical-address binding is rejected.
        let mut moved = region;
        assert!(decrypt_region(&keys.content_key, &mut moved, &trailer, 43).is_err());
    }

    #[test]
    fn the_protection_word_names_exactly_two_protections() {
        for protection in [VolumeProtection::Encrypted, VolumeProtection::Plaintext] {
            assert_eq!(
                VolumeProtection::from_word(protection.word()),
                Some(protection)
            );
        }
        assert_ne!(
            VolumeProtection::Encrypted.word(),
            VolumeProtection::Plaintext.word()
        );
        for refused in [0, 3, u32::MAX] {
            assert_eq!(VolumeProtection::from_word(refused), None, "word {refused}");
        }
    }

    #[test]
    fn a_plaintext_metadata_tag_is_a_checksum_padded_with_zeroes() {
        let block = [0x42u8; 512];
        let tag = MetaSeal::Checksum.tag(&[&block]);
        assert_eq!(tag[..4], tairix_crc32c::checksum(&block).to_le_bytes());
        assert!(tag[4..].iter().all(|&byte| byte == 0));
        assert_eq!(MetaSeal::Checksum.tag(&[&block[..100], &block[100..]]), tag);
        let mut flipped = block;
        flipped[300] ^= 0x01;
        assert_ne!(MetaSeal::Checksum.tag(&[&flipped]), tag);
        let (_, keys) = provision(&VK, &mut CountingEntropy::new(6)).expect("provision");
        let keyed = MetaSeal::Keyed(keys.mac_key);
        assert_ne!(keyed.tag(&[&block]), tag);
        assert_eq!(keyed.tag(&[&block[..7], &block[7..]]), keyed.tag(&[&block]));
    }

    #[test]
    fn a_plaintext_region_is_stored_as_written_and_refused_when_moved_or_rotted() {
        let plain = *b"a directory entry name or file data block content!!";
        let seal = RegionSeal::Checksum;
        let mut region = plain;
        let mut trailer = [0xEEu8; CRYPTO_TRAILER];
        seal.seal(&mut region, &mut trailer, 42, 7).expect("seal");
        assert_eq!(region, plain, "a plaintext region is not transformed");
        assert!(trailer[..AEAD_NONCE_LEN].iter().all(|&byte| byte == 0));

        assert_eq!(seal.open(&mut region, &trailer, 42), Ok(()));
        assert!(
            seal.open(&mut region, &trailer, 43).is_err(),
            "read at another address"
        );
        let mut rotted = region;
        rotted[3] ^= 0x10;
        assert!(seal.open(&mut rotted, &trailer, 42).is_err());
        let mut padded = trailer;
        padded[CRYPTO_TRAILER - 1] = 1;
        assert!(
            seal.open(&mut region, &padded, 42).is_err(),
            "a nonzero pad byte"
        );
    }

    #[test]
    fn an_aead_region_seal_encrypts_and_authenticates() {
        let (_, keys) = provision(&VK, &mut CountingEntropy::new(7)).expect("provision");
        let seal = RegionSeal::Aead(keys.content_key);
        let plain = *b"content sealed through the volume's region seal...";
        let mut region = plain;
        let mut trailer = [0u8; CRYPTO_TRAILER];
        seal.seal(&mut region, &mut trailer, 9, 1).expect("seal");
        assert_ne!(region, plain);
        assert_eq!(seal.open(&mut region, &trailer, 9), Ok(()));
        assert_eq!(region, plain);
        assert!(RegionSeal::Checksum.open(&mut region, &trailer, 9).is_err());
    }

    #[test]
    fn a_plaintext_volumes_dedupe_domain_follows_its_uuid() {
        let a = Protection::plaintext(0x1111);
        assert_eq!(a.dedupe_domain, Protection::plaintext(0x1111).dedupe_domain);
        assert_ne!(a.dedupe_domain, Protection::plaintext(0x2222).dedupe_domain);
        assert_eq!(a.kind(), VolumeProtection::Plaintext);
    }
}
