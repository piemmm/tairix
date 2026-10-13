//! The pinboard-apply IPC protocol (`plans/PINBOARD.md` §6): the reserved
//! rendezvous the desktop session binds to accept a new pinboard settings
//! document from the Settings application (and, later, any other tool the
//! user runs to change their desktop).
//!
//! Settings and the backdrop's own context menu both **ask**; the session
//! **decides, applies, and persists** (`plans/PINBOARD.md` §6). A
//! caller renders its intended settings into the `key value` document
//! `lib/wallpaper` owns and posts it here; the session is the one engine
//! that parses that grammar, applies the result, and rewrites the user's
//! own settings file. This module never parses the document's grammar
//! itself — it validates only that the bytes are a well-formed, bounded,
//! multi-line transportable UTF-8 text, exactly as `lib/wallpaper`'s own
//! parser re-validates on arrival.
//!
//! # Security posture
//!
//! The document is configuration data and carries **no authority**: it
//! names a wallpaper path and a handful of closed-set option values, never
//! a credential. The session serving [`PINBOARD_ENDPOINT`] accepts a
//! request only from a caller whose kernel-attested [`crate::origin`] uid
//! matches its own — anything else is refused and logged — and it then
//! reads any path the document names **under its own identity**. A caller
//! can therefore never use this channel to reach a file it could not read
//! itself: the worst a hostile or buggy caller can do is ask the session to
//! adopt a document the session's own read access already bounds.
//!
//! [`PinboardRequest::Apply`] carries the desktop's own document. The sound
//! requests read and edit the user's sound choices, which the session keeps
//! privately and hands the cue authority while it holds the seat
//! (`plans/SOUND.md`): a caller reads them ([`encode_sound_reading`]) and the
//! themes to choose among ([`encode_sound_themes`]), lays settings over them,
//! and has the user choose a file in the session's own picker or go back to
//! the theme's sound. No request names a path, so no caller can have the
//! session play a file the user did not choose. Every other reply is the
//! shared status frame ([`crate::reply::encode_status_reply`] /
//! [`crate::reply::decode_status_reply`]).
//!
//! Every decode fails closed: an unknown magic, version, operation or event, a
//! frame of any length but its operation's, an over-long or empty document,
//! non-UTF-8, a forbidden control character, or a dirty reserved byte all
//! refuse rather than guessing.

use crate::bounded_text::{is_forbidden_character, WideText};
use crate::le::{exact_len, put_u16, put_u32, read_u16, read_u32};
use crate::reply::{decode_page_reply, encode_page_reply, put_refusal, take_payload};
use crate::reply::{PAGE_HEADER_LEN, STATUS_REPLY_LEN};
use crate::sound_cue::{SoundEvent, POLICY_DOCUMENT_MAX};
use crate::{Errno, FS_NAME_MAX};

/// Reserved well-known call-endpoint id of the desktop session's pinboard
/// service (`"PB"` ASCII hex-spelled prefix, mirroring
/// [`crate::notify_ipc::NOTIFY_ENDPOINT`]'s convention). Like the window,
/// notification, and Switchboard tray-summary rendezvous it is
/// **seat-scoped** ([`crate::ipc::is_reserved_endpoint`],
/// [`crate::ipc::is_seat_scoped_endpoint`]): the kernel authorises its bind
/// either by `CAP_IPC_BIND_PRIVILEGED` or by the caller's kernel-attested
/// **live seat lease** — the desktop session that owns the seat serves the
/// pinboard shown on it, and nothing else may. A squatter claiming the
/// rendezvous first could adopt a fabricated document into another user's
/// session, so an unentitled bind fails closed.
pub const PINBOARD_ENDPOINT: u64 = 0x5042_1001;

/// Magic number identifying a pinboard-channel request (`"PIN1"`
/// little-endian).
pub const PINBOARD_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"PIN1");

/// The `pinboard-v1` protocol version.
pub const PINBOARD_VERSION_V1: u16 = 1;

/// Maximum request, in bytes, the [`PINBOARD_ENDPOINT`] accepts: a document
/// of the longest length.
pub const PINBOARD_MAX_REQUEST: usize = DOCUMENT_OFFSET + PINBOARD_DOCUMENT_MAX;

/// Maximum reply, in bytes, the [`PINBOARD_ENDPOINT`] answers with: the
/// longer of a sound reading and a page of themes.
pub const PINBOARD_MAX_REPLY: usize = if SOUND_READING_MAX > SOUND_THEMES_REPLY_MAX {
    SOUND_READING_MAX
} else {
    SOUND_THEMES_REPLY_MAX
};

