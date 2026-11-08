//! What the editor's commands do to a drawing.
//!
//! Every operation takes a drawing and answers a new one, sharing every node
//! it does not touch, so it is one step of history whatever it changes. An
//! object is named by its path from its layer; several objects named at once
//! are worked on in each one's own parent, a map given in the drawing's
//! space brought into that parent's before it is applied.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::boolean::{boolean, components, BooleanError, Operation};
use tairix_bezier::fit::{fitted, FitError};
use tairix_bezier::offset::{grown, Join};
use tairix_bezier::{Affine, Bounds, FillRule, Path, Point};
use tairix_collections::{try_arc, TryReserveError};
use tairix_drawing::render::stroke_area;
use tairix_drawing::text::Glyphs;
use tairix_drawing::{
    Baseline, Children, Drawing, DrawingError, Ids, Kind, Node, NodePath, Paint, PathShape,
    RunStyle, Shape, SharedStr, Style, Text, TextPath, DEFAULT_LINE_HEIGHT,
};
use tairix_util::fallible;
use tairix_util::mathf::{atan2, fabs, fmax, sqrt};

/// Why an edit could not be made.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EditError {
    /// The drawing refused what the edit would make of it.
    Drawing(DrawingError),
    /// Nothing the edit works on was named, or what was named is not what it
    /// works on.
    Nothing,
    /// The objects named do not share the parent the edit needs.
    Apart,
    /// Text the edit traces holds characters whose glyphs could not be had.
    Unlettered,
    /// The edit would leave nothing of an object it works on.
    Vanishes,
    /// The pictures the edit brings in would take the drawing's past what a
    /// drawing's pictures may hold together.
    Crowded,
}

impl From<DrawingError> for EditError {
    fn from(error: DrawingError) -> Self {
        Self::Drawing(error)
    }
}

impl From<FitError> for EditError {
    fn from(error: FitError) -> Self {
        Self::Drawing(error.into())
    }
}

impl From<BooleanError> for EditError {
    fn from(error: BooleanError) -> Self {
        Self::Drawing(error.into())
    }
}

impl From<TryReserveError> for EditError {
    fn from(error: TryReserveError) -> Self {
        Self::Drawing(error.into())
    }
}

impl From<alloc::collections::TryReserveError> for EditError {
    fn from(error: alloc::collections::TryReserveError) -> Self {
        Self::Drawing(error.into())
    }
}

impl core::fmt::Display for EditError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Drawing(DrawingError::Invalid) => {
                "the result would hold a value a drawing cannot"
            }
            Self::Drawing(DrawingError::TooComplex) => "the result would be too complex",
            Self::Drawing(DrawingError::OutOfMemory) => "there is not enough memory",
            Self::Nothing => "there is nothing selected it applies to",
            Self::Apart => "the objects must be in the same group or layer",
            Self::Unlettered => "the font has no glyph for some of the text's characters",
            Self::Vanishes => "it would leave nothing of an object",
            Self::Crowded => "the drawing would hold more pictures than a drawing can",
        })
    }
}

type Result<T> = core::result::Result<T, EditError>;

/// The group a path's last step is taken in: its parent's path, and the
/// object's index there.
fn split(path: &[usize]) -> Option<(&[usize], usize)> {
    let (&index, parent) = path.split_last()?;
    Some((parent, index))
}

/// The children of the group at `path`.
fn children_at<'d>(drawing: &'d Drawing, path: &[usize]) -> Option<&'d Children> {
    drawing.node_at(path)?.children()
}

/// `drawing` with the group at `parent` holding `children` instead.
fn with_children(drawing: &Drawing, parent: &[usize], children: Children) -> Result<Drawing> {
    let group = drawing.node_at(parent).ok_or(EditError::Nothing)?;
    let Kind::Group { clip, .. } = group.kind() else {
        return Err(EditError::Nothing);
    };
    let rebuilt = group.with_kind(Kind::Group {
        children,
        clip: clip.clone(),
    })?;
    Ok(drawing.with_node_at(parent, try_arc(rebuilt)?)?)
}

/// The map from the space the node at `path` is placed in — its parent's —
/// onto the drawing's.
#[must_use]
pub fn parent_space(drawing: &Drawing, path: &[usize]) -> Affine {
    let mut map = Affine::IDENTITY;
    for depth in (1..path.len()).rev() {
        if let Some(node) = drawing.node_at(&path[..depth]) {
            map = map.then(node.transform());
        }
    }
    map
}

/// The exact bounds, in the drawing's space, of what the objects at `paths`
/// outline: their geometry, as a selection is framed and aligned by, not the
/// conservative reach of their paint.
///
/// Text is framed by what its glyphs ink where `glyphs` knows them all, and
/// by its estimated box where it does not.
#[must_use]
pub fn bounds_of(drawing: &Drawing, paths: &[NodePath], glyphs: &dyn Glyphs) -> Bounds {
    framed(drawing, paths, glyphs).bounds
}

/// The bounds [`bounds_of`] answers, and whether they are final: no text
/// among them waits on a glyph `glyphs` does not yet know.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Framed {
    /// The bounds.
    pub bounds: Bounds,
    /// Whether every text was framed by its glyphs.
    pub complete: bool,
}

/// What [`bounds_of`] answers, saying whether it is final.
#[must_use]
pub fn framed(drawing: &Drawing, paths: &[NodePath], glyphs: &dyn Glyphs) -> Framed {
    let mut complete = true;
    let bounds = paths.iter().fold(Bounds::EMPTY, |bounds, path| {
        drawing.node_at(path).map_or(bounds, |node| {
            let space = parent_space(drawing, path);
            bounds.union(outline_bounds(node, space, glyphs, &mut complete))
        })
    });
    Framed { bounds, complete }
}

/// What `text` inks, where `glyphs` knows every glyph of it and it inks
/// anything; its estimated box otherwise, `complete` cleared where the
/// estimate stands in for glyphs not yet known.
fn text_bounds(text: &tairix_drawing::Text, glyphs: &dyn Glyphs, complete: &mut bool) -> Bounds {
    let inked = match text.set(glyphs) {
        Ok(setting) if setting.complete => {
            setting.glyphs.iter().fold(Bounds::EMPTY, |bounds, placed| {
                bounds.union(placed.glyph.ink().transformed(placed.at))
            })
        }
        _ => {
            *complete = false;
            Bounds::EMPTY
        }
    };
    if inked.is_empty() {
        text.estimated_bounds()
    } else {
        inked
    }
}

/// The tight bounds of `path` mapped through `map`, taken a segment at a time
/// so the path is never copied to be measured.
fn mapped_bounds(path: &Path, map: Affine) -> Bounds {
    path.subpaths.iter().fold(Bounds::EMPTY, |bounds, subpath| {
        subpath.segments().fold(
            bounds.union(Bounds::of_point(map.map(subpath.start))),
            |bounds, segment| bounds.union(segment.transformed(map).bounds()),
        )
    })
}

/// The exact bounds of `node`'s outline in the space `to_drawing` maps its
/// parent's onto.
fn outline_bounds(
    node: &Node,
    to_drawing: Affine,
    glyphs: &dyn Glyphs,
    complete: &mut bool,
) -> Bounds {
    let map = node.transform().then(to_drawing);
    match node.kind() {
        Kind::Shape { shape, .. } => shape.to_path().map_or_else(
            |_| node.local_bounds().transformed(map),
            |path| mapped_bounds(&path, map),
        ),
        Kind::Text { text, .. } => text_bounds(text, glyphs, complete).transformed(map),
        Kind::Image { image } => image.place().transformed(map),
        Kind::Group { children, clip } => {
            let inner = children.iter().fold(Bounds::EMPTY, |bounds, child| {
                bounds.union(outline_bounds(child, map, glyphs, complete))
            });
            match clip {
                Some(clip) => inner.intersection(outline_bounds(clip, map, glyphs, complete)),
                None => inner,
            }
        }
    }
}

