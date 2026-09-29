//! The colours a syntax colourer draws with.
//!
//! [`SyntaxRole`] is the closed vocabulary every lexer classifies text into
//! and every painter draws from, so a format and a theme meet in one place:
//! adding a language adds no colour, and adding a theme adds no code.

use crate::color::Rgba;

/// What a span of a document is, as far as colouring it goes.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum SyntaxRole {
    /// Text no rule claims.
    Plain,
    /// A keyword, or a literal the language spells as a word (`true`).
    Keyword,
    /// A type name.
    Type,
    /// A function or macro name.
    Function,
    /// A string or character literal.
    String,
    /// An escape inside text: `\n`, `&amp;`.
    Escape,
    /// A numeric literal.
    Number,
    /// A comment.
    Comment,
    /// An operator or punctuation.
    Punctuation,
    /// A markup element name.
    Tag,
    /// A markup attribute or a style property.
    Attribute,
    /// A settings or object key.
    Key,
    /// A directive: a preprocessor line, a doctype, a prolog, a shebang, a
    /// section header.
    Directive,
    /// A heading in prose markup.
    Heading,
    /// Emphasis in prose markup.
    Emphasis,
    /// A link or address.
    Link,
    /// Code quoted in prose markup.
    Code,
    /// Text the format refuses: an unterminated string, a stray delimiter.
    Error,
    /// A control byte, drawn as a `[x03]` token.
    Control,
    /// A byte that is not UTF-8, drawn as a `[xC3]` token.
    Invalid,
    /// A format character that draws nothing — a bidirectional control or a
    /// zero-width character — drawn as a `[U+202E]` token.
    Invisible,
}

impl SyntaxRole {
    /// How many roles there are.
    pub const COUNT: usize = core::mem::variant_count::<Self>();

    /// Every role, in declaration order: the one index a wire encoding of a
    /// role uses.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Plain,
        Self::Keyword,
        Self::Type,
        Self::Function,
        Self::String,
        Self::Escape,
        Self::Number,
        Self::Comment,
        Self::Punctuation,
        Self::Tag,
        Self::Attribute,
        Self::Key,
        Self::Directive,
        Self::Heading,
        Self::Emphasis,
        Self::Link,
        Self::Code,
        Self::Error,
        Self::Control,
        Self::Invalid,
        Self::Invisible,
    ];

    /// Whether [`ALL`](Self::ALL) lists every role at its own index.
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

    /// This role's position in [`ALL`](Self::ALL).
    #[must_use]
    pub const fn index(self) -> u8 {
        self as u8
    }

    /// The role at `index` in [`ALL`](Self::ALL), if there is one.
    #[must_use]
    pub const fn from_index(index: u8) -> Option<Self> {
        let at = index as usize;
        if at < Self::COUNT {
            Some(Self::ALL[at])
        } else {
            None
        }
    }
}

const _: () = assert!(SyntaxRole::listed_in_order());

/// A theme's colour for every [`SyntaxRole`] but [`Plain`](SyntaxRole::Plain),
/// which draws in [`Palette::on_surface`](crate::Palette::on_surface).
///
/// Each is authored against [`Palette::document`](crate::Palette::document),
/// the ground a document is drawn on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SyntaxPalette {
    /// [`SyntaxRole::Keyword`].
    pub keyword: Rgba,
    /// [`SyntaxRole::Type`].
    pub type_name: Rgba,
    /// [`SyntaxRole::Function`].
    pub function: Rgba,
    /// [`SyntaxRole::String`].
    pub string: Rgba,
    /// [`SyntaxRole::Escape`].
    pub escape: Rgba,
    /// [`SyntaxRole::Number`].
    pub number: Rgba,
    /// [`SyntaxRole::Comment`].
    pub comment: Rgba,
    /// [`SyntaxRole::Punctuation`].
    pub punctuation: Rgba,
    /// [`SyntaxRole::Tag`].
    pub tag: Rgba,
    /// [`SyntaxRole::Attribute`].
    pub attribute: Rgba,
    /// [`SyntaxRole::Key`].
    pub key: Rgba,
    /// [`SyntaxRole::Directive`].
    pub directive: Rgba,
    /// [`SyntaxRole::Heading`].
    pub heading: Rgba,
    /// [`SyntaxRole::Emphasis`].
    pub emphasis: Rgba,
    /// [`SyntaxRole::Link`].
    pub link: Rgba,
    /// [`SyntaxRole::Code`].
    pub code: Rgba,
    /// [`SyntaxRole::Error`].
    pub error: Rgba,
    /// [`SyntaxRole::Control`].
    pub control: Rgba,
    /// [`SyntaxRole::Invalid`].
    pub invalid: Rgba,
    /// [`SyntaxRole::Invisible`].
    pub invisible: Rgba,
}

