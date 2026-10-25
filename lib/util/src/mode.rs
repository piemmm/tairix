//! A file mode as a command line spells it, and the bits it gives a node.
//!
//! The one reading of `chmod`'s mode operand, which `mkdir -m` and the shell's
//! `umask` read too: an octal number, or GNU's symbolic grammar
//! `[ugoa]*([-+=]([rwxXst]*|[ugo]))+|[-+=][0-7]+`, clauses separated by commas
//! and applied in turn.

use alloc::vec::Vec;

use tairix_abi::fs::{
    FS_MODE_MASK, FS_PERMISSION_BITS, FS_SETGID_BIT, FS_SETUID_BIT, FS_STICKY_BIT,
};

/// The set-ID bits, which a change leaves as they are on a directory unless
/// it names them.
pub const SET_ID: u32 = FS_SETUID_BIT | FS_SETGID_BIT;

/// `who` bit: the owning user's field.
pub const WHO_USER: u8 = 0b001;
/// `who` bit: the owning group's field.
pub const WHO_GROUP: u8 = 0b010;
/// `who` bit: everyone else's field.
pub const WHO_OTHER: u8 = 0b100;
/// Every field, as `a` names them.
pub const WHO_ALL: u8 = WHO_USER | WHO_GROUP | WHO_OTHER;

/// Permission letter `r`: read.
pub const PERM_READ: u8 = 0b00_0001;
/// Permission letter `w`: write.
pub const PERM_WRITE: u8 = 0b00_0010;
/// Permission letter `x`: execute.
pub const PERM_EXEC: u8 = 0b00_0100;
/// Permission letter `X`: execute for a directory, or for a file some field
/// may already execute.
pub const PERM_COND_EXEC: u8 = 0b00_1000;
/// Permission letter `s`: set-user-ID for the owner, set-group-ID for the
/// group.
pub const PERM_SETID: u8 = 0b01_0000;
/// Permission letter `t`: the sticky bit, for the other field.
pub const PERM_STICKY: u8 = 0b10_0000;

/// A mode operand, read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Mode {
    /// An octal number, whose bits replace the node's.
    Octal {
        /// The mode bits, within `0o7777`.
        bits: u32,
        /// Whether five digits or more spell it, which names a directory's
        /// set-ID bits where fewer leave them as they are.
        names_set_id: bool,
    },
    /// Symbolic changes, applied in turn to the node's bits.
    Symbolic(Vec<Change>),
}

/// One change a symbolic mode makes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Change {
    /// The fields changed, as `WHO_*` bits; `0` when the clause names none,
    /// which changes every field but the bits the umask holds.
    pub who: u8,
    /// How the change meets the node's bits.
    pub op: Op,
    /// What the change applies.
    pub perms: Perms,
}

/// How a [`Change`] meets the bits it finds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Op {
    /// `+`: turn the bits on.
    Add,
    /// `-`: turn the bits off.
    Remove,
    /// `=`: give the fields exactly the bits, clearing the rest of theirs.
    Set,
}

/// What a [`Change`] applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Perms {
    /// Permission letters, as `PERM_*` bits.
    Letters(u8),
    /// The read, write and execute bits one field holds when the change is
    /// made (`g=u`), as one `WHO_*` bit.
    CopyOf(u8),
    /// Mode bits in octal after the operator (`=644`), taken as written: no
    /// umask spares any of them.
    Octal(u32),
}

impl Mode {
    /// Read `spec` as an octal or a symbolic mode, or `None` when it is
    /// neither.
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        if let Some(bits) = octal(spec) {
            return Some(Self::Octal {
                bits,
                names_set_id: spec.len() >= 5,
            });
        }
        let mut changes = Vec::new();
        for clause in spec.split(',') {
            clause_changes(clause, &mut changes)?;
        }
        Some(Self::Symbolic(changes))
    }

    /// The mode bits `self` gives a node holding `current`, a directory when
    /// `dir`, under the creation mask `umask`.
    ///
    /// A clause naming no field spares the bits `umask` holds, as GNU's
    /// `chmod` does; on a directory the set-ID bits the mode does not name
    /// keep their values, so a group-inheriting directory stays one. The
    /// answer is within `0o7777`.
    #[must_use]
    pub fn resolve(&self, current: u32, dir: bool, umask: u32) -> u32 {
        let current = current & FS_MODE_MASK;
        let umask = umask & FS_PERMISSION_BITS;
        let (bits, named) = match self {
            Self::Octal { bits, names_set_id } => {
                (*bits, if *names_set_id { SET_ID } else { bits & SET_ID })
            }
            Self::Symbolic(changes) => {
                changes.iter().fold((current, 0), |(mode, named), change| {
                    (
                        change.apply(mode, dir, umask),
                        named | change.named_set_id(),
                    )
                })
            }
        };
        if !dir {
            return bits;
        }
        let kept = SET_ID & !named;
        (bits & !kept) | (current & kept)
    }
}

impl Change {
    /// What `mode` becomes, on a directory when `dir`, under `umask`.
    fn apply(self, mode: u32, dir: bool, umask: u32) -> u32 {
        let (who, spared) = if self.who == 0 {
            (WHO_ALL, umask)
        } else {
            (self.who, 0)
        };
        let (bits, cleared) = match self.perms {
            Perms::Octal(bits) => (bits, FS_MODE_MASK),
            Perms::Letters(letters) => {
                (letter_bits(letters, who, mode, dir) & !spared, fields(who))
            }
            Perms::CopyOf(field) => (spread(field_bits(mode, field), who) & !spared, fields(who)),
        };
        match self.op {
            Op::Add => mode | bits,
            Op::Remove => mode & !bits,
            Op::Set => (mode & !cleared) | bits,
        }
    }

