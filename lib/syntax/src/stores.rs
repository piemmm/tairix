//! The TAIRiX settings stores, each read through its own crate's line
//! grammar and, where the store has one, its registry of keys: an unknown key
//! or a value outside its key's set is coloured as the error it is while it is
//! being typed.

use tairix_appconf::{line_shape, LineShape};
use tairix_fontface::{manifest_line, ManifestLine};
use tairix_netconfig::{valid_iface_name, IfaceKey};
use tairix_sysconfig::{Key, SystemConfig};
use tairix_theme::SyntaxRole;
use tairix_users::{
    GroupRecord, GroupsDb, RecordLine, UserRecord, UsersDb, FIELD_SEPARATOR, FORMAT_HEADER,
    GROUPS_FORMAT_HEADER,
};
use tairix_util::conf::{comment_at, setting_line, SettingLine, ValueShape};

use crate::clike::quoted;
use crate::lex::{Emit, LineState};

/// Colour a whole line the stores refuse to read at all: it is not UTF-8.
fn text<'a>(line: &'a [u8], out: &mut Emit<'_>) -> Option<&'a str> {
    let text = core::str::from_utf8(line).ok();
    if text.is_none() {
        out.push(0, line.len(), SyntaxRole::Error);
    }
    text
}

/// A `lib/appconf` document: an application's settings, or the program
/// library catalog.
pub(crate) fn appconf(line: &[u8], out: &mut Emit<'_>) -> LineState {
    let Some(raw) = text(line, out) else {
        return LineState::START;
    };
    match line_shape(raw) {
        LineShape::Blank => {}
        LineShape::Comment { at } => out.push(at, line.len(), SyntaxRole::Comment),
        LineShape::Setting {
            key,
            separator,
            value,
            comment,
        } => {
            out.push(key.start, key.end, SyntaxRole::Key);
            out.push(separator, separator + 1, SyntaxRole::Punctuation);
            if line.get(value.start) == Some(&b'"') {
                out.push(value.start, value.start + 1, SyntaxRole::String);
                quoted(&line[..value.end], value.start + 1, b'"', out);
            } else {
                out.push(value.start, value.end, SyntaxRole::String);
            }
            if let Some(at) = comment {
                out.push(at, line.len(), SyntaxRole::Comment);
            }
        }
        LineShape::Unparsed(_) => out.push(0, line.len(), SyntaxRole::Error),
    }
    LineState::START
}

/// Colour a `key value` line: a setting with a value through `classify`,
/// then its comment, left to right.
fn key_value(
    line: &[u8],
    out: &mut Emit<'_>,
    classify: fn(&SettingLine<'_>, &str, &mut Emit<'_>),
) -> LineState {
    let Some(raw) = text(line, out) else {
        return LineState::START;
    };
    if let Some(found) = setting_line(raw) {
        match found.value {
            Some(value) => classify(&found, value, out),
            // Every `key value` store refuses a key with nothing after it.
            None => out.push(found.key_at, key_end(&found), SyntaxRole::Error),
        }
    }
    if let Some(at) = comment_at(raw) {
        out.push(at, raw.len(), SyntaxRole::Comment);
    }
    LineState::START
}

/// Where `setting`'s key ends in its line.
fn key_end(setting: &SettingLine<'_>) -> usize {
    setting.key_at + setting.key.len()
}

/// Colour `value`, `setting`'s value, as `role`.
fn push_value(setting: &SettingLine<'_>, value: &str, role: SyntaxRole, out: &mut Emit<'_>) {
    out.push(setting.value_at, setting.value_at + value.len(), role);
}

/// The role of a value its key admitted or refused.
fn value_role(admitted: bool, shape: ValueShape) -> SyntaxRole {
    match (admitted, shape) {
        (false, _) => SyntaxRole::Error,
        (true, ValueShape::Closed(_)) => SyntaxRole::Keyword,
        (true, ValueShape::Free(_)) => SyntaxRole::String,
    }
}

/// The boot-time system configuration store.
pub(crate) fn system_config(line: &[u8], out: &mut Emit<'_>) -> LineState {
    key_value(line, out, |found, value, out| {
        let Some(key) = Key::from_name(found.key) else {
            out.push(found.key_at, key_end(found), SyntaxRole::Error);
            return;
        };
        out.push(found.key_at, key_end(found), SyntaxRole::Key);
        let admitted = SystemConfig::default().set(key, value).is_ok();
        push_value(found, value, value_role(admitted, key.shape()), out);
    })
}

/// The network configuration store: `<interface>.<key> value`.
pub(crate) fn network_config(line: &[u8], out: &mut Emit<'_>) -> LineState {
    key_value(line, out, |found, value, out| {
        let key = found.key.split_once('.').and_then(|(iface, suffix)| {
            valid_iface_name(iface).then_some(())?;
            IfaceKey::from_name(suffix).map(|key| (iface.len(), key))
        });
        let Some((iface_len, key)) = key else {
            out.push(found.key_at, key_end(found), SyntaxRole::Error);
            return;
        };
        let dot = found.key_at + iface_len;
        out.push(found.key_at, dot, SyntaxRole::Key);
        out.push(dot, dot + 1, SyntaxRole::Punctuation);
        out.push(dot + 1, key_end(found), SyntaxRole::Attribute);
        push_value(
            found,
            value,
            value_role(key.admits(value), key.shape()),
            out,
        );
    })
}

