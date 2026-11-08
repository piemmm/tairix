//! The data URL: base64 both ways against RFC 4648's own vectors, the
//! percent form, what a reader refuses, and the bound it holds a payload to.

use alloc::string::String;
use alloc::vec::Vec;

use super::{decode, write};
use crate::DrawingError;

fn written(bytes: &[u8]) -> String {
    let mut url = String::new();
    write(&mut url, "image/png", bytes).expect("written");
    url
}

#[test]
fn base64_spells_rfc_4648s_vectors() {
    for (bytes, spelled) in [
        (&b""[..], ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (b"foob", "Zm9vYg=="),
        (b"fooba", "Zm9vYmE="),
        (b"foobar", "Zm9vYmFy"),
    ] {
        let url = written(bytes);
        assert_eq!(url, alloc::format!("data:image/png;base64,{spelled}"));
        assert_eq!(decode(&url, 64), Ok(Some(bytes.to_vec())), "{spelled:?}");
    }
}

#[test]
fn every_byte_and_length_reads_back_as_it_was_written() {
    let mut rng = tairix_fuzzseed::Prng::new(0xDA7A_0001);
    let mut bytes: Vec<u8> = (0..=255).collect();
    for _ in 0..10_000 {
        bytes.push(rng.next_u64().to_le_bytes()[0]);
    }
    for length in (0..40).chain([3071, 3072, 3073, 6144, bytes.len()]) {
        let url = written(&bytes[..length]);
        assert_eq!(
            decode(&url, usize::MAX),
            Ok(Some(bytes[..length].to_vec())),
            "{length}"
        );
    }
}

#[test]
fn white_space_padding_and_case_are_a_writers_own_choice() {
    assert_eq!(
        decode(" DATA:image/png;BASE64,Zm9v\n YmE=\r\n ", 64),
        Ok(Some(b"fooba".to_vec()))
    );
    assert_eq!(
        decode("data:image/jpeg;base64,Zm9vYmE", 64),
        Ok(Some(b"fooba".to_vec())),
        "padding may be left off"
    );
    assert_eq!(
        decode("data:,a%20b%2Cc", 64),
        Ok(Some(b"a b,c".to_vec())),
        "the percent form, with no media type"
    );
}

#[test]
fn a_link_that_is_no_data_url_is_no_picture_this_reader_reaches() {
    for link in [
        "picture.png",
        "https://example.org/a.png",
        "#defined",
        "",
        "dat:x,y",
    ] {
        assert_eq!(decode(link, 64), Ok(None), "{link:?}");
    }
}

#[test]
fn a_malformed_payload_is_refused() {
    for url in [
        "data:image/png;base64",
        "data:image/png;base64,Zm9v!",
        "data:image/png;base64,Z",
        "data:image/png;base64,Zm9vY",
        "data:image/png;base64,Zg==Zg==",
        "data:image/png;base64,Zg=x",
        "data:,%2",
        "data:,%zz",
    ] {
        assert_eq!(decode(url, 64), Err(DrawingError::Invalid), "{url:?}");
    }
}

#[test]
fn a_payload_past_the_bound_is_refused_before_it_is_held() {
    let url = written(&[7; 65]);
    assert_eq!(
        decode(&url, 65).map(|bytes| bytes.map(|b| b.len())),
        Ok(Some(65))
    );
    assert_eq!(decode(&url, 64), Err(DrawingError::TooComplex));
    assert_eq!(decode("data:,abcde", 4), Err(DrawingError::TooComplex));
}

#[test]
fn any_text_is_read_or_refused_and_never_otherwise() {
    let mut rng = tairix_fuzzseed::Prng::new(0xDA7A_0002);
    let alphabet = b"data:;base64,%ZYmz09+/= \n";
    for _ in 0..20_000 {
        let length = usize::try_from(rng.next_u64() % 40).unwrap_or(0);
        let text: String = (0..length)
            .map(|_| {
                let at = usize::try_from(rng.next_u64() % alphabet.len() as u64).unwrap_or(0);
                char::from(alphabet[at])
            })
            .collect();
        if let Ok(Some(bytes)) = decode(&text, 16) {
            assert!(bytes.len() <= 16, "{text:?} held more than the bound");
        }
    }
}
