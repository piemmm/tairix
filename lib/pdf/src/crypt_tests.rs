//! Encrypted documents: the empty password, the user's and the owner's, a
//! wrong one, each method, and what is left in the clear.
//!
//! The ciphertexts and the handler's entries come from an implementation of
//! ISO 32000-2 §7.6 written apart from this one, so a check here is never this
//! code agreeing with itself.

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use tairix_colour::Rgba;
use tairix_crypto::AES_BLOCK_LEN;

use super::Security;
use crate::display::{Item, Paint};
use crate::document::{Document, Options};
use crate::fixture::{reference, Builder};
use crate::limits::MAX_PASSWORD_BYTES;
use crate::object::{Dict, Name, ObjRef, Object};
use crate::PdfError;

const ID: &str = "0123456789abcdef0123456789abcdef";

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex digits"))
        .collect()
}

/// A one-page document whose content, object 4, is `content` as written,
/// encrypted as `encrypt` says, object 5 the string `string`.
fn encrypted(encrypt: &str, content: &[u8], string: &str) -> Vec<u8> {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 100 100] /Contents 4 0 R>>")
        .stream(4, "", content)
        .object(5, &format!("<{string}>"))
        .object(9, encrypt);
    pdf.tabled(1, &format!("/Encrypt 9 0 R /ID [<{ID}> <{ID}>]"))
}

fn open(file: &[u8], password: Option<&[u8]>) -> Result<Document, PdfError> {
    Document::open(crate::fixture::kept(file), &options(password)).map_err(PdfError::from)
}

fn options(password: Option<&[u8]>) -> Options<'_> {
    Options {
        password,
        budget: 1 << 20,
    }
}

/// The colour of the one fill the document's page paints.
fn painted(document: &Document) -> Rgba {
    let list = document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list");
    match list.items.as_slice() {
        [Item::Fill {
            paint: Paint::Colour(colour),
            ..
        }] => *colour,
        other => panic!("one fill: {other:?}"),
    }
}

const RED: Rgba = Rgba::new(255, 0, 0, 255);

const R3: &str = "<</Filter /Standard /V 2 /R 3 /Length 128 /P -3904 /O <0ba3835f88f90388e74e54584125ce142be0de24c6b0d37746e075b891756671> /U <ad944bba9714b0f0d9da179955e1945000000000000000000000000000000000>>>";

#[test]
fn a_refused_file_is_handed_back_whole_to_be_opened_again() {
    let file = encrypted(
        R3,
        &hex("30602e43be946bb3592a420487c37b03158848a005fc99"),
        "6e8e80c004",
    );
    let refused = Document::open(file.clone(), &options(None))
        .err()
        .expect("refused");
    assert_eq!(refused.error, PdfError::NeedsPassword);
    assert_eq!(refused.file, file);
    let refused = Document::open(refused.file, &options(Some(b"guess")))
        .err()
        .expect("refused");
    assert_eq!(refused.error, PdfError::WrongPassword);
    let document = Document::open(refused.file, &options(Some(b"user"))).expect("opened");
    assert_eq!(painted(&document), RED);
    let junk = Document::open(b"junk".to_vec(), &options(None))
        .err()
        .expect("refused");
    assert_eq!(
        (junk.error, junk.file),
        (PdfError::Malformed, b"junk".to_vec())
    );
}

#[test]
fn a_file_needing_a_password_says_so_and_opens_with_either() {
    let file = encrypted(
        R3,
        &hex("30602e43be946bb3592a420487c37b03158848a005fc99"),
        "6e8e80c004",
    );
    assert_eq!(open(&file, None).err(), Some(PdfError::NeedsPassword));
    assert_eq!(
        open(&file, Some(b"guess")).err(),
        Some(PdfError::WrongPassword)
    );
    for password in [&b"user"[..], b"owner"] {
        let document = open(&file, Some(password)).expect("an opened document");
        assert_eq!(painted(&document), RED);
        let string = document
            .resolve(&reference(5))
            .expect("a string")
            .into_owned()
            .expect("room");
        assert_eq!(string, Object::String(b"Hello".to_vec()));
    }
}