/// The service enrolment overrides: `<service> enabled|disabled`.
pub(crate) fn service_overrides(line: &[u8], out: &mut Emit<'_>) -> LineState {
    key_value(line, out, |found, value, out| {
        let name_role = if tairix_enrolment::validate_service_name(found.key).is_ok() {
            SyntaxRole::Key
        } else {
            SyntaxRole::Error
        };
        out.push(found.key_at, key_end(found), name_role);
        let role = if tairix_abi::ServiceEnrolment::from_name(value).is_some() {
            SyntaxRole::Keyword
        } else {
            SyntaxRole::Error
        };
        push_value(found, value, role, out);
    })
}

/// The records follow the header line.
const RECORDS: u32 = 1;

/// One of the account databases, as its own crate reads it.
struct Database {
    header: &'static str,
    /// Each field's role, in record order.
    fields: &'static [SyntaxRole],
    line: fn(&str) -> RecordLine<'_>,
    valid: fn(&str) -> bool,
}

/// Colour a line of `db`: its header, then one record per line.
fn database(db: &Database, state: LineState, line: &[u8], out: &mut Emit<'_>) -> LineState {
    let Some(raw) = text(line, out) else {
        return LineState::from_raw(RECORDS);
    };
    if state.raw() != RECORDS {
        // The parser reads lines as `str::lines` splits them, which drops
        // the `\r` of a CRLF ending.
        let role = if raw.strip_suffix('\r').unwrap_or(raw) == db.header {
            SyntaxRole::Directive
        } else {
            SyntaxRole::Error
        };
        out.push(0, line.len(), role);
        return LineState::from_raw(RECORDS);
    }
    match (db.line)(raw) {
        RecordLine::Blank => {}
        RecordLine::Comment { at } => out.push(at, line.len(), SyntaxRole::Comment),
        RecordLine::Record { at, text } if (db.valid)(text) => {
            let mut at = at;
            for (index, field) in text.split(FIELD_SEPARATOR).enumerate() {
                if index > 0 {
                    out.push(at, at + FIELD_SEPARATOR.len_utf8(), SyntaxRole::Punctuation);
                    at += FIELD_SEPARATOR.len_utf8();
                }
                let role = db.fields.get(index).copied().unwrap_or(SyntaxRole::Error);
                out.push(at, at + field.len(), role);
                at += field.len();
            }
        }
        RecordLine::Record { .. } | RecordLine::TooLong => {
            out.push(0, line.len(), SyntaxRole::Error);
        }
    }
    LineState::from_raw(RECORDS)
}

/// The users database, one `lib/users` record per line.
pub(crate) fn users(state: LineState, line: &[u8], out: &mut Emit<'_>) -> LineState {
    const USERS: Database = Database {
        header: FORMAT_HEADER,
        fields: &[
            SyntaxRole::Key,
            SyntaxRole::Number,
            SyntaxRole::Number,
            SyntaxRole::Number,
            SyntaxRole::String,
            SyntaxRole::Link,
            SyntaxRole::Link,
            SyntaxRole::Keyword,
            SyntaxRole::Keyword,
            SyntaxRole::Comment,
        ],
        line: UsersDb::line,
        valid: |record| UserRecord::decode_line(record).is_ok(),
    };
    database(&USERS, state, line, out)
}

/// The groups database, one `lib/users` group record per line.
pub(crate) fn groups(state: LineState, line: &[u8], out: &mut Emit<'_>) -> LineState {
    const GROUPS: Database = Database {
        header: GROUPS_FORMAT_HEADER,
        fields: &[SyntaxRole::Key, SyntaxRole::Number],
        line: GroupsDb::line,
        valid: |record| GroupRecord::decode_line(record).is_ok(),
    };
    database(&GROUPS, state, line, out)
}

/// A font family manifest, read with `lib/fontface`'s own line reader.
pub(crate) fn font_family(line: &[u8], out: &mut Emit<'_>) -> LineState {
    let Some(raw) = text(line, out) else {
        return LineState::START;
    };
    match manifest_line(raw) {
        ManifestLine::Blank => {}
        ManifestLine::Comment { at } => out.push(at, line.len(), SyntaxRole::Comment),
        ManifestLine::Field { key, value } => {
            out.push(key.start, key.end, SyntaxRole::Key);
            if let Some(separator) = raw.get(key.end..value.start).and_then(|gap| gap.find('=')) {
                let at = key.end + separator;
                out.push(at, at + 1, SyntaxRole::Punctuation);
            }
            out.push(value.start, value.end, SyntaxRole::String);
        }
        ManifestLine::Malformed => out.push(0, line.len(), SyntaxRole::Error),
    }
    LineState::START
}
