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
use tairix_abi::window_ipc::{DragOwnIcon, DragOwnSource, ICON_PATH_MAX};
use tairix_abi::{SYSTEM_COMMAND_STORE, SYSTEM_COMPONENT_STORE, SYSTEM_SERVICE_STORE};
use tairix_fsmeta::preset::acorn::Filetype;
use tairix_icon::{FolderSample, IconKind, IconRequest, SampleCard};

use super::{
    ancestry, canonical_filetype, drag_picture, entry_icon, folder_sample, media_for_document,
    media_for_entry, media_for_filetype, media_for_name, media_for_named, name_endings,
    BlankDocument, Ending, Family, MediaType, Site, ALL, EXTENSION_TABLE, FILETYPE_TABLE,
};
use crate::entry::{Entry, EntryKind, LinkTarget, Occupancy};
use crate::presentation::Pictures;

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
    ("mod.wasm", MediaType::Wasm, IconKind::WasmModule),
    ("image.elf", MediaType::Elf, IconKind::ElfExecutable),
    ("notes.txt", MediaType::TextPlain, IconKind::Text),
    ("guide.rst", MediaType::TextPlain, IconKind::Text),
    ("boot.log", MediaType::TextPlain, IconKind::Text),
    ("Cargo.toml", MediaType::Toml, IconKind::Text),
    ("display.ini", MediaType::TextPlain, IconKind::Text),
    ("session.cfg", MediaType::TextPlain, IconKind::Text),
    ("network.conf", MediaType::TextPlain, IconKind::Text),
    ("README.md", MediaType::TextMarkdown, IconKind::Text),
    ("book.markdown", MediaType::TextMarkdown, IconKind::Text),
    ("rows.csv", MediaType::TextCsv, IconKind::TextCsv),
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
    ("release.zip", MediaType::ArchiveZip, IconKind::ArchiveZip),
    ("backup.tar", MediaType::ArchiveTar, IconKind::ArchiveTar),
    ("dump.gz", MediaType::ArchiveGzip, IconKind::ArchiveGzip),
    ("bundle.tgz", MediaType::ArchiveGzip, IconKind::ArchiveGzip),
    ("source.xz", MediaType::ArchiveXz, IconKind::ArchiveXz),
    ("source.txz", MediaType::ArchiveXz, IconKind::ArchiveXz),
    ("old.bz2", MediaType::ArchiveBzip2, IconKind::ArchiveBzip2),
    ("old.tbz2", MediaType::ArchiveBzip2, IconKind::ArchiveBzip2),
    ("old.tbz", MediaType::ArchiveBzip2, IconKind::ArchiveBzip2),
    ("new.zst", MediaType::ArchiveZstd, IconKind::ArchiveZstd),
    ("pack.7z", MediaType::Archive7z, IconKind::Archive7z),
    ("disk.rar", MediaType::ArchiveRar, IconKind::ArchiveRar),
    ("games.spk", MediaType::ArchiveSpark, IconKind::ArchiveSpark),
    ("apps.arc", MediaType::ArcFs, IconKind::ArchiveArcFs),
    ("Workbench.hdf", MediaType::RawDiskImage, IconKind::DiskHard),
    ("install.iso", MediaType::CdImage, IconKind::DiskOptical),
    (
        "Lemmings.adf",
        MediaType::RawFloppyDiskImage,
        IconKind::DiskFloppy,
    ),
    (
        "Elite.adl",
        MediaType::RawFloppyDiskImage,
        IconKind::DiskFloppy,
    ),
    (
        "Welcome.adm",
        MediaType::RawFloppyDiskImage,
        IconKind::DiskFloppy,
    ),
    (
        "Startup.ads",
        MediaType::RawFloppyDiskImage,
        IconKind::DiskFloppy,
    ),
    ("Fixed.img", MediaType::RawDiskImage, IconKind::DiskHard),
    (
        "Dos.ima",
        MediaType::RawFloppyDiskImage,
        IconKind::DiskFloppy,
    ),
    (
        "Boot.vfd",
        MediaType::RawFloppyDiskImage,
        IconKind::DiskFloppy,
    ),
    (
        "Games.flp",
        MediaType::RawFloppyDiskImage,
        IconKind::DiskFloppy,
    ),
    ("Original.imd", MediaType::ImageDisk, IconKind::DiskFloppy),
    ("Elite.ssd", MediaType::AcornDfsDisc, IconKind::DiskFloppy),
    ("Welcome.dsd", MediaType::AcornDfsDisc, IconKind::DiskFloppy),
    ("Repton.uef", MediaType::TapeUef, IconKind::Archive),
    ("Recorded.csw", MediaType::TapeCsw, IconKind::Archive),
    ("song.mp3", MediaType::AudioMpeg, IconKind::Audio),
    ("broadcast.mp2", MediaType::AudioMpeg, IconKind::Audio),
    ("tape.mp1", MediaType::AudioMpeg, IconKind::Audio),
    ("clip.mpa", MediaType::AudioMpeg, IconKind::Audio),
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
    ("film.mpg", MediaType::VideoMpeg, IconKind::Video),
    ("film.mpeg", MediaType::VideoMpeg, IconKind::Video),
    ("plugin.lua", MediaType::TextLua, IconKind::Text),
    ("tool.pl", MediaType::TextPerl, IconKind::Text),
    ("Module.pm", MediaType::TextPerl, IconKind::Text),
    ("rules.mk", MediaType::TextMakefile, IconKind::TextMakefile),
    ("build.mak", MediaType::TextMakefile, IconKind::TextMakefile),
    ("paper.tex", MediaType::TextTex, IconKind::Text),
    ("paper.ltx", MediaType::TextTex, IconKind::Text),
    ("letter.rtf", MediaType::Rtf, IconKind::Text),
    ("page.ps", MediaType::PostScript, IconKind::PostScript),
    ("figure.eps", MediaType::PostScript, IconKind::PostScript),
    ("job.pcl", MediaType::Pcl, IconKind::PrintData),
    ("scan.jp2", MediaType::ImageJpeg2000, IconKind::Image),
    ("scan.j2k", MediaType::ImageJpeg2000, IconKind::Image),
    ("art.psd", MediaType::ImagePhotoshop, IconKind::Image),
    ("old.pcx", MediaType::ImagePcx, IconKind::Image),
    ("Sans.ttf", MediaType::FontTtf, IconKind::Font),
    ("Sans.otf", MediaType::FontOtf, IconKind::Font),
    ("Sans.woff", MediaType::FontWoff, IconKind::Font),
    ("app.jar", MediaType::JavaArchive, IconKind::JavaArchive),
    ("loop.aif", MediaType::AudioAiff, IconKind::Audio),
    ("loop.aiff", MediaType::AudioAiff, IconKind::Audio),
    ("loop.aifc", MediaType::AudioAiff, IconKind::Audio),
    ("tune.mid", MediaType::AudioMidi, IconKind::Audio),
    ("tune.midi", MediaType::AudioMidi, IconKind::Audio),
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
        MediaType::TairixScreensaver,
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
    assert_eq!(media_for_name("notes.txt,123"), Some(MediaType::TextPlain));
    assert_eq!(media_for_name("Sprites,ff9.png"), Some(MediaType::ImagePng));
    assert_eq!(media_for_name("a,b,ff9"), Some(MediaType::ImageSprite));
    assert_eq!(media_for_name("Track,123"), None);
    assert_eq!(media_for_name(",b60"), None, "a file type needs a stem");
}

