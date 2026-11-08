//! Encryption (ISO 32000-2 §7.6): the standard security handler opening a
//! file with its password, and each object's strings and streams decrypted
//! under its own key. The constructions are `lib/crypto`'s.

use alloc::borrow::Cow;
use alloc::vec::Vec;

use tairix_crypto::pdf::{
    pdf_aes_decrypt, pdf_legacy_file_key, pdf_modern_file_key, pdf_object_key, pdf_rc4_decrypt,
    PdfKey, PdfLegacyEntries, PdfModernEntries,
};
use tairix_unicode::{saslprep, Unassigned, MAX_DECOMPOSITION};
use tairix_util::fallible;
use tairix_util::secret::{Wiped, WipedBuf};

use crate::document::Document;
use crate::limits::MAX_NESTING;
use crate::object::{Dict, ObjRef, Object};
use crate::pdfdoc;
use crate::PdfError;

/// How strings or streams are encrypted.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Method {
    /// Not at all.
    Identity,
    Rc4,
    Aes128,
    Aes256,
}

/// An encrypted file's key, and how its strings and streams were encrypted.
pub(crate) struct Security {
    key: PdfKey,
    strings: Method,
    streams: Method,
    /// Whether its metadata streams are encrypted.
    metadata: bool,
    /// The encryption dictionary's own object, whose strings are not.
    dictionary: Option<u32>,
    /// The crypt filters a stream may name.
    filters: Filters,
}

impl Security {
    /// The security of the file whose trailer is `trailer`, opened with
    /// `password` — and with the empty password first, which opens a file
    /// protected only by its permissions: `None` for a file not encrypted.
    pub(crate) fn open(
        document: &Document<'_>,
        trailer: &Dict,
        password: Option<&[u8]>,
    ) -> Result<Option<Self>, PdfError> {
        let Some(entry) = trailer.get(b"Encrypt") else {
            return Ok(None);
        };
        let dictionary = entry.as_reference().map(|reference| reference.num);
        let resolved = document.resolve(entry)?;
        let dict = resolved.as_dict().ok_or(PdfError::Malformed)?;
        if dict.name(b"Filter") != Some(b"Standard") {
            return Err(PdfError::Unsupported);
        }
        let int =
            |key: &[u8]| -> Result<Option<i64>, PdfError> { Ok(document.get(dict, key)?.as_int()) };
        let version = int(b"V")?.unwrap_or(0);
        let revision = u8::try_from(int(b"R")?.ok_or(PdfError::Malformed)?)
            .map_err(|_| PdfError::Unsupported)?;
        let metadata = document
            .get(dict, b"EncryptMetadata")?
            .as_bool()
            .unwrap_or(true);
        let Methods {
            strings,
            streams,
            filters,
            stream_key_bits,
        } = methods(document, dict, version)?;
        let id = trailer
            .get(b"ID")
            .and_then(Object::as_array)
            .and_then(|ids| ids.first())
            .and_then(Object::as_string)
            .unwrap_or_default();
        let (owner, user) = (document.get(dict, b"O")?, document.get(dict, b"U")?);
        let owner = owner.as_string().ok_or(PdfError::Malformed)?;
        let user = user.as_string().ok_or(PdfError::Malformed)?;
        let wrapped = if revision >= 5 {
            let wrapped_key = |key: &[u8]| {
                document
                    .get(dict, key)?
                    .as_string()
                    .and_then(|bytes| bytes.first_chunk().copied())
                    .ok_or(PdfError::Malformed)
            };
            Some((wrapped_key(b"OE")?, wrapped_key(b"UE")?))
        } else {
            None
        };
        let key_with = |given: &[u8]| -> Result<Option<PdfKey>, PdfError> {
            if let Some((owner_key, user_key)) = &wrapped {
                let entries = PdfModernEntries {
                    revision,
                    owner: owner.first_chunk().ok_or(PdfError::Malformed)?,
                    user: user.first_chunk().ok_or(PdfError::Malformed)?,
                    owner_key,
                    user_key,
                };
                // SASLprep's form first, as the handler asks, then the bytes as
                // given, which a producer that prepares nothing hashed.
                let prepared = modern_password(given)?;
                for candidate in prepared.as_deref().into_iter().chain([given]) {
                    let key = pdf_modern_file_key(&entries, candidate)
                        .map_err(|_| PdfError::Unsupported)?;
                    if key.is_some() {
                        return Ok(key);
                    }
                }
                return Ok(None);
            }
            let bits = match version {
                0 | 1 => 40,
                2 | 3 => int(b"Length")?.unwrap_or(40),
                _ => stream_key_bits,
            };
            let entries = PdfLegacyEntries {
                revision,
                key_len: usize::try_from(bits / 8).map_err(|_| PdfError::Malformed)?,
                owner,
                user,
                permissions: permissions(int(b"P")?.ok_or(PdfError::Malformed)?),
                id,
                encrypt_metadata: metadata,
            };
            let encoded = legacy_password(given)?;
            pdf_legacy_file_key(&entries, encoded.as_deref().unwrap_or(given))
                .map_err(|_| PdfError::Unsupported)
        };
        let key = match key_with(&[])? {
            Some(key) => key,
            None => match password {
                None => return Err(PdfError::NeedsPassword),
                Some(given) => key_with(given)?.ok_or(PdfError::WrongPassword)?,
            },
        };
        Ok(Some(Self {
            key,
            strings,
            streams,
            metadata,
            dictionary,
            filters,
        }))
    }