/// Maximum encoded length, in bytes, of a rendered desktop settings document.
///
/// A validation bound, not a capacity: an apply carries one group of the
/// desktop's keys, and the widest group is the notification policy, whose
/// source list alone may fill one whole settings value
/// ([`crate::appdata_ipc::APPDATA_VALUE_MAX`]). Twice that leaves room for the
/// group's other lines; a document that needs more is refused, never
/// truncated.
pub const PINBOARD_DOCUMENT_MAX: usize = 2 * crate::appdata_ipc::APPDATA_VALUE_MAX;

/// A rendered pinboard settings document: at least one and at most
/// [`PINBOARD_DOCUMENT_MAX`] bytes of well-formed UTF-8.
///
/// A settings document is legitimately **multi-line**: the `key = value`
/// grammar `lib/appconf` owns, over which `lib/wallpaper` defines the
/// pinboard registry (`plans/PINBOARD.md` §2), puts one setting per line —
/// so it is the shared wide validator's newline-privileged form. No other
/// control character is permitted, so the wire form stays a flat,
/// unambiguous byte stream the receiving parser can split on `'\n'` alone.
///
/// The document is validated at construction *and* again at decode, so a
/// value that reached a [`PinboardRequest`] is always well-formed. It is
/// never sanitised: a malformed document is refused, not silently repaired.
pub type PinboardDocument = WideText<1, PINBOARD_DOCUMENT_MAX, true>;

/// One pinboard-channel operation (`plans/PINBOARD.md` §6, `plans/SOUND.md`).
///
/// The session alone decides whether to honour a request (see the module's
/// security posture above), applies it, and persists it — the caller never
/// writes a settings file itself.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PinboardRequest {
    /// Ask the session to adopt `document` as the caller's pinboard
    /// settings.
    Apply {
        /// The rendered settings document (`plans/PINBOARD.md` §2);
        /// parsed and validated by the one engine `lib/wallpaper` owns,
        /// never by this module.
        document: PinboardDocument,
    },
    /// Say what the user's sound choices are ([`encode_sound_reading`]).
    SoundRead,
    /// Name the sound themes from the `from`th on, in name order
    /// ([`encode_sound_themes`]).
    SoundThemes {
        /// How many themes the pages before this one named.
        from: u16,
    },
    /// Lay `document`'s sound choices over the user's own.
    SoundApply {
        /// The rendered sound settings, naming no file; parsed by the one
        /// engine `lib/soundtheme` owns.
        document: PinboardDocument,
    },
    /// Have the user choose, in the session's own picker, the file `event`
    /// plays; a file chosen turns the event on. Answered once the picker is
    /// closed, chosen or not.
    SoundPick {
        /// The event the file is for.
        event: SoundEvent,
    },
    /// Have `event` play its theme's sound again, turning it on.
    SoundUseTheme {
        /// The event.
        event: SoundEvent,
    },
}

/// Wire operation discriminants.
mod op {
    pub const APPLY: u16 = 1;
    pub const SOUND_READ: u16 = 2;
    pub const SOUND_THEMES: u16 = 3;
    pub const SOUND_APPLY: u16 = 4;
    pub const SOUND_PICK: u16 = 5;
    pub const SOUND_USE_THEME: u16 = 6;
}

/// Magic (4), version (2) and operation (2).
const HEADER_LEN: usize = 8;
/// Byte offset of the document length prefix.
const DOCUMENT_LEN_OFFSET: usize = HEADER_LEN;
/// Byte offset of a reserved pair following the length prefix; must be
/// zero.
const RESERVED_OFFSET: usize = 10;
/// Byte offset of the document text.
const DOCUMENT_OFFSET: usize = 12;
/// Length of an operation's frame whose body is one small field and a
/// reserved remainder: a theme index, or an event.
const FIELD_FRAME_LEN: usize = HEADER_LEN + 4;

