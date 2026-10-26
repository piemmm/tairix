//! GNU coreutils' quoting styles: how a name is written inside a message, a
//! listing or a command's output, so the text a file name carries can neither
//! hide nor rewrite what is shown around it.
//!
//! [`quote`] names a file inside a message's sentence, a `-v` report or a
//! prompt; [`quote_subject`] names the file a `name: reason` line leads with;
//! [`quote_if_needed`] is `printf`'s `%q`; [`output_style`] is how a command
//! writes a name to its standard output; and a [`Quoted`] in any [`Style`] is
//! `ls`'s `--quoting-style`. Each formats straight into its destination,
//! allocating nothing.
//!
//! Three deliberate departures from GNU 9.4. Every character a faithful
//! rendering escapes is escaped ([`Faithful`]: the hidden characters, and a
//! mark stacked or doubled past telling apart), the tree's one rule for text
//! from elsewhere, where GNU escapes only what its locale calls unprintable and
//! so leaves a bidirectional override raw. The shell styles write GNU's own
//! form save its two faults with a name holding a `'` that ends in an escape:
//! GNU prefixes a needless `''` to it, and when such a name also begins with an
//! escape drops that escape's `$`, so its word no longer reads back as the
//! name. And a marked character takes no backslash inside `"…"`, where a shell
//! would read the backslash back too.

use core::fmt::{self, Write};

use tairix_visible::Faithful;

/// A GNU `--quoting-style`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Style {
    /// The name as it is (`literal`).
    Literal,
    /// Quoted where a shell needs it (`shell`).
    Shell,
    /// Always quoted (`shell-always`).
    ShellAlways,
    /// Quoted where a shell needs it, hidden characters as `$'…'`
    /// (`shell-escape`).
    ShellEscape,
    /// Always quoted, hidden characters as `$'…'` (`shell-escape-always`).
    ShellEscapeAlways,
    /// A C string literal (`c`).
    C,
    /// C escapes with no surrounding quotes (`escape`).
    Escape,
}

/// `text` written in one [`Style`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Quoted<'a> {
    text: &'a str,
    style: Style,
    hide: bool,
    marks: &'a str,
}

/// `text` as a message's sentence names a file or an operand: always quoted,
/// hidden characters escaped.
#[must_use]
pub const fn quote(text: &str) -> Quoted<'_> {
    Quoted::new(text, Style::ShellEscapeAlways)
}

/// `text` as a `name: reason` line leads with it: quoted where a shell needs
/// it or where it holds a `:` of its own, which would pass for the one ending
/// the name, hidden characters escaped.
#[must_use]
pub const fn quote_subject(text: &str) -> Quoted<'_> {
    Quoted::new(text, Style::ShellEscape).marking(":")
}

/// `text` quoted only where a shell needs it, hidden characters escaped
/// (`printf`'s `%q`).
#[must_use]
pub const fn quote_if_needed(text: &str) -> Quoted<'_> {
    Quoted::new(text, Style::ShellEscape)
}

/// The style a command writes a name from elsewhere to its standard output
/// in, as `ls` defaults: escaped where a shell needs it at an attested
/// terminal, where a hidden character could command or rearrange the screen,
/// and the name's own bytes anywhere else, so a pipe carries what is there.
#[must_use]
pub const fn output_style(at_terminal: bool) -> Style {
    if at_terminal {
        Style::ShellEscape
    } else {
        Style::Literal
    }
}

impl<'a> Quoted<'a> {
    /// `text` in `style`, hidden characters shown as they are wherever the
    /// style does not escape them.
    #[must_use]
    pub const fn new(text: &'a str, style: Style) -> Self {
        Self {
            text,
            style,
            hide: false,
            marks: "",
        }
    }

    /// The same, with each character a style does not escape that a faithful
    /// rendering would shown as `?` (`ls -q`). The escaping styles are
    /// unchanged.
    #[must_use]
    pub const fn hiding(self) -> Self {
        Self { hide: true, ..self }
    }

    /// The same, with each of `marks` kept apart from the text the name sits
    /// in: it forces the shell styles to quote, and the C styles escape it
    /// (`ls -R` marks the `:` its headings end in, `ls -b` a space).
    #[must_use]
    pub const fn marking(self, marks: &'a str) -> Self {
        Self { marks, ..self }
    }

    fn c(&self, f: &mut fmt::Formatter<'_>, quoted: bool) -> fmt::Result {
        if quoted {
            f.write_char('"')?;
        }
        let mut run = 0;
        let mut faithful = Faithful::default();
        for (at, ch) in self.text.char_indices() {
            let escaped = faithful.escapes(ch);
            let backslashed = match ch {
                '\\' => true,
                '"' if quoted => true,
                _ if escaped => {
                    f.write_str(&self.text[run..at])?;
                    write_escape(f, ch)?;
                    run = at + ch.len_utf8();
                    continue;
                }
                _ => self.marks.contains(ch),
            };
            if backslashed {
                f.write_str(&self.text[run..at])?;
                f.write_char('\\')?;
                run = at;
            }
        }
        f.write_str(&self.text[run..])?;
        if quoted {
            f.write_char('"')?;
        }
        Ok(())
    }

