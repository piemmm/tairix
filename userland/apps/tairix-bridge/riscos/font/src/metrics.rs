//! A font's metrics file, `IntMetrics` or `IntMetric<n>`, read as the PRM
//! lays it out: each character's bounding box and offsets in thousandths of
//! an em, the font's own box and its other measures, and its kern pairs.
//! The file is the guest's, so every length and offset is checked and a
//! file that does not hold together is refused whole; what is read of it is
//! zeroed when let go.

use alloc::vec::Vec;
use core::mem::size_of;

/// The bytes before the character map: the name, two words, then the count
/// and version bytes.
const HEADER: usize = 52;
const NAME: usize = 40;
const COUNT_LOW: usize = 48;
const VERSION: usize = 49;
const FLAGS: usize = 50;
const COUNT_HIGH: usize = 51;
/// The characters a map covers when its size is not given.
const MAP_DEFAULT: usize = 256;
/// The file format versions: 0 has no flags, 2 has them; 1 is refused.
const PLAIN: u8 = 0;
const FLAGGED: u8 = 2;

/// The flags byte's bits.
pub(crate) mod flag {
    pub(crate) const NO_BOXES: u8 = 1;
    pub(crate) const NO_X_OFFSETS: u8 = 1 << 1;
    pub(crate) const NO_Y_OFFSETS: u8 = 1 << 2;
    pub(super) const MORE: u8 = 1 << 3;
    pub(super) const RESERVED: u8 = 1 << 4 | 1 << 7;
    pub(super) const MAP_SIZED: u8 = 1 << 5;
    pub(super) const WIDE_KERNS: u8 = 1 << 6;
}

/// A box in thousandths of an em: `x0, y0` inclusive, `x1, y1` exclusive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Box {
    pub(crate) x0: i32,
    pub(crate) y0: i32,
    pub(crate) x1: i32,
    pub(crate) y1: i32,
}

impl Box {
    /// Whether it holds nothing.
    pub(crate) const fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    /// The box holding both.
    pub(crate) fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }
}

/// The measures a metrics file's miscellaneous area gives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Misc {
    pub(crate) bbox: Box,
    pub(crate) x_offset: i32,
    pub(crate) y_offset: i32,
    pub(crate) italic: i32,
    /// In 256ths of an em.
    pub(crate) underline_position: i32,
    pub(crate) underline_thickness: i32,
    pub(crate) cap_height: i32,
    pub(crate) x_height: i32,
    pub(crate) descender: i32,
    pub(crate) ascender: i32,
}

/// One kern pair: the amounts the right-hand character moves by after the
/// left-hand one, in thousandths of an em.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Kern {
    pub(crate) left: u16,
    pub(crate) right: u16,
    pub(crate) x: i32,
    pub(crate) y: i32,
}

/// A metrics file, read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Metrics {
    /// The name the file gives, its padding of returns left off.
    pub(crate) name: Vec<u8>,
    /// Its flags byte, nought for a file of version 0.
    pub(crate) flags: u8,
    /// Each character's slot in the tables, 0 for a character not defined:
    /// none where the code is the slot.
    map: Option<Vec<u16>>,
    /// The tables, a slot each.
    boxes: Option<Vec<Box>>,
    x_offsets: Option<Vec<i32>>,
    y_offsets: Option<Vec<i32>>,
    pub(crate) misc: Option<Misc>,
    /// The kern pairs, ordered by left then right character.
    pub(crate) kerns: Vec<Kern>,
}

/// What a character measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Measure {
    /// Its box, where the file gives boxes.
    pub(crate) bbox: Option<Box>,
    /// Where it moves the pen to.
    pub(crate) x_offset: i32,
    pub(crate) y_offset: i32,
}

/// A metrics file that does not hold together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Malformed;

