//! The machine-settings protocol (`settings-v1`): an administrator's program,
//! run elevated, asks the settings service to read or change one of the
//! machine's settings documents.
//!
//! The service is the one writer of the administrator's layer of each
//! [`SystemConfigFile`]. A change names its document and the settings it
//! sets by the names that document's registry spells, and the service applies
//! them all or none through the document's own engine, so a change is never
//! half-written and two programs changing one document at once lose neither
//! change. Every request is answered with the document as it stands after
//! it ([`SystemConfigFile::read_effective`]) — a read's as it is, a change's
//! as the change left it, so a caller adopts what was written rather than
//! what it asked for — or with the refusal ([`encode_reply`]). The network
//! configuration names the machine's hardware and addresses, so neither
//! answer is open to every account: who may ask is the kernel's decision, the
//! rendezvous being bound requiring
//! [`CAP_SETTINGS_WRITE`](crate::CapabilityId::SETTINGS_WRITE).
//!
//! Every decode fails closed: an unknown magic, version, operation or
//! document, a dirty reserved byte, a read naming settings, a change of none
//! or of more than [`MAX_SETTINGS`], a name that is empty or longer than
//! [`NAME_MAX`], values totalling more than [`DOCUMENT_MAX`], text that is not
//! UTF-8 or carries a control character, or bytes past the end are all
//! refused.

use crate::bounded_text::is_forbidden_character;
use crate::driver_store::SystemConfigFile;
use crate::le::{put_u16, put_u32, read_u16, read_u32};
use crate::reply::{decode_status_reply, put_refusal, STATUS_REPLY_LEN};
use crate::Errno;

/// Reserved well-known call-endpoint id of the settings service (`"SG"`).
///
/// Binding it requires `CAP_IPC_BIND_PRIVILEGED`
/// ([`crate::ipc::is_reserved_endpoint`]): a squatter holding it would be
/// handed every change an administrator makes, and would decide what is
/// written and what a read answers.
pub const SETTINGS_ENDPOINT: u64 = 0x5347_1001;

/// Magic number identifying a settings request (`"STG1"` little-endian).
pub const SETTINGS_MAGIC: u32 = u32::from_le_bytes(*b"STG1");

/// The `settings-v1` protocol version.
pub const SETTINGS_VERSION_V1: u16 = 1;

/// Magic (4), version (2), operation (1), document (1), count (2), and a
/// reserved pair.
const HEADER_LEN: usize = 12;

/// A setting's name length (1) and value length (2).
const SETTING_HEADER_LEN: usize = 3;

/// Operation discriminants.
mod op {
    pub const READ: u8 = 1;
    pub const SET: u8 = 2;
}

/// The most settings one change sets.
///
/// A validation bound: each document's engine pins that naming every one of
/// its settings at once fits.
pub const MAX_SETTINGS: usize = 512;

/// The longest setting name. Each document's engine pins that its longest
/// name fits.
pub const NAME_MAX: usize = 64;

/// The longest document a reply carries, and the most value bytes one change
/// carries: every value lands in the document it changes. Each document's
/// engine pins that its own ceiling fits.
pub const DOCUMENT_MAX: usize = 16 * 1024;

/// The longest request: a change of the most settings, each at the longest
/// name, with values totalling [`DOCUMENT_MAX`].
pub const SETTINGS_REQUEST_MAX: usize =
    HEADER_LEN + MAX_SETTINGS * (SETTING_HEADER_LEN + NAME_MAX) + DOCUMENT_MAX;

/// A reply's status word, then the document's length.
const REPLY_HEADER_LEN: usize = STATUS_REPLY_LEN + 2;

/// The longest reply: the longest document.
pub const SETTINGS_REPLY_MAX: usize = REPLY_HEADER_LEN + DOCUMENT_MAX;

const _: () = assert!(NAME_MAX <= u8::MAX as usize && DOCUMENT_MAX <= u16::MAX as usize);

/// One request to the settings service.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SettingsRequest<'a> {
    /// Answer `document` as it stands.
    Read {
        /// The document to read.
        document: SystemConfigFile,
    },
    /// Set each of `settings` in `document`, all or none, and answer it as
    /// the change left it.
    Set {
        /// The document to change.
        document: SystemConfigFile,
        /// What to set in it.
        settings: Settings<'a>,
    },
}

