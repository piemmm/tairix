//! The one content-type registry the browser classifies entries through.
//!
//! A listed entry has exactly one [`MediaType`], named by its IANA (or TAIRiX
//! vendor) media-type spelling. That one classification drives *both* the
//! file-type glyph the grid tile draws ([`MediaType::icon`]) and the "Open
//! With…" association vocabulary a bundle declares in its `AppInfo`
//! ([`MediaType::as_str`] / [`MediaType::from_media_str`]), so the icon a name
//! gets and the applications offered for it can never drift apart: they read
//! the same closed table. It also says which types New ▸ can make
//! ([`BlankDocument`]).
//!
//! This is the one classifier the windowed file manager and the trusted file
//! picker share (`plans/NEW-FILEMANAGER.md`), so the two can never disagree
//! about what a name *is*. It is a **display and offer hint only**: it decides
//! an icon and which applications are *offered*, never an operation. What a
//! file is for the purposes of reading, launching, or permission is the VFS's
//! and the launcher's job; and the signed load gate still verifies and
//! capability-checks whichever bundle the user picks.
//!
//! # Two independent facts
//!
//! An entry's *type* and the *icon* drawn for it are separate: the type is the
//! association vocabulary a bundle's `AppInfo` declares, the icon is only how
//! the type is pictured. Many types deliberately share one glyph — every
//! textual and structured-config type draws [`IconKind::Text`] — but they stay
//! distinct types, because merging two would silently stop an application that
//! declares one of them from matching its own files.
//!
//! # Subclassing
//!
//! A concrete textual format *is* plain text as well as being itself, so
//! [`MediaType::parent`] names the broader type each one specialises — the
//! same subclass relation the freedesktop.org shared-mime-info database uses
//! (`text/x-csrc` is a subclass of `text/plain`). Association matching walks
//! that chain, so a text editor declaring `text/plain` opens a `.rs` file
//! while a Rust IDE declaring `text/x-rust` still claims it more
//! specifically. Naming a format precisely therefore costs nothing: the
//! broader declaration keeps matching.
//!
//! # Closed by design
//!
//! [`MediaType`] is a closed enum. An unrecognised extension classifies as
//! [`MediaType::ApplicationOctetStream`] (drawn with the generic file glyph)
//! rather than becoming a free-form string at a draw or association site, and
//! an unrecognised media-type spelling is simply not one this registry knows
//! ([`from_media_str`](MediaType::from_media_str) returns `None`) — fail
//! closed, never a guess.

use alloc::string::String;

use tairix_abi::fs::{DirEntry, FileKind};
use tairix_abi::SYSTEM_SERVICE_STORE;
use tairix_icon::{
    DocumentStamp, FolderSample, IconKind, IconRequest, Reading, SampleCard, Thumbnail,
};

use crate::entry::{Entry, EntryKind};

/// A content type TAIRiX recognises, named by its media type.
///
/// Closed by design: an unrecognised spelling classifies as
/// [`ApplicationOctetStream`](Self::ApplicationOctetStream) rather than
/// becoming free-form text at a draw or association site.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum MediaType {
    /// A directory the browser can descend into (`inode/directory`).
    InodeDirectory,
    /// A `<Name>.app` application bundle (`application/x-tairix-app`).
    TairixApp,
    /// A `<Name>.app` bundle listed from the system service store
    /// (`application/x-tairix-service`).
    TairixService,
    /// The TAIRiX executable envelope (`application/x-tairix-rxe`).
    TairixRxe,
    /// A bare WebAssembly module (`application/wasm`).
    Wasm,
    /// A raw ELF image (`application/x-elf`).
    Elf,
    /// Plain text and the config formats with no type of their own
    /// (`text/plain`).
    TextPlain,
    /// A Markdown document (`text/markdown`).
    TextMarkdown,
    /// Comma-separated values (`text/csv`).
    TextCsv,
    /// A JSON document (`application/json`).
    Json,
    /// A YAML document (`application/yaml`).
    Yaml,
    /// A TOML document (`application/toml`).
    Toml,
    /// An XML document (`application/xml`).
    Xml,
    /// An HTML document (`text/html`).
    TextHtml,
    /// A CSS style sheet (`text/css`).
    TextCss,
    /// JavaScript source (`text/javascript`).
    TextJavaScript,
    /// Rust source (`text/x-rust`).
    TextRust,
    /// Java source (`text/x-java`).
    TextJava,
    /// C source or a C header (`text/x-c`).
    TextC,
    /// Python source (`text/x-python`).
    TextPython,
    /// A shell script (`application/x-shellscript`).
    ShellScript,
    /// A PDF document (`application/pdf`).
    Pdf,
    /// A PNG image (`image/png`).
    ImagePng,
    /// A JPEG image (`image/jpeg`).
    ImageJpeg,
    /// A GIF image (`image/gif`).
    ImageGif,
    /// An SVG image (`image/svg+xml`).
    ImageSvg,
    /// A RISC OS sprite image (`image/x-riscos-sprite`).
    ImageSprite,
    /// A BMP image (`image/bmp`).
    ImageBmp,
    /// A Windows icon image (`image/vnd.microsoft.icon`).
    ImageIcon,
    /// A WebP image (`image/webp`).
    ImageWebp,
    /// A TIFF image (`image/tiff`).
    ImageTiff,
    /// An OpenRaster layered image (`image/openraster`).
    ImageOpenRaster,
    /// A ZIP archive (`application/zip`).
    ArchiveZip,
    /// A tar archive (`application/x-tar`).
    ArchiveTar,
    /// A gzip archive (`application/gzip`).
    ArchiveGzip,
    /// An xz archive (`application/x-xz`).
    ArchiveXz,
    /// A bzip2 archive (`application/x-bzip2`).
    ArchiveBzip2,
    /// A Zstandard archive (`application/zstd`).
    ArchiveZstd,
    /// A 7z archive (`application/x-7z-compressed`).
    Archive7z,
    /// A RAR archive (`application/vnd.rar`).
    ArchiveRar,
    /// An MPEG audio file — MP3 (`audio/mpeg`).
    AudioMpeg,
    /// A FLAC audio file (`audio/flac`).
    AudioFlac,
    /// An Ogg audio file (`audio/ogg`).
    AudioOgg,
    /// An Opus audio file, Opus in Ogg (`audio/opus`).
    AudioOpus,
    /// A WAVE audio file (`audio/wav`).
    AudioWav,
    /// A Sun/NeXT audio file (`audio/basic`).
    AudioAu,
    /// An AAC audio stream (`audio/aac`).
    AudioAac,
    /// An MPEG-4 audio file (`audio/mp4`).
    AudioMp4,
    /// An MPEG-4 video (`video/mp4`).
    VideoMp4,
    /// A `WebM` video (`video/webm`).
    VideoWebm,
    /// A Matroska video (`video/x-matroska`).
    VideoMatroska,
    /// A `QuickTime` video (`video/quicktime`).
    VideoQuicktime,
    /// An AVI video (`video/x-msvideo`).
    VideoAvi,
    /// Any content of no recognised type (`application/octet-stream`).
    ApplicationOctetStream,
}

