//! The language descriptions `_kernel_init` was given: each one's code and
//! procedures, walked by their size words.

use tairix_riscos_kernel::os::{Fail, Os};

use super::context;
use super::statics::{at, Statics};

/// A description's words.
pub(crate) mod word {
    pub(crate) const SIZE: u32 = 0;
    pub(crate) const CODE_START: u32 = 4;
    pub(crate) const CODE_LIMIT: u32 = 8;
    pub(crate) const NAME: u32 = 12;
    pub(crate) const INIT: u32 = 16;
    pub(crate) const FINALISE: u32 = 20;
    pub(crate) const TRAP: u32 = 24;
    pub(crate) const UNCAUGHT_TRAP: u32 = 28;
    pub(crate) const EVENT: u32 = 32;
    pub(crate) const UNHANDLED_EVENT: u32 = 36;
    pub(crate) const FAST_EVENT: u32 = 40;
    pub(crate) const UNWIND: u32 = 44;
    pub(crate) const NAME_PROC: u32 = 48;
}

/// The most descriptions read, which the failed marks of a trap's search
/// have a bit for each of.
pub(crate) const LANGUAGES_MOST: u32 = 32;

/// A language description.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Language {
    /// Its place among the descriptions.
    pub(crate) index: u32,
    pub(crate) at: u32,
    size: u32,
    code: (u32, u32),
}

impl Language {
    /// Whether `pc` lies in its code.
    pub(crate) const fn holds(&self, pc: u32) -> bool {
        self.code.0 <= pc && pc < self.code.1
    }

    /// Its procedure at `word`; none where its size stops short of it, it
    /// is nought, or it lies outside its code, each of which takes the
    /// default.
    pub(crate) fn procedure(&self, os: &mut Os<'_>, word: u32) -> Result<Option<u32>, Fail> {
        if word.saturating_add(4) > self.size {
            return Ok(None);
        }
        let procedure = os.as_caller().read_word(self.at.wrapping_add(word))?;
        Ok((procedure != 0 && self.holds(procedure)).then_some(procedure))
    }

    /// Its size, which is the offset of the next.
    pub(crate) const fn size(&self) -> u32 {
        self.size
    }

    /// Its name.
    pub(crate) fn name(&self, os: &mut Os<'_>) -> Result<u32, Fail> {
        os.as_caller().read_word(self.at.wrapping_add(word::NAME))
    }

    /// Whether its name is `name`.
    fn named(&self, os: &mut Os<'_>, name: &[u8]) -> Result<bool, Fail> {
        let at = self.name(os)?;
        Ok(at != 0 && os.as_caller().read_until(at, name.len() + 1, |b| b == 0)?.0 == name)
    }

    /// Its finalisation procedure: its own, else for a language named C the
    /// C library's `Finalise`, which the PRM gives as the reason a C
    /// program's language must have that name; none for the default
    /// otherwise.
    pub(crate) fn finalisation(&self, os: &mut Os<'_>) -> Result<Option<u32>, Fail> {
        if let Some(procedure) = self.procedure(os, word::FINALISE)? {
            return Ok(Some(procedure));
        }
        if self.named(os, b"C")? {
            return context::c_entry(os, crate::c::FINALISE).map(Some);
        }
        Ok(None)
    }
}

/// The client's descriptions in their order, at most [`LANGUAGES_MOST`],
/// ending where a size would not reach the next or the area ends.
pub(crate) fn languages(os: &mut Os<'_>, statics: Statics) -> Result<Languages, Fail> {
    let start = statics.read(os, at::LANGUAGES)?;
    let end = statics.read(os, at::LANGUAGES_END)?;
    Ok(Languages {
        next: start,
        end,
        index: 0,
    })
}

/// A walk from the description at `position` to the area's end.
pub(crate) fn from(os: &mut Os<'_>, statics: Statics, position: u32) -> Result<Languages, Fail> {
    let end = statics.read(os, at::LANGUAGES_END)?;
    Ok(Languages {
        next: position,
        end,
        index: 0,
    })
}

/// A walk of the descriptions.
pub(crate) struct Languages {
    next: u32,
    end: u32,
    index: u32,
}

impl Languages {
    /// Where the walk stands: the next description's address.
    pub(crate) const fn position(&self) -> u32 {
        self.next
    }

    /// The next description, none past the last.
    pub(crate) fn next(&mut self, os: &mut Os<'_>) -> Result<Option<Language>, Fail> {
        if self.index >= LANGUAGES_MOST || self.next >= self.end {
            return Ok(None);
        }
        let at = self.next;
        let size = os.as_caller().read_word(at.wrapping_add(word::SIZE))?;
        let code = (
            os.as_caller()
                .read_word(at.wrapping_add(word::CODE_START))?,
            os.as_caller()
                .read_word(at.wrapping_add(word::CODE_LIMIT))?,
        );
        let language = Language {
            index: self.index,
            at,
            size,
            code,
        };
        self.index += 1;
        self.next = if size < word::FINALISE + 4 {
            self.end
        } else {
            at.wrapping_add(size)
        };
        Ok(Some(language))
    }
}

/// The language whose code holds `pc`, none where none does.
pub(crate) fn holding(
    os: &mut Os<'_>,
    statics: Statics,
    pc: u32,
) -> Result<Option<Language>, Fail> {
    let mut walk = languages(os, statics)?;
    while let Some(language) = walk.next(os)? {
        if language.holds(pc) {
            return Ok(Some(language));
        }
    }
    Ok(None)
}
