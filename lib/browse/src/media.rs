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

use tairix_abi::driver_store::DRIVER_STORE_PATH;
use tairix_abi::fs::{DirEntry, FileKind};
use tairix_abi::home::USERS_DIR;
use tairix_abi::{BundleEntry, ProgramKind, HOME_APPLICATION_STORE_DIR, HOME_COMMAND_STORE_DIR};
use tairix_fsmeta::preset::acorn::{self, Filetype};
use tairix_icon::{
    DocumentStamp, FolderSample, IconKind, IconRequest, Reading, SampleCard, Thumbnail,
};

use crate::entry::{is_bundle_name, Entry, EntryKind};
use crate::presentation::Pictures;

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
    /// A `<Name>.app` bundle listed from a store of programs the system
    /// starts — the service or component store
    /// (`application/x-tairix-service`).
    TairixService,
    /// A `<Name>.app` bundle listed from a screensaver store, the system's or
    /// the user's own, which the desktop starts in the drawing sandbox
    /// (`application/x-tairix-screensaver`).
    TairixScreensaver,
    /// The TAIRiX executable envelope (`application/x-tairix-rxe`).
    TairixRxe,
    /// A bare WebAssembly module (`application/wasm`).
    Wasm,
    /// A raw ELF image (`application/x-elf`).
    Elf,
    /// A signed driver bundle's image: the `Run` the driver store holds for
    /// each driver (`application/x-tairix-driver`).
    TairixDriver,
    /// A bundle's signed manifest, its `AppInfo`
    /// (`application/x-tairix-appinfo`).
    TairixAppInfo,
    /// A RISC OS program loaded at the address it was built for, an Absolute
    /// file (`application/x-riscos-absolute`).
    RiscOsAbsolute,
    /// A RISC OS utility, code that runs wherever it is loaded
    /// (`application/x-riscos-utility`).
    RiscOsUtility,
    /// A RISC OS relocatable module (`application/x-riscos-module`).
    RiscOsModule,
    /// A tokenised BBC BASIC program, as RISC OS saves one
    /// (`application/x-riscos-basic`).
    RiscOsBasic,
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
    /// Lua source (`text/x-lua`).
    TextLua,
    /// Perl source (`text/x-perl`).
    TextPerl,
    /// A makefile (`text/x-makefile`).
    TextMakefile,
    /// A TeX or LaTeX document (`text/x-tex`).
    TextTex,
    /// A BBC BASIC program kept as text (`text/x-riscos-basic`).
    RiscOsBasicText,
    /// A Rich Text Format document (`application/rtf`).
    Rtf,
    /// A shell script (`application/x-shellscript`).
    ShellScript,
    /// A RISC OS Obey file, a script of command lines
    /// (`application/x-riscos-obey`).
    RiscOsObey,
    /// A RISC OS Command file, keystrokes `*Exec` feeds to the command line
    /// (`application/x-riscos-command`).
    RiscOsCommand,
    /// A PDF document (`application/pdf`).
    Pdf,
    /// A PostScript document (`application/postscript`).
    PostScript,
    /// Printer Command Language print data (`application/vnd.hp-pcl`).
    Pcl,
    /// Data RISC OS prepared for a dot-matrix printer
    /// (`application/x-riscos-printout`).
    RiscOsPrintout,
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
    /// A JPEG 2000 image (`image/jp2`).
    ImageJpeg2000,
    /// A Photoshop image (`image/vnd.adobe.photoshop`).
    ImagePhotoshop,
    /// A PCX image (`image/vnd.zbrush.pcx`).
    ImagePcx,
    /// A RISC OS Draw vector drawing (`image/x-riscos-draw`).
    RiscOsDraw,
    /// An `ArtWorks` vector drawing (`image/x-artworks`).
    ArtWorks,
    /// The window templates a RISC OS application builds its windows from
    /// (`application/x-riscos-template`).
    RiscOsTemplate,
    /// The Toolbox definitions of a RISC OS application's windows and menus
    /// (`application/x-riscos-resource`).
    RiscOsResource,
    /// A RISC OS font, outline or bitmap (`application/x-riscos-font`).
    RiscOsFont,
    /// A BBC Micro font, a series of character definitions
    /// (`application/x-bbc-font`).
    BbcFont,
    /// A TrueType font (`font/ttf`).
    FontTtf,
    /// An OpenType font (`font/otf`).
    FontOtf,
    /// A WOFF web font (`font/woff`).
    FontWoff,
    /// A RISC OS desktop palette (`application/x-riscos-palette`).
    RiscOsPalette,
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
    /// A Java archive, a ZIP of classes (`application/java-archive`).
    JavaArchive,
    /// One file compressed by RISC OS Squash (`application/x-riscos-squash`).
    RiscOsSquash,
    /// An `ArcFS` archive (`application/x-arcfs`).
    ArcFs,
    /// The image of a disk, holding one volume (`application/x-raw-disk-image`):
    /// a hard-disk partition's, and the broader type of the disc and floppy
    /// images.
    RawDiskImage,
    /// The image of an optical disc (`application/x-cd-image`).
    CdImage,
    /// The image of a floppy disk, Acorn's or Amiga's alike
    /// (`application/x-raw-floppy-disk-image`): which one is the kernel's probe
    /// to tell, never the name.
    RawFloppyDiskImage,
    /// An MPEG audio file of any layer — MP3, MP2 or MP1 (`audio/mpeg`).
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
    /// An AIFF audio file (`audio/aiff`).
    AudioAiff,
    /// A Standard MIDI File (`audio/midi`).
    AudioMidi,
    /// A Sound Tracker music module (`audio/x-mod`).
    AudioTracker,
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
    /// An MPEG-1 or MPEG-2 video (`video/mpeg`).
    VideoMpeg,
    /// An Acorn Replay movie (`video/x-armovie`).
    VideoArMovie,
    /// Any content of no recognised type (`application/octet-stream`).
    ApplicationOctetStream,
}