impl MediaType {
    /// The IANA (or TAIRiX vendor) media-type spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InodeDirectory => "inode/directory",
            Self::TairixApp => "application/x-tairix-app",
            Self::TairixService => "application/x-tairix-service",
            Self::TairixRxe => "application/x-tairix-rxe",
            Self::Wasm => "application/wasm",
            Self::Elf => "application/x-elf",
            Self::TextPlain => "text/plain",
            Self::TextMarkdown => "text/markdown",
            Self::TextCsv => "text/csv",
            Self::Json => "application/json",
            Self::Yaml => "application/yaml",
            Self::Toml => "application/toml",
            Self::Xml => "application/xml",
            Self::TextHtml => "text/html",
            Self::TextCss => "text/css",
            Self::TextJavaScript => "text/javascript",
            Self::TextRust => "text/x-rust",
            Self::TextJava => "text/x-java",
            Self::TextC => "text/x-c",
            Self::TextPython => "text/x-python",
            Self::ShellScript => "application/x-shellscript",
            Self::Pdf => "application/pdf",
            Self::ImagePng => "image/png",
            Self::ImageJpeg => "image/jpeg",
            Self::ImageGif => "image/gif",
            Self::ImageSvg => "image/svg+xml",
            Self::ImageSprite => "image/x-riscos-sprite",
            Self::ImageBmp => "image/bmp",
            Self::ImageIcon => "image/vnd.microsoft.icon",
            Self::ImageWebp => "image/webp",
            Self::ImageTiff => "image/tiff",
            Self::ImageOpenRaster => "image/openraster",
            Self::ArchiveZip => "application/zip",
            Self::ArchiveTar => "application/x-tar",
            Self::ArchiveGzip => "application/gzip",
            Self::ArchiveXz => "application/x-xz",
            Self::ArchiveBzip2 => "application/x-bzip2",
            Self::ArchiveZstd => "application/zstd",
            Self::Archive7z => "application/x-7z-compressed",
            Self::ArchiveRar => "application/vnd.rar",
            Self::AudioMpeg => "audio/mpeg",
            Self::AudioFlac => "audio/flac",
            Self::AudioOgg => "audio/ogg",
            Self::AudioOpus => "audio/opus",
            Self::AudioWav => "audio/wav",
            Self::AudioAu => "audio/basic",
            Self::AudioAac => "audio/aac",
            Self::AudioMp4 => "audio/mp4",
            Self::VideoMp4 => "video/mp4",
            Self::VideoWebm => "video/webm",
            Self::VideoMatroska => "video/x-matroska",
            Self::VideoQuicktime => "video/quicktime",
            Self::VideoAvi => "video/x-msvideo",
            Self::ApplicationOctetStream => "application/octet-stream",
        }
    }

    /// The registry entry for a media-type spelling, if it is one we know.
    ///
    /// The match is ASCII-case-insensitive so a type reads the same however it
    /// was cased, mirroring the association matching. An unknown spelling is
    /// not one this closed registry knows: `None`, never a guess.
    #[must_use]
    pub fn from_media_str(text: &str) -> Option<Self> {
        ALL.iter()
            .copied()
            .find(|media| media.as_str().eq_ignore_ascii_case(text))
    }

    /// The broader type this one is a subclass of, if any.
    ///
    /// A concrete textual format is *also* plain text — a `.rs` file is Rust
    /// source and readable text both — so every textual type names
    /// [`TextPlain`](Self::TextPlain) (directly, or through an intermediate
    /// such as SVG's `application/xml`). A codec's file is likewise its
    /// container's: an Opus file is an Ogg stream, so
    /// [`AudioOpus`](Self::AudioOpus) names [`AudioOgg`](Self::AudioOgg).
    /// Every other binary type names `None`.
    /// The relation exists so naming a format precisely never narrows what can
    /// open it: association matching walks this chain, offering an application
    /// that declares an ancestor type while ranking one that declares the exact
    /// type ahead of it.
    ///
    /// The chain is **finite and acyclic**: each step names a strictly broader
    /// type and the broadest ones return `None`, so a walk from any variant
    /// reaches `None` in a bounded number of steps.
    #[must_use]
    pub const fn parent(self) -> Option<Self> {
        match self {
            Self::TextMarkdown
            | Self::TextCsv
            | Self::Json
            | Self::Yaml
            | Self::Toml
            | Self::Xml
            | Self::TextHtml
            | Self::TextCss
            | Self::TextJavaScript
            | Self::TextRust
            | Self::TextJava
            | Self::TextC
            | Self::TextPython
            | Self::ShellScript => Some(Self::TextPlain),
            Self::ImageSvg => Some(Self::Xml),
            Self::AudioOpus => Some(Self::AudioOgg),
            Self::InodeDirectory
            | Self::TairixApp
            | Self::TairixService
            | Self::TairixRxe
            | Self::Wasm
            | Self::Elf
            | Self::TextPlain
            | Self::Pdf
            | Self::ImagePng
            | Self::ImageJpeg
            | Self::ImageGif
            | Self::ImageSprite
            | Self::ImageBmp
            | Self::ImageIcon
            | Self::ImageWebp
            | Self::ImageTiff
            | Self::ImageOpenRaster
            | Self::ArchiveZip
            | Self::ArchiveTar
            | Self::ArchiveGzip
            | Self::ArchiveXz
            | Self::ArchiveBzip2
            | Self::ArchiveZstd
            | Self::Archive7z
            | Self::ArchiveRar
            | Self::AudioMpeg
            | Self::AudioFlac
            | Self::AudioOgg
            | Self::AudioWav
            | Self::AudioAu
            | Self::AudioAac
            | Self::AudioMp4
            | Self::VideoMp4
            | Self::VideoWebm
            | Self::VideoMatroska
            | Self::VideoQuicktime
            | Self::VideoAvi
            | Self::ApplicationOctetStream => None,
        }
    }

    /// The icon that represents this content type.
    ///
    /// **This mapping is deliberately many-to-one, and it is the only part of
    /// the registry allowed to be.** A type's identity (its spelling, the
    /// vocabulary bundles declare associations in) and the picture drawn for it
    /// are two independent facts: several distinct types share one glyph — every
    /// textual and structured-config type draws [`IconKind::Text`], every
    /// archive draws [`IconKind::Archive`] — and that is correct. Never
    /// "simplify" two types into one because they draw the same icon: an
    /// application whose manifest declares the type that disappeared would
    /// silently stop matching its own files.
    ///
    /// How a thumbnail of a file of this type reads its format, or `None` for
    /// a type the shared raster decoders do not read (`lib/image`).
    #[must_use]
    pub const fn thumbnail(self) -> Option<Reading> {
        match self {
            Self::ImagePng
            | Self::ImageJpeg
            | Self::ImageGif
            | Self::ImageBmp
            | Self::ImageIcon
            | Self::ImageWebp
            | Self::ImageTiff
            | Self::ImageOpenRaster => Some(Reading::Signature),
            Self::ImageSprite => Some(Reading::Sprite),
            _ => None,
        }
    }

    /// A fine-grained kind shares its broad family's glyph when the system
    /// ships no distinct raster artwork for it, so the returned kind is always
    /// drawable (`lib/icon`).
    #[must_use]
    pub const fn icon(self) -> IconKind {
        match self {
            Self::InodeDirectory => IconKind::Folder,
            Self::TairixApp => IconKind::AppBundle,
            Self::TairixService => IconKind::ServiceBundle,
            Self::TairixRxe | Self::Wasm | Self::Elf => IconKind::Executable,
            Self::TextPlain
            | Self::TextMarkdown
            | Self::TextCsv
            | Self::Json
            | Self::Yaml
            | Self::Toml
            | Self::Xml
            | Self::TextCss
            | Self::TextJavaScript
            | Self::TextC
            | Self::TextPython => IconKind::Text,
            Self::TextHtml => IconKind::TextHtml,
            Self::TextRust => IconKind::TextRust,
            Self::TextJava => IconKind::TextJava,
            Self::ShellScript => IconKind::ShellScript,
            Self::Pdf => IconKind::Pdf,
            Self::ImagePng => IconKind::ImagePng,
            Self::ImageJpeg => IconKind::ImageJpeg,
            Self::ImageGif => IconKind::ImageGif,
            Self::ImageSvg => IconKind::ImageSvg,
            Self::ImageSprite => IconKind::ImageSprite,
            Self::ImageBmp
            | Self::ImageIcon
            | Self::ImageWebp
            | Self::ImageTiff
            | Self::ImageOpenRaster => IconKind::Image,
            Self::ArchiveZip
            | Self::ArchiveTar
            | Self::ArchiveGzip
            | Self::ArchiveXz
            | Self::ArchiveBzip2
            | Self::ArchiveZstd
            | Self::Archive7z
            | Self::ArchiveRar => IconKind::Archive,
            Self::AudioMpeg
            | Self::AudioFlac
            | Self::AudioOgg
            | Self::AudioOpus
            | Self::AudioWav
            | Self::AudioAu
            | Self::AudioAac
            | Self::AudioMp4 => IconKind::Audio,
            Self::VideoMp4
            | Self::VideoWebm
            | Self::VideoMatroska
            | Self::VideoQuicktime
            | Self::VideoAvi => IconKind::Video,
            Self::ApplicationOctetStream => IconKind::File,
        }
    }

    /// What a new, empty document of this type is called, or `None` when an
    /// empty file is not a complete document of it.
    #[must_use]
    pub const fn blank_noun(self) -> Option<&'static str> {
        match self {
            Self::TextPlain => Some("Text Document"),
            Self::TextMarkdown => Some("Markdown Document"),
            Self::TextCsv => Some("CSV Document"),
            Self::Yaml => Some("YAML Document"),
            Self::Toml => Some("TOML Document"),
            Self::TextCss => Some("Style Sheet"),
            Self::TextJavaScript => Some("JavaScript File"),
            Self::TextRust => Some("Rust Source File"),
            Self::TextJava => Some("Java Source File"),
            Self::TextPython => Some("Python Script"),
            Self::ShellScript => Some("Shell Script"),
            // JSON needs a value, XML a root element, HTML a doctype and a
            // title, and ISO C a declaration; every binary format needs its
            // header.
            Self::Json
            | Self::Xml
            | Self::TextHtml
            | Self::TextC
            | Self::InodeDirectory
            | Self::TairixApp
            | Self::TairixService
            | Self::TairixRxe
            | Self::Wasm
            | Self::Elf
            | Self::Pdf
            | Self::ImagePng
            | Self::ImageJpeg
            | Self::ImageGif
            | Self::ImageSvg
            | Self::ImageSprite
            | Self::ImageBmp
            | Self::ImageIcon
            | Self::ImageWebp
            | Self::ImageTiff
            | Self::ImageOpenRaster
            | Self::ArchiveZip
            | Self::ArchiveTar
            | Self::ArchiveGzip
            | Self::ArchiveXz
            | Self::ArchiveBzip2
            | Self::ArchiveZstd
            | Self::Archive7z
            | Self::ArchiveRar
            | Self::AudioMpeg
            | Self::AudioFlac
            | Self::AudioOgg
            | Self::AudioOpus
            | Self::AudioWav
            | Self::AudioAu
            | Self::AudioAac
            | Self::AudioMp4
            | Self::VideoMp4
            | Self::VideoWebm
            | Self::VideoMatroska
            | Self::VideoQuicktime
            | Self::VideoAvi
            | Self::ApplicationOctetStream => None,
        }
    }
}

