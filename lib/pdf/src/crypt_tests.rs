//! Encrypted documents: the empty password, the user's and the owner's, a
//! wrong one, each method, and what is left in the clear.
//!
//! The ciphertexts and the handler's entries come from an implementation of
//! ISO 32000-2 §7.6 written apart from this one, so a check here is never this
//! code agreeing with itself.

use alloc::format;
use alloc::vec::Vec;

use tairix_colour::Rgba;

use crate::display::{Item, Paint};
use crate::document::{Document, Options};
use crate::fixture::{reference, Builder};
use crate::object::Object;
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

fn open<'f>(file: &'f [u8], password: Option<&[u8]>) -> Result<Document<'f>, PdfError> {
    Document::open(
        file,
        &Options {
            password,
            budget: 1 << 20,
        },
    )
}

/// The colour of the one fill the document's page paints.
fn painted(document: &Document<'_>) -> Rgba {
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
            .into_owned();
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

#[test]
fn aes_128_crypt_filters_decrypt_streams() {
    let encrypt = "<</Filter /Standard /V 4 /R 4 /Length 128 /P -1028 /O <0ba3835f88f90388e74e54584125ce142be0de24c6b0d37746e075b891756671> /U <c19e29f3359b13734f81225b927a683800000000000000000000000000000000> /CF <</StdCF <</CFM /AESV2 /Length 16>>>> /StmF /StdCF /StrF /StdCF>>";
    let content = hex("202122232425262728292a2b2c2d2e2f3bd9bbd23c5eee3d8d5f0a590b08be49eec07bfc0230a213d9bcf0fabab4b6bf");
    let file = encrypted(encrypt, &content, "00");
    assert_eq!(
        painted(&open(&file, Some(b"user")).expect("an opened document")),
        RED
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

#[test]
fn identity_streams_and_clear_metadata_are_not_decrypted() {
    let encrypt = "<</Filter /Standard /V 4 /R 4 /Length 128 /P -1028 /EncryptMetadata false /O <566fa873ee33c797cd3b904fdadf814afa34df9a38f6ed41b984e2c6da2aa6f5> /U <c865678d5c54e4586b93adf5b0f704ad00000000000000000000000000000000> /CF <</StdCF <</CFM /AESV2 /Length 16>>>> /StmF /StdCF /StrF /Identity>>";
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
        .object(9, encrypt);
    let file = pdf.tabled(1, &format!("/Encrypt 9 0 R /ID [<{ID}> <{ID}>]"));
    let document = open(&file, None).expect("an opened document");
    assert_eq!(painted(&document), RED);
    assert_eq!(
        document
            .resolve(&reference(5))
            .expect("a string")
            .into_owned(),
        Object::String(b"clear".to_vec())
    );
    let Ok(Object::Stream(metadata)) = document
        .resolve(&reference(6))
        .map(alloc::borrow::Cow::into_owned)
    else {
        panic!("a stream");
    };
    assert_eq!(
        document.decode(&metadata).expect("its bytes").bytes,
        b"<x/>"
    );
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
    for (version, revision, length) in [(2, 1, 128), (2, 3, 256), (5, 7, 256)] {
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
    let content = hex("202122232425262728292a2b2c2d2e2f3bd9bbd23c5eee3d8d5f0a590b08be49eec07bfc0230a213d9bcf0fabab4b6bf");
    let file = encrypted(encrypt, &content, "00");
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
