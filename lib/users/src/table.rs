//! The file layout the users and groups databases share: a header line, then
//! one record per line, with blank lines and `#` comments between records.

use alloc::vec::Vec;

use tairix_util::fallible::{collected, filled};

use crate::{LocatedError, ParseError};

/// What one line after a database's header is, as its parser reads it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecordLine<'a> {
    /// Nothing but white space.
    Blank,
    /// A comment, starting at byte `at`.
    Comment {
        /// Where the `#` is.
        at: usize,
    },
    /// A record, starting at byte `at`, without its surrounding white space.
    Record {
        /// Where the record starts.
        at: usize,
        /// The record itself.
        text: &'a str,
    },
    /// Longer than the database admits a line to be: refused whole.
    TooLong,
}

/// A record a database keys twice, by a name and by a numeric id, neither of
/// which two of its records may share.
pub(crate) trait Keyed {
    /// The numeric id.
    type Id: Ord + Copy;

    /// The name no other record may have.
    fn key_name(&self) -> &str;

    /// The id no other record may have.
    fn key_id(&self) -> Self::Id;
}

/// One database's rules.
pub(crate) struct Table {
    /// The exact first line.
    pub header: &'static str,
    /// Most bytes the whole text may hold.
    pub max_len: usize,
    /// Most bytes one line may hold.
    pub max_line_len: usize,
    /// Most records it may hold.
    pub max_records: usize,
    /// The refusal of a record past `max_records`.
    pub too_many: ParseError,
    /// The refusal of a record repeating an earlier one's name.
    pub duplicate_name: ParseError,
    /// The refusal of a record repeating an earlier one's id.
    pub duplicate_id: ParseError,
}

/// How `line`, a line after the header of a database admitting lines of
/// `max_line_len` bytes, reads.
fn classify(line: &str, max_line_len: usize) -> RecordLine<'_> {
    if line.len() > max_line_len {
        return RecordLine::TooLong;
    }
    let text = line.trim();
    let at = line.len() - line.trim_start().len();
    if text.is_empty() {
        RecordLine::Blank
    } else if text.starts_with('#') {
        RecordLine::Comment { at }
    } else {
        RecordLine::Record { at, text }
    }
}