#[test]
fn a_file_protected_only_by_its_permissions_opens_without_asking() {
    let encrypt = "<</Filter /Standard /V 2 /R 3 /Length 128 /P -3904 /O <566fa873ee33c797cd3b904fdadf814afa34df9a38f6ed41b984e2c6da2aa6f5> /U <04b8718d4765719ba91311d5246bb44100000000000000000000000000000000>>>";
    let file = encrypted(
        encrypt,
        &hex("59146377ddd28b3b8fcd396bc1c0e4df920d52487af745"),
        "00",
    );
    assert_eq!(
        painted(&open(&file, None).expect("an opened document")),
        RED
    );
    assert_eq!(
        painted(&open(&file, Some(b"anything")).expect("still opened")),
        RED
    );
}

/// Version 4 entries for the user password `user`, its strings and streams
/// under AES-128.
const AES_128: &str = "<</Filter /Standard /V 4 /R 4 /Length 128 /P -1028 /O <0ba3835f88f90388e74e54584125ce142be0de24c6b0d37746e075b891756671> /U <c19e29f3359b13734f81225b927a683800000000000000000000000000000000> /CF <</StdCF <</CFM /AESV2 /Length 16>>>> /StmF /StdCF /StrF /StdCF>>";

/// A red square encrypted as object 4 under [`AES_128`]'s file key.
const AES_128_CONTENT: &str = "202122232425262728292a2b2c2d2e2f3bd9bbd23c5eee3d8d5f0a590b08be49eec07bfc0230a213d9bcf0fabab4b6bf";

#[test]
fn aes_128_crypt_filters_decrypt_streams() {
    let file = encrypted(AES_128, &hex(AES_128_CONTENT), "00");
    let document = open(&file, Some(b"user")).expect("an opened document");
    assert!(document.encrypted());
    assert_eq!(painted(&document), RED);
    // A string shorter than its IV holds nothing.
    assert_eq!(
        document
            .resolve(&reference(5))
            .expect("a string")
            .into_owned()
            .expect("room"),
        Object::String(Vec::new())
    );
}

#[test]
fn aes_256_decrypts_with_the_file_key() {
    let encrypt = "<</Filter /Standard /V 5 /R 6 /Length 256 /P -1028 /O <8a315bf69dd058f051451db82b059609a733a4f9f4ebc58b17c6de8eccea32844f5653414c5430314f4b53414c543031> /U <2767bfdcbf33c97e7801a89cb7525250849f8643c4783ca21967491d65b0a9de555653414c543031554b53414c543031> /OE <b8d1b8f36e636b4b697c76d2c9c72cd1587e9be03c38638f15223d0d70fe5d48> /UE <338a5284dd153ef6fb90831a254ac183ae89a699708fa0c45ba82035bbbf5f4b> /CF <</StdCF <</CFM /AESV3 /Length 32>>>> /StmF /StdCF /StrF /StdCF>>";
    let content = hex("202122232425262728292a2b2c2d2e2f00749bf08c14fc6d0d95b9d03db266e39afc7edfe9d7b1c7a81a92b5509e1b98");
    let file = encrypted(encrypt, &content, "00");
    assert_eq!(open(&file, None).err(), Some(PdfError::NeedsPassword));
    assert_eq!(
        painted(&open(&file, Some(b"owner")).expect("an opened document")),
        RED
    );
}

/// Revision 6 entries for the owner password `owner` and the user password
/// `user` spelled as `/O`, `/U`, `/OE` and `/UE` say, over [`AES_256_CONTENT`].
fn revision_6(entries: &str) -> Vec<u8> {
    let encrypt = format!(
        "<</Filter /Standard /V 5 /R 6 /Length 256 /P -1028 {entries} /CF <</StdCF <</CFM /AESV3 /Length 32>>>> /StmF /StdCF /StrF /StdCF>>"
    );
    encrypted(&encrypt, &hex(AES_256_CONTENT), "00")
}

/// A red square encrypted under the file key both revision 6 files share.
const AES_256_CONTENT: &str = "202122232425262728292a2b2c2d2e2f809e609df1c4d732edf381f1c9b7b19c9907effa8fdeb0ee7a825e6d990791c6";

/// Made by a producer that prepared the user password `\u{2168}` (ROMAN
/// NUMERAL NINE) by SASLprep, so hashed `IX`.
const PREPARED: &str = "/O <5603fa177e6a8179eacc6953eaa0af4441d531cb53f23d048fc930928d14ba7f4f5653414c5430314f4b53414c543031> /U <1e9d3cbf6b39b2dcf4b6aa903b797f95ee32c8766f6142d233d8719544e9c077555653414c543031554b53414c543031> /OE <0021ea746a34871dae6b0c29c0b070ed322c4151bde50e841d73dec1bc919615> /UE <d4de1720acb4090c416037aa311bf9db6034bc5c77f5f2e9fde7a945c463a1d6>";