impl<'a> SettingsRequest<'a> {
    /// Decode and validate a whole request.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] for a truncated frame, [`Errno::BadMagic`]
    /// for a foreign magic, [`Errno::AbiVersionUnsupported`] for another
    /// version, and [`Errno::OutOfRange`] for anything else the module
    /// refuses.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Errno> {
        let head = bytes.get(..HEADER_LEN).ok_or(Errno::BufferTooSmall)?;
        if read_u32(head, 0) != SETTINGS_MAGIC {
            return Err(Errno::BadMagic);
        }
        if read_u16(head, 4) != SETTINGS_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        if read_u16(head, 10) != 0 {
            return Err(Errno::OutOfRange);
        }
        let document = SystemConfigFile::from_u8(head[7]).ok_or(Errno::OutOfRange)?;
        let count = usize::from(read_u16(head, 8));
        let body = &bytes[HEADER_LEN..];
        match head[6] {
            op::READ if count == 0 && body.is_empty() => Ok(Self::Read { document }),
            op::SET if count != 0 && count <= MAX_SETTINGS => {
                let mut rest = body;
                let mut values = 0;
                for _ in 0..count {
                    let ((_, value), used) = setting_at(rest)?;
                    values += value.len();
                    rest = &rest[used..];
                }
                if values > DOCUMENT_MAX || !rest.is_empty() {
                    return Err(Errno::OutOfRange);
                }
                Ok(Self::Set {
                    document,
                    settings: Settings { count, bytes: body },
                })
            }
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// The length of a read request: what [`encode_read`] writes.
pub const READ_REQUEST_LEN: usize = HEADER_LEN;

/// Encode a read of `document` into `out`, returning the frame's length.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_read(document: SystemConfigFile, out: &mut [u8]) -> Result<usize, Errno> {
    put_header(out, op::READ, document, 0)?;
    Ok(HEADER_LEN)
}

/// The length [`encode_set`] writes for `settings`, so a caller sizes its
/// buffer to the change rather than to [`SETTINGS_REQUEST_MAX`]. Saturates
/// rather than wrapping, so an oversized change is still refused by the
/// encoder.
#[must_use]
pub fn set_request_len(settings: &[(&str, &str)]) -> usize {
    settings.iter().fold(HEADER_LEN, |len, (name, value)| {
        len.saturating_add(SETTING_HEADER_LEN)
            .saturating_add(name.len())
            .saturating_add(value.len())
    })
}

/// Encode a change of `document` setting each of `settings` into `out`,
/// returning the frame's length.
///
/// # Errors
///
/// [`Errno::LengthOutOfRange`] for no settings, more than [`MAX_SETTINGS`],
/// a name past [`NAME_MAX`], or values past [`DOCUMENT_MAX`];
/// [`Errno::OutOfRange`] for an empty name or text carrying a control
/// character; [`Errno::BufferTooSmall`] if `out` cannot hold the frame.
pub fn encode_set(
    document: SystemConfigFile,
    settings: &[(&str, &str)],
    out: &mut [u8],
) -> Result<usize, Errno> {
    if settings.is_empty() || settings.len() > MAX_SETTINGS {
        return Err(Errno::LengthOutOfRange);
    }
    let count = u16::try_from(settings.len()).map_err(|_| Errno::LengthOutOfRange)?;
    put_header(out, op::SET, document, count)?;
    let mut at = HEADER_LEN;
    let mut values = 0;
    for (name, value) in settings {
        let name_len = check_name(name)?;
        let value_len = check_value(value)?;
        values += value.len();
        if values > DOCUMENT_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        let end = at + SETTING_HEADER_LEN + name.len() + value.len();
        let frame = out.get_mut(at..end).ok_or(Errno::BufferTooSmall)?;
        frame[0] = name_len;
        put_u16(frame, 1, value_len);
        let (name_out, value_out) = frame[SETTING_HEADER_LEN..].split_at_mut(name.len());
        name_out.copy_from_slice(name.as_bytes());
        value_out.copy_from_slice(value.as_bytes());
        at = end;
    }
    Ok(at)
}

/// Write a request header into the front of `out`.
fn put_header(
    out: &mut [u8],
    operation: u8,
    document: SystemConfigFile,
    count: u16,
) -> Result<(), Errno> {
    let head = out.get_mut(..HEADER_LEN).ok_or(Errno::BufferTooSmall)?;
    put_u32(head, 0, SETTINGS_MAGIC);
    put_u16(head, 4, SETTINGS_VERSION_V1);
    head[6] = operation;
    head[7] = document.as_u8();
    put_u16(head, 8, count);
    put_u16(head, 10, 0);
    Ok(())
}

/// Encode the answer to a request: the document as it stands after it (empty
/// where neither layer exists), or the refusal. Returns the frame's length.
///
/// # Errors
///
/// [`Errno::LengthOutOfRange`] for a document past [`DOCUMENT_MAX`] and
/// [`Errno::BufferTooSmall`] if `out` cannot hold the frame.
pub fn encode_reply(answer: Result<&str, Errno>, out: &mut [u8]) -> Result<usize, Errno> {
    let document = match answer {
        Ok(document) => document,
        Err(err) => {
            let status = out
                .get_mut(..STATUS_REPLY_LEN)
                .ok_or(Errno::BufferTooSmall)?;
            put_refusal(status, err);
            return Ok(STATUS_REPLY_LEN);
        }
    };
    let len = u16::try_from(document.len())
        .ok()
        .filter(|_| document.len() <= DOCUMENT_MAX)
        .ok_or(Errno::LengthOutOfRange)?;
    let end = REPLY_HEADER_LEN + document.len();
    let frame = out.get_mut(..end).ok_or(Errno::BufferTooSmall)?;
    frame[..STATUS_REPLY_LEN].fill(0);
    put_u16(frame, STATUS_REPLY_LEN, len);
    frame[REPLY_HEADER_LEN..].copy_from_slice(document.as_bytes());
    Ok(end)
}

/// Decode the answer to a request: the document as it stands after it.
///
/// # Errors
///
/// The service's refusal; [`Errno::BufferTooSmall`] for a truncated frame;
/// [`Errno::OutOfRange`] for a length that disagrees with the frame, a
/// document past [`DOCUMENT_MAX`], or one that is not UTF-8.
pub fn decode_reply(bytes: &[u8]) -> Result<&str, Errno> {
    decode_status_reply(bytes)?;
    let payload = &bytes[STATUS_REPLY_LEN..];
    let head = payload.get(..2).ok_or(Errno::BufferTooSmall)?;
    let len = usize::from(read_u16(head, 0));
    let document = &payload[2..];
    if document.len() != len || len > DOCUMENT_MAX {
        return Err(Errno::OutOfRange);
    }
    core::str::from_utf8(document).map_err(|_| Errno::OutOfRange)
}

/// The settings of one change, validated whole when decoded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Settings<'a> {
    count: usize,
    bytes: &'a [u8],
}

