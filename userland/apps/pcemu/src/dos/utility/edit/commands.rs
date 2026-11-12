//! What `EDIT`'s commands do: the dialogs that ask for what they need and
//! what their buttons then do, the search and the change that verifies
//! each match, the clipboard and a dialog's listing kept at the store's
//! end, and what the editor makes of the program's answers.

use super::dialogs::{check, Button, Dialog, Kind, Lists, Outcome, Source};
use super::editor::{Editor, Spot};
use super::help::Topic;
use super::menus::Command;
use super::text::{in_word, Store, Text, LF};
use super::ui::{entry, entry_back, failure, Action, Changing, Done, Focus, Settings, Then, Ui};
use alloc::vec::Vec;

/// What the messages say.
mod said {
    pub const NOT_FOUND: &[u8] = b"Match not found";
    pub const CHANGED: &[u8] = b"Change complete";
    pub const TABS: &[u8] = b"Tab stops must be from 1 to 99";
    pub const KEPT: &[u8] = b"Saved as ";
}

/// What a failure says.
fn failure_text(code: u8) -> &'static [u8] {
    match code {
        0x02 => b"File not found",
        0x03 => b"Path not found",
        0x04 => b"Too many files are open",
        0x05 => b"Path/File access error",
        0x0F => b"Invalid drive",
        0x13 => b"Disk is write-protected",
        0x15 => b"Drive not ready",
        failure::DISK_FULL => b"Disk full",
        failure::TOO_LARGE => b"The file is too large to edit",
        failure::MEMORY => b"Out of memory",
        failure::BAD_NAME => b"Bad file name",
        _ => b"Device fault",
    }
}

/// Entry `n` of a listing being made below a clipboard of `clip` bytes:
/// of `kind`, named `name`.
pub fn put_entry<S: Store>(text: &mut Text<'_, S>, clip: u32, n: u16, kind: u8, name: &[u8]) {
    let mut raw = [0u8; entry::LEN];
    raw[0] = kind;
    let len = name.len().min(entry::LEN - 1);
    raw[1..=len].copy_from_slice(&name[..len]);
    text.write_end(entry_back(clip, n), &raw);
}

/// Whether a listing entry is `..`, which leads its directories.
fn is_parent(raw: &[u8; entry::LEN]) -> bool {
    raw[0] == entry::DIRECTORY && raw[1..3] == *b".." && raw[3] == 0
}

/// Whether `name` holds a wildcard, and so names files to list.
#[must_use]
pub fn is_pattern(name: &[u8]) -> bool {
    name.iter().any(|&byte| byte == b'*' || byte == b'?')
}

/// `name` split at its last `\` or `:`: the directory it names, and the
/// file or pattern in it.
#[must_use]
pub fn split_name(name: &[u8]) -> (&[u8], &[u8]) {
    match name.iter().rposition(|&byte| byte == b'\\' || byte == b':') {
        Some(at) => (&name[..=at], &name[at + 1..]),
        None => (b"", name),
    }
}

impl Ui {
    /// `command` carried out.
    pub fn command<S: Store>(&mut self, command: Command, text: &mut Text<'_, S>) -> Action {
        match command {
            Command::New => self.leaving(Then::New, text),
            Command::Open => self.leaving(Then::Open, text),
            Command::Exit => self.leaving(Then::Exit, text),
            Command::Save => self.save(),
            Command::SaveAs => self.save_as(),
            Command::Print => {
                let mut dialog = Dialog::open(Kind::Print);
                dialog.choose(u8::from(self.edit(text).selection().is_none()));
                self.open_dialog(&dialog)
            }
            Command::Cut | Command::Copy => match self.edit(text).copy(command == Command::Cut) {
                Some(bytes) => self.keep(&bytes, text),
                None => Action::Wait,
            },
            Command::Paste if self.clip == 0 => Action::Wait,
            Command::Paste => {
                let bytes = self.clipboard(text);
                let done = self.edit(text).type_in(&bytes);
                self.edited(done, text)
            }
            Command::Clear => {
                self.edit(text).clear();
                Action::Wait
            }
            Command::Find | Command::Change => self.search_dialog(command, text),
            Command::Repeat if self.search.find.len == 0 => self.search_dialog(Command::Find, text),
            Command::Repeat => self.find_next(text),
            Command::Display => self.display_dialog(),
            Command::Started => self.show_help(Topic::Started, false),
            Command::Keys => self.show_help(Topic::Keys, false),
            Command::About => self.open_dialog(&Dialog::open(Kind::About)),
        }
    }