/// Made by a producer that hashed the same password as its UTF-8, unprepared.
const UNPREPARED: &str = "/O <bf484e63c6f49ba374665ee8d51d79a16fdec2d213ac8889d4f1864483ed8d2d4f5653414c5430314f4b53414c543031> /U <88249313792d28ab0ac3b6bf381f29ac241788afdd56b386e6b9d18a3db5897f555653414c543031554b53414c543031> /OE <8c7b926b74596216fbbf400aca614c059d1a4ef59640a579bb3044c34143fcdc> /UE <374941fa4e9621f15d52a4057e918de0e993d3742ba5f342a35ae97602a1668d>";

#[test]
fn a_revision_6_password_is_prepared_by_saslprep_before_it_is_hashed() {
    let file = revision_6(PREPARED);
    for typed in ["\u{2168}", "IX", "I\u{ad}X", "\u{ff29}\u{ff38}"] {
        let opened = open(&file, Some(typed.as_bytes())).expect("an opened document");
        assert_eq!(painted(&opened), RED, "{typed:?}");
    }
    assert_eq!(
        open(&file, Some(b"ix")).err(),
        Some(PdfError::WrongPassword)
    );
    assert_eq!(painted(&open(&file, Some(b"owner")).expect("opened")), RED);
}

/// Several producers hash a password as typed; its bytes are tried once its
/// prepared form fails.
#[test]
fn a_password_hashed_unprepared_still_opens_its_file() {
    let file = revision_6(UNPREPARED);
    let typed = "\u{2168}".as_bytes();
    assert_eq!(painted(&open(&file, Some(typed)).expect("opened")), RED);
    assert_eq!(
        open(&file, Some(b"IX")).err(),
        Some(PdfError::WrongPassword),
        "the prepared spelling is another password to this file"
    );
}

/// Version 4 entries the empty password opens, metadata left in the clear,
/// and the crypt filters `filters` states.
fn version_4(filters: &str) -> String {
    format!(
        "<</Filter /Standard /V 4 /R 4 /Length 128 /P -1028 /EncryptMetadata false /O <566fa873ee33c797cd3b904fdadf814afa34df9a38f6ed41b984e2c6da2aa6f5> /U <c865678d5c54e4586b93adf5b0f704ad00000000000000000000000000000000> {filters}>>"
    )
}

#[test]
fn identity_streams_and_clear_metadata_are_not_decrypted() {
    let encrypt =
        version_4("/CF <</StdCF <</CFM /AESV2 /Length 16>>>> /StmF /StdCF /StrF /Identity");
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R /Metadata 6 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 100 100] /Contents 4 0 R>>")
        .stream(
            4,
            "/Filter /Crypt /DecodeParms <</Name /Identity>>",
            b"1 0 0 rg 0 0 10 10 re f",
        )
        .object(5, "(clear)")
        .stream(6, "/Type /Metadata /Subtype /XML", b"<x/>")
        .object(9, &encrypt);
    let file = pdf.tabled(1, &format!("/Encrypt 9 0 R /ID [<{ID}> <{ID}>]"));
    let document = open(&file, None).expect("an opened document");
    assert_eq!(painted(&document), RED);
    assert_eq!(
        document
            .resolve(&reference(5))
            .expect("a string")
            .into_owned()
            .expect("room"),
        Object::String(b"clear".to_vec())
    );
    let Ok(Object::Stream(metadata)) = document
        .resolve(&reference(6))
        .and_then(crate::object::Resolved::into_owned)
    else {
        panic!("a stream");
    };
    assert_eq!(
        document.decode(&metadata).expect("its bytes").bytes,
        b"<x/>"
    );
}

#[test]
fn a_refused_encryption_dictionary_is_not_repaired_into_a_clear_file() {
    let file = revision_6(&zeroed_entries("U"));
    let end = file
        .windows(b"startxref".len())
        .rposition(|bytes| bytes == b"startxref")
        .expect("a startxref");
    // A later trailer naming no encryption, which only a repair reads.
    let file = [
        &file[..end],
        &b"trailer\n<</Root 1 0 R>>\n"[..],
        &file[end..],
    ]
    .concat();
    assert_eq!(open(&file, None).err(), Some(PdfError::Unsupported));
}

