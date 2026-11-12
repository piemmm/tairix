//! What `DOSKEY` keeps, as MS-DOS 5's keeps it: the macros and the commands
//! typed, in one buffer of the size `/BUFSIZE` gives, the macros first.
//! Each entry is ended by a NUL, a macro's name and its text joined by `=`.
//! The oldest commands are given up to make room for a new one or for a
//! macro; what will not fit even then is refused, the commands kept.

use alloc::vec;
use alloc::vec::Vec;

/// The longest command line: what function `0Ah`'s 128 bytes hold beside
/// the CR.
pub const MAX_LINE: usize = 127;

/// A macro: its name, kept in capitals, and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Macro {
    pub name: Vec<u8>,
    pub text: Vec<u8>,
}

impl Macro {
    /// Its bytes in the buffer.
    fn len(&self) -> usize {
        self.name.len() + self.text.len() + 2
    }
}

/// The macros and commands of a buffer of `size` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Store {
    size: usize,
    macros: Vec<Macro>,
    /// The commands typed, the oldest first.
    history: Vec<Vec<u8>>,
}

/// A macro the buffer cannot hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoRoom;

impl Store {
    /// The store a buffer of `size` bytes holds as `bytes`: its first
    /// `macros` bytes the macros, the rest the commands. What does not read
    /// as an entry ends it.
    #[must_use]
    pub fn read(size: usize, bytes: &[u8], macros: usize) -> Self {
        let (defined, typed) = bytes.split_at(macros.min(bytes.len()));
        let entries = |region: &[u8]| -> Vec<Vec<u8>> {
            region
                .split(|&byte| byte == 0)
                .filter(|entry| !entry.is_empty())
                .map(<[u8]>::to_vec)
                .collect()
        };
        let macros = entries(defined)
            .into_iter()
            .filter_map(|entry| {
                let at = entry.iter().position(|&byte| byte == b'=')?;
                Some(Macro {
                    name: entry[..at].to_vec(),
                    text: entry[at + 1..].to_vec(),
                })
            })
            .collect();
        let mut store = Self {
            size,
            macros,
            history: entries(typed),
        };
        while store.used() > size && !store.history.is_empty() {
            store.history.remove(0);
        }
        store
    }

    /// Its bytes in the buffer, and how many of them are the macros'.
    #[must_use]
    pub fn bytes(&self) -> (Vec<u8>, usize) {
        let mut out = Vec::with_capacity(self.used());
        for each in &self.macros {
            out.extend_from_slice(&each.name);
            out.push(b'=');
            out.extend_from_slice(&each.text);
            out.push(0);
        }
        let macros = out.len();
        for line in &self.history {
            out.extend_from_slice(line);
            out.push(0);
        }
        (out, macros)
    }

    fn used(&self) -> usize {
        self.macros.iter().map(Macro::len).sum::<usize>()
            + self
                .history
                .iter()
                .map(|line| line.len() + 1)
                .sum::<usize>()
    }

    /// The oldest commands given up until `more` bytes fit, none where they
    /// could not even with every command gone; whether they fit.
    fn make_room(&mut self, more: usize) -> bool {
        let defined: usize = self.macros.iter().map(Macro::len).sum();
        if defined + more > self.size {
            return false;
        }
        let mut used = self.used();
        let mut gone = 0;
        while used + more > self.size {
            used -= self.history[gone].len() + 1;
            gone += 1;
        }
        self.history.drain(..gone);
        true
    }

    /// `line` remembered as the newest command, unless it is empty or no
    /// room can be made for it.
    pub fn remember(&mut self, line: &[u8]) {
        let line = &line[..line.len().min(MAX_LINE)];
        if !line.is_empty() && self.make_room(line.len() + 1) {
            self.history.push(line.to_vec());
        }
    }

    /// Macro `name` given `text`, or with no text deleted.
    ///
    /// # Errors
    ///
    /// [`NoRoom`] when the buffer cannot hold it, the commands given up.
    pub fn define(&mut self, name: &[u8], text: &[u8]) -> Result<(), NoRoom> {
        let name = name.to_ascii_uppercase();
        self.macros.retain(|each| each.name != name);
        if text.is_empty() {
            return Ok(());
        }
        let defined = Macro {
            name,
            text: text.to_vec(),
        };
        if !self.make_room(defined.len()) {
            return Err(NoRoom);
        }
        self.macros.push(defined);
        Ok(())
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    pub fn clear_macros(&mut self) {
        self.macros.clear();
    }

    #[must_use]
    pub fn history(&self) -> &[Vec<u8>] {
        &self.history
    }

    #[must_use]
    pub fn macros(&self) -> &[Macro] {
        &self.macros
    }

    /// The commands `line` runs when its first word names a macro: the
    /// macro's text with its replaceable parameters filled in, split at
    /// each `$T`. None for a line beginning with a blank, which is how a
    /// command a macro shadows is reached.
    #[must_use]
    pub fn expand(&self, line: &[u8]) -> Option<Vec<Vec<u8>>> {
        let blank = |byte: &u8| matches!(byte, b' ' | b'\t');
        if line.first().is_none_or(blank) {
            return None;
        }
        let name_len = line.iter().position(blank).unwrap_or(line.len());
        let name = line[..name_len].to_ascii_uppercase();
        let found = self.macros.iter().find(|each| each.name == name)?;
        let rest = &line[name_len..];
        let all = &rest[rest
            .iter()
            .position(|byte| !blank(byte))
            .unwrap_or(rest.len())..];
        let words: Vec<&[u8]> = rest.split(blank).filter(|word| !word.is_empty()).collect();
        let mut commands = vec![Vec::new()];
        let mut text = found.text.iter().copied();
        while let Some(byte) = text.next() {
            let code = if byte == b'$' { text.next() } else { None };
            if byte == b'$' && matches!(code, Some(b'T' | b't')) {
                commands.push(Vec::new());
                continue;
            }
            let Some(command) = commands.last_mut() else {
                break;
            };
            match (byte, code) {
                (b'$', Some(digit @ b'1'..=b'9')) => {
                    command.extend_from_slice(
                        words
                            .get(usize::from(digit - b'1'))
                            .copied()
                            .unwrap_or_default(),
                    );
                }
                (b'$', Some(b'*')) => command.extend_from_slice(all),
                (b'$', Some(b'G' | b'g')) => command.push(b'>'),
                (b'$', Some(b'L' | b'l')) => command.push(b'<'),
                (b'$', Some(b'B' | b'b')) => command.push(b'|'),
                (b'$', Some(b'$') | None) => command.push(b'$'),
                (b'$', Some(other)) => command.extend_from_slice(&[b'$', other]),
                (plain, _) => command.push(plain),
            }
        }
        for command in &mut commands {
            command.truncate(MAX_LINE);
        }
        Some(commands)
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