/// A type is saved typed by its file type and, unless RISC OS is its home,
/// named by its extensions; a sprite file is named by its type alone.
#[test]
fn a_save_format_is_typed_by_the_registry() {
    use super::save_format;
    let png = save_format(MediaType::ImagePng);
    assert_eq!(png.filetype(), Filetype::new(0xB60));
    assert_eq!(png.extensions().collect::<Vec<_>>(), [".png"]);
    let jpeg = save_format(MediaType::ImageJpeg);
    assert_eq!(jpeg.extensions().collect::<Vec<_>>(), [".jpg", ".jpeg"]);
    let sprite = save_format(MediaType::ImageSprite);
    assert_eq!(sprite.filetype(), Filetype::new(0xFF9));
    assert_eq!(sprite.extensions().count(), 0, "named by its type alone");
    let markdown = save_format(MediaType::TextMarkdown);
    assert_eq!(markdown.filetype(), None, "RISC OS gives it no type");
    assert!(markdown.extensions().count() > 0);
}

/// Every type's save format keeps each extension the registry names it by, as
/// many as a save pick holds: none is refused by the pick's own checks.
#[test]
fn a_save_format_keeps_every_extension_the_registry_names() {
    use super::{save_format, FILETYPE_NAMED};
    use tairix_abi::window_ipc::SAVE_EXTENSIONS_MAX;
    for media in ALL {
        let named: Vec<String> = if FILETYPE_NAMED.contains(media) {
            Vec::new()
        } else {
            EXTENSION_TABLE
                .iter()
                .filter(|(held, _)| held == media)
                .flat_map(|(_, extensions)| extensions.iter())
                .take(SAVE_EXTENSIONS_MAX)
                .map(|extension| alloc::format!(".{extension}"))
                .collect()
        };
        let format = save_format(*media);
        assert_eq!(format.extensions().collect::<Vec<_>>(), named, "{media:?}");
    }
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
    // A component is started by the system too, never by a user's open.
    assert_eq!(
        media_for_entry(&bundle("desktop.app"), &components(SYSTEM_COMPONENT_STORE)),
        MediaType::TairixService
    );

    for store in [
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

/// A bundle in a screensaver store, the system's or a user's own, is a
/// screensaver: the desktop starts it, never a user's open.
#[test]
fn a_bundle_in_a_screensaver_store_is_a_screensaver() {
    for store in [
        "/System/Screensavers",
        "/Users/ada/Library/Screensavers",
        "/Users/ada/Library/Screensavers/kept",
    ] {
        let saver = media_for_entry(&bundle("Waves.app"), &components(store));
        assert_eq!(saver, MediaType::TairixScreensaver, "{store}");
        assert_eq!(saver.as_str(), "application/x-tairix-screensaver");
        assert_eq!(saver.icon(), IconKind::Screensaver);
    }
    assert_eq!(
        media_for_entry(&bundle("Waves.app"), &components("/Users/ada/Library")),
        MediaType::TairixApp,
        "beside the store is no store"
    );
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
    MediaType::TextLua,
    MediaType::TextPerl,
    MediaType::TextMakefile,
    MediaType::TextTex,
    MediaType::RiscOsBasicText,
    MediaType::Rtf,
    MediaType::ShellScript,
    MediaType::RiscOsObey,
    MediaType::RiscOsDesktop,
    MediaType::RiscOsCommand,
    MediaType::PostScript,
    MediaType::ImageSvg,
];

/// Each codec's file and the container it is carried in.
const CONTAINED: &[(MediaType, MediaType)] = &[
    (MediaType::AudioOpus, MediaType::AudioOgg),
    (MediaType::JavaArchive, MediaType::ArchiveZip),
    (MediaType::RiscOsDesktop, MediaType::RiscOsObey),
];

/// Each medium's image and the disk image it is a kind of.
const IMAGED: &[(MediaType, MediaType)] = &[
    (MediaType::CdImage, MediaType::RawDiskImage),
    (MediaType::RawFloppyDiskImage, MediaType::RawDiskImage),
];

/// What is not a stream of bytes: a directory, and a bundle, which is one.
const NOT_BYTES: &[MediaType] = &[
    MediaType::InodeDirectory,
    MediaType::TairixApp,
    MediaType::TairixService,
    MediaType::TairixScreensaver,
];

#[test]
fn every_type_narrower_than_bytes_names_what_it_is_a_kind_of() {
    for media in TEXTUAL {
        assert!(
            ancestry(*media).any(|step| step == MediaType::TextPlain),
            "{media:?}"
        );
    }
    for (narrow, broad) in CONTAINED.iter().chain(IMAGED) {
        assert_eq!(narrow.parent(), Some(*broad), "{narrow:?}");
    }
    // The converse: every other file type is bytes and nothing narrower.
    for media in ALL {
        let narrower = TEXTUAL.contains(media)
            || CONTAINED
                .iter()
                .chain(IMAGED)
                .any(|(narrow, _)| narrow == media);
        if !narrower && !NOT_BYTES.contains(media) && *media != MediaType::ApplicationOctetStream {
            assert_eq!(
                media.parent(),
                Some(MediaType::ApplicationOctetStream),
                "{media:?}"
            );
        }
    }
}

/// A listing's type column orders by these names, so no two types may read
/// alike, and none is empty or longer than the column shows whole.
#[test]
fn every_type_has_a_name_of_its_own_that_a_type_column_shows_whole() {
    let mut seen: Vec<&str> = Vec::new();
    for media in ALL {
        let name = media.name();
        assert!(!name.is_empty() && name.len() <= 32, "{media:?}: {name:?}");
        assert!(
            !seen.contains(&name),
            "{media:?} reads as another type: {name}"
        );
        seen.push(name);
    }
}

#[test]
fn every_file_is_at_last_bytes_and_nothing_else_is() {
    for media in ALL {
        let root = ancestry(*media).last().expect("the walk yields the type");
        if NOT_BYTES.contains(media) {
            assert_eq!(root, *media, "{media:?} is no stream of bytes");
        } else {
            assert_eq!(root, MediaType::ApplicationOctetStream, "{media:?}");
        }
    }
    assert_eq!(MediaType::ApplicationOctetStream.parent(), None);
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
        [
            MediaType::ImageSvg,
            MediaType::Xml,
            MediaType::TextPlain,
            MediaType::ApplicationOctetStream
        ]
    );
    assert_eq!(
        ancestry(MediaType::TextPlain).collect::<Vec<_>>(),
        [MediaType::TextPlain, MediaType::ApplicationOctetStream]
    );
}

#[test]
fn distinct_types_deliberately_share_one_icon() {
    // One family draws one glyph while each type keeps its own identity — the
    // icon mapping is many-to-one, the vocabulary is not.
    let share_the_text_glyph = [
        MediaType::TextPlain,
        MediaType::TextMarkdown,
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
    // Each archive format has its labelled master, and every one of them
    // falls back to the one archive glyph.
    for media in archives {
        assert_ne!(media.icon(), IconKind::Archive, "{media:?}");
        assert_eq!(
            tairix_icon::builtin_icon(media.icon(), tairix_raster::Color::rgb(0, 0, 0)),
            tairix_icon::builtin_icon(IconKind::Archive, tairix_raster::Color::rgb(0, 0, 0)),
            "{media:?}"
        );
    }
    assert_eq!(MediaType::RiscOsArchive.icon(), IconKind::Archive);
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
            MediaType::InodeDirectory | MediaType::ApplicationOctetStream | MediaType::RiscOsData
        );
        assert_eq!(media.family().is_none(), none, "{media:?}");
    }
}