/// A family of file, as a folder's picture of what it holds shows one: the
/// broad kinds things are filed by.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Family {
    /// Images.
    Picture,
    /// Text, source code and structured text.
    Text,
    /// Paged documents.
    Document,
    /// Sound.
    Audio,
    /// Video.
    Video,
    /// Archives.
    Archive,
    /// Programs: bundles and executables.
    Program,
}

impl Family {
    /// Every family.
    const ALL: [Self; 7] = [
        Self::Picture,
        Self::Text,
        Self::Document,
        Self::Audio,
        Self::Video,
        Self::Archive,
        Self::Program,
    ];

    /// How many families there are.
    const COUNT: usize = Self::ALL.len();

    const fn index(self) -> usize {
        self as usize
    }
}

impl MediaType {
    /// The family a file of this type belongs to, or `None` for one a
    /// folder's picture shows no card for: a directory, or content of no
    /// recognised type.
    #[must_use]
    pub const fn family(self) -> Option<Family> {
        match self {
            Self::ImagePng
            | Self::ImageJpeg
            | Self::ImageGif
            | Self::ImageSvg
            | Self::ImageSprite
            | Self::ImageBmp
            | Self::ImageIcon
            | Self::ImageWebp
            | Self::ImageTiff
            | Self::ImageOpenRaster => Some(Family::Picture),
            Self::TextPlain
            | Self::TextMarkdown
            | Self::TextCsv
            | Self::Json
            | Self::Yaml
            | Self::Toml
            | Self::Xml
            | Self::TextHtml
            | Self::TextCss
            | Self::TextJavaScript
            | Self::TextRust
            | Self::TextJava
            | Self::TextC
            | Self::TextPython
            | Self::ShellScript => Some(Family::Text),
            Self::Pdf => Some(Family::Document),
            Self::AudioMpeg
            | Self::AudioFlac
            | Self::AudioOgg
            | Self::AudioOpus
            | Self::AudioWav
            | Self::AudioAu
            | Self::AudioAac
            | Self::AudioMp4 => Some(Family::Audio),
            Self::VideoMp4
            | Self::VideoWebm
            | Self::VideoMatroska
            | Self::VideoQuicktime
            | Self::VideoAvi => Some(Family::Video),
            Self::ArchiveZip
            | Self::ArchiveTar
            | Self::ArchiveGzip
            | Self::ArchiveXz
            | Self::ArchiveBzip2
            | Self::ArchiveZstd
            | Self::Archive7z
            | Self::ArchiveRar => Some(Family::Archive),
            Self::TairixApp | Self::TairixService | Self::TairixRxe | Self::Wasm | Self::Elf => {
                Some(Family::Program)
            }
            Self::InodeDirectory | Self::ApplicationOctetStream => None,
        }
    }
}