impl SyntaxPalette {
    /// The dark appearance's colours.
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            keyword: Rgba::rgb(0xf0, 0x89, 0x4a),
            type_name: Rgba::rgb(0x5e, 0xc8, 0xd8),
            function: Rgba::rgb(0x7a, 0xb4, 0xf5),
            string: Rgba::rgb(0x9e, 0xd3, 0x6a),
            escape: Rgba::rgb(0xf5, 0xc3, 0x5a),
            number: Rgba::rgb(0xc7, 0x9b, 0xf2),
            comment: Rgba::rgb(0x8a, 0x95, 0x9c),
            punctuation: Rgba::rgb(0xb6, 0xc0, 0xc6),
            tag: Rgba::rgb(0x6a, 0xae, 0xf5),
            attribute: Rgba::rgb(0xe8, 0xc2, 0x6a),
            key: Rgba::rgb(0x8c, 0xc8, 0xf0),
            directive: Rgba::rgb(0xe0, 0x7a, 0xb8),
            heading: Rgba::rgb(0xff, 0xa5, 0x5a),
            emphasis: Rgba::rgb(0xf0, 0xd5, 0x8c),
            link: Rgba::rgb(0x5f, 0xb0, 0xff),
            code: Rgba::rgb(0xa8, 0xd8, 0x80),
            error: Rgba::rgb(0xff, 0x6b, 0x6b),
            control: Rgba::rgb(0x5b, 0x8c, 0xff),
            invalid: Rgba::rgb(0xff, 0x7a, 0x59),
            invisible: Rgba::rgb(0xe8, 0xb1, 0x3a),
        }
    }

    /// The light appearance's colours.
    #[must_use]
    pub const fn light() -> Self {
        Self {
            keyword: Rgba::rgb(0xb0, 0x4a, 0x0c),
            type_name: Rgba::rgb(0x0a, 0x72, 0x85),
            function: Rgba::rgb(0x1d, 0x5f, 0xb8),
            string: Rgba::rgb(0x3d, 0x7d, 0x16),
            escape: Rgba::rgb(0x9a, 0x62, 0x00),
            number: Rgba::rgb(0x7a, 0x3d, 0xb0),
            comment: Rgba::rgb(0x6a, 0x70, 0x75),
            punctuation: Rgba::rgb(0x4a, 0x52, 0x58),
            tag: Rgba::rgb(0x17, 0x57, 0xb0),
            attribute: Rgba::rgb(0x8a, 0x57, 0x00),
            key: Rgba::rgb(0x0b, 0x5f, 0x8f),
            directive: Rgba::rgb(0xa3, 0x29, 0x6e),
            heading: Rgba::rgb(0xa8, 0x43, 0x00),
            emphasis: Rgba::rgb(0x6b, 0x4f, 0x00),
            link: Rgba::rgb(0x0b, 0x62, 0xc4),
            code: Rgba::rgb(0x2f, 0x6e, 0x12),
            error: Rgba::rgb(0xc4, 0x16, 0x16),
            control: Rgba::rgb(0x1d, 0x4f, 0xd8),
            invalid: Rgba::rgb(0xc2, 0x41, 0x0c),
            invisible: Rgba::rgb(0x8f, 0x62, 0x00),
        }
    }
}