    /// Decrypt every string `object`, indirect object `owner`, holds.
    pub(crate) fn decrypt_object(
        &self,
        object: &mut Object,
        owner: ObjRef,
    ) -> Result<(), PdfError> {
        if self.dictionary == Some(owner.num) || self.strings == Method::Identity {
            return Ok(());
        }
        if object.as_dict().is_some_and(|dict| dict.is_type(b"XRef")) {
            return Ok(());
        }
        let method = self.strings;
        self.with_object_key(owner, method, |key| decrypt_strings(object, key, method, 0))
    }

    /// Decrypt every string stream `owner`'s dictionary `dict` holds.
    pub(crate) fn decrypt_dict(&self, dict: &mut Dict, owner: ObjRef) -> Result<(), PdfError> {
        if self.dictionary == Some(owner.num)
            || self.strings == Method::Identity
            || dict.is_type(b"XRef")
        {
            return Ok(());
        }
        let method = self.strings;
        self.with_object_key(owner, method, |key| {
            for value in dict.values_mut() {
                decrypt_strings(value, key, method, 0)?;
            }
            Ok(())
        })
    }

    /// Stream `owner`'s bytes `data`, of dictionary `dict`, decrypted: not a
    /// cross-reference stream, nor metadata a file leaves in the clear, nor a
    /// stream whose own crypt filter is the identity.
    pub(crate) fn decrypt_stream<'d>(
        &self,
        dict: &Dict,
        data: &'d [u8],
        owner: ObjRef,
        crypt: Option<&[u8]>,
    ) -> Result<Cow<'d, [u8]>, PdfError> {
        let method = match crypt {
            Some(name) => self.named(name),
            None => self.streams,
        };
        let clear = dict.is_type(b"XRef") || (dict.is_type(b"Metadata") && !self.metadata);
        if method == Method::Identity || clear {
            return Ok(Cow::Borrowed(data));
        }
        let mut bytes = fallible::copied(data).ok_or(PdfError::OutOfMemory)?;
        let kept = self.with_object_key(owner, method, |key| decrypt(&mut bytes, key, method))?;
        let end = kept.end;
        bytes.truncate(end);
        bytes.drain(..kept.start);
        Ok(Cow::Owned(bytes))
    }

    /// The method crypt filter `name` names: the identity for `/Identity` or
    /// one the file does not define.
    fn named(&self, name: &[u8]) -> Method {
        self.filters
            .iter()
            .find(|filter| filter.name == name)
            .map_or(Method::Identity, |filter| filter.method)
    }

    /// `work` done with the key object `owner` is encrypted with under
    /// `method`: AES-256's file key itself, and otherwise one made for the
    /// object.
    fn with_object_key<R>(
        &self,
        owner: ObjRef,
        method: Method,
        work: impl FnOnce(&PdfKey) -> R,
    ) -> R {
        if method == Method::Aes256 {
            return work(&self.key);
        }
        work(&pdf_object_key(
            &self.key,
            owner.num,
            owner.gen,
            method == Method::Aes128,
        ))
    }
}