/// What a batch of `folder`'s entries shows of it: up to three of its
/// members, **variety first, then fill**. One card for each of the most
/// frequent families in turn, a tie going to whichever was seen first; cards
/// left over go round the families again, in the same order, while a family
/// has members not yet shown. Within a family, members come in the batch's
/// order. Folders, links and files of no recognised type make no card.
///
/// A member is drawn as its own picture where its tile would be — a regular
/// file the listing names with an identity, of a type that has a reading —
/// and as its kind otherwise.
///
/// `entries` is walked twice, once to count and once to pick, so a sample
/// allocates only the paths of the pictures it keeps.
pub fn folder_sample<'n>(
    folder: &[String],
    entries: impl Iterator<Item = DirEntry<'n>> + Clone,
) -> FolderSample {
    // A count and the first position it was seen at, so a larger count, then
    // an earlier first sighting, wins.
    #[derive(Copy, Clone, Default)]
    struct Seen {
        count: u32,
        first: u32,
    }
    let service_store = is_system_service_store(folder);
    let classify = |record: &DirEntry<'n>| {
        let name = core::str::from_utf8(record.name).ok()?;
        let media = media_for_named(
            name,
            EntryKind::for_listing(record.kind, name, None),
            service_store,
        );
        Some((media.family()?, media, name))
    };
    let mut families = [Seen::default(); Family::COUNT];
    for (at, record) in (0u32..).zip(entries.clone()) {
        let Some((family, ..)) = classify(&record) else {
            continue;
        };
        let seen = &mut families[family.index()];
        if seen.count == 0 {
            seen.first = at;
        }
        seen.count += 1;
    }
    let mut order = Family::ALL;
    order.sort_unstable_by_key(|family| {
        let seen = families[family.index()];
        core::cmp::Reverse((seen.count, core::cmp::Reverse(seen.first)))
    });
    // Which member of which family each card shows, front card first.
    let mut wanted = [None::<(Family, u32)>; FolderSample::MOST];
    let mut chosen = 0;
    'rounds: for round in 0.. {
        let mut any = false;
        for family in order {
            if families[family.index()].count <= round {
                continue;
            }
            let Some(slot) = wanted.get_mut(chosen) else {
                break 'rounds;
            };
            *slot = Some((family, round));
            chosen += 1;
            any = true;
        }
        if !any {
            break;
        }
    }
    let mut cards: [Option<SampleCard>; FolderSample::MOST] = Default::default();
    let mut met = [0u32; Family::COUNT];
    let mut path = None::<String>;
    for record in entries {
        let Some((family, media, name)) = classify(&record) else {
            continue;
        };
        let nth = met[family.index()];
        met[family.index()] += 1;
        let Some(slot) = wanted.iter().position(|want| *want == Some((family, nth))) else {
            continue;
        };
        let kind = media.icon();
        let reading = media
            .thumbnail()
            .filter(|_| record.kind == FileKind::Regular && !record.id.is_none());
        cards[slot] = Some(match reading {
            Some(reading) => {
                let dir = path.get_or_insert_with(|| crate::vfs::spell_absolute_path(folder));
                let mut member = dir.clone();
                crate::vfs::push_child(&mut member, name);
                SampleCard::Picture(
                    kind,
                    Thumbnail {
                        path: member,
                        stamp: DocumentStamp {
                            size: record.size,
                            modified: record.modified,
                            id: record.id,
                            content_gen: record.content_gen,
                        },
                        reading,
                    },
                )
            }
            None => SampleCard::Kind(kind),
        });
    }
    FolderSample::new(cards.into_iter().flatten())
}