    pub(super) fn open_dialog(&mut self, dialog: &Dialog) -> Action {
        self.dialog = *dialog;
        self.focus = Focus::Dialog;
        Action::Wait
    }

    /// The dialog closed, any listing it had given back to the document.
    fn close_dialog<S: Store>(&mut self, text: &mut Text<'_, S>) {
        self.dialog = Dialog::default();
        self.focus = Focus::Text;
        self.release_listing(text);
    }

    /// A message saying `note`, in place of any dialog.
    fn message<S: Store>(&mut self, note: &[u8], text: &mut Text<'_, S>) -> Action {
        self.close_dialog(text);
        self.open_dialog(&Dialog::message(note))
    }

    /// The message a failure gives; nothing waits on it.
    pub(super) fn failed<S: Store>(&mut self, code: u8, text: &mut Text<'_, S>) -> Action {
        self.then = Then::Nothing;
        if code == failure::KEPT {
            let note = [said::KEPT, self.target.text()].concat();
            return self.message(&note, text);
        }
        self.message(failure_text(code), text)
    }

    /// The document about to be left for `then`: asked first whether to
    /// save it, where it has changes.
    fn leaving<S: Store>(&mut self, then: Then, text: &mut Text<'_, S>) -> Action {
        if self.editor.modified {
            self.then = then;
            return self.open_dialog(&Dialog::open(Kind::Unsaved));
        }
        self.carry_on(then, text)
    }

    fn carry_on<S: Store>(&mut self, then: Then, text: &mut Text<'_, S>) -> Action {
        self.then = Then::Nothing;
        match then {
            Then::Nothing => Action::Wait,
            Then::New => {
                let len = text.len();
                text.delete(0..len);
                self.name.set(b"");
                self.loaded(text);
                Action::Wait
            }
            Then::Open => {
                let mut dialog = Dialog::open(Kind::Open);
                dialog.fields[0].set(self.pattern.text());
                dialog.fields[0].fresh = true;
                self.open_dialog(&dialog);
                Action::List
            }
            Then::Exit => Action::Exit,
        }
    }

    /// The document is the file just read, or a new one: the editor at its
    /// start, its lines counted.
    pub fn loaded<S: Store>(&mut self, text: &mut Text<'_, S>) {
        let len = text.len();
        let lines = text.breaks_in(0..len).saturating_add(1);
        self.editor = Editor {
            rows: self.editor.rows,
            columns: self.editor.columns,
            tab: self.editor.tab,
            insert: self.editor.insert,
            lines,
            ..Editor::default()
        };
        self.marks = super::ui::Marks::default();
    }

    fn save(&mut self) -> Action {
        if self.name.len == 0 {
            return self.save_as();
        }
        self.target = self.name;
        Action::Save
    }

    fn save_as(&mut self) -> Action {
        let mut dialog = Dialog::open(Kind::SaveAs);
        if self.name.len > 0 {
            dialog.fields[0].set(&self.title());
        }
        self.open_dialog(&dialog);
        Action::List
    }

    /// Find or Change opened, its text the selection's, the word at the
    /// cursor's, or the last search's.
    fn search_dialog<S: Store>(&mut self, command: Command, text: &mut Text<'_, S>) -> Action {
        let kind = if command == Command::Change {
            Kind::Change
        } else {
            Kind::Find
        };
        let mut dialog = Dialog::open(kind);
        let offered = self.offered(text);
        dialog.fields[0].set(if offered.is_empty() {
            self.search.find.text()
        } else {
            &offered
        });
        dialog.fields[0].fresh = true;
        dialog.fields[1].set(self.search.change.text());
        dialog.checks = self.search.flags;
        self.open_dialog(&dialog)
    }

