//! A page: its geometry and resources, each attribute the page tree lets a
//! page inherit taken from the nearest node that states it.

use alloc::sync::Arc;

use tairix_bezier::Affine;
use tairix_collections::try_arc;

use crate::content;
use crate::display::DisplayList;
use crate::document::Document;
use crate::error::tolerated;
use crate::limits::{MAX_BOX_COORDINATE, MAX_USER_UNIT};
use crate::object::{Dict, Object};
use crate::PdfError;

/// The attributes a page inherits from the nodes above it, as those nodes
/// state them, each shared by every node and page below the one stating it.
#[derive(Default)]
pub(crate) struct Inherited {
    resources: Option<Arc<Object>>,
    media_box: Option<Arc<Object>>,
    crop_box: Option<Arc<Object>>,
    rotate: Option<Arc<Object>>,
}

impl Inherited {
    /// What the kids of tree node `node` inherit, `node` being under
    /// `above`: shared with `above` where `node` states none of them.
    pub(crate) fn extended(above: &Arc<Self>, node: &Dict) -> Result<Arc<Self>, PdfError> {
        let stated = |key: &[u8]| -> Result<Option<Arc<Object>>, PdfError> {
            match node.get(key) {
                Some(value) => Ok(Some(try_arc(value.try_clone()?)?)),
                None => Ok(None),
            }
        };
        let (resources, media_box, crop_box, rotate) = (
            stated(b"Resources")?,
            stated(b"MediaBox")?,
            stated(b"CropBox")?,
            stated(b"Rotate")?,
        );
        if resources.is_none() && media_box.is_none() && crop_box.is_none() && rotate.is_none() {
            return Ok(Arc::clone(above));
        }
        Ok(try_arc(Self {
            resources: resources.or_else(|| above.resources.clone()),
            media_box: media_box.or_else(|| above.media_box.clone()),
            crop_box: crop_box.or_else(|| above.crop_box.clone()),
            rotate: rotate.or_else(|| above.rotate.clone()),
        })?)
    }
}

/// What page `dict` states for `key`, else what it inherits.
fn attribute<'a>(dict: &'a Dict, key: &[u8], inherited: Option<&'a Object>) -> Option<&'a Object> {
    dict.get(key).or(inherited)
}

/// A rectangle in a page's default user space.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Rect {
    pub(crate) x0: f64,
    pub(crate) y0: f64,
    pub(crate) x1: f64,
    pub(crate) y1: f64,
}

impl Rect {
    /// The rectangle with opposite corners `(x0, y0)` and `(x1, y1)`.
    pub(crate) fn spanning(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    /// US Letter: what a page with no usable media box is read as, as every
    /// reader does.
    const LETTER: Self = Self {
        x0: 0.0,
        y0: 0.0,
        x1: 612.0,
        y1: 792.0,
    };

    fn width(self) -> f64 {
        self.x1 - self.x0
    }

    fn height(self) -> f64 {
        self.y1 - self.y0
    }

    /// The rectangle `object` states: four numbers, any two opposite corners.
    fn of(document: &Document, object: &Object) -> Result<Option<Self>, PdfError> {
        let object = document.resolve(object)?;
        let Some([a, b, c, d]) = object
            .as_array()
            .and_then(|items| <&[Object; 4]>::try_from(items).ok())
        else {
            return Ok(None);
        };
        let mut corners = [0.0; 4];
        for (corner, item) in corners.iter_mut().zip([a, b, c, d]) {
            match document.resolve(item)?.as_number() {
                Some(value) if value.is_finite() && value.abs() <= MAX_BOX_COORDINATE => {
                    *corner = value;
                }
                _ => return Ok(None),
            }
        }
        let [x0, y0, x1, y1] = corners;
        let rect = Self::spanning(x0, y0, x1, y1);
        Ok(rect.has_area().then_some(rect))
    }

    /// Where it overlaps `other`, if it does.
    fn intersection(self, other: Self) -> Option<Self> {
        let rect = Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        rect.has_area().then_some(rect)
    }

    /// Whether its sides are normal positive lengths: a page box narrower
    /// than any number holds is no box.
    fn has_area(self) -> bool {
        self.width().is_normal()
            && self.width() > 0.0
            && self.height().is_normal()
            && self.height() > 0.0
    }
}

/// A page of a [`Document`], read only through the document it is of.
pub struct Page<'d> {
    document: &'d Document,
    pub(crate) dict: Dict,
    /// The resources it inherits, which its own replace.
    inherited_resources: Option<Arc<Object>>,
    crop_box: Rect,
    /// Clockwise quarter turns it is shown rotated by.
    quarter_turns: u8,
    user_unit: f64,
}