/// Every [`MediaType`], in registry order: the spelling round-trip
/// ([`from_media_str`](MediaType::from_media_str)) and the order New ▸ offers
/// its documents in.
pub(crate) const ALL: &[MediaType] = &[
    MediaType::InodeDirectory,
    MediaType::TairixApp,
    MediaType::TairixService,
    MediaType::TairixRxe,
    MediaType::Wasm,
    MediaType::Elf,
    MediaType::TextPlain,
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
    MediaType::Pdf,
    MediaType::ImagePng,
    MediaType::ImageJpeg,
    MediaType::ImageGif,
    MediaType::ImageSvg,
    MediaType::ImageSprite,
    MediaType::ImageBmp,
    MediaType::ImageIcon,
    MediaType::ImageWebp,
    MediaType::ImageTiff,
    MediaType::ImageOpenRaster,
    MediaType::ArchiveZip,
    MediaType::ArchiveTar,
    MediaType::ArchiveGzip,
    MediaType::ArchiveXz,
    MediaType::ArchiveBzip2,
    MediaType::ArchiveZstd,
    MediaType::Archive7z,
    MediaType::ArchiveRar,
    MediaType::AudioMpeg,
    MediaType::AudioFlac,
    MediaType::AudioOgg,
    MediaType::AudioOpus,
    MediaType::AudioWav,
    MediaType::AudioAu,
    MediaType::AudioAac,
    MediaType::AudioMp4,
    MediaType::VideoMp4,
    MediaType::VideoWebm,
    MediaType::VideoMatroska,
    MediaType::VideoQuicktime,
    MediaType::VideoAvi,
    MediaType::ApplicationOctetStream,
];