    /// What a search dialog offers to look for: the selection on one line,
    /// or the word the cursor is in.
    fn offered<S: Store>(&mut self, text: &mut Text<'_, S>) -> Vec<u8> {
        if let Some(range) = self.edit(text).selection() {
            let bytes = text.read(range);
            return if bytes.contains(&LF) || bytes.len() > super::dialogs::FIELD_LEN {
                Vec::new()
            } else {
                bytes
            };
        }
        let at = self.edit(text).cursor_offset();
        let word = |byte: Option<u8>| byte.is_some_and(in_word);
        let (mut start, mut end) = (at, at);
        while start > 0 && word(text.byte(start - 1)) {
            start -= 1;
        }
        while word(text.byte(end))
            && usize::try_from(end - start).is_ok_and(|n| n < super::dialogs::FIELD_LEN)
        {
            end += 1;
        }
        text.read(start..end)
    }

    fn display_dialog(&mut self) -> Action {
        let mut dialog = Dialog::open(Kind::Display);
        let colour = self.settings.colour;
        dialog.pick(0, u16::from(colour & 0x0F), 6);
        dialog.pick(1, u16::from((colour >> 4) & 0x07), 6);
        if self.settings.bars {
            dialog.checks = check::SCROLL_BARS;
        }
        dialog.fields[0].set(alloc::format!("{}", self.settings.tab).as_bytes());
        self.open_dialog(&dialog)
    }

    /// What a dialog's key or click came to.
    pub(super) fn outcome<S: Store>(&mut self, outcome: Outcome, text: &mut Text<'_, S>) -> Action {
        let kind = self.dialog.kind;
        match (kind, outcome) {
            (_, Outcome::Kept) | (Kind::Display, Outcome::Picked(_) | Outcome::Taken(_)) => {
                Action::Wait
            }
            (_, Outcome::Pressed(Button::Help)) => self.dialog_help(),
            (Kind::Open | Kind::SaveAs, _) => self.file_outcome(outcome, text),
            (Kind::Find, Outcome::Pressed(Button::Ok)) => {
                self.take_search();
                self.close_dialog(text);
                if self.search.find.len == 0 {
                    return Action::Wait;
                }
                self.find_next(text)
            }
            (Kind::Change, Outcome::Pressed(Button::Verify | Button::ChangeAll)) => {
                self.take_search();
                self.close_dialog(text);
                if self.search.find.len == 0 {
                    return Action::Wait;
                }
                let from = self.edit(text).cursor_offset();
                self.changing = Changing {
                    limit: from,
                    ..Changing::default()
                };
                if outcome == Outcome::Pressed(Button::ChangeAll) {
                    return self.change_all(from, text);
                }
                self.next_change(from, text)
            }
            (Kind::Verify, Outcome::Pressed(Button::Change)) => {
                if let Err(full) = self.change_selected(text) {
                    return self.failed(full, text);
                }
                let from = self.edit(text).cursor_offset();
                self.next_change(from, text)
            }
            (Kind::Verify, Outcome::Pressed(Button::Skip)) => {
                let from = self.edit(text).cursor_offset();
                self.next_change(from, text)
            }
            (Kind::Print, Outcome::Pressed(Button::Ok)) => {
                let selection = self.dialog.choice == 0 && self.edit(text).selection().is_some();
                self.close_dialog(text);
                Action::Print { selection }
            }
            (Kind::Display, Outcome::Pressed(Button::Ok)) => self.apply_display(text),
            (Kind::Unsaved, Outcome::Pressed(Button::Yes)) => self.save(),
            (Kind::Unsaved, Outcome::Pressed(Button::No)) => {
                let then = self.then;
                self.close_dialog(text);
                self.carry_on(then, text)
            }
            (Kind::Overwrite, Outcome::Pressed(Button::Yes)) => {
                self.close_dialog(text);
                Action::Save
            }
            (Kind::Overwrite, Outcome::Pressed(_)) => {
                let mut dialog = Dialog::open(Kind::SaveAs);
                dialog.fields[0].set(self.target.text());
                self.open_dialog(&dialog);
                Action::List
            }
            _ => {
                if kind == Kind::Unsaved {
                    self.then = Then::Nothing;
                }
                self.close_dialog(text);
                Action::Wait
            }
        }
    }