#[test]
fn a_refused_encryption_dictionary_is_not_traded_for_another_a_repair_reaches() {
    let file = revision_6(&zeroed_entries("U"));
    let end = file
        .windows(b"startxref".len())
        .rposition(|bytes| bytes == b"startxref")
        .expect("a startxref");
    // A later trailer naming another encryption dictionary, one the empty
    // password opens, which only a repair reads.
    let other = format!(
        "10 0 obj\n{}\nendobj\ntrailer\n<</Root 1 0 R /Encrypt 10 0 R>>\n",
        version_4("/StmF /Identity /StrF /Identity")
    );
    let file = [&file[..end], other.as_bytes(), &file[end..]].concat();
    assert_eq!(open(&file, None).err(), Some(PdfError::Unsupported));
}

#[test]
fn another_security_handler_is_unsupported() {
    let file = encrypted("<</Filter /Adobe.PubSec /V 4 /R 4>>", b"", "00");
    assert_eq!(open(&file, None).err(), Some(PdfError::Unsupported));
}

#[test]
fn a_latin_1_password_opens_given_as_text_or_as_its_bytes() {
    let encrypt = "<</Filter /Standard /V 2 /R 3 /Length 128 /P -3904 /O <97a40f051f081398dcb19f7d256bd6bd2e1bd702c69e6511feb6cb17b2d031eb> /U <88abc75f37b0db38eed9ad93eb344cb800000000000000000000000000000000>>>";
    let file = encrypted(
        encrypt,
        &hex("a0943232de8f20a76b17285565095ca24ede6ca19ad562"),
        "d4ffbf1b27",
    );
    for password in ["été".as_bytes(), b"\xe9t\xe9"] {
        let document = open(&file, Some(password)).expect("an opened document");
        assert_eq!(painted(&document), RED);
    }
    assert_eq!(
        open(&file, Some(b"ete")).err(),
        Some(PdfError::WrongPassword)
    );
}

#[test]
fn a_legacy_password_is_hashed_in_pdfdocencoding() {
    let encrypt = "<</Filter /Standard /V 2 /R 3 /Length 128 /P -3904 /O <dea5944288f90388e74e54584125ce142be0de24c6b0d37746e075b891756671> /U <52916fb3416f45318929050442ce537a00000000000000000000000000000000>>>";
    let file = encrypted(
        encrypt,
        &hex("71651aefc0797efa9f327bcde678c4532f0f16e5e88dd8"),
        "b3b4d49219",
    );
    let document = open(&file, Some("€uro".as_bytes())).expect("an opened document");
    assert_eq!(painted(&document), RED);
}

#[test]
fn only_text_pdfdocencoding_holds_is_rewritten() {
    let encoded = super::legacy_password("été €".as_bytes()).expect("room");
    assert_eq!(encoded.as_deref(), Some(&b"\xe9t\xe9 \xa0"[..]));
    for given in [&b"ascii"[..], "中".as_bytes(), b"\xe9t\xe9"] {
        assert!(super::legacy_password(given).expect("room").is_none());
    }
}

#[test]
fn entries_no_password_opens_are_unsupported_not_asked_about() {
    let owner = "0ba3835f88f90388e74e54584125ce142be0de24c6b0d37746e075b891756671";
    let user = "ad944bba9714b0f0d9da179955e1945000000000000000000000000000000000";
    let salts = "00000000000000000000000000000000";
    for (version, revision, length) in [(2, 1, 128), (2, 3, 256), (2, 3, -128), (5, 7, 256)] {
        let encrypt = format!("<</Filter /Standard /V {version} /R {revision} /Length {length} /P -3904 /O <{owner}{salts}> /U <{user}{salts}> /OE <{owner}> /UE <{owner}>>>");
        let file = encrypted(&encrypt, b"0 0 1 1 re f", "00");
        for password in [None, Some(&b"user"[..])] {
            assert_eq!(
                open(&file, password).err(),
                Some(PdfError::Unsupported),
                "version {version}, revision {revision}"
            );
        }
    }
}

#[test]
fn a_version_4_key_is_as_long_as_its_crypt_filter_says() {
    let encrypt = "<</Filter /Standard /V 4 /R 4 /P -1028 /O <0ba3835f88f90388e74e54584125ce142be0de24c6b0d37746e075b891756671> /U <c19e29f3359b13734f81225b927a683800000000000000000000000000000000> /CF <</StdCF <</CFM /AESV2>>>> /StmF /StdCF /StrF /StdCF>>";
    let file = encrypted(encrypt, &hex(AES_128_CONTENT), "00");
    assert_eq!(
        painted(&open(&file, Some(b"user")).expect("an opened document")),
        RED
    );
}