/// Extension → [`MediaType`] table, the one source for the mapping a file
/// name's extension implies. Directory / app / service types are kind-implied,
/// not extension-implied, so they are absent here (they are reached only
/// through [`media_for_entry`]).
///
/// Every extension the browser recognises appears exactly once. Source and
/// structured-config formats map to their honest concrete type — a `.json`
/// file is `application/json`, not "text that happens to draw a text glyph" —
/// so an application that declares one of them keeps matching its own files;
/// only [`MediaType::icon`] is coarse. `text/plain` is left for the formats
/// that genuinely have no type of their own. The two TAIRiX-native forms (the
/// executable envelope, a service bundle) carry a vendor
/// `application/x-tairix-*` spelling.
const EXTENSION_TABLE: &[(MediaType, &[&str])] = &[
    (MediaType::TairixRxe, &["rxe"]),
    (MediaType::Wasm, &["wasm"]),
    (MediaType::Elf, &["elf"]),
    (
        MediaType::TextPlain,
        &["txt", "rst", "log", "ini", "cfg", "conf"],
    ),
    (MediaType::TextMarkdown, &["md", "markdown"]),
    (MediaType::TextCsv, &["csv"]),
    (MediaType::Json, &["json"]),
    (MediaType::Yaml, &["yaml", "yml"]),
    (MediaType::Toml, &["toml"]),
    (MediaType::Xml, &["xml"]),
    (MediaType::TextHtml, &["html", "htm"]),
    (MediaType::TextCss, &["css"]),
    (MediaType::TextJavaScript, &["js", "mjs"]),
    (MediaType::TextRust, &["rs"]),
    (MediaType::TextJava, &["java"]),
    (MediaType::TextC, &["c", "h"]),
    (MediaType::TextPython, &["py"]),
    (MediaType::ShellScript, &["sh"]),
    (MediaType::Pdf, &["pdf"]),
    (MediaType::ImagePng, &["png"]),
    (MediaType::ImageJpeg, &["jpg", "jpeg"]),
    (MediaType::ImageGif, &["gif"]),
    (MediaType::ImageSvg, &["svg"]),
    (MediaType::ImageSprite, &["spr"]),
    (MediaType::ImageBmp, &["bmp"]),
    (MediaType::ImageIcon, &["ico"]),
    (MediaType::ImageWebp, &["webp"]),
    (MediaType::ImageTiff, &["tiff", "tif"]),
    (MediaType::ImageOpenRaster, &["ora"]),
    (MediaType::ArchiveZip, &["zip"]),
    (MediaType::ArchiveTar, &["tar"]),
    (MediaType::ArchiveGzip, &["gz", "tgz"]),
    (MediaType::ArchiveXz, &["xz"]),
    (MediaType::ArchiveBzip2, &["bz2"]),
    (MediaType::ArchiveZstd, &["zst"]),
    (MediaType::Archive7z, &["7z"]),
    (MediaType::ArchiveRar, &["rar"]),
    (MediaType::AudioMpeg, &["mp3"]),
    (MediaType::AudioFlac, &["flac"]),
    (MediaType::AudioOgg, &["ogg", "oga"]),
    (MediaType::AudioOpus, &["opus"]),
    (MediaType::AudioWav, &["wav"]),
    (MediaType::AudioAu, &["au", "snd"]),
    (MediaType::AudioAac, &["aac"]),
    (MediaType::AudioMp4, &["m4a"]),
    (MediaType::VideoMp4, &["mp4", "m4v"]),
    (MediaType::VideoWebm, &["webm"]),
    (MediaType::VideoMatroska, &["mkv"]),
    (MediaType::VideoQuicktime, &["mov"]),
    (MediaType::VideoAvi, &["avi"]),
];

/// RISC OS file types a name may carry after a comma — how a RISC OS file
/// keeps its type on a filesystem with no field for one (`Sprites,ff9`) —
/// for the types this registry knows.
const FILETYPE_TABLE: &[(MediaType, &str)] = &[
    (MediaType::ImageSprite, "ff9"),
    (MediaType::ImagePng, "b60"),
    (MediaType::ImageJpeg, "c85"),
    (MediaType::ImageGif, "695"),
    (MediaType::ImageBmp, "69c"),
    (MediaType::ImageTiff, "ff0"),
    (MediaType::TextPlain, "fff"),
];

