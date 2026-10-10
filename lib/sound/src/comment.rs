//! The Vorbis comment block FLAC carries (RFC 9639, section 8.6), as Vorbis
//! and Opus carry it too: a vendor string, then `NAME=value` fields, each
//! length little-endian.
//!
//! A field is read only as far as its name unless it is kept, so a block of
//! any size costs a read a field and the bytes the metadata budget keeps.
//! What follows the last field is the container's: Vorbis ends the block with
//! a framing bit, Opus with data of its own.

use alloc::vec::Vec;

use crate::input::{Fields, Region};
use crate::meta::{Collector, TagKey, TagKind};
use crate::DecodeError;

/// Bytes of a field read to find its name: the longest name with a reading
/// here, and the channel mask whole.
const NAME_PROBE: usize = 64;

/// The field naming the channels' speaker positions.
const CHANNEL_MASK: &[u8] = b"WAVEFORMATEXTENSIBLE_CHANNEL_MASK";

/// What a comment block states beside its tags.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Comments {
    /// The speaker positions its channels hold, as a WAVE speaker mask.
    pub(crate) channel_mask: Option<u32>,
}

/// What a field's upper-cased name names.
fn kind(name: &[u8]) -> Option<TagKind> {
    Some(match name {
        b"TITLE" => TagKind::Title,
        b"ARTIST" => TagKind::Artist,
        b"ALBUM" => TagKind::Album,
        b"COMMENT" | b"DESCRIPTION" => TagKind::Comment,
        b"DATE" => TagKind::Date,
        b"GENRE" => TagKind::Genre,
        b"COPYRIGHT" => TagKind::Copyright,
        b"ENCODER" => TagKind::Software,
        b"TRACKNUMBER" => TagKind::Track,
        _ => TagKind::Other(TagKey::new(name)?),
    })
}

/// A channel mask's value: `0x` and hexadecimal digits, case blind.
fn mask(value: &[u8]) -> Option<u32> {
    let digits = value.strip_prefix(b"0x").or(value.strip_prefix(b"0X"))?;
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0u32, |mask, &digit| {
        let nibble = char::from(digit).to_digit(16)?;
        mask.checked_mul(16)?.checked_add(nibble)
    })
}

/// Read the comment block `region` holds, keeping its tags in `collector`
/// and refusing a block that does not hold together with `malformed`.
pub(crate) fn read(
    region: &mut impl Region,
    collector: &mut Collector,
    malformed: DecodeError,
) -> Result<Comments, DecodeError> {
    let mut fields = Fields::new(region, malformed);
    let mut text = Vec::new();
    let vendor = u64::from(u32::from_le_bytes(fields.array()?));
    keep(
        &mut fields,
        TagKind::Software,
        &[],
        vendor,
        collector,
        &mut text,
    )?;
    let count = u64::from(u32::from_le_bytes(fields.array()?));
    if count > fields.remaining() / 4 {
        return Err(malformed);
    }
    let mut comments = Comments::default();
    for _ in 0..count {
        let len = u64::from(u32::from_le_bytes(fields.array()?));
        if len > fields.remaining() {
            return Err(malformed);
        }
        let mut probe = [0u8; NAME_PROBE];
        let probed = usize::try_from(len).map_or(NAME_PROBE, |len| len.min(NAME_PROBE));
        let probe = &mut probe[..probed];
        fields.bytes(probe)?;
        let rest = len - u64::try_from(probed).map_err(|_| malformed)?;
        let Some(equals) = probe.iter().position(|&byte| byte == b'=') else {
            if rest == 0 {
                return Err(malformed);
            }
            fields.skip(rest)?;
            continue;
        };
        let (name, value) = (&probe[..equals], &probe[equals + 1..]);
        let mut upper = [0u8; NAME_PROBE];
        let upper = &mut upper[..name.len()];
        for (to, &from) in upper.iter_mut().zip(name) {
            if !(0x20..=0x7E).contains(&from) {
                return Err(malformed);
            }
            *to = from.to_ascii_uppercase();
        }
        if upper == CHANNEL_MASK {
            let parsed = (rest == 0).then(|| mask(value)).flatten();
            if comments.channel_mask.is_some() || parsed.is_none() {
                return Err(malformed);
            }
            comments.channel_mask = parsed;
            continue;
        }
        match kind(upper) {
            Some(kind) => keep(&mut fields, kind, value, rest, collector, &mut text)?,
            None => fields.skip(rest)?,
        }
    }
    Ok(comments)
}