    /// What Open's or Save As's key or click came to.
    fn file_outcome<S: Store>(&mut self, outcome: Outcome, text: &mut Text<'_, S>) -> Action {
        match outcome {
            Outcome::Picked(Source::Files) => {
                let pick = self.dialog.picks[0];
                let name = self.listed_name(Source::Files, pick, text);
                self.dialog.fields[0].set(&name);
                Action::Wait
            }
            Outcome::Taken(source) => {
                let pick = self.dialog.picks[super::dialogs::list_index(source)];
                let name = self.listed_name(source, pick, text);
                self.dialog.fields[0].set(&name);
                self.submit(text)
            }
            Outcome::Pressed(Button::Ok) => self.submit(text),
            Outcome::Pressed(_) => {
                self.then = Then::Nothing;
                self.close_dialog(text);
                Action::Wait
            }
            Outcome::Picked(_) | Outcome::Kept => Action::Wait,
        }
    }

    /// A listed entry as a name DOS takes: a drive's `[-C-]` as `C:`.
    fn listed_name<S: Store>(&mut self, source: Source, n: u16, text: &mut Text<'_, S>) -> Vec<u8> {
        let shown = self.entries(text).entry(source, n);
        match shown.as_slice() {
            [b'[', b'-', letter, b'-', b']'] => alloc::vec![*letter, b':'],
            _ => shown,
        }
    }

    /// The name in the file dialog's field given to the program.
    fn submit<S: Store>(&mut self, text: &mut Text<'_, S>) -> Action {
        let name = self.dialog.fields[0].text();
        if name.is_empty() {
            return Action::Wait;
        }
        if self.dialog.kind == Kind::SaveAs {
            if is_pattern(name) {
                return self.failed(failure::BAD_NAME, text);
            }
            return Action::SaveAs;
        }
        Action::Open
    }

    fn take_search(&mut self) {
        self.search.find = self.dialog.fields[0];
        self.search.change = self.dialog.fields[1];
        self.search.flags = self.dialog.checks;
    }

    /// The bytes from `start` to `end` selected, the cursor at the end, the
    /// window over it.
    fn select<S: Store>(&mut self, start: u32, end: u32, text: &mut Text<'_, S>) {
        let mut edit = self.edit(text);
        let first = edit.spot_at(start);
        let last = edit.spot_near(end, first);
        edit.editor.anchor = Some(first);
        edit.editor.cursor = last;
        edit.editor.goal = last.column;
        edit.show_cursor();
    }

    fn needle(&self) -> (Vec<u8>, bool, bool) {
        let flags = self.search.flags;
        (
            self.search.find.text().to_vec(),
            flags & check::MATCH_CASE != 0,
            flags & check::WHOLE_WORD != 0,
        )
    }

    /// The last search's text found again from the cursor, round the
    /// document's end.
    fn find_next<S: Store>(&mut self, text: &mut Text<'_, S>) -> Action {
        let (needle, exact, whole) = self.needle();
        let from = self.edit(text).cursor_offset();
        match text.find(&needle, from, exact, whole) {
            Some(at) => {
                self.select(at, at + u32::try_from(needle.len()).unwrap_or(0), text);
                Action::Wait
            }
            None => self.message(said::NOT_FOUND, text),
        }
    }