/// What ends a file name: the RISC OS file type after its last comma, when
/// that is three hex digits, and the extension after the last dot of what
/// comes before it. Each needs a stem of its own, so a name whose only dot or
/// comma starts it ends in neither.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ending<'a> {
    /// What the ending follows: the whole name when it ends in neither.
    pub stem: &'a str,
    /// The file type's three hex digits.
    pub filetype: Option<&'a str>,
    /// The extension, without its dot.
    pub extension: Option<&'a str>,
}

impl<'a> Ending<'a> {
    /// What ends `name`.
    #[must_use]
    pub fn of(name: &'a str) -> Self {
        let (rest, filetype) = match name.rsplit_once(',') {
            Some((stem, filetype))
                if !stem.is_empty()
                    && tairix_fsmeta::preset::acorn::filetype_from_value(filetype.as_bytes())
                        .is_ok() =>
            {
                (stem, Some(filetype))
            }
            _ => (name, None),
        };
        let (stem, extension) = match rest.rsplit_once('.') {
            Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
                (stem, Some(extension))
            }
            _ => (rest, None),
        };
        Self {
            stem,
            filetype,
            extension,
        }
    }

    /// Whether the name ends in neither.
    #[must_use]
    pub const fn is_none(&self) -> bool {
        self.filetype.is_none() && self.extension.is_none()
    }
}

/// The media type a file name implies, if any: its [`Ending`]'s file type
/// when that is one the registry knows, else its extension.
///
/// The lookup is ASCII-case-insensitive and allocates no `String`: it splits
/// the suffix off in place and compares it against the static tables.
/// `None` for a name with neither, or a suffix the registry does not
/// recognise.
#[must_use]
pub fn media_for_name(name: &str) -> Option<MediaType> {
    let ending = Ending::of(name);
    let known = ending.filetype.and_then(|filetype| {
        FILETYPE_TABLE
            .iter()
            .find(|(_, code)| code.eq_ignore_ascii_case(filetype))
    });
    if let Some((media, _)) = known {
        return Some(*media);
    }
    let ext = ending.extension?;
    EXTENSION_TABLE
        .iter()
        .find(|(_, exts)| {
            exts.iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(ext))
        })
        .map(|(media, _)| *media)
}

/// Every ending the registry knows fits a save pick's bound with its dot in
/// front, so a requester can offer any of them.
const _: () = {
    let mut longest = 0;
    let mut row = 0;
    while row < EXTENSION_TABLE.len() {
        let extensions = EXTENSION_TABLE[row].1;
        let mut at = 0;
        while at < extensions.len() {
            if extensions[at].len() > longest {
                longest = extensions[at].len();
            }
            at += 1;
        }
        row += 1;
    }
    assert!(longest < tairix_abi::window_ipc::SAVE_ENDING_MAX);
};

/// The endings a name of `media` is known by — each extension after a `.`,
/// then its RISC OS file type after a `,` — as [`media_for_name`] reads
/// them back, most usual first.
pub fn name_endings(media: MediaType) -> impl Iterator<Item = (char, &'static str)> {
    let extensions = EXTENSION_TABLE
        .iter()
        .filter(move |(held, _)| *held == media)
        .flat_map(|(_, extensions)| extensions.iter().map(|extension| ('.', *extension)));
    let filetype = FILETYPE_TABLE
        .iter()
        .filter(move |(held, _)| *held == media)
        .map(|(_, code)| (',', *code));
    extensions.chain(filetype)
}

/// A document type New ▸ can make: an empty file is already a complete
/// document of it, and an extension names it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BlankDocument {
    media: MediaType,
    noun: &'static str,
    extension: &'static str,
}

impl BlankDocument {
    /// `media` as a document New ▸ can make, or `None` when it is not one.
    #[must_use]
    pub fn of(media: MediaType) -> Option<Self> {
        let noun = media.blank_noun()?;
        let extension = name_endings(media)
            .find_map(|(separator, ending)| (separator == '.').then_some(ending))?;
        Some(Self {
            media,
            noun,
            extension,
        })
    }

    /// The document's media type.
    #[must_use]
    pub const fn media(self) -> MediaType {
        self.media
    }

    /// What one is called: the menu row's label, and the stem of a new one's
    /// name.
    #[must_use]
    pub const fn noun(self) -> &'static str {
        self.noun
    }

    /// The usual extension of its name, without the dot.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        self.extension
    }
}

/// The media type of a listed entry, given the components of the directory it
/// was listed from.
///
/// A directory classifies as [`InodeDirectory`](MediaType::InodeDirectory)
/// regardless of its name; a bundle classifies as
/// [`TairixService`](MediaType::TairixService) when `parent` is the system
/// service store and [`TairixApp`](MediaType::TairixApp) otherwise; a regular
/// file takes its extension's type, or
/// [`ApplicationOctetStream`](MediaType::ApplicationOctetStream) when the
/// extension is unrecognised (or absent) — fail closed to the generic type.
#[must_use]
pub fn media_for_entry(entry: &Entry, parent: &[String]) -> MediaType {
    media_for_named(entry.name(), entry.kind(), is_system_service_store(parent))
}