    /// The set-ID bits this change names, which it may change on a directory.
    fn named_set_id(self) -> u32 {
        match self.perms {
            Perms::Letters(letters) if letters & PERM_SETID != 0 => {
                set_id_bits(if self.who == 0 { WHO_ALL } else { self.who })
            }
            Perms::Octal(_) if self.op == Op::Set => SET_ID,
            Perms::Octal(bits) => bits & SET_ID,
            Perms::Letters(_) | Perms::CopyOf(_) => 0,
        }
    }
}

/// `spec` read as octal digits, when it is only those and spells at most
/// `0o7777`.
fn octal(spec: &str) -> Option<u32> {
    if spec.is_empty() {
        return None;
    }
    spec.bytes().try_fold(0, |value: u32, byte| {
        let value = value * 8 + char::from(byte).to_digit(8)?;
        (value <= FS_MODE_MASK).then_some(value)
    })
}

/// Append the changes `clause` makes to `changes`, or `None` when it makes
/// none or is malformed.
fn clause_changes(clause: &str, changes: &mut Vec<Change>) -> Option<()> {
    let bytes = clause.as_bytes();
    let mut at = 0;
    let mut who = 0;
    while let Some(field) = bytes.get(at).and_then(|&byte| who_bit(byte)) {
        who |= field;
        at += 1;
    }
    let mut made = false;
    while let Some(&byte) = bytes.get(at) {
        let op = match byte {
            b'+' => Op::Add,
            b'-' => Op::Remove,
            b'=' => Op::Set,
            _ => return None,
        };
        at += 1;
        made = true;
        // Octal after an operator names no field and ends its clause.
        if who == 0 && bytes.get(at).is_some_and(u8::is_ascii_digit) {
            let bits = octal(clause.get(at..)?)?;
            changes.push(Change {
                who,
                op,
                perms: Perms::Octal(bits),
            });
            return Some(());
        }
        let perms = if let Some(field) = bytes.get(at).and_then(|&byte| copy_source(byte)) {
            at += 1;
            Perms::CopyOf(field)
        } else {
            let mut letters = 0;
            while let Some(letter) = bytes.get(at).and_then(|&byte| letter_bit(byte)) {
                letters |= letter;
                at += 1;
            }
            Perms::Letters(letters)
        };
        changes.push(Change { who, op, perms });
    }
    made.then_some(())
}

/// The fields a who letter names.
const fn who_bit(byte: u8) -> Option<u8> {
    match byte {
        b'u' => Some(WHO_USER),
        b'g' => Some(WHO_GROUP),
        b'o' => Some(WHO_OTHER),
        b'a' => Some(WHO_ALL),
        _ => None,
    }
}

/// The field whose bits a letter after an operator copies.
const fn copy_source(byte: u8) -> Option<u8> {
    match byte {
        b'u' => Some(WHO_USER),
        b'g' => Some(WHO_GROUP),
        b'o' => Some(WHO_OTHER),
        _ => None,
    }
}

/// The permission a letter after an operator stands for.
const fn letter_bit(byte: u8) -> Option<u8> {
    match byte {
        b'r' => Some(PERM_READ),
        b'w' => Some(PERM_WRITE),
        b'x' => Some(PERM_EXEC),
        b'X' => Some(PERM_COND_EXEC),
        b's' => Some(PERM_SETID),
        b't' => Some(PERM_STICKY),
        _ => None,
    }
}

/// The bits `letters` stand for in the `who` fields of a node holding `mode`.
const fn letter_bits(letters: u8, who: u8, mode: u32, dir: bool) -> u32 {
    let mut triple = 0;
    if letters & PERM_READ != 0 {
        triple |= 0o4;
    }
    if letters & PERM_WRITE != 0 {
        triple |= 0o2;
    }
    if letters & PERM_EXEC != 0 || (letters & PERM_COND_EXEC != 0 && (dir || mode & 0o111 != 0)) {
        triple |= 0o1;
    }
    let mut bits = spread(triple, who);
    if letters & PERM_SETID != 0 {
        bits |= set_id_bits(who);
    }
    if letters & PERM_STICKY != 0 && who & WHO_OTHER != 0 {
        bits |= FS_STICKY_BIT;
    }
    bits
}

/// A field's read, write and execute `triple` in each of the `who` fields.
const fn spread(triple: u32, who: u8) -> u32 {
    let mut bits = 0;
    if who & WHO_USER != 0 {
        bits |= triple << 6;
    }
    if who & WHO_GROUP != 0 {
        bits |= triple << 3;
    }
    if who & WHO_OTHER != 0 {
        bits |= triple;
    }
    bits
}

/// The read, write and execute bits `field` holds in `mode`, as a triple.
const fn field_bits(mode: u32, field: u8) -> u32 {
    let shift = match field {
        WHO_USER => 6,
        WHO_GROUP => 3,
        _ => 0,
    };
    (mode >> shift) & 0o7
}

/// Every bit the `who` fields hold, which `=` clears before it sets.
const fn fields(who: u8) -> u32 {
    let mut bits = 0;
    if who & WHO_USER != 0 {
        bits |= FS_SETUID_BIT | 0o700;
    }
    if who & WHO_GROUP != 0 {
        bits |= FS_SETGID_BIT | 0o070;
    }
    if who & WHO_OTHER != 0 {
        bits |= FS_STICKY_BIT | 0o007;
    }
    bits
}

/// The set-ID bits the `who` fields carry; the other field carries none.
const fn set_id_bits(who: u8) -> u32 {
    let mut bits = 0;
    if who & WHO_USER != 0 {
        bits |= FS_SETUID_BIT;
    }
    if who & WHO_GROUP != 0 {
        bits |= FS_SETGID_BIT;
    }
    bits
}

#[cfg(test)]
#[path = "mode_tests.rs"]
mod tests;
