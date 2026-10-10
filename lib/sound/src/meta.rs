//! What a file says about its sound, beside the sound itself.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_inline::ArrayString;

use crate::{DecodeError, DecodeLimits};

/// What a tag names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TagKind {
    /// The work's title.
    Title,
    /// Who performed or made it.
    Artist,
    /// The album, or the product it belongs to.
    Album,
    /// Free text: a WAV comment, an AU annotation.
    Comment,
    /// When it was made.
    Date,
    /// Its genre.
    Genre,
    /// Its copyright.
    Copyright,
    /// The software that wrote the file.
    Software,
    /// Its track number.
    Track,
    /// A tag of the file's own vocabulary with no reading here, by its key.
    Other(TagKey),
}

/// Most bytes a [`TagKey`] holds.
pub const TAG_KEY_MAX: usize = 32;

/// A tag's key in its file's own vocabulary: a RIFF `INFO` chunk id, a Vorbis
/// comment field name. Printable ASCII other than `=`, and upper case where the
/// vocabulary does not tell case apart.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TagKey(ArrayString<TAG_KEY_MAX>);

impl TagKey {
    /// The key `bytes` spell, or `None` for an empty one, one longer than
    /// [`TAG_KEY_MAX`], or one holding a byte no key may.
    #[must_use]
    pub fn new(bytes: &[u8]) -> Option<Self> {
        let printable = bytes
            .iter()
            .all(|&byte| (0x20..=0x7E).contains(&byte) && byte != b'=');
        if bytes.is_empty() || !printable {
            return None;
        }
        let text = core::str::from_utf8(bytes).ok()?;
        ArrayString::try_from(text).ok().map(Self)
    }

    /// The key `bytes` spell in upper case, for a vocabulary blind to case.
    #[must_use]
    pub fn uppercase(bytes: &[u8]) -> Option<Self> {
        let mut key = [0u8; TAG_KEY_MAX];
        let upper = key.get_mut(..bytes.len())?;
        for (to, from) in upper.iter_mut().zip(bytes) {
            *to = from.to_ascii_uppercase();
        }
        Self::new(upper)
    }

    /// The key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// One tag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tag {
    /// What it names.
    pub kind: TagKind,
    /// Its text.
    pub value: String,
}

/// A marked position in the stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Cue {
    /// The file's id for it.
    pub id: u32,
    /// The frame it marks.
    pub frame: u64,
}

/// How a sampler plays a loop.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LoopKind {
    /// Start to end, over again.
    Forward,
    /// Start to end and back.
    Alternating,
    /// End to start.
    Backward,
    /// A kind of the file's own vocabulary with no reading here.
    Other(u32),
}

/// A sampler loop.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Loop {
    /// Its first frame.
    pub start: u64,
    /// Its last frame.
    pub end: u64,
    /// How it plays.
    pub kind: LoopKind,
    /// Times it plays; zero for endlessly.
    pub count: u32,
}

/// Where a file holds its cover picture whole: the image's own bytes, which a
/// reader holding the file reads and hands to an image decoder.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CoverRange {
    /// Its first byte, from the file's start.
    pub offset: u64,
    /// Its length; never zero.
    pub len: u64,
}

/// What a file says about its sound, within the decode limits.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Metadata {
    /// Its tags, in the order the file holds them.
    pub tags: Vec<Tag>,
    /// Its marked positions.
    pub cues: Vec<Cue>,
    /// Its sampler loops.
    pub loops: Vec<Loop>,
    /// The note the recording sounds at unpitched, as a MIDI note.
    pub unity_note: Option<u8>,
    /// Its front cover, else the first picture it carries, where the file
    /// holds that picture whole. A picture laid across a container's pages is
    /// not one.
    pub cover: Option<CoverRange>,
    /// Something the file holds was left out for want of room within the
    /// limits.
    pub omitted: bool,
}

/// What keeping one tag costs beyond its text: its entry, which no target
/// lays out in more. A fixed figure rather than the target's own size, so a
/// file keeps the same tags on every machine.
const TAG_CHARGE: usize = 80;

const _: () = assert!(size_of::<Tag>() <= TAG_CHARGE);

/// The share of a metadata budget keeping a tag of `text` bytes takes.
const fn tag_cost(text: usize) -> usize {
    text.saturating_add(TAG_CHARGE)
}

