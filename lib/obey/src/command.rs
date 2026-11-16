//! Command lines as `OS_CLI` begins reading them; `If`'s choice of command;
//! and a command line or an Obey file read as data, which runs no command.
//!
//! The read-only reading learns what a line or a file would leave in its
//! variables and which sprite files it would load, without running it: of
//! its commands only `Set`, `SetMacro`, `Unset`, `If` and `IconSprites` are
//! read, and nothing is touched but the variables it is given.

use crate::args::substitute;
use crate::expr::{evaluate_in_stages, Answer, Failure};
use crate::gstrans::{translated, Flags};
use crate::vars::{set, Request, Variables};
use crate::{byte, is_terminator, line, name, Budget, Error};

/// The longest command line: its terminator comes within this many bytes.
pub const MOST_LINE: usize = 1024;

/// The most `If`s one line nests.
const MOST_NESTED: usize = 32;

/// Whether `text` begins with `word`, its letters in either case.
fn begins(text: &[u8], word: &[u8]) -> bool {
    text.get(..word.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(word))
}

/// `If`'s choice: the command its condition picks from `tail`, the text
/// after `If`, or `None` for a false one with no `Else`.
///
/// The condition ends at the first space before `Then` and a space or the
/// line's end, quotes or no quotes; the `Then` command at the first space
/// before `Else` and a space. A condition must be an integer, true where it
/// is not zero; no `Then`, or a string, is the command's syntax refused.
pub fn choose<'a>(
    tail: &'a [u8],
    vars: &mut dyn Variables,
    budget: &mut Budget,
) -> Result<Option<&'a [u8]>, Error> {
    let line = line(tail);
    let then = (0..line.len())
        .find(|&at| {
            line[at] == b' '
                && begins(&line[at + 1..], b"THEN")
                && matches!(byte(line, at + 5), b' ' | 0 | 10 | 13)
        })
        .ok_or(Error::Syntax)?;
    let condition = match evaluate_in_stages(&line[..then], vars, budget) {
        Ok(Answer::Integer(n)) => n,
        Err(Failure::Expanding(error)) if error.contains() => return Err(error),
        // A string, or a failure while its references were expanded, reads
        // as a string's refusal of the integer `If` wants.
        Ok(Answer::String(_)) | Err(Failure::Expanding(_)) => return Err(Error::Syntax),
        Err(Failure::Evaluating(error)) => return Err(error),
    };
    let after = &line[then + 5..];
    let otherwise = (0..after.len()).find(|&at| {
        after[at] == b' ' && begins(&after[at + 1..], b"ELSE") && byte(after, at + 5) == b' '
    });
    Ok(match (condition != 0, otherwise) {
        (true, Some(at)) => Some(&after[..at]),
        (true, None) => Some(after),
        (false, Some(at)) => Some(&after[at + 5..]),
        (false, None) => None,
    })
}

/// What a read-only reading reports besides the variables it set.
pub trait Observer {
    /// An `IconSprites` command's sprite file, GSTrans'd; `budget` is what
    /// the reading has left.
    fn icon_sprites(&mut self, file: &[u8], vars: &mut dyn Variables, budget: &mut Budget);
}

/// What a reading reads with.
pub struct Context<'a> {
    /// The variables it expands, sets and unsets.
    pub vars: &'a mut dyn Variables,
    /// The longest value it may set.
    pub longest: usize,
    /// Told of each sprite file.
    pub observer: &'a mut dyn Observer,
    /// The references it may still expand.
    pub budget: &'a mut Budget,
}

/// An Obey file read as data, its parameters `arguments`, with `Obey$Dir`
/// first set to `dir`. Its lines end at a CR, an LF or a NUL; each has the
/// parameters put in, with no more appended, in a line's room, then is read
/// as [`read_line`] reads one. The reading ends at the first line that
/// fails, with its error, as `*Obey` abandons a file at its first.
pub fn read_only(
    file: &[u8],
    dir: &[u8],
    arguments: &[u8],
    cx: &mut Context<'_>,
) -> Result<(), Error> {
    set(
        cx.vars,
        b"Obey$Dir",
        Request::Literal(dir),
        cx.longest,
        cx.budget,
    )?;
    for line in file.split(|&b| is_terminator(b)) {
        read_line(&substitute(arguments, line, false, MOST_LINE)?, cx)?;
    }
    Ok(())
}

/// One command line read as data, as `OS_CLI` begins to run it: `Set`,
/// `SetMacro`, `Unset` and `If` change the variables and `IconSprites` tells
/// the observer, each refusing parameters its syntax does not take. Any other
/// command, and a line that redirects, names its command by an abbreviation
/// or reaches it through an alias, changes nothing and fails nothing, since
/// what it would do is not this reading's to know.
pub fn read_line(line: &[u8], cx: &mut Context<'_>) -> Result<(), Error> {
    if crate::line(line).len() >= MOST_LINE {
        return Err(Error::LineTooLong);
    }
    command(line, cx, 0)
}