#[test]
fn an_object_stream_read_before_its_security_is_read_again_decrypted() {
    // `/P` lies in an object stream left in the clear: reading it before the
    // file key exists must not leave the stream cached undecrypted, so the
    // page's box read from it later is decrypted like any stream's.
    let encrypt = R3.replace("/P -3904", "/P 20 0 R");
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox 21 0 R>>")
        .object(9, &encrypt)
        .object_stream(30, &[(20, "-3904"), (21, "[0 0 50 60]")]);
    let at = pdf.xref_stream(
        40,
        &format!("/Root 1 0 R /Encrypt 9 0 R /ID [<{ID}> <{ID}>]"),
        &[(20, 30, 0), (21, 30, 1)],
    );
    let file = pdf.finish(at);
    let document = open(&file, Some(b"user")).expect("an opened document");
    assert_eq!(document.page(0).expect("a page").size(), (612.0, 792.0));
}

#[test]
fn a_crypt_filter_the_file_does_not_define_is_refused_not_read_as_the_identity() {
    for (filters, refusal) in [
        (
            "/CF <</StdCF <</CFM /AESV2>>>> /StmF /Missing",
            PdfError::Unsupported,
        ),
        (
            "/CF <</StdCF <</CFM /AESV2>>>> /StrF /Missing",
            PdfError::Unsupported,
        ),
        ("/StmF /StdCF /StrF /StdCF", PdfError::Unsupported),
        ("/CF <</StdCF 5>> /StmF /StdCF", PdfError::Malformed),
        (
            "/CF <</StdCF <</CFM 5>>>> /StmF /StdCF",
            PdfError::Malformed,
        ),
        ("/StmF 5", PdfError::Malformed),
    ] {
        let file = encrypted(&version_4(filters), b"1 0 0 rg 0 0 10 10 re f", "00");
        assert_eq!(open(&file, None).err(), Some(refusal), "{filters}");
    }
    // `/CF` cannot make the standard's identity filter another.
    let redefined = version_4("/CF <</Identity <</CFM /AESV2>>>> /StmF /Identity /StrF /Identity");
    let file = encrypted(&redefined, b"1 0 0 rg 0 0 10 10 re f", "00");
    assert_eq!(
        painted(&open(&file, None).expect("an opened document")),
        RED
    );
}

#[test]
fn a_stream_naming_a_crypt_filter_the_file_does_not_define_is_refused() {
    let encrypt = version_4("/CF <</StdCF <</CFM /AESV2>>>> /StmF /StdCF");
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 100 100] /Contents 4 0 R>>")
        .stream(
            4,
            "/Filter /Crypt /DecodeParms <</Name /Missing>>",
            b"1 0 0 rg 0 0 10 10 re f",
        )
        .object(9, &encrypt);
    let file = pdf.tabled(1, &format!("/Encrypt 9 0 R /ID [<{ID}> <{ID}>]"));
    let document = open(&file, None).expect("an opened document");
    let Ok(Object::Stream(content)) = document
        .resolve(&reference(4))
        .and_then(crate::object::Resolved::into_owned)
    else {
        panic!("a stream");
    };
    assert_eq!(document.decode(&content).err(), Some(PdfError::Unsupported));
}

#[test]
fn an_unencrypted_stream_naming_a_crypt_filter_but_the_identity_is_refused() {
    for (name, decoded) in [
        ("/Identity", Ok(())),
        ("/StdCF", Err(PdfError::Unsupported)),
    ] {
        let mut pdf = Builder::new();
        pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
            .object(2, "<</Type /Pages /Kids [3 0 R]>>")
            .object(3, "<</Type /Page /MediaBox [0 0 100 100] /Contents 4 0 R>>")
            .stream(
                4,
                &format!("/Filter /Crypt /DecodeParms <</Name {name}>>"),
                b"1 0 0 rg 0 0 10 10 re f",
            );
        let file = pdf.tabled(1, "");
        let document = open(&file, None).expect("an opened document");
        let Ok(Object::Stream(content)) = document
            .resolve(&reference(4))
            .and_then(crate::object::Resolved::into_owned)
        else {
            panic!("a stream");
        };
        assert_eq!(document.decode(&content).map(|_| ()), decoded, "{name}");
    }
}

