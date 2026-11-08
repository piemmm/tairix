//! Unit tests for the PDF standard security handler's constructions.
//!
//! The known answers come from an implementation of ISO 32000-2 §7.6.4 written
//! apart from this one — `CPython`'s `hashlib` for MD5 and SHA-2, the
//! `cryptography` package's AES, and an RC4 written from its definition —
//! making each file's `/O`, `/U`, `/OE` and `/UE` from known passwords, so a
//! check here is never this code agreeing with itself.

use super::{
    pdf_aes_decrypt, pdf_legacy_file_key, pdf_modern_file_key, pdf_object_key, pdf_rc4_decrypt,
    PdfKey, PdfLegacyEntries, PdfModernEntries, PdfUnusable,
};
use crate::test_hex::hex;

const ID: &str = "0123456789abcdef0123456789abcdef";

fn legacy<'a>(
    revision: u8,
    key_len: usize,
    (owner, user): (&'a [u8; 32], &'a [u8; 32]),
    permissions: i32,
    id: &'a [u8; 16],
) -> PdfLegacyEntries<'a> {
    PdfLegacyEntries {
        revision,
        key_len,
        owner,
        user,
        permissions,
        id,
        encrypt_metadata: true,
    }
}

#[test]
fn revision_2_opens_with_the_user_or_the_owner_password() {
    let (owner, user, id) = (
        hex::<32>("94e8094419662a774442fb072e3d9f19e9d130ec09a4d0061e78fe920f7ab62f"),
        hex::<32>("d83fe8c6e10495ecde93e470d372bb99614ca49ca40c8d9c82a154615bcff2f7"),
        hex::<16>(ID),
    );
    let entries = legacy(2, 5, (&owner, &user), -44, &id);
    let key = pdf_legacy_file_key(&entries, b"user")
        .expect("usable entries")
        .expect("the user's password");
    assert_eq!(key.as_bytes(), hex::<5>("4173f564b4"));
    let by_owner = pdf_legacy_file_key(&entries, b"owner")
        .expect("usable entries")
        .expect("the owner's password");
    assert_eq!(by_owner.as_bytes(), key.as_bytes());
    assert!(matches!(pdf_legacy_file_key(&entries, b"guess"), Ok(None)));
    assert_eq!(
        pdf_object_key(&key, 7, 0, false).as_bytes(),
        hex::<10>("835889470df58cf22b80")
    );
}

#[test]
fn revisions_3_and_4_open_with_either_password() {
    let id = hex::<16>(ID);
    let owner = hex::<32>("0ba3835f88f90388e74e54584125ce142be0de24c6b0d37746e075b891756671");
    for (revision, user, permissions, key, object) in [
        (
            3,
            "ad944bba9714b0f0d9da179955e1945000000000000000000000000000000000",
            -3904,
            "f39f55714572d7d71ba77d681118d8b6",
            ("5f9ca4148ae4f696f019cdd515d9b966", false),
        ),
        (
            4,
            "c19e29f3359b13734f81225b927a683800000000000000000000000000000000",
            -1028,
            "5257044448363c982214ce4e81c975f9",
            ("71d1295ae3b155a19e57e65a95e2fffe", true),
        ),
    ] {
        let user = hex::<32>(user);
        let entries = legacy(revision, 16, (&owner, &user), permissions, &id);
        let made = pdf_legacy_file_key(&entries, b"user")
            .expect("usable entries")
            .expect("the user's password");
        assert_eq!(made.as_bytes(), hex::<16>(key), "revision {revision}");
        let by_owner = pdf_legacy_file_key(&entries, b"owner")
            .expect("usable entries")
            .expect("the owner's password");
        assert_eq!(by_owner.as_bytes(), made.as_bytes());
        assert!(matches!(pdf_legacy_file_key(&entries, b""), Ok(None)));
        assert_eq!(
            pdf_object_key(&made, 7, 0, object.1).as_bytes(),
            hex::<16>(object.0)
        );
    }
}

#[test]
fn an_empty_user_password_opens_a_file_protected_only_by_its_permissions() {
    let id = hex::<16>(ID);
    let owner = hex::<32>("566fa873ee33c797cd3b904fdadf814afa34df9a38f6ed41b984e2c6da2aa6f5");
    let user = hex::<32>("04b8718d4765719ba91311d5246bb44100000000000000000000000000000000");
    let entries = legacy(3, 16, (&owner, &user), -3904, &id);
    let key = pdf_legacy_file_key(&entries, b"")
        .expect("usable entries")
        .expect("the empty password");
    assert_eq!(
        key.as_bytes(),
        hex::<16>("a60eb440fe19bb452932f8c4caa98b6c")
    );
}

#[test]
fn legacy_entries_out_of_range_are_unusable_whatever_the_password() {
    let id = hex::<16>(ID);
    let owner = [0u8; 32];
    let user = [0u8; 32];
    for (revision, key_len) in [(1, 5), (5, 16), (3, 4), (3, 17), (4, 32)] {
        let entries = legacy(revision, key_len, (&owner, &user), 0, &id);
        assert_eq!(
            pdf_legacy_file_key(&entries, b"").err(),
            Some(PdfUnusable),
            "revision {revision}, {key_len} bytes"
        );
    }
    let short = [0u8; 31];
    for (revision, owner, user) in [
        (3, &short[..], &user[..]),
        (2, &owner[..], &user[..16]),
        (3, &owner[..], &user[..15]),
    ] {
        let entries = PdfLegacyEntries {
            owner,
            user,
            ..legacy(revision, 16, (&[0; 32], &[0; 32]), 0, &id)
        };
        assert_eq!(
            pdf_legacy_file_key(&entries, b"").err(),
            Some(PdfUnusable),
            "revision {revision}: /O {} bytes, /U {}",
            owner.len(),
            user.len()
        );
    }
}

