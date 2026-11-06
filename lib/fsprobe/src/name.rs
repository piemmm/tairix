//! The catalog-naming policy (`docs/src/drivers/block.md`): the one the
//! volume manager names a recognised volume by and the kernel names a network
//! or disk-image volume by (`plans/FSSERVE.md` §6, `plans/DISK-IMAGES.md`).
//!
//! A volume is published under `/Storage/<name>`, a name derived from the
//! volume itself, never from its format or from what else is attached:
//!
//! 1. its **label**, read where its format keeps it, in kebab case: ASCII
//!    letters lowercased, digits kept, each run of anything else one `-`,
//!    and `-` trimmed from both ends, so `My Backup Disk` is
//!    `my-backup-disk`;
//! 2. with no usable label, `<transport>_<fingerprint>`: the bus its device
//!    is reached over and the start of its identity's fingerprint
//!    (`usb_25cm`), so no unlabelled volume's name depends on another's;
//! 3. a label already in use, or one spelling a name the storage namespace
//!    or the resource grammar already defines, takes `_<fingerprint>` of the
//!    volume's own identity (`backup_k3x9`), and a generated name still in
//!    use is lengthened to more of the fingerprint;
//! 4. a twin, a volume carrying the identity of one shown already, asking
//!    the name that volume holds is shown under it with `_<n>` (`backup_2`),
//!    the kernel taking the lowest `n` free ([`twin`]).
//!
//! A label never yields `_`, so no label can spell a name generated for
//! another volume. Every candidate satisfies the kernel's volume-name grammar
//! (`tairix_abi::volume::validate_volume_name`) by construction; the kernel
//! still re-validates it.

use crate::{fingerprint, FINGERPRINT_CHARS};
use tairix_abi::blkio::BlkTransport;
use tairix_abi::fs::ROOT_VIEW;
use tairix_abi::volume::{validate_volume_name, VOLUME_NAME_MAX};
use tairix_path::RESOLVER_NAMES;
use tairix_resref::KnownNamespace;

/// A catalog name: a short ASCII string in the volume-name grammar, held
/// inline, so the policy allocates nothing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VolumeName {
    bytes: [u8; VOLUME_NAME_MAX],
    len: usize,
}

impl VolumeName {
    const EMPTY: Self = Self {
        bytes: [0; VOLUME_NAME_MAX],
        len: 0,
    };

    /// The name's bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Append `byte`, dropped once the name is at the grammar's bound.
    fn push(&mut self, byte: u8) {
        if let Some(slot) = self.bytes.get_mut(self.len) {
            *slot = byte;
            self.len += 1;
        }
    }

    /// Cut the name back to its first `len` bytes, and then to its last
    /// letter or digit, so a cut never leaves it ending in a separator.
    fn truncate(&mut self, len: usize) {
        let mut kept = len.min(self.len);
        while kept > 0 && !self.bytes[kept - 1].is_ascii_alphanumeric() {
            kept -= 1;
        }
        self.len = kept;
    }
}

/// What a volume's name is made from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NameBase {
    /// Its label, in kebab case.
    Label(VolumeName),
    /// It has no usable label; its device is reached over this transport,
    /// or declared none.
    Unlabelled(Option<BlkTransport>),
}

impl NameBase {
    /// The base of a volume labelled `label` on a device reached over
    /// `transport`.
    #[must_use]
    pub fn of(label: &[u8], transport: Option<BlkTransport>) -> Self {
        kebab(label).map_or(Self::Unlabelled(transport), Self::Label)
    }
}

/// Fingerprint characters a generated name carries, shortest first; the last
/// is the whole fingerprint, which no two identities share.
const SUFFIX_LENGTHS: [usize; 3] = [4, 8, FINGERPRINT_CHARS];

