//! Extended attribute spaces (ECMA-167 4/14.10) and the Macintosh Finder
//! information UDF records in one (UDF 2.60 3.3.4.5.4.2).

use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_fsmeta::preset::mac::FinderInfo;
use tairix_fsprobe::udf::{Tag, TAG_EAHD, TAG_LEN};

use crate::fields::Fields;

/// Bytes of the Extended Attribute Header Descriptor a space opens with.
const HEADER_LEN: usize = 24;
/// An attribute location naming no attribute of its class.
const NOT_PRESENT: u32 = u32::MAX;
/// The attribute type of implementation use attributes.
const TYPE_IMPLEMENTATION: u32 = 2048;
/// Bytes of every attribute's own header: type, subtype and length.
const ATTRIBUTE_HEADER: usize = 12;
/// Bytes before an implementation use attribute's data: its header, the
/// data's length and the implementation identifier.
const DATA_AT: usize = 48;
/// The identifier UDF records Finder information under.
const FINDER_INFO: &[u8] = b"*UDF Mac FinderInfo";
/// Bytes of a directory's Finder information: the checksum, a reserved
/// word, the parent directory id, the Finder information and its extension.
const DIRECTORY_DATA: usize = 40;
/// Bytes of a file's, which adds its resource fork's two lengths.
const FILE_DATA: usize = 48;
/// Where a file's type and creator lie in the data.
const TYPE_AT: usize = 8;
const CREATOR_AT: usize = 12;
/// Where the Finder flags lie in the data, a file's and a directory's alike.
const FLAGS_AT: usize = 16;

/// Where the Finder information attribute lies in `space`, an attribute
/// space whose header was recorded at block `location`; `None` for a space
/// recording none.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for a space whose header or attribute
/// lengths do not hold.
pub(crate) fn find_finder(space: &[u8], location: u32) -> Result<Option<usize>, DriverError> {
    if space.is_empty() {
        return Ok(None);
    }
    header(space, location)?;
    let mut at = HEADER_LEN;
    while let Some(rest) = space.get(at..).filter(|rest| !rest.is_empty()) {
        // Writers pad a space to whole words with zeros.
        if rest.iter().all(|&byte| byte == 0) {
            break;
        }
        let len = usize::try_from(Fields(rest).u32(8)?).map_err(|_| DriverError::DeviceFault)?;
        let attribute = rest
            .get(..len)
            .filter(|_| len >= ATTRIBUTE_HEADER)
            .ok_or(DriverError::DeviceFault)?;
        if is_finder(attribute) {
            return Ok(Some(at));
        }
        at += len;
    }
    Ok(None)
}

/// The Finder information of the attribute at `at` in `space`, for a node
/// that `is_dir`.
pub(crate) fn finder_at(space: &[u8], at: usize, is_dir: bool) -> Result<FinderInfo, DriverError> {
    let data = Fields(space.get(at + DATA_AT..).ok_or(DriverError::DeviceFault)?);
    let code = |offset: usize| -> Result<[u8; 4], DriverError> {
        // Recorded as a little-endian word, as every UDF number is.
        Ok(if is_dir {
            [0; 4]
        } else {
            data.u32(offset)?.to_be_bytes()
        })
    };
    Ok(FinderInfo {
        file_type: code(TYPE_AT)?,
        creator: code(CREATOR_AT)?,
        flags: data.u16(FLAGS_AT)?,
    })
}

