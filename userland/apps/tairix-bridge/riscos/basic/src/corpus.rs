//! The recorded corpora's formats (`plans/RISCOS-EMULATOR.md` §17), shared
//! by the replays and the reference harness that records them: the
//! language's programs, and the assembler's lines with what genuine BASIC
//! laid down for each.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

/// The language corpus: programs, each run with `-quit`, what it prints
/// redirected to a file and its input read from another.
///
/// A program opens `program NAME`; its numbered lines follow, then any
/// `input "TEXT"`, bytes its keyboard types, printable ASCII but for `\xHH`.
pub mod language {
    use super::{format, String, ToString, Vec};

    /// A program of the corpus.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Program {
        /// Its name.
        pub name: String,
        /// Its text, a line ending each.
        pub text: Vec<u8>,
        /// What its keyboard types.
        pub input: Vec<u8>,
    }

    /// The programs `corpus` holds.
    ///
    /// # Errors
    ///
    /// The line of the corpus that is not a program's name, line or input,
    /// and why.
    pub fn programs(corpus: &str) -> Result<Vec<Program>, String> {
        let mut programs: Vec<Program> = Vec::new();
        let mut last_number: Option<u32> = None;
        for (index, line) in corpus.lines().enumerate() {
            let refused = |reason: &str| format!("line {}: {reason}", index + 1);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix("program ") {
                let named = !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
                if !named || programs.iter().any(|p| p.name == name) {
                    return Err(refused("a program has a name of its own"));
                }
                if programs.last().is_some_and(|p| p.text.is_empty()) {
                    return Err(refused("a program has lines"));
                }
                programs.push(Program {
                    name: name.to_string(),
                    text: Vec::new(),
                    input: Vec::new(),
                });
                last_number = None;
                continue;
            }
            let Some(program) = programs.last_mut() else {
                return Err(refused("a line within a program"));
            };
            if let Some(rest) = line.strip_prefix("input ") {
                let typed = super::unquoted(rest).ok_or_else(|| refused("one quoted text"))?;
                program.input.extend_from_slice(&typed);
                continue;
            }
            if !program.input.is_empty() {
                return Err(refused("a program's input follows its lines"));
            }
            let digits = line.bytes().take_while(u8::is_ascii_digit).count();
            let number: Option<u32> = line[..digits].parse().ok();
            let ascending = number.is_some_and(|n| last_number.is_none_or(|last| n > last));
            if !ascending || !line.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
                return Err(refused(
                    "a numbered line of printable ASCII, after the one before",
                ));
            }
            last_number = number;
            program.text.extend_from_slice(line.as_bytes());
            program.text.push(b'\n');
        }
        if programs.last().is_some_and(|p| p.text.is_empty()) {
            return Err("a program has lines".into());
        }
        Ok(programs)
    }
}

/// The assembler corpus: lines of assembler, each assembled alone in a
/// framing, and the bytes each dialect laid down, or the error it met.
///
/// A line is its framing, `at`, `off` or `sup`, then its text. The probe
/// program frames each line on a line of its own and writes what it laid
/// down to a file, the bytes in hexadecimal, or `E` and the error's number.
/// The answers give each line, then `= ` and what both dialects laid down,
/// or `v ` and `vi ` and what each did.
pub mod assembler {
    use super::{format, String, ToString, Vec, Write};

