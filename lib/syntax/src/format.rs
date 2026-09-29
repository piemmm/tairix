//! The closed set of formats a document is coloured and checked as.

/// What a document is written in.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Format {
    /// Text with no syntax.
    PlainText,
    /// HTML, with its scripts and style sheets.
    Html,
    /// XML, SVG included.
    Xml,
    /// A CSS style sheet.
    Css,
    /// JavaScript.
    JavaScript,
    /// JSON.
    Json,
    /// YAML.
    Yaml,
    /// TOML.
    Toml,
    /// Markdown.
    Markdown,
    /// Rust.
    Rust,
    /// C, and its headers.
    C,
    /// Java.
    Java,
    /// Python.
    Python,
    /// A shell script.
    Shell,
    /// An application's `key = value` settings document (`lib/appconf`).
    AppSettings,
    /// The program library catalog (`lib/proglib`).
    ProgramLibrary,
    /// The boot-time system configuration store (`lib/sysconfig`).
    SystemConfig,
    /// The network configuration store (`lib/netconfig`).
    NetworkConfig,
    /// The service enrolment overrides.
    ServiceOverrides,
    /// The users database (`lib/users`).
    UsersDb,
    /// The groups database (`lib/users`).
    GroupsDb,
    /// A font family manifest (`lib/fontface`).
    FontFamily,
}

impl Format {
    /// How many formats there are.
    pub const COUNT: usize = core::mem::variant_count::<Self>();

    /// Every format, in declaration order: the one index a wire encoding of
    /// a format uses, and the order a chooser lists them in.
    pub const ALL: [Self; Self::COUNT] = [
        Self::PlainText,
        Self::Html,
        Self::Xml,
        Self::Css,
        Self::JavaScript,
        Self::Json,
        Self::Yaml,
        Self::Toml,
        Self::Markdown,
        Self::Rust,
        Self::C,
        Self::Java,
        Self::Python,
        Self::Shell,
        Self::AppSettings,
        Self::ProgramLibrary,
        Self::SystemConfig,
        Self::NetworkConfig,
        Self::ServiceOverrides,
        Self::UsersDb,
        Self::GroupsDb,
        Self::FontFamily,
    ];

    /// Whether [`ALL`](Self::ALL) lists every format at its own index.
    const fn listed_in_order() -> bool {
        let mut at = 0;
        while at < Self::COUNT {
            if Self::ALL[at] as usize != at {
                return false;
            }
            at += 1;
        }
        true
    }

    /// This format's position in [`ALL`](Self::ALL).
    #[must_use]
    pub const fn index(self) -> u8 {
        self as u8
    }

    /// The format at `index` in [`ALL`](Self::ALL), if there is one.
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        let at = index as usize;
        if at < Self::COUNT {
            Some(Self::ALL[at])
        } else {
            None
        }
    }

    /// What a chooser calls this format.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PlainText => "Plain text",
            Self::Html => "HTML",
            Self::Xml => "XML",
            Self::Css => "CSS",
            Self::JavaScript => "JavaScript",
            Self::Json => "JSON",
            Self::Yaml => "YAML",
            Self::Toml => "TOML",
            Self::Markdown => "Markdown",
            Self::Rust => "Rust",
            Self::C => "C",
            Self::Java => "Java",
            Self::Python => "Python",
            Self::Shell => "Shell script",
            Self::AppSettings => "App settings",
            Self::ProgramLibrary => "Program library",
            Self::SystemConfig => "System configuration",
            Self::NetworkConfig => "Network configuration",
            Self::ServiceOverrides => "Service overrides",
            Self::UsersDb => "Users database",
            Self::GroupsDb => "Groups database",
            Self::FontFamily => "Font family manifest",
        }
    }

    /// Whether this is a TAIRiX settings store, which is validated against
    /// the parser the system reads it with.
    #[must_use]
    pub const fn is_store(self) -> bool {
        self.store_len().is_some()
    }

    /// The longest document this format's store reads, or `None` for a
    /// format that is not a store.
    pub(crate) const fn store_len(self) -> Option<usize> {
        match self {
            Self::SystemConfig => Some(tairix_sysconfig::MAX_CONFIG_LEN),
            Self::NetworkConfig => Some(tairix_netconfig::MAX_CONFIG_LEN),
            Self::ServiceOverrides => Some(tairix_enrolment::MAX_DOCUMENT_LEN),
            Self::UsersDb => Some(tairix_users::MAX_DB_LEN),
            Self::GroupsDb => Some(tairix_users::MAX_GROUPS_DB_LEN),
            Self::FontFamily => Some(tairix_fontface::MAX_MANIFEST_BYTES),
            Self::AppSettings | Self::ProgramLibrary => Some(tairix_appconf::MAX_DOCUMENT_LEN),
            Self::PlainText
            | Self::Html
            | Self::Xml
            | Self::Css
            | Self::JavaScript
            | Self::Json
            | Self::Yaml
            | Self::Toml
            | Self::Markdown
            | Self::Rust
            | Self::C
            | Self::Java
            | Self::Python
            | Self::Shell => None,
        }
    }

    /// The marker that comments out the rest of a line, for a format that
    /// has one.
    #[must_use]
    pub const fn line_comment(self) -> Option<&'static str> {
        match self {
            Self::JavaScript | Self::Rust | Self::C | Self::Java => Some("//"),
            Self::Yaml
            | Self::Toml
            | Self::Python
            | Self::Shell
            | Self::AppSettings
            | Self::ProgramLibrary
            | Self::SystemConfig
            | Self::NetworkConfig
            | Self::ServiceOverrides
            | Self::UsersDb
            | Self::GroupsDb
            | Self::FontFamily => Some("#"),
            Self::PlainText | Self::Html | Self::Xml | Self::Css | Self::Json | Self::Markdown => {
                None
            }
        }
    }
}

const _: () = assert!(Format::listed_in_order());
