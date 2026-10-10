//! Unit tests for the one content-type registry.
//!
//! The tests iterate the registry's own tables rather than a second copy of
//! them, so a row added to the registry is covered the moment it lands: the
//! extension coverage check walks [`EXTENSION_TABLE`] and the spelling
//! round-trip walks [`ALL`].

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::fs::FileId;
use tairix_abi::time::Time64;
use tairix_abi::SYSTEM_SERVICE_STORE;
use tairix_icon::{IconKind, IconRequest, Reading};

use super::{
    ancestry, entry_icon, media_for_entry, media_for_name, name_endings, BlankDocument, Ending,
    MediaType, ALL, EXTENSION_TABLE,
};
use crate::entry::{Entry, EntryKind, LinkTarget};

/// The root-first components of a directory path, as a listing carries them.
fn components(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// A `<Name>.app` bundle entry.
fn bundle(name: &str) -> Entry {
    Entry::new(name, EntryKind::Bundle, 0, Time64::UNIX_EPOCH)
}

/// One representative file name per recognised extension, with the type it
/// classifies as and the icon that type draws — the extension → type → icon
/// chain, spelled out end to end.
///
/// [`every_recognised_extension_has_a_row`] proves this covers
/// [`EXTENSION_TABLE`] exactly, so no extension can enter the registry without
/// its mapping being asserted here.
const ROWS: &[(&str, MediaType, IconKind)] = &[
    ("tool.rxe", MediaType::TairixRxe, IconKind::Executable),
    ("mod.wasm", MediaType::Wasm, IconKind::Executable),
    ("image.elf", MediaType::Elf, IconKind::Executable),
    ("notes.txt", MediaType::TextPlain, IconKind::Text),
    ("guide.rst", MediaType::TextPlain, IconKind::Text),
    ("boot.log", MediaType::TextPlain, IconKind::Text),
    ("Cargo.toml", MediaType::Toml, IconKind::Text),
    ("display.ini", MediaType::TextPlain, IconKind::Text),
    ("session.cfg", MediaType::TextPlain, IconKind::Text),
    ("network.conf", MediaType::TextPlain, IconKind::Text),
    ("README.md", MediaType::TextMarkdown, IconKind::Text),
    ("book.markdown", MediaType::TextMarkdown, IconKind::Text),
    ("rows.csv", MediaType::TextCsv, IconKind::Text),
    ("data.json", MediaType::Json, IconKind::Text),
    ("deploy.yaml", MediaType::Yaml, IconKind::Text),
    ("deploy.yml", MediaType::Yaml, IconKind::Text),
    ("style.css", MediaType::TextCss, IconKind::Text),
    ("app.js", MediaType::TextJavaScript, IconKind::Text),
    ("module.mjs", MediaType::TextJavaScript, IconKind::Text),
    ("tool.py", MediaType::TextPython, IconKind::Text),
    ("layout.xml", MediaType::Xml, IconKind::Text),
    ("index.html", MediaType::TextHtml, IconKind::TextHtml),
    ("index.htm", MediaType::TextHtml, IconKind::TextHtml),
    ("main.rs", MediaType::TextRust, IconKind::TextRust),
    ("Main.java", MediaType::TextJava, IconKind::TextJava),
    ("parse.c", MediaType::TextC, IconKind::Text),
    ("parse.h", MediaType::TextC, IconKind::Text),
    ("install.sh", MediaType::ShellScript, IconKind::ShellScript),
    ("manual.pdf", MediaType::Pdf, IconKind::Pdf),
    ("photo.png", MediaType::ImagePng, IconKind::ImagePng),
    ("scan.jpg", MediaType::ImageJpeg, IconKind::ImageJpeg),
    ("scan.jpeg", MediaType::ImageJpeg, IconKind::ImageJpeg),
    ("spin.gif", MediaType::ImageGif, IconKind::ImageGif),
    ("logo.svg", MediaType::ImageSvg, IconKind::ImageSvg),
    ("tile.spr", MediaType::ImageSprite, IconKind::ImageSprite),
    ("shot.bmp", MediaType::ImageBmp, IconKind::Image),
    ("app.ico", MediaType::ImageIcon, IconKind::Image),
    ("hero.webp", MediaType::ImageWebp, IconKind::Image),
    ("plate.tiff", MediaType::ImageTiff, IconKind::Image),
    ("plate.tif", MediaType::ImageTiff, IconKind::Image),
    ("layers.ora", MediaType::ImageOpenRaster, IconKind::Image),
    ("release.zip", MediaType::ArchiveZip, IconKind::Archive),
    ("backup.tar", MediaType::ArchiveTar, IconKind::Archive),
    ("dump.gz", MediaType::ArchiveGzip, IconKind::Archive),
    ("bundle.tgz", MediaType::ArchiveGzip, IconKind::Archive),
    ("source.xz", MediaType::ArchiveXz, IconKind::Archive),
    ("old.bz2", MediaType::ArchiveBzip2, IconKind::Archive),
    ("new.zst", MediaType::ArchiveZstd, IconKind::Archive),
    ("pack.7z", MediaType::Archive7z, IconKind::Archive),
    ("disk.rar", MediaType::ArchiveRar, IconKind::Archive),
    ("song.mp3", MediaType::AudioMpeg, IconKind::Audio),
    ("song.flac", MediaType::AudioFlac, IconKind::Audio),
    ("song.ogg", MediaType::AudioOgg, IconKind::Audio),
    ("song.oga", MediaType::AudioOgg, IconKind::Audio),
    ("song.opus", MediaType::AudioOpus, IconKind::Audio),
    ("song.wav", MediaType::AudioWav, IconKind::Audio),
    ("chime.au", MediaType::AudioAu, IconKind::Audio),
    ("chime.snd", MediaType::AudioAu, IconKind::Audio),
    ("song.aac", MediaType::AudioAac, IconKind::Audio),
    ("song.m4a", MediaType::AudioMp4, IconKind::Audio),
    ("film.mp4", MediaType::VideoMp4, IconKind::Video),
    ("film.m4v", MediaType::VideoMp4, IconKind::Video),
    ("film.webm", MediaType::VideoWebm, IconKind::Video),
    ("film.mkv", MediaType::VideoMatroska, IconKind::Video),
    ("film.mov", MediaType::VideoQuicktime, IconKind::Video),
    ("film.avi", MediaType::VideoAvi, IconKind::Video),
];

#[test]
fn every_row_maps_its_extension_through_its_type_to_its_icon() {
    for (name, media, icon) in ROWS {
        assert_eq!(media_for_name(name), Some(*media), "{name}");
        assert_eq!(media.icon(), *icon, "{name}");
        // The entry path agrees with the bare-name path for a regular file.
        assert_eq!(media_for_entry(&Entry::file(*name), &[]), *media, "{name}");
    }
}

#[test]
fn every_recognised_extension_has_a_row() {
    let mut listed = 0usize;
    for (media, exts) in EXTENSION_TABLE {
        for ext in *exts {
            listed += 1;
            let row = ROWS
                .iter()
                .find(|(name, _, _)| Ending::of(name).extension == Some(*ext));
            let (_, row_media, _) = row.unwrap_or_else(|| panic!("no row covers .{ext}"));
            assert_eq!(row_media, media, ".{ext}");
        }
    }
    // …and no row names an extension the registry does not carry.
    assert_eq!(ROWS.len(), listed);
}

#[test]
fn every_media_type_round_trips_through_its_spelling() {
    for media in ALL {
        assert_eq!(MediaType::from_media_str(media.as_str()), Some(*media));
    }
}

#[test]
fn every_spelling_is_distinct() {
    for (position, media) in ALL.iter().enumerate() {
        for other in &ALL[position + 1..] {
            assert_ne!(media.as_str(), other.as_str(), "{media:?} vs {other:?}");
        }
    }
}

#[test]
fn every_type_the_registry_can_produce_is_in_the_round_trip_table() {
    for (media, _) in EXTENSION_TABLE {
        assert!(ALL.contains(media), "{media:?}");
    }
    for media in [
        MediaType::InodeDirectory,
        MediaType::TairixApp,
        MediaType::TairixService,
        MediaType::ApplicationOctetStream,
    ] {
        assert!(ALL.contains(&media), "{media:?}");
    }
}

#[test]
fn a_risc_os_file_type_after_a_comma_names_the_media() {
    assert_eq!(media_for_name("Sprites,ff9"), Some(MediaType::ImageSprite));
    assert_eq!(media_for_name("Logo,B60"), Some(MediaType::ImagePng));
    assert_eq!(media_for_name("photo,c85"), Some(MediaType::ImageJpeg));
    assert_eq!(media_for_name("anim,695"), Some(MediaType::ImageGif));
    assert_eq!(media_for_name("scan,69C"), Some(MediaType::ImageBmp));
    assert_eq!(media_for_name("pages,ff0"), Some(MediaType::ImageTiff));
    assert_eq!(media_for_name("ReadMe,fff"), Some(MediaType::TextPlain));
    // A type the registry does not know leaves the extension to say.
    assert_eq!(media_for_name("notes.txt,ffb"), Some(MediaType::TextPlain));
    assert_eq!(media_for_name("Sprites,ff9.png"), Some(MediaType::ImagePng));
    assert_eq!(media_for_name("a,b,ff9"), Some(MediaType::ImageSprite));
    assert_eq!(media_for_name("Obey,feb"), None);
    assert_eq!(media_for_name(",b60"), None, "a file type needs a stem");
}

/// A name ends in a file type only after a stem and only as three hex
/// digits, and in an extension only after a stem: anything else ends it in
/// nothing.
#[test]
fn a_name_ends_only_in_a_file_type_or_an_extension_after_a_stem() {
    let both = Ending::of("notes.txt,ffb");
    assert_eq!(
        (both.stem, both.filetype, both.extension),
        ("notes", Some("ffb"), Some("txt"))
    );
    for bare in [
        "Smith, J",
        ",b60",
        ".png",
        "plain",
        "trailing.",
        "a,b6",
        "a,b600",
    ] {
        let ending = Ending::of(bare);
        assert!(ending.is_none(), "{bare}");
        assert_eq!(ending.stem, bare, "{bare}");
    }
    assert_eq!(Ending::of("a.tar.gz").stem, "a.tar");
    assert_eq!(Ending::of("my.photos").extension, Some("photos"));
    assert_eq!(Ending::of("Logo,B60").filetype, Some("B60"));
}

/// Every ending a type is known by names that type back, extensions first.
#[test]
fn a_types_name_endings_read_back_as_the_type() {
    for media in ALL {
        for (separator, code) in name_endings(*media) {
            let name = alloc::format!("picture{separator}{code}");
            assert_eq!(media_for_name(&name), Some(*media), "{name}");
        }
    }
    let png: Vec<(char, &str)> = name_endings(MediaType::ImagePng).collect();
    assert_eq!(png, [('.', "png"), (',', "b60")]);
    assert_eq!(name_endings(MediaType::InodeDirectory).count(), 0);
}

#[test]
fn spelling_and_extension_matching_ignore_ascii_case() {
    assert_eq!(media_for_name("PHOTO.PNG"), Some(MediaType::ImagePng));
    assert_eq!(media_for_name("Notes.TxT"), Some(MediaType::TextPlain));
    assert_eq!(media_for_name("READ.MD"), Some(MediaType::TextMarkdown));
    assert_eq!(media_for_name("A.ZiP"), Some(MediaType::ArchiveZip));
    assert_eq!(
        MediaType::from_media_str("IMAGE/PNG"),
        Some(MediaType::ImagePng)
    );
    assert_eq!(
        MediaType::from_media_str("Application/Json"),
        Some(MediaType::Json)
    );
}

#[test]
fn an_unknown_spelling_is_not_one_the_closed_registry_knows() {
    assert_eq!(MediaType::from_media_str("application/x-invented"), None);
    assert_eq!(MediaType::from_media_str("text"), None);
    assert_eq!(MediaType::from_media_str(""), None);
}

#[test]
fn a_name_with_no_usable_extension_has_no_type() {
    // No dot at all.
    assert_eq!(media_for_name("Makefile"), None);
    // A dotfile whose only dot starts the name.
    assert_eq!(media_for_name(".profile"), None);
    // A trailing dot with nothing after it is not an extension.
    assert_eq!(media_for_name("archive."), None);
    assert_eq!(media_for_name(""), None);
}

#[test]
fn a_dotfile_still_takes_a_further_extension() {
    assert_eq!(media_for_name(".config.toml"), Some(MediaType::Toml));
}

#[test]
fn the_last_extension_wins_for_a_multi_part_name() {
    assert_eq!(media_for_name("a.txt.zip"), Some(MediaType::ArchiveZip));
    assert_eq!(media_for_name("dump.tar.gz"), Some(MediaType::ArchiveGzip));
    assert_eq!(media_for_name("theme.dark.svg"), Some(MediaType::ImageSvg),);
}

#[test]
fn an_unrecognised_extension_falls_closed_to_the_generic_type_and_glyph() {
    assert_eq!(media_for_name("blob.qwerty"), None);
    let generic = media_for_entry(&Entry::file("blob.qwerty"), &[]);
    assert_eq!(generic, MediaType::ApplicationOctetStream);
    assert_eq!(generic.as_str(), "application/octet-stream");
    assert_eq!(generic.icon(), IconKind::File);
    // A file with no extension at all takes the same generic answer.
    assert_eq!(
        media_for_entry(&Entry::file("Makefile"), &[]),
        MediaType::ApplicationOctetStream
    );
}

#[test]
fn a_directory_is_a_directory_whatever_it_is_named() {
    let parent = components("/Users/ada");
    for name in ["Documents", "backup.zip", "notes.txt", "Editor.app"] {
        let media = media_for_entry(&Entry::directory(name), &parent);
        assert_eq!(media, MediaType::InodeDirectory, "{name}");
        assert_eq!(media.as_str(), "inode/directory");
        assert_eq!(media.icon(), IconKind::Folder);
    }
}

#[test]
fn a_bundle_in_the_service_store_is_a_service_and_elsewhere_an_application() {
    let service = media_for_entry(&bundle("fontd.app"), &components(SYSTEM_SERVICE_STORE));
    assert_eq!(service, MediaType::TairixService);
    assert_eq!(service.as_str(), "application/x-tairix-service");
    assert_eq!(service.icon(), IconKind::ServiceBundle);

    for store in [
        "/Apps",
        "/System/Commands",
        "/System/Applications",
        "/Users/ada/Commands",
        "/Users/ada/Applications",
    ] {
        let app = media_for_entry(&bundle("Editor.app"), &components(store));
        assert_eq!(app, MediaType::TairixApp, "{store}");
        assert_eq!(app.as_str(), "application/x-tairix-app");
        assert_eq!(app.icon(), IconKind::AppBundle);
    }
}

#[test]
fn only_the_exact_service_store_path_makes_a_bundle_a_service() {
    // A prefix of the store path, a longer path beneath it, and a same-named
    // directory elsewhere are all ordinary application stores: the match is the
    // whole component sequence, never a prefix.
    for parent in [
        "/System",
        "/System/Services/fontd.app",
        "/Users/ada/Services",
        "/",
    ] {
        assert_eq!(
            media_for_entry(&bundle("fontd.app"), &components(parent)),
            MediaType::TairixApp,
            "{parent}"
        );
    }
}

/// Every (file name, media-type spelling) pair the pre-registry association
/// table produced and the registry still produces unchanged.
///
/// This is the guard against a *fold*: collapsing two content types into one
/// because they happen to draw the same glyph would silently stop an
/// application whose manifest declares the vanished type from being offered for
/// its own files. Only [`MediaType::icon`] is allowed to be many-to-one.
const PRESERVED: &[(&str, &str)] = &[
    ("notes.txt", "text/plain"),
    ("boot.log", "text/plain"),
    ("guide.rst", "text/plain"),
    ("display.ini", "text/plain"),
    ("session.cfg", "text/plain"),
    ("network.conf", "text/plain"),
    ("README.md", "text/markdown"),
    ("book.markdown", "text/markdown"),
    ("rows.csv", "text/csv"),
    ("data.json", "application/json"),
    ("deploy.yaml", "application/yaml"),
    ("deploy.yml", "application/yaml"),
    ("layout.xml", "application/xml"),
    ("photo.png", "image/png"),
    ("scan.jpg", "image/jpeg"),
    ("scan.jpeg", "image/jpeg"),
    ("spin.gif", "image/gif"),
    ("shot.bmp", "image/bmp"),
    ("logo.svg", "image/svg+xml"),
    ("app.ico", "image/vnd.microsoft.icon"),
    ("hero.webp", "image/webp"),
    ("plate.tiff", "image/tiff"),
    ("plate.tif", "image/tiff"),
    ("layers.ora", "image/openraster"),
    ("release.zip", "application/zip"),
    ("backup.tar", "application/x-tar"),
    ("dump.gz", "application/gzip"),
    ("bundle.tgz", "application/gzip"),
    ("source.xz", "application/x-xz"),
    ("old.bz2", "application/x-bzip2"),
    ("new.zst", "application/zstd"),
    ("pack.7z", "application/x-7z-compressed"),
    ("disk.rar", "application/vnd.rar"),
    ("tool.rxe", "application/x-tairix-rxe"),
    ("mod.wasm", "application/wasm"),
    ("image.elf", "application/x-elf"),
];

/// The names the registry types *more specifically* than the pre-registry
/// table did: `(name, the older broader spelling, the spelling now produced)`.
///
/// Each is a form with its own honest type, so the registry names it rather
/// than lumping it in with plain text or its container. Refining a name is only
/// admissible because it takes nothing away: the broader spelling is still a
/// type the registry knows *and* is still an ancestor of the refined one, so an
/// application declaring it keeps matching these names. The test below asserts
/// that no-regression property rather than merely recording the change.
const REFINED: &[(&str, &str, &str)] = &[
    ("main.rs", "text/plain", "text/x-rust"),
    ("install.sh", "text/plain", "application/x-shellscript"),
    ("parse.c", "text/plain", "text/x-c"),
    ("parse.h", "text/plain", "text/x-c"),
    ("Cargo.toml", "text/plain", "application/toml"),
    ("song.opus", "audio/ogg", "audio/opus"),
];

#[test]
fn the_association_vocabulary_never_shrinks() {
    for (name, spelling) in PRESERVED {
        assert_eq!(
            media_for_name(name).map(MediaType::as_str),
            Some(*spelling),
            "{name}"
        );
    }
    for (name, broader, spelling) in REFINED {
        let refined = media_for_name(name).expect("a refined name still types");
        assert_eq!(refined.as_str(), *spelling, "{name}");
        let broader = MediaType::from_media_str(broader).expect("the broader type is still known");
        // Refinement takes nothing away: the broader type a manifest may declare
        // is an ancestor of the refined one, so it still matches this name.
        assert!(ancestry(refined).any(|step| step == broader), "{name}");
    }
}

/// Every type that *is* readable text without *being* `text/plain`: the
/// textual types that name a parent.
///
/// [`every_textual_type_reaches_plain_text_and_a_codec_its_container`]
/// checks this list and [`CONTAINED`] in both directions, so a textual type
/// cannot lose its parent (silently narrowing what opens it) and no other
/// binary type can gain one.
const TEXTUAL: &[MediaType] = &[
    MediaType::TextMarkdown,
    MediaType::TextCsv,
    MediaType::Json,
    MediaType::Yaml,
    MediaType::Toml,
    MediaType::Xml,
    MediaType::TextHtml,
    MediaType::TextCss,
    MediaType::TextJavaScript,
    MediaType::TextRust,
    MediaType::TextJava,
    MediaType::TextC,
    MediaType::TextPython,
    MediaType::ShellScript,
    MediaType::ImageSvg,
];

/// Each codec's file and the container it is carried in.
const CONTAINED: &[(MediaType, MediaType)] = &[(MediaType::AudioOpus, MediaType::AudioOgg)];

#[test]
fn every_textual_type_reaches_plain_text_and_a_codec_its_container() {
    for media in TEXTUAL {
        let root = ancestry(*media).last().expect("the walk yields the type");
        assert_eq!(root, MediaType::TextPlain, "{media:?}");
    }
    for (codec, container) in CONTAINED {
        assert_eq!(codec.parent(), Some(*container), "{codec:?}");
    }
    // The converse: nothing else subclasses anything, so plain text itself and
    // every other binary type are roots.
    for media in ALL {
        if !TEXTUAL.contains(media) && !CONTAINED.iter().any(|(codec, _)| codec == media) {
            assert_eq!(media.parent(), None, "{media:?}");
        }
    }
}

#[test]
fn the_subclass_chain_terminates_for_every_type() {
    for media in ALL {
        let mut seen: Vec<MediaType> = Vec::new();
        let mut step = Some(*media);
        // A chain longer than the registry itself must repeat a type, so this
        // bound is only ever reached by a cycle.
        for _ in 0..=ALL.len() {
            let Some(current) = step else { break };
            assert!(!seen.contains(&current), "{media:?} revisits {current:?}");
            seen.push(current);
            step = current.parent();
        }
        assert_eq!(step, None, "{media:?} does not terminate");
        // The bounded walk sees exactly the same chain.
        assert_eq!(ancestry(*media).collect::<Vec<_>>(), seen, "{media:?}");
    }
}

#[test]
fn a_multi_step_chain_widens_one_level_at_a_time() {
    // SVG is XML, and XML is text: the walk is a chain, not a flat parent.
    assert_eq!(
        ancestry(MediaType::ImageSvg).collect::<Vec<_>>(),
        [MediaType::ImageSvg, MediaType::Xml, MediaType::TextPlain]
    );
    assert_eq!(
        ancestry(MediaType::TextPlain).collect::<Vec<_>>(),
        [MediaType::TextPlain]
    );
}

#[test]
fn distinct_types_deliberately_share_one_icon() {
    // One family draws one glyph while each type keeps its own identity — the
    // icon mapping is many-to-one, the vocabulary is not.
    let share_the_text_glyph = [
        MediaType::TextPlain,
        MediaType::TextMarkdown,
        MediaType::TextCsv,
        MediaType::Json,
        MediaType::Yaml,
        MediaType::Toml,
        MediaType::Xml,
        MediaType::TextCss,
        MediaType::TextJavaScript,
        MediaType::TextC,
        MediaType::TextPython,
    ];
    for media in share_the_text_glyph {
        assert_eq!(media.icon(), IconKind::Text, "{media:?}");
    }
    for (position, media) in share_the_text_glyph.iter().enumerate() {
        for other in &share_the_text_glyph[position + 1..] {
            assert_ne!(media.as_str(), other.as_str());
        }
    }

    let archives = [
        MediaType::ArchiveZip,
        MediaType::ArchiveTar,
        MediaType::ArchiveGzip,
        MediaType::ArchiveXz,
        MediaType::ArchiveBzip2,
        MediaType::ArchiveZstd,
        MediaType::Archive7z,
        MediaType::ArchiveRar,
    ];
    for media in archives {
        assert_eq!(media.icon(), IconKind::Archive, "{media:?}");
    }
    for (position, media) in archives.iter().enumerate() {
        for other in &archives[position + 1..] {
            assert_ne!(media.as_str(), other.as_str());
        }
    }
}

#[test]
fn every_type_that_starts_empty_has_an_extension_to_name_it_by() {
    for media in ALL.iter().copied() {
        assert_eq!(
            BlankDocument::of(media).is_some(),
            media.blank_noun().is_some(),
            "{media:?}"
        );
    }
    let text = BlankDocument::of(MediaType::TextPlain).expect("text starts empty");
    assert_eq!(text.noun(), "Text Document");
    assert_eq!(text.extension(), "txt");
    assert_eq!(
        media_for_name("New Text Document.txt"),
        Some(MediaType::TextPlain)
    );
}

#[test]
fn a_type_whose_empty_file_is_not_a_document_makes_none() {
    for media in [
        MediaType::Json,
        MediaType::Xml,
        MediaType::TextHtml,
        MediaType::TextC,
        MediaType::ImagePng,
        MediaType::ImageSvg,
        MediaType::ArchiveZip,
        MediaType::InodeDirectory,
        MediaType::ApplicationOctetStream,
    ] {
        assert_eq!(BlankDocument::of(media), None, "{media:?}");
    }
}

#[test]
fn every_blank_noun_is_distinct_and_fits_a_menu_row_and_a_name() {
    use tairix_abi::window_ipc::APP_MENU_LABEL_MAX;
    use tairix_abi::FS_NAME_MAX;

    let documents: Vec<BlankDocument> = ALL
        .iter()
        .filter_map(|media| BlankDocument::of(*media))
        .collect();
    for (at, document) in documents.iter().enumerate() {
        assert!(document.noun().len() <= APP_MENU_LABEL_MAX, "{document:?}");
        let longest = alloc::format!(
            "New {} {}.{}",
            document.noun(),
            usize::MAX,
            document.extension()
        );
        assert!(longest.len() <= FS_NAME_MAX, "{document:?}");
        assert!(
            documents[..at]
                .iter()
                .all(|earlier| earlier.noun() != document.noun()),
            "{document:?}"
        );
    }
}

/// Every type but a directory and content of no recognised type belongs to a
/// family, so every file a folder's picture could show a card for has one.
#[test]
fn every_type_but_a_directory_and_unknown_content_has_a_family() {
    for media in ALL {
        let none = matches!(
            media,
            MediaType::InodeDirectory | MediaType::ApplicationOctetStream
        );
        assert_eq!(media.family().is_none(), none, "{media:?}");
    }
}

const PICTURES: &str = "/Users/ann/UserFiles/Pictures";

/// The file every listed picture names.
const LISTED: FileId = FileId {
    volume: [7; 16],
    node: 42,
};

/// `entry`'s icon, and how the thumbnail it asks for under its own path is
/// read, or `None` when it asks for none — listed out of [`PICTURES`].
fn pictured(entry: &Entry) -> (IconKind, Option<Reading>) {
    let mut scratch = String::new();
    let (kind, request) = entry_icon(PICTURES, &components(PICTURES), entry, &mut scratch);
    let path = alloc::format!("{PICTURES}/{}", entry.name());
    let reading = [Reading::Signature, Reading::Sprite]
        .into_iter()
        .find(|&reading| request == IconRequest::thumbnail(kind, &path, entry.stamp(), reading));
    (kind, reading)
}

/// A picture file asks for its own content, keyed by the file, path, size and
/// time its listing reports, so a changed or replaced file is decoded afresh.
#[test]
fn a_picture_file_asks_for_its_own_content() {
    let written = Time64::from_secs(1_700_000_000);
    let photo = Entry::new("cat.jpg", EntryKind::File, 4096, written).with_id(LISTED);
    let (kind, reading) = pictured(&photo);
    assert_eq!(
        kind,
        MediaType::ImageJpeg.icon(),
        "drawn as its class meanwhile"
    );
    assert_eq!(reading, Some(Reading::Signature));
}

/// A sprite area carries no signature, so it is read as one only because its
/// name says it is.
#[test]
fn a_sprite_area_is_read_as_its_name_says() {
    let sprites =
        Entry::new("Sprites,ff9", EntryKind::File, 64, Time64::UNIX_EPOCH).with_id(LISTED);
    let (_, reading) = pictured(&sprites);
    assert_eq!(reading, Some(Reading::Sprite));
}

/// Only a picture the shared raster decoders read has a thumbnail; a link's
/// listed size and time are its own, so it draws its class picture.
#[test]
fn only_a_listed_picture_the_decoders_read_has_a_thumbnail() {
    let readable = [
        MediaType::ImagePng,
        MediaType::ImageJpeg,
        MediaType::ImageGif,
        MediaType::ImageBmp,
        MediaType::ImageIcon,
        MediaType::ImageWebp,
        MediaType::ImageTiff,
        MediaType::ImageOpenRaster,
        MediaType::ImageSprite,
    ];
    for media in ALL {
        assert_eq!(
            media.thumbnail().is_some(),
            readable.contains(media),
            "{}",
            media.as_str()
        );
    }
    let mut scratch = String::new();
    for entry in [
        Entry::new("notes.txt", EntryKind::File, 10, Time64::UNIX_EPOCH),
        Entry::new("logo.svg", EntryKind::File, 10, Time64::UNIX_EPOCH),
        Entry::new(
            "cat.jpg",
            EntryKind::Link(LinkTarget::File),
            10,
            Time64::UNIX_EPOCH,
        ),
        // A listing that named no file gives the open nothing to check.
        Entry::new("dog.jpg", EntryKind::File, 10, Time64::UNIX_EPOCH),
    ]
    .map(|entry| {
        if entry.name() == "dog.jpg" {
            entry
        } else {
            entry.with_id(LISTED)
        }
    }) {
        let (kind, request) = entry_icon(PICTURES, &components(PICTURES), &entry, &mut scratch);
        assert_eq!(request, IconRequest::kind(kind), "{}", entry.name());
    }
}