impl MediaType {
    /// What a person calls this type: the name a listing's type column shows.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::InodeDirectory => "Folder",
            Self::TairixApp => "Application",
            Self::TairixService => "Service",
            Self::TairixScreensaver => "Screensaver",
            Self::TairixRxe => "Program",
            Self::Wasm => "WebAssembly module",
            Self::Elf => "ELF program",
            Self::TairixDriver => "Driver",
            Self::TairixAppInfo => "Application manifest",
            Self::RiscOsAbsolute => "RISC OS program",
            Self::RiscOsUtility => "RISC OS utility",
            Self::RiscOsModule => "RISC OS module",
            Self::RiscOsBasic => "BASIC program",
            Self::TextPlain => "Text",
            Self::TextMarkdown => "Markdown document",
            Self::TextCsv => "CSV table",
            Self::Json => "JSON document",
            Self::Yaml => "YAML document",
            Self::Toml => "TOML document",
            Self::Xml => "XML document",
            Self::TextHtml => "HTML page",
            Self::TextCss => "Style sheet",
            Self::TextJavaScript => "JavaScript",
            Self::TextRust => "Rust source",
            Self::TextJava => "Java source",
            Self::TextC => "C source",
            Self::TextPython => "Python script",
            Self::TextLua => "Lua script",
            Self::TextPerl => "Perl script",
            Self::TextMakefile => "Makefile",
            Self::TextTex => "TeX document",
            Self::RiscOsBasicText => "BASIC source",
            Self::Rtf => "Rich Text document",
            Self::ShellScript => "Shell script",
            Self::RiscOsObey => "Obey script",
            Self::RiscOsCommand => "Command script",
            Self::Pdf => "PDF document",
            Self::PostScript => "PostScript document",
            Self::Pcl => "PCL print data",
            Self::RiscOsPrintout => "Printout",
            Self::ImagePng => "PNG image",
            Self::ImageJpeg => "JPEG image",
            Self::ImageGif => "GIF image",
            Self::ImageSvg => "SVG image",
            Self::ImageSprite => "Sprite",
            Self::ImageBmp => "BMP image",
            Self::ImageIcon => "Icon",
            Self::ImageWebp => "WebP image",
            Self::ImageTiff => "TIFF image",
            Self::ImageOpenRaster => "OpenRaster image",
            Self::ImageJpeg2000 => "JPEG 2000 image",
            Self::ImagePhotoshop => "Photoshop image",
            Self::ImagePcx => "PCX image",
            Self::RiscOsDraw => "Draw drawing",
            Self::ArtWorks => "ArtWorks drawing",
            Self::RiscOsTemplate => "Window templates",
            Self::RiscOsResource => "Toolbox resources",
            Self::RiscOsFont => "RISC OS font",
            Self::BbcFont => "BBC font",
            Self::FontTtf => "TrueType font",
            Self::FontOtf => "OpenType font",
            Self::FontWoff => "WOFF font",
            Self::RiscOsPalette => "Palette",
            Self::ArchiveZip => "ZIP archive",
            Self::ArchiveTar => "Tar archive",
            Self::ArchiveGzip => "Gzip archive",
            Self::ArchiveXz => "XZ archive",
            Self::ArchiveBzip2 => "Bzip2 archive",
            Self::ArchiveZstd => "Zstandard archive",
            Self::Archive7z => "7z archive",
            Self::ArchiveRar => "RAR archive",
            Self::JavaArchive => "Java archive",
            Self::RiscOsSquash => "Squash file",
            Self::ArcFs => "ArcFS archive",
            Self::RawDiskImage => "Disk image",
            Self::CdImage => "Disc image",
            Self::RawFloppyDiskImage => "Floppy disk image",
            Self::AudioMpeg => "MPEG audio",
            Self::AudioFlac => "FLAC audio",
            Self::AudioOgg => "Ogg audio",
            Self::AudioOpus => "Opus audio",
            Self::AudioWav => "WAVE audio",
            Self::AudioAu => "AU audio",
            Self::AudioAac => "AAC audio",
            Self::AudioMp4 => "MPEG-4 audio",
            Self::AudioAiff => "AIFF audio",
            Self::AudioMidi => "MIDI music",
            Self::AudioTracker => "Tracker module",
            Self::VideoMp4 => "MPEG-4 video",
            Self::VideoWebm => "WebM video",
            Self::VideoMatroska => "Matroska video",
            Self::VideoQuicktime => "QuickTime video",
            Self::VideoAvi => "AVI video",
            Self::VideoMpeg => "MPEG video",
            Self::VideoArMovie => "Replay movie",
            Self::ApplicationOctetStream => "Data",
        }
    }

    /// The IANA (or TAIRiX vendor) media-type spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InodeDirectory => "inode/directory",
            Self::TairixApp => "application/x-tairix-app",
            Self::TairixService => "application/x-tairix-service",
            Self::TairixScreensaver => "application/x-tairix-screensaver",
            Self::TairixRxe => "application/x-tairix-rxe",
            Self::Wasm => "application/wasm",
            Self::Elf => "application/x-elf",
            Self::TairixDriver => "application/x-tairix-driver",
            Self::TairixAppInfo => "application/x-tairix-appinfo",
            Self::RiscOsAbsolute => "application/x-riscos-absolute",
            Self::RiscOsUtility => "application/x-riscos-utility",
            Self::RiscOsModule => "application/x-riscos-module",
            Self::RiscOsBasic => "application/x-riscos-basic",
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
            Self::TextLua => "text/x-lua",
            Self::TextPerl => "text/x-perl",
            Self::TextMakefile => "text/x-makefile",
            Self::TextTex => "text/x-tex",
            Self::RiscOsBasicText => "text/x-riscos-basic",
            Self::Rtf => "application/rtf",
            Self::ShellScript => "application/x-shellscript",
            Self::RiscOsObey => "application/x-riscos-obey",
            Self::RiscOsCommand => "application/x-riscos-command",
            Self::Pdf => "application/pdf",
            Self::PostScript => "application/postscript",
            Self::Pcl => "application/vnd.hp-pcl",
            Self::RiscOsPrintout => "application/x-riscos-printout",
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
            Self::ImageJpeg2000 => "image/jp2",
            Self::ImagePhotoshop => "image/vnd.adobe.photoshop",
            Self::ImagePcx => "image/vnd.zbrush.pcx",
            Self::RiscOsDraw => "image/x-riscos-draw",
            Self::ArtWorks => "image/x-artworks",
            Self::RiscOsTemplate => "application/x-riscos-template",
            Self::RiscOsResource => "application/x-riscos-resource",
            Self::RiscOsFont => "application/x-riscos-font",
            Self::BbcFont => "application/x-bbc-font",
            Self::FontTtf => "font/ttf",
            Self::FontOtf => "font/otf",
            Self::FontWoff => "font/woff",
            Self::RiscOsPalette => "application/x-riscos-palette",
            Self::ArchiveZip => "application/zip",
            Self::ArchiveTar => "application/x-tar",
            Self::ArchiveGzip => "application/gzip",
            Self::ArchiveXz => "application/x-xz",
            Self::ArchiveBzip2 => "application/x-bzip2",
            Self::ArchiveZstd => "application/zstd",
            Self::Archive7z => "application/x-7z-compressed",
            Self::ArchiveRar => "application/vnd.rar",
            Self::JavaArchive => "application/java-archive",
            Self::RiscOsSquash => "application/x-riscos-squash",
            Self::ArcFs => "application/x-arcfs",
            Self::RawDiskImage => "application/x-raw-disk-image",
            Self::CdImage => "application/x-cd-image",
            Self::RawFloppyDiskImage => "application/x-raw-floppy-disk-image",
            Self::AudioMpeg => "audio/mpeg",
            Self::AudioFlac => "audio/flac",
            Self::AudioOgg => "audio/ogg",
            Self::AudioOpus => "audio/opus",
            Self::AudioWav => "audio/wav",
            Self::AudioAu => "audio/basic",
            Self::AudioAac => "audio/aac",
            Self::AudioMp4 => "audio/mp4",
            Self::AudioAiff => "audio/aiff",
            Self::AudioMidi => "audio/midi",
            Self::AudioTracker => "audio/x-mod",
            Self::VideoMp4 => "video/mp4",
            Self::VideoWebm => "video/webm",
            Self::VideoMatroska => "video/x-matroska",
            Self::VideoQuicktime => "video/quicktime",
            Self::VideoAvi => "video/x-msvideo",
            Self::VideoMpeg => "video/mpeg",
            Self::VideoArMovie => "video/x-armovie",
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
    /// And every file is, at last, bytes: each broadest file type names
    /// [`ApplicationOctetStream`](Self::ApplicationOctetStream), so a byte
    /// editor that claims it opens anything. A directory is no stream of
    /// bytes, nor is a bundle, which is a directory too: they name `None`.
    ///
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
            | Self::TextLua
            | Self::TextPerl
            | Self::TextMakefile
            | Self::TextTex
            | Self::RiscOsBasicText
            | Self::Rtf
            | Self::ShellScript
            | Self::RiscOsObey
            | Self::RiscOsCommand
            | Self::PostScript => Some(Self::TextPlain),
            Self::ImageSvg => Some(Self::Xml),
            Self::JavaArchive => Some(Self::ArchiveZip),
            Self::AudioOpus => Some(Self::AudioOgg),
            Self::CdImage | Self::RawFloppyDiskImage => Some(Self::RawDiskImage),
            Self::InodeDirectory
            | Self::TairixApp
            | Self::TairixService
            | Self::TairixScreensaver
            | Self::ApplicationOctetStream => None,
            Self::TairixRxe
            | Self::Wasm
            | Self::Elf
            | Self::TairixDriver
            | Self::TairixAppInfo
            | Self::RiscOsAbsolute
            | Self::RiscOsUtility
            | Self::RiscOsModule
            | Self::RiscOsBasic
            | Self::TextPlain
            | Self::Pdf
            | Self::Pcl
            | Self::RiscOsPrintout
            | Self::ImagePng
            | Self::ImageJpeg
            | Self::ImageGif
            | Self::ImageSprite
            | Self::ImageBmp
            | Self::ImageIcon
            | Self::ImageWebp
            | Self::ImageTiff
            | Self::ImageOpenRaster
            | Self::ImageJpeg2000
            | Self::ImagePhotoshop
            | Self::ImagePcx
            | Self::RiscOsDraw
            | Self::ArtWorks
            | Self::RiscOsTemplate
            | Self::RiscOsResource
            | Self::RiscOsFont
            | Self::BbcFont
            | Self::FontTtf
            | Self::FontOtf
            | Self::FontWoff
            | Self::RiscOsPalette
            | Self::ArchiveZip
            | Self::ArchiveTar
            | Self::ArchiveGzip
            | Self::ArchiveXz
            | Self::ArchiveBzip2
            | Self::ArchiveZstd
            | Self::Archive7z
            | Self::ArchiveRar
            | Self::RiscOsSquash
            | Self::ArcFs
            | Self::RawDiskImage
            | Self::AudioMpeg
            | Self::AudioFlac
            | Self::AudioOgg
            | Self::AudioWav
            | Self::AudioAu
            | Self::AudioAac
            | Self::AudioMp4
            | Self::AudioAiff
            | Self::AudioMidi
            | Self::AudioTracker
            | Self::VideoMp4
            | Self::VideoWebm
            | Self::VideoMatroska
            | Self::VideoQuicktime
            | Self::VideoAvi
            | Self::VideoMpeg
            | Self::VideoArMovie => Some(Self::ApplicationOctetStream),
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
            Self::TairixScreensaver => IconKind::Screensaver,
            Self::TairixRxe => IconKind::Executable,
            Self::Wasm => IconKind::WasmModule,
            Self::Elf => IconKind::ElfExecutable,
            Self::TairixDriver => IconKind::Driver,
            Self::TairixAppInfo => IconKind::Manifest,
            Self::RiscOsAbsolute => IconKind::RiscOsProgram,
            Self::RiscOsUtility => IconKind::RiscOsUtility,
            Self::RiscOsModule => IconKind::RiscOsModule,
            Self::RiscOsBasic | Self::RiscOsBasicText => IconKind::BasicProgram,
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
            | Self::TextPython
            | Self::TextLua
            | Self::TextPerl
            | Self::TextMakefile
            | Self::TextTex
            | Self::Rtf => IconKind::Text,
            Self::TextHtml => IconKind::TextHtml,
            Self::TextRust => IconKind::TextRust,
            Self::TextJava => IconKind::TextJava,
            Self::ShellScript => IconKind::ShellScript,
            Self::RiscOsObey | Self::RiscOsCommand => IconKind::RiscOsObey,
            Self::Pdf => IconKind::Pdf,
            Self::PostScript => IconKind::PostScript,
            Self::Pcl | Self::RiscOsPrintout => IconKind::PrintData,
            Self::ImagePng => IconKind::ImagePng,
            Self::ImageJpeg => IconKind::ImageJpeg,
            Self::ImageGif => IconKind::ImageGif,
            Self::ImageSvg => IconKind::ImageSvg,
            Self::ImageSprite => IconKind::ImageSprite,
            Self::ImageBmp
            | Self::ImageIcon
            | Self::ImageWebp
            | Self::ImageTiff
            | Self::ImageOpenRaster
            | Self::ImageJpeg2000
            | Self::ImagePhotoshop
            | Self::ImagePcx => IconKind::Image,
            Self::RiscOsDraw | Self::ArtWorks => IconKind::Drawing,
            Self::RiscOsTemplate | Self::RiscOsResource => IconKind::WindowTemplate,
            Self::RiscOsFont | Self::BbcFont | Self::FontTtf | Self::FontOtf | Self::FontWoff => {
                IconKind::Font
            }
            Self::RiscOsPalette => IconKind::Palette,
            Self::ArchiveZip
            | Self::ArchiveTar
            | Self::ArchiveGzip
            | Self::ArchiveXz
            | Self::ArchiveBzip2
            | Self::ArchiveZstd
            | Self::Archive7z
            | Self::ArchiveRar
            | Self::JavaArchive
            | Self::RiscOsSquash
            | Self::ArcFs => IconKind::Archive,
            Self::RawDiskImage => IconKind::DiskHard,
            Self::CdImage => IconKind::DiskOptical,
            Self::RawFloppyDiskImage => IconKind::DiskFloppy,
            Self::AudioMpeg
            | Self::AudioFlac
            | Self::AudioOgg
            | Self::AudioOpus
            | Self::AudioWav
            | Self::AudioAu
            | Self::AudioAac
            | Self::AudioMp4
            | Self::AudioAiff
            | Self::AudioMidi
            | Self::AudioTracker => IconKind::Audio,
            Self::VideoMp4
            | Self::VideoWebm
            | Self::VideoMatroska
            | Self::VideoQuicktime
            | Self::VideoAvi
            | Self::VideoMpeg
            | Self::VideoArMovie => IconKind::Video,
            Self::ApplicationOctetStream => IconKind::File,
        }
    }

    /// Whether a file of this type holds a volume, so opening it is mounting
    /// it: a disk image of any kind.
    #[must_use]
    pub fn holds_a_volume(self) -> bool {
        ancestry(self).any(|media| media == Self::RawDiskImage)
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
            | Self::TairixScreensaver
            | Self::TairixRxe
            | Self::Wasm
            | Self::Elf
            | Self::TairixDriver
            | Self::TairixAppInfo
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
            | Self::RawDiskImage
            | Self::CdImage
            | Self::RawFloppyDiskImage
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
            | Self::RiscOsAbsolute
            | Self::RiscOsUtility
            | Self::RiscOsModule
            | Self::RiscOsBasic
            | Self::TextLua
            | Self::TextPerl
            | Self::TextMakefile
            | Self::TextTex
            | Self::RiscOsBasicText
            | Self::Rtf
            | Self::RiscOsObey
            | Self::RiscOsCommand
            | Self::PostScript
            | Self::Pcl
            | Self::RiscOsPrintout
            | Self::ImageJpeg2000
            | Self::ImagePhotoshop
            | Self::ImagePcx
            | Self::RiscOsDraw
            | Self::ArtWorks
            | Self::RiscOsTemplate
            | Self::RiscOsResource
            | Self::RiscOsFont
            | Self::BbcFont
            | Self::FontTtf
            | Self::FontOtf
            | Self::FontWoff
            | Self::RiscOsPalette
            | Self::JavaArchive
            | Self::RiscOsSquash
            | Self::ArcFs
            | Self::AudioAiff
            | Self::AudioMidi
            | Self::AudioTracker
            | Self::VideoMpeg
            | Self::VideoArMovie
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
    /// Archives and disk images: files that hold other files.
    Archive,
    /// Programs: bundles and executables, and the resources they build
    /// their windows from.
    Program,
    /// Typefaces.
    Font,
}