/// `node` put last in the group at `parent`, answering its path.
///
/// # Errors
///
/// [`EditError::Nothing`] where `parent` names no group, and
/// [`EditError::Crowded`] where its pictures would take the drawing's past
/// what they may hold together.
pub fn insert(drawing: &Drawing, parent: &[usize], node: Arc<Node>) -> Result<(Drawing, NodePath)> {
    admit(drawing, core::slice::from_ref(&node))?;
    if parent.is_empty() {
        let layers = drawing.layers();
        let at = layers.len();
        let drawing = drawing.with_layers(layers.inserted(at, node)?)?;
        return Ok((drawing, alloc::vec![at]));
    }
    let children = children_at(drawing, parent).ok_or(EditError::Nothing)?;
    let at = children.len();
    let drawing = with_children(drawing, parent, children.inserted(at, node)?)?;
    let mut path = Vec::new();
    if !fallible::reserve(&mut path, parent.len() + 1) {
        return Err(DrawingError::OutOfMemory.into());
    }
    path.extend_from_slice(parent);
    path.push(at);
    Ok((drawing, path))
}

/// `nodes` put last in the group at `parent`, in order, answering their
/// paths.
///
/// # Errors
///
/// [`EditError::Nothing`] where `parent` names no group within a layer, and
/// [`EditError::Crowded`] where their pictures would take the drawing's past
/// what they may hold together.
pub fn append(
    drawing: &Drawing,
    parent: &[usize],
    nodes: Vec<Arc<Node>>,
) -> Result<(Drawing, Vec<NodePath>)> {
    if parent.is_empty() {
        return Err(EditError::Nothing);
    }
    admit(drawing, &nodes)?;
    let children = children_at(drawing, parent).ok_or(EditError::Nothing)?;
    let first = children.len();
    let count = nodes.len();
    let drawing = with_children(drawing, parent, children.appended(nodes)?)?;
    Ok((drawing, paths_in(parent, first..first + count)?))
}

/// Refuse `nodes` where their pictures would take `drawing`'s past what a
/// drawing's may hold, so no reader refuses what is written for its pictures'
/// pixels.
fn admit(drawing: &Drawing, nodes: &[Arc<Node>]) -> Result<()> {
    if tairix_drawing::pictures_fit(drawing, nodes)? {
        Ok(())
    } else {
        Err(EditError::Crowded)
    }
}