    /// How a line is assembled.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Framing {
        /// At `C%` under `OPT 2`: the bytes from `C%` to `P%`.
        At,
        /// As at `&10000`, laid down at `C%` under `OPT 6`: the bytes from
        /// `C%` to `O%`. An encoding's text with its branches' targets
        /// absolute assembles so.
        Off,
        /// As at `&100`, laid down at `C%` under `OPT 4`, errors hidden.
        Sup,
    }

    impl Framing {
        const fn name(self) -> &'static str {
            match self {
                Self::At => "at",
                Self::Off => "off",
                Self::Sup => "sup",
            }
        }

        /// The statements before and after the line, and the pointer the
        /// bytes end at.
        const fn frame(self) -> (&'static str, &'static str) {
            match self {
                Self::At => ("P%=C%:[OPT 2:", "P%"),
                Self::Off => ("O%=C%:P%=&10000:[OPT 6:", "O%"),
                Self::Sup => ("O%=C%:P%=&100:[OPT 4:", "O%"),
            }
        }
    }

    /// A line of the corpus.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Line {
        /// Its framing.
        pub framing: Framing,
        /// Its assembler text.
        pub text: String,
    }

    impl Line {
        /// The line as the corpus and the answers spell it.
        #[must_use]
        pub fn spelt(&self) -> String {
            format!("{} {}", self.framing.name(), self.text)
        }
    }

    /// The line `spelt` spells.
    fn line_of(spelt: &str) -> Option<Line> {
        let (name, text) = spelt.split_once(' ')?;
        let framing = match name {
            "at" => Framing::At,
            "off" => Framing::Off,
            "sup" => Framing::Sup,
            _ => return None,
        };
        let printable = !text.is_empty() && text.bytes().all(|b| (0x20..=0x7E).contains(&b));
        printable.then(|| Line {
            framing,
            text: text.to_string(),
        })
    }

    /// The lines `corpus` holds.
    ///
    /// # Errors
    ///
    /// The line of the corpus that is not a framing and printable text.
    pub fn lines(corpus: &str) -> Result<Vec<Line>, String> {
        corpus
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.is_empty() && !l.starts_with('#'))
            .map(|(index, l)| {
                line_of(l).ok_or_else(|| format!("line {}: a framing and its text", index + 1))
            })
            .collect()
    }

    /// The BASIC line number the corpus's line `index` is framed on.
    #[must_use]
    pub fn line_number(index: usize) -> usize {
        10 * (index + 3)
    }

    /// The probe program, a text one, writing what each line lays down to
    /// `output`.
    #[must_use]
    pub fn program(lines: &[Line], output: &str) -> String {
        let mut text = format!(
            "10 DIM C% 300:F%=OPENOUT\"{output}\"\n20 ON ERROR BPUT#F%,STR$ERL+\" E\"+STR$ERR:GOTO ERL+10\n"
        );
        for (index, line) in lines.iter().enumerate() {
            let (before, end) = line.framing.frame();
            let n = line_number(index);
            let _ = writeln!(text, "{n} {before}{}:]:PROCw({n},{end})", line.text);
        }
        let n = line_number(lines.len());
        let _ = writeln!(text, "{n} CLOSE#F%:QUIT");
        let _ = writeln!(
            text,
            "{} DEF PROCw(L%,E%):LOCAL T$,Q%:T$=STR$L%:Q%=C%:WHILE Q%<E%:T$+=\" \"+RIGHT$(\"0\"+STR$~?Q%,2):Q%+=1:ENDWHILE:BPUT#F%,T$:ENDPROC",
            n + 10
        );
        text
    }

    /// What the probe wrote for each of `count` lines: the bytes laid down,
    /// or `E` and an error's number.
    ///
    /// # Errors
    ///
    /// A line the probe wrote nothing for, or wrote twice.
    pub fn results(written: &[u8], count: usize) -> Result<Vec<String>, String> {
        let mut results: Vec<Option<String>> = Vec::new();
        results.resize(count, None);
        for record in written.split(|&b| b == b'\n').filter(|r| !r.is_empty()) {
            let record = String::from_utf8_lossy(record);
            let (number, rest) = record.split_once(' ').unwrap_or((&record, ""));
            let Ok(number) = number.trim().parse::<usize>() else {
                return Err(format!("the probe wrote {record:?}"));
            };
            let index = (number / 10).checked_sub(3).filter(|_| number % 10 == 0);
            let Some(slot) = index.and_then(|i| results.get_mut(i)) else {
                return Err(format!("the probe wrote for line {number}"));
            };
            if slot.replace(rest.trim().to_string()).is_some() {
                return Err(format!("the probe wrote twice for line {number}"));
            }
        }
        results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                r.ok_or_else(|| format!("the probe wrote nothing for line {}", line_number(i)))
            })
            .collect()
    }

    /// A line's recorded answer.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Recorded {
        /// The line.
        pub line: Line,
        /// What BASIC V laid down.
        pub v: String,
        /// What BASIC VI laid down.
        pub vi: String,
    }

    /// The answers file: `provenance`, then each line and what each dialect
    /// laid down.
    #[must_use]
    pub fn answers(provenance: &[String], lines: &[Line], v: &[String], vi: &[String]) -> String {
        let mut out = String::new();
        for line in provenance {
            let _ = writeln!(out, "# {line}");
        }
        out.push_str("# Never edit by hand.\n");
        for ((line, v), vi) in lines.iter().zip(v).zip(vi) {
            out.push_str(&line.spelt());
            out.push('\n');
            if v == vi {
                let _ = writeln!(out, "= {v}");
            } else {
                let _ = writeln!(out, "v {v}\nvi {vi}");
            }
        }
        out
    }

    /// The answers `recorded` holds.
    ///
    /// # Errors
    ///
    /// The answers' line out of its place.
    pub fn recorded(recorded: &str) -> Result<Vec<Recorded>, String> {
        let mut answers = Vec::new();
        let mut lines = recorded.lines().filter(|l| !l.starts_with('#')).peekable();
        while let Some(spelt) = lines.next() {
            let line = line_of(spelt).ok_or_else(|| format!("{spelt:?} is no line"))?;
            let answer = lines.next().ok_or("a line without its answer")?;
            let (v, vi) = if let Some(both) = answer.strip_prefix("= ") {
                (both.to_string(), both.to_string())
            } else {
                let v = answer
                    .strip_prefix("v ")
                    .ok_or("an answer out of its place")?;
                let vi = lines
                    .next()
                    .and_then(|l| l.strip_prefix("vi "))
                    .ok_or("BASIC VI's answer out of its place")?;
                (v.to_string(), vi.to_string())
            };
            answers.push(Recorded { line, v, vi });
        }
        Ok(answers)
    }
}

