//! Command lines as `OS_CLI` begins reading them; `If`'s choice of command;
//! and an Obey file read as data, which runs no command.
//!
//! The read-only reading learns what a file would leave in its variables and
//! which sprite files it would load, without running it: of its commands only
//! `Set`, `SetMacro`, `Unset`, `If` and `IconSprites` are read, and nothing is
//! touched but the variables it is given.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::args::substitute;
use crate::expr::{evaluate_in_stages, Answer, Failure};
use crate::gstrans::{translated, Flags};
use crate::vars::{set, Request, Store, Variables};
use crate::{byte, is_terminator, name, Budget, Error};

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
/// is not zero.
pub fn choose<'a>(
    tail: &'a [u8],
    vars: &mut dyn Variables,
    budget: &mut Budget,
) -> Result<Option<&'a [u8]>, Error> {
    let line = &tail[..tail
        .iter()
        .position(|&b| is_terminator(b))
        .unwrap_or(tail.len())];
    let then = (0..line.len())
        .find(|&at| {
            line[at] == b' '
                && begins(&line[at + 1..], b"THEN")
                && matches!(byte(line, at + 5), b' ' | 0 | 10 | 13)
        })
        .ok_or(Error::NoThen)?;
    let condition = match evaluate_in_stages(&line[..then], vars, budget) {
        Ok(Answer::Integer(n)) => n,
        // A string, or a failure while its references were expanded, reads
        // as a string's refusal of the integer `If` wants.
        Ok(Answer::String(_)) | Err(Failure::Expanding(_)) => {
            return Err(Error::ExpressionIsAString)
        }
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
    fn icon_sprites(&mut self, file: &[u8], vars: &mut Store, budget: &mut Budget);
}

/// An Obey file read as data, given no parameters, with `Obey$Dir` first set
/// to `dir`. A line that is too long, opens a redirection, names its command
/// by an abbreviation or an alias, or fails, changes nothing.
pub fn read_only(
    file: &[u8],
    dir: &[u8],
    vars: &mut Store,
    observer: &mut dyn Observer,
    budget: &mut Budget,
) -> Result<(), Error> {
    let longest = vars.longest();
    set(vars, b"Obey$Dir", Request::Literal(dir), longest, budget)?;
    for line in file.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.len() >= MOST_LINE {
            continue;
        }
        // Obey's parameters, none here, are substituted first.
        let Ok(line) = substitute(b"", line, false, MOST_LINE) else {
            continue;
        };
        // A failing command ends only its own line.
        let _ = command(&line, vars, observer, budget, 0);
    }
    Ok(())
}

/// Whether `b` ends a command's name.
const fn ends_name(b: u8) -> bool {
    b <= b' '
        || matches!(
            b,
            b'"' | b'$' | b'%' | b'&' | b',' | b':' | b'<' | b'>' | b'\\' | b'^' | b'|' | 0x7F
        )
}

/// Whether `line` opens a redirection: a `{` outside quotes with a space
/// after it.
fn redirects(line: &[u8]) -> bool {
    let mut quoted = false;
    line.iter().enumerate().any(|(at, &b)| {
        if b == b'"' {
            quoted = !quoted;
        }
        b == b'{' && !quoted && byte(line, at + 1) == b' '
    })
}

/// One command line read, `depth` `If`s in.
fn command(
    line: &[u8],
    vars: &mut Store,
    observer: &mut dyn Observer,
    budget: &mut Budget,
    depth: usize,
) -> Result<(), Error> {
    let end = line
        .iter()
        .position(|&b| is_terminator(b))
        .unwrap_or(line.len());
    let start = line[..end]
        .iter()
        .take_while(|&&b| b == b' ' || b == b'*')
        .count();
    let line = &line[start..end];
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
    if aliases_apply && has_alias(word, vars)? {
        return Ok(());
    }
    let tail = &tail[tail.iter().take_while(|&&b| b == b' ').count()..];
    let longest = vars.longest();
    let given = parameters(tail);
    let is = |command: &[u8]| word.eq_ignore_ascii_case(command);
    if (is(b"Set") || is(b"SetMacro")) && given >= 2 {
        let target = name::name(tail);
        let value = &tail[target.len()..];
        let value = &value[value.iter().take_while(|&&b| b == b' ').count()..];
        let request = if is(b"Set") {
            Request::String(value)
        } else {
            Request::Macro(value)
        };
        set(vars, target, request, longest, budget)?;
    } else if is(b"Unset") && given == 1 {
        vars.delete(name::name(tail))?;
    } else if is(b"If") && depth < MOST_NESTED {
        if let Some(chosen) = choose(tail, vars, budget)? {
            command(chosen, vars, observer, budget, depth + 1)?;
        }
    } else if is(b"IconSprites") && given >= 1 {
        let (file, _) = translated(tail, Flags::SPACE_ENDS, vars, budget, MOST_LINE)?;
        observer.icon_sprites(&file, vars, budget);
    }
    Ok(())
}

/// The parameters `tail` holds, as `OS_CLI` counts them: runs of spaces part
/// them, and one opening with `"` runs on to the next `"`.
fn parameters(tail: &[u8]) -> usize {
    let end = tail
        .iter()
        .position(|&b| is_terminator(b))
        .unwrap_or(tail.len());
    let tail = &tail[..end];
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
fn has_alias(word: &[u8], vars: &mut Store) -> Result<bool, Error> {
    let mut alias = Vec::new();
    if !fallible::reserve(&mut alias, 6 + word.len()) {
        return Err(Error::SystemHeapFull);
    }
    alias.extend_from_slice(b"Alias$");
    alias.extend_from_slice(word);
    Ok(!word.is_empty() && !name::is_wild(word) && vars.lookup(&alias)?.is_some())
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