/// The media type of a node known only by its `name` and `kind` — what a
/// surface describing *one* node has, as against a listing that knows the
/// directory each row came out of.
///
/// The same classification [`media_for_entry`] applies, so a node is typed
/// identically whether it is reached as a listed row or as the subject of its
/// own window. `service_store` says whether a bundle was found in the system
/// service store, which only a caller that knows the parent can answer; a
/// caller that does not passes `false` and a bundle types as an application.
///
/// A link classifies as what it *names*: a shortcut to a folder is typed and
/// opened as a folder. A link that resolves to nothing has no content type at
/// all, so it falls closed to the generic one.
#[must_use]
pub fn media_for_named(name: &str, kind: EntryKind, service_store: bool) -> MediaType {
    match kind.resolved() {
        Some(EntryKind::Directory) => MediaType::InodeDirectory,
        Some(EntryKind::Bundle) if service_store => MediaType::TairixService,
        Some(EntryKind::Bundle) => MediaType::TairixApp,
        Some(EntryKind::File) => media_for_name(name).unwrap_or(MediaType::ApplicationOctetStream),
        // `resolved` never yields a link, and a dangling one yields nothing.
        Some(EntryKind::Link(_)) | None => MediaType::ApplicationOctetStream,
    }
}

/// The icon drawn for a listed entry: its content-type glyph, refined by what
/// the browser knows about a plain directory's contents.
///
/// A directory that has been probed and holds something draws
/// [`IconKind::FolderFilled`]; every other case — empty, unprobed, or a probe
/// that was refused — draws the plain [`IconKind::Folder`], so an unknown
/// answer never claims contents. Files and bundles take their
/// [`media_for_entry`] glyph unchanged.
///
/// This is the one place an entry becomes an icon: the grid tile, the list row
/// and the desktop all reach it, so no two can picture one entry differently.
#[must_use]
pub fn icon_for_entry(entry: &Entry, parent: &[String]) -> IconKind {
    icon_of(entry, media_for_entry(entry, parent))
}

/// `media`'s glyph for `entry`, refined by a plain directory's occupancy.
fn icon_of(entry: &Entry, media: MediaType) -> IconKind {
    if entry.is_directory() && entry.occupancy().pictured().is_some() {
        IconKind::FolderFilled
    } else {
        media.icon()
    }
}

/// The icon one entry listed out of the directory `dir` (root-first `parent`)
/// is drawn with, and the request its picture is asked for by.
///
/// The request names the entry's own picture where it has one: an
/// application bundle's own icon, an occupied folder's picture of what it
/// holds, a picture file's own content. Each falls back to the icon's class
/// picture where it will not serve. `scratch` is a buffer the caller reuses
/// across the entries of one frame, so a grid spells its paths without
/// allocating one per tile.
///
/// Both surfaces that draw entries as tiles — the file manager's grid and the
/// desktop — ask here, so an entry cannot be pictured one way on the desktop
/// and another in the manager.
#[must_use]
pub fn entry_icon<'a>(
    dir: &str,
    parent: &[String],
    entry: &'a Entry,
    scratch: &'a mut String,
) -> (IconKind, IconRequest<'a>) {
    let media = media_for_entry(entry, parent);
    let kind = icon_of(entry, media);
    if let (true, Some(sample)) = (entry.is_directory(), entry.occupancy().pictured()) {
        return (kind, IconRequest::folder(sample));
    }
    // A link's listed size and time are its own, not its target's, so they
    // cannot key the picture of what it points at; a listing naming no file
    // gives an open nothing to be checked against.
    let thumbnail = match entry.kind() {
        EntryKind::File if !entry.id().is_none() => media.thumbnail(),
        _ => None,
    };
    if thumbnail.is_none() && !entry.is_bundle() {
        return (kind, IconRequest::kind(kind));
    }
    scratch.clear();
    scratch.push_str(dir);
    crate::vfs::push_child(scratch, entry.name());
    let request = match thumbnail {
        Some(reading) => IconRequest::thumbnail(kind, scratch, entry.stamp(), reading),
        None => IconRequest::bundle(kind, scratch),
    };
    (kind, request)
}

/// Whether `parent`'s root-first components are exactly the system service
/// store ([`SYSTEM_SERVICE_STORE`]).
///
/// The store path is spelled once in `lib/abi`; this compares against that one
/// definition component-wise rather than carrying a second literal.
fn is_system_service_store(parent: &[String]) -> bool {
    let mut expected = SYSTEM_SERVICE_STORE
        .split('/')
        .filter(|part| !part.is_empty());
    let mut got = parent.iter();
    loop {
        match (expected.next(), got.next()) {
            (Some(want), Some(have)) if want == have => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

/// `media` followed by every broader type it is a subclass of, most specific
/// first — the chain application association matches along.
///
/// The walk is bounded by the number of registry entries: the relation is
/// acyclic by construction, and a chain longer than the registry itself could
/// only mean a mis-edited [`MediaType::parent`] had introduced a cycle, so the
/// bound stops rather than spins.
pub(crate) fn ancestry(media: MediaType) -> impl Iterator<Item = MediaType> {
    let mut step = Some(media);
    core::iter::from_fn(move || {
        let current = step?;
        step = current.parent();
        Some(current)
    })
    .take(ALL.len())
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;