const PICTURES: &str = "/Users/ann/Home/Pictures";

/// The file every listed picture names.
const LISTED: FileId = FileId {
    volume: [7; 16],
    node: 42,
};

/// `entry`'s icon, and whether it asks for a thumbnail under its own path —
/// listed out of [`PICTURES`].
fn pictured(entry: &Entry) -> (IconKind, bool) {
    let mut scratch = String::new();
    let (kind, request) = entry_icon(
        PICTURES,
        &components(PICTURES),
        entry,
        Pictures::ALL,
        &mut scratch,
    );
    let path = alloc::format!("{PICTURES}/{}", entry.name());
    (
        kind,
        request == IconRequest::thumbnail(kind, &path, entry.stamp()),
    )
}

/// A picture file asks for its own content, keyed by the file, path, size and
/// time its listing reports, so a changed or replaced file is decoded afresh.
#[test]
fn a_picture_file_asks_for_its_own_content() {
    let written = Time64::from_secs(1_700_000_000);
    let photo = Entry::new("cat.jpg", EntryKind::File, 4096, written).with_id(LISTED);
    let (kind, thumbnail) = pictured(&photo);
    assert_eq!(
        kind,
        MediaType::ImageJpeg.icon(),
        "drawn as its class meanwhile"
    );
    assert!(thumbnail);
}

/// A sprite area holds many pictures, so it draws the sprite file's own icon
/// whether its name or the filetype its volume records says what it is.
#[test]
fn a_sprite_area_draws_its_class_picture_never_a_thumbnail() {
    let named = Entry::new("Sprites,ff9", EntryKind::File, 64, Time64::UNIX_EPOCH).with_id(LISTED);
    let typed = Entry::new("Sprites", EntryKind::File, 64, Time64::UNIX_EPOCH)
        .with_id(LISTED)
        .with_filetype(filetype(0xFF9));
    for sprites in [named, typed] {
        assert_eq!(pictured(&sprites), (IconKind::ImageSprite, false));
    }
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
    ];
    for media in ALL {
        assert_eq!(
            media.has_thumbnail(),
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
        let (kind, request) = entry_icon(
            PICTURES,
            &components(PICTURES),
            &entry,
            Pictures::ALL,
            &mut scratch,
        );
        assert_eq!(request, IconRequest::kind(kind), "{}", entry.name());
    }
}

