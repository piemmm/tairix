use alloc::vec::Vec;

use super::{Archive, Writer, ZipError, MOST_ENTRIES};

fn written(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = Writer::new();
    for (name, data) in entries {
        writer.store(name, data).expect("room");
    }
    writer.finish().expect("room")
}

#[test]
fn stored_entries_read_back_in_order() {
    let zip = written(&[
        ("mimetype", b"image/openraster"),
        ("stack.xml", b"<image/>"),
    ]);
    assert_eq!(
        &zip[30..38],
        b"mimetype",
        "the first entry's name right after its header"
    );
    assert_eq!(&zip[38..54], b"image/openraster", "and its bytes, stored");
    let archive = Archive::open(&zip).expect("readable");
    let names: Vec<&[u8]> = archive.names().collect();
    assert_eq!(names, [&b"mimetype"[..], &b"stack.xml"[..]]);
    assert_eq!(
        archive.read("stack.xml", 100).expect("good").as_deref(),
        Some(&b"<image/>"[..])
    );
    assert_eq!(archive.read("missing", 100).expect("good"), None);
    assert_eq!(
        archive.read("stack.xml", 3),
        Err(ZipError::TooLarge),
        "past what is allowed"
    );
}

#[test]
fn a_deflated_entry_is_inflated() {
    let mut zip = written(&[("a", b"hello")]);
    // Rewrite the entry as one deflate stored block: the same bytes framed.
    let block: &[u8] = &[0x01, 5, 0, !5u8, 0xFF, b'h', b'e', b'l', b'l', b'o'];
    let mut deflated = Vec::new();
    deflated.extend_from_slice(&zip[..30]);
    deflated[8] = 8;
    deflated[18..22].copy_from_slice(&(u32::try_from(block.len()).expect("small")).to_le_bytes());
    deflated.extend_from_slice(b"a");
    deflated.extend_from_slice(block);
    let central_at = 30 + 1 + 5;
    let mut central = zip[central_at..].to_vec();
    central[10] = 8;
    central[20..24].copy_from_slice(&(u32::try_from(block.len()).expect("small")).to_le_bytes());
    let offset = u32::try_from(deflated.len()).expect("small");
    let end = central.len() - 22;
    central[end + 16..end + 20].copy_from_slice(&offset.to_le_bytes());
    deflated.extend_from_slice(&central);
    zip = deflated;
    let archive = Archive::open(&zip).expect("readable");
    assert_eq!(
        archive.read("a", 10).expect("good").as_deref(),
        Some(&b"hello"[..])
    );
}

#[test]
fn damage_encryption_and_lies_are_refused() {
    let zip = written(&[("a", b"payload")]);
    let mut corrupt = zip.clone();
    corrupt[30 + 1] ^= 0xFF;
    assert_eq!(
        Archive::open(&corrupt)
            .expect("directory intact")
            .read("a", 100),
        Err(ZipError::Malformed),
        "the CRC catches it"
    );
    let mut encrypted = zip.clone();
    let central_at = 30 + 1 + 7;
    encrypted[central_at + 8] |= 1;
    assert_eq!(Archive::open(&encrypted).err(), Some(ZipError::Unsupported));
    assert_eq!(
        Archive::open(&zip[..zip.len() - 3]).err(),
        Some(ZipError::Malformed),
        "truncated"
    );
    let mut many = zip.clone();
    let end = many.len() - 22;
    let count = u16::try_from(MOST_ENTRIES + 1)
        .expect("small")
        .to_le_bytes();
    many[end + 8..end + 10].copy_from_slice(&count);
    many[end + 10..end + 12].copy_from_slice(&count);
    assert_eq!(
        Archive::open(&many).err(),
        Some(ZipError::Malformed),
        "more entries than allowed"
    );
    assert_eq!(
        Archive::open(b"not a zip at all").err(),
        Some(ZipError::Malformed)
    );
}
