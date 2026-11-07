//! What a reader could not keep of a file.
//!
//! A drawing read from a file holds what the drawing model can hold; what it
//! cannot — a filter, a mask, a picture until embedded rasters land — is
//! recorded here rather than silently dropped, so an editor can say what was
//! lost and keep the file it came from from being written over with less.

/// A part of a file the drawing could not keep.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Lost {
    /// Filter effects.
    Filters,
    /// Masks.
    Masks,
    /// Pattern fills, drawn with their fallback colour.
    Patterns,
    /// Markers other than the arrowheads the drawing keeps.
    Markers,
    /// Embedded pictures.
    Images,
    /// Text set in more than one style within one text object.
    TextStyling,
    /// Text placed character by character.
    TextPositioning,
    /// Text set along a path.
    TextPaths,
    /// Strokes that keep their width however they are scaled.
    NonScalingStrokes,
    /// Content in another language, such as HTML.
    ForeignContent,
    /// Animation.
    Animation,
    /// Scripts.
    Scripts,
    /// The alternatives of a conditional group other than the one shown.
    Alternatives,
    /// Hyperlinks.
    Links,
    /// Titles and descriptions of objects, and metadata.
    Annotations,
}

impl Lost {
    /// Every part, in the order an editor lists them.
    pub const ALL: [Self; 15] = [
        Self::Filters,
        Self::Masks,
        Self::Patterns,
        Self::Markers,
        Self::Images,
        Self::TextStyling,
        Self::TextPositioning,
        Self::TextPaths,
        Self::NonScalingStrokes,
        Self::ForeignContent,
        Self::Animation,
        Self::Scripts,
        Self::Alternatives,
        Self::Links,
        Self::Annotations,
    ];

    const fn bit(self) -> u32 {
        1 << self as u32
    }

    /// What an editor calls it, plural, in a list of what was lost.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Filters => "filter effects",
            Self::Masks => "masks",
            Self::Patterns => "pattern fills",
            Self::Markers => "markers",
            Self::Images => "embedded pictures",
            Self::TextStyling => "mixed text styles",
            Self::TextPositioning => "character positions",
            Self::TextPaths => "text on paths",
            Self::NonScalingStrokes => "non-scaling strokes",
            Self::ForeignContent => "foreign content",
            Self::Animation => "animation",
            Self::Scripts => "scripts",
            Self::Alternatives => "conditional alternatives",
            Self::Links => "links",
            Self::Annotations => "object titles, descriptions and metadata",
        }
    }
}

/// Every part of a file a reader could not keep.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Unkept(u32);

impl Unkept {
    /// Nothing was lost.
    pub const NONE: Self = Self(0);

    /// Note that `lost` was.
    pub const fn insert(&mut self, lost: Lost) {
        self.0 |= lost.bit();
    }

    /// Whether `lost` was.
    #[must_use]
    pub const fn contains(self, lost: Lost) -> bool {
        self.0 & lost.bit() != 0
    }

    /// Whether everything was kept.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Both readers' losses.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Every part lost, in the order an editor lists them.
    pub fn iter(self) -> impl Iterator<Item = Lost> {
        Lost::ALL
            .into_iter()
            .filter(move |&lost| self.contains(lost))
    }

    /// The bits, for a wire form.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The losses `bits` names, refusing a bit no loss stands for.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        let known = (1_u32 << Lost::ALL.len()) - 1;
        if bits & !known == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Lost, Unkept};

    #[test]
    fn losses_are_kept_apart_and_listed_in_order() {
        let mut unkept = Unkept::NONE;
        assert!(unkept.is_empty());
        unkept.insert(Lost::Masks);
        unkept.insert(Lost::Filters);
        assert!(unkept.contains(Lost::Masks) && !unkept.contains(Lost::Images));
        let listed: alloc::vec::Vec<Lost> = unkept.iter().collect();
        assert_eq!(listed, [Lost::Filters, Lost::Masks]);
        assert_eq!(Unkept::from_bits(unkept.bits()), Some(unkept));
        assert_eq!(Unkept::from_bits(1 << 31), None);
    }
}