/// The names a volume of `base` and `identity` may be published under, in
/// the order they are tried: its bare label, unless that is reserved, then
/// its stem with ever more of its fingerprint.
pub fn candidates(base: NameBase, identity: [u8; 16]) -> impl Iterator<Item = VolumeName> {
    let (plain, stem) = match base {
        NameBase::Label(label) => ((!reserved(label.as_bytes())).then_some(label), label),
        NameBase::Unlabelled(transport) => (None, word(transport_word(transport))),
    };
    let print = fingerprint(&identity);
    plain.into_iter().chain(
        SUFFIX_LENGTHS
            .into_iter()
            .map(move |take| suffixed(stem, &print[..take])),
    )
}

/// Whether `name` is one a volume may be given by choice rather than by the
/// policy: a label's own kebab-case spelling, and no reserved word, so a
/// chosen name never takes one the policy generates for another volume.
#[must_use]
pub fn chosen(name: &[u8]) -> bool {
    kebab(name).is_some_and(|label| label.as_bytes() == name) && !reserved(name)
}

/// The name a twin of the volume shown as `name` is shown under: `name`, cut
/// to leave room, then `_` and `ordinal` in decimal (`backup_2`). A label
/// never yields `_` and a fingerprint suffix runs to four characters or more,
/// so no label or fingerprint suffix spells it. `None` for a `name` outside
/// the kernel's volume-name grammar.
#[must_use]
pub fn twin(name: &[u8], ordinal: u8) -> Option<VolumeName> {
    validate_volume_name(name).ok()?;
    let mut digits = [0u8; 3];
    let mut start = digits.len();
    let mut left = ordinal;
    loop {
        start -= 1;
        digits[start] = b'0' + left % 10;
        left /= 10;
        if left == 0 {
            break;
        }
    }
    Some(suffixed(word(name), &digits[start..]))
}

/// `label` in kebab case, or `None` when no ASCII letter or digit survives.
fn kebab(label: &[u8]) -> Option<VolumeName> {
    let mut name = VolumeName::EMPTY;
    let mut gap = false;
    for byte in label.iter().map(u8::to_ascii_lowercase) {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            if gap && name.len > 0 {
                name.push(b'-');
            }
            gap = false;
            name.push(byte);
        } else {
            gap = true;
        }
    }
    name.truncate(VOLUME_NAME_MAX);
    (name.len > 0).then_some(name)
}

/// `stem` cut to leave room for `_` and `print`, then both appended.
fn suffixed(mut stem: VolumeName, print: &[u8]) -> VolumeName {
    stem.truncate(VOLUME_NAME_MAX - 1 - print.len());
    stem.push(b'_');
    for &byte in print {
        stem.push(byte);
    }
    stem
}

/// The word an unlabelled volume's name begins with: the bus its device is
/// reached over.
const fn transport_word(transport: Option<BlkTransport>) -> &'static [u8] {
    match transport {
        Some(BlkTransport::Ata) => b"ata",
        Some(BlkTransport::Nvme) => b"nvme",
        Some(BlkTransport::Usb) => b"usb",
        Some(BlkTransport::Sd) => b"sd",
        Some(BlkTransport::Virtual) => b"virtual",
        None => b"disk",
    }
}

fn word(bytes: &[u8]) -> VolumeName {
    let mut name = VolumeName::EMPTY;
    for &byte in bytes {
        name.push(byte);
    }
    name
}

