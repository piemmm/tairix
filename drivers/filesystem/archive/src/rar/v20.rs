//! RAR 2's compression: LZ77 over up to a mebibyte, coded through Huffman
//! tables a member sends as deltas of the last, and the multimedia mode,
//! which codes each sample as its error against an adaptive linear
//! prediction from its channel's history.

use tairix_abi::DriverError;

use super::bits::Reader;
use super::filter;
use super::huffman::Code;
use super::lz::{DIST, LEN_BASE, LEN_BITS, LEN_SLOTS, SHORT_BASE, SHORT_BITS};
use super::window::Window;
use super::Sink;

/// Main table symbols: 256 bytes, the last match, four repeated distances,
/// eight short distances, a new table, then 28 length slots.
const NC: usize = 298;
/// Distance symbols.
const DC: usize = 48;
/// Length symbols of a repeated distance.
const RC: usize = LEN_SLOTS;
/// An audio channel's symbols: 256 deltas and a new table.
const MC: usize = 257;
/// The most audio channels.
const CHANNELS: usize = 4;

/// One audio channel's predictor.
#[derive(Default)]
struct Channel {
    weight: [i32; 5],
    delta: [i32; 4],
    last_delta: i32,
    error: [u32; 11],
    count: u32,
    last: u8,
}

/// A RAR 2 decoder, carried from one member of a solid run to the next.
pub(super) struct V20 {
    lengths: [u8; CHANNELS * MC],
    main: Code,
    dist: Code,
    len: Code,
    audio_codes: [Code; CHANNELS],
    have_tables: bool,
    audio: bool,
    channels: usize,
    channel: usize,
    channel_delta: i32,
    predictors: [Channel; CHANNELS],
    old: [u64; 4],
    old_at: usize,
    last_dist: u64,
    last_len: u32,
    next: u64,
    file_end: u64,
    owed: u64,
}

impl V20 {
    pub(super) fn new() -> Self {
        Self {
            lengths: [0; CHANNELS * MC],
            main: Code::empty(),
            dist: Code::empty(),
            len: Code::empty(),
            audio_codes: core::array::from_fn(|_| Code::empty()),
            have_tables: false,
            audio: false,
            channels: 1,
            channel: 0,
            channel_delta: 0,
            predictors: Default::default(),
            old: [0; 4],
            old_at: 0,
            last_dist: 0,
            last_len: 0,
            next: 0,
            file_end: 0,
            owed: 0,
        }
    }

    /// Stand at a member of `len` bytes whose data is `packed` bytes,
    /// reading its tables where the run has none yet.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for tables out of shape.
    pub(super) fn begin(
        &mut self,
        br: &mut Reader<'_>,
        _win: &Window,
        len: u64,
        packed: u64,
    ) -> Result<(), DriverError> {
        self.owed = self.next;
        self.file_end = self.next.checked_add(len).ok_or(DriverError::MediumError)?;
        if !self.have_tables && packed > 0 {
            self.read_tables(br)?;
        }
        Ok(())
    }

    /// Give out the member's next bytes.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for data out of shape.
    pub(super) fn read(
        &mut self,
        br: &mut Reader<'_>,
        win: &mut Window,
        sink: &mut Sink<'_>,
    ) -> Result<(), DriverError> {
        while sink.wants() && self.owed < self.file_end {
            let upto = win.pos().min(self.file_end);
            if upto > self.owed {
                self.owed += sink.give_window(win, self.owed, upto - self.owed)?;
                continue;
            }
            let target = self.file_end.min(self.owed + win.ahead(sink.room()));
            while win.pos() < target {
                win.has_room(self.owed)?;
                self.step(br, win)?;
            }
        }
        Ok(())
    }

    /// End the member: the next starts after its last byte.
    pub(super) fn finish(&mut self) {
        self.next = self.file_end;
    }

    fn read_tables(&mut self, br: &mut Reader<'_>) -> Result<(), DriverError> {
        self.audio = br.bit()?;
        if !br.bit()? {
            self.lengths.fill(0);
        }
        let count = if self.audio {
            self.channels = usize::try_from(br.bits(2)?).unwrap_or(0) + 1;
            if self.channel >= self.channels {
                self.channel = 0;
            }
            self.channels * MC
        } else {
            NC + DC + RC
        };
        let mut pre = [0u8; 19];
        for len in &mut pre {
            *len = br.bits(4)?.to_le_bytes()[0];
        }
        let mut precode = Code::empty();
        precode.build(&pre)?;
        let lengths = &mut self.lengths[..count];
        let mut i = 0;
        while i < count {
            let symbol = precode.decode(br)?;
            match symbol {
                0..=15 => {
                    lengths[i] = (lengths[i] + symbol.to_le_bytes()[0]) & 15;
                    i += 1;
                }
                16 => {
                    let prev = *i
                        .checked_sub(1)
                        .and_then(|prev| lengths.get(prev))
                        .ok_or(DriverError::MediumError)?;
                    let run = (usize::try_from(br.bits(2)?).unwrap_or(0) + 3).min(count - i);
                    lengths[i..i + run].fill(prev);
                    i += run;
                }
                _ => {
                    let run = if symbol == 17 {
                        br.bits(3)? + 3
                    } else {
                        br.bits(7)? + 11
                    };
                    let run = usize::try_from(run).unwrap_or(0).min(count - i);
                    lengths[i..i + run].fill(0);
                    i += run;
                }
            }
        }
        if self.audio {
            for (code, lengths) in self.audio_codes.iter_mut().zip(lengths.chunks(MC)) {
                code.build(lengths)?;
            }
        } else {
            let (main, rest) = lengths.split_at(NC);
            let (dist, len) = rest.split_at(DC);
            self.main.build(main)?;
            self.dist.build(dist)?;
            self.len.build(len)?;
        }
        self.have_tables = true;
        Ok(())
    }

