//! `gpio-header-v1`: the GPIO lines a board lends to system services through
//! its expansion header (`plans/COOLING.md` CL2).
//!
//! Which of a controller's lines reach the header is the board's fact, stated
//! by discovery on the controller's node ([`HeaderLines`], as the
//! [`HwProperty::GpioHeaderLines`](crate::HwProperty::GpioHeaderLines)
//! value). The controller's driver binds its node's [`GPIO_HEADER_ENDPOINTS`]
//! id as a restricted-sender endpoint requiring
//! [`CapabilityId::GPIO`](crate::CapabilityId::GPIO), then publishes a
//! [`GPIO_HEADER_NODE_COMPATIBLE`] child node carrying it, so a service finds
//! the header in the hardware tree once it serves.
//!
//! A holder lists the lent lines ([`HeaderRequest::Lines`]), claims a free one
//! as an output ([`HeaderRequest::Claim`]), switches it
//! ([`HeaderRequest::Drive`]) and gives it back ([`HeaderRequest::Release`]).
//! Every line a holder held returns to an input when it ends.

use crate::hwtree::NodeEndpointBlock;
use crate::le::{put_u16, put_u32, read_u16, read_u32};
use crate::reply::{decode_status_reply, encode_status_reply, STATUS_REPLY_LEN};
use crate::Errno;

/// The endpoints GPIO controllers serve their header's lines on, indexed by
/// the controller's node id.
pub const GPIO_HEADER_ENDPOINTS: NodeEndpointBlock = NodeEndpointBlock::tagged(*b"GH");

/// `compatible` of the child node a controller's driver publishes once its
/// header's lines are served.
pub const GPIO_HEADER_NODE_COMPATIBLE: &[u8] = b"tairix,gpio-header";

/// Lines one controller can lend: the width of the [`HeaderLines`] mask.
pub const GPIO_HEADER_LINES_MAX: usize = 64;

/// The lines a controller lends to its board's header, a bit per line.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct HeaderLines(u64);

impl HeaderLines {
    /// The lines whose bits `mask` sets.
    #[must_use]
    pub const fn from_mask(mask: u64) -> Self {
        Self(mask)
    }

    /// Lines `first..=last`, or no lines for a range past
    /// [`GPIO_HEADER_LINES_MAX`] or reversed.
    #[must_use]
    pub fn span(first: u8, last: u8) -> Self {
        if first > last || usize::from(last) >= GPIO_HEADER_LINES_MAX {
            return Self(0);
        }
        let above_last = u64::MAX >> (GPIO_HEADER_LINES_MAX - 1 - usize::from(last));
        Self(above_last & (u64::MAX << first))
    }

    /// The mask, as the property carries it.
    #[must_use]
    pub const fn mask(self) -> u64 {
        self.0
    }

    /// Whether `line` is lent.
    #[must_use]
    pub fn contains(self, line: u8) -> bool {
        usize::from(line) < GPIO_HEADER_LINES_MAX && self.0 & (1 << line) != 0
    }

    /// How many lines are lent.
    #[must_use]
    pub fn len(self) -> usize {
        self.iter().count()
    }

    /// Whether no line is lent.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The lent lines, lowest first.
    pub fn iter(self) -> impl Iterator<Item = u8> {
        let mut mask = self.0;
        core::iter::from_fn(move || {
            if mask == 0 {
                return None;
            }
            let line = mask.trailing_zeros();
            mask &= mask - 1;
            u8::try_from(line).ok()
        })
    }
}

/// Which level of a line means its load is on.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Polarity {
    /// High is on.
    #[default]
    ActiveHigh = 0,
    /// Low is on.
    ActiveLow = 1,
}

impl Polarity {
    pub(crate) const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::ActiveHigh),
            1 => Some(Self::ActiveLow),
            _ => None,
        }
    }

    /// The level that asserts (`true`) or deasserts (`false`) the load.
    #[must_use]
    pub const fn level(self, asserted: bool) -> bool {
        match self {
            Self::ActiveHigh => asserted,
            Self::ActiveLow => !asserted,
        }
    }
}

/// Who holds a line driven as an output.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Holder {
    /// The caller asking.
    Caller,
    /// Another holder.
    Other,
}