/// `/P` as the handler hashes it: its low 32 bits.
fn permissions(value: i64) -> i32 {
    let [a, b, c, d, ..] = value.to_le_bytes();
    i32::from_le_bytes([a, b, c, d])
}

/// A password as revisions 2 to 4 hash it where that differs from its bytes
/// as given: in `PDFDocEncoding`, as the handler asks, where it is text every
/// character of which that encoding holds. The copy is erased when dropped.
fn legacy_password(given: &[u8]) -> Result<Option<WipedBuf>, PdfError> {
    let Ok(text) = core::str::from_utf8(given) else {
        return Ok(None);
    };
    if text.is_ascii() || !text.chars().all(|c| pdfdoc::code_of(c).is_some()) {
        return Ok(None);
    }
    let count = text.chars().count();
    let mut encoded = WipedBuf::with_capacity(count)?;
    let bytes = encoded.extend_zeroed(count).ok_or(PdfError::OutOfMemory)?;
    for (byte, c) in bytes.iter_mut().zip(text.chars()) {
        *byte = pdfdoc::code_of(c).unwrap_or(b'?');
    }
    Ok(Some(encoded))
}

/// A password as revisions 5 and 6 hash it where that differs from its bytes
/// as given (ISO 32000-2 §7.6.4.3.3): its text prepared by SASLprep (RFC
/// 4013) as a query, so a code point Unicode 3.2 does not assign passes as it
/// is, in UTF-8. `None` for a password that is no text, that SASLprep
/// refuses, or that it leaves as it was. Every copy is erased when dropped.
fn modern_password(given: &[u8]) -> Result<Option<WipedBuf>, PdfError> {
    let Ok(text) = core::str::from_utf8(given) else {
        return Ok(None);
    };
    // ASCII is prepared to itself, or refused for a control it holds.
    if text.is_ascii() {
        return Ok(None);
    }
    let room = text
        .chars()
        .count()
        .checked_mul(MAX_DECOMPOSITION)
        .ok_or(PdfError::OutOfMemory)?;
    let mut chars = WipedBuf::<char>::with_capacity(room)?;
    let work = chars.extend_zeroed(room).ok_or(PdfError::OutOfMemory)?;
    let Ok(len) = saslprep(text, Unassigned::Allowed, work) else {
        return Ok(None);
    };
    let prepared = work.get(..len).unwrap_or_default();
    if prepared.iter().copied().eq(text.chars()) {
        return Ok(None);
    }
    let bytes = prepared.iter().map(|ch| ch.len_utf8()).sum();
    let mut encoded = WipedBuf::with_capacity(bytes)?;
    let out = encoded.extend_zeroed(bytes).ok_or(PdfError::OutOfMemory)?;
    let mut at = 0;
    for &ch in prepared {
        let mut one = Wiped::<4>::new();
        let spelled = ch.encode_utf8(&mut *one).as_bytes();
        let end = at + spelled.len();
        if let Some(into) = out.get_mut(at..end) {
            into.copy_from_slice(spelled);
        }
        at = end;
    }
    Ok(Some(encoded))
}

/// A crypt filter a file defines: its name, its method, and the key length in
/// bits its `/Length` states.
struct Filter {
    name: Vec<u8>,
    method: Method,
    key_bits: Option<i64>,
}

/// The crypt filters a file defines.
type Filters = Vec<Filter>;

/// How an encryption dictionary encrypts strings and streams.
struct Methods {
    strings: Method,
    streams: Method,
    /// The crypt filters a stream may name.
    filters: Filters,
    /// The file key's length in bits a version 4 dictionary sets: its stream
    /// filter's, else 128.
    stream_key_bits: i64,
}