impl PinboardRequest {
    /// Write the request's frame to `out`, answering its length.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for an `out` shorter than the frame.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let len = match self {
            Self::Apply { document } | Self::SoundApply { document } => {
                DOCUMENT_OFFSET + usize::from(document.len_u16())
            }
            Self::SoundRead => HEADER_LEN,
            Self::SoundThemes { .. } | Self::SoundPick { .. } | Self::SoundUseTheme { .. } => {
                FIELD_FRAME_LEN
            }
        };
        let out = out.get_mut(..len).ok_or(Errno::LengthOutOfRange)?;
        out.fill(0);
        put_u32(out, 0, PINBOARD_REQUEST_MAGIC);
        put_u16(out, 4, PINBOARD_VERSION_V1);
        let op = match *self {
            Self::Apply { document } | Self::SoundApply { document } => {
                let text = document.len_u16();
                put_u16(out, DOCUMENT_LEN_OFFSET, text);
                out[DOCUMENT_OFFSET..].copy_from_slice(document.as_str().as_bytes());
                if matches!(self, Self::Apply { .. }) {
                    op::APPLY
                } else {
                    op::SOUND_APPLY
                }
            }
            Self::SoundRead => op::SOUND_READ,
            Self::SoundThemes { from } => {
                put_u16(out, HEADER_LEN, from);
                op::SOUND_THEMES
            }
            Self::SoundPick { event } => {
                out[HEADER_LEN] = event.as_u8();
                op::SOUND_PICK
            }
            Self::SoundUseTheme { event } => {
                out[HEADER_LEN] = event.as_u8();
                op::SOUND_USE_THEME
            }
        };
        put_u16(out, 6, op);
        Ok(len)
    }

    /// Decode from `bytes`, failing closed on any malformed input.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] / [`Errno::LengthOutOfRange`] — shorter
    ///   or longer than its operation's frame, or a document length outside
    ///   `1..=PINBOARD_DOCUMENT_MAX`.
    /// * [`Errno::BadMagic`] — wrong magic or a dirty reserved byte.
    /// * [`Errno::AbiVersionUnsupported`] — not `pinboard-v1`.
    /// * [`Errno::OutOfRange`] — an operation or event outside its closed
    ///   set, or a document that is not UTF-8 or holds a control character
    ///   other than `'\n'`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() < HEADER_LEN {
            return Err(Errno::BufferTooSmall);
        }
        if read_u32(bytes, 0) != PINBOARD_REQUEST_MAGIC {
            return Err(Errno::BadMagic);
        }
        if read_u16(bytes, 4) != PINBOARD_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        match read_u16(bytes, 6) {
            op::APPLY => Ok(Self::Apply {
                document: document_of(bytes)?,
            }),
            op::SOUND_APPLY => Ok(Self::SoundApply {
                document: document_of(bytes)?,
            }),
            op::SOUND_READ => exact_len(bytes, HEADER_LEN).map(|()| Self::SoundRead),
            op::SOUND_THEMES => {
                field_frame(bytes, 2)?;
                Ok(Self::SoundThemes {
                    from: read_u16(bytes, HEADER_LEN),
                })
            }
            op::SOUND_PICK => {
                field_frame(bytes, 1)?;
                Ok(Self::SoundPick {
                    event: SoundEvent::from_u8(bytes[HEADER_LEN])?,
                })
            }
            op::SOUND_USE_THEME => {
                field_frame(bytes, 1)?;
                Ok(Self::SoundUseTheme {
                    event: SoundEvent::from_u8(bytes[HEADER_LEN])?,
                })
            }
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// The document a document-carrying frame holds, which must be exactly as
/// long as it says.
fn document_of(bytes: &[u8]) -> Result<PinboardDocument, Errno> {
    if bytes.len() < DOCUMENT_OFFSET {
        return Err(Errno::BufferTooSmall);
    }
    if read_u16(bytes, RESERVED_OFFSET) != 0 {
        return Err(Errno::BadMagic);
    }
    let len = usize::from(read_u16(bytes, DOCUMENT_LEN_OFFSET));
    if len > PINBOARD_DOCUMENT_MAX {
        return Err(Errno::LengthOutOfRange);
    }
    exact_len(bytes, DOCUMENT_OFFSET + len)?;
    let text = core::str::from_utf8(&bytes[DOCUMENT_OFFSET..]).map_err(|_| Errno::OutOfRange)?;
    PinboardDocument::new(text)
}

/// Refuse a one-field frame of the wrong length or whose bytes past its
/// `field`-byte field are not zero.
fn field_frame(bytes: &[u8], field: usize) -> Result<(), Errno> {
    exact_len(bytes, FIELD_FRAME_LEN)?;
    if bytes[HEADER_LEN + field..].iter().any(|&byte| byte != 0) {
        return Err(Errno::BadMagic);
    }
    Ok(())
}

/// What plays for an event, as the Sound Effects pane shows it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SoundFile<'a> {
    /// The theme's sound.
    Theme,
    /// The file the user chose, by its name.
    Chosen(&'a str),
    /// The file the user chose, by its name, which could not be opened when
    /// the session last handed their choices over: the theme's sound plays
    /// until it can be.
    Unavailable(&'a str),
}

/// The user's sound choices, as the session reads them to a caller.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SoundReading<'a> {
    /// Every choice but the files, as in effect: the user's own over the
    /// machine's. At most [`POLICY_DOCUMENT_MAX`] bytes.
    pub document: &'a str,
    /// What plays for each event, in [`SoundEvent::ALL`] order.
    pub files: [SoundFile<'a>; SoundEvent::ALL.len()],
}

/// File-entry kinds.
const FILE_THEME: u8 = 0;
const FILE_CHOSEN: u8 = 1;
const FILE_UNAVAILABLE: u8 = 2;

/// Where a sound reading's document begins: after the status word, its
/// length and a reserved pair.
const READING_DOCUMENT_OFFSET: usize = STATUS_REPLY_LEN + 4;

/// Each file entry's kind and name length, ahead of the name.
const FILE_ENTRY_HEAD: usize = 2;

/// The longest sound reading: the longest document, and every event with a
/// file of the longest name.
pub const SOUND_READING_MAX: usize = READING_DOCUMENT_OFFSET
    + POLICY_DOCUMENT_MAX
    + SoundEvent::ALL.len() * (FILE_ENTRY_HEAD + FS_NAME_MAX);

/// Whether `name` is a file name a reading may carry: one to [`FS_NAME_MAX`]
/// bytes with no control character.
fn is_shown_name(name: &str) -> bool {
    (1..=FS_NAME_MAX).contains(&name.len())
        && !name.chars().any(|c| is_forbidden_character(c, false))
}

/// Encode the answer to [`PinboardRequest::SoundRead`] into `out`, answering
/// its length.
///
/// # Errors
///
/// [`Errno::LengthOutOfRange`] for an `out` shorter than the frame, a
/// document past [`POLICY_DOCUMENT_MAX`], or a name a reading cannot carry,
/// and [`Errno::OutOfRange`] for a document holding a control character:
/// what the decode would refuse is never written.
pub fn encode_sound_reading(
    result: Result<&SoundReading<'_>, Errno>,
    out: &mut [u8],
) -> Result<usize, Errno> {
    let reading = match result {
        Ok(reading) => reading,
        Err(err) => {
            let out = out
                .get_mut(..STATUS_REPLY_LEN)
                .ok_or(Errno::LengthOutOfRange)?;
            put_refusal(out, err);
            return Ok(STATUS_REPLY_LEN);
        }
    };
    let document = reading.document;
    if document.len() > POLICY_DOCUMENT_MAX {
        return Err(Errno::LengthOutOfRange);
    }
    if document.chars().any(|c| is_forbidden_character(c, true)) {
        return Err(Errno::OutOfRange);
    }
    let mut len = READING_DOCUMENT_OFFSET + document.len();
    for file in &reading.files {
        len += FILE_ENTRY_HEAD;
        if let SoundFile::Chosen(name) | SoundFile::Unavailable(name) = file {
            if !is_shown_name(name) {
                return Err(Errno::LengthOutOfRange);
            }
            len += name.len();
        }
    }
    let out = out.get_mut(..len).ok_or(Errno::LengthOutOfRange)?;
    out.fill(0);
    let text = u16::try_from(document.len()).map_err(|_| Errno::LengthOutOfRange)?;
    put_u16(out, STATUS_REPLY_LEN, text);
    let mut at = READING_DOCUMENT_OFFSET;
    out[at..at + document.len()].copy_from_slice(document.as_bytes());
    at += document.len();
    for file in &reading.files {
        let (kind, name) = match file {
            SoundFile::Theme => (FILE_THEME, ""),
            SoundFile::Chosen(name) => (FILE_CHOSEN, *name),
            SoundFile::Unavailable(name) => (FILE_UNAVAILABLE, *name),
        };
        out[at] = kind;
        out[at + 1] = u8::try_from(name.len()).map_err(|_| Errno::LengthOutOfRange)?;
        at += FILE_ENTRY_HEAD;
        out[at..at + name.len()].copy_from_slice(name.as_bytes());
        at += name.len();
    }
    Ok(len)
}

/// Decode the answer to [`PinboardRequest::SoundRead`].
///
/// # Errors
///
/// * The session's refusal.
/// * [`Errno::BufferTooSmall`] / [`Errno::LengthOutOfRange`] — a frame
///   shorter or longer than its fields say, or a document past
///   [`POLICY_DOCUMENT_MAX`].
/// * [`Errno::BadMagic`] — a dirty reserved pair, or a name beside the
///   theme's sound.
/// * [`Errno::OutOfRange`] — an unknown entry kind, or text that is not
///   UTF-8, is empty where a name is, or holds a control character.
pub fn decode_sound_reading(bytes: &[u8]) -> Result<SoundReading<'_>, Errno> {
    let body = take_payload(bytes, bytes.len().max(STATUS_REPLY_LEN))?;
    if body.len() < READING_DOCUMENT_OFFSET - STATUS_REPLY_LEN {
        return Err(Errno::BufferTooSmall);
    }
    if read_u16(body, 2) != 0 {
        return Err(Errno::BadMagic);
    }
    let text = usize::from(read_u16(body, 0));
    if text > POLICY_DOCUMENT_MAX {
        return Err(Errno::LengthOutOfRange);
    }
    let mut rest = &body[4..];
    let (document, after) = rest.split_at_checked(text).ok_or(Errno::BufferTooSmall)?;
    let document = core::str::from_utf8(document).map_err(|_| Errno::OutOfRange)?;
    if document.chars().any(|c| is_forbidden_character(c, true)) {
        return Err(Errno::OutOfRange);
    }
    rest = after;
    let mut files = [SoundFile::Theme; SoundEvent::ALL.len()];
    for file in &mut files {
        let (&[kind, len], after) = rest.split_first_chunk().ok_or(Errno::BufferTooSmall)?;
        let (name, after) = after
            .split_at_checked(usize::from(len))
            .ok_or(Errno::BufferTooSmall)?;
        rest = after;
        let name = core::str::from_utf8(name).map_err(|_| Errno::OutOfRange)?;
        *file = match kind {
            FILE_THEME if name.is_empty() => SoundFile::Theme,
            FILE_THEME => return Err(Errno::BadMagic),
            FILE_CHOSEN | FILE_UNAVAILABLE if !is_shown_name(name) => {
                return Err(Errno::OutOfRange);
            }
            FILE_CHOSEN => SoundFile::Chosen(name),
            FILE_UNAVAILABLE => SoundFile::Unavailable(name),
            _ => return Err(Errno::OutOfRange),
        };
    }
    if !rest.is_empty() {
        return Err(Errno::LengthOutOfRange);
    }
    Ok(SoundReading { document, files })
}

/// The most theme names one page of [`PinboardRequest::SoundThemes`] carries;
/// a page with fewer is the last.
pub const SOUND_THEMES_PAGE: u16 = 8;

/// One theme name's record: its length, then the name padded with zeros.
const THEME_RECORD_LEN: usize = 1 + FS_NAME_MAX;

/// The longest page of themes.
pub const SOUND_THEMES_REPLY_MAX: usize =
    STATUS_REPLY_LEN + PAGE_HEADER_LEN + SOUND_THEMES_PAGE as usize * THEME_RECORD_LEN;

/// Encode a page of theme `names`, at most [`SOUND_THEMES_PAGE`], into `out`.
///
/// # Errors
///
/// [`Errno::LengthOutOfRange`] for more names than a page holds or a name a
/// page cannot carry, and [`Errno::BufferTooSmall`] for a short `out`.
pub fn encode_sound_themes(names: &[&str], out: &mut [u8]) -> Result<usize, Errno> {
    let mut records = [[0u8; THEME_RECORD_LEN]; SOUND_THEMES_PAGE as usize];
    let records = records
        .get_mut(..names.len())
        .ok_or(Errno::LengthOutOfRange)?;
    for (record, name) in records.iter_mut().zip(names) {
        if !is_shown_name(name) {
            return Err(Errno::LengthOutOfRange);
        }
        record[0] = u8::try_from(name.len()).map_err(|_| Errno::LengthOutOfRange)?;
        record[1..=name.len()].copy_from_slice(name.as_bytes());
    }
    encode_page_reply(records, SOUND_THEMES_PAGE, out)
}

/// Decode a page of theme names, each validated before any is handed out.
///
/// # Errors
///
/// What [`decode_page_reply`] refuses, [`Errno::LengthOutOfRange`] for a
/// frame longer than its records, [`Errno::BadMagic`] for a dirty record
/// tail, and [`Errno::OutOfRange`] for a name that is empty, too long, not
/// UTF-8 or holds a control character.
pub fn decode_sound_themes(bytes: &[u8]) -> Result<ThemePage<'_>, Errno> {
    let (count, records) = decode_page_reply(bytes, THEME_RECORD_LEN, SOUND_THEMES_PAGE)?;
    if bytes.len() != STATUS_REPLY_LEN + PAGE_HEADER_LEN + records.len() {
        return Err(Errno::LengthOutOfRange);
    }
    for record in records.as_chunks::<THEME_RECORD_LEN>().0 {
        theme_name(record)?;
    }
    Ok(ThemePage {
        count: usize::from(count),
        records,
    })
}

