//! One object of a drawing: a shape, a text, or a group.

use alloc::sync::Arc;

use tairix_bezier::{Affine, Bounds};

use crate::children::Children;
use crate::id::ObjectId;
use crate::limits::{self, MAX_DEPTH, MAX_NAME_BYTES, MAX_OBJECTS, MAX_TOTAL_PATH_NODES};
use crate::shape::Shape;
use crate::style::Style;
use crate::text::Text;
use crate::DrawingError;

/// What an object is.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// A shape, painted as its style says.
    Shape {
        /// Its geometry.
        shape: Shape,
        /// How it is painted.
        style: Arc<Style>,
    },
    /// A text object, painted as its style says.
    Text {
        /// Its text and how it is set.
        text: Arc<Text>,
        /// How it is painted.
        style: Arc<Style>,
    },
    /// A group of objects, drawn together.
    Group {
        /// Its objects, bottom first.
        children: Children,
        /// The object whose outline clips them, in the group's space, if
        /// any: what it fills, whatever its own paint, is what shows.
        clip: Option<Arc<Node>>,
    },
}

/// One object of a drawing, immutable once made: an edit makes a new node.
///
/// Every node is checked as it is made — its transform, opacity, name,
/// style, geometry and text within their ranges, and a group's subtree within
/// the drawing's bounds — and carries its conservative bounds in its
/// parent's space, so nothing culling by bounds ever leaves it out.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    id: ObjectId,
    name: Option<Arc<str>>,
    visible: bool,
    locked: bool,
    opacity: f64,
    transform: Affine,
    kind: Kind,
    /// Its bounds in its own space, and in its parent's.
    local: Bounds,
    bounds: Bounds,
    objects: usize,
    path_nodes: usize,
    depth: usize,
}

impl Node {
    /// A visible, unlocked, opaque, untransformed object of `kind`.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] or [`DrawingError::TooComplex`] for a value
    /// a drawing cannot hold, and [`DrawingError::OutOfMemory`] when its
    /// outline cannot be traced.
    pub fn new(id: ObjectId, kind: Kind) -> Result<Self, DrawingError> {
        Self::assembled(id, None, true, false, 1.0, Affine::IDENTITY, kind)
    }

    /// A node of every part given.
    fn assembled(
        id: ObjectId,
        name: Option<Arc<str>>,
        visible: bool,
        locked: bool,
        opacity: f64,
        transform: Affine,
        kind: Kind,
    ) -> Result<Self, DrawingError> {
        check_name(name.as_deref())?;
        if !(0.0..=1.0).contains(&opacity) || !limits::transform(transform) {
            return Err(DrawingError::Invalid);
        }
        let (local, objects, path_nodes, depth) = measure(&kind)?;
        if objects > MAX_OBJECTS || path_nodes > MAX_TOTAL_PATH_NODES || depth > MAX_DEPTH {
            return Err(DrawingError::TooComplex);
        }
        Ok(Self {
            id,
            name,
            visible,
            locked,
            opacity,
            transform,
            kind,
            local,
            bounds: local.transformed(transform),
            objects,
            path_nodes,
            depth,
        })
    }

    /// Its identity.
    #[must_use]
    pub const fn id(&self) -> ObjectId {
        self.id
    }

    /// Its name, if it has one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Whether it is drawn.
    #[must_use]
    pub const fn visible(&self) -> bool {
        self.visible
    }

    /// Whether it is kept from being chosen and edited.
    #[must_use]
    pub const fn locked(&self) -> bool {
        self.locked
    }

    /// The opacity it is composited at, `0..=1`.
    #[must_use]
    pub const fn opacity(&self) -> f64 {
        self.opacity
    }

    /// Its space into its parent's.
    #[must_use]
    pub const fn transform(&self) -> Affine {
        self.transform
    }

    /// What it is.
    #[must_use]
    pub const fn kind(&self) -> &Kind {
        &self.kind
    }

    /// Bounds that hold everything it paints, in its own space.
    #[must_use]
    pub const fn local_bounds(&self) -> Bounds {
        self.local
    }

    /// Bounds that hold everything it paints, in its parent's space.
    #[must_use]
    pub const fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// How many objects it is, everything within it counted.
    #[must_use]
    pub const fn objects(&self) -> usize {
        self.objects
    }

    /// How many path nodes it holds.
    #[must_use]
    pub const fn path_nodes(&self) -> usize {
        self.path_nodes
    }