    /// The next match a change meets from `from`: to the document's end,
    /// then from its start to where the change began.
    fn next_match<S: Store>(&mut self, from: u32, text: &mut Text<'_, S>) -> Option<u32> {
        let (needle, exact, whole) = self.needle();
        if !self.changing.wrapped {
            let len = text.len();
            if let Some(at) = text.find_in(&needle, from..len, exact, whole) {
                return Some(at);
            }
            self.changing.wrapped = true;
            return text.find_in(&needle, 0..self.changing.limit, exact, whole);
        }
        text.find_in(&needle, from..self.changing.limit, exact, whole)
    }

    /// The selection, a match, changed to the search's replacement, the
    /// change's end moved with the text before it.
    fn change_selected<S: Store>(&mut self, text: &mut Text<'_, S>) -> Result<(), u8> {
        let replacement = self.search.change.text().to_vec();
        let found = self.search.find.len;
        self.edit(text)
            .type_in(&replacement)
            .map_err(|_| failure::MEMORY)?;
        if self.changing.wrapped {
            let grown = i64::from(self.changing.limit)
                + i64::try_from(replacement.len()).unwrap_or(0)
                - i64::from(found);
            self.changing.limit = u32::try_from(grown.max(0)).unwrap_or(0);
        }
        self.changing.changed = self.changing.changed.saturating_add(1);
        Ok(())
    }

    /// The next match shown and asked about, or the change's end said.
    fn next_change<S: Store>(&mut self, from: u32, text: &mut Text<'_, S>) -> Action {
        let Some(at) = self.next_match(from, text) else {
            let said = if self.changing.found {
                said::CHANGED
            } else {
                said::NOT_FOUND
            };
            return self.message(said, text);
        };
        self.changing.found = true;
        self.select(at, at + u32::from(self.search.find.len), text);
        self.open_dialog(&Dialog::open(Kind::Verify))
    }

    /// Every match changed without asking.
    fn change_all<S: Store>(&mut self, mut from: u32, text: &mut Text<'_, S>) -> Action {
        while let Some(at) = self.next_match(from, text) {
            self.changing.found = true;
            self.select(at, at + u32::from(self.search.find.len), text);
            if let Err(full) = self.change_selected(text) {
                return self.failed(full, text);
            }
            from = self.edit(text).cursor_offset();
        }
        let said = if self.changing.found {
            said::CHANGED
        } else {
            said::NOT_FOUND
        };
        self.message(said, text)
    }

    /// Display's choices made the editor's, each place kept over the bytes
    /// it stood at as the tab stops change.
    fn apply_display<S: Store>(&mut self, text: &mut Text<'_, S>) -> Action {
        let field = self.dialog.fields[0].text();
        let tab = core::str::from_utf8(field)
            .ok()
            .and_then(|digits| digits.trim().parse::<u8>().ok())
            .filter(|tab| (1..=99).contains(tab));
        let Some(tab) = tab else {
            return self.message(said::TABS, text);
        };
        let [foreground, background] = self.dialog.picks;
        let colour = (u8::try_from(background & 0x07).unwrap_or(0) << 4)
            | u8::try_from(foreground & 0x0F).unwrap_or(0);
        self.settings = Settings {
            colour,
            bars: self.dialog.checks & check::SCROLL_BARS != 0,
            tab,
        };
        let mut edit = self.edit(text);
        let cursor = edit.editor.cursor;
        let at = edit.offset_of(cursor);
        let anchor = edit
            .editor
            .anchor
            .map(|anchor| (anchor, edit.offset_of(anchor)));
        edit.editor.tab = tab;
        let line_start = |spot: Spot| Spot { column: 0, ..spot };
        let cursor = edit.spot_near(at, line_start(cursor));
        edit.editor.cursor = cursor;
        edit.editor.goal = cursor.column;
        let anchor = anchor.map(|(anchor, at)| edit.spot_near(at, line_start(anchor)));
        edit.editor.anchor = anchor;
        edit.editor.left = 0;
        edit.show_cursor();
        self.close_dialog(text);
        Action::Settings
    }

    /// The clipboard's bytes.
    fn clipboard<S: Store>(&self, text: &mut Text<'_, S>) -> Vec<u8> {
        let mut bytes = alloc::vec![0u8; usize::try_from(self.clip).unwrap_or(0)];
        text.read_end(self.clip, &mut bytes);
        bytes
    }

