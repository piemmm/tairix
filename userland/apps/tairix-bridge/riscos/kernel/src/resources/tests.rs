//! The Messages module's tests: the ROM's files in one block, registered
//! while it lives and again when ResourceFS starts again.

use crate::filing::filetype;
use crate::filing::fs::stamp;
use crate::module::utility::UTILITY;
use crate::msgtrans::tests::session;
use crate::resourcefs::Object;
use crate::testkit::Session;

/// The file `name` finds, if one.
fn file(s: &mut Session, name: &[u8]) -> Option<crate::resourcefs::Entry> {
    match s.kernel.os().resource(name).expect("looked up") {
        Some(Object::File(entry)) => Some(entry),
        _ => None,
    }
}

#[test]
fn every_built_in_s_files_are_registered_stamped_as_the_rom_s_are() {
    let mut s = session();
    let roms: alloc::vec::Vec<_> = s
        .kernel
        .os()
        .world
        .modules
        .roms
        .iter()
        .map(|rom| rom.builtin)
        .collect();
    for builtin in roms {
        for resource in builtin.resources {
            let entry = file(&mut s, resource.name.as_bytes()).expect(resource.name);
            assert_eq!(
                s.read(entry.data, entry.length as usize),
                resource.data,
                "{}",
                resource.name
            );
            assert_eq!(entry.load >> 20, 0xFFF, "typed");
            assert_eq!((entry.load >> 8) & 0xFFF, resource.file_type);
            assert_eq!(
                s.peek(entry.data - 4),
                entry.length + 4,
                "its length again before its data"
            );
            assert_eq!(entry.attributes, 3);
        }
    }
    let entry = file(&mut s, UTILITY.resources[0].name.as_bytes()).expect("the kernel's");
    let stamp = (u64::from(entry.load & 0xFF) << 32) | u64::from(entry.exec);
    // 24 April 2024 at 16:29:06.62, in centiseconds since 1900.
    assert_eq!(stamp, (25_567 + 19_837) * 8_640_000 + 5_934_662);
}

#[test]
fn the_rom_s_files_go_with_messages_and_come_back_with_resourcefs() {
    let mut s = crate::cli::tests::session();
    let name = b"Resources.Global.Messages";
    crate::cli::tests::cli(&mut s, "RMKill Messages").expect("killed");
    assert_eq!(file(&mut s, name), None, "deregistered as it died");
    crate::cli::tests::cli(&mut s, "RMReInit Messages").expect("started");
    assert!(file(&mut s, name).is_some());
    crate::cli::tests::cli(&mut s, "RMReInit ResourceFS").expect("started again");
    assert!(
        file(&mut s, name).is_some(),
        "registered again as ResourceFS started"
    );
}

#[test]
fn the_wimp_s_sprites_are_a_sprite_area_of_each_type_large_and_small() {
    let mut s = session();
    let entry = file(&mut s, b"Resources.Wimp.Sprites22").expect("the Wimp's sprites");
    assert_eq!(
        stamp::type_of(entry.load),
        Some(filetype::SPRITE),
        "a sprite file"
    );
    let (count, first) = (s.peek(entry.data), s.peek(entry.data + 4));
    assert_eq!(first, 16, "the sprites after the header");
    let mut names = alloc::vec::Vec::new();
    let mut at = entry.data + first - 4;
    for _ in 0..count {
        let name = s.read(at + 4, 12);
        let name = name.split(|&b| b == 0).next().unwrap_or_default().to_vec();
        let mode = s.peek(at + 40);
        assert_eq!(
            mode,
            1 << 31 | 5 << 27 | 90 << 14 | 90 << 1 | 1,
            "16-bit colour, alpha, 90 dpi"
        );
        let side = s.peek(at + 20) + 1;
        let expected = if name.starts_with(b"small_") { 18 } else { 34 };
        assert_eq!(
            side,
            expected,
            "{}",
            alloc::string::String::from_utf8_lossy(&name)
        );
        names.push(name);
        at += s.peek(at);
    }
    assert_eq!(
        at,
        entry.data + s.peek(entry.data + 8) - 4,
        "the area ends where it says"
    );
    for name in [
        &b"file_fff"[..],
        b"small_fff",
        b"file_ffb",
        b"file_xxx",
        b"small_xxx",
    ] {
        assert!(
            names.iter().any(|n| n == name),
            "{}",
            alloc::string::String::from_utf8_lossy(name)
        );
    }
}