/// A picture file or a folder whose own picture is left off is asked for as
/// its kind; with it on, the folder asks for what it holds.
#[test]
fn a_picture_left_off_is_asked_for_as_its_kind() {
    let off = Pictures {
        thumbnails: false,
        folder_contents: false,
    };
    let at = components(PICTURES);
    let mut scratch = String::new();
    let photo = Entry::new("cat.jpg", EntryKind::File, 4096, Time64::UNIX_EPOCH).with_id(LISTED);
    let (kind, request) = entry_icon(PICTURES, &at, &photo, off, &mut scratch);
    assert_eq!(request, IconRequest::kind(kind));

    let mut folder = Entry::directory("Trips");
    folder.set_occupancy(crate::entry::Occupancy::NonEmpty(
        tairix_icon::FolderSample::new([tairix_icon::SampleCard::Kind(IconKind::Text)]),
    ));
    let (kind, request) = entry_icon(PICTURES, &at, &folder, off, &mut scratch);
    assert_eq!(kind, IconKind::FolderFilled);
    assert_eq!(request, IconRequest::kind(IconKind::FolderFilled));
    let (_, request) = entry_icon(PICTURES, &at, &folder, Pictures::ALL, &mut scratch);
    assert_ne!(request, IconRequest::kind(IconKind::FolderFilled));
}

/// A mounted volume's root is drawn as its drive, even once probed as holding
/// something: its picture is the drive's own artwork, never a folder card.
#[test]
fn a_volume_root_is_pictured_as_its_drive() {
    let storage = components("/Storage");
    let mut root = Entry::directory("Backup");
    root.set_occupancy(crate::entry::Occupancy::NonEmpty(
        tairix_icon::FolderSample::default(),
    ));
    root.set_drive(Some(&crate::entry::DriveMark {
        icon: IconKind::DiskNvme,
        facts: crate::places::VolumeFacts {
            filesystem: String::from("arxfs"),
            title: String::new(),
            class: tairix_abi::sysinfo::VolumeClass::Local,
            free: Some(1 << 30),
            availability: tairix_abi::sysinfo::MountAvailability::Available,
            owner: None,
            volume_id: [0; 16],
            identity: [0; 16],
            read_only: false,
            titled: false,
            title_writes: false,
        },
    }));
    let mut scratch = String::new();
    let (kind, request) = entry_icon("/Storage", &storage, &root, Pictures::ALL, &mut scratch);
    assert_eq!(kind, IconKind::DiskNvme);
    assert_eq!(request, IconRequest::kind(IconKind::DiskNvme));
    assert_eq!(super::icon_for_entry(&root, &storage), IconKind::DiskNvme);

    root.set_drive(None);
    assert_eq!(
        super::icon_for_entry(&root, &storage),
        IconKind::FolderFilled
    );
}

/// A listed bundle asks for its own icon at the version its listing reports,
/// so one replaced or changed where its folder sees it is decoded afresh
/// rather than served its predecessor's picture. A link carries its own
/// stamp, not its target's, and a listing that named no file gives nothing to
/// key on, so each asks for the bundle at no version.
#[test]
fn a_listed_bundle_asks_for_its_icon_at_the_version_listed() {
    const APPS: &str = "/Storage/B/Apps";
    let mut scratch = String::new();
    let written = Time64::from_secs(1_700_000_000);
    let path = alloc::format!("{APPS}/One.app");

    let bundle = Entry::new("One.app", EntryKind::Bundle, 4096, written).with_id(LISTED);
    let (kind, request) = entry_icon(
        APPS,
        &components(APPS),
        &bundle,
        Pictures::ALL,
        &mut scratch,
    );
    assert_eq!(
        request,
        IconRequest::listed_bundle(kind, &path, bundle.stamp())
    );

    let link =
        Entry::new("One.app", EntryKind::Link(LinkTarget::Bundle), 16, written).with_id(LISTED);
    let unnamed = Entry::new("One.app", EntryKind::Bundle, 4096, written);
    for entry in [link, unnamed] {
        let (kind, request) =
            entry_icon(APPS, &components(APPS), &entry, Pictures::ALL, &mut scratch);
        assert_eq!(
            request,
            IconRequest::bundle(kind, &path),
            "{:?}",
            entry.kind()
        );
    }
}

/// A shortcut is pictured where its activation reaches what it names: at its
/// target where that names another root, which no walk through the shortcut
/// follows — an application shortcut's alias spelling, a RISC OS application
/// on a volume by its alias — and through itself where a walk follows it.
#[test]
fn a_shortcut_naming_another_root_is_pictured_at_its_target() {
    const DESK: &str = "/Users/ann/Desktop";
    let mut scratch = String::new();
    let written = Time64::from_secs(1_700_000_000);
    let at = components(DESK);

    let app = Entry::new("Editor", EntryKind::Link(LinkTarget::Bundle), 16, written)
        .with_id(LISTED)
        .with_target("System:/Applications/Editor.app");
    let (kind, request) = entry_icon(DESK, &at, &app, Pictures::ALL, &mut scratch);
    assert_eq!(
        request,
        IconRequest::bundle(kind, "/System/Applications/Editor.app")
    );

    let sprite = Entry::new("Edit", EntryKind::Link(LinkTarget::Directory), 16, written)
        .with_id(LISTED)
        .with_target("Disc:/Apps/!Edit");
    let (kind, request) = entry_icon(DESK, &at, &sprite, Pictures::ALL, &mut scratch);
    assert_eq!(
        request,
        IconRequest::riscos_application(kind, "Disc:/Apps/!Edit", None)
    );

    let followed = Entry::new("Tools", EntryKind::Link(LinkTarget::Bundle), 16, written)
        .with_id(LISTED)
        .with_target("/System/Applications/Tools.app");
    let (kind, request) = entry_icon(DESK, &at, &followed, Pictures::ALL, &mut scratch);
    assert_eq!(
        request,
        IconRequest::bundle(kind, "/Users/ann/Desktop/Tools"),
        "a walk through the shortcut follows its target"
    );
}

