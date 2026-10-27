//! A secret a request carries: the password an elevation, a login or a
//! network share offers.

/// A secret borrowed from the buffer it crossed in.
///
/// No `Debug` shows it, so a request holding one can be formatted without
/// the secret reaching a diagnostic, and it implements nothing the log
/// accepts, so it cannot be logged by mistake. It is borrowed, so the one
/// copy is the holder's buffer, which the holder zeroises once the exchange
/// resolves.
///
/// Equality compares the text, so the requests holding one compare in codec
/// round trips; no authority decides by it — a password is judged by what
/// is derived from it.
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct Secret<'a>(&'a str);

impl<'a> Secret<'a> {
    /// The secret `text`.
    #[must_use]
    pub const fn new(text: &'a str) -> Self {
        Self(text)
    }

    /// The secret's text, for the one use it is offered for.
    #[must_use]
    pub const fn expose(self) -> &'a str {
        self.0
    }

    /// Its length in bytes.
    #[must_use]
    pub const fn len(self) -> usize {
        self.0.len()
    }

    /// Whether it is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }
}

impl core::fmt::Debug for Secret<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::format;

    use super::Secret;

    #[test]
    fn no_debug_shows_a_secret() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(..)");
        assert_eq!(format!("{:?}", Some(secret)), "Some(Secret(..))");
        assert_eq!((secret.expose(), secret.len()), ("hunter2", 7));
        assert!(Secret::new("").is_empty());
    }
}