/// The session corpus: lines typed at BASIC's prompt, what each session
/// printed redirected to a file and its keys read from another.
///
/// A session opens `session NAME`; each `type "TEXT"` is a line typed,
/// printable ASCII but for `\xHH`, and `QUIT` is typed after the last.
/// Ahead of the sessions runs the calibration, [`session::CALIBRATION`]'s
/// lines, each of which prints one message: a line a session prints that is
/// one of those messages is told by its kind, never its text, so neither
/// BASIC's own messages nor the emulator's own words for them are compared.
/// What a session prints before its first prompt, the banner and the room
/// free, is left out.
pub mod session {
    use super::{format, quoted, unquoted, String, ToString, Vec, Write};

    /// A session of the corpus.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Session {
        /// Its name.
        pub name: String,
        /// Each line typed.
        pub typed: Vec<Vec<u8>>,
    }

    /// The calibration: each message's kind, and the lines that make it
    /// the first one the last of them prints.
    pub const CALIBRATION: [(&str, &[&str]); 11] = [
        ("syntax", &["DELETE"]),
        ("comma", &["DELETE 1"]),
        ("step", &["AUTO 1,0"]),
        ("mistake", &["TWIN"]),
        ("listo", &["LISTO 32"]),
        ("variable", &["LISTO"]),
        ("none", &["HELP TWIN"]),
        ("letters", &["HELP P"]),
        ("number", &["HELP 123"]),
        ("resident", &["LVAR"]),
        ("high", &["1 REM", "2 REM", "RENUMBER 65279,1"]),
    ];

    /// The sessions `corpus` holds.
    ///
    /// # Errors
    ///
    /// The line of the corpus that is not a session's name or a line it
    /// types, and why.
    pub fn sessions(corpus: &str) -> Result<Vec<Session>, String> {
        let mut sessions: Vec<Session> = Vec::new();
        for (index, line) in corpus.lines().enumerate() {
            let refused = |reason: &str| format!("line {}: {reason}", index + 1);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix("session ") {
                let named = !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
                if !named || sessions.iter().any(|s| s.name == name) {
                    return Err(refused("a session has a name of its own"));
                }
                if sessions.last().is_some_and(|s| s.typed.is_empty()) {
                    return Err(refused("a session types lines"));
                }
                sessions.push(Session {
                    name: name.to_string(),
                    typed: Vec::new(),
                });
                continue;
            }
            let Some(session) = sessions.last_mut() else {
                return Err(refused("a line within a session"));
            };
            let typed = line
                .strip_prefix("type ")
                .and_then(unquoted)
                .filter(|t| t.iter().all(|&b| b >= b' ') && t.len() < LONGEST_TYPED)
                .ok_or_else(|| {
                    refused("a line typed, one quoted text the line editor takes whole")
                })?;
            session.typed.push(typed);
        }
        if sessions.last().is_some_and(|s| s.typed.is_empty()) {
            return Err("a session types lines".into());
        }
        Ok(sessions)
    }

    /// The most a typed line may hold: what BASIC's line editor takes.
    const LONGEST_TYPED: usize = 239;

    /// What a session's keyboard types: each line, then `QUIT`.
    #[must_use]
    pub fn keys<'t>(typed: impl IntoIterator<Item = &'t [u8]>) -> Vec<u8> {
        let mut keys = Vec::new();
        for line in typed.into_iter().chain([&b"QUIT"[..]]) {
            keys.extend_from_slice(line);
            keys.push(b'\n');
        }
        keys
    }

    /// The calibration's keys.
    #[must_use]
    pub fn calibration_keys() -> Vec<u8> {
        keys(
            CALIBRATION
                .iter()
                .flat_map(|(_, lines)| lines.iter().map(|l| l.as_bytes())),
        )
    }

    /// What a run printed from its first prompt on, a line each, without
    /// the LF CR that ended it.
    #[must_use]
    pub fn printed(out: &[u8]) -> Vec<&[u8]> {
        let mut lines: Vec<&[u8]> = Vec::new();
        let mut rest = out;
        while !rest.is_empty() {
            let end = rest
                .windows(2)
                .position(|w| w == b"\n\r")
                .unwrap_or(rest.len());
            lines.push(&rest[..end]);
            rest = rest.get(end + 2..).unwrap_or(&[]);
        }
        let first = lines
            .iter()
            .position(|l| l.first() == Some(&b'>'))
            .unwrap_or(lines.len());
        lines.split_off(first)
    }

    /// Each calibration message: its kind, and the first line printed after
    /// the echo of its last line that is not empty.
    ///
    /// # Errors
    ///
    /// A calibration line the run did not echo, or one that printed nothing.
    pub fn messages(out: &[u8]) -> Result<Vec<(&'static str, Vec<u8>)>, String> {
        let lines = printed(out);
        let mut at = 0;
        let mut messages = Vec::with_capacity(CALIBRATION.len());
        for (kind, typed) in CALIBRATION {
            for line in typed {
                let echo = lines[at..]
                    .iter()
                    .position(|l| l.first() == Some(&b'>') && &l[1..] == line.as_bytes())
                    .ok_or_else(|| format!("the calibration did not echo {line:?}"))?;
                at += echo + 1;
            }
            let said = lines[at..]
                .iter()
                .take_while(|l| l.first() != Some(&b'>'))
                .find(|l| !l.is_empty())
                .ok_or_else(|| format!("the calibration's {kind} line printed nothing"))?;
            messages.push((kind, said.to_vec()));
        }
        Ok(messages)
    }

    /// A session's answer lines: each line it printed quoted, or `message`
    /// and its kind where it is a calibration message.
    #[must_use]
    pub fn told(out: &[u8], messages: &[(&'static str, Vec<u8>)]) -> Vec<String> {
        printed(out)
            .into_iter()
            .map(
                |line| match messages.iter().find(|(_, said)| said.as_slice() == line) {
                    Some((kind, _)) => format!("message {kind}"),
                    None => quoted(line),
                },
            )
            .collect()
    }

    /// The answers file: `provenance`, then each session and what each
    /// dialect printed, `v` or `vi` before each answer line.
    #[must_use]
    pub fn answers(
        provenance: &[String],
        sessions: &[Session],
        v: &[Vec<String>],
        vi: &[Vec<String>],
    ) -> String {
        let mut out = String::new();
        for line in provenance {
            let _ = writeln!(out, "# {line}");
        }
        out.push_str("# Never edit by hand.\n");
        for ((session, v), vi) in sessions.iter().zip(v).zip(vi) {
            let _ = writeln!(out, "session {}", session.name);
            for (dialect, lines) in [("v", v), ("vi", vi)] {
                for line in lines {
                    let _ = writeln!(out, "{dialect} {line}");
                }
            }
        }
        out
    }

    /// What a session printed under each dialect, as the answers record it.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Answered {
        /// The session's name.
        pub name: String,
        /// BASIC V's answer lines.
        pub v: Vec<String>,
        /// BASIC VI's.
        pub vi: Vec<String>,
    }

    /// The answers `recorded` holds.
    ///
    /// # Errors
    ///
    /// The answers' line out of its place.
    pub fn recorded(recorded: &str) -> Result<Vec<Answered>, String> {
        let mut answers: Vec<Answered> = Vec::new();
        for line in recorded.lines().filter(|l| !l.starts_with('#')) {
            if let Some(name) = line.strip_prefix("session ") {
                answers.push(Answered {
                    name: name.to_string(),
                    ..Answered::default()
                });
                continue;
            }
            let answered = answers.last_mut().ok_or("an answer before its session")?;
            if let Some(answer) = line.strip_prefix("v ") {
                answered.v.push(answer.to_string());
            } else if let Some(answer) = line.strip_prefix("vi ") {
                answered.vi.push(answer.to_string());
            } else {
                return Err(format!("{line:?} is no answer"));
            }
        }
        Ok(answers)
    }
}

