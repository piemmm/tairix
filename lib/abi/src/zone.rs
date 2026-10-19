//! The names of civil time zones.
//!
//! A zone is named by its IANA identifier (`Europe/London`). The shape rule is
//! defined here once, because the zone store's format, the desktop's
//! description of itself and the machine's own setting (`time.zone`,
//! `lib/sysconfig`) must each refuse the same names.

use core::fmt;

/// The longest zone name.
pub const ZONE_NAME_MAX: usize = 64;

/// Whether `text` has a zone name's shape: one to [`ZONE_NAME_MAX`] bytes of
/// ASCII letters, digits, `_`, `-` and `+`, in components joined by single
/// `/`s.
#[must_use]
pub fn is_zone_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= ZONE_NAME_MAX
        && text.split('/').all(|component| {
            !component.is_empty()
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+'))
        })
}

/// A zone's name, held inline so a fixed record can carry one: a value with
/// [`is_zone_name`]'s shape.
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct ZoneName {
    len: u8,
    /// The name's bytes, zero past `len`.
    bytes: [u8; ZONE_NAME_MAX],
}

impl ZoneName {
    /// `name` as a zone name, or `None` unless it has one's shape.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        if !is_zone_name(name) {
            return None;
        }
        let mut bytes = [0u8; ZONE_NAME_MAX];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        Some(Self {
            len: u8::try_from(name.len()).ok()?,
            bytes,
        })
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.bytes
            .get(..usize::from(self.len))
            .and_then(|name| core::str::from_utf8(name).ok())
            .unwrap_or_default()
    }

    /// The name's length and its bytes, zero past it, as a record carries
    /// them.
    pub(crate) const fn wire(&self) -> (u8, &[u8; ZONE_NAME_MAX]) {
        (self.len, &self.bytes)
    }

    /// The name a record carries as `len` bytes of `field`, or `None` for a
    /// zero length.
    ///
    /// # Errors
    ///
    /// [`crate::Errno::OutOfRange`] for a length past the field or a name
    /// without one's shape; [`crate::Errno::BadMagic`] for a byte past the
    /// name that is not zero.
    pub(crate) fn from_wire(len: u8, field: &[u8]) -> Result<Option<Self>, crate::Errno> {
        let len = usize::from(len);
        let (name, tail) = field
            .split_at_checked(len)
            .ok_or(crate::Errno::OutOfRange)?;
        if tail.iter().any(|&byte| byte != 0) {
            return Err(crate::Errno::BadMagic);
        }
        if len == 0 {
            return Ok(None);
        }
        core::str::from_utf8(name)
            .ok()
            .and_then(Self::new)
            .map(Some)
            .ok_or(crate::Errno::OutOfRange)
    }
}

impl fmt::Debug for ZoneName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zone_name_has_one_shape() {
        for name in [
            "UTC",
            "Europe/London",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+5",
        ] {
            assert!(is_zone_name(name), "{name}");
        }
        for name in [
            "",
            "/London",
            "Europe/",
            "Europe//London",
            "Europe/Lon don",
            "Ü",
        ] {
            assert!(!is_zone_name(name), "{name:?}");
        }
        assert!(is_zone_name(&"A".repeat(ZONE_NAME_MAX)));
        assert!(!is_zone_name(&"A".repeat(ZONE_NAME_MAX + 1)));
    }

    #[test]
    fn a_held_name_reads_back_and_its_record_is_admitted_whole() {
        let name = ZoneName::new("Europe/Paris").expect("a zone name");
        assert_eq!(name.as_str(), "Europe/Paris");
        assert_eq!(ZoneName::new("not a zone"), None);
        let (len, field) = name.wire();
        assert_eq!(ZoneName::from_wire(len, field), Ok(Some(name)));
        assert_eq!(ZoneName::from_wire(0, &[0; ZONE_NAME_MAX]), Ok(None));
        let mut dirty = *field;
        dirty[ZONE_NAME_MAX - 1] = 1;
        assert_eq!(
            ZoneName::from_wire(len, &dirty),
            Err(crate::Errno::BadMagic)
        );
        assert_eq!(
            ZoneName::from_wire(u8::try_from(ZONE_NAME_MAX + 1).expect("small"), field),
            Err(crate::Errno::OutOfRange)
        );
        let mut spaced = *field;
        spaced[6] = b' ';
        assert_eq!(
            ZoneName::from_wire(len, &spaced),
            Err(crate::Errno::OutOfRange)
        );
    }
}