impl Metrics {
    /// The metrics file `bytes`.
    pub(crate) fn read(bytes: &[u8]) -> Result<Self, Malformed> {
        let header = bytes.get(..HEADER).ok_or(Malformed)?;
        let version = header[VERSION];
        let flags = match version {
            PLAIN => 0,
            FLAGGED => header[FLAGS],
            _ => return Err(Malformed),
        };
        if version == PLAIN && (header[FLAGS] != 0 || header[COUNT_HIGH] != 0) {
            return Err(Malformed);
        }
        if flags & flag::RESERVED != 0 {
            return Err(Malformed);
        }
        let slots = usize::from(header[COUNT_HIGH]) << 8 | usize::from(header[COUNT_LOW]);
        let name_end = header[..NAME]
            .iter()
            .position(|&b| b == b'\r' || b == 0)
            .unwrap_or(NAME);
        let mut at = HEADER;
        let map = if flags & flag::MAP_SIZED != 0 {
            let size = usize::from(u16_at(bytes, at)?);
            at += 2;
            match size {
                0 => None,
                _ if slots > MAP_DEFAULT => return Err(Malformed),
                _ => Some(map(bytes, &mut at, size)?),
            }
        } else if slots > MAP_DEFAULT {
            return Err(Malformed);
        } else {
            Some(map(bytes, &mut at, MAP_DEFAULT)?)
        };
        let mut table = |present: bool| -> Result<Option<Vec<i32>>, Malformed> {
            if !present {
                return Ok(None);
            }
            let values = (0..slots)
                .map(|slot| i16_at(bytes, at + 2 * slot).map(i32::from))
                .collect::<Result<Vec<i32>, Malformed>>()?;
            at += 2 * slots;
            Ok(Some(values))
        };
        let boxes = if flags & flag::NO_BOXES == 0 {
            let x0 = table(true)?.ok_or(Malformed)?;
            let y0 = table(true)?.ok_or(Malformed)?;
            let x1 = table(true)?.ok_or(Malformed)?;
            let y1 = table(true)?.ok_or(Malformed)?;
            Some(
                (0..slots)
                    .map(|i| Box {
                        x0: x0[i],
                        y0: y0[i],
                        x1: x1[i],
                        y1: y1[i],
                    })
                    .collect(),
            )
        } else {
            None
        };
        let x_offsets = table(flags & flag::NO_X_OFFSETS == 0)?;
        let y_offsets = table(flags & flag::NO_Y_OFFSETS == 0)?;
        let (misc, kerns) = if flags & flag::MORE != 0 {
            more(bytes, at, flags)?
        } else {
            (None, Vec::new())
        };
        Ok(Self {
            name: header[..name_end].to_vec(),
            flags,
            map,
            boxes,
            x_offsets,
            y_offsets,
            misc,
            kerns,
        })
    }

    /// The slot character `code` has; none for a character not defined.
    fn slot(&self, code: u32) -> Option<usize> {
        let slots = self.slots();
        match &self.map {
            Some(map) => {
                let slot = usize::from(*map.get(usize::try_from(code).ok()?)?);
                (slot != 0 && slot < slots).then_some(slot)
            }
            None => usize::try_from(code).ok().filter(|&slot| slot < slots),
        }
    }

    /// How many slots the tables have.
    fn slots(&self) -> usize {
        self.boxes
            .as_ref()
            .map(Vec::len)
            .or_else(|| self.x_offsets.as_ref().map(Vec::len))
            .or_else(|| self.y_offsets.as_ref().map(Vec::len))
            .unwrap_or(0)
    }

    /// What character `code` measures; none for one not defined. An offset
    /// the file leaves out is its miscellaneous area's default, or nought.
    pub(crate) fn measure(&self, code: u32) -> Option<Measure> {
        let slot = self.slot(code)?;
        let default = self.misc.unwrap_or_default();
        Some(Measure {
            bbox: self.boxes.as_ref().map(|boxes| boxes[slot]),
            x_offset: self
                .x_offsets
                .as_ref()
                .map_or(default.x_offset, |offsets| offsets[slot]),
            y_offset: self
                .y_offsets
                .as_ref()
                .map_or(default.y_offset, |offsets| offsets[slot]),
        })
    }

    /// The kern after `left` when `right` follows it.
    pub(crate) fn kern(&self, left: u16, right: u16) -> Option<Kern> {
        self.kerns
            .binary_search_by_key(&(left, right), |kern| (kern.left, kern.right))
            .ok()
            .map(|at| self.kerns[at])
    }

    /// Whether the file gives each character's box.
    pub(crate) const fn has_boxes(&self) -> bool {
        self.boxes.is_some()
    }

    /// How many characters its map covers.
    pub(crate) fn map_len(&self) -> usize {
        self.map.as_ref().map_or_else(|| self.slots(), Vec::len)
    }

    /// Roughly what it holds in memory.
    pub(crate) fn bytes(&self) -> usize {
        let tables = self.x_offsets.as_ref().map_or(0, Vec::len)
            + self.y_offsets.as_ref().map_or(0, Vec::len);
        size_of::<Self>()
            + self.name.len()
            + self.map.as_ref().map_or(0, |m| m.len() * size_of::<u16>())
            + self
                .boxes
                .as_ref()
                .map_or(0, |b| b.len() * size_of::<Box>())
            + tables * size_of::<i32>()
            + self.kerns.len() * size_of::<Kern>()
    }
}