/// How one lent line stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum LineState {
    /// Nothing holds it and no peripheral is routed to it: it may be claimed.
    Free,
    /// Routed to a peripheral, its alternate function `function` (0–5), by
    /// whatever set the board up; it is never lent.
    Peripheral {
        /// The alternate function selected.
        function: u8,
    },
    /// Held as an interrupt by a device the hardware tree links to it.
    Interrupt,
    /// Held as an output.
    Output {
        /// Who holds it.
        holder: Holder,
        /// Whether its load is on.
        asserted: bool,
    },
}

impl LineState {
    /// Whether a claim may take the line.
    #[must_use]
    pub const fn is_free(self) -> bool {
        matches!(self, Self::Free)
    }
}

const STATE_FREE: u8 = 0;
const STATE_PERIPHERAL: u8 = 1;
const STATE_INTERRUPT: u8 = 2;
const STATE_OUTPUT: u8 = 3;
const OUTPUT_ASSERTED: u8 = 1 << 0;
const OUTPUT_CALLER: u8 = 1 << 1;
/// The alternate functions a line's function select can name.
const PERIPHERAL_FUNCTIONS: u8 = 6;

/// One lent line and how it stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct LineEntry {
    /// The controller's line number.
    pub line: u8,
    /// How it stands.
    pub state: LineState,
}

/// Bytes one [`LineEntry`] takes on the wire: the line, the state, its
/// detail and a reserved byte.
const ENTRY_LEN: usize = 4;

impl LineEntry {
    fn encode(self, out: &mut [u8; ENTRY_LEN]) {
        let (state, detail) = match self.state {
            LineState::Free => (STATE_FREE, 0),
            LineState::Peripheral { function } => (STATE_PERIPHERAL, function),
            LineState::Interrupt => (STATE_INTERRUPT, 0),
            LineState::Output { holder, asserted } => {
                let mut flags = 0;
                if asserted {
                    flags |= OUTPUT_ASSERTED;
                }
                if holder == Holder::Caller {
                    flags |= OUTPUT_CALLER;
                }
                (STATE_OUTPUT, flags)
            }
        };
        *out = [self.line, state, detail, 0];
    }

    fn decode([line, state, detail, reserved]: [u8; ENTRY_LEN]) -> Result<Self, Errno> {
        if reserved != 0 || usize::from(line) >= GPIO_HEADER_LINES_MAX {
            return Err(Errno::BadMagic);
        }
        let state = match (state, detail) {
            (STATE_FREE, 0) => LineState::Free,
            (STATE_PERIPHERAL, function) if function < PERIPHERAL_FUNCTIONS => {
                LineState::Peripheral { function }
            }
            (STATE_INTERRUPT, 0) => LineState::Interrupt,
            (STATE_OUTPUT, flags) if flags & !(OUTPUT_ASSERTED | OUTPUT_CALLER) == 0 => {
                LineState::Output {
                    holder: if flags & OUTPUT_CALLER == 0 {
                        Holder::Other
                    } else {
                        Holder::Caller
                    },
                    asserted: flags & OUTPUT_ASSERTED != 0,
                }
            }
            _ => return Err(Errno::BadMagic),
        };
        Ok(Self { line, state })
    }
}

/// Every lent line of a header and how each stands, lowest line first.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LineTable {
    entries: [LineEntry; GPIO_HEADER_LINES_MAX],
    len: usize,
}

impl Default for LineTable {
    fn default() -> Self {
        Self {
            entries: [LineEntry {
                line: 0,
                state: LineState::Free,
            }; GPIO_HEADER_LINES_MAX],
            len: 0,
        }
    }
}

impl LineTable {
    /// Append `entry`, which must name a line above every line already held.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a line out of order, or one past
    /// [`GPIO_HEADER_LINES_MAX`].
    pub fn push(&mut self, entry: LineEntry) -> Result<(), Errno> {
        let ordered = self
            .entries()
            .last()
            .is_none_or(|last| last.line < entry.line);
        if !ordered || usize::from(entry.line) >= GPIO_HEADER_LINES_MAX {
            return Err(Errno::OutOfRange);
        }
        self.entries[self.len] = entry;
        self.len += 1;
        Ok(())
    }