/// The name a validated theme record holds.
fn theme_name(record: &[u8]) -> Result<&str, Errno> {
    let len = usize::from(record[0]);
    let (name, tail) = record[1..].split_at_checked(len).ok_or(Errno::OutOfRange)?;
    if tail.iter().any(|&byte| byte != 0) {
        return Err(Errno::BadMagic);
    }
    core::str::from_utf8(name)
        .ok()
        .filter(|name| is_shown_name(name))
        .ok_or(Errno::OutOfRange)
}

/// A decoded page of theme names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ThemePage<'a> {
    count: usize,
    records: &'a [u8],
}

impl<'a> ThemePage<'a> {
    /// How many names the page holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Whether the page holds none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Whether this is the last page: one with fewer names than a page holds.
    #[must_use]
    pub const fn is_last(&self) -> bool {
        self.count < SOUND_THEMES_PAGE as usize
    }

    /// The names, in the order the session listed them.
    pub fn names(&self) -> impl Iterator<Item = &'a str> + 'a {
        self.records
            .as_chunks::<THEME_RECORD_LEN>()
            .0
            .iter()
            .filter_map(|record| theme_name(record).ok())
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::vec::Vec;

    use super::{
        decode_sound_reading, decode_sound_themes, encode_sound_reading, encode_sound_themes,
        PinboardDocument, PinboardRequest, SoundFile, SoundReading, PINBOARD_DOCUMENT_MAX,
        PINBOARD_MAX_REPLY, PINBOARD_MAX_REQUEST, SOUND_READING_MAX, SOUND_THEMES_PAGE,
    };
    use crate::sound_cue::{SoundEvent, POLICY_DOCUMENT_MAX};
    use crate::{Errno, FS_NAME_MAX};

    fn sample_document() -> &'static str {
        "wallpaper /System/Graphics/Wallpapers/TAIRiX/tairix-dark.jpg\n\
         fit fill\n\
         backdrop theme\n\
         icons leading\n\
         sort name\n"
    }