    fn copy(&mut self, win: &mut Window, dist: u64, len: u32) -> Result<(), DriverError> {
        self.old[self.old_at & 3] = dist;
        self.old_at = self.old_at.wrapping_add(1);
        self.last_dist = dist;
        self.last_len = len;
        win.copy(dist, len)
    }

    fn step(&mut self, br: &mut Reader<'_>, win: &mut Window) -> Result<(), DriverError> {
        if self.audio {
            let symbol = self.audio_codes[self.channel].decode(br)?;
            if symbol == 256 {
                return self.read_tables(br);
            }
            let byte = self.audio_byte(symbol.to_le_bytes()[0]);
            win.literal(byte);
            self.channel += 1;
            if self.channel == self.channels {
                self.channel = 0;
            }
            return Ok(());
        }
        let symbol = self.main.decode(br)?;
        match symbol {
            0..=255 => {
                win.literal(symbol.to_le_bytes()[0]);
                Ok(())
            }
            256 => self.copy(win, self.last_dist, self.last_len),
            257..=260 => {
                let dist = self.old[self.old_at.wrapping_sub(usize::from(symbol - 256)) & 3];
                let slot = usize::from(self.len.decode(br)?);
                let base = *LEN_BASE.get(slot).ok_or(DriverError::MediumError)?;
                let mut len = base + 2 + br.bits(LEN_BITS[slot])?;
                if dist >= 0x101 {
                    len += 1;
                    if dist >= 0x2000 {
                        len += 1;
                        if dist >= 0x4_0000 {
                            len += 1;
                        }
                    }
                }
                self.copy(win, dist, len)
            }
            261..=268 => {
                let slot = usize::from(symbol - 261);
                let dist = SHORT_BASE[slot] + 1 + u64::from(br.bits(SHORT_BITS[slot])?);
                self.copy(win, dist, 2)
            }
            269 => self.read_tables(br),
            _ => {
                let slot = usize::from(symbol - 270);
                let base = *LEN_BASE.get(slot).ok_or(DriverError::MediumError)?;
                let mut len = base + 3 + br.bits(LEN_BITS[slot])?;
                let dist_slot = usize::from(self.dist.decode(br)?);
                let &(dist_base, dist_bits) =
                    DIST[..DC].get(dist_slot).ok_or(DriverError::MediumError)?;
                let dist = dist_base + 1 + u64::from(br.bits(dist_bits)?);
                if dist >= 0x2000 {
                    len += 1;
                    if dist >= 0x4_0000 {
                        len += 1;
                    }
                }
                self.copy(win, dist, len)
            }
        }
    }

    /// The sample `coded` is the prediction error of, for the current
    /// channel.
    fn audio_byte(&mut self, coded: u8) -> u8 {
        let shared = self.channel_delta;
        let channel = &mut self.predictors[self.channel];
        channel.count = channel.count.wrapping_add(1);
        channel.delta[3] = channel.delta[2];
        channel.delta[2] = channel.delta[1];
        channel.delta[1] = channel.last_delta - channel.delta[0];
        channel.delta[0] = channel.last_delta;
        let w = &channel.weight;
        let d = &channel.delta;
        let sum = 8 * i32::from(channel.last)
            + w[0] * d[0]
            + w[1] * d[1]
            + w[2] * d[2]
            + w[3] * d[3]
            + w[4] * shared;
        let byte = (sum >> 3).to_le_bytes()[0].wrapping_sub(coded);
        let error = i32::from(i8::from_le_bytes([coded])) << 3;
        channel.error[0] += error.unsigned_abs();
        for (k, &delta) in d.iter().enumerate() {
            channel.error[1 + 2 * k] += (error - delta).unsigned_abs();
            channel.error[2 + 2 * k] += (error + delta).unsigned_abs();
        }
        channel.error[9] += (error - shared).unsigned_abs();
        channel.error[10] += (error + shared).unsigned_abs();
        channel.last_delta = i32::from(i8::from_le_bytes([byte.wrapping_sub(channel.last)]));
        self.channel_delta = channel.last_delta;
        channel.last = byte;
        if channel.count.is_multiple_of(32) {
            filter::adapt(&mut channel.error, &mut channel.weight);
        }
        byte
    }
}