/// Whether `b` ends a command's name.
const fn ends_name(b: u8) -> bool {
    b <= b' '
        || matches!(
            b,
            b'"' | b'$' | b'%' | b',' | b':' | b'<' | b'>' | b'\\' | b'^' | b'|' | 0x7F
        )
}

/// Whether `line` opens a redirection as the PRMs write one, `{ > file }` or
/// `{ < file }`, outside quotes.
fn redirects(line: &[u8]) -> bool {
    let mut quoted = false;
    line.iter().enumerate().any(|(at, &b)| {
        if b == b'"' {
            quoted = !quoted;
        }
        b == b'{'
            && !quoted
            && byte(line, at + 1) == b' '
            && matches!(byte(line, at + 2), b'<' | b'>')
    })
}

/// One command line read, `depth` `If`s in.
fn command(line: &[u8], cx: &mut Context<'_>, depth: usize) -> Result<(), Error> {
    let line = crate::line(line);
    let start = line.iter().take_while(|&&b| b == b' ' || b == b'*').count();
    let line = &line[start..];
    if line.first().is_none_or(|&b| b == b'|') || redirects(line) {
        return Ok(());
    }
    // A `%` reaches the command past any alias of it.
    let (aliases_apply, line) = match line.strip_prefix(b"%") {
        Some(rest) => (false, rest),
        None => (true, line),
    };
    let len = line
        .iter()
        .position(|&b| ends_name(b))
        .unwrap_or(line.len());
    let (word, tail) = line.split_at(len);
    if aliases_apply && has_alias(word, cx.vars)? {
        return Ok(());
    }
    let tail = &tail[tail.iter().take_while(|&&b| b == b' ').count()..];
    let given = parameters(tail);
    let is = |command: &[u8]| word.eq_ignore_ascii_case(command);
    let takes = |fewest: usize, most: usize| {
        if (fewest..=most).contains(&given) {
            Ok(())
        } else {
            Err(Error::Syntax)
        }
    };
    if is(b"Set") || is(b"SetMacro") {
        takes(2, usize::MAX)?;
        let target = name::name(tail);
        let value = &tail[target.len()..];
        let value = &value[value.iter().take_while(|&&b| b == b' ').count()..];
        let request = if is(b"Set") {
            Request::String(value)
        } else {
            Request::Macro(value)
        };
        set(cx.vars, target, request, cx.longest, cx.budget)?;
    } else if is(b"Unset") {
        takes(1, 1)?;
        let pattern = name::name(tail);
        cx.budget.charge(cx.vars.cost(pattern))?;
        cx.vars.unset(pattern)?;
    } else if is(b"If") {
        if depth == MOST_NESTED {
            return Err(Error::ExpansionTooComplex);
        }
        if let Some(chosen) = choose(tail, cx.vars, cx.budget)? {
            command(chosen, cx, depth + 1)?;
        }
    } else if is(b"IconSprites") {
        takes(1, 1)?;
        let (file, _) = translated(tail, Flags::SPACE_ENDS, cx.vars, cx.budget, MOST_LINE)?;
        cx.observer.icon_sprites(&file, cx.vars, cx.budget);
    }
    Ok(())
}

/// The parameters `tail` holds, as `OS_CLI` counts them: runs of spaces part
/// them, and one opening with `"` runs on to the next `"`.
fn parameters(tail: &[u8]) -> usize {
    let tail = line(tail);
    let mut count = 0;
    let mut at = 0;
    loop {
        at += tail[at..].iter().take_while(|&&b| b == b' ').count();
        if at == tail.len() {
            return count;
        }
        count += 1;
        if tail[at] == b'"' {
            at += 1 + tail[at + 1..]
                .iter()
                .position(|&b| b == b'"')
                .unwrap_or(tail.len() - at - 1);
        }
        at += tail[at.min(tail.len())..]
            .iter()
            .take_while(|&&b| b != b' ')
            .count();
        at = at.min(tail.len());
    }
}

/// Whether an `Alias$` variable stands for the command `word`, which would
/// then run in its place.
fn has_alias(word: &[u8], vars: &mut dyn Variables) -> Result<bool, Error> {
    const ALIAS: &[u8] = b"Alias$";
    if word.is_empty() || name::is_wild(word) {
        return Ok(false);
    }
    let mut alias = [0; ALIAS.len() + MOST_LINE];
    let alias = alias
        .get_mut(..ALIAS.len() + word.len())
        .ok_or(Error::BufferOverflow)?;
    alias[..ALIAS.len()].copy_from_slice(ALIAS);
    alias[ALIAS.len()..].copy_from_slice(word);
    vars.exists(alias)
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