    fn apply() -> PinboardRequest {
        PinboardRequest::Apply {
            document: PinboardDocument::new(sample_document()).expect("a valid document"),
        }
    }

    fn frame(request: &PinboardRequest) -> Vec<u8> {
        let mut out = [0u8; PINBOARD_MAX_REQUEST];
        let len = request.encode(&mut out).expect("encodes");
        out[..len].to_vec()
    }

    fn requests() -> [PinboardRequest; 6] {
        let sound =
            PinboardDocument::new("sound.bell.enabled = false\n").expect("a valid document");
        [
            apply(),
            PinboardRequest::SoundRead,
            PinboardRequest::SoundThemes { from: 8 },
            PinboardRequest::SoundApply { document: sound },
            PinboardRequest::SoundPick {
                event: SoundEvent::Error,
            },
            PinboardRequest::SoundUseTheme {
                event: SoundEvent::Bell,
            },
        ]
    }

    #[test]
    fn every_request_round_trips_through_exactly_its_own_frame() {
        for request in requests() {
            let frame = frame(&request);
            assert_eq!(
                PinboardRequest::from_bytes(&frame),
                Ok(request),
                "{request:?}"
            );
            assert_eq!(
                PinboardRequest::from_bytes(&frame[..frame.len() - 1]),
                Err(Errno::BufferTooSmall),
                "{request:?}"
            );
            let mut long = frame.clone();
            long.push(0);
            assert_eq!(
                PinboardRequest::from_bytes(&long),
                Err(Errno::LengthOutOfRange),
                "{request:?}"
            );
        }
    }

