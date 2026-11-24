//! A Draw file checked whole before anything of it is drawn or measured,
//! as RISC OS 5.30's DrawFile checks one: its header, every object's size
//! against what holds it, one font table at most, and every text's font in
//! it.
//!
//! An object too big for what holds it is reported after the last object
//! the check accepted, at any depth, as RISC OS reports it; an object no
//! bigger than its own type and size words, or of no whole number of words,
//! is reported so too, where RISC OS reads on into what follows it.

use tairix_drawing::riscos::format::{
    Misframed, Object, Objects, BARE_HEADER, FONT_NUMBER, FONT_TABLE, GROUP, HEADER,
    HEADER_VERSION, MAGIC, MAJOR_VERSION, TAGGED, TEXT, TEXT_STYLE, TRANSFORMED_TEXT,
    TRANSFORMED_TEXT_FIELDS,
};
use tairix_util::endian::le_u32;

use super::render::terminated;
use super::stack::{self, Depth, Level};

/// What holds an object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Within {
    File,
    Group,
    Tagged,
}

/// Why a file is refused before anything of it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    NotDraw,
    Version(u32),
    TwoFontTables {
        at: usize,
    },
    NoSuchFont {
        number: u32,
        at: usize,
    },
    TooBig {
        within: Within,
        after: Option<usize>,
    },
}

/// A file's font table: each number's name, where it is in the file.
#[derive(Debug, Default)]
pub(super) struct Fonts {
    /// The table's place, if the file has one.
    pub(super) table: Option<usize>,
    /// Each named font's number and where its name starts.
    pub(super) names: alloc::vec::Vec<(u8, usize)>,
}

impl Fonts {
    pub(super) fn named(&self, number: u32) -> Option<usize> {
        self.names
            .iter()
            .rev()
            .find(|&&(n, _)| u32::from(n) == number)
            .map(|&(_, at)| at)
    }

    fn read(&mut self, table: Object<'_>) -> Result<(), NoRoom> {
        let mut at = BARE_HEADER;
        while let Some(&number) = table.bytes.get(at) {
            if number == 0 {
                break;
            }
            let name = at + 1;
            let length = terminated(table.bytes, name).len();
            tairix_util::fallible::try_push(&mut self.names, (number, table.at + name))
                .map_err(|_| NoRoom)?;
            at = name + length + 1;
        }
        Ok(())
    }
}

/// The host refused memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NoRoom;

/// What checking stopped at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stop {
    Refused(Refusal),
    NoRoom,
    /// The objects nest deeper than the supervisor stack the check runs on.
    Overflowed,
}

impl From<NoRoom> for Stop {
    fn from(_: NoRoom) -> Self {
        Self::NoRoom
    }
}

impl From<Refusal> for Stop {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// `bytes`, a whole Draw file, checked: its font table if it is sound.
pub(super) fn check(bytes: &[u8]) -> Result<Fonts, Stop> {
    if bytes.len() < HEADER || !bytes.starts_with(MAGIC) {
        return Err(Refusal::NotDraw.into());
    }
    let major = le_u32(bytes, HEADER_VERSION).unwrap_or(0);
    if major > MAJOR_VERSION {
        return Err(Refusal::Version(major).into());
    }
    let mut checker = Checker {
        fonts: Fonts::default(),
        after: None,
    };
    let objects = Objects::new(&bytes[HEADER..], HEADER);
    checker.run(objects, Within::File, Depth::default())?;
    Ok(checker.fonts)
}

struct Checker {
    fonts: Fonts,
    /// The last object accepted.
    after: Option<usize>,
}

impl Checker {
    fn run(&mut self, objects: Objects<'_>, within: Within, depth: Depth) -> Result<(), Stop> {
        for object in objects {
            self.object(object, within, depth)?;
        }
        Ok(())
    }

    fn object(
        &mut self,
        object: Result<Object<'_>, Misframed>,
        within: Within,
        depth: Depth,
    ) -> Result<(), Stop> {
        let Ok(object) = object else {
            return Err(Refusal::TooBig {
                within,
                after: self.after,
            }
            .into());
        };
        self.after = Some(object.at);
        match object.kind {
            FONT_TABLE => {
                if self.fonts.table.is_some() {
                    return Err(Refusal::TwoFontTables { at: object.at }.into());
                }
                self.fonts.table = Some(object.at);
                self.fonts.read(object)?;
            }
            TEXT | TRANSFORMED_TEXT => {
                let fields = if object.kind == TEXT {
                    0
                } else {
                    TRANSFORMED_TEXT_FIELDS
                };
                let number = le_u32(object.bytes, TEXT_STYLE + fields).unwrap_or(0) & FONT_NUMBER;
                if number != 0 && self.fonts.named(number).is_none() {
                    return Err(Refusal::NoSuchFont {
                        number,
                        at: object.at,
                    }
                    .into());
                }
            }
            GROUP => {
                let deeper = depth
                    .enter(stack::CHECK, Level::Group)
                    .ok_or(Stop::Overflowed)?;
                self.run(stack::group_objects(object), Within::Group, deeper)?;
            }
            TAGGED => {
                if let Some(inner) = stack::tagged_object(object) {
                    let deeper = depth
                        .enter(stack::CHECK, Level::Tagged)
                        .ok_or(Stop::Overflowed)?;
                    self.object(inner, Within::Tagged, deeper)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