fn modern<'a>(
    revision: u8,
    (owner, user): (&'a [u8; 48], &'a [u8; 48]),
    (owner_key, user_key): (&'a [u8; 32], &'a [u8; 32]),
) -> PdfModernEntries<'a> {
    PdfModernEntries {
        revision,
        owner,
        user,
        owner_key,
        user_key,
    }
}

#[test]
fn revisions_5_and_6_unwrap_the_file_key_with_either_password() {
    let file_key: [u8; 32] = core::array::from_fn(|at| u8::try_from(at).expect("a small index"));
    for (revision, owner, user, owner_key, user_key) in [
        (
            6,
            "8a315bf69dd058f051451db82b059609a733a4f9f4ebc58b17c6de8eccea32844f5653414c5430314f4b53414c543031",
            "2767bfdcbf33c97e7801a89cb7525250849f8643c4783ca21967491d65b0a9de555653414c543031554b53414c543031",
            "b8d1b8f36e636b4b697c76d2c9c72cd1587e9be03c38638f15223d0d70fe5d48",
            "338a5284dd153ef6fb90831a254ac183ae89a699708fa0c45ba82035bbbf5f4b",
        ),
        (
            5,
            "b0548c2589881b9da10375ce13e9412ddd094a80ea70355c905b430ad73cbe3f4f5653414c5430314f4b53414c543031",
            "f65349b87ba9d9ac280dbd3971611405a7e3a6471d7660456bd4f0cb700739a2555653414c543031554b53414c543031",
            "654804116889e8ac55728c3811fb5c928a3bebdd507b6010de3e3830366bf747",
            "d21bcf2c7c1b4295850606de5a7e472027ab9b3d6397e875bd44d0042e86952a",
        ),
    ] {
        let (owner, user) = (hex::<48>(owner), hex::<48>(user));
        let (owner_key, user_key) = (hex::<32>(owner_key), hex::<32>(user_key));
        let entries = modern(revision, (&owner, &user), (&owner_key, &user_key));
        let by_user = pdf_modern_file_key(&entries, b"user")
            .expect("usable entries")
            .expect("the user's password");
        assert_eq!(by_user.as_bytes(), file_key, "revision {revision}");
        let by_owner = pdf_modern_file_key(&entries, b"owner")
            .expect("usable entries")
            .expect("the owner's password");
        assert_eq!(by_owner.as_bytes(), file_key, "revision {revision}");
        assert!(matches!(pdf_modern_file_key(&entries, b""), Ok(None)));
        for unknown in [4, 7] {
            let entries = modern(unknown, (&owner, &user), (&owner_key, &user_key));
            assert_eq!(
                pdf_modern_file_key(&entries, b"user").err(),
                Some(PdfUnusable)
            );
        }
    }
}

#[test]
fn aes_strings_decrypt_in_place_their_padding_left_out() {
    let mut data = hex::<32>("101112131415161718191a1b1c1d1e1fc112bc37e390beec0926f303ffb4e0b8");
    let key = PdfKey::of(&core::array::from_fn::<u8, 16, _>(|at| {
        u8::try_from(at).expect("a small index")
    }));
    let plain = pdf_aes_decrypt(&key, &mut data).expect("whole blocks");
    assert_eq!(&data[plain], b"Hello, PDF");
    let mut data = hex::<32>("101112131415161718191a1b1c1d1e1f94cb26536a58ebe6ad85137954bb7028");
    let key = PdfKey::of(&core::array::from_fn::<u8, 32, _>(|at| {
        u8::try_from(at).expect("a small index")
    }));
    let plain = pdf_aes_decrypt(&key, &mut data).expect("whole blocks");
    assert_eq!(&data[plain], b"Hello, PDF");
}

#[test]
fn a_final_part_block_is_dropped_and_data_shorter_than_its_iv_is_nothing() {
    let key = PdfKey::of(&core::array::from_fn::<u8, 16, _>(|at| {
        u8::try_from(at).expect("a small index")
    }));
    let whole = hex::<32>("101112131415161718191a1b1c1d1e1fc112bc37e390beec0926f303ffb4e0b8");
    let mut damaged = [0u8; 39];
    damaged[..32].copy_from_slice(&whole);
    let plain = pdf_aes_decrypt(&key, &mut damaged).expect("a key AES takes");
    assert_eq!(&damaged[plain], b"Hello, PDF");
    assert_eq!(pdf_aes_decrypt(&key, &mut [0u8; 8]), Some(0..0));
    assert_eq!(pdf_aes_decrypt(&key, &mut [0u8; 16]), Some(16..16));
    assert!(pdf_aes_decrypt(&PdfKey::of(&[0u8; 5]), &mut [0u8; 32]).is_none());
}

#[test]
fn rc4_decrypts_in_place() {
    let mut data = hex::<10>("fa5c0f699f11e0778885");
    pdf_rc4_decrypt(&PdfKey::of(&hex::<5>("0102030405")), &mut data);
    assert_eq!(&data, b"Hello, PDF");
}

#[test]
fn rc4_takes_every_key_length_the_handler_makes() {
    let key: [u8; 32] = core::array::from_fn(|at| u8::try_from(at).expect("a small index"));
    let mut data = hex::<10>("58419da5c583896d327a");
    pdf_rc4_decrypt(&PdfKey::of(&key), &mut data);
    assert_eq!(&data, b"Hello, PDF");
}