/// Each layout that names what a file is, found by where a folder stands.
#[test]
fn a_folder_s_site_is_the_layout_it_stands_in() {
    use super::Site;
    use tairix_abi::ProgramKind;

    let bundle = |kind, code| Site::Bundle { kind, code };
    for (path, site) in [
        ("/Users/ann/Home", Site::Plain),
        ("/System/Drivers", Site::Drivers),
        ("/System/Drivers/input/usb_hid", Site::Drivers),
        ("/System/Services", Site::StartedStore),
        ("/System/Components", Site::StartedStore),
        ("/System/Screensavers", Site::SaverStore),
        ("/Users/ann/Library/Screensavers", Site::SaverStore),
        ("/Users/ann/Library/Screensavers/kept", Site::SaverStore),
        ("/Users/ann/Library/Apps", Site::Plain),
        (
            "/Users/ann/Library/Screensavers/kept/Waves.app",
            bundle(Some(ProgramKind::Screensaver), false),
        ),
        (
            "/System/Screensavers/Starfield.app",
            bundle(Some(ProgramKind::Screensaver), false),
        ),
        (
            "/Users/ann/Library/Screensavers/Waves.app",
            bundle(Some(ProgramKind::Screensaver), false),
        ),
        (
            "/System/Applications/Paint.app",
            bundle(Some(ProgramKind::Application), false),
        ),
        (
            "/System/Applications/Paint.app/Code",
            bundle(Some(ProgramKind::Application), true),
        ),
        ("/System/Applications/Paint.app/Resources", Site::Plain),
        (
            "/System/Commands/ls.app",
            bundle(Some(ProgramKind::Command), false),
        ),
        (
            "/System/Services/fontd.app",
            bundle(Some(ProgramKind::Service), false),
        ),
        (
            "/Users/ann/Applications/games/Chess.app",
            bundle(Some(ProgramKind::Application), false),
        ),
        (
            "/Users/ann/Commands/tool.app",
            bundle(Some(ProgramKind::Command), false),
        ),
        ("/Users/ann/Home/Builds/Mine.app", bundle(None, false)),
    ] {
        assert_eq!(Site::of(&components(path)), site, "{path}");
    }
}

/// What a file is in each layout, and the picture it is drawn as: a driver's
/// image, a program by the kind of bundle it runs, and a bundle's manifest.
#[test]
fn a_layout_names_its_own_files_and_pictures_its_programs_by_kind() {
    for (folder, name, media, icon) in [
        (
            "/System/Drivers/input/usb_hid",
            "Run",
            MediaType::TairixDriver,
            IconKind::Driver,
        ),
        (
            "/System/Applications/Paint.app",
            "Run",
            MediaType::TairixRxe,
            IconKind::ApplicationProgram,
        ),
        (
            "/Users/ann/Commands/tool.app",
            "Run",
            MediaType::TairixRxe,
            IconKind::CommandProgram,
        ),
        (
            "/System/Services/fontd.app",
            "Run",
            MediaType::TairixRxe,
            IconKind::Executable,
        ),
        (
            "/System/Applications/Paint.app/Code",
            "filters",
            MediaType::TairixRxe,
            IconKind::ApplicationProgram,
        ),
        (
            "/System/Applications/Paint.app",
            "AppInfo",
            MediaType::TairixAppInfo,
            IconKind::Manifest,
        ),
        (
            "/Users/ann/Home",
            "Run",
            MediaType::ApplicationOctetStream,
            IconKind::File,
        ),
        (
            "/System/Drivers/input/usb_hid",
            "notes.txt",
            MediaType::TextPlain,
            IconKind::Text,
        ),
    ] {
        let parent = components(folder);
        let file = Entry::file(name);
        assert_eq!(media_for_entry(&file, &parent), media, "{folder}/{name}");
        assert_eq!(
            super::icon_for_entry(&file, &parent),
            icon,
            "{folder}/{name}"
        );
    }
}

#[test]
fn a_disk_image_or_a_served_archive_holds_a_volume_and_nothing_else_does() {
    const VOLUMES: [MediaType; 11] = [
        MediaType::RawDiskImage,
        MediaType::CdImage,
        MediaType::RawFloppyDiskImage,
        MediaType::ArchiveZip,
        MediaType::ArchiveTar,
        MediaType::ArchiveGzip,
        MediaType::ArchiveBzip2,
        MediaType::ArchiveRar,
        MediaType::ArchiveSpark,
        MediaType::RiscOsArchive,
        MediaType::ArcFs,
    ];
    for media in VOLUMES {
        assert!(media.holds_a_volume(), "{media:?}");
        assert_eq!(media.family(), Some(Family::Archive));
    }
    assert_eq!(MediaType::CdImage.parent(), Some(MediaType::RawDiskImage));
    assert_eq!(
        MediaType::RawFloppyDiskImage.parent(),
        Some(MediaType::RawDiskImage)
    );
    // A Java archive is a zip, but its own document; xz, Zstandard, 7z and
    // Squash are no format the system serves as a volume.
    for media in ALL.iter().copied().filter(|media| !VOLUMES.contains(media)) {
        assert!(!media.holds_a_volume(), "{media:?}");
    }
}