/// Whether `name` already names something a reader would take it for: a
/// root-view entry and the machine alias of the same name, a storage
/// resolver, or a resource namespace.
fn reserved(name: &[u8]) -> bool {
    let Ok(name) = core::str::from_utf8(name) else {
        return false;
    };
    ROOT_VIEW
        .iter()
        .any(|entry| entry.eq_ignore_ascii_case(name))
        || RESOLVER_NAMES.contains(&name)
        || KnownNamespace::from_name(name).is_some()
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    fn names(base: NameBase, identity: [u8; 16]) -> Vec<Vec<u8>> {
        candidates(base, identity)
            .map(|name| name.as_bytes().to_vec())
            .collect()
    }

    fn label(text: &[u8]) -> NameBase {
        NameBase::of(text, Some(BlkTransport::Usb))
    }

    fn print(identity: [u8; 16], take: usize) -> Vec<u8> {
        fingerprint(&identity)[..take].to_vec()
    }

    #[test]
    fn a_label_is_named_in_kebab_case_with_its_word_boundaries() {
        for (text, name) in [
            (&b"My Backup Disk"[..], &b"my-backup-disk"[..]),
            (b"__Data-2024__", b"data-2024"),
            (b"a - b", b"a-b"),
            (b"Photos 2024!", b"photos-2024"),
            (b"caf\xC3\xA9 menu", b"caf-menu"),
            (b"MY_DISK", b"my-disk"),
        ] {
            assert_eq!(names(label(text), [1; 16])[0], name, "{text:?}");
        }
    }

    #[test]
    fn a_label_with_no_letter_or_digit_is_no_label() {
        for text in [&b""[..], b"!!! ***", b"----", "διακοπές".as_bytes()] {
            assert_eq!(label(text), NameBase::Unlabelled(Some(BlkTransport::Usb)));
        }
    }

    #[test]
    fn an_unlabelled_volume_is_named_by_its_transport_and_identity_alone() {
        let first = [0x11; 16];
        let second = [0x22; 16];
        let unlabelled = NameBase::Unlabelled(Some(BlkTransport::Usb));
        let mut expected = b"usb_".to_vec();
        expected.extend(print(first, 4));
        assert_eq!(names(unlabelled, first)[0], expected);
        assert_ne!(
            names(unlabelled, first)[0],
            names(unlabelled, second)[0],
            "neither waits on the other's name, in either order"
        );
        assert!(names(NameBase::Unlabelled(None), first)[0].starts_with(b"disk_"));
        assert!(names(NameBase::Unlabelled(Some(BlkTransport::Sd)), first)[0].starts_with(b"sd_"));
    }

    #[test]
    fn a_taken_label_takes_its_own_fingerprint_lengthening_while_taken() {
        let identity = [7; 16];
        let tried = names(label(b"Backup"), identity);
        assert_eq!(tried.len(), 1 + SUFFIX_LENGTHS.len());
        assert_eq!(tried[0], b"backup");
        for (name, take) in tried[1..].iter().zip(SUFFIX_LENGTHS) {
            assert!(name.ends_with(&print(identity, take)), "{name:?}");
            assert!(name.starts_with(b"back"));
            assert_eq!(name[name.len() - take - 1], b'_');
        }
        assert_eq!(names(label(b"Backup"), identity), tried, "always the same");
        assert_ne!(names(label(b"Backup"), [8; 16])[1], tried[1]);
    }

    #[test]
    fn a_reserved_label_is_never_published_bare() {
        for text in [
            &b"System"[..],
            b"STORAGE",
            b"users",
            b"id",
            b"alias",
            b"dev",
            b"view",
            b"disk",
            b"vol",
            b"cap",
        ] {
            let tried = names(label(text), [3; 16]);
            assert_eq!(tried.len(), SUFFIX_LENGTHS.len(), "{text:?}");
            assert!(tried.iter().all(|name| name.contains(&b'_')), "{text:?}");
        }
        assert_eq!(names(label(b"Systems"), [3; 16])[0], b"systems");
    }

    #[test]
    fn a_hostile_label_cannot_spell_a_generated_name_or_escape() {
        let victim = [0x5C; 16];
        let generated = names(NameBase::Unlabelled(Some(BlkTransport::Usb)), victim);
        for name in &generated {
            let forged = names(label(name), [0x99; 16]);
            assert!(!forged.contains(name), "{name:?}");
        }
        for text in [&b"../../System"[..], b"a/b", b"x\x1b[2Jy", b"\0\0"] {
            for name in names(label(text), [1; 16]) {
                assert!(validate_volume_name(&name).is_ok(), "{name:?}");
                assert!(!name.contains(&b'/') && !name.contains(&b'.'));
            }
        }
    }

    #[test]
    fn every_candidate_meets_the_kernel_grammar_even_at_the_bound() {
        let long = [b'a'; 100];
        let separated: Vec<u8> = (0..40).flat_map(|_| *b"ab ").collect();
        for base in [
            label(&long),
            label(&separated),
            label(b"x"),
            NameBase::Unlabelled(Some(BlkTransport::Virtual)),
            NameBase::Unlabelled(None),
        ] {
            for name in names(base, [0xA5; 16]) {
                assert!(validate_volume_name(&name).is_ok(), "{name:?}");
            }
        }
        let last = names(label(&long), [0xA5; 16]).pop().expect("a candidate");
        assert!(last.ends_with(&print([0xA5; 16], FINGERPRINT_CHARS)));
    }

    #[test]
    fn a_name_is_chosen_only_as_a_label_would_be_given_it() {
        for name in [&b"work"[..], b"my-backup-disk", b"disk2"] {
            assert!(chosen(name), "{name:?}");
        }
        let victim = [0x5C; 16];
        for generated in names(label(b"backup"), victim)
            .into_iter()
            .skip(1)
            .chain(names(NameBase::Unlabelled(Some(BlkTransport::Usb)), victim))
        {
            assert!(!chosen(&generated), "{generated:?} is generated");
        }
        for name in [&b"system"[..], b"users", b"storage", b"id", b"alias"] {
            assert!(!chosen(name), "{name:?} is reserved");
        }
        for name in [&b"my--disk"[..], b"-disk", b"Disk", b"disk_", b""] {
            assert!(!chosen(name), "{name:?} is no label's spelling");
        }
    }

    #[test]
    fn a_twin_is_named_beside_the_volume_holding_its_name() {
        let named = |name: &[u8], n| twin(name, n).map(|shown| shown.as_bytes().to_vec());
        assert_eq!(named(b"backup", 2), Some(b"backup_2".to_vec()));
        assert_eq!(named(b"usb_25cm", 16), Some(b"usb_25cm_16".to_vec()));
        assert_eq!(named(b"disk_2", 2), Some(b"disk_2_2".to_vec()));
        assert_eq!(named(b"x", u8::MAX), Some(b"x_255".to_vec()));
        let long = [b'a'; VOLUME_NAME_MAX];
        let mut cut = long[..VOLUME_NAME_MAX - 2].to_vec();
        cut.extend_from_slice(b"_2");
        assert_eq!(named(&long, 2), Some(cut), "cut to the bound");
        let mut separated = long;
        separated[VOLUME_NAME_MAX - 3] = b'-';
        let mut kept = long[..VOLUME_NAME_MAX - 3].to_vec();
        kept.extend_from_slice(b"_2");
        assert_eq!(
            named(&separated, 2),
            Some(kept),
            "never cut to end in a separator"
        );
        for refused in [&b""[..], b"-a", b"a-", b"Disk", b"a/b"] {
            assert_eq!(named(refused, 2), None, "{refused:?}");
        }
        assert_eq!(named("caf\u{e9}".as_bytes(), 2), None, "not ASCII");
    }

    #[test]
    fn a_twin_name_is_spelt_by_no_label_or_fingerprint_suffix() {
        let hostile: [&[u8]; 5] = [
            "\u{dc}n\u{ef}c\u{f8}d\u{e9} Disk".as_bytes(),
            b"Disk_2",
            b"photos 2024",
            &[b'x'; 100],
            "\u{3b4}\u{3b9}\u{3b1}\u{3ba}\u{3bf}\u{3c0}\u{3ad}\u{3c2} 2".as_bytes(),
        ];
        for text in hostile {
            for name in names(label(text), [0x42; 16]) {
                for ordinal in 2..=u8::MAX {
                    let shown = twin(&name, ordinal).expect("every candidate has twin names");
                    let spelt = shown.as_bytes();
                    assert!(validate_volume_name(spelt).is_ok(), "{spelt:?}");
                    assert_ne!(spelt, name.as_slice());
                    assert!(
                        names(label(spelt), [0x42; 16])
                            .iter()
                            .all(|other| other.as_slice() != spelt),
                        "{spelt:?} is no label's"
                    );
                }
            }
        }
        assert!(
            SUFFIX_LENGTHS.iter().all(|&take| take > 3),
            "a fingerprint suffix runs longer than any ordinal"
        );
    }
}
