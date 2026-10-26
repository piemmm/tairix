//! The one DER codec: ASN.1's Distinguished Encoding Rules (ITU-T X.690 §10),
//! read and written.
//!
//! SPNEGO (RFC 4178) and Kerberos (RFC 4120) both speak DER, and both are
//! spoken by strangers, so the reader is strict: every value has exactly one
//! encoding, and any other — an indefinite or non-minimal length, a padded
//! integer, a boolean that is neither `00` nor `FF`, a bit string with stray
//! padding bits, a non-minimal identifier — is refused as
//! [`DerError::NonCanonical`] rather than tolerated. Nothing is guessed and
//! nothing is repaired.
//!
//! - [`Reader`] is zero-copy: every content it returns borrows the input. It
//!   descends at most [`MAX_DEPTH`] levels, and an element that does not fit
//!   in what encloses it is [`DerError::Truncated`].
//! - [`Writer`] builds one encoding in a buffer, latching the first failure
//!   so a message is written with plain calls and checked once.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod read;
mod write;

pub use read::{BitString, Reader, Tlv};
pub use write::Writer;

use core::fmt;

/// The deepest a reader descends into nested constructed elements.
///
/// A security bound on a stranger's structure, not a capacity: Kerberos's
/// deepest message nests about a dozen levels, and recursion past this is
/// refused as [`DerError::TooDeep`] rather than walked.
pub const MAX_DEPTH: usize = 32;

/// An identifier's class (X.690 §8.1.2.2).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Class {
    /// The types ASN.1 itself defines.
    Universal,
    /// A tag a protocol assigns its own messages (Kerberos's `[APPLICATION n]`).
    Application,
    /// A tag scoped to the enclosing type (`[n]`).
    Context,
    /// A tag private to an organisation.
    Private,
}

impl Class {
    const fn bits(self) -> u8 {
        match self {
            Self::Universal => 0x00,
            Self::Application => 0x40,
            Self::Context => 0x80,
            Self::Private => 0xc0,
        }
    }

    const fn from_bits(byte: u8) -> Self {
        match byte & 0xc0 {
            0x00 => Self::Universal,
            0x40 => Self::Application,
            0x80 => Self::Context,
            _ => Self::Private,
        }
    }
}

/// An element's identifier: its class, whether it is constructed, and its
/// number.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Tag {
    class: Class,
    constructed: bool,
    number: u32,
}

impl Tag {
    /// `BOOLEAN`.
    pub const BOOLEAN: Self = Self::universal(1, false);
    /// `INTEGER`.
    pub const INTEGER: Self = Self::universal(2, false);
    /// `BIT STRING`.
    pub const BIT_STRING: Self = Self::universal(3, false);
    /// `OCTET STRING`.
    pub const OCTET_STRING: Self = Self::universal(4, false);
    /// `NULL`.
    pub const NULL: Self = Self::universal(5, false);
    /// `OBJECT IDENTIFIER`.
    pub const OBJECT_IDENTIFIER: Self = Self::universal(6, false);
    /// `ENUMERATED`.
    pub const ENUMERATED: Self = Self::universal(10, false);
    /// `SEQUENCE` and `SEQUENCE OF`.
    pub const SEQUENCE: Self = Self::universal(16, true);
    /// `GeneralizedTime`.
    pub const GENERALIZED_TIME: Self = Self::universal(24, false);
    /// `GeneralString`, Kerberos's `KerberosString`.
    pub const GENERAL_STRING: Self = Self::universal(27, false);

    const fn universal(number: u32, constructed: bool) -> Self {
        Self {
            class: Class::Universal,
            constructed,
            number,
        }
    }

    /// An identifier of any class, number and form.
    #[must_use]
    pub const fn new(class: Class, constructed: bool, number: u32) -> Self {
        Self {
            class,
            constructed,
            number,
        }
    }

    /// `[n]` wrapping an explicitly tagged value: context class, constructed.
    #[must_use]
    pub const fn context(number: u32) -> Self {
        Self::new(Class::Context, true, number)
    }

    /// `[APPLICATION n]` wrapping a constructed value.
    #[must_use]
    pub const fn application(number: u32) -> Self {
        Self::new(Class::Application, true, number)
    }

    /// The class.
    #[must_use]
    pub const fn class(self) -> Class {
        self.class
    }

    /// Whether the contents are themselves elements.
    #[must_use]
    pub const fn is_constructed(self) -> bool {
        self.constructed
    }

    /// The number within the class.
    #[must_use]
    pub const fn number(self) -> u32 {
        self.number
    }
}

/// Why an encoding was refused or could not be written.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DerError {
    /// The input ended inside an element, or an element claimed more than
    /// what encloses it holds.
    Truncated,
    /// Bytes followed the last element where nothing may.
    TrailingData,
    /// A length was indefinite, which DER never uses.
    Indefinite,
    /// A valid BER encoding that is not DER's one canonical form, or bytes
    /// that are no encoding at all.
    NonCanonical,
    /// An element of another identifier stood where one was required.
    UnexpectedTag,
    /// Nesting went past [`MAX_DEPTH`].
    TooDeep,
    /// A value does not fit the type it is read into or written from.
    OutOfRange,
    /// The writer's buffer could not grow.
    OutOfMemory,
    /// An element's contents are longer than a four-octet length states,
    /// which no message this codec serves approaches.
    TooLong,
}

impl fmt::Display for DerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "der: truncated element",
            Self::TrailingData => "der: trailing data",
            Self::Indefinite => "der: indefinite length",
            Self::NonCanonical => "der: not the canonical encoding",
            Self::UnexpectedTag => "der: unexpected tag",
            Self::TooDeep => "der: nested too deeply",
            Self::OutOfRange => "der: value out of range",
            Self::OutOfMemory => "der: out of memory",
            Self::TooLong => "der: element too long",
        })
    }
}

#[cfg(test)]
mod tests;