impl Table {
    /// How `line`, a line after the header, reads.
    pub(crate) fn line<'a>(&self, line: &'a str) -> RecordLine<'a> {
        classify(line, self.max_line_len)
    }

    /// The record lines of `text` in order, each with its line number, read
    /// in place; a text too long or headed wrongly is that defect alone, and
    /// a line too long ends the records with its defect.
    pub(crate) fn records<'t>(
        &self,
        text: &'t str,
    ) -> impl Iterator<Item = Result<(usize, &'t str), LocatedError>> + 't {
        let mut lines = text.lines();
        let mut refused = if text.len() > self.max_len {
            Some(LocatedError::whole(ParseError::TooLong))
        } else if lines.next() != Some(self.header) {
            Some(LocatedError::at(1, ParseError::Header))
        } else {
            None
        };
        let mut numbered = lines.enumerate();
        let max_line_len = self.max_line_len;
        let mut ended = false;
        core::iter::from_fn(move || {
            if ended {
                return None;
            }
            if let Some(refused) = refused.take() {
                ended = true;
                return Some(Err(refused));
            }
            loop {
                let (index, line) = numbered.next()?;
                let number = index + 2;
                match classify(line, max_line_len) {
                    RecordLine::Blank | RecordLine::Comment { .. } => {}
                    RecordLine::Record { text, .. } => return Some(Ok((number, text))),
                    RecordLine::TooLong => {
                        ended = true;
                        return Some(Err(LocatedError::at(number, ParseError::LineTooLong)));
                    }
                }
            }
        })
    }

    /// Refuse `records` that are too many, or two of which collide.
    pub(crate) fn check<R: Keyed>(&self, records: &[R]) -> Result<(), ParseError> {
        if records.len() > self.max_records {
            return Err(self.too_many);
        }
        self.first_collision(records)?
            .map_or(Ok(()), |(_, refused)| Err(refused))
    }

    /// Read `text`, decoding each record with `decode`: the records, or the
    /// first defect at the line that raised it — the header, a record, or the
    /// later of two colliding records. An over-long text is refused whole,
    /// and so is a text the heap cannot hold the records of.
    pub(crate) fn parse<R: Keyed>(
        &self,
        text: &str,
        decode: impl Fn(&str) -> Result<R, ParseError>,
    ) -> Result<Vec<R>, LocatedError> {
        let oom = |_| LocatedError::whole(ParseError::OutOfMemory);
        let mut records = Vec::new();
        let mut record_lines = Vec::new();
        for read in self.records(text) {
            let refused = match read {
                Ok((number, line)) if records.len() < self.max_records => match decode(line) {
                    Ok(record) => {
                        records.try_reserve(1).map_err(oom)?;
                        record_lines.try_reserve(1).map_err(oom)?;
                        records.push(record);
                        record_lines.push(number);
                        continue;
                    }
                    Err(ParseError::OutOfMemory) => {
                        return Err(LocatedError::whole(ParseError::OutOfMemory))
                    }
                    Err(kind) => LocatedError::at(number, kind),
                },
                Ok((number, _)) => LocatedError::at(number, self.too_many),
                Err(refused) => refused,
            };
            // A collision among the records above this line is the earlier
            // defect, and the one to name.
            return Err(self.collision(&records, &record_lines)?.unwrap_or(refused));
        }
        match self.collision(&records, &record_lines)? {
            Some(collision) => Err(collision),
            None => Ok(records),
        }
    }

    /// The first collision among `records`, at the line of the later record.
    fn collision<R: Keyed>(
        &self,
        records: &[R],
        lines: &[usize],
    ) -> Result<Option<LocatedError>, LocatedError> {
        let found = self.first_collision(records).map_err(LocatedError::whole)?;
        Ok(found.map(|(at, kind)| {
            lines.get(at).map_or(LocatedError::whole(kind), |&line| {
                LocatedError::at(line, kind)
            })
        }))
    }

    /// The first record that repeats a name or an id an earlier one holds, by
    /// index, and which it repeats. Repeating both, the earlier of the two
    /// records it meets decides, and a name wins over an id held by the same
    /// record.
    fn first_collision<R: Keyed>(
        &self,
        records: &[R],
    ) -> Result<Option<(usize, ParseError)>, ParseError> {
        let held = |list: Option<Vec<usize>>| list.ok_or(ParseError::OutOfMemory);
        let mut order = held(collected(records.len(), 0..records.len()))?;
        let mut by_name = held(filled(records.len(), usize::MAX))?;
        let mut by_id = held(filled(records.len(), usize::MAX))?;
        // Each record's earliest holder of its name, and of its id: the first
        // of its run once sorted, the index settling ties so the sort takes
        // no scratch from the heap.
        order.sort_unstable_by(|&a, &b| {
            records[a]
                .key_name()
                .cmp(records[b].key_name())
                .then(a.cmp(&b))
        });
        for run in order.chunk_by(|&a, &b| records[a].key_name() == records[b].key_name()) {
            for &later in &run[1..] {
                by_name[later] = run[0];
            }
        }
        order.sort_unstable_by(|&a, &b| {
            records[a]
                .key_id()
                .cmp(&records[b].key_id())
                .then(a.cmp(&b))
        });
        for run in order.chunk_by(|&a, &b| records[a].key_id() == records[b].key_id()) {
            for &later in &run[1..] {
                by_id[later] = run[0];
            }
        }
        let held_by = |earlier: usize| (earlier != usize::MAX).then_some(earlier);
        for index in 0..records.len() {
            match (held_by(by_name[index]), held_by(by_id[index])) {
                (Some(name), Some(id)) if id < name => return Ok(Some((index, self.duplicate_id))),
                (Some(_), _) => return Ok(Some((index, self.duplicate_name))),
                (None, Some(_)) => return Ok(Some((index, self.duplicate_id))),
                (None, None) => {}
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::RecordLine;
    use crate::{GroupsDb, UsersDb, MAX_LINE_LEN};
    use alloc::string::String;

    #[test]
    fn a_line_reads_as_the_parser_reads_it() {
        assert_eq!(UsersDb::line(""), RecordLine::Blank);
        assert_eq!(UsersDb::line("   \t"), RecordLine::Blank);
        assert_eq!(UsersDb::line("  # note"), RecordLine::Comment { at: 2 });
        assert_eq!(
            GroupsDb::line("  wheel:0  "),
            RecordLine::Record {
                at: 2,
                text: "wheel:0"
            }
        );
        let mut long = String::from("ada");
        while long.len() <= MAX_LINE_LEN {
            long.push(' ');
        }
        assert_eq!(
            UsersDb::line(&long),
            RecordLine::TooLong,
            "padding counts: the parser refuses the line whole"
        );
    }
}