#[test]
fn a_stream_naming_its_crypt_filter_with_no_name_is_refused() {
    let encrypt = version_4("/CF <</StdCF <</CFM /AESV2>>>> /StmF /StdCF");
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 100 100] /Contents 4 0 R>>")
        .stream(
            4,
            "/Filter /Crypt /DecodeParms <</Name (Identity)>>",
            b"1 0 0 rg 0 0 10 10 re f",
        )
        .object(9, &encrypt);
    let file = pdf.tabled(1, &format!("/Encrypt 9 0 R /ID [<{ID}> <{ID}>]"));
    let document = open(&file, None).expect("an opened document");
    let Ok(Object::Stream(content)) = document
        .resolve(&reference(4))
        .and_then(crate::object::Resolved::into_owned)
    else {
        panic!("a stream");
    };
    assert_eq!(document.decode(&content).err(), Some(PdfError::Malformed));
}

/// Revision 6 entries of zeros, each as long as revision 6 reads but the one
/// named `short`, a byte shorter.
fn zeroed_entries(short: &str) -> String {
    [("O", 48), ("U", 48), ("OE", 32), ("UE", 32)].iter().fold(
        String::new(),
        |mut entries, &(name, len)| {
            let len = if name == short { len - 1 } else { len };
            write!(entries, "/{name} <{}> ", "00".repeat(len)).expect("a string takes any text");
            entries
        },
    )
}

#[test]
fn modern_entries_too_short_to_read_are_unsupported() {
    for short in ["O", "U", "OE", "UE"] {
        let file = revision_6(&zeroed_entries(short));
        for password in [None, Some(&b"user"[..])] {
            assert_eq!(
                open(&file, password).err(),
                Some(PdfError::Unsupported),
                "/{short}"
            );
        }
    }
}

#[test]
fn a_password_past_the_bound_opens_nothing() {
    let file = revision_6(PREPARED);
    // Soft hyphens, which SASLprep maps to nothing, pad `IX` out.
    let padded = |hyphens: usize| format!("I{}X", "\u{ad}".repeat(hyphens));
    let longest = padded((MAX_PASSWORD_BYTES - 2) / 2);
    assert_eq!(longest.len(), MAX_PASSWORD_BYTES);
    assert_eq!(
        painted(&open(&file, Some(longest.as_bytes())).expect("opened")),
        RED
    );
    let past = padded(MAX_PASSWORD_BYTES / 2);
    assert_eq!(
        open(&file, Some(past.as_bytes())).err(),
        Some(PdfError::WrongPassword)
    );
}

#[test]
fn the_file_key_stays_where_it_was_made_however_its_handler_moves() {
    let file = encrypted(AES_128, &hex(AES_128_CONTENT), "00");
    let document = open(&file, Some(b"user")).expect("an opened document");
    let trailer = Dict::from_written(alloc::vec![
        (Name(b"Encrypt".to_vec()), reference(9)),
        (
            Name(b"ID".to_vec()),
            Object::Array(alloc::vec![Object::String(hex(ID)); 2])
        ),
    ])
    .expect("room");
    let security = Security::open(&document, &trailer, Some(b"user"))
        .expect("usable entries")
        .expect("an encrypted file");
    let at = |security: &Security| core::ptr::from_ref::<tairix_crypto::pdf::PdfKey>(&security.key);
    let made = at(&security);
    let held = alloc::vec![security];
    assert_eq!(made, at(&held[0]));
}

#[test]
fn an_aes_streams_iv_is_not_copied_with_its_ciphertext() {
    let content = hex(AES_128_CONTENT);
    let file = encrypted(AES_128, &content, "00");
    let document = open(&file, Some(b"user")).expect("an opened document");
    let trailer = Dict::from_written(alloc::vec![
        (Name(b"Encrypt".to_vec()), reference(9)),
        (
            Name(b"ID".to_vec()),
            Object::Array(alloc::vec![Object::String(hex(ID)); 2])
        ),
    ])
    .expect("room");
    let security = Security::open(&document, &trailer, Some(b"user"))
        .expect("usable entries")
        .expect("an encrypted file");
    let owner = ObjRef { num: 4, gen: 0 };
    let Ok(Cow::Owned(plain)) = security.decrypt_stream(&Dict::new(), &content, owner, None) else {
        panic!("a decrypted copy");
    };
    assert!(!plain.is_empty());
    assert_eq!(plain.capacity(), content.len() - AES_BLOCK_LEN);
}
