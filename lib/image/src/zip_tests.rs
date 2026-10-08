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

/// An archive of one entry, `a`, holding `hello` deflated as one stored
/// block: the same bytes framed.
fn deflated_hello() -> Vec<u8> {
    let zip = written(&[("a", b"hello")]);
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
    deflated
}

#[test]
fn a_deflated_entry_is_inflated() {
    let zip = deflated_hello();
    let archive = Archive::open(&zip).expect("readable");
    assert_eq!(
        archive.read("a", 10).expect("good").as_deref(),
        Some(&b"hello"[..])
    );
}

/// A view names an entry's size, and its bytes in place only where they are
/// stored as they read; a name no entry carries has none.
#[test]
fn a_view_shows_a_stored_entry_in_place_and_a_deflated_one_by_its_size() {
    let stored = written(&[("a", b"hello"), ("b", b"!")]);
    let archive = Archive::open(&stored).expect("readable");
    let view = archive.view("a").expect("good").expect("named");
    assert_eq!((view.size, view.stored), (5, Some(&b"hello"[..])));
    assert!(archive.view("c").expect("good").is_none());
    let packed = deflated_hello();
    let archive = Archive::open(&packed).expect("readable");
    let view = archive.view("a").expect("good").expect("named");
    assert_eq!((view.size, view.stored), (5, None));
}

/// What the directory's record holds covers every entry it records.
#[test]
fn the_directory_held_covers_every_entry() {
    let names: Vec<alloc::string::String> = (0..64).map(|at| alloc::format!("e{at}")).collect();
    let entries: Vec<(&str, &[u8])> = names
        .iter()
        .map(|name| (name.as_str(), &b"x"[..]))
        .collect();
    let (few, many) = (written(&entries[..1]), written(&entries));
    let held = |zip: &[u8]| Archive::open(zip).expect("readable").held_bytes();
    let entry = core::mem::size_of::<super::Entry>();
    assert!(held(&few) >= entry);
    assert!(held(&many) >= 64 * entry, "{}", held(&many));
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

/// A head holds an entry's opening bytes, stored or deflated, fewer where the
/// entry is shorter, and nothing for a name the archive does not hold.
#[test]
fn a_head_reads_an_entrys_opening_bytes_however_it_is_stored() {
    let data: Vec<u8> = (0..100u8).collect();
    let mut zip = Writer::new();
    zip.store("stored", &data).expect("room");
    zip.store_deflated("packed", &data).expect("room");
    zip.store("short", b"abc").expect("room");
    let bytes = zip.finish().expect("room");
    let archive = Archive::open(&bytes).expect("readable");
    let mut head = [0u8; 10];
    for name in ["stored", "packed"] {
        assert_eq!(archive.head(name, &mut head), Ok(Some(10)), "{name}");
        assert_eq!(head, data[..10], "{name}");
    }
    assert_eq!(archive.head("short", &mut head), Ok(Some(3)));
    assert_eq!(&head[..3], b"abc");
    assert_eq!(archive.head("absent", &mut head), Ok(None));
}