/// Keep a tag of `kind` whose value is `head` then `rest` more bytes at the
/// cursor, where the budget has room; move past it either way.
fn keep<R: Region>(
    fields: &mut Fields<'_, R>,
    kind: TagKind,
    head: &[u8],
    rest: u64,
    collector: &mut Collector,
    text: &mut Vec<u8>,
) -> Result<(), DecodeError> {
    let len = u64::try_from(head.len())
        .ok()
        .and_then(|head| head.checked_add(rest))
        .ok_or(fields.short())?;
    if !collector.has_room(len) {
        return fields.skip(rest);
    }
    let len = usize::try_from(len).map_err(|_| DecodeError::OutOfMemory)?;
    if !tairix_util::fallible::grow_to(text, len, 0u8) {
        return Err(DecodeError::OutOfMemory);
    }
    let value = &mut text[..len];
    value[..head.len()].copy_from_slice(head);
    fields.bytes(&mut value[head.len()..])?;
    collector.tag(kind, value)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::{read, Comments};
    use crate::input::Span;
    use crate::meta::{Collector, TagKey, TagKind};
    use crate::{DecodeError, DecodeLimits};

    const BAD: DecodeError = DecodeError::FlacBadComment;

    fn block(vendor: &[u8], fields: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(u32::try_from(vendor.len()).expect("small").to_le_bytes());
        out.extend(vendor);
        out.extend(u32::try_from(fields.len()).expect("small").to_le_bytes());
        for field in fields {
            out.extend(u32::try_from(field.len()).expect("small").to_le_bytes());
            out.extend(*field);
        }
        out
    }

    fn parse(bytes: &[u8], budget: u32) -> (Result<Comments, DecodeError>, Collector) {
        let limits = DecodeLimits::new(8, budget, 0);
        let mut collector = Collector::new(&limits);
        let mut input = bytes;
        let mut span = Span {
            input: &mut input,
            start: 0,
            len: bytes.len() as u64,
        };
        (read(&mut span, &mut collector, BAD), collector)
    }

    #[test]
    fn fields_keep_their_kind_whatever_the_case_of_their_names() {
        let bytes = block(
            b"encoder 1.0",
            &[
                b"title=A tune",
                b"Artist=Someone",
                b"replaygain_track_gain=-6.1 dB",
            ],
        );
        let (comments, collector) = parse(&bytes, 4096);
        assert_eq!(comments, Ok(Comments::default()));
        let tags: Vec<(TagKind, std::string::String)> = collector
            .finish()
            .tags
            .into_iter()
            .map(|tag| (tag.kind, tag.value))
            .collect();
        let gain = TagKey::new(b"REPLAYGAIN_TRACK_GAIN").expect("a key");
        assert_eq!(
            tags,
            [
                (TagKind::Software, "encoder 1.0".into()),
                (TagKind::Title, "A tune".into()),
                (TagKind::Artist, "Someone".into()),
                (TagKind::Other(gain), "-6.1 dB".into()),
            ]
        );
    }

    #[test]
    fn the_channel_mask_is_read_and_not_kept_as_a_tag() {
        let bytes = block(b"", &[b"WaveFormatExtensible_Channel_Mask=0x0000003F"]);
        let (comments, collector) = parse(&bytes, 4096);
        assert_eq!(comments.map(|c| c.channel_mask), Ok(Some(0x3F)));
        assert!(collector.finish().tags.is_empty());
        for bad in [
            &b"WAVEFORMATEXTENSIBLE_CHANNEL_MASK=3F"[..],
            b"WAVEFORMATEXTENSIBLE_CHANNEL_MASK=0x",
            b"WAVEFORMATEXTENSIBLE_CHANNEL_MASK=0x1FFFFFFFF",
        ] {
            assert_eq!(parse(&block(b"", &[bad]), 4096).0, Err(BAD), "{bad:?}");
        }
        let twice = block(
            b"",
            &[
                b"WAVEFORMATEXTENSIBLE_CHANNEL_MASK=0x3",
                b"WAVEFORMATEXTENSIBLE_CHANNEL_MASK=0x3",
            ],
        );
        assert_eq!(parse(&twice, 4096).0, Err(BAD));
    }

    /// A field past the budget is stepped over unread, and the tags after it
    /// that fit are still kept.
    #[test]
    fn a_field_past_the_budget_is_skipped_and_those_after_it_kept() {
        let long = [b"LYRICS=".as_slice(), &[b'x'; 5000]].concat();
        let bytes = block(b"", &[&long, b"TITLE=t"]);
        let (comments, collector) = parse(&bytes, 200);
        assert!(comments.is_ok());
        let metadata = collector.finish();
        assert!(metadata.omitted);
        assert_eq!(metadata.tags.len(), 1);
        assert_eq!(metadata.tags[0].kind, TagKind::Title);
    }

    #[test]
    fn a_block_that_does_not_hold_together_is_refused() {
        let mut overlong = block(b"", &[b"TITLE=t"]);
        overlong[8] = 0xFF;
        let mut count = block(b"", &[b"TITLE=t"]);
        count[4] = 9;
        for bytes in [
            overlong,
            count,
            block(b"", &[b"no equals sign"]),
            block(b"", &[b"BAD\x01NAME=x"]),
            block(&[0; 3], &[])[..5].to_vec(),
        ] {
            assert_eq!(parse(&bytes, 4096).0, Err(BAD), "{bytes:?}");
        }
    }
}