    #[test]
    fn a_document_with_embedded_newlines_survives_intact() {
        let text = "wallpaper none\nfit centre\nbackdrop #112233\nicons trailing\nsort date\n";
        let document = PinboardDocument::new(text).expect("newlines are permitted");
        assert_eq!(document.as_str(), text);
        let request = PinboardRequest::Apply { document };
        assert_eq!(PinboardRequest::from_bytes(&frame(&request)), Ok(request));
    }

    #[test]
    fn full_width_document_round_trips() {
        let text = "a".repeat(PINBOARD_DOCUMENT_MAX);
        let document = PinboardDocument::new(&text).expect("max-length document");
        let request = PinboardRequest::Apply { document };
        let frame = frame(&request);
        assert_eq!(frame.len(), PINBOARD_MAX_REQUEST);
        assert_eq!(PinboardRequest::from_bytes(&frame), Ok(request));
    }

    #[test]
    fn document_bounds_are_enforced() {
        assert_eq!(PinboardDocument::new(""), Err(Errno::LengthOutOfRange));
        assert_eq!(
            PinboardDocument::new(&"x".repeat(PINBOARD_DOCUMENT_MAX + 1)),
            Err(Errno::LengthOutOfRange)
        );
        // A newline is permitted, but every other control character is
        // refused, never sanitised.
        assert!(PinboardDocument::new("wallpaper none\nfit fill\n").is_ok());
        assert_eq!(PinboardDocument::new("fit\tfill"), Err(Errno::OutOfRange));
        assert_eq!(
            PinboardDocument::new("fit fill\r\n"),
            Err(Errno::OutOfRange)
        );
        assert_eq!(PinboardDocument::new("bell\u{7}"), Err(Errno::OutOfRange));
    }

