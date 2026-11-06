//! EBML's variable-size integers and element values (RFC 8794), read and
//! written.

/// Most bytes an element ID takes in a `WebM` file.
pub(super) const MAX_ID_LEN: usize = 4;

/// Most bytes an element size takes.
pub(super) const MAX_SIZE_LEN: usize = 8;

/// Most bytes an element header takes: its ID, then its size.
pub(super) const MAX_HEAD_LEN: usize = MAX_ID_LEN + MAX_SIZE_LEN;

/// An eight-byte size field whose value bits are all ones: a size not stated.
pub(super) const UNKNOWN_SIZE: [u8; MAX_SIZE_LEN] =
    [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

/// Why bytes do not hold a field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Field {
    /// The bytes end inside it.
    Short,
    /// It is not one EBML allows.
    Bad,
}

/// An element's size, as its header states it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Size {
    Known(u64),
    /// The value bits are all ones: the element runs until an element that
    /// cannot be inside it.
    Unknown,
}

/// A field of up to eight bytes, as written.
#[derive(Clone, Copy, Debug)]
pub(super) struct Written {
    bytes: [u8; 8],
    len: usize,
}

impl Written {
    pub(super) fn as_slice(&self) -> &[u8] {
        &self.bytes[8 - self.len..]
    }
}

/// How many bytes a variable-size integer opening with `first` takes: one more
/// than its leading zero bits, `None` for a zero byte.
fn width(first: u8) -> Option<usize> {
    (0..8)
        .find(|&bit| first & (0x80 >> bit) != 0)
        .map(|bit| bit + 1)
}

/// The big-endian value of up to eight bytes.
fn big_endian(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0, |value, &byte| (value << 8) | u64::from(byte))
}

/// The element ID opening `bytes`, and its width.
///
/// Refuses one wider than `max_len`, one whose value bits are all zeros or all
/// ones, and one a shorter field could hold: EBML admits none of them as an ID.
pub(super) fn read_id(bytes: &[u8], max_len: usize) -> Result<(u32, usize), Field> {
    let first = *bytes.first().ok_or(Field::Short)?;
    let len = width(first)
        .filter(|&len| len <= max_len.min(MAX_ID_LEN))
        .ok_or(Field::Bad)?;
    let field = bytes.get(..len).ok_or(Field::Short)?;
    let id = u32::try_from(big_endian(field)).map_err(|_| Field::Bad)?;
    let ones = (1u32 << (7 * len)) - 1;
    let value = id & ones;
    let shorter = len > 1 && value < (1u32 << (7 * (len - 1))) - 1;
    if value == 0 || value == ones || shorter {
        return Err(Field::Bad);
    }
    Ok((id, len))
}

/// The value bits of the variable-size integer opening `bytes`, `None` where
/// they are all ones, and its width.
pub(super) fn read_vint(bytes: &[u8], max_len: usize) -> Result<(Option<u64>, usize), Field> {
    let first = *bytes.first().ok_or(Field::Short)?;
    let len = width(first)
        .filter(|&len| len <= max_len.min(MAX_SIZE_LEN))
        .ok_or(Field::Bad)?;
    let field = bytes.get(..len).ok_or(Field::Short)?;
    let ones = (1u64 << (7 * len)) - 1;
    let value = big_endian(field) & ones;
    Ok(((value != ones).then_some(value), len))
}

/// The element size opening `bytes`, and its width.
pub(super) fn read_size(bytes: &[u8], max_len: usize) -> Result<(Size, usize), Field> {
    let (value, len) = read_vint(bytes, max_len)?;
    Ok((value.map_or(Size::Unknown, Size::Known), len))
}

/// The signed variable-size integer opening `bytes`, as EBML lacing states a
/// frame's size against the one before: its value bits less half their range.
pub(super) fn read_signed_vint(bytes: &[u8]) -> Result<(i64, usize), Field> {
    let (value, len) = read_vint(bytes, MAX_SIZE_LEN)?;
    let value = i64::try_from(value.ok_or(Field::Bad)?).map_err(|_| Field::Bad)?;
    Ok((value - ((1i64 << (7 * len - 1)) - 1), len))
}

/// How many bytes `id` takes, its marker bit included.
pub(super) const fn id_len(id: u32) -> usize {
    if id > 0x00FF_FFFF {
        4
    } else if id > 0xFFFF {
        3
    } else if id > 0xFF {
        2
    } else {
        1
    }
}

/// `id` as written.
pub(super) fn id_field(id: u32) -> Written {
    Written {
        bytes: u64::from(id).to_be_bytes(),
        len: id_len(id),
    }
}

/// The fewest bytes a size field holding `value` takes, `None` past what
/// eight bytes state.
pub(super) fn size_len(value: u64) -> Option<usize> {
    (1..=MAX_SIZE_LEN).find(|&len| value < (1 << (7 * len)) - 1)
}

/// `value` in a size field `len` bytes wide, `None` where it does not fit.
pub(super) fn size_field(value: u64, len: usize) -> Option<Written> {
    if !(1..=MAX_SIZE_LEN).contains(&len) {
        return None;
    }
    let marker = 1u64 << (7 * len);
    (value < marker - 1).then_some(Written {
        bytes: (marker | value).to_be_bytes(),
        len,
    })
}

/// The fewest bytes an unsigned integer element holds `value` in, one at least.
pub(super) fn uint_len(value: u64) -> usize {
    (1..MAX_SIZE_LEN)
        .find(|&len| value >> (8 * len) == 0)
        .unwrap_or(MAX_SIZE_LEN)
}

/// `value` as an unsigned integer element's data, in the fewest bytes.
pub(super) fn uint_field(value: u64) -> Written {
    Written {
        bytes: value.to_be_bytes(),
        len: uint_len(value),
    }
}

/// An unsigned integer element's value.
pub(super) fn read_uint(data: &[u8]) -> Option<u64> {
    (data.len() <= 8).then(|| big_endian(data))
}

/// A signed integer element's value.
pub(super) fn read_int(data: &[u8]) -> Option<i64> {
    let start = 8usize.checked_sub(data.len())?;
    let negative = data.first().is_some_and(|&byte| byte & 0x80 != 0);
    let mut bytes = [if negative { 0xFF } else { 0 }; 8];
    bytes.get_mut(start..)?.copy_from_slice(data);
    Some(i64::from_be_bytes(bytes))
}

/// A float element's value: zero, four or eight bytes.
pub(super) fn read_float(data: &[u8]) -> Option<f64> {
    match data.len() {
        0 => Some(0.0),
        4 => <[u8; 4]>::try_from(data)
            .ok()
            .map(|bytes| f64::from(f32::from_be_bytes(bytes))),
        8 => <[u8; 8]>::try_from(data).ok().map(f64::from_be_bytes),
        _ => None,
    }
}

/// A string element's text: printable ASCII, then nothing but zero padding.
pub(super) fn read_ascii(data: &[u8]) -> Option<&str> {
    let len = data
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(data.len());
    let (text, padding) = data.split_at(len);
    let printable = text.iter().all(|byte| (0x20..=0x7E).contains(byte));
    if !printable || padding.iter().any(|&byte| byte != 0) {
        return None;
    }
    core::str::from_utf8(text).ok()
}
