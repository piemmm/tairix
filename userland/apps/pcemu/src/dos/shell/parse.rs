//! What `COMMAND.COM` makes of a line before running it: its pipeline's
//! stages, each stage's redirections, the command word and its tail, the
//! internal command a word names, a batch line's `%` expansions, and the
//! prompt's `$` codes. Pure functions over bytes; the shell does the I/O.

use alloc::vec::Vec;

/// The longest line the shell runs, as DOS's line input takes one.
pub const LINE_MAX: usize = 127;

/// Why a line cannot be run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// A redirection names no file, or a pipe has nothing on a side.
    Syntax,
}

/// Bytes that end a command word.
const fn ends_word(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b',' | b';' | b'=' | b'+' | b'/' | b'"' | b'<' | b'>' | b'|'
    )
}

/// Bytes between arguments.
pub const fn is_separator(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b',' | b';' | b'=')
}

/// The stages of `line`'s pipeline, split at each `|` outside quotes.
///
/// # Errors
///
/// [`ParseError::Syntax`] for a stage with nothing in it.
pub fn stages(line: &[u8]) -> Result<Vec<&[u8]>, ParseError> {
    let mut out = Vec::new();
    let (mut start, mut quoted) = (0, false);
    for (at, &byte) in line.iter().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b'|' if !quoted => {
                out.push(&line[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    out.push(&line[start..]);
    if out.len() > 1 && out.iter().any(|stage| trim(stage).is_empty()) {
        return Err(ParseError::Syntax);
    }
    Ok(out)
}

/// A stage with its redirections taken out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Redirected {
    /// The command and its arguments.
    pub command: Vec<u8>,
    /// The file standard input is read from.
    pub input: Option<Vec<u8>>,
    /// The file standard output is written to, and whether it is appended
    /// to.
    pub output: Option<(Vec<u8>, bool)>,
}

/// `stage` with its `<`, `>` and `>>` taken out: the last of each kind
/// counts, as DOS has it.
///
/// # Errors
///
/// [`ParseError::Syntax`] for a redirection that names no file.
pub fn redirections(stage: &[u8]) -> Result<Redirected, ParseError> {
    let mut out = Redirected::default();
    let mut at = 0;
    let mut quoted = false;
    while at < stage.len() {
        let byte = stage[at];
        if byte == b'"' {
            quoted = !quoted;
        }
        if quoted || !matches!(byte, b'<' | b'>') {
            out.command.push(byte);
            at += 1;
            continue;
        }
        let append = byte == b'>' && stage.get(at + 1) == Some(&b'>');
        at += if append { 2 } else { 1 };
        while stage.get(at).is_some_and(|&b| b == b' ' || b == b'\t') {
            at += 1;
        }
        let start = at;
        while stage
            .get(at)
            .is_some_and(|&b| !is_separator(b) && !matches!(b, b'<' | b'>' | b'|'))
        {
            at += 1;
        }
        let name = stage[start..at].to_vec();
        if name.is_empty() {
            return Err(ParseError::Syntax);
        }
        if byte == b'<' {
            out.input = Some(name);
        } else {
            out.output = Some((name, append));
        }
    }
    Ok(out)
}

/// `text` without the separators that lead and trail it.
pub fn trim(text: &[u8]) -> &[u8] {
    let start = text
        .iter()
        .position(|&b| !matches!(b, b' ' | b'\t'))
        .unwrap_or(text.len());
    let end = text
        .iter()
        .rposition(|&b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        .map_or(start, |at| at + 1);
    text.get(start..end.max(start)).unwrap_or_default()
}

/// The command word of `text` — its separators skipped — and the tail after
/// it, from the byte that ended it.
pub fn command_word(text: &[u8]) -> (&[u8], &[u8]) {
    let text = trim_start(text);
    let end = text
        .iter()
        .position(|&b| ends_word(b))
        .unwrap_or(text.len());
    (&text[..end], &text[end..])
}

fn trim_start(text: &[u8]) -> &[u8] {
    let start = text
        .iter()
        .position(|&b| !is_separator(b))
        .unwrap_or(text.len());
    &text[start..]
}

/// The shell's internal commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs, reason = "each is the command of its name")]
pub enum Internal {
    Break,
    Call,
    Cd,
    Chcp,
    Cls,
    Copy,
    Ctty,
    Date,
    Del,
    Dir,
    Echo,
    Exit,
    For,
    Goto,
    If,
    LoadHigh,
    Md,
    Path,
    Pause,
    Prompt,
    Rd,
    Rem,
    Ren,
    Set,
    Shift,
    Time,
    Truename,
    Type,
    Ver,
    Verify,
    Vol,
}

/// The internal commands, by every name each answers to.
const NAMES: &[(&[u8], Internal)] = &[
    (b"BREAK", Internal::Break),
    (b"CALL", Internal::Call),
    (b"CD", Internal::Cd),
    (b"CHDIR", Internal::Cd),
    (b"CHCP", Internal::Chcp),
    (b"CLS", Internal::Cls),
    (b"COPY", Internal::Copy),
    (b"CTTY", Internal::Ctty),
    (b"DATE", Internal::Date),
    (b"DEL", Internal::Del),
    (b"ERASE", Internal::Del),
    (b"DIR", Internal::Dir),
    (b"ECHO", Internal::Echo),
    (b"EXIT", Internal::Exit),
    (b"FOR", Internal::For),
    (b"GOTO", Internal::Goto),
    (b"IF", Internal::If),
    (b"LH", Internal::LoadHigh),
    (b"LOADHIGH", Internal::LoadHigh),
    (b"MD", Internal::Md),
    (b"MKDIR", Internal::Md),
    (b"PATH", Internal::Path),
    (b"PAUSE", Internal::Pause),
    (b"PROMPT", Internal::Prompt),
    (b"RD", Internal::Rd),
    (b"RMDIR", Internal::Rd),
    (b"REM", Internal::Rem),
    (b"REN", Internal::Ren),
    (b"RENAME", Internal::Ren),
    (b"SET", Internal::Set),
    (b"SHIFT", Internal::Shift),
    (b"TIME", Internal::Time),
    (b"TRUENAME", Internal::Truename),
    (b"TYPE", Internal::Type),
    (b"VER", Internal::Ver),
    (b"VERIFY", Internal::Verify),
    (b"VOL", Internal::Vol),
];

/// The internal command `word` names, and where its tail starts in it: a
/// word with a drive is a program's, and `CD..` or `ECHO.` is a command and
/// the rest of the word.
pub fn internal(word: &[u8]) -> Option<(Internal, usize)> {
    if word.contains(&b':') {
        return None;
    }
    let end = word
        .iter()
        .position(|&b| matches!(b, b'.' | b'\\'))
        .unwrap_or(word.len());
    let name = &word[..end];
    NAMES
        .iter()
        .find(|(spelt, _)| spelt.eq_ignore_ascii_case(name))
        .map(|&(_, command)| (command, end))
}

/// The arguments of a tail, each without its separators; a switch begins
/// one, and quotes keep separators in one.
pub fn arguments(tail: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < tail.len() {
        while tail.get(at).is_some_and(|&b| is_separator(b)) {
            at += 1;
        }
        if at >= tail.len() {
            break;
        }
        let start = at;
        let mut quoted = false;
        at += 1;
        if tail[start] == b'"' {
            quoted = true;
        }
        while let Some(&byte) = tail.get(at) {
            if byte == b'"' {
                quoted = !quoted;
            } else if !quoted && (is_separator(byte) || byte == b'/') {
                break;
            }
            at += 1;
        }
        out.push(&tail[start..at]);
    }
    out
}

/// What a batch line's `%` expansions read: the batch's arguments, its
/// environment, and the variable a `FOR` is running.
pub trait Source {
    /// Argument `n`, from `%0` on, after the shifts made.
    fn argument(&self, n: usize) -> Option<&[u8]>;
    /// The environment variable `name`, upper case.
    fn variable(&self, name: &[u8]) -> Option<Vec<u8>>;
}

/// `line` with a batch file's expansions made: `%0`–`%9` as the arguments,
/// `%NAME%` as the environment's value, `%%` as one `%`; a `%` that begins
/// none of them is dropped, as DOS drops it. Cut to [`LINE_MAX`] bytes.
pub fn expand(line: &[u8], source: &impl Source) -> Vec<u8> {
    let mut out = Vec::with_capacity(line.len());
    let mut at = 0;
    while at < line.len() {
        let byte = line[at];
        if byte != b'%' {
            out.push(byte);
            at += 1;
            continue;
        }
        match line.get(at + 1) {
            Some(b'%') => {
                out.push(b'%');
                at += 2;
            }
            Some(&digit) if digit.is_ascii_digit() => {
                if let Some(argument) = source.argument(usize::from(digit - b'0')) {
                    out.extend_from_slice(argument);
                }
                at += 2;
            }
            _ => match line[at + 1..].iter().position(|&b| b == b'%') {
                Some(len) if len > 0 => {
                    let name = line[at + 1..at + 1 + len].to_ascii_uppercase();
                    if let Some(value) = source.variable(&name) {
                        out.extend_from_slice(&value);
                    }
                    at += len + 2;
                }
                _ => at += 1,
            },
        }
    }
    out.truncate(LINE_MAX);
    out
}

/// What the prompt's codes read.
pub struct Now<'a> {
    /// The current drive's letter.
    pub drive: u8,
    /// Its current directory, as `C:\DOS`; `None` when DOS cannot give it.
    pub directory: Option<&'a [u8]>,
    /// The date, as `Wed 11-10-2026`.
    pub date: &'a [u8],
    /// The time, as `14:02:51.37`.
    pub time: &'a [u8],
    /// The version, as the `VER` command shows it.
    pub version: &'a [u8],
}

/// The prompt `PROMPT`'s value spells, `$` codes made: `$P` the directory,
/// or that the drive is no longer valid, `$N` the drive, `$G` `>`, `$L`
/// `<`, `$B` `|`, `$Q` `=`, `$$` `$`, `$T` the time, `$D` the date, `$V` the
/// version, `$H` a backspace that rubs out, `$E` an escape, `$_` a new line.
/// A code it does not know is dropped.
pub fn prompt(spec: &[u8], now: &Now<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut codes = spec.iter();
    while let Some(&byte) = codes.next() {
        if byte != b'$' {
            out.push(byte);
            continue;
        }
        match codes.next().map(u8::to_ascii_uppercase) {
            Some(b'P') => {
                out.extend_from_slice(now.directory.unwrap_or(super::text::BAD_CURRENT_DRIVE));
            }
            Some(b'N') => out.push(now.drive),
            Some(b'G') => out.push(b'>'),
            Some(b'L') => out.push(b'<'),
            Some(b'B') => out.push(b'|'),
            Some(b'Q') => out.push(b'='),
            Some(b'$') => out.push(b'$'),
            Some(b'T') => out.extend_from_slice(now.time),
            Some(b'D') => out.extend_from_slice(now.date),
            Some(b'V') => out.extend_from_slice(now.version),
            Some(b'H') => out.extend_from_slice(crate::dos::char::RUB_OUT),
            Some(b'E') => out.push(0x1B),
            Some(b'_') => out.extend_from_slice(b"\r\n"),
            _ => {}
        }
    }
    out
}

/// A batch line's label, if it is one: the word after `:`, cut to eight
/// bytes as DOS compares them.
pub fn label(line: &[u8]) -> Option<&[u8]> {
    let line = trim_start(line);
    let rest = line.strip_prefix(b":")?;
    let rest = trim_start(rest);
    let end = rest
        .iter()
        .position(|&b| is_separator(b) || b == b'\r')
        .unwrap_or(rest.len());
    Some(&rest[..end.min(8)])
}

/// Whether two labels are the same, as DOS compares them: case aside, to
/// their first eight bytes.
pub fn same_label(a: &[u8], b: &[u8]) -> bool {
    let (a, b) = (&a[..a.len().min(8)], &b[..b.len().min(8)]);
    a.eq_ignore_ascii_case(b)
}

/// An `IF` condition, as its words spell it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition<'a> {
    /// `ERRORLEVEL n`: the last code is `n` or more.
    ErrorLevel(u8),
    /// `EXIST name`: a file matches the name.
    Exist(&'a [u8]),
    /// `a==b`: the strings are the same.
    Equal(&'a [u8], &'a [u8]),
}

/// `IF`'s tail parsed: whether it is negated, the condition, and the command
/// it runs.
///
/// # Errors
///
/// [`ParseError::Syntax`] for a condition DOS cannot read.
pub fn condition(tail: &[u8]) -> Result<(bool, Condition<'_>, &[u8]), ParseError> {
    let (word, rest) = split_word(tail);
    let (negated, word, rest) = if word.eq_ignore_ascii_case(b"NOT") {
        let (word, rest) = split_word(rest);
        (true, word, rest)
    } else {
        (false, word, rest)
    };
    if word.eq_ignore_ascii_case(b"ERRORLEVEL") {
        let (number, rest) = split_word(rest);
        let level = decimal(number).ok_or(ParseError::Syntax)?;
        return Ok((negated, Condition::ErrorLevel(level), rest));
    }
    if word.eq_ignore_ascii_case(b"EXIST") {
        let (name, rest) = split_word(rest);
        return if name.is_empty() {
            Err(ParseError::Syntax)
        } else {
            Ok((negated, Condition::Exist(name), rest))
        };
    }
    let text = trim_start(if word.is_empty() {
        rest
    } else {
        &tail[tail.len() - rest.len() - word.len()..]
    });
    let equals = find(text, b"==").ok_or(ParseError::Syntax)?;
    let left = &text[..equals];
    let after = trim_start(&text[equals + 2..]);
    let end = after
        .iter()
        .position(|&b| is_separator(b))
        .unwrap_or(after.len());
    let left = left
        .iter()
        .rposition(|&b| !is_separator(b))
        .map_or(&left[..0], |at| &left[..=at]);
    Ok((
        negated,
        Condition::Equal(left, &after[..end]),
        &after[end..],
    ))
}

/// The first word of `text`, and what follows it.
fn split_word(text: &[u8]) -> (&[u8], &[u8]) {
    let text = trim_start(text);
    let end = text
        .iter()
        .position(|&b| is_separator(b))
        .unwrap_or(text.len());
    (&text[..end], &text[end..])
}

fn find(text: &[u8], what: &[u8]) -> Option<usize> {
    text.windows(what.len()).position(|window| window == what)
}

/// A decimal byte, if `text` is one.
pub fn decimal(text: &[u8]) -> Option<u8> {
    if text.len() > 3 {
        return None;
    }
    number(text).and_then(|n| u8::try_from(n).ok())
}

/// `text` as a decimal number, if it is digits alone, at least one, and the
/// number fits 32 bits.
#[must_use]
pub fn number(text: &[u8]) -> Option<u32> {
    if text.is_empty() {
        return None;
    }
    text.iter().try_fold(0u32, |n, &b| {
        let digit = b.checked_sub(b'0').filter(|&d| d < 10)?;
        n.checked_mul(10)?.checked_add(u32::from(digit))
    })
}

/// A `FOR` command parsed: its variable, the set's text, and the command it
/// runs for each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct For<'a> {
    /// The variable's letter.
    pub variable: u8,
    /// The set, between its parentheses.
    pub set: &'a [u8],
    /// The command.
    pub command: &'a [u8],
}

/// `FOR`'s tail parsed: `%v IN (set) DO command`. A batch file writes the
/// variable `%%v`, which its line's expansion has made `%v` by now.
///
/// # Errors
///
/// [`ParseError::Syntax`] for a tail that is not one.
pub fn for_loop(tail: &[u8]) -> Result<For<'_>, ParseError> {
    let (variable, rest) = split_word(tail);
    let [b'%', letter] = variable else {
        return Err(ParseError::Syntax);
    };
    let letter = *letter;
    let (word, rest) = split_word(rest);
    if !word.eq_ignore_ascii_case(b"IN") {
        return Err(ParseError::Syntax);
    }
    let rest = trim_start(rest);
    let open = rest.strip_prefix(b"(").ok_or(ParseError::Syntax)?;
    let close = open
        .iter()
        .position(|&b| b == b')')
        .ok_or(ParseError::Syntax)?;
    let (word, command) = split_word(&open[close + 1..]);
    if !word.eq_ignore_ascii_case(b"DO") {
        return Err(ParseError::Syntax);
    }
    Ok(For {
        variable: letter,
        set: &open[..close],
        command: trim_start(command),
    })
}

/// `command` with `FOR`'s variable `%v` made `value`.
pub fn substitute(command: &[u8], variable: u8, value: &[u8]) -> Vec<u8> {
    let pattern = [b'%', variable];
    let mut out = Vec::with_capacity(command.len());
    let mut at = 0;
    while at < command.len() {
        if command[at..].starts_with(&pattern) {
            out.extend_from_slice(value);
            at += pattern.len();
        } else {
            out.push(command[at]);
            at += 1;
        }
    }
    out.truncate(LINE_MAX);
    out
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