/// `space`, the attribute space of an entry, with the Finder information of
/// a node that `is_dir` set to `finder`: rewritten where the space records
/// it, else added after its last implementation use attribute — the space
/// opened with a header sealed as `tag` when the entry has none. `revision`
/// is the UDF revision a new attribute is identified with.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for a damaged space, which is left as it is.
pub(crate) fn with_finder(
    space: &[u8],
    tag: Tag,
    is_dir: bool,
    finder: FinderInfo,
    revision: u16,
) -> Result<Vec<u8>, DriverError> {
    let (mut out, at) = if let Some(at) = find_finder(space, tag.location)? {
        (joined(&[space])?, at)
    } else if space.is_empty() {
        let mut out = joined(&[&[0; HEADER_LEN], &new_finder(is_dir, revision)?])?;
        set_locations(&mut out, tag, word(HEADER_LEN)?, NOT_PRESENT)?;
        (out, HEADER_LEN)
    } else {
        let attribute = new_finder(is_dir, revision)?;
        let (existing, implementation, application) = header(space, tag.location)?;
        let insert = location(application, space.len())?.unwrap_or(space.len());
        let implementation = location(implementation, insert)?.unwrap_or(insert);
        let application = match application {
            NOT_PRESENT => NOT_PRESENT,
            offset => offset
                .checked_add(word(attribute.len())?)
                .ok_or(DriverError::DeviceFault)?,
        };
        let mut out = joined(&[&space[..insert], &attribute, &space[insert..]])?;
        set_locations(&mut out, existing, word(implementation)?, application)?;
        (out, insert)
    };
    let data = out
        .get_mut(at + DATA_AT..)
        .ok_or(DriverError::DeviceFault)?;
    data[FLAGS_AT..FLAGS_AT + 2].copy_from_slice(&finder.flags.to_le_bytes());
    if !is_dir {
        data[TYPE_AT..TYPE_AT + 4]
            .copy_from_slice(&u32::from_be_bytes(finder.file_type).to_le_bytes());
        data[CREATOR_AT..CREATOR_AT + 4]
            .copy_from_slice(&u32::from_be_bytes(finder.creator).to_le_bytes());
    }
    Ok(out)
}

/// `parts` end to end, reserved fallibly.
fn joined(parts: &[&[u8]]) -> Result<Vec<u8>, DriverError> {
    let mut out = Vec::new();
    if !tairix_util::fallible::reserve(&mut out, parts.iter().map(|part| part.len()).sum()) {
        return Err(DriverError::OutOfMemory);
    }
    for part in parts {
        out.extend_from_slice(part);
    }
    Ok(out)
}

/// A space's header, verified: its tag and its two attribute locations.
fn header(space: &[u8], location: u32) -> Result<(Tag, u32, u32), DriverError> {
    let tag = Tag::verify(space, location)
        .filter(|tag| tag.id == TAG_EAHD)
        .ok_or(DriverError::DeviceFault)?;
    let f = Fields(space);
    Ok((tag, f.u32(TAG_LEN)?, f.u32(TAG_LEN + 4)?))
}

/// Record the first implementation and application use attributes' places
/// in the header opening `space`, sealing it as `tag`.
fn set_locations(
    space: &mut [u8],
    tag: Tag,
    implementation: u32,
    application: u32,
) -> Result<(), DriverError> {
    space[TAG_LEN..TAG_LEN + 4].copy_from_slice(&implementation.to_le_bytes());
    space[TAG_LEN + 4..HEADER_LEN].copy_from_slice(&application.to_le_bytes());
    tag.seal(&mut space[..HEADER_LEN], HEADER_LEN - TAG_LEN)
}

/// The offset an attribute location records, `None` when it names none;
/// damage when it lies outside the header-to-`end` range.
fn location(recorded: u32, end: usize) -> Result<Option<usize>, DriverError> {
    if recorded == NOT_PRESENT {
        return Ok(None);
    }
    usize::try_from(recorded)
        .ok()
        .filter(|&at| (HEADER_LEN..=end).contains(&at))
        .map(Some)
        .ok_or(DriverError::DeviceFault)
}

fn word(len: usize) -> Result<u32, DriverError> {
    u32::try_from(len).map_err(|_| DriverError::DeviceFault)
}