    /// The lines, lowest first.
    #[must_use]
    pub fn entries(&self) -> &[LineEntry] {
        &self.entries[..self.len]
    }

    /// The longest a table is on the wire: its count, then every line.
    pub const WIRE_MAX: usize = TABLE_HEADER_LEN + GPIO_HEADER_LINES_MAX * ENTRY_LEN;

    /// The table's length on the wire.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        TABLE_HEADER_LEN + self.len * ENTRY_LEN
    }

    /// Write the table into `out`: a count, three reserved bytes, then each
    /// entry. Answers the bytes written.
    pub(crate) fn write(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let len = self.wire_len();
        let frame = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        frame.fill(0);
        frame[0] = u8::try_from(self.len).map_err(|_| Errno::OutOfRange)?;
        let (slots, _) = frame[TABLE_HEADER_LEN..].as_chunks_mut::<ENTRY_LEN>();
        for (entry, slot) in self.entries().iter().zip(slots) {
            entry.encode(slot);
        }
        Ok(len)
    }

    /// Read a table [`Self::write`] wrote, exactly `bytes` long.
    pub(crate) fn read(bytes: &[u8]) -> Result<Self, Errno> {
        let header = bytes.get(..TABLE_HEADER_LEN).ok_or(Errno::BadMagic)?;
        let count = usize::from(header[0]);
        if header[1..] != [0; 3]
            || count > GPIO_HEADER_LINES_MAX
            || bytes.len() != TABLE_HEADER_LEN + count * ENTRY_LEN
        {
            return Err(Errno::BadMagic);
        }
        let mut table = Self::default();
        let (entries, _) = bytes[TABLE_HEADER_LEN..].as_chunks::<ENTRY_LEN>();
        for raw in entries {
            table
                .push(LineEntry::decode(*raw)?)
                .map_err(|_| Errno::BadMagic)?;
        }
        Ok(table)
    }

    /// How `line` stands, or [`None`] for a line the header does not lend.
    #[must_use]
    pub fn state(&self, line: u8) -> Option<LineState> {
        self.entries()
            .binary_search_by_key(&line, |entry| entry.line)
            .ok()
            .map(|index| self.entries[index].state)
    }
}

/// Magic opening every request frame (`"GHRQ"`).
pub const HEADER_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"GHRQ");

/// The `gpio-header-v1` protocol version.
pub const HEADER_VERSION_V1: u16 = 1;

/// Every request frame's length: magic, version, operation, a reserved byte,
/// then the line, its polarity, its state and a reserved byte.
pub const HEADER_REQUEST_LEN: usize = 12;

/// The longest reply: a [`HeaderRequest::Lines`] table of every line.
pub const HEADER_MAX_REPLY: usize = STATUS_REPLY_LEN + LineTable::WIRE_MAX;

/// The count and three reserved bytes ahead of a table's entries.
const TABLE_HEADER_LEN: usize = 4;

/// A request to a header's endpoint.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HeaderRequest {
    /// Answer every lent line and how it stands.
    Lines,
    /// Take `line` as an output driven with `polarity`, its load `asserted`
    /// from the start.
    Claim {
        /// The line.
        line: u8,
        /// Which level is on.
        polarity: Polarity,
        /// Whether the load starts on.
        asserted: bool,
    },
    /// Switch the caller's `line` on (`asserted`) or off.
    Drive {
        /// The line.
        line: u8,
        /// Whether its load is on.
        asserted: bool,
    },
    /// Give `line` back, an input again.
    Release {
        /// The line.
        line: u8,
    },
}

impl HeaderRequest {
    const fn op(self) -> u8 {
        match self {
            Self::Lines => 1,
            Self::Claim { .. } => 2,
            Self::Drive { .. } => 3,
            Self::Release { .. } => 4,
        }
    }

    /// Encode the request into `out`, answering its length.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `out` cannot hold it.
    pub fn encode(self, out: &mut [u8]) -> Result<usize, Errno> {
        let frame = out
            .get_mut(..HEADER_REQUEST_LEN)
            .ok_or(Errno::BufferTooSmall)?;
        frame.fill(0);
        put_u32(frame, 0, HEADER_REQUEST_MAGIC);
        put_u16(frame, 4, HEADER_VERSION_V1);
        frame[6] = self.op();
        let body = match self {
            Self::Lines => [0; 4],
            Self::Claim {
                line,
                polarity,
                asserted,
            } => [line, polarity as u8, u8::from(asserted), 0],
            Self::Drive { line, asserted } => [line, 0, u8::from(asserted), 0],
            Self::Release { line } => [line, 0, 0, 0],
        };
        frame[8..].copy_from_slice(&body);
        Ok(HEADER_REQUEST_LEN)
    }

