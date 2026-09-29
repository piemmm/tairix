//! The file layout the users and groups databases share: a header line, then
//! one record per line, with blank lines and `#` comments between records.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

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

impl Table {
    /// How `line`, a line after the header, reads.
    pub(crate) fn line<'a>(&self, line: &'a str) -> RecordLine<'a> {
        if line.len() > self.max_line_len {
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

    /// Refuse `records` that are too many, or two of which collide.
    pub(crate) fn check<R: Keyed>(&self, records: &[R]) -> Result<(), ParseError> {
        if records.len() > self.max_records {
            return Err(self.too_many);
        }
        self.first_collision(records)
            .map_or(Ok(()), |(_, refused)| Err(refused))
    }

    /// Read `text`, decoding each record with `decode`: the records, or the
    /// first defect at the line that raised it — the header, a record, or the
    /// later of two colliding records. An over-long text is refused whole.
    pub(crate) fn parse<R: Keyed>(
        &self,
        text: &str,
        decode: impl Fn(&str) -> Result<R, ParseError>,
    ) -> Result<Vec<R>, LocatedError> {
        if text.len() > self.max_len {
            return Err(LocatedError::whole(ParseError::TooLong));
        }
        let mut lines = text.lines();
        if lines.next() != Some(self.header) {
            return Err(LocatedError::at(1, ParseError::Header));
        }
        let mut records = Vec::new();
        let mut record_lines = Vec::new();
        for (index, line) in lines.enumerate() {
            let number = index + 2;
            let kind = match self.line(line) {
                RecordLine::TooLong => ParseError::LineTooLong,
                RecordLine::Blank | RecordLine::Comment { .. } => continue,
                RecordLine::Record { text, .. } if records.len() < self.max_records => {
                    match decode(text) {
                        Ok(record) => {
                            records.push(record);
                            record_lines.push(number);
                            continue;
                        }
                        Err(kind) => kind,
                    }
                }
                RecordLine::Record { .. } => self.too_many,
            };
            // A collision among the records above this line is the earlier
            // defect, and the one to name.
            return Err(self
                .collision(&records, &record_lines)
                .unwrap_or(LocatedError::at(number, kind)));
        }
        self.collision(&records, &record_lines)
            .map_or(Ok(records), Err)
    }

    /// The first collision among `records`, at the line of the later record.
    fn collision<R: Keyed>(&self, records: &[R], lines: &[usize]) -> Option<LocatedError> {
        let (at, kind) = self.first_collision(records)?;
        Some(lines.get(at).map_or(LocatedError::whole(kind), |&line| {
            LocatedError::at(line, kind)
        }))
    }

    /// The first record that repeats a name or an id an earlier one holds, by
    /// index, and which it repeats. Repeating both, the earlier of the two
    /// records it meets decides, and a name wins over an id held by the same
    /// record.
    fn first_collision<R: Keyed>(&self, records: &[R]) -> Option<(usize, ParseError)> {
        let mut names: BTreeMap<&str, usize> = BTreeMap::new();
        let mut ids: BTreeMap<R::Id, usize> = BTreeMap::new();
        for (index, record) in records.iter().enumerate() {
            let name = names.get(record.key_name()).copied();
            let id = ids.get(&record.key_id()).copied();
            match (name, id) {
                (Some(by_name), Some(by_id)) if by_id < by_name => {
                    return Some((index, self.duplicate_id))
                }
                (Some(_), _) => return Some((index, self.duplicate_name)),
                (None, Some(_)) => return Some((index, self.duplicate_id)),
                (None, None) => {}
            }
            names.insert(record.key_name(), index);
            ids.insert(record.key_id(), index);
        }
        None
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