/// The attribute is UDF's Finder information, intact.
fn is_finder(attribute: &[u8]) -> bool {
    let f = Fields(attribute);
    let identified = f.u32(0).is_ok_and(|kind| kind == TYPE_IMPLEMENTATION)
        && f.u8(4).is_ok_and(|subtype| subtype == 1)
        && f.bytes(17, 23).is_ok_and(|id| {
            id.starts_with(FINDER_INFO) && id[FINDER_INFO.len()..].iter().all(|&byte| byte == 0)
        });
    let whole = f
        .u32(12)
        .ok()
        .and_then(|len| usize::try_from(len).ok())
        .is_some_and(|len| len >= DIRECTORY_DATA && DATA_AT + len <= attribute.len());
    identified
        && whole
        && checksum(attribute)
            .is_some_and(|sum| f.u16(DATA_AT).is_ok_and(|recorded| recorded == sum))
}

/// The checksum UDF opens an implementation use attribute's data with: the
/// sum of the bytes before it.
fn checksum(attribute: &[u8]) -> Option<u16> {
    let header = attribute.get(..DATA_AT)?;
    Some(
        header
            .iter()
            .fold(0u16, |sum, &byte| sum.wrapping_add(u16::from(byte))),
    )
}

/// A Finder information attribute for a node that `is_dir`, recording none.
fn new_finder(is_dir: bool, revision: u16) -> Result<Vec<u8>, DriverError> {
    let data = if is_dir { DIRECTORY_DATA } else { FILE_DATA };
    let len = DATA_AT + data;
    let mut attribute = crate::zeroed_len(len)?;
    attribute[..4].copy_from_slice(&TYPE_IMPLEMENTATION.to_le_bytes());
    attribute[4] = 1;
    attribute[8..12].copy_from_slice(&word(len)?.to_le_bytes());
    attribute[12..16].copy_from_slice(&word(data)?.to_le_bytes());
    attribute[17..17 + FINDER_INFO.len()].copy_from_slice(FINDER_INFO);
    // The identifier's suffix names the UDF revision it was recorded under.
    attribute[40..42].copy_from_slice(&revision.to_le_bytes());
    let sum = checksum(&attribute).ok_or(DriverError::DeviceFault)?;
    attribute[DATA_AT..DATA_AT + 2].copy_from_slice(&sum.to_le_bytes());
    Ok(attribute)
}

#[cfg(test)]
mod tests {
    // Test attributes are a few dozen bytes, so every length fits a word.
    #![allow(clippy::cast_possible_truncation)]

    use super::*;

    const AT: u32 = 77;

    fn tag() -> Tag {
        Tag {
            id: TAG_EAHD,
            version: 3,
            serial: 0,
            location: AT,
        }
    }

    fn file_info() -> FinderInfo {
        FinderInfo {
            file_type: *b"TEXT",
            creator: *b"ttxt",
            flags: 0x0100,
        }
    }

    /// An attribute of `kind` holding `body`, padded to whole words.
    fn attribute(kind: u32, body: &[u8]) -> Vec<u8> {
        let len = (ATTRIBUTE_HEADER + body.len()).next_multiple_of(4);
        let mut out = alloc::vec![0u8; len];
        out[..4].copy_from_slice(&kind.to_le_bytes());
        out[4] = 1;
        out[8..12].copy_from_slice(&(len as u32).to_le_bytes());
        out[ATTRIBUTE_HEADER..ATTRIBUTE_HEADER + body.len()].copy_from_slice(body);
        out
    }

    #[test]
    fn finder_information_is_added_to_an_entry_with_no_space_and_read_back() {
        let space = with_finder(&[], tag(), false, file_info(), 0x0260).expect("built");
        assert_eq!(space.len(), HEADER_LEN + DATA_AT + FILE_DATA);
        let at = find_finder(&space, AT).expect("intact").expect("found");
        assert_eq!(finder_at(&space, at, false), Ok(file_info()));
        assert_eq!(
            &space[at + DATA_AT + TYPE_AT..][..4],
            b"TXET",
            "a little-endian word"
        );
        let (_, implementation, application) = header(&space, AT).expect("sealed");
        assert_eq!(
            (implementation, application),
            (HEADER_LEN as u32, NOT_PRESENT)
        );
        assert_eq!(
            find_finder(&space, AT + 1),
            Err(DriverError::DeviceFault),
            "the header names its own block"
        );
    }