fn filetype(raw: u16) -> Filetype {
    Filetype::new(raw).expect("twelve bits")
}

/// RISC OS's filetypes type the files they name: as one of TAIRiX's own types
/// where that is the content, as RISC OS's own where nothing else is.
#[test]
fn a_risc_os_filetype_names_the_type_it_holds() {
    for (raw, media) in [
        (0xFFF, MediaType::TextPlain),
        (0xFDB, MediaType::TextPlain),
        (0xFFB, MediaType::RiscOsBasic),
        (0xFD1, MediaType::RiscOsBasicText),
        (0xFEB, MediaType::RiscOsObey),
        (0xFD7, MediaType::RiscOsObey),
        (0xFEA, MediaType::RiscOsDesktop),
        (0xFFE, MediaType::RiscOsCommand),
        (0xFF8, MediaType::RiscOsAbsolute),
        (0xFFC, MediaType::RiscOsUtility),
        (0xFFA, MediaType::RiscOsModule),
        (0xFF9, MediaType::ImageSprite),
        (0xAFF, MediaType::RiscOsDraw),
        (0xFEC, MediaType::RiscOsTemplate),
        (0xFF6, MediaType::RiscOsFont),
        (0xFF5, MediaType::PostScript),
        (0xFF4, MediaType::RiscOsPrintout),
        (0xFED, MediaType::RiscOsPalette),
        (0xFAF, MediaType::TextHtml),
        (0xC85, MediaType::ImageJpeg),
        (0xB60, MediaType::ImagePng),
        (0xADF, MediaType::Pdf),
        (0xDDC, MediaType::RiscOsArchive),
        (0x3FB, MediaType::ArcFs),
        (0xA91, MediaType::ArchiveZip),
        (0xFCA, MediaType::RiscOsSquash),
        (0xFCE, MediaType::RawFloppyDiskImage),
        (0xFCD, MediaType::RawDiskImage),
        (0xE1F, MediaType::Elf),
        (0x1AD, MediaType::AudioMpeg),
        (0xFFD, MediaType::RiscOsData),
    ] {
        assert_eq!(media_for_filetype(filetype(raw)), Some(media), "&{raw:03X}");
    }
    for unknown in [0x000, 0x123, 0xFF2] {
        assert_eq!(
            media_for_filetype(filetype(unknown)),
            None,
            "&{unknown:03X}"
        );
    }
    // Each RISC OS type is pictured as what it is.
    assert_eq!(MediaType::RiscOsBasic.icon(), IconKind::BasicProgram);
    assert_eq!(MediaType::RiscOsObey.icon(), IconKind::RiscOsObey);
    assert_eq!(MediaType::RiscOsCommand.icon(), IconKind::RiscOsCommand);
    assert_eq!(MediaType::RiscOsDesktop.icon(), IconKind::RiscOsDesktop);
    assert_eq!(MediaType::RiscOsData.icon(), IconKind::RiscOsData);
    assert_eq!(MediaType::RiscOsSquash.icon(), IconKind::RiscOsSquash);
    assert_eq!(MediaType::ArcFs.icon(), IconKind::ArchiveArcFs);
    assert_eq!(MediaType::RiscOsAbsolute.icon(), IconKind::RiscOsProgram);
    assert_eq!(MediaType::RiscOsModule.icon(), IconKind::RiscOsModule);
    assert_eq!(MediaType::RiscOsDraw.icon(), IconKind::Drawing);
    assert_eq!(MediaType::RiscOsTemplate.icon(), IconKind::WindowTemplate);
    assert_eq!(MediaType::FontTtf.icon(), IconKind::Font);
    assert_eq!(MediaType::RiscOsPrintout.icon(), IconKind::PrintData);
    assert_eq!(MediaType::RiscOsPalette.icon(), IconKind::Palette);
}

/// Every filetype the table lists reads back as its row's type, from a name
/// as from a volume, and the one a type is offered under after a comma is the
/// one RISC OS gives a new file of it.
#[test]
fn every_listed_filetype_reads_back_and_a_new_files_comes_first() {
    for (media, codes) in FILETYPE_TABLE {
        for code in *codes {
            let parsed = Filetype::from_hex(code.as_bytes()).expect("three hex digits");
            assert_eq!(media_for_filetype(parsed), Some(*media), "{code}");
            let named = alloc::format!("File,{code}");
            assert_eq!(media_for_name(&named), Some(*media), "{named}");
        }
        let after_comma: Vec<&str> = name_endings(*media)
            .filter(|(separator, _)| *separator == ',')
            .map(|(_, code)| code)
            .collect();
        assert_eq!(after_comma, [codes[0]], "{media:?}");
        assert_eq!(
            canonical_filetype(*media).map(Filetype::hex),
            Some(*<&[u8; 3]>::try_from(codes[0].as_bytes()).expect("three digits")),
            "{media:?}"
        );
    }
}