impl Family {
    /// Every family.
    const ALL: [Self; 8] = [
        Self::Picture,
        Self::Text,
        Self::Document,
        Self::Audio,
        Self::Video,
        Self::Archive,
        Self::Program,
        Self::Font,
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
            | Self::ImageOpenRaster
            | Self::ImageJpeg2000
            | Self::ImagePhotoshop
            | Self::ImagePcx
            | Self::RiscOsDraw
            | Self::ArtWorks
            | Self::RiscOsPalette => Some(Family::Picture),
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
            | Self::TextLua
            | Self::TextPerl
            | Self::TextMakefile
            | Self::TextTex
            | Self::RiscOsBasicText
            | Self::ShellScript
            | Self::RiscOsObey
            | Self::RiscOsCommand => Some(Family::Text),
            Self::Pdf | Self::Rtf | Self::PostScript | Self::Pcl | Self::RiscOsPrintout => {
                Some(Family::Document)
            }
            Self::AudioMpeg
            | Self::AudioFlac
            | Self::AudioOgg
            | Self::AudioOpus
            | Self::AudioWav
            | Self::AudioAu
            | Self::AudioAac
            | Self::AudioMp4
            | Self::AudioAiff
            | Self::AudioMidi
            | Self::AudioTracker => Some(Family::Audio),
            Self::VideoMp4
            | Self::VideoWebm
            | Self::VideoMatroska
            | Self::VideoQuicktime
            | Self::VideoAvi
            | Self::VideoMpeg
            | Self::VideoArMovie => Some(Family::Video),
            Self::ArchiveZip
            | Self::ArchiveTar
            | Self::ArchiveGzip
            | Self::ArchiveXz
            | Self::ArchiveBzip2
            | Self::ArchiveZstd
            | Self::Archive7z
            | Self::ArchiveRar
            | Self::JavaArchive
            | Self::RiscOsSquash
            | Self::ArcFs
            | Self::RawDiskImage
            | Self::CdImage
            | Self::RawFloppyDiskImage => Some(Family::Archive),
            Self::TairixApp
            | Self::TairixService
            | Self::TairixScreensaver
            | Self::TairixRxe
            | Self::Wasm
            | Self::Elf
            | Self::TairixDriver
            | Self::TairixAppInfo
            | Self::RiscOsAbsolute
            | Self::RiscOsUtility
            | Self::RiscOsModule
            | Self::RiscOsBasic
            | Self::RiscOsTemplate
            | Self::RiscOsResource => Some(Family::Program),
            Self::RiscOsFont | Self::BbcFont | Self::FontTtf | Self::FontOtf | Self::FontWoff => {
                Some(Family::Font)
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
/// and as its kind otherwise. Each member comes with the RISC OS filetype its
/// volume records for it, where one was read, so it is typed as its own
/// listing would type it.
///
/// `entries` is walked twice, once to count and once to pick, so a sample
/// allocates only the paths of the pictures it keeps.
pub fn folder_sample<'n>(
    folder: &[String],
    entries: impl Iterator<Item = (DirEntry<'n>, Option<Filetype>)> + Clone,
) -> FolderSample {
    // A count and the first position it was seen at, so a larger count, then
    // an earlier first sighting, wins.
    #[derive(Copy, Clone, Default)]
    struct Seen {
        count: u32,
        first: u32,
    }
    let site = Site::of(folder);
    let classify = |(record, recorded): &(DirEntry<'n>, Option<Filetype>)| {
        let name = core::str::from_utf8(record.name).ok()?;
        let kind = EntryKind::for_listing(record.kind, name, None);
        let media = media_for_named(name, kind, *recorded, site);
        Some((media.family()?, media, name))
    };
    let mut families = [Seen::default(); Family::COUNT];
    for (at, member) in (0u32..).zip(entries.clone()) {
        let Some((family, ..)) = classify(&member) else {
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
    for member in entries {
        let Some((family, media, name)) = classify(&member) else {
            continue;
        };
        let record = member.0;
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
    MediaType::TairixScreensaver,
    MediaType::TairixRxe,
    MediaType::Wasm,
    MediaType::Elf,
    MediaType::TairixDriver,
    MediaType::TairixAppInfo,
    MediaType::RiscOsAbsolute,
    MediaType::RiscOsUtility,
    MediaType::RiscOsModule,
    MediaType::RiscOsBasic,
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
    MediaType::TextLua,
    MediaType::TextPerl,
    MediaType::TextMakefile,
    MediaType::TextTex,
    MediaType::RiscOsBasicText,
    MediaType::Rtf,
    MediaType::ShellScript,
    MediaType::RiscOsObey,
    MediaType::RiscOsCommand,
    MediaType::Pdf,
    MediaType::PostScript,
    MediaType::Pcl,
    MediaType::RiscOsPrintout,
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
    MediaType::ImageJpeg2000,
    MediaType::ImagePhotoshop,
    MediaType::ImagePcx,
    MediaType::RiscOsDraw,
    MediaType::ArtWorks,
    MediaType::RiscOsTemplate,
    MediaType::RiscOsResource,
    MediaType::RiscOsFont,
    MediaType::BbcFont,
    MediaType::FontTtf,
    MediaType::FontOtf,
    MediaType::FontWoff,
    MediaType::RiscOsPalette,
    MediaType::ArchiveZip,
    MediaType::ArchiveTar,
    MediaType::ArchiveGzip,
    MediaType::ArchiveXz,
    MediaType::ArchiveBzip2,
    MediaType::ArchiveZstd,
    MediaType::Archive7z,
    MediaType::ArchiveRar,
    MediaType::JavaArchive,
    MediaType::RiscOsSquash,
    MediaType::ArcFs,
    MediaType::RawDiskImage,
    MediaType::CdImage,
    MediaType::RawFloppyDiskImage,
    MediaType::AudioMpeg,
    MediaType::AudioFlac,
    MediaType::AudioOgg,
    MediaType::AudioOpus,
    MediaType::AudioWav,
    MediaType::AudioAu,
    MediaType::AudioAac,
    MediaType::AudioMp4,
    MediaType::AudioAiff,
    MediaType::AudioMidi,
    MediaType::AudioTracker,
    MediaType::VideoMp4,
    MediaType::VideoWebm,
    MediaType::VideoMatroska,
    MediaType::VideoQuicktime,
    MediaType::VideoAvi,
    MediaType::VideoMpeg,
    MediaType::VideoArMovie,
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
    (MediaType::TextLua, &["lua"]),
    (MediaType::TextPerl, &["pl", "pm"]),
    (MediaType::TextMakefile, &["mk", "mak"]),
    (MediaType::TextTex, &["tex", "ltx"]),
    (MediaType::Rtf, &["rtf"]),
    (MediaType::ShellScript, &["sh"]),
    (MediaType::Pdf, &["pdf"]),
    (MediaType::PostScript, &["ps", "eps"]),
    (MediaType::Pcl, &["pcl"]),
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
    (MediaType::ImageJpeg2000, &["jp2", "j2k"]),
    (MediaType::ImagePhotoshop, &["psd"]),
    (MediaType::ImagePcx, &["pcx"]),
    (MediaType::FontTtf, &["ttf"]),
    (MediaType::FontOtf, &["otf"]),
    (MediaType::FontWoff, &["woff"]),
    (MediaType::ArchiveZip, &["zip"]),
    (MediaType::ArchiveTar, &["tar"]),
    (MediaType::ArchiveGzip, &["gz", "tgz"]),
    (MediaType::ArchiveXz, &["xz"]),
    (MediaType::ArchiveBzip2, &["bz2"]),
    (MediaType::ArchiveZstd, &["zst"]),
    (MediaType::Archive7z, &["7z"]),
    (MediaType::ArchiveRar, &["rar"]),
    (MediaType::JavaArchive, &["jar"]),
    (MediaType::RawDiskImage, &["hdf"]),
    (MediaType::CdImage, &["iso"]),
    (MediaType::RawFloppyDiskImage, &["adf", "adl", "adm", "ads"]),
    (MediaType::AudioMpeg, &["mp3", "mp2", "mp1", "mpa"]),
    (MediaType::AudioFlac, &["flac"]),
    (MediaType::AudioOgg, &["ogg", "oga"]),
    (MediaType::AudioOpus, &["opus"]),
    (MediaType::AudioWav, &["wav"]),
    (MediaType::AudioAu, &["au", "snd"]),
    (MediaType::AudioAac, &["aac"]),
    (MediaType::AudioMp4, &["m4a"]),
    (MediaType::AudioAiff, &["aif", "aiff", "aifc"]),
    (MediaType::AudioMidi, &["mid", "midi"]),
    (MediaType::VideoMp4, &["mp4", "m4v"]),
    (MediaType::VideoWebm, &["webm"]),
    (MediaType::VideoMatroska, &["mkv"]),
    (MediaType::VideoQuicktime, &["mov"]),
    (MediaType::VideoAvi, &["avi"]),
    (MediaType::VideoMpeg, &["mpg", "mpeg"]),
];

/// RISC OS filetype → [`MediaType`] table: the one source for the type a
/// filetype names, whether the volume records it ([`media_for_filetype`]) or a
/// name carries it after a comma, as a RISC OS file keeps its type on a
/// filesystem with no field for one (`Sprites,ff9`).
///
/// Each type lists the filetypes that hold it, the one RISC OS gives a new file
/// of it first; a filetype holding no type the registry knows is not listed,
/// so it reads as [`ApplicationOctetStream`](MediaType::ApplicationOctetStream).
/// The numbers are RISC OS's own allocations.
const FILETYPE_TABLE: &[(MediaType, &[&str])] = &[
    (MediaType::Elf, &["e1f", "fe6"]),
    (MediaType::RiscOsAbsolute, &["ff8", "aa6", "fd3"]),
    (MediaType::RiscOsUtility, &["ffc"]),
    (MediaType::RiscOsModule, &["ffa"]),
    (MediaType::RiscOsBasic, &["ffb", "aa5", "f7a"]),
    (MediaType::TextPlain, &["fff", "fdb"]),
    (MediaType::TextCsv, &["dfe"]),
    (MediaType::Json, &["f75"]),
    (MediaType::Yaml, &["f74"]),
    (MediaType::Xml, &["f80"]),
    (MediaType::TextHtml, &["faf"]),
    (MediaType::TextCss, &["f79"]),
    (MediaType::TextJavaScript, &["f81"]),
    (MediaType::TextLua, &["18c", "18d"]),
    (MediaType::TextPerl, &["102", "a06"]),
    (MediaType::TextMakefile, &["fe1"]),
    (MediaType::TextTex, &["ce5", "aca"]),
    (MediaType::RiscOsBasicText, &["fd1"]),
    (MediaType::Rtf, &["c32"]),
    (MediaType::RiscOsObey, &["feb", "fd7", "fea", "f7b"]),
    (MediaType::RiscOsCommand, &["ffe", "fd6"]),
    (MediaType::Pdf, &["adf"]),
    (MediaType::PostScript, &["ff5"]),
    (MediaType::Pcl, &["ff3"]),
    (MediaType::RiscOsPrintout, &["ff4"]),
    (MediaType::ImagePng, &["b60"]),
    (MediaType::ImageJpeg, &["c85"]),
    (MediaType::ImageGif, &["695"]),
    (MediaType::ImageSvg, &["aad"]),
    (MediaType::ImageSprite, &["ff9"]),
    (MediaType::ImageBmp, &["69c"]),
    (MediaType::ImageIcon, &["132"]),
    (MediaType::ImageWebp, &["a66"]),
    (MediaType::ImageTiff, &["ff0"]),
    (MediaType::ImageJpeg2000, &["a65"]),
    (MediaType::ImagePhotoshop, &["f98"]),
    (MediaType::ImagePcx, &["697"]),
    (MediaType::RiscOsDraw, &["aff"]),
    (MediaType::ArtWorks, &["d94"]),
    (MediaType::RiscOsTemplate, &["fec"]),
    (MediaType::RiscOsResource, &["fae"]),
    (MediaType::RiscOsFont, &["ff6", "adb"]),
    (MediaType::BbcFont, &["ff7"]),
    (MediaType::FontTtf, &["77f"]),
    (MediaType::FontOtf, &["07f"]),
    (MediaType::FontWoff, &["7ff"]),
    (MediaType::RiscOsPalette, &["fed"]),
    (MediaType::ArchiveZip, &["ddc", "a91"]),
    (MediaType::ArchiveTar, &["c46"]),
    (MediaType::ArchiveGzip, &["f89"]),
    (MediaType::JavaArchive, &["ae4"]),
    (MediaType::RiscOsSquash, &["fca"]),
    (MediaType::ArcFs, &["3fb"]),
    (MediaType::RawDiskImage, &["fcd", "fc8"]),
    (MediaType::RawFloppyDiskImage, &["fce"]),
    (MediaType::AudioMpeg, &["1ad"]),
    (MediaType::AudioFlac, &["1cf"]),
    (MediaType::AudioOgg, &["1a8"]),
    (MediaType::AudioWav, &["fb1"]),
    (MediaType::AudioAiff, &["fc2"]),
    (MediaType::AudioMidi, &["fd4"]),
    (MediaType::AudioTracker, &["cb6"]),
    (MediaType::VideoMp4, &["a64"]),
    (MediaType::VideoMatroska, &["a63"]),
    (MediaType::VideoMpeg, &["bf8"]),
    (MediaType::VideoArMovie, &["ae7"]),
    (MediaType::ApplicationOctetStream, &["ffd"]),
];

/// Every filetype's row in [`FILETYPE_TABLE`] plus one, or zero where it has
/// none, so a lookup is one load however many types the table holds. Building
/// it proves at compile time that each code is three lowercase hex digits, that
/// no filetype names two types and that no type has two rows.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a hex digit is four bits, and the table holds fewer rows than a u8 counts"
)]
const FILETYPE_ROWS: [u8; 0x1000] = {
    let mut rows = [0u8; 0x1000];
    let mut row = 0;
    while row < FILETYPE_TABLE.len() {
        assert!(row < u8::MAX as usize);
        let mut earlier = 0;
        while earlier < row {
            assert!(FILETYPE_TABLE[earlier].0 as usize != FILETYPE_TABLE[row].0 as usize);
            earlier += 1;
        }
        let codes = FILETYPE_TABLE[row].1;
        assert!(!codes.is_empty());
        let mut at = 0;
        while at < codes.len() {
            let code = codes[at].as_bytes();
            assert!(code.len() == 3);
            let mut raw = 0usize;
            let mut digit = 0;
            while digit < 3 {
                let byte = code[digit];
                assert!(byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
                let nibble = if byte.is_ascii_digit() {
                    byte - b'0'
                } else {
                    byte - b'a' + 10
                };
                raw = raw << 4 | nibble as usize;
                digit += 1;
            }
            assert!(rows[raw] == 0);
            rows[raw] = (row + 1) as u8;
            at += 1;
        }
        row += 1;
    }
    rows
};

/// The type `filetype` names, if it is one the registry knows.
#[must_use]
pub fn media_for_filetype(filetype: Filetype) -> Option<MediaType> {
    let row = usize::from(*FILETYPE_ROWS.get(usize::from(filetype.get()))?);
    FILETYPE_TABLE
        .get(row.checked_sub(1)?)
        .map(|(media, _)| *media)
}

/// What ends a file name: the RISC OS filetype after its last comma, when
/// that is three hex digits, and the extension after the last dot of what
/// comes before it. Each needs a stem of its own, so a name whose only dot or
/// comma starts it ends in neither.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ending<'a> {
    /// What the ending follows: the whole name when it ends in neither.
    pub stem: &'a str,
    /// The filetype the name carries.
    pub filetype: Option<Filetype>,
    /// The extension, without its dot.
    pub extension: Option<&'a str>,
}

impl<'a> Ending<'a> {
    /// What ends `name`.
    #[must_use]
    pub fn of(name: &'a str) -> Self {
        let carried = name.rsplit_once(',').and_then(|(stem, code)| {
            let filetype = acorn::filetype_from_value(code.as_bytes()).ok()?;
            (!stem.is_empty()).then_some((stem, filetype))
        });
        let (rest, filetype) = match carried {
            Some((stem, filetype)) => (stem, Some(filetype)),
            None => (name, None),
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

/// The media type a file name implies, if any: its [`Ending`]'s filetype
/// when that is one the registry knows, else its extension.
///
/// The lookup is ASCII-case-insensitive and allocates no `String`: it splits
/// the suffix off in place and compares it against the static tables.
/// `None` for a name with neither, or a suffix the registry does not
/// recognise.
#[must_use]
pub fn media_for_name(name: &str) -> Option<MediaType> {
    let ending = Ending::of(name);
    if let Some(media) = ending.filetype.and_then(media_for_filetype) {
        return Some(media);
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
/// then the RISC OS filetype a new file of it is given after a `,` — as
/// [`media_for_name`] reads them back, most usual first.
pub fn name_endings(media: MediaType) -> impl Iterator<Item = (char, &'static str)> {
    let extensions = EXTENSION_TABLE
        .iter()
        .filter(move |(held, _)| *held == media)
        .flat_map(|(_, extensions)| extensions.iter().map(|extension| ('.', *extension)));
    let filetype = FILETYPE_TABLE
        .iter()
        .filter(move |(held, _)| *held == media)
        .filter_map(|(_, codes)| codes.first())
        .map(|code| (',', *code));
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
/// [`TairixService`](MediaType::TairixService) when `parent` is a store of
/// programs the system starts and [`TairixApp`](MediaType::TairixApp)
/// otherwise; a regular file is what the layout it stands in names it
/// ([`Site`]), else what [`media_for_document`] reads from its name and the
/// filetype its volume records, or
/// [`ApplicationOctetStream`](MediaType::ApplicationOctetStream) when neither
/// says — fail closed to the generic type.
#[must_use]
pub fn media_for_entry(entry: &Entry, parent: &[String]) -> MediaType {
    media_for_named(
        entry.name(),
        entry.kind(),
        entry.filetype(),
        Site::of(parent),
    )
}

/// What a file called `name` is, its volume recording `recorded` as its RISC
/// OS filetype: what the name's ending says, else what the recorded filetype
/// says, else `None` where neither names a type the registry knows.
///
/// The name decides first because it is TAIRiX's own way of typing a file and
/// RISC OS gives every file a filetype, Data where it was told nothing: a
/// `notes.txt` RISC OS stored as Data is still text. The recorded filetype is
/// what types a file whose name says nothing, as a RISC OS file's never does.
///
/// The one rule a listing and a program opening a document both apply, so a
/// file is the same type wherever it is met.
#[must_use]
pub fn media_for_document(name: &str, recorded: Option<Filetype>) -> Option<MediaType> {
    media_for_name(name).or_else(|| recorded.and_then(media_for_filetype))
}

/// The media type of a node known only by its `name` and `kind` — what a
/// surface describing *one* node has, as against a listing that knows the
/// directory each row came out of.
///
/// The same classification [`media_for_entry`] applies, so a node is typed
/// identically whether it is reached as a listed row or as the subject of its
/// own window. `site` is where it was found, which only a caller that knows
/// the parent can answer; a caller that does not passes [`Site::Plain`], and
/// a bundle types as an application and a file by its name and `recorded`, the
/// RISC OS filetype its volume records for it, alone.
///
/// A link classifies as what it *names*: a shortcut to a folder is typed and
/// opened as a folder. A link that resolves to nothing has no content type at
/// all, so it falls closed to the generic one.
#[must_use]
pub fn media_for_named(
    name: &str,
    kind: EntryKind,
    recorded: Option<Filetype>,
    site: Site,
) -> MediaType {
    match kind.resolved() {
        Some(EntryKind::Directory) => MediaType::InodeDirectory,
        Some(EntryKind::Bundle) if site == Site::StartedStore => MediaType::TairixService,
        Some(EntryKind::Bundle) if site == Site::SaverStore => MediaType::TairixScreensaver,
        Some(EntryKind::Bundle) => MediaType::TairixApp,
        Some(EntryKind::File) => site
            .file_named(name)
            .or_else(|| media_for_document(name, recorded))
            .unwrap_or(MediaType::ApplicationOctetStream),
        // `resolved` never yields a link, and a dangling one yields nothing.
        Some(EntryKind::Link(_)) | None => MediaType::ApplicationOctetStream,
    }
}

/// The icon drawn for a listed entry: the drive a mounted volume's root is
/// on, else its content-type glyph refined by what the browser knows about a
/// plain directory's contents.
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
    entry.drive().unwrap_or_else(|| {
        let site = Site::of(parent);
        icon_of(
            entry,
            media_for_named(entry.name(), entry.kind(), entry.filetype(), site),
            site,
        )
    })
}

/// `media`'s glyph for `entry` found at `site`, refined by a plain
/// directory's occupancy and by the kind of bundle a program runs.
fn icon_of(entry: &Entry, media: MediaType, site: Site) -> IconKind {
    if entry.is_directory() && entry.occupancy().pictured().is_some() {
        IconKind::FolderFilled
    } else if media == MediaType::TairixRxe {
        site.program_icon()
    } else {
        media.icon()
    }
}

/// The icon one entry listed out of the directory `dir` (root-first `parent`)
/// is drawn with, and the request its picture is asked for by.
///
/// The request names the entry's own picture where it has one and `pictures`
/// asks for it: an application bundle's own icon, a RISC OS application's
/// sprite, an occupied folder's picture of what it holds, a picture file's own
/// content — a bundle, an application folder and a picture file each at the
/// version the listing saw. Each falls back to the icon's class picture where it
/// will not serve, a RISC OS application to the folder it is. `scratch` is a
/// buffer the caller reuses across the entries of one frame, so a grid spells
/// its paths without allocating one per tile.
///
/// Every surface that draws an entry's picture — the file manager's tiles and
/// rows, and the desktop — asks here, so an entry cannot be pictured one way
/// in one and another elsewhere.
#[must_use]
pub fn entry_icon<'a>(
    dir: &str,
    parent: &[String],
    entry: &'a Entry,
    pictures: Pictures,
    scratch: &'a mut String,
) -> (IconKind, IconRequest<'a>) {
    if let Some(drive) = entry.drive() {
        return (drive, IconRequest::kind(drive));
    }
    let site = Site::of(parent);
    let media = media_for_named(entry.name(), entry.kind(), entry.filetype(), site);
    let kind = icon_of(entry, media, site);
    let contents = match (
        pictures.folder_contents,
        entry.is_directory(),
        entry.occupancy().pictured(),
    ) {
        (true, true, Some(sample)) => Some(sample),
        _ => None,
    };
    if entry.is_riscos_application() {
        scratch.clear();
        scratch.push_str(dir);
        crate::vfs::push_child(scratch, entry.name());
        // A link's listed stamp is its own, not its target's.
        let stamp =
            (entry.kind() == EntryKind::Directory && !entry.id().is_none()).then(|| entry.stamp());
        let request = IconRequest::riscos_application(kind, scratch, stamp);
        return (
            kind,
            match contents {
                Some(sample) => request.or_folder(sample),
                None => request,
            },
        );
    }
    if let Some(sample) = contents {
        return (kind, IconRequest::folder(sample));
    }
    // A link's listed size and time are its own, not its target's, so they
    // cannot key the picture of what it points at; a listing naming no file
    // gives an open nothing to be checked against.
    let thumbnail = match entry.kind() {
        EntryKind::File if pictures.thumbnails && !entry.id().is_none() => media.thumbnail(),
        _ => None,
    };
    if thumbnail.is_none() && !entry.is_bundle() {
        return (kind, IconRequest::kind(kind));
    }
    scratch.clear();
    scratch.push_str(dir);
    crate::vfs::push_child(scratch, entry.name());
    let request = match (thumbnail, entry.kind()) {
        (Some(reading), _) => IconRequest::thumbnail(kind, scratch, entry.stamp(), reading),
        // A link to a bundle carries its own stamp, not its target's.
        (None, EntryKind::Bundle) if !entry.id().is_none() => {
            IconRequest::listed_bundle(kind, scratch, entry.stamp())
        }
        (None, _) => IconRequest::bundle(kind, scratch),
    };
    (kind, request)
}

/// Where a listed folder stands, as far as what it holds is concerned: the
/// layouts that name what a file is by where it is.
///
/// Every store and layout is spelled once in `lib/abi`; a site is compared
/// against those definitions component-wise rather than a second literal.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Site {
    /// Somewhere no layout names what a file is.
    Plain,
    /// A store of programs the system starts — the service or the component
    /// store — whose bundles no user launches.
    StartedStore,
    /// A screensaver store, the system's or the user's own, whose bundles the
    /// desktop starts in the drawing sandbox and no user launches.
    SaverStore,
    /// A bundle's own folder (`code` false) or its `Code/` folder, the bundle
    /// filed in a store of `kind` — `None` for one outside every store, whose
    /// kind its path cannot say.
    Bundle {
        /// The kind of program the store holding the bundle holds.
        kind: Option<ProgramKind>,
        /// Whether this is the bundle's `Code/` folder.
        code: bool,
    },
    /// Somewhere in the driver store.
    Drivers,
}

impl Site {
    /// Where the folder of root-first `components` stands.
    #[must_use]
    pub fn of(components: &[String]) -> Self {
        if starts_with(components, DRIVER_STORE_PATH) {
            return Self::Drivers;
        }
        if [ProgramKind::Service, ProgramKind::Component]
            .into_iter()
            .any(|kind| is(components, kind.store()))
        {
            return Self::StartedStore;
        }
        // Bundles do not nest, so a folder is in the first bundle its path
        // names.
        let bundle = components.iter().position(|part| is_bundle_name(part));
        let in_saver_store =
            is(components, ProgramKind::Screensaver.store()) || in_user_saver_store(components);
        let Some(at) = bundle else {
            return if in_saver_store {
                Self::SaverStore
            } else {
                Self::Plain
            };
        };
        let (store, within) = (&components[..at], &components[at + 1..]);
        let code = match within {
            [] => false,
            [folder] if folder == BundleEntry::Code.as_str() => true,
            _ => return Self::Plain,
        };
        Self::Bundle {
            kind: store_kind(store),
            code,
        }
    }

    /// What a file named `name` here is by the layout it stands in, if the
    /// layout says: a driver store's `Run` is a driver's image, and a
    /// bundle's `Run` and every file in its `Code/` a program, its `AppInfo`
    /// its signed manifest.
    #[must_use]
    pub fn file_named(self, name: &str) -> Option<MediaType> {
        let entry = BundleEntry::from_name(name);
        match (self, entry) {
            (Self::Drivers, Some(BundleEntry::Run)) => Some(MediaType::TairixDriver),
            (Self::Bundle { code: false, .. }, Some(BundleEntry::Run))
            | (Self::Bundle { code: true, .. }, _) => Some(MediaType::TairixRxe),
            (Self::Bundle { code: false, .. }, Some(BundleEntry::AppInfo)) => {
                Some(MediaType::TairixAppInfo)
            }
            _ => None,
        }
    }

    /// The picture of a program found here: the one of the kind of bundle it
    /// runs, where there is one.
    const fn program_icon(self) -> IconKind {
        match self {
            Self::Bundle {
                kind: Some(ProgramKind::Application),
                ..
            } => IconKind::ApplicationProgram,
            Self::Bundle {
                kind: Some(ProgramKind::Command),
                ..
            } => IconKind::CommandProgram,
            Self::Plain
            | Self::StartedStore
            | Self::SaverStore
            | Self::Drivers
            | Self::Bundle { .. } => IconKind::Executable,
        }
    }
}

/// The kind of program the store of root-first `components` holds: a system
/// store's own kind, or a command, an application or a screensaver for a
/// user's own stores, which file bundles in folders of their own.
fn store_kind(components: &[String]) -> Option<ProgramKind> {
    if let Some(kind) = ProgramKind::ALL
        .into_iter()
        .find(|kind| is(components, kind.store()))
    {
        return Some(kind);
    }
    if in_user_saver_store(components) {
        return Some(ProgramKind::Screensaver);
    }
    match components {
        [users, _, store, ..] if users == USERS_DIR => {
            if store == HOME_COMMAND_STORE_DIR {
                Some(ProgramKind::Command)
            } else if store == HOME_APPLICATION_STORE_DIR {
                Some(ProgramKind::Application)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether root-first `components` are a user's own screensaver store, or a
/// folder the user filed in it.
fn in_user_saver_store(components: &[String]) -> bool {
    tairix_abi::in_user_saver_store(components.iter().map(String::as_str))
}

/// Whether root-first `components` begin with the absolute `path`'s.
fn starts_with(components: &[String], path: &str) -> bool {
    let mut got = components.iter();
    path.split('/')
        .filter(|part| !part.is_empty())
        .all(|want| got.next().is_some_and(|have| have == want))
}

/// Whether root-first `components` are exactly the absolute `path`'s.
fn is(components: &[String], path: &str) -> bool {
    starts_with(components, path)
        && components.len() == path.split('/').filter(|part| !part.is_empty()).count()
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