    #[test]
    fn finder_information_is_rewritten_in_place_and_keeps_its_neighbours() {
        let first = with_finder(&[], tag(), false, file_info(), 0x0201).expect("built");
        let changed = FinderInfo {
            creator: *b"R*ch",
            ..file_info()
        };
        let again = with_finder(&first, tag(), false, changed, 0x0260).expect("rewritten");
        assert_eq!(again.len(), first.len());
        let at = find_finder(&again, AT).expect("intact").expect("found");
        assert_eq!(finder_at(&again, at, false), Ok(changed));
        assert_eq!(
            again[at + 40..at + 42],
            0x0201u16.to_le_bytes(),
            "the recorded revision stays"
        );
    }

    #[test]
    fn a_new_attribute_lands_between_the_implementation_and_application_attributes() {
        let ecma = attribute(5, &[1, 2, 3, 4]);
        let application = attribute(65_536, &[9; 40]);
        let mut space = alloc::vec![0u8; HEADER_LEN];
        let application_at = HEADER_LEN + ecma.len();
        set_locations(&mut space, tag(), NOT_PRESENT, application_at as u32).expect("sealed");
        space.extend_from_slice(&ecma);
        space.extend_from_slice(&application);
        let out = with_finder(
            &space,
            tag(),
            true,
            FinderInfo {
                flags: 0x4000,
                ..FinderInfo::default()
            },
            0x0260,
        )
        .expect("added");
        let at = find_finder(&out, AT).expect("intact").expect("found");
        assert_eq!(at, application_at);
        let (_, implementation, moved) = header(&out, AT).expect("sealed");
        assert_eq!(implementation as usize, application_at);
        assert_eq!(moved as usize, application_at + DATA_AT + DIRECTORY_DATA);
        assert_eq!(&out[moved as usize..], &application[..]);
        assert_eq!(&out[HEADER_LEN..application_at], &ecma[..]);
        assert_eq!(finder_at(&out, at, true).map(|f| f.flags), Ok(0x4000));
    }

    #[test]
    fn a_directory_records_flags_but_no_type_or_creator() {
        let space = with_finder(&[], tag(), true, file_info(), 0x0260).expect("built");
        let at = find_finder(&space, AT).expect("intact").expect("found");
        assert_eq!(space.len(), HEADER_LEN + DATA_AT + DIRECTORY_DATA);
        assert_eq!(
            finder_at(&space, at, true),
            Ok(FinderInfo {
                flags: 0x0100,
                ..FinderInfo::default()
            })
        );
    }

    #[test]
    fn damage_is_refused_rather_than_read_or_rewritten() {
        let good = with_finder(&[], tag(), false, file_info(), 0x0260).expect("built");
        let mut bad_sum = good.clone();
        bad_sum[HEADER_LEN + DATA_AT] ^= 1;
        assert_eq!(
            find_finder(&bad_sum, AT),
            Ok(None),
            "a checksum that fails names no Finder information"
        );
        let mut bad_len = good.clone();
        bad_len[HEADER_LEN + 8..HEADER_LEN + 12].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(find_finder(&bad_len, AT), Err(DriverError::DeviceFault));
        assert_eq!(
            with_finder(&bad_len, tag(), false, file_info(), 0x0260),
            Err(DriverError::DeviceFault)
        );
        let mut bad_header = good.clone();
        bad_header[TAG_LEN] ^= 1;
        assert_eq!(find_finder(&bad_header, AT), Err(DriverError::DeviceFault));
        let mut padded = good;
        padded.extend_from_slice(&[0; 8]);
        assert!(
            find_finder(&padded, AT).expect("intact").is_some(),
            "zero padding ends the space"
        );
    }
}