/// The paths of the children `indices` of the group at `parent`.
pub(crate) fn paths_in(
    parent: &[usize],
    indices: impl ExactSizeIterator<Item = usize>,
) -> Result<Vec<NodePath>> {
    let mut paths = Vec::new();
    if !fallible::reserve(&mut paths, indices.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    for index in indices {
        let mut path = Vec::new();
        if !fallible::reserve(&mut path, parent.len() + 1) {
            return Err(DrawingError::OutOfMemory.into());
        }
        path.extend_from_slice(parent);
        path.push(index);
        paths.push(path);
    }
    Ok(paths)
}

/// The indices within their shared parent of the objects at `paths`,
/// ascending, each once.
fn indices_of(paths: &[NodePath]) -> Result<Vec<usize>> {
    let mut indices = Vec::new();
    if !fallible::reserve(&mut indices, paths.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    indices.extend(paths.iter().filter_map(|path| path.last().copied()));
    indices.sort_unstable();
    indices.dedup();
    Ok(indices)
}

/// The children at `indices` of `children`, shared.
fn taken(children: &Children, indices: &[usize]) -> Result<Vec<Arc<Node>>> {
    let mut nodes = Vec::new();
    if !fallible::reserve(&mut nodes, indices.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    for &index in indices {
        nodes.push(children.get(index).ok_or(EditError::Nothing)?.clone());
    }
    Ok(nodes)
}

/// `paths` in the order that keeps every later one valid as earlier ones
/// are taken out: deepest and last first.
fn removal_order(paths: &[NodePath]) -> Result<Vec<NodePath>> {
    let mut ordered = Vec::new();
    if !fallible::reserve(&mut ordered, paths.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    ordered.extend(paths.iter().cloned());
    ordered.sort_unstable_by(|a, b| b.cmp(a));
    ordered.dedup();
    Ok(ordered)
}

/// `drawing` without the objects at `paths`. A layer is never removed here;
/// see [`remove_layer`].
///
/// # Errors
///
/// [`EditError::Nothing`] where a path names no object within a layer.
pub fn remove(drawing: &Drawing, paths: &[NodePath]) -> Result<Drawing> {
    let ordered = removal_order(paths)?;
    let mut drawing = drawing.clone();
    let mut rest = ordered.as_slice();
    // Siblings are taken out together. Going deepest and last first, taking
    // a run out moves nothing a later run names.
    while let Some(first) = rest.first() {
        let (parent, _) = split(first)
            .filter(|(parent, _)| !parent.is_empty())
            .ok_or(EditError::Nothing)?;
        let run = rest
            .iter()
            .take_while(|path| split(path).is_some_and(|(other, _)| other == parent))
            .count();
        let (siblings, later) = rest.split_at(run);
        rest = later;
        let mut indices = Vec::new();
        if !fallible::reserve(&mut indices, run) {
            return Err(DrawingError::OutOfMemory.into());
        }
        indices.extend(
            siblings
                .iter()
                .rev()
                .filter_map(|path| path.last().copied()),
        );
        let children = children_at(&drawing, parent).ok_or(EditError::Nothing)?;
        if indices.last().is_some_and(|&last| last >= children.len()) {
            return Err(EditError::Nothing);
        }
        drawing = with_children(&drawing, parent, children.without(&indices)?)?;
    }
    Ok(drawing)
}

/// Replace each object at `paths` by what `change` makes of it, given the
/// drawing as it stands and the object's path; one it makes nothing of is
/// left as it is, so a drawing nothing changed in is the very drawing given.
///
/// # Errors
///
/// [`EditError::Nothing`] where `paths` is empty or names no object, or what
/// `change` refuses.
pub(crate) fn each(
    drawing: &Drawing,
    paths: &[NodePath],
    mut change: impl FnMut(&Drawing, &[usize], &Node) -> Result<Option<Node>>,
) -> Result<Drawing> {
    if paths.is_empty() {
        return Err(EditError::Nothing);
    }
    let mut ordered = Vec::new();
    if !fallible::reserve(&mut ordered, paths.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    ordered.extend(paths.iter().map(Vec::as_slice));
    ordered.sort_unstable();
    ordered.dedup();
    let mut drawing = drawing.clone();
    let mut rest = ordered.as_slice();
    // Siblings are replaced together, so each parent is rebuilt once.
    while let Some(first) = rest.first() {
        let (parent, _) = split(first).ok_or(EditError::Nothing)?;
        let run = rest
            .iter()
            .take_while(|path| split(path).is_some_and(|(other, _)| other == parent))
            .count();
        let (siblings, later) = rest.split_at(run);
        rest = later;
        let mut changes = Vec::new();
        for path in siblings {
            let node = drawing.node_at(path).ok_or(EditError::Nothing)?;
            if let Some(changed) = change(&drawing, path, node)? {
                let index = path.last().copied().ok_or(EditError::Nothing)?;
                if !fallible::push(&mut changes, (index, try_arc(changed)?)) {
                    return Err(DrawingError::OutOfMemory.into());
                }
            }
        }
        if changes.is_empty() {
            continue;
        }
        drawing = if parent.is_empty() {
            drawing.with_layers(drawing.layers().with_replaced(changes)?)?
        } else {
            let children = children_at(&drawing, parent).ok_or(EditError::Nothing)?;
            with_children(&drawing, parent, children.with_replaced(changes)?)?
        };
    }
    Ok(drawing)
}

/// Every object at `paths` moved by `map`, given in the drawing's space.
///
/// # Errors
///
/// As the drawing refuses a transform past its bounds.
pub fn transform(drawing: &Drawing, paths: &[NodePath], map: Affine) -> Result<Drawing> {
    each(drawing, paths, |drawing, path, node| {
        mapped(drawing, path, node, map).map(Some)
    })
}

/// `node`, at `path`, moved by `map`, given in the drawing's space.
fn mapped(drawing: &Drawing, path: &[usize], node: &Node, map: Affine) -> Result<Node> {
    let outer = parent_space(drawing, path);
    let back = outer.invert().ok_or(DrawingError::Invalid)?;
    Ok(node.with_transform(node.transform().then(outer).then(map).then(back))?)
}

/// The objects at `paths` each moved by the distance `moves` gives its path,
/// in the drawing's space, in one pass; those it gives none are left as they
/// are, so moving nothing is the very drawing given.
fn moved_by(
    drawing: &Drawing,
    paths: &[NodePath],
    mut moves: Vec<(&[usize], Point)>,
) -> Result<Drawing> {
    if moves.is_empty() {
        return Ok(drawing.clone());
    }
    moves.sort_unstable_by(|a, b| a.0.cmp(b.0));
    each(drawing, paths, |drawing, path, node| {
        let Ok(at) = moves.binary_search_by(|(moved, _)| (*moved).cmp(path)) else {
            return Ok(None);
        };
        let by = moves[at].1;
        mapped(drawing, path, node, Affine::translate(by.x, by.y)).map(Some)
    })
}

/// An object being restyled, as it stands: what its outline spans in its
/// own space, and where that space lies on the page.
pub struct Placed<'a> {
    node: &'a Node,
    to_drawing: Affine,
    glyphs: &'a dyn Glyphs,
}

impl Placed<'_> {
    /// The bounds of its outline in its own space, which a gradient fitted
    /// to it spans.
    #[must_use]
    pub fn bounds(&self) -> Bounds {
        own_bounds(self.node, self.glyphs)
    }

    /// The map from its own space onto the drawing's.
    #[must_use]
    pub const fn to_drawing(&self) -> Affine {
        self.to_drawing
    }
}

/// What restyles an object: its style, and the object as it stands.
pub type Restyle<'a> =
    dyn Fn(&Style, &Placed<'_>) -> core::result::Result<Style, DrawingError> + 'a;

/// Every object at `paths` restyled by `restyle`, text framed by what
/// `glyphs` knows of it; a group's objects within it, every one.
///
/// # Errors
///
/// What `restyle` refuses, and as the drawing refuses a style.
pub fn restyle(
    drawing: &Drawing,
    paths: &[NodePath],
    restyle: &Restyle<'_>,
    glyphs: &dyn Glyphs,
) -> Result<Drawing> {
    each(drawing, paths, |drawing, path, node| {
        restyled(node, parent_space(drawing, path), restyle, glyphs)
    })
}

/// `node`, whose parent's space `to_drawing` maps onto the drawing's,
/// restyled by `restyle`, or nothing where it changes no style.
fn restyled(
    node: &Node,
    to_drawing: Affine,
    restyle: &Restyle<'_>,
    glyphs: &dyn Glyphs,
) -> Result<Option<Node>> {
    let placed = Placed {
        node,
        to_drawing: node.transform().then(to_drawing),
        glyphs,
    };
    let kind = match node.kind() {
        Kind::Shape { shape, style } => {
            let Some(style) = changed_style(style, &|style| restyle(style, &placed))? else {
                return Ok(None);
            };
            Kind::Shape {
                shape: shape.clone(),
                style,
            }
        }
        Kind::Text { text, style: was } => {
            let Some(style) = changed_style(was, &|style| restyle(style, &placed))? else {
                return Ok(None);
            };
            // A paint given the whole object reaches every character of it.
            let fill = style.fill != was.fill;
            let stroke = style.stroke.paint != was.stroke.paint;
            let text = if (fill || stroke) && text.repaints() {
                let repainted = text.restyled(0..text.content.len(), |run| RunStyle {
                    fill: run.fill.filter(|_| !fill),
                    stroke: run.stroke.filter(|_| !stroke),
                    ..run.clone()
                })?;
                try_arc(repainted)?
            } else {
                text.clone()
            };
            Kind::Text { text, style }
        }
        // A picture has no style to change.
        Kind::Image { .. } => return Ok(None),
        Kind::Group { children, clip } => {
            let mut changed = Vec::new();
            for (index, child) in children.iter().enumerate() {
                if let Some(child) = restyled(child, placed.to_drawing, restyle, glyphs)? {
                    if !fallible::push(&mut changed, (index, try_arc(child)?)) {
                        return Err(DrawingError::OutOfMemory.into());
                    }
                }
            }
            if changed.is_empty() {
                return Ok(None);
            }
            Kind::Group {
                children: children.with_replaced(changed)?,
                clip: clip.clone(),
            }
        }
    };
    Ok(Some(node.with_kind(kind)?))
}

/// `style` restyled, or nothing where restyling leaves it as it is.
fn changed_style(
    style: &Arc<Style>,
    restyle: &dyn Fn(&Style) -> core::result::Result<Style, DrawingError>,
) -> Result<Option<Arc<Style>>> {
    let restyled = restyle(style)?;
    if restyled == **style {
        return Ok(None);
    }
    Ok(Some(try_arc(restyled)?))
}

/// The angle the one object at `paths` stands at: the direction its own
/// rightward runs on the page, in degrees anticlockwise. Several objects
/// together stand at none of their own.
#[must_use]
pub fn angle_of(drawing: &Drawing, paths: &[NodePath]) -> f64 {
    let [path] = paths else {
        return 0.0;
    };
    let Some(node) = drawing.node_at(path) else {
        return 0.0;
    };
    let map = node.transform().then(parent_space(drawing, path));
    if map.a == 0.0 && map.b == 0.0 {
        return 0.0;
    }
    crate::gradient::normalised(-atan2(map.b, map.a).to_degrees())
}

/// The exact bounds of `node`'s outline in its own space; text's by what
/// `glyphs` knows of it, else its estimated box.
#[must_use]
pub fn own_bounds(node: &Node, glyphs: &dyn Glyphs) -> Bounds {
    match node.kind() {
        Kind::Shape { shape, .. } => shape.bounds().unwrap_or_else(|_| node.local_bounds()),
        Kind::Text { text, .. } => text_bounds(text, glyphs, &mut true),
        Kind::Image { image } => image.place(),
        Kind::Group { .. } => node.local_bounds(),
    }
}

/// Where an object goes in its parent's stack.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Arrange {
    /// Up past the next object.
    Raise,
    /// Down past the next object.
    Lower,
    /// To the top.
    Top,
    /// To the bottom.
    Bottom,
}

/// The objects at `paths`, which must share a parent, moved as `arrange`
/// says, answering where each now is.
///
/// # Errors
///
/// [`EditError::Apart`] for objects in different parents.
pub fn arrange(
    drawing: &Drawing,
    paths: &[NodePath],
    arrange: Arrange,
) -> Result<(Drawing, Vec<NodePath>)> {
    let parent = common_parent(paths)?;
    let children = children_at(drawing, parent).ok_or(EditError::Nothing)?;
    let moving = indices_of(paths)?;
    let (Some(&first), Some(&last)) = (moving.first(), moving.last()) else {
        return Err(EditError::Nothing);
    };
    if last >= children.len() {
        return Err(EditError::Nothing);
    }
    let kept = children
        .len()
        .checked_sub(moving.len())
        .ok_or(EditError::Nothing)?;
    // Where the moving objects go among those staying.
    let place = match arrange {
        Arrange::Top => kept,
        Arrange::Bottom => 0,
        Arrange::Raise => (last + 2 - moving.len()).min(kept),
        Arrange::Lower => first.saturating_sub(1),
    };
    let placed = paths_in(parent, place..place + moving.len())?;
    let in_place = moving.iter().zip(place..).all(|(&index, at)| index == at);
    if in_place {
        return Ok((drawing.clone(), placed));
    }
    let taken = taken(children, &moving)?;
    let rearranged = children
        .without(&moving)?
        .with_inserted(taken.into_iter().map(|node| (place, node)))?;
    Ok((with_children(drawing, parent, rearranged)?, placed))
}

/// The parent every one of `paths` shares.
fn common_parent(paths: &[NodePath]) -> Result<&[usize]> {
    let (first, rest) = paths.split_first().ok_or(EditError::Nothing)?;
    let (parent, _) = split(first).ok_or(EditError::Nothing)?;
    if parent.is_empty()
        || rest
            .iter()
            .any(|path| split(path).map(|(other, _)| other) != Some(parent))
    {
        return Err(EditError::Apart);
    }
    Ok(parent)
}

/// The objects at `paths`, which must share a parent, gathered into a new
/// group where the topmost of them stood, answering the group's path.
///
/// # Errors
///
/// [`EditError::Apart`] for objects in different parents.
pub fn group(drawing: &Drawing, paths: &[NodePath], ids: &mut Ids) -> Result<(Drawing, NodePath)> {
    let parent = common_parent(paths)?;
    let children = children_at(drawing, parent).ok_or(EditError::Nothing)?;
    let members = indices_of(paths)?;
    let top = members.last().copied().ok_or(EditError::Nothing)?;
    let at = top + 1 - members.len();
    let group = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(taken(children, &members)?)?,
            clip: None,
        },
    )?;
    let regrouped = children.without(&members)?.inserted(at, try_arc(group)?)?;
    let drawing = with_children(drawing, parent, regrouped)?;
    let mut path = paths_in(parent, core::iter::once(at))?;
    Ok((drawing, path.pop().ok_or(EditError::Nothing)?))
}

/// The group at `path` replaced by what it holds, its place, opacity and
/// clip carried by each — a clipped member held in a group of its own, with
/// identities issued from `ids` — answering where each now is.
///
/// # Errors
///
/// [`EditError::Nothing`] where `path` names no group within a layer.
pub fn ungroup(
    drawing: &Drawing,
    path: &[usize],
    ids: &mut Ids,
) -> Result<(Drawing, Vec<NodePath>)> {
    let (parent, index) = split(path)
        .filter(|(parent, _)| !parent.is_empty())
        .ok_or(EditError::Nothing)?;
    let group = drawing.node_at(path).ok_or(EditError::Nothing)?;
    let Kind::Group { children, clip } = group.kind() else {
        return Err(EditError::Nothing);
    };
    let mut released = Vec::new();
    for child in children.iter() {
        let mut node = child
            .with_transform(child.transform().then(group.transform()))?
            .with_opacity(child.opacity() * group.opacity())?;
        if let Some(clip) = clip {
            // Each member keeps the clip it was seen through, in a group of
            // its own holding a copy of its own.
            let placed_clip = clip.with_transform(clip.transform().then(group.transform()))?;
            let seen_through = renumbered(&placed_clip, ids)?;
            node = Node::new(
                ids.issue(),
                Kind::Group {
                    children: Children::single(try_arc(node)?)?,
                    clip: Some(try_arc(seen_through)?),
                },
            )?;
        }
        if !fallible::push(&mut released, try_arc(node)?) {
            return Err(DrawingError::OutOfMemory.into());
        }
    }
    let siblings = children_at(drawing, parent).ok_or(EditError::Nothing)?;
    let placed = paths_in(parent, index..index + released.len())?;
    let opened = siblings
        .without(&[index])?
        .with_inserted(released.into_iter().map(|node| (index, node)))?;
    Ok((with_children(drawing, parent, opened)?, placed))
}

/// What the object at `path` traces, in the drawing's space, and the rule it
/// fills by; text as its glyphs, a group as everything within it.
///
/// # Errors
///
/// [`EditError::Nothing`] for an object that traces nothing.
pub fn traced(drawing: &Drawing, path: &[usize], glyphs: &dyn Glyphs) -> Result<(Path, FillRule)> {
    let node = drawing.node_at(path).ok_or(EditError::Nothing)?;
    let traced = outline_of(node, glyphs)?;
    Ok((traced.0.transformed(parent_space(drawing, path))?, traced.1))
}

/// The outlines `text`'s glyphs set, in its own space.
///
/// # Errors
///
/// [`EditError::Unlettered`] where a glyph of it could not be had.
fn glyph_outlines(text: &tairix_drawing::Text, glyphs: &dyn Glyphs) -> Result<Path> {
    let setting = text.set(glyphs)?;
    // Traced without them, the text would lose its characters for good.
    if !setting.complete {
        return Err(EditError::Unlettered);
    }
    Ok(setting.outline(|_| true)?)
}

/// `node`'s outline in its parent's space and its rule.
fn outline_of(node: &Node, glyphs: &dyn Glyphs) -> Result<(Path, FillRule)> {
    let (local, rule) = match node.kind() {
        Kind::Shape { shape, style } => (shape.to_path()?, style.fill_rule),
        Kind::Text { text, .. } => (glyph_outlines(text, glyphs)?, FillRule::NonZero),
        // A picture takes part by its frame, as it clips by it.
        Kind::Image { image } => (image.outline()?, FillRule::NonZero),
        Kind::Group { children, .. } => {
            let mut union = Path::new();
            for child in children.iter() {
                let (part, rule) = outline_of(child, glyphs)?;
                union = boolean(&union, FillRule::NonZero, &part, rule, Operation::Union)?;
            }
            (union, FillRule::NonZero)
        }
    };
    Ok((local.transformed(node.transform())?, rule))
}

/// The objects at `paths` combined by `operation` into one path where the
/// lowest of them stood, painted as it was: union, intersection, exclusion,
/// or the lowest less every other.
///
/// # Errors
///
/// [`EditError::Nothing`] for fewer than two objects, and as the boolean
/// operation refuses its geometry.
pub fn combine(
    drawing: &Drawing,
    paths: &[NodePath],
    operation: Operation,
    glyphs: &dyn Glyphs,
    ids: &mut Ids,
) -> Result<(Drawing, NodePath)> {
    if paths.len() < 2 {
        return Err(EditError::Nothing);
    }
    let mut ordered = paths.to_vec();
    ordered.sort_unstable();
    let lowest = ordered[0].clone();
    let (mut result, mut rule) = traced(drawing, &lowest, glyphs)?;
    for path in &ordered[1..] {
        let (other, other_rule) = traced(drawing, path, glyphs)?;
        result = boolean(&result, rule, &other, other_rule, operation)?;
        rule = FillRule::NonZero;
    }
    let style = match drawing.node_at(&lowest).and_then(|node| node.style()) {
        Some(style) => style.clone(),
        None => try_arc(Style::default())?,
    };
    let back = parent_space(drawing, &lowest)
        .invert()
        .ok_or(DrawingError::Invalid)?;
    let shape = PathShape::from_path(&result.transformed(back)?)?;
    let node = Node::new(
        ids.issue(),
        Kind::Shape {
            shape: Shape::Path(try_arc(shape)?),
            style,
        },
    )?;
    let drawing = drawing.with_node_at(&lowest, try_arc(node)?)?;
    // Every other path sorts after the lowest, so taking those objects out
    // leaves the lowest where it was.
    let drawing = remove(&drawing, &ordered[1..])?;
    Ok((drawing, lowest))
}

/// Copies of the objects at `paths`, which must share a parent, each moved
/// by `offset` in the drawing's space and put just above its original,
/// answering where the copies are.
///
/// # Errors
///
/// [`EditError::Apart`] for objects in different parents.
pub fn duplicate(
    drawing: &Drawing,
    paths: &[NodePath],
    offset: Point,
    ids: &mut Ids,
) -> Result<(Drawing, Vec<NodePath>)> {
    let parent = common_parent(paths)?;
    let children = children_at(drawing, parent).ok_or(EditError::Nothing)?;
    let indices = indices_of(paths)?;
    let outer = parent_space(drawing, &paths[0]);
    let back = outer.invert().ok_or(DrawingError::Invalid)?;
    let shift = outer.then(Affine::translate(offset.x, offset.y)).then(back);
    let mut copies = Vec::new();
    if !fallible::reserve(&mut copies, indices.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    for &index in &indices {
        let node = children.get(index).ok_or(EditError::Nothing)?;
        let copy = renumbered(node, ids)?;
        let moved = copy.with_transform(copy.transform().then(shift))?;
        copies.push((index + 1, try_arc(moved)?));
    }
    let doubled = children.with_inserted(copies)?;
    // Every original below another has put its copy beneath it.
    let placed = paths_in(
        parent,
        indices
            .iter()
            .enumerate()
            .map(|(below, &index)| index + below + 1),
    )?;
    Ok((with_children(drawing, parent, doubled)?, placed))
}

/// `node` and everything within it given fresh identities.
///
/// # Errors
///
/// As the drawing refuses the renumbered group.
pub(crate) fn renumbered(node: &Node, ids: &mut Ids) -> Result<Node> {
    let node = node.with_id(ids.issue());
    match node.kind() {
        Kind::Group { children, clip } => {
            let mut nodes = children.to_vec()?;
            for child in &mut nodes {
                *child = try_arc(renumbered(child, ids)?)?;
            }
            let clip = match clip {
                Some(clip) => Some(try_arc(renumbered(clip, ids)?)?),
                None => None,
            };
            Ok(node.with_kind(Kind::Group {
                children: Children::from_nodes(nodes)?,
                clip,
            })?)
        }
        _ => Ok(node),
    }
}

/// Which way objects are lined up.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Align {
    /// Their left edges.
    Left,
    /// Their centres, across.
    Centre,
    /// Their right edges.
    Right,
    /// Their top edges.
    Top,
    /// Their middles, down.
    Middle,
    /// Their bottom edges.
    Bottom,
}

/// The objects at `paths` lined up as `align` says against `to`, the
/// bounds they are aligned within: their own, or the page's, each framed as
/// [`bounds_of`] frames it with `glyphs`.
///
/// # Errors
///
/// As the drawing refuses a transform.
pub fn align(
    drawing: &Drawing,
    paths: &[NodePath],
    align: Align,
    to: Bounds,
    glyphs: &dyn Glyphs,
) -> Result<Drawing> {
    let mut moves = Vec::new();
    if !fallible::reserve(&mut moves, paths.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    for path in paths {
        let bounds = bounds_of(drawing, core::slice::from_ref(path), glyphs);
        if bounds.is_empty() {
            continue;
        }
        let by = match align {
            Align::Left => Point::new(to.min.x - bounds.min.x, 0.0),
            Align::Centre => Point::new(to.centre().x - bounds.centre().x, 0.0),
            Align::Right => Point::new(to.max.x - bounds.max.x, 0.0),
            Align::Top => Point::new(0.0, to.min.y - bounds.min.y),
            Align::Middle => Point::new(0.0, to.centre().y - bounds.centre().y),
            Align::Bottom => Point::new(0.0, to.max.y - bounds.max.y),
        };
        if by != Point::ZERO {
            moves.push((path.as_slice(), by));
        }
    }
    moved_by(drawing, paths, moves)
}

/// The objects at `paths` spread so the gaps between them are equal,
/// across or `down`, the outermost two left where they are, each framed as
/// [`bounds_of`] frames it with `glyphs`.
///
/// # Errors
///
/// [`EditError::Nothing`] for fewer than three objects.
pub fn distribute(
    drawing: &Drawing,
    paths: &[NodePath],
    down: bool,
    glyphs: &dyn Glyphs,
) -> Result<Drawing> {
    if paths.len() < 3 {
        return Err(EditError::Nothing);
    }
    let along = |bounds: Bounds| {
        if down {
            (bounds.min.y, bounds.max.y)
        } else {
            (bounds.min.x, bounds.max.x)
        }
    };
    let mut placed = Vec::new();
    if !fallible::reserve(&mut placed, paths.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    placed.extend(
        paths
            .iter()
            .map(|path| {
                let bounds = bounds_of(drawing, core::slice::from_ref(path), glyphs);
                (path.as_slice(), bounds)
            })
            .filter(|(_, bounds)| !bounds.is_empty()),
    );
    placed.sort_by(|a, b| along(a.1).0.total_cmp(&along(b.1).0));
    let (Some(first), Some(last)) = (placed.first(), placed.last()) else {
        return Err(EditError::Nothing);
    };
    let span = along(last.1).1 - along(first.1).0;
    let filled: f64 = placed
        .iter()
        .map(|(_, bounds)| along(*bounds).1 - along(*bounds).0)
        .sum();
    let gaps = f64::from(u32::try_from(placed.len() - 1).unwrap_or(u32::MAX));
    let gap = (span - filled) / gaps;
    let mut cursor = along(first.1).0;
    let mut moves = Vec::new();
    if !fallible::reserve(&mut moves, placed.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    for &(path, bounds) in &placed {
        let (start, end) = along(bounds);
        let by = cursor - start;
        if by != 0.0 {
            moves.push((
                path,
                if down {
                    Point::new(0.0, by)
                } else {
                    Point::new(by, 0.0)
                },
            ));
        }
        cursor += end - start + gap;
    }
    moved_by(drawing, paths, moves)
}

/// Every object at `paths` mirrored about the centre of their bounds, as
/// [`bounds_of`] frames them with `glyphs`, across or `down`.
///
/// # Errors
///
/// As the drawing refuses a transform.
pub fn flip(
    drawing: &Drawing,
    paths: &[NodePath],
    down: bool,
    glyphs: &dyn Glyphs,
) -> Result<Drawing> {
    let centre = bounds_of(drawing, paths, glyphs).centre();
    let (sx, sy) = if down { (1.0, -1.0) } else { (-1.0, 1.0) };
    let map = Affine::translate(-centre.x, -centre.y)
        .then(Affine::scale(sx, sy))
        .then(Affine::translate(centre.x, centre.y));
    transform(drawing, paths, map)
}

/// Every shape at `paths` that is not a path made the path it traces, and
/// every text the outlines of its glyphs, each painted as it was: a text
/// whose runs paint it otherwise a group of one path for each way its glyphs
/// are painted, their identities issued from `ids`.
///
/// # Errors
///
/// As the drawing refuses the paths.
pub fn to_paths(
    drawing: &Drawing,
    paths: &[NodePath],
    glyphs: &dyn Glyphs,
    ids: &mut Ids,
) -> Result<Drawing> {
    each(drawing, paths, |_, _, node| {
        let (path, style) = match node.kind() {
            Kind::Shape {
                shape: Shape::Path(_),
                ..
            }
            | Kind::Group { .. }
            | Kind::Image { .. } => return Ok(None),
            Kind::Shape { shape, style } => (shape.to_path()?, style.clone()),
            Kind::Text { text, style } if text.repaints() => {
                return painted_outlines(node, text, style, glyphs, ids).map(Some);
            }
            Kind::Text { text, style } => (glyph_outlines(text, glyphs)?, style.clone()),
        };
        let traced = node.with_kind(Kind::Shape {
            shape: Shape::Path(try_arc(PathShape::from_path(&path)?)?),
            style,
        })?;
        Ok(Some(traced))
    })
}

/// `node`, a text whose runs paint its glyphs otherwise than `style`, as a
/// group in its place of one path for each paint its glyphs are set in.
fn painted_outlines(
    node: &Node,
    text: &Text,
    style: &Style,
    glyphs: &dyn Glyphs,
    ids: &mut Ids,
) -> Result<Node> {
    let setting = text.set(glyphs)?;
    if !setting.complete {
        return Err(EditError::Unlettered);
    }
    let mut paints: Vec<Style> = Vec::new();
    let mut outlines: Vec<Path> = Vec::new();
    for part in setting.painted(text) {
        let painted = part.style(style);
        let outline = setting.outline_of(part.glyphs, |_| true)?;
        match paints.iter().position(|seen| *seen == painted) {
            Some(at) => {
                let held = &mut outlines[at].subpaths;
                held.try_reserve(outline.subpaths.len())?;
                held.extend(outline.subpaths);
            }
            None => {
                if !fallible::push(&mut paints, painted) || !fallible::push(&mut outlines, outline)
                {
                    return Err(DrawingError::OutOfMemory.into());
                }
            }
        }
    }
    let mut parts = Vec::new();
    if !fallible::reserve(&mut parts, paints.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    for (painted, outline) in paints.into_iter().zip(outlines) {
        let shape = Shape::Path(try_arc(PathShape::from_path(&outline)?)?);
        let part = Node::new(
            ids.issue(),
            Kind::Shape {
                shape,
                style: try_arc(painted)?,
            },
        )?;
        parts.push(try_arc(part)?);
    }
    Ok(node.with_kind(Kind::Group {
        children: Children::from_nodes(parts)?,
        clip: None,
    })?)
}

/// The one text and one shape at `paths`, in either order, made one object:
/// the text set along the shape's outline — brought into the text's own
/// space, every line feed of it a space — in the text's place, the shape
/// taken away; answers where the text stands.
///
/// # Errors
///
/// [`EditError::Nothing`] unless `paths` names one text and one shape and
/// the text's space can be mapped back from, and as the text refuses the
/// path.
pub fn set_on_path(drawing: &Drawing, paths: &[NodePath]) -> Result<(Drawing, NodePath)> {
    let [first, second] = paths else {
        return Err(EditError::Nothing);
    };
    let is_text = |path: &NodePath| {
        drawing
            .node_at(path)
            .is_some_and(|node| matches!(node.kind(), Kind::Text { .. }))
    };
    let (text_at, shape_at) = if is_text(first) {
        (first, second)
    } else {
        (second, first)
    };
    let text_node = drawing.node_at(text_at).ok_or(EditError::Nothing)?;
    let shape_node = drawing.node_at(shape_at).ok_or(EditError::Nothing)?;
    let (Kind::Text { text, style }, Kind::Shape { shape, .. }) =
        (text_node.kind(), shape_node.kind())
    else {
        return Err(EditError::Nothing);
    };
    let to_text = text_node
        .transform()
        .then(parent_space(drawing, text_at))
        .invert()
        .ok_or(EditError::Nothing)?;
    let to_drawing = shape_node.transform().then(parent_space(drawing, shape_at));
    let outline = shape.to_path()?.transformed(to_drawing.then(to_text))?;
    let mut content = alloc::string::String::new();
    content.try_reserve(text.content.len())?;
    content.extend(
        text.content
            .chars()
            .map(|c| if c == '\n' { ' ' } else { c }),
    );
    let along = Text {
        content: SharedStr::try_from_string(content)?,
        baseline: Baseline::Path(try_arc(TextPath {
            path: try_arc(PathShape::from_path(&outline)?)?,
            start: 0.0,
        })?),
        ..(**text).clone()
    };
    let set = text_node.with_kind(Kind::Text {
        text: try_arc(along)?,
        style: style.clone(),
    })?;
    let drawing = drawing.with_node_at(text_at, try_arc(set)?)?;
    let drawing = remove(&drawing, core::slice::from_ref(shape_at))?;
    Ok((drawing, after_removal(text_at, shape_at)?))
}

/// Every text at `paths` set along a path set in lines again, from where its
/// path began, and the path given back after it as an object of its own in
/// `style`, its identity issued from `ids`; answers where each text and each
/// path stands.
///
/// # Errors
///
/// [`EditError::Nothing`] where `paths` names no text set along a path, and
/// as the drawing refuses what the edit makes.
pub fn release_from_path(
    drawing: &Drawing,
    paths: &[NodePath],
    style: &Style,
    ids: &mut Ids,
) -> Result<(Drawing, Vec<NodePath>)> {
    let mut along = Vec::new();
    if !fallible::reserve(&mut along, paths.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    along.extend(paths.iter().filter(|path| {
        drawing.node_at(path).is_some_and(|node| {
            matches!(node.kind(), Kind::Text { text, .. } if matches!(text.baseline, Baseline::Path(_)))
        })
    }));
    if along.is_empty() {
        return Err(EditError::Nothing);
    }
    // Last first, so giving a path back never moves a text still to come.
    along.sort_unstable_by(|a, b| b.cmp(a));
    let mut drawing = drawing.clone();
    let mut released = Vec::new();
    if !fallible::reserve(&mut released, 2 * along.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    for at in along {
        let node = drawing.node_at(at).ok_or(EditError::Nothing)?;
        let Kind::Text {
            text,
            style: lettering,
        } = node.kind()
        else {
            return Err(EditError::Nothing);
        };
        let Baseline::Path(path) = &text.baseline else {
            return Err(EditError::Nothing);
        };
        let lines = Text {
            baseline: Baseline::Lines {
                origin: path.first_point(),
                line_height: DEFAULT_LINE_HEIGHT,
            },
            ..(**text).clone()
        };
        let given = Node::new(
            ids.issue(),
            Kind::Shape {
                shape: Shape::Path(path.path.clone()),
                style: try_arc(style.clone())?,
            },
        )?
        .with_transform(node.transform())?;
        let set = node.with_kind(Kind::Text {
            text: try_arc(lines)?,
            style: lettering.clone(),
        })?;
        released.push(set.id());
        released.push(given.id());
        drawing = drawing.with_node_at(at, try_arc(set)?)?;
        drawing = inserted_after(&drawing, at, try_arc(given)?)?;
    }
    let found = drawing.find_each(&released)?;
    let mut placed = Vec::new();
    if !fallible::reserve(&mut placed, found.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    placed.extend(found.into_iter().flatten());
    Ok((drawing, placed))
}

/// `node` put just after the object at `path`, in its parent.
fn inserted_after(drawing: &Drawing, path: &[usize], node: Arc<Node>) -> Result<Drawing> {
    admit(drawing, core::slice::from_ref(&node))?;
    let (parent, index) = split(path).ok_or(EditError::Nothing)?;
    if parent.is_empty() {
        return Err(EditError::Nothing);
    }
    let children = children_at(drawing, parent).ok_or(EditError::Nothing)?;
    with_children(drawing, parent, children.inserted(index + 1, node)?)
}

/// Where the object at `path` stands once the one at `removed` is taken
/// away: one sooner wherever the removed one came before it among the
/// children of one of its ancestors.
fn after_removal(path: &[usize], removed: &[usize]) -> Result<NodePath> {
    let mut moved = Vec::new();
    if !fallible::reserve(&mut moved, path.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    moved.extend_from_slice(path);
    if let Some((parent, index)) = split(removed) {
        let depth = parent.len();
        if path.len() > depth && path[..depth] == *parent && path[depth] > index {
            moved[depth] -= 1;
        }
    }
    Ok(moved)
}

/// Every stroked shape at `paths` made the outline its stroke paints,
/// filled with what the stroke painted.
///
/// # Errors
///
/// As the outline refuses its geometry.
pub fn outline_strokes(drawing: &Drawing, paths: &[NodePath]) -> Result<Drawing> {
    each(drawing, paths, |_, _, node| {
        let Kind::Shape { shape, style } = node.kind() else {
            return Ok(None);
        };
        if !style.stroke.paints() {
            return Ok(None);
        }
        let area = stroke_area(&shape.to_path()?, &style.stroke)?;
        let filled = Style {
            fill: style.stroke.paint.clone(),
            fill_rule: FillRule::NonZero,
            stroke: tairix_drawing::Stroke {
                paint: Paint::None,
                ..tairix_drawing::Stroke::default()
            },
            paint_order: style.paint_order,
        };
        let outlined = node.with_kind(Kind::Shape {
            shape: Shape::Path(try_arc(PathShape::from_path(&area)?)?),
            style: try_arc(filled)?,
        })?;
        Ok(Some(outlined))
    })
}

/// Every path at `paths` run the other way, so what follows its direction —
/// its arrowheads, its dashes — follows it round; other objects are left as
/// they are.
///
/// # Errors
///
/// As the drawing refuses the paths.
pub fn reverse(drawing: &Drawing, paths: &[NodePath]) -> Result<Drawing> {
    each(drawing, paths, |_, _, node| {
        let Kind::Shape {
            shape: Shape::Path(path),
            style,
        } = node.kind()
        else {
            return Ok(None);
        };
        let reversed = node.with_kind(Kind::Shape {
            shape: Shape::Path(try_arc(path.reversed()?)?),
            style: style.clone(),
        })?;
        Ok(Some(reversed))
    })
}

/// How far the map from `node`'s own space, at `path`, onto the drawing's
/// stretches a length, taken across both axes.
fn stretch(drawing: &Drawing, path: &[usize], node: &Node) -> f64 {
    let map = node.transform().then(parent_space(drawing, path));
    sqrt(fabs(map.a * map.d - map.b * map.c))
}

/// The outline of the shape or text `node` in its own space, the rule it
/// fills by and its style; nothing for a group or a picture.
fn own_outline(node: &Node, glyphs: &dyn Glyphs) -> Result<Option<(Path, FillRule, Arc<Style>)>> {
    Ok(match node.kind() {
        Kind::Group { .. } | Kind::Image { .. } => None,
        Kind::Shape { shape, style } => Some((shape.to_path()?, style.fill_rule, style.clone())),
        Kind::Text { text, style } => Some((
            glyph_outlines(text, glyphs)?,
            FillRule::NonZero,
            style.clone(),
        )),
    })
}

/// How near, in the drawing's units, a traced or grown outline keeps to the
/// one it follows.
const FIDELITY: f64 = 0.01;

/// Every shape and text at `paths` grown by `distance` in the drawing's
/// units, shrunk where it is negative, its corners rounded: the path what
/// it fills becomes, painted as it was.
///
/// # Errors
///
/// [`EditError::Vanishes`] for an inset deeper than an object, and as the
/// geometry refuses.
pub fn offset(
    drawing: &Drawing,
    paths: &[NodePath],
    distance: f64,
    glyphs: &dyn Glyphs,
) -> Result<Drawing> {
    each(drawing, paths, |drawing, path, node| {
        let Some((outline, rule, style)) = own_outline(node, glyphs)? else {
            return Ok(None);
        };
        let stretch = fmax(stretch(drawing, path, node), f64::MIN_POSITIVE);
        let area = grown(
            &outline,
            rule,
            distance / stretch,
            Join::Round,
            4.0,
            FIDELITY / stretch,
        )?;
        if area.subpaths.is_empty() {
            return Err(EditError::Vanishes);
        }
        let offset = node.with_kind(Kind::Shape {
            shape: Shape::Path(try_arc(PathShape::from_path(&area)?)?),
            style,
        })?;
        Ok(Some(offset))
    })
}

/// Every shape and text at `paths` traced again with as few nodes as keep it
/// within `tolerance` of itself, in the drawing's units, its corners kept: a
/// path painted as it was. A path the trace would not make simpler is left
/// as it is.
///
/// # Errors
///
/// As the geometry refuses.
pub fn simplify(
    drawing: &Drawing,
    paths: &[NodePath],
    tolerance: f64,
    glyphs: &dyn Glyphs,
) -> Result<Drawing> {
    each(drawing, paths, |drawing, path, node| {
        let Some((outline, _, style)) = own_outline(node, glyphs)? else {
            return Ok(None);
        };
        let tolerance = tolerance / fmax(stretch(drawing, path, node), f64::MIN_POSITIVE);
        let traced = PathShape::from_path(&refitted(&outline, tolerance)?)?;
        let simpler = match node.kind() {
            Kind::Shape {
                shape: Shape::Path(was),
                ..
            } => traced.node_count() < was.node_count(),
            _ => true,
        };
        if !simpler {
            return Ok(None);
        }
        let simplified = node.with_kind(Kind::Shape {
            shape: Shape::Path(try_arc(traced)?),
            style,
        })?;
        Ok(Some(simplified))
    })
}

/// `outline` with each sub-path flattened finely and fitted again within
/// `tolerance`, its corners kept.
fn refitted(outline: &Path, tolerance: f64) -> Result<Path> {
    let mut traced = Path::new();
    if traced.subpaths.try_reserve(outline.subpaths.len()).is_err() {
        return Err(DrawingError::OutOfMemory.into());
    }
    let mut points = Vec::new();
    for subpath in &outline.subpaths {
        points.clear();
        subpath.flatten(tolerance / 4.0, &mut points)?;
        if subpath.closed && !fallible::push(&mut points, subpath.start) {
            return Err(DrawingError::OutOfMemory.into());
        }
        if let Some(mut fitted) = fitted(&points, tolerance, crate::tools::CORNER_TURN)? {
            fitted.closed = subpath.closed;
            traced.subpaths.push(fitted);
        }
    }
    Ok(traced)
}

/// The objects at `paths`, which must share a parent, each traced and cut
/// into the pieces their overlaps make, every piece a path painted as the
/// uppermost object over it was, in the lowest object's place, answering
/// where the pieces are.
///
/// # Errors
///
/// [`EditError::Nothing`] for fewer than two objects, and as the boolean
/// operations refuse their geometry.
pub fn divide(
    drawing: &Drawing,
    paths: &[NodePath],
    glyphs: &dyn Glyphs,
    ids: &mut Ids,
) -> Result<(Drawing, Vec<NodePath>)> {
    if paths.len() < 2 {
        return Err(EditError::Nothing);
    }
    let parent = common_parent(paths)?;
    let order = indices_of(paths)?;
    let Some(&lowest) = order.first() else {
        return Err(EditError::Nothing);
    };
    // Each piece so far, in the drawing's space, and the style it shows.
    let mut pieces: Vec<(Path, Arc<Style>)> = Vec::new();
    for &index in &order {
        let at = paths_in(parent, core::iter::once(index))?;
        let at = at.first().ok_or(EditError::Nothing)?;
        let (shape, rule) = traced(drawing, at, glyphs)?;
        let style = match drawing.node_at(at).and_then(|node| node.style()) {
            Some(style) => style.clone(),
            None => try_arc(Style::default())?,
        };
        let mut next = Vec::new();
        let mut rest = boolean(
            &shape,
            rule,
            &Path::new(),
            FillRule::NonZero,
            Operation::Union,
        )?;
        for (piece, kept) in pieces {
            let under = boolean(
                &piece,
                FillRule::NonZero,
                &shape,
                rule,
                Operation::Intersection,
            )?;
            let beside = boolean(
                &piece,
                FillRule::NonZero,
                &shape,
                rule,
                Operation::Difference,
            )?;
            rest = boolean(
                &rest,
                FillRule::NonZero,
                &piece,
                FillRule::NonZero,
                Operation::Difference,
            )?;
            push_regions(&mut next, &under, &style)?;
            push_regions(&mut next, &beside, &kept)?;
        }
        push_regions(&mut next, &rest, &style)?;
        pieces = next;
    }
    let back = parent_space(drawing, &paths[0])
        .invert()
        .ok_or(DrawingError::Invalid)?;
    let mut made = Vec::new();
    if !fallible::reserve(&mut made, pieces.len()) {
        return Err(DrawingError::OutOfMemory.into());
    }
    for (piece, style) in pieces {
        let shape = PathShape::from_path(&piece.transformed(back)?)?;
        let node = Node::new(
            ids.issue(),
            Kind::Shape {
                shape: Shape::Path(try_arc(shape)?),
                style,
            },
        )?;
        made.push(try_arc(node)?);
    }
    let children = children_at(drawing, parent).ok_or(EditError::Nothing)?;
    let count = made.len();
    let cut = children
        .without(&order)?
        .with_inserted(made.into_iter().map(|node| (lowest, node)))?;
    let drawing = with_children(drawing, parent, cut)?;
    Ok((drawing, paths_in(parent, lowest..lowest + count)?))
}

/// Each connected region of `area` onto `pieces`, shown in `style`.
fn push_regions(
    pieces: &mut Vec<(Path, Arc<Style>)>,
    area: &Path,
    style: &Arc<Style>,
) -> Result<()> {
    for region in components(area)? {
        if !fallible::push(pieces, (region, style.clone())) {
            return Err(DrawingError::OutOfMemory.into());
        }
    }
    Ok(())
}

/// The objects at `paths`, which must share a parent, gathered into a group
/// shown only within the uppermost of them, which becomes the group's clip,
/// where that one stood, answering the group's path.
///
/// # Errors
///
/// [`EditError::Nothing`] for fewer than two objects, and
/// [`EditError::Apart`] for objects in different parents.
pub fn clip(drawing: &Drawing, paths: &[NodePath], ids: &mut Ids) -> Result<(Drawing, NodePath)> {
    if paths.len() < 2 {
        return Err(EditError::Nothing);
    }
    let parent = common_parent(paths)?;
    let children = children_at(drawing, parent).ok_or(EditError::Nothing)?;
    let members = indices_of(paths)?;
    let (&top, beneath) = members.split_last().ok_or(EditError::Nothing)?;
    let clip = children.get(top).ok_or(EditError::Nothing)?.clone();
    let at = top + 1 - members.len();
    let group = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(taken(children, beneath)?)?,
            clip: Some(clip),
        },
    )?;
    let clipped = children.without(&members)?.inserted(at, try_arc(group)?)?;
    let drawing = with_children(drawing, parent, clipped)?;
    let mut path = paths_in(parent, core::iter::once(at))?;
    Ok((drawing, path.pop().ok_or(EditError::Nothing)?))
}

/// Every clipped group at `paths` shown whole again, its clip put back
/// among what it holds as the uppermost of them.
///
/// # Errors
///
/// As the drawing refuses the groups.
pub fn release_clip(drawing: &Drawing, paths: &[NodePath]) -> Result<Drawing> {
    each(drawing, paths, |_, _, node| {
        let Kind::Group {
            children,
            clip: Some(clip),
        } = node.kind()
        else {
            return Ok(None);
        };
        let released = node.with_kind(Kind::Group {
            children: children.appended(core::iter::once(clip.clone()))?,
            clip: None,
        })?;
        Ok(Some(released))
    })
}

/// A new, empty layer called `name` put above every other, answering its
/// path.
///
/// # Errors
///
/// As the drawing refuses another layer.
pub fn add_layer(drawing: &Drawing, name: &str, ids: &mut Ids) -> Result<(Drawing, NodePath)> {
    let layer = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::new(),
            clip: None,
        },
    )?
    .with_name(Some(SharedStr::try_from_str(name)?))?;
    insert(drawing, &[], try_arc(layer)?)
}

/// `drawing` without the layer at `index`, as long as another remains.
///
/// # Errors
///
/// [`EditError::Nothing`] for the last layer, or no layer there.
pub fn remove_layer(drawing: &Drawing, index: usize) -> Result<Drawing> {
    let layers = drawing.layers();
    if layers.len() < 2 || index >= layers.len() {
        return Err(EditError::Nothing);
    }
    Ok(drawing.with_layers(layers.removed(index)?)?)
}

/// The layer at `from` moved to stand at `to`.
///
/// # Errors
///
/// [`EditError::Nothing`] where either names no layer.
pub fn move_layer(drawing: &Drawing, from: usize, to: usize) -> Result<Drawing> {
    let layers = drawing.layers();
    let layer = layers.get(from).ok_or(EditError::Nothing)?.clone();
    if to >= layers.len() {
        return Err(EditError::Nothing);
    }
    let without = layers.removed(from)?;
    Ok(drawing.with_layers(without.inserted(to, layer)?)?)
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