impl<'d> Page<'d> {
    /// Page `dict` of `document`, inheriting `inherited`.
    pub(crate) fn new(
        document: &'d Document,
        dict: Dict,
        inherited: &Inherited,
    ) -> Result<Self, PdfError> {
        // A box or rotation that cannot be read is read as none, so one
        // damaged attribute costs a page its geometry rather than the
        // document its page list.
        let boxed = |key: &[u8], inherited: Option<&Object>| -> Result<Option<Rect>, PdfError> {
            Ok(match attribute(&dict, key, inherited) {
                Some(stated) => tolerated(Rect::of(document, stated))?.flatten(),
                None => None,
            })
        };
        let media_box = boxed(b"MediaBox", inherited.media_box.as_deref())?.unwrap_or(Rect::LETTER);
        let crop_box = boxed(b"CropBox", inherited.crop_box.as_deref())?
            .and_then(|crop| crop.intersection(media_box))
            .unwrap_or(media_box);
        let rotate = match attribute(&dict, b"Rotate", inherited.rotate.as_deref()) {
            Some(rotate) => tolerated(document.resolve(rotate))?
                .and_then(|rotate| rotate.as_int())
                .unwrap_or(0),
            None => 0,
        };
        // A rotation that is not a quarter turn is read as none.
        let quarter_turns = if rotate % 90 == 0 {
            u8::try_from((rotate / 90).rem_euclid(4)).unwrap_or(0)
        } else {
            0
        };
        // A unit is kept only where the size it scales the page to is still
        // a size, never one too small for a number to hold.
        let shown = |unit: f64| {
            (crop_box.width() * unit).is_normal() && (crop_box.height() * unit).is_normal()
        };
        let user_unit = tolerated(document.get(&dict, b"UserUnit"))?
            .and_then(|unit| unit.as_number())
            .filter(|&unit| unit > 0.0 && unit <= MAX_USER_UNIT && shown(unit))
            .unwrap_or(1.0);
        Ok(Self {
            document,
            dict,
            inherited_resources: inherited.resources.clone(),
            crop_box,
            quarter_turns,
            user_unit,
        })
    }

    /// What it paints, in order.
    ///
    /// # Errors
    ///
    /// [`PdfError::TooComplex`] for a page past a containment bound, and
    /// [`PdfError::OutOfMemory`]. Damaged content is not refused: what can be
    /// read is painted, and the list records what was lost.
    pub fn display_list(&self) -> Result<DisplayList, PdfError> {
        content::interpret(self)
    }

    /// The document it is of.
    pub(crate) const fn document(&self) -> &'d Document {
        self.document
    }

    /// Its resources, its own or those it inherits: read only when its
    /// content is, so listing pages reads none.
    pub(crate) fn resources(&self) -> Result<Dict, PdfError> {
        let stated = attribute(
            &self.dict,
            b"Resources",
            self.inherited_resources.as_deref(),
        );
        Ok(match stated {
            Some(resources) => match self.document.resolve(resources)?.into_owned()? {
                Object::Dict(resources) => resources,
                _ => Dict::new(),
            },
            None => Dict::new(),
        })
    }

    /// Its width and height as shown, in points: its crop box turned by its
    /// rotation and scaled by its user unit.
    #[must_use]
    pub fn size(&self) -> (f64, f64) {
        let (width, height) = (
            self.crop_box.width() * self.user_unit,
            self.crop_box.height() * self.user_unit,
        );
        if self.quarter_turns % 2 == 1 {
            (height, width)
        } else {
            (width, height)
        }
    }

    /// The map from its default user space to the space it is shown in:
    /// points from the top left of its rotated crop box, y down.
    pub(crate) fn transform(&self) -> Affine {
        let Rect { x0, y0, x1, y1 } = self.crop_box;
        let (width, height) = (x1 - x0, y1 - y0);
        // Flipped to y down about the crop box's top left.
        let upright = Affine {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            e: -x0,
            f: y1,
        };
        let turned = match self.quarter_turns {
            1 => Affine {
                a: 0.0,
                b: 1.0,
                c: -1.0,
                d: 0.0,
                e: height,
                f: 0.0,
            },
            2 => Affine {
                a: -1.0,
                b: 0.0,
                c: 0.0,
                d: -1.0,
                e: width,
                f: height,
            },
            3 => Affine {
                a: 0.0,
                b: -1.0,
                c: 1.0,
                d: 0.0,
                e: 0.0,
                f: width,
            },
            _ => Affine::IDENTITY,
        };
        upright
            .then(turned)
            .then(Affine::scale(self.user_unit, self.user_unit))
    }
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;