/// A file is typed by its name first and by the filetype its volume records
/// second, so RISC OS's Data never hides a name that says more; a layout's own
/// names, and a folder, are typed before either.
#[test]
fn a_name_types_a_file_before_its_recorded_filetype() {
    let text = Some(filetype(0xFFF));
    assert_eq!(
        media_for_document("ReadMe", text),
        Some(MediaType::TextPlain)
    );
    assert_eq!(
        media_for_document("notes.txt", Some(filetype(0xFFD))),
        Some(MediaType::TextPlain)
    );
    assert_eq!(
        media_for_document("index.html", Some(filetype(0xFFB))),
        Some(MediaType::TextHtml)
    );
    assert_eq!(
        media_for_document("Game.bas", Some(filetype(0xFFB))),
        Some(MediaType::RiscOsBasic),
        "an ending the registry does not know says nothing"
    );
    assert_eq!(media_for_document("Game", None), None);
    assert_eq!(media_for_document("Game", Some(filetype(0x123))), None);
    let bundle = components("/System/Applications/Edit.app");
    assert_eq!(
        media_for_named("Run", EntryKind::File, text, Site::of(&bundle)),
        MediaType::TairixRxe
    );
    assert_eq!(
        media_for_named("!Edit", EntryKind::Directory, text, Site::Plain),
        MediaType::InodeDirectory
    );
    let program = Entry::new("!RunImage", EntryKind::File, 1, Time64::UNIX_EPOCH)
        .with_filetype(filetype(0xFF8));
    assert_eq!(media_for_entry(&program, &[]), MediaType::RiscOsAbsolute);
    let image =
        Entry::new("Floppy", EntryKind::File, 1, Time64::UNIX_EPOCH).with_filetype(filetype(0xFCE));
    assert!(media_for_entry(&image, &[]).holds_a_volume());
}

/// A folder's picture of what it holds types each member as its own listing
/// would, its recorded filetype included.
#[test]
fn a_folder_sample_types_its_members_by_their_recorded_filetypes() {
    let record = |name: &'static str| tairix_abi::fs::DirEntry {
        kind: tairix_abi::fs::FileKind::Regular,
        size: 1,
        allocated: 0,
        modified: Time64::UNIX_EPOCH,
        id: FileId::NONE,
        nlink: 1,
        content_gen: 0,
        name: name.as_bytes(),
    };
    let members = [
        (record("ReadMe"), Some(filetype(0xFFF))),
        (record("Data"), None),
        (record("Prog"), Some(filetype(0xFFB))),
    ];
    let sample = folder_sample(&components("/disc"), members.iter().copied());
    let kinds: Vec<IconKind> = sample.cards().map(tairix_icon::DrawnCard::kind).collect();
    assert_eq!(kinds, [IconKind::Text, IconKind::BasicProgram]);
}

/// A RISC OS application asks for the sprite RISC OS's Filer shows it by, at
/// the version its listing named, falling back to the folder it is — to its
/// picture of what it holds where it has one. A link to one keys no version,
/// and a folder not named as one is a folder.
#[test]
fn a_risc_os_application_asks_for_its_own_sprite_then_the_folder_it_is() {
    const DISC: &str = "/Storage/disc/Apps";
    let mut scratch = String::new();
    let written = Time64::from_secs(1_700_000_000);
    let path = alloc::format!("{DISC}/!Edit");
    let mut app = Entry::new("!Edit", EntryKind::Directory, 0, written).with_id(LISTED);
    let (kind, request) = entry_icon(DISC, &components(DISC), &app, Pictures::ALL, &mut scratch);
    assert_eq!(kind, IconKind::Folder);
    assert_eq!(
        request,
        IconRequest::riscos_application(kind, &path, Some(app.stamp()))
    );

    let sample = FolderSample::new([SampleCard::Kind(IconKind::Text)]);
    app.set_occupancy(Occupancy::NonEmpty(sample.clone()));
    let (kind, request) = entry_icon(DISC, &components(DISC), &app, Pictures::ALL, &mut scratch);
    assert_eq!(kind, IconKind::FolderFilled);
    assert_eq!(
        request,
        IconRequest::riscos_application(kind, &path, Some(app.stamp())).or_folder(&sample)
    );

    let link = Entry::new(
        "Editor",
        EntryKind::Link(LinkTarget::Directory),
        16,
        written,
    )
    .with_id(LISTED)
    .with_target("../Tools/!Edit");
    let (kind, request) = entry_icon(DISC, &components(DISC), &link, Pictures::ALL, &mut scratch);
    let linked = alloc::format!("{DISC}/Editor");
    assert_eq!(
        request,
        IconRequest::riscos_application(kind, &linked, None)
    );

    for folder in [
        Entry::new("Edit", EntryKind::Directory, 0, written),
        Entry::new("!", EntryKind::Directory, 0, written),
        Entry::new("Edit", EntryKind::Link(LinkTarget::Directory), 0, written).with_target("Edit"),
    ] {
        let (kind, request) = entry_icon(
            DISC,
            &components(DISC),
            &folder,
            Pictures::ALL,
            &mut scratch,
        );
        assert_eq!(request, IconRequest::kind(kind), "{folder:?}");
    }
    let boot = Entry::new("!Boot", EntryKind::File, 0, written);
    assert!(!boot.is_riscos_application(), "a file is no application");
}

/// A dragged bundle carries its own picture — an application's, a command's —
/// by its path and the version listed, beside its class picture; one whose
/// path a drag cannot carry crosses as its class picture alone.
#[test]
fn a_dragged_bundle_carries_its_own_picture() {
    const APPS: &str = "/Users/ann/Applications";
    let mut scratch = String::new();
    let from = Some((3, -4));
    for (dir, name) in [(APPS, "Editor.app"), (SYSTEM_COMMAND_STORE, "ls.app")] {
        let entry = bundle(name).with_id(LISTED);
        let picture =
            drag_picture((dir, &components(dir)), &entry, from, &mut scratch).expect("a picture");
        let path = alloc::format!("{dir}/{name}");
        let own = DragOwnIcon::new(DragOwnSource::Bundle, &path, Some(entry.stamp()))
            .expect("an own picture");
        assert_eq!(
            picture.icon.as_str(),
            IconKind::AppBundle.asset_id(),
            "{name}"
        );
        assert_eq!((picture.from, picture.own), (from, Some(own)), "{name}");
    }

    let deep = alloc::format!("/Storage/{}", "d".repeat(ICON_PATH_MAX));
    let entry = bundle("Editor.app").with_id(LISTED);
    let picture =
        drag_picture((&deep, &components(&deep)), &entry, None, &mut scratch).expect("a picture");
    assert_eq!(picture.icon.as_str(), IconKind::AppBundle.asset_id());
    assert_eq!(
        picture.own, None,
        "a path past the bound carries no own picture"
    );
}