impl Drop for Metrics {
    fn drop(&mut self) {
        self.name.fill(0);
        for table in [&mut self.x_offsets, &mut self.y_offsets]
            .into_iter()
            .flatten()
        {
            table.fill(0);
        }
        if let Some(map) = &mut self.map {
            map.fill(0);
        }
        if let Some(boxes) = &mut self.boxes {
            boxes.fill(Box::default());
        }
        self.kerns.fill(Kern {
            left: 0,
            right: 0,
            x: 0,
            y: 0,
        });
        self.misc = None;
    }
}

/// A character map of `size` byte entries at `*at`.
fn map(bytes: &[u8], at: &mut usize, size: usize) -> Result<Vec<u16>, Malformed> {
    let entries = bytes.get(*at..*at + size).ok_or(Malformed)?;
    *at += size;
    Ok(entries.iter().map(|&slot| u16::from(slot)).collect())
}

/// The extra data areas at `at`: the miscellaneous area and the kern
/// pairs, each where its offset from the offsets' start says, the two
/// reserved areas past them.
fn more(bytes: &[u8], at: usize, flags: u8) -> Result<(Option<Misc>, Vec<Kern>), Malformed> {
    let misc_at = at + usize::from(u16_at(bytes, at)?);
    let kerns_at = at + usize::from(u16_at(bytes, at + 2)?);
    let reserved_at = at + usize::from(u16_at(bytes, at + 4)?);
    if misc_at > kerns_at || kerns_at > reserved_at || reserved_at > bytes.len() {
        return Err(Malformed);
    }
    let misc = if kerns_at > misc_at {
        let field = |offset: usize| i16_at(bytes, misc_at + offset).map(i32::from);
        Some(Misc {
            bbox: Box {
                x0: field(0)?,
                y0: field(2)?,
                x1: field(4)?,
                y1: field(6)?,
            },
            x_offset: field(8)?,
            y_offset: field(10)?,
            italic: field(12)?,
            underline_position: i32::from(byte_at(bytes, misc_at + 14)?.cast_signed()),
            underline_thickness: i32::from(byte_at(bytes, misc_at + 15)?),
            cap_height: field(16)?,
            x_height: field(18)?,
            descender: field(20)?,
            ascender: field(22)?,
        })
    } else {
        None
    };
    let kerns = kerns(&bytes[..reserved_at], kerns_at, flags)?;
    Ok((misc, kerns))
}

/// The kern pairs from `at`: each left-hand character, its right-hand ones
/// with their amounts to a nought, the whole to a nought.
fn kerns(bytes: &[u8], mut at: usize, flags: u8) -> Result<Vec<Kern>, Malformed> {
    let wide = flags & flag::WIDE_KERNS != 0;
    let code = |at: &mut usize| -> Result<u16, Malformed> {
        let value = if wide {
            u16_at(bytes, *at)?
        } else {
            u16::from(byte_at(bytes, *at)?)
        };
        *at += if wide { 2 } else { 1 };
        Ok(value)
    };
    let mut kerns = Vec::new();
    if at >= bytes.len() {
        return Ok(kerns);
    }
    loop {
        let left = code(&mut at)?;
        if left == 0 {
            break;
        }
        loop {
            let right = code(&mut at)?;
            if right == 0 {
                break;
            }
            let mut amount = |present: bool| -> Result<i32, Malformed> {
                if !present {
                    return Ok(0);
                }
                let value = i16_at(bytes, at)?;
                at += 2;
                Ok(i32::from(value))
            };
            let x = amount(flags & flag::NO_X_OFFSETS == 0)?;
            let y = amount(flags & flag::NO_Y_OFFSETS == 0)?;
            kerns.try_reserve(1).map_err(|_| Malformed)?;
            kerns.push(Kern { left, right, x, y });
        }
    }
    kerns.sort_by_key(|kern| (kern.left, kern.right));
    kerns.dedup_by_key(|kern| (kern.left, kern.right));
    Ok(kerns)
}

fn byte_at(bytes: &[u8], at: usize) -> Result<u8, Malformed> {
    bytes.get(at).copied().ok_or(Malformed)
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Malformed> {
    let pair = bytes.get(at..at + 2).ok_or(Malformed)?;
    Ok(u16::from_le_bytes([pair[0], pair[1]]))
}

fn i16_at(bytes: &[u8], at: usize) -> Result<i16, Malformed> {
    u16_at(bytes, at).map(u16::cast_signed)
}

#[cfg(test)]
mod tests;