    /// How deep it nests: one for a shape or text.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Its children, if it is a group.
    #[must_use]
    pub const fn children(&self) -> Option<&Children> {
        match &self.kind {
            Kind::Group { children, .. } => Some(children),
            Kind::Shape { .. } | Kind::Text { .. } => None,
        }
    }

    /// Its style, if it is a shape or text.
    #[must_use]
    pub const fn style(&self) -> Option<&Arc<Style>> {
        match &self.kind {
            Kind::Shape { style, .. } | Kind::Text { style, .. } => Some(style),
            Kind::Group { .. } => None,
        }
    }

    /// The node with the identity `id`.
    #[must_use]
    pub fn with_id(&self, id: ObjectId) -> Self {
        Self { id, ..self.clone() }
    }

    /// The node named `name`, or unnamed.
    ///
    /// # Errors
    ///
    /// As [`Node::new`].
    pub fn with_name(&self, name: Option<Arc<str>>) -> Result<Self, DrawingError> {
        check_name(name.as_deref())?;
        Ok(Self {
            name,
            ..self.clone()
        })
    }

    /// The node shown or hidden.
    #[must_use]
    pub fn with_visible(&self, visible: bool) -> Self {
        Self {
            visible,
            ..self.clone()
        }
    }

    /// The node locked or unlocked.
    #[must_use]
    pub fn with_locked(&self, locked: bool) -> Self {
        Self {
            locked,
            ..self.clone()
        }
    }

    /// The node composited at `opacity`.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] outside `0..=1`.
    pub fn with_opacity(&self, opacity: f64) -> Result<Self, DrawingError> {
        if !(0.0..=1.0).contains(&opacity) {
            return Err(DrawingError::Invalid);
        }
        Ok(Self {
            opacity,
            ..self.clone()
        })
    }

    /// The node placed by `transform`.
    ///
    /// # Errors
    ///
    /// [`DrawingError::Invalid`] for a transform a drawing cannot hold.
    pub fn with_transform(&self, transform: Affine) -> Result<Self, DrawingError> {
        if !limits::transform(transform) {
            return Err(DrawingError::Invalid);
        }
        Ok(Self {
            transform,
            bounds: self.local.transformed(transform),
            ..self.clone()
        })
    }

    /// The node made `kind`, everything else kept.
    ///
    /// # Errors
    ///
    /// As [`Node::new`].
    pub fn with_kind(&self, kind: Kind) -> Result<Self, DrawingError> {
        Self::assembled(
            self.id,
            self.name.clone(),
            self.visible,
            self.locked,
            self.opacity,
            self.transform,
            kind,
        )
    }
}

/// Whether `name` may name an object: not empty, holding no control
/// character, and within the length bound.
pub(crate) fn check_name(name: Option<&str>) -> Result<(), DrawingError> {
    let Some(name) = name else {
        return Ok(());
    };
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err(DrawingError::Invalid);
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(DrawingError::TooComplex);
    }
    Ok(())
}

/// `kind`'s bounds in its own space, the objects and path nodes it holds and
/// how deep it nests, its every value checked.
fn measure(kind: &Kind) -> Result<(Bounds, usize, usize, usize), DrawingError> {
    match kind {
        Kind::Shape { shape, style } => {
            shape.check()?;
            style.check()?;
            let local = style.painted_bounds(shape.bounds()?);
            Ok((local, 1, shape.node_count(), 1))
        }
        Kind::Text { text, style } => {
            text.check()?;
            style.check()?;
            Ok((style.painted_bounds(text.estimated_bounds()), 1, 0, 1))
        }
        Kind::Group { children, clip } => {
            let summary = children.summary();
            let mut local = summary.bounds;
            let mut objects = summary.objects.saturating_add(1);
            let mut path_nodes = summary.path_nodes;
            let mut depth = summary.depth;
            if let Some(clip) = clip {
                local = local.intersection(clip.bounds());
                if local.is_empty() {
                    local = Bounds::EMPTY;
                }
                objects = objects.saturating_add(clip.objects());
                path_nodes = path_nodes.saturating_add(clip.path_nodes());
                depth = depth.max(clip.depth());
            }
            Ok((local, objects, path_nodes, depth + 1))
        }
    }
}

#[cfg(test)]
#[path = "node_tests.rs"]
mod tests;