    /// Decode a request frame.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] — not exactly a request's length.
    /// * [`Errno::BadMagic`] — a wrong magic, a dirty reserved byte or a field
    ///   the operation does not carry.
    /// * [`Errno::AbiVersionUnsupported`] — not [`HEADER_VERSION_V1`].
    /// * [`Errno::OutOfRange`] — an unknown operation, or a line past
    ///   [`GPIO_HEADER_LINES_MAX`].
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() != HEADER_REQUEST_LEN {
            return Err(Errno::LengthOutOfRange);
        }
        if read_u32(bytes, 0) != HEADER_REQUEST_MAGIC || bytes[7] != 0 || bytes[11] != 0 {
            return Err(Errno::BadMagic);
        }
        if read_u16(bytes, 4) != HEADER_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        let [line, polarity, state] = [bytes[8], bytes[9], bytes[10]];
        let line_in_range = || {
            if usize::from(line) < GPIO_HEADER_LINES_MAX {
                Ok(line)
            } else {
                Err(Errno::OutOfRange)
            }
        };
        let switch = || match state {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Errno::BadMagic),
        };
        match bytes[6] {
            1 if [line, polarity, state] == [0; 3] => Ok(Self::Lines),
            2 => Ok(Self::Claim {
                line: line_in_range()?,
                polarity: Polarity::from_u8(polarity).ok_or(Errno::BadMagic)?,
                asserted: switch()?,
            }),
            3 if polarity == 0 => Ok(Self::Drive {
                line: line_in_range()?,
                asserted: switch()?,
            }),
            4 if [polarity, state] == [0; 2] => Ok(Self::Release {
                line: line_in_range()?,
            }),
            1..=4 => Err(Errno::BadMagic),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// Frame the answer to a [`HeaderRequest::Lines`]: the table, or why there
/// is none.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold the reply.
pub fn encode_lines(out: &mut [u8], outcome: Result<&LineTable, Errno>) -> Result<usize, Errno> {
    let empty = LineTable::default();
    let table = outcome.unwrap_or(&empty);
    let frame = out
        .get_mut(..STATUS_REPLY_LEN + table.wire_len())
        .ok_or(Errno::BufferTooSmall)?;
    frame[..STATUS_REPLY_LEN].copy_from_slice(&encode_status_reply(outcome.map(|_| ())));
    Ok(STATUS_REPLY_LEN + table.write(&mut frame[STATUS_REPLY_LEN..])?)
}

/// Decode the answer to a [`HeaderRequest::Lines`].
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame whose
/// length disagrees with its count, a dirty reserved byte, an entry out of
/// order, or an undefined state.
pub fn decode_lines(bytes: &[u8]) -> Result<LineTable, Errno> {
    if bytes.len() < STATUS_REPLY_LEN {
        return Err(Errno::BadMagic);
    }
    decode_status_reply(bytes)?;
    LineTable::read(&bytes[STATUS_REPLY_LEN..])
}

/// Frame the answer to a claim, a drive or a release.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold the status word.
pub fn encode_status(out: &mut [u8], outcome: Result<(), Errno>) -> Result<usize, Errno> {
    let frame = out
        .get_mut(..STATUS_REPLY_LEN)
        .ok_or(Errno::BufferTooSmall)?;
    frame.copy_from_slice(&encode_status_reply(outcome));
    Ok(STATUS_REPLY_LEN)
}

/// Decode the answer to a claim, a drive or a release.
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame of the
/// wrong length.
pub fn decode_status(bytes: &[u8]) -> Result<(), Errno> {
    if bytes.len() != STATUS_REPLY_LEN {
        return Err(Errno::BadMagic);
    }
    decode_status_reply(bytes)
}

#[cfg(test)]
#[path = "gpio_tests.rs"]
mod tests;