    #[test]
    fn rejects_bad_magic_version_and_op() {
        let altered = |at: usize, value: u8| {
            let mut frame = frame(&apply());
            frame[at] = value;
            PinboardRequest::from_bytes(&frame)
        };
        assert_eq!(altered(0, 0), Err(Errno::BadMagic));
        assert_eq!(altered(4, 0xFF), Err(Errno::AbiVersionUnsupported));
        // Operation 9 is outside the closed set.
        assert_eq!(altered(6, 9), Err(Errno::OutOfRange));
        assert_eq!(
            PinboardRequest::from_bytes(&[0u8; 7]),
            Err(Errno::BufferTooSmall)
        );
    }

    #[test]
    fn rejects_dirty_reserved_bytes() {
        let mut document = frame(&apply());
        document[super::RESERVED_OFFSET] = 1;
        assert_eq!(PinboardRequest::from_bytes(&document), Err(Errno::BadMagic));
        for request in [
            PinboardRequest::SoundThemes { from: 0 },
            PinboardRequest::SoundPick {
                event: SoundEvent::Info,
            },
        ] {
            let field = if matches!(request, PinboardRequest::SoundThemes { .. }) {
                2
            } else {
                1
            };
            for at in super::HEADER_LEN + field..super::FIELD_FRAME_LEN {
                let mut frame = frame(&request);
                frame[at] = 1;
                assert_eq!(
                    PinboardRequest::from_bytes(&frame),
                    Err(Errno::BadMagic),
                    "{request:?} {at}"
                );
            }
        }
    }

    #[test]
    fn an_event_outside_the_set_is_refused() {
        let mut frame = frame(&PinboardRequest::SoundPick {
            event: SoundEvent::Error,
        });
        frame[super::HEADER_LEN] = 0;
        assert_eq!(PinboardRequest::from_bytes(&frame), Err(Errno::OutOfRange));
        frame[super::HEADER_LEN] = 14;
        assert_eq!(PinboardRequest::from_bytes(&frame), Err(Errno::OutOfRange));
    }

    #[test]
    fn rejects_out_of_range_document_lengths() {
        let mut frame = frame(&apply());
        super::put_u16(
            &mut frame,
            super::DOCUMENT_LEN_OFFSET,
            u16::try_from(PINBOARD_DOCUMENT_MAX + 1).expect("the bound plus one fits a u16"),
        );
        assert_eq!(
            PinboardRequest::from_bytes(&frame),
            Err(Errno::LengthOutOfRange)
        );
        let empty = [&frame[..super::DOCUMENT_LEN_OFFSET], &[0, 0, 0, 0][..]].concat();
        assert_eq!(
            PinboardRequest::from_bytes(&empty),
            Err(Errno::LengthOutOfRange)
        );
    }