    /// `bytes` made the clipboard, at the store's end, the document's room
    /// made the less for it; the clipboard as it was where they do not fit.
    pub(super) fn keep<S: Store>(&mut self, bytes: &[u8], text: &mut Text<'_, S>) -> Action {
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let capacity = text
            .store_len()
            .checked_sub(len)
            .and_then(|left| left.checked_sub(self.listing_len()));
        if capacity.is_none_or(|capacity| text.resize(capacity).is_err()) {
            return self.failed(failure::MEMORY, text);
        }
        self.clip = len;
        text.write_end(len, bytes);
        Action::Wait
    }

    /// The bytes a dialog's listing takes below the clipboard.
    fn listing_len(&self) -> u32 {
        (u32::from(self.files) + u32::from(self.others))
            .saturating_mul(u32::try_from(entry::LEN).unwrap_or(u32::MAX))
    }

    /// A listing begun: the document's gap closed so all it leaves is the
    /// listing's; the entries it has room for.
    pub fn begin_listing<S: Store>(&mut self, text: &mut Text<'_, S>) -> u16 {
        self.release_listing(text);
        let len = text.len();
        if text.resize(len).is_err() {
            return 0;
        }
        let room = text
            .store_len()
            .saturating_sub(self.clip)
            .saturating_sub(len);
        u16::try_from(room / u32::try_from(entry::LEN).unwrap_or(u32::MAX)).unwrap_or(u16::MAX)
    }

    /// The listing of `count` entries finished: sorted, files first, then
    /// `..`, the directories and the drives, and the document's gap given
    /// back all the listing does not take.
    pub fn end_listing<S: Store>(&mut self, text: &mut Text<'_, S>, count: u16) {
        let mut entries: Vec<[u8; entry::LEN]> = (0..count)
            .map(|n| {
                let mut raw = [0u8; entry::LEN];
                text.read_end(entry_back(self.clip, n), &mut raw);
                raw
            })
            .collect();
        entries.sort_unstable_by_key(|raw| (raw[0], !is_parent(raw), *raw));
        for (n, raw) in (0u16..).zip(&entries) {
            text.write_end(entry_back(self.clip, n), raw);
        }
        self.files =
            u16::try_from(entries.iter().filter(|raw| raw[0] == entry::FILE).count()).unwrap_or(0);
        self.others = count - self.files;
        let capacity = text
            .store_len()
            .saturating_sub(self.clip)
            .saturating_sub(self.listing_len());
        let _ = text.resize(capacity);
    }

    /// Any listing given back to the document.
    pub fn release_listing<S: Store>(&mut self, text: &mut Text<'_, S>) {
        self.files = 0;
        self.others = 0;
        let capacity = text.store_len().saturating_sub(self.clip);
        let _ = text.resize(capacity);
    }

    /// What came of the last action.
    pub fn done<S: Store>(&mut self, done: Done, text: &mut Text<'_, S>) -> Action {
        match done {
            Done::Listed => {
                self.dialog.tops = [0, 0];
                self.dialog.picks = [0, 0];
                let offered = if self.dialog.kind == Kind::Open {
                    self.pattern.text().to_vec()
                } else if self.name.len > 0 {
                    self.title()
                } else {
                    Vec::new()
                };
                self.dialog.fields[0].set(&offered);
                self.dialog.fields[0].fresh = true;
                Action::Wait
            }
            Done::Loaded => {
                self.close_dialog(text);
                self.loaded(text);
                Action::Wait
            }
            Done::Exists => self.open_dialog(&Dialog::open(Kind::Overwrite)),
            Done::Saved => {
                self.close_dialog(text);
                self.editor.modified = false;
                let then = self.then;
                self.carry_on(then, text)
            }
            Done::Printed | Done::Stored => Action::Wait,
            Done::Failed(code) => self.failed(code, text),
        }
    }
}

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