impl<'a> Settings<'a> {
    /// Each `(name, value)`, in the order written.
    #[must_use]
    pub const fn iter(&self) -> SettingsIter<'a> {
        SettingsIter {
            rest: self.bytes,
            left: self.count,
        }
    }
}

impl<'a> IntoIterator for Settings<'a> {
    type Item = (&'a str, &'a str);
    type IntoIter = SettingsIter<'a>;

    fn into_iter(self) -> SettingsIter<'a> {
        self.iter()
    }
}

impl<'a> IntoIterator for &Settings<'a> {
    type Item = (&'a str, &'a str);
    type IntoIter = SettingsIter<'a>;

    fn into_iter(self) -> SettingsIter<'a> {
        self.iter()
    }
}

/// An iterator over a change's settings ([`Settings::iter`]).
#[derive(Clone, Debug)]
pub struct SettingsIter<'a> {
    rest: &'a [u8],
    left: usize,
}

impl<'a> Iterator for SettingsIter<'a> {
    type Item = (&'a str, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        let (setting, used) = setting_at(self.rest).ok()?;
        self.rest = self.rest.get(used..)?;
        self.left -= 1;
        Some(setting)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.left, Some(self.left))
    }
}

impl ExactSizeIterator for SettingsIter<'_> {}

/// A setting name's wire length, refusing an empty or over-long one or one
/// carrying a control character.
fn check_name(name: &str) -> Result<u8, Errno> {
    if name.len() > NAME_MAX {
        return Err(Errno::LengthOutOfRange);
    }
    if name.is_empty() || has_control(name) {
        return Err(Errno::OutOfRange);
    }
    u8::try_from(name.len()).map_err(|_| Errno::LengthOutOfRange)
}

/// A value's wire length, refusing an over-long one or one carrying a control
/// character. A value may be empty: that is how a setting with no default is
/// unset.
fn check_value(value: &str) -> Result<u16, Errno> {
    if value.len() > DOCUMENT_MAX {
        return Err(Errno::LengthOutOfRange);
    }
    if has_control(value) {
        return Err(Errno::OutOfRange);
    }
    u16::try_from(value.len()).map_err(|_| Errno::LengthOutOfRange)
}

/// Whether `text` carries a control character, a line break included: every
/// value is one line of its document.
fn has_control(text: &str) -> bool {
    text.chars().any(|c| is_forbidden_character(c, false))
}

/// The setting at the front of `bytes`, and how many bytes it takes.
fn setting_at(bytes: &[u8]) -> Result<((&str, &str), usize), Errno> {
    let head = bytes
        .get(..SETTING_HEADER_LEN)
        .ok_or(Errno::BufferTooSmall)?;
    let name_len = usize::from(head[0]);
    let end = SETTING_HEADER_LEN + name_len + usize::from(read_u16(head, 1));
    let body = bytes
        .get(SETTING_HEADER_LEN..end)
        .ok_or(Errno::BufferTooSmall)?;
    let (name, value) = body.split_at(name_len);
    let name = core::str::from_utf8(name).map_err(|_| Errno::OutOfRange)?;
    let value = core::str::from_utf8(value).map_err(|_| Errno::OutOfRange)?;
    check_name(name).map_err(|_| Errno::OutOfRange)?;
    check_value(value).map_err(|_| Errno::OutOfRange)?;
    Ok(((name, value), end))
}

#[cfg(test)]
#[path = "settings_ipc_tests.rs"]
mod tests;