/// A dragged RISC OS application carries its own sprite by its path and the
/// version listed, its class picture the folder it is — the filled folder
/// where it holds something, never the folder's picture of what it holds.
#[test]
fn a_dragged_risc_os_application_carries_its_sprite() {
    const DISC: &str = "/Storage/disc/Apps";
    let mut scratch = String::new();
    let written = Time64::from_secs(1_700_000_000);
    let mut app = Entry::new("!Edit", EntryKind::Directory, 0, written).with_id(LISTED);
    let own = DragOwnIcon::new(
        DragOwnSource::RiscOsApplication,
        "/Storage/disc/Apps/!Edit",
        Some(app.stamp()),
    )
    .expect("an own picture");
    let picture =
        drag_picture((DISC, &components(DISC)), &app, None, &mut scratch).expect("a picture");
    assert_eq!(picture.icon.as_str(), IconKind::Folder.asset_id());
    assert_eq!(picture.own, Some(own));

    let sample = FolderSample::new([SampleCard::Kind(IconKind::Text)]);
    app.set_occupancy(Occupancy::NonEmpty(sample));
    let picture =
        drag_picture((DISC, &components(DISC)), &app, None, &mut scratch).expect("a picture");
    assert_eq!(picture.icon.as_str(), IconKind::FolderFilled.asset_id());
    assert_eq!(picture.own, Some(own), "still its sprite, never its fan");
}

/// A shortcut is named for itself, so what it shows is carried by the path it
/// resolves to, as its activation reaches it — an application's by its alias
/// spelling, a RISC OS application's by its relative one — at no version, a
/// link's listed stamp being its own.
#[test]
fn a_dragged_shortcut_carries_what_it_resolves_to() {
    const DESK: &str = "/Users/ann/Desktop";
    let mut scratch = String::new();
    let written = Time64::from_secs(1_700_000_000);
    let (bundle_icon, sprite_icon) = (DragOwnSource::Bundle, DragOwnSource::RiscOsApplication);
    let app = Entry::new("Editor", EntryKind::Link(LinkTarget::Bundle), 16, written)
        .with_id(LISTED)
        .with_target("System:/Applications/Editor.app");
    let sprite = Entry::new("Edit", EntryKind::Link(LinkTarget::Directory), 16, written)
        .with_id(LISTED)
        .with_target("../Tools/!Edit");
    for (entry, source, path) in [
        (app, bundle_icon, "/System/Applications/Editor.app"),
        (sprite, sprite_icon, "/Users/ann/Desktop/../Tools/!Edit"),
    ] {
        let picture =
            drag_picture((DESK, &components(DESK)), &entry, None, &mut scratch).expect("a picture");
        let own = DragOwnIcon::new(source, path, None).expect("an own picture");
        assert_eq!(picture.own, Some(own), "{}", entry.name());
    }
}

/// A file, a picture file and an occupied folder cross a drag as their class
/// picture alone: neither a thumbnail nor a folder's fan is carried.
#[test]
fn a_dragged_file_or_folder_carries_its_class_picture_alone() {
    let at = components(PICTURES);
    let mut scratch = String::new();
    let notes = Entry::new("notes.txt", EntryKind::File, 12, Time64::UNIX_EPOCH).with_id(LISTED);
    let photo = Entry::new("cat.jpg", EntryKind::File, 4096, Time64::UNIX_EPOCH).with_id(LISTED);
    let mut folder = Entry::directory("Trips");
    let sample = FolderSample::new([SampleCard::Kind(IconKind::Text)]);
    folder.set_occupancy(Occupancy::NonEmpty(sample));
    for entry in [notes, photo, folder] {
        let picture =
            drag_picture((PICTURES, &at), &entry, Some((0, 0)), &mut scratch).expect("a picture");
        let (kind, _) = entry_icon(PICTURES, &at, &entry, Pictures::ALL, &mut scratch);
        assert_eq!(picture.icon.as_str(), kind.asset_id(), "{}", entry.name());
        assert_eq!(picture.own, None, "{}", entry.name());
    }
}

#[test]
fn an_imagedisk_archive_is_a_floppy_record_but_no_volume() {
    assert_eq!(
        MediaType::ImageDisk.parent(),
        Some(MediaType::ApplicationOctetStream),
        "bytes of its own, not a raw image"
    );
    assert!(!MediaType::ImageDisk.holds_a_volume());
    assert_eq!(
        MediaType::from_media_str("application/x-imagedisk"),
        Some(MediaType::ImageDisk)
    );
    assert_eq!(
        media_for_name("TAIRIX.IMD"),
        Some(MediaType::ImageDisk),
        "an extension in any case"
    );
}

#[test]
fn bbc_micro_discs_and_tapes_are_their_own_types_and_no_volumes() {
    for (media, spelling) in [
        (MediaType::AcornDfsDisc, "application/x-acorn-dfs-disc"),
        (MediaType::TapeUef, "application/x-uef-tape"),
        (MediaType::TapeCsw, "application/x-csw-tape"),
    ] {
        assert_eq!(
            media.parent(),
            Some(MediaType::ApplicationOctetStream),
            "{spelling}"
        );
        assert!(!media.holds_a_volume(), "{spelling}");
        assert_eq!(MediaType::from_media_str(spelling), Some(media));
        assert_eq!(media.family(), Some(Family::Archive), "{spelling}");
    }
    assert_eq!(media_for_name("GAMES.SSD"), Some(MediaType::AcornDfsDisc));
}
