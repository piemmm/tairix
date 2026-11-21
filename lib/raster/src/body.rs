//! What a window lays at one pixel of a frame: its client's pixel cut to its
//! shape, the plate beneath the client, or its furniture, at its opacity.
//!
//! The compositor composes every window row through it and the pinned `body`
//! pass is held to it, so a window composed on the CPU and one composed on a
//! device lay the same pixels.

use crate::color::{div255, Pixel};

/// What a window holds at one pixel of a frame, before it is laid.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Body {
    /// Where the pixel is one of the client's columns on one of its rows: the
    /// client's own pixel there — else the plate laid where it has none, else
    /// nothing — and how much of the pixel the client's cut covers.
    pub client: Option<(Option<Pixel>, u8)>,
    /// The furniture's pixel there, where a strip holds one.
    pub furniture: Option<Pixel>,
}

impl Body {
    /// The pixel laid here and the factor it is laid at by a window drawn at
    /// `opacity`, through [`Pixel::laid_over`]; `None` where the window lays
    /// nothing.
    ///
    /// A client pixel the cut leaves partly uncovered gives way to the
    /// furniture, because there the frame's rim is what the window's curve is
    /// made of; only where no furniture is does the coverage anti-alias the
    /// client's own edge.
    #[must_use]
    pub fn laid(self, opacity: u8) -> Option<(Pixel, u8)> {
        let furniture = self.furniture.map(|pixel| (pixel, opacity));
        match self.client {
            None => furniture,
            Some((client, u8::MAX)) => client.map(|pixel| (pixel, opacity)),
            Some((client, coverage)) => furniture.or_else(|| {
                client.map(|pixel| (pixel, div255(u32::from(opacity) * u32::from(coverage))))
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Body, Pixel};

    const CLIENT: Pixel = Pixel {
        r: 10,
        g: 20,
        b: 30,
        a: 200,
    };
    const FURNITURE: Pixel = Pixel {
        r: 40,
        g: 50,
        b: 60,
        a: 255,
    };

    #[test]
    fn a_covered_client_pixel_is_laid_at_the_opacity_whatever_furniture_is_there() {
        let body = Body {
            client: Some((Some(CLIENT), 255)),
            furniture: Some(FURNITURE),
        };
        assert_eq!(body.laid(180), Some((CLIENT, 180)));
        let bare = Body {
            client: Some((None, 255)),
            furniture: Some(FURNITURE),
        };
        assert_eq!(
            bare.laid(180),
            None,
            "a covered pixel with nothing lays nothing"
        );
    }

    #[test]
    fn a_cut_client_pixel_gives_way_to_furniture_and_else_is_weakened_by_its_coverage() {
        let cut = |furniture| Body {
            client: Some((Some(CLIENT), 128)),
            furniture,
        };
        assert_eq!(cut(Some(FURNITURE)).laid(180), Some((FURNITURE, 180)));
        assert_eq!(cut(None).laid(180), Some((CLIENT, 90)));
        assert_eq!(cut(None).laid(255), Some((CLIENT, 128)));
        let outside = Body {
            client: Some((Some(CLIENT), 0)),
            furniture: None,
        };
        assert_eq!(outside.laid(255), Some((CLIENT, 0)));
    }

    #[test]
    fn past_the_client_only_furniture_is_laid() {
        let furniture = Body {
            client: None,
            furniture: Some(FURNITURE),
        };
        assert_eq!(furniture.laid(200), Some((FURNITURE, 200)));
        assert_eq!(Body::default().laid(200), None);
    }
}
