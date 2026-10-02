//! The descriptors a configuration descriptor stream concatenates (USB 2.0
//! §9.5), walked once for every reader of one.

/// A descriptor its stream cannot hold: shorter than its own two-byte header,
/// or running past the stream's end.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Malformed;

/// Every descriptor of `bytes`, each its `bLength` bytes. Trailing bytes too
/// few to begin a descriptor end the walk; a malformed descriptor ends it
/// with [`Malformed`].
#[must_use]
pub const fn descriptors(bytes: &[u8]) -> Descriptors<'_> {
    Descriptors { rest: bytes }
}

/// The walk [`descriptors`] returns.
#[derive(Clone, Debug)]
pub struct Descriptors<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Descriptors<'a> {
    type Item = Result<&'a [u8], Malformed>;

    fn next(&mut self) -> Option<Self::Item> {
        let rest = core::mem::take(&mut self.rest);
        let length = usize::from(*rest.first()?);
        if rest.len() < 2 {
            return None;
        }
        if length < 2 || length > rest.len() {
            return Some(Err(Malformed));
        }
        let (descriptor, after) = rest.split_at(length);
        self.rest = after;
        Some(Ok(descriptor))
    }
}

impl core::iter::FusedIterator for Descriptors<'_> {}

#[cfg(test)]
mod tests {
    use super::{descriptors, Malformed};

    #[test]
    fn each_descriptor_is_its_stated_length_and_a_fragment_ends_the_walk() {
        let stream = [3, 0x24, 7, 2, 0x05, 9];
        let walked: [Result<&[u8], Malformed>; 2] = [Ok(&stream[..3]), Ok(&stream[3..5])];
        assert!(descriptors(&stream).eq(walked));
    }

    #[test]
    fn a_descriptor_too_short_or_too_long_ends_the_walk_malformed() {
        assert!(descriptors(&[0, 0x05, 1, 2]).eq([Err(Malformed)]));
        assert!(descriptors(&[1, 0x05]).eq([Err(Malformed)]));
        assert!(descriptors(&[2, 0x04, 9, 0x05]).eq([Ok(&[2u8, 0x04][..]), Err(Malformed)]));
        assert_eq!(descriptors(&[]).next(), None);
    }
}
