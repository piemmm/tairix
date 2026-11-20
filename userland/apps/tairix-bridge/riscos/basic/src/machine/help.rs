//! `HELP`, which takes the rest of its line: a keyword's topic, every
//! keyword or those a letter begins in columns, `@%`'s or the assembler's
//! topic, or the general text with the program's sizes, as genuine BASIC
//! chooses among them.

use alloc::vec::Vec;

use tairix_openbasic::list::{pieces, Piece};

use super::functions::decimal;
use super::next::Next;
use super::{Machine, Step};
use crate::help::{self, Topic, KEYWORDS};
use crate::messages::Error;

/// Whether `topic`'s keyword begins with the letter `byte`, in either case.
fn begins(topic: &Topic, byte: u8) -> bool {
    topic.name.as_bytes().first() == Some(&byte.to_ascii_uppercase())
}

/// The keyword tables' column width, and the columns a row holds.
const COLUMN: usize = 10;
const COLUMNS: usize = 8;

impl Machine<'_, '_> {
    /// `HELP [topic]`.
    pub(crate) fn help_command(&mut self) -> Step<Next> {
        self.skip_spaces()?;
        let start = self.ptr;
        self.skip_line()?;
        let mut rest = Vec::new();
        let length = (self.ptr - start) as usize;
        rest.try_reserve_exact(length).map_err(|_| Error::NoRoom)?;
        rest.resize(length, 0);
        self.os.read_bytes(start, &mut rest)?;
        match pieces(&rest).next().map(|(_, piece)| piece) {
            Some(Piece::Word(name)) => match KEYWORDS.iter().find(|topic| topic.name == name) {
                Some(topic) => self.help_lines(topic.text)?,
                None => self.help_lines(&[help::NONE])?,
            },
            Some(Piece::Byte { byte: b'.', .. }) => self.help_table(KEYWORDS.iter())?,
            Some(Piece::Byte { byte: b'@', .. }) if rest.get(1) == Some(&b'%') => {
                self.help_lines(help::PRINT_FORMAT)?;
            }
            Some(Piece::Byte { byte: b'[', .. }) => self.help_lines(help::ASSEMBLER)?,
            Some(Piece::Byte { byte, .. }) if byte.is_ascii_digit() => {
                self.help_lines(&[help::NUMBER])?;
            }
            Some(Piece::Line(_)) => self.help_lines(&[help::NUMBER])?,
            Some(Piece::Byte { byte, .. }) if KEYWORDS.iter().any(|topic| begins(topic, byte)) => {
                self.help_lines(&[help::LETTER, ""])?;
                self.help_table(KEYWORDS.iter().filter(|topic| begins(topic, byte)))?;
            }
            _ => self.help_general()?,
        }
        Ok(Next::Done)
    }

    /// `lines`, each ended.
    fn help_lines(&mut self, lines: &[&str]) -> Step {
        for line in lines {
            self.output(line.as_bytes())?;
            self.newline()?;
        }
        Ok(())
    }

    /// `topics`' names in rows of columns, the last of each row unpadded.
    fn help_table<'t>(&mut self, topics: impl Iterator<Item = &'t Topic>) -> Step {
        let mut column = 0;
        let mut last = 0;
        for topic in topics {
            if column == COLUMNS {
                self.newline()?;
                column = 0;
            } else if column > 0 {
                self.output_spaces(COLUMN.saturating_sub(last))?;
            }
            self.output(topic.name.as_bytes())?;
            last = topic.name.len();
            column += 1;
        }
        if column > 0 {
            self.newline()?;
        }
        Ok(())
    }

    /// The general text, then the program's and its variables' sizes and
    /// the room free.
    fn help_general(&mut self) -> Step {
        self.help_lines(help::GENERAL)?;
        let program = decimal(self.top.wrapping_sub(self.page).cast_signed());
        let variables = decimal(self.fsa.wrapping_sub(self.lomem).cast_signed());
        let free = decimal(self.himem.saturating_sub(self.fsa).cast_signed());
        for part in [
            "The program is ".as_bytes(),
            program.as_bytes(),
            b" bytes long and its variables ",
            variables.as_bytes(),
            b"; ",
            free.as_bytes(),
            b" bytes are free.",
        ] {
            self.output(part)?;
        }
        self.newline()
    }
}