    fn reading() -> SoundReading<'static> {
        let mut files = [SoundFile::Theme; SoundEvent::ALL.len()];
        files[SoundEvent::Error.index()] = SoundFile::Chosen("Klaxon.flac");
        files[SoundEvent::Bell.index()] = SoundFile::Unavailable("bell é.wav");
        SoundReading {
            document: "sound.enabled = true\nsound.theme = TAIRiX\n",
            files,
        }
    }

    fn encoded(result: Result<&SoundReading<'_>, Errno>) -> Vec<u8> {
        let mut out = [0u8; SOUND_READING_MAX];
        let len = encode_sound_reading(result, &mut out).expect("encodes");
        out[..len].to_vec()
    }

    #[test]
    fn a_sound_reading_round_trips_and_a_refusal_is_its_status() {
        let reading = reading();
        assert_eq!(decode_sound_reading(&encoded(Ok(&reading))), Ok(reading));
        assert_eq!(
            decode_sound_reading(&encoded(Err(Errno::PermissionDenied))),
            Err(Errno::PermissionDenied)
        );
        let (document, name) = ("a".repeat(POLICY_DOCUMENT_MAX), "n".repeat(FS_NAME_MAX));
        let longest = SoundReading {
            document: &document,
            files: [SoundFile::Chosen(&name); SoundEvent::ALL.len()],
        };
        let frame = encoded(Ok(&longest));
        assert_eq!(frame.len(), SOUND_READING_MAX);
        const { assert!(SOUND_READING_MAX <= PINBOARD_MAX_REPLY) };
        assert_eq!(decode_sound_reading(&frame), Ok(longest));
    }

    #[test]
    fn a_reading_no_decode_would_take_is_never_encoded() {
        let mut out = [0u8; SOUND_READING_MAX];
        let mut bad = reading();
        bad.files[0] = SoundFile::Chosen("");
        assert_eq!(
            encode_sound_reading(Ok(&bad), &mut out),
            Err(Errno::LengthOutOfRange)
        );
        let mut bad = reading();
        bad.document = "a\tb";
        assert_eq!(
            encode_sound_reading(Ok(&bad), &mut out),
            Err(Errno::OutOfRange)
        );
        let mut short = [0u8; 8];
        assert_eq!(
            encode_sound_reading(Ok(&reading()), &mut short),
            Err(Errno::LengthOutOfRange)
        );
    }

    #[test]
    fn a_malformed_reading_is_refused() {
        let good = encoded(Ok(&reading()));
        assert_eq!(
            decode_sound_reading(&good[..good.len() - 1]),
            Err(Errno::BufferTooSmall)
        );
        let mut long = good.clone();
        long.push(0);
        assert_eq!(decode_sound_reading(&long), Err(Errno::LengthOutOfRange));
        let altered = |at: usize, value: u8| {
            let mut frame = good.clone();
            frame[at] = value;
            decode_sound_reading(&frame).map(|_| ())
        };
        assert_eq!(altered(6, 1), Err(Errno::BadMagic), "the reserved pair");
        let first_entry = 8 + reading().document.len();
        assert_eq!(
            altered(first_entry, 9),
            Err(Errno::OutOfRange),
            "an entry kind"
        );
        // A name beside the theme's sound is a frame no session writes.
        let mut named_theme = good[..first_entry].to_vec();
        named_theme.extend_from_slice(&[0, 1, b'x']);
        named_theme.extend_from_slice(&good[first_entry + 2..]);
        assert_eq!(decode_sound_reading(&named_theme), Err(Errno::BadMagic));
    }

    #[test]
    fn theme_pages_round_trip_and_say_which_is_last() {
        let names = ["TAIRiX", "Glass", "Héloïse"];
        let mut out = [0u8; super::SOUND_THEMES_REPLY_MAX];
        let len = encode_sound_themes(&names, &mut out).expect("encodes");
        let page = decode_sound_themes(&out[..len]).expect("decodes");
        assert_eq!(page.len(), 3);
        assert!(page.is_last());
        assert_eq!(page.names().collect::<Vec<_>>(), names);

        let full = ["a"; SOUND_THEMES_PAGE as usize];
        let len = encode_sound_themes(&full, &mut out).expect("encodes");
        assert_eq!(len, super::SOUND_THEMES_REPLY_MAX);
        assert!(!decode_sound_themes(&out[..len]).expect("decodes").is_last());

        let over = ["a"; SOUND_THEMES_PAGE as usize + 1];
        assert_eq!(
            encode_sound_themes(&over, &mut out),
            Err(Errno::LengthOutOfRange)
        );
        assert_eq!(
            encode_sound_themes(&[""], &mut out),
            Err(Errno::LengthOutOfRange)
        );
        let len = encode_sound_themes(&[], &mut out).expect("encodes");
        let empty = decode_sound_themes(&out[..len]).expect("decodes");
        assert!(empty.is_empty() && empty.is_last());
    }

    #[test]
    fn a_malformed_theme_page_is_refused() {
        let mut out = [0u8; super::SOUND_THEMES_REPLY_MAX];
        let len = encode_sound_themes(&["TAIRiX"], &mut out).expect("encodes");
        let good = out[..len].to_vec();
        let mut long = good.clone();
        long.push(0);
        assert_eq!(decode_sound_themes(&long), Err(Errno::LengthOutOfRange));
        let record = 8;
        let altered = |at: usize, value: u8| {
            let mut frame = good.clone();
            frame[at] = value;
            decode_sound_themes(&frame).map(|page| page.len())
        };
        assert_eq!(
            altered(record, 0),
            Err(Errno::BadMagic),
            "the name left in the tail"
        );
        let mut empty = good.clone();
        empty[record..record + 1 + "TAIRiX".len()].fill(0);
        assert_eq!(
            decode_sound_themes(&empty).map(|page| page.len()),
            Err(Errno::OutOfRange),
            "an empty name"
        );
        assert_eq!(altered(record + 8, 1), Err(Errno::BadMagic), "a dirty tail");
        assert_eq!(
            altered(record + 1, 0x07),
            Err(Errno::OutOfRange),
            "a control character"
        );
    }
}