impl Metadata {
    /// Whether this is metadata a decode under `limits` can keep: its tags
    /// within the budget, its cues and loops within the marker count, and a
    /// unity note that is a MIDI note.
    #[must_use]
    pub fn within(&self, limits: &DecodeLimits) -> bool {
        let budget = usize::try_from(limits.max_metadata_bytes()).unwrap_or(usize::MAX);
        let markers = usize::try_from(limits.max_markers()).unwrap_or(usize::MAX);
        let mut spent = 0usize;
        for tag in &self.tags {
            spent = spent.saturating_add(tag_cost(tag.value.len()));
        }
        spent <= budget
            && self.cues.len().saturating_add(self.loops.len()) <= markers
            && self.unity_note.is_none_or(|note| note <= MAX_NOTE)
    }
}

/// The highest MIDI note.
const MAX_NOTE: u8 = 127;

/// The metadata a decode is building, held to its limits.
pub(crate) struct Collector {
    metadata: Metadata,
    budget_left: usize,
    markers_left: usize,
    cover_is_front: bool,
}

impl Collector {
    pub(crate) fn new(limits: &DecodeLimits) -> Self {
        Self {
            metadata: Metadata::default(),
            budget_left: usize::try_from(limits.max_metadata_bytes()).unwrap_or(usize::MAX),
            markers_left: usize::try_from(limits.max_markers()).unwrap_or(usize::MAX),
            cover_is_front: false,
        }
    }

    /// Offer a picture held whole at `range`: a front cover replaces any
    /// other, and otherwise the first picture stands.
    pub(crate) fn picture(&mut self, range: CoverRange, front: bool) {
        if self.metadata.cover.is_none() || (front && !self.cover_is_front) {
            self.metadata.cover = Some(range);
            self.cover_is_front = front;
        }
    }

    /// Whether a tag of `bytes` of text would fit, noting the omission when
    /// not.
    pub(crate) fn has_room(&mut self, bytes: u64) -> bool {
        let fits = usize::try_from(bytes).is_ok_and(|bytes| tag_cost(bytes) <= self.budget_left);
        if !fits {
            self.metadata.omitted = true;
        }
        fits
    }

    /// Keep a tag holding `raw`, read as UTF-8 or else as Latin-1, its
    /// trailing NULs and spaces dropped; an empty one is no tag.
    pub(crate) fn tag(&mut self, kind: TagKind, raw: &[u8]) -> Result<(), DecodeError> {
        let end = raw
            .iter()
            .rposition(|&byte| byte != 0 && !byte.is_ascii_whitespace())
            .map_or(0, |last| last + 1);
        let raw = &raw[..end];
        if raw.is_empty() {
            return Ok(());
        }
        let text = core::str::from_utf8(raw);
        let len = match text {
            Ok(text) => text.len(),
            Err(_) => raw.len() + raw.iter().filter(|&&byte| byte >= 0x80).count(),
        };
        let cost = tag_cost(len);
        if cost > self.budget_left {
            self.metadata.omitted = true;
            return Ok(());
        }
        let mut value = String::new();
        value
            .try_reserve_exact(len)
            .map_err(|_| DecodeError::OutOfMemory)?;
        match text {
            Ok(text) => value.push_str(text),
            Err(_) => value.extend(raw.iter().map(|&byte| char::from(byte))),
        }
        if self.metadata.tags.try_reserve(1).is_err() {
            return Err(DecodeError::OutOfMemory);
        }
        self.budget_left -= cost;
        self.metadata.tags.push(Tag { kind, value });
        Ok(())
    }

    /// Keep a marked position, answering whether there was room for it.
    pub(crate) fn cue(&mut self, cue: Cue) -> Result<bool, DecodeError> {
        if !self.take_marker() {
            return Ok(false);
        }
        if self.metadata.cues.try_reserve(1).is_err() {
            return Err(DecodeError::OutOfMemory);
        }
        self.metadata.cues.push(cue);
        Ok(true)
    }

    /// Keep a sampler loop, answering whether there was room for it.
    pub(crate) fn sampler_loop(&mut self, sampler_loop: Loop) -> Result<bool, DecodeError> {
        if !self.take_marker() {
            return Ok(false);
        }
        if self.metadata.loops.try_reserve(1).is_err() {
            return Err(DecodeError::OutOfMemory);
        }
        self.metadata.loops.push(sampler_loop);
        Ok(true)
    }

    /// Note the recording's unity note, if it is a MIDI note.
    pub(crate) fn unity_note(&mut self, note: u32) {
        if let Some(note) = u8::try_from(note).ok().filter(|&note| note <= MAX_NOTE) {
            self.metadata.unity_note = Some(note);
        }
    }

    fn take_marker(&mut self) -> bool {
        if let Some(left) = self.markers_left.checked_sub(1) {
            self.markers_left = left;
            true
        } else {
            self.metadata.omitted = true;
            false
        }
    }

    pub(crate) fn finish(self) -> Metadata {
        self.metadata
    }
}