/// `bytes` as a quoted text: printable ASCII but for a quote and a
/// backslash, every other byte `\xHH`.
#[must_use]
pub fn quoted(bytes: &[u8]) -> String {
    let mut out = String::from("\"");
    for &b in bytes {
        if (0x20..=0x7E).contains(&b) && b != b'"' && b != b'\\' {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "\\x{b:02X}");
        }
    }
    out.push('"');
    out
}

/// The bytes a quoted text spells: printable ASCII but for `\xHH`; none for
/// a text that is not one quoted text.
#[must_use]
pub fn unquoted(text: &str) -> Option<Vec<u8>> {
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = Vec::new();
    let mut rest = inner.as_bytes();
    while let Some((&b, after)) = rest.split_first() {
        if b == b'"' {
            return None;
        }
        if b == b'\\' {
            let hex = after.get(1..3)?;
            if after.first() != Some(&b'x') {
                return None;
            }
            let value = u8::from_str_radix(core::str::from_utf8(hex).ok()?, 16).ok()?;
            out.push(value);
            rest = &after[3..];
            continue;
        }
        out.push(b);
        rest = after;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::assembler::{
        answers, line_number, lines, program, recorded, results, Framing, Line,
    };
    use super::language::programs;
    use super::unquoted;
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn programs_are_read_with_their_lines_and_input() {
        let corpus = "# a comment\nprogram first\n10 PRINT 1\n20 END\ninput \"a\\x0D\"\nprogram second\n10 PRINT 2\n";
        let read = programs(corpus).expect("a corpus");
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].name, "first");
        assert_eq!(read[0].text, b"10 PRINT 1\n20 END\n");
        assert_eq!(read[0].input, b"a\r");
        assert_eq!(read[1].text, b"10 PRINT 2\n");
    }

    #[test]
    fn a_malformed_language_corpus_is_refused() {
        for corpus in [
            "10 PRINT 1\n",
            "program a\nprogram b\n10 PRINT\n",
            "program a\n20 PRINT\n10 PRINT\n",
            "program a\n10 PRINT\ninput \"x\"\n20 PRINT\n",
            "program A\n10 PRINT\n",
            "program a\n10 PRINT\nprogram a\n10 PRINT\n",
            "program a\n10 PRINT\ninput x\n",
        ] {
            assert!(programs(corpus).is_err(), "{corpus:?}");
        }
    }

    #[test]
    fn a_quoted_text_spells_its_bytes() {
        assert_eq!(unquoted("\"ab\\x22c\""), Some(b"ab\"c".to_vec()));
        assert_eq!(unquoted("\"a\"b\""), None);
        assert_eq!(unquoted("ab"), None);
        assert_eq!(unquoted("\"\\x4\""), None);
    }

    #[test]
    fn each_line_is_framed_on_its_own_numbered_line() {
        let read = lines("# lines\nat MOV R0,R1\noff B &10008\nsup MOV R0,#257\n").expect("lines");
        assert_eq!(
            read[1],
            Line {
                framing: Framing::Off,
                text: "B &10008".to_string()
            }
        );
        let text = program(&read, "HostFS:$.Out");
        let numbered: alloc::vec::Vec<&str> = text.lines().collect();
        assert!(numbered[2].starts_with("30 P%=C%:[OPT 2:MOV R0,R1:]:PROCw(30,P%)"));
        assert!(numbered[3].starts_with("40 O%=C%:P%=&10000:[OPT 6:B &10008:]:PROCw(40,O%)"));
        assert!(numbered[4].starts_with("50 O%=C%:P%=&100:[OPT 4:MOV R0,#257:]:PROCw(50,O%)"));
        assert_eq!(line_number(2), 50);
        assert!(lines("at\n").is_err());
        assert!(lines("mid MOV R0,R1\n").is_err());
    }

    #[test]
    fn what_the_probe_wrote_is_read_back_per_line() {
        let written = b"30 01 00 A0 E1\n40 E2\n50\n";
        assert_eq!(
            results(written, 3).expect("results"),
            ["01 00 A0 E1", "E2", ""]
        );
        assert!(results(b"30 E1\n30 E2\n", 1).is_err(), "twice");
        assert!(results(b"30 E1\n", 2).is_err(), "nothing for the second");
        assert!(results(b"35 E1\n", 1).is_err(), "no line's number");
    }

    #[test]
    fn answers_round_trip_with_each_dialect_shown_where_they_differ() {
        let read = lines("at A\nat B\n").expect("lines");
        let v = vec!["01".to_string(), "E2".to_string()];
        let vi = vec!["01".to_string(), "E6".to_string()];
        let text = answers(&["recorded".to_string()], &read, &v, &vi);
        assert_eq!(
            text,
            "# recorded\n# Never edit by hand.\nat A\n= 01\nat B\nv E2\nvi E6\n"
        );
        let back = recorded(&text).expect("answers");
        assert_eq!(back[1].vi, "E6");
        assert_eq!(back[0].line, read[0]);
        assert!(recorded("at A\n").is_err());
        assert!(recorded("at A\nvi E1\n").is_err());
    }
}