    fn shell(&self, f: &mut fmt::Formatter<'_>, escape: bool, always: bool) -> fmt::Result {
        let shape = Shape::of(self.text, escape, self.marks);
        if !always && !shape.needs_quotes {
            return self.shown(f, self.text);
        }
        if shape.has_single_quote && !shape.hard {
            f.write_char('"')?;
            self.shown(f, self.text)?;
            return f.write_char('"');
        }
        // Inside `'…'` unless a `$'…'` group is open; a `'` always leaves the
        // name inside `'…'` again, so the form needs no third state.
        let mut escaping = false;
        f.write_char('\'')?;
        let mut run = 0;
        let mut faithful = Faithful::default();
        for (at, ch) in self.text.char_indices() {
            if escape && faithful.escapes(ch) {
                self.shown(f, &self.text[run..at])?;
                if !escaping {
                    f.write_str("'$'")?;
                    escaping = true;
                }
                write_escape(f, ch)?;
                run = at + ch.len_utf8();
            } else if ch == '\'' {
                self.shown(f, &self.text[run..at])?;
                f.write_str("'\\''")?;
                escaping = false;
                run = at + 1;
            } else if escaping {
                f.write_str("''")?;
                escaping = false;
                run = at;
            }
        }
        self.shown(f, &self.text[run..])?;
        f.write_char('\'')
    }

    /// `text` as it stands, each character a faithful rendering escapes `?`
    /// when hiding.
    fn shown(&self, f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
        if !self.hide {
            return f.write_str(text);
        }
        let mut run = 0;
        let mut faithful = Faithful::default();
        for (at, ch) in text.char_indices() {
            if faithful.escapes(ch) {
                f.write_str(&text[run..at])?;
                f.write_char('?')?;
                run = at + ch.len_utf8();
            }
        }
        f.write_str(&text[run..])
    }
}

impl fmt::Display for Quoted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.style {
            Style::Literal => self.shown(f, self.text),
            Style::C => self.c(f, true),
            Style::Escape => self.c(f, false),
            Style::Shell => self.shell(f, false, false),
            Style::ShellAlways => self.shell(f, false, true),
            Style::ShellEscape => self.shell(f, true, false),
            Style::ShellEscapeAlways => self.shell(f, true, true),
        }
    }
}

/// What a name holds, as the shell styles decide its quoting.
struct Shape {
    needs_quotes: bool,
    has_single_quote: bool,
    /// A character other than a space or `'` forces quoting, so the concise
    /// `"…"` form cannot carry the name.
    hard: bool,
}

impl Shape {
    fn of(text: &str, escape: bool, marks: &str) -> Self {
        let mut shape = Self {
            needs_quotes: text.is_empty(),
            has_single_quote: false,
            hard: false,
        };
        let mut faithful = Faithful::default();
        for (at, ch) in text.char_indices() {
            let (forces, hard) = if escape && faithful.escapes(ch) {
                (true, true)
            } else {
                bearing(ch, at, text.len(), escape)
            };
            shape.needs_quotes |= forces || marks.contains(ch);
            shape.hard |= hard;
            shape.has_single_quote |= ch == '\'';
        }
        shape
    }
}

/// How `ch`, starting at byte `at` of a name `len` bytes long, bears on its
/// quoting: whether it forces quotes, and whether it rules out the `"…"` form.
/// `#` and `~` mean something to a shell only leading a word and a brace only
/// as the whole of one, but GNU keeps all of them out of `"…"` elsewhere.
const fn bearing(ch: char, at: usize, len: usize, escape: bool) -> (bool, bool) {
    match ch {
        '\'' | ' ' => (true, false),
        '#' | '~' => (at == 0, at != 0),
        '{' | '}' => (len == 1, true),
        '!' | '"' | '$' | '&' | '(' | ')' | '*' | ';' | '<' | '=' | '>' | '?' | '[' | '\\'
        | '^' | '`' | '|' => (true, true),
        // A shell splits or joins a word at these, so even the styles that
        // leave them raw must quote them.
        '\t' | '\n' | '\r' if !escape => (true, true),
        _ => (false, false),
    }
}

/// `ch`'s C escape: its letter where C names one, else each byte of its UTF-8
/// encoding in three octal digits, so the escape reads back as the character.
fn write_escape(f: &mut fmt::Formatter<'_>, ch: char) -> fmt::Result {
    let letter = match ch {
        '\u{7}' => 'a',
        '\u{8}' => 'b',
        '\t' => 't',
        '\n' => 'n',
        '\u{b}' => 'v',
        '\u{c}' => 'f',
        '\r' => 'r',
        _ => {
            let mut utf8 = [0; 4];
            for byte in ch.encode_utf8(&mut utf8).bytes() {
                write!(f, "\\{byte:03o}")?;
            }
            return Ok(());
        }
    };
    f.write_char('\\')?;
    f.write_char(letter)
}

#[cfg(test)]
#[path = "quote_tests.rs"]
mod tests;