/// How an encryption dictionary of version `version` encrypts.
fn methods(document: &Document<'_>, dict: &Dict, version: i64) -> Result<Methods, PdfError> {
    let mut filters = Vec::new();
    if version < 4 {
        return Ok(Methods {
            strings: Method::Rc4,
            streams: Method::Rc4,
            filters,
            stream_key_bits: 40,
        });
    }
    let defined = document.get(dict, b"CF")?;
    if let Some(defined) = defined.as_dict() {
        for (name, filter) in defined.iter() {
            let filter = document.resolve(filter)?;
            let filter = filter.as_dict();
            let method = match filter.and_then(|filter| filter.name(b"CFM")) {
                Some(b"V2") => Method::Rc4,
                Some(b"AESV2") => Method::Aes128,
                Some(b"AESV3") => Method::Aes256,
                Some(b"None") | None => Method::Identity,
                Some(_) => return Err(PdfError::Unsupported),
            };
            let key_bits = match filter {
                Some(filter) => document.get(filter, b"Length")?.as_int().map(in_bits),
                None => None,
            };
            let name = fallible::copied(name.as_bytes()).ok_or(PdfError::OutOfMemory)?;
            fallible::try_push(
                &mut filters,
                Filter {
                    name,
                    method,
                    key_bits,
                },
            )?;
        }
    }
    let chosen = |key: &[u8]| -> Result<Option<&Filter>, PdfError> {
        Ok(document
            .get(dict, key)?
            .as_name()
            .and_then(|name| filters.iter().find(|filter| filter.name == name)))
    };
    let method = |filter: Option<&Filter>| filter.map_or(Method::Identity, |filter| filter.method);
    let (strings, streams) = (chosen(b"StrF")?, chosen(b"StmF")?);
    let stream_key_bits = streams
        .or(strings)
        .and_then(|filter| filter.key_bits)
        .unwrap_or(128);
    let (strings, streams) = (method(strings), method(streams));
    Ok(Methods {
        strings,
        streams,
        filters,
        stream_key_bits,
    })
}

/// A crypt filter's `/Length` in bits: the standard handler states it in
/// bytes, and a value too small to be bits is read as bytes where a producer
/// wrote bits.
const fn in_bits(length: i64) -> i64 {
    if length < 40 {
        length.saturating_mul(8)
    } else {
        length
    }
}

/// `bytes` decrypted in place under `key` by `method`, answering where the
/// plaintext lies.
///
/// # Errors
///
/// [`PdfError::Malformed`] for an AES method over a key AES takes no such
/// length of, which only a damaged encryption dictionary makes.
fn decrypt(
    bytes: &mut [u8],
    key: &PdfKey,
    method: Method,
) -> Result<core::ops::Range<usize>, PdfError> {
    match method {
        Method::Identity => Ok(0..bytes.len()),
        Method::Rc4 => {
            pdf_rc4_decrypt(key, bytes);
            Ok(0..bytes.len())
        }
        Method::Aes128 | Method::Aes256 => pdf_aes_decrypt(key, bytes).ok_or(PdfError::Malformed),
    }
}

fn decrypt_strings(
    object: &mut Object,
    key: &PdfKey,
    method: Method,
    depth: usize,
) -> Result<(), PdfError> {
    if depth > MAX_NESTING {
        return Err(PdfError::TooComplex);
    }
    match object {
        Object::String(bytes) => {
            let kept = decrypt(bytes, key, method)?;
            let end = kept.end;
            bytes.truncate(end);
            bytes.drain(..kept.start);
        }
        Object::Array(items) => {
            for item in items {
                decrypt_strings(item, key, method, depth + 1)?;
            }
        }
        Object::Dict(dict) | Object::Stream(crate::object::Stream { dict, .. }) => {
            for value in dict.values_mut() {
                decrypt_strings(value, key, method, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
#[path = "crypt_tests.rs"]
mod tests;
