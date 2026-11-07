//! What the editor's commands do to a drawing.
//!
//! Every operation takes a drawing and answers a new one, sharing every node
//! it does not touch, so it is one step of history whatever it changes. An
//! object is named by its path from its layer; several objects named at once
//! are worked on in each one's own parent, a map given in the drawing's
//! space brought into that parent's before it is applied.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::boolean::{boolean, BooleanError, Operation};
use tairix_bezier::offset::{outline, StrokeStyle};
use tairix_bezier::{Affine, Bounds, FillRule, Path, Point};
use tairix_drawing::render::dashes;
use tairix_drawing::text::Glyphs;
use tairix_drawing::{
    Children, Drawing, DrawingError, Ids, Kind, Node, NodePath, Paint, PathShape, Shape, Style,
};
use tairix_util::fallible;

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
}

impl From<DrawingError> for EditError {
    fn from(error: DrawingError) -> Self {
        Self::Drawing(error)
    }
}

impl From<BooleanError> for EditError {
    fn from(error: BooleanError) -> Self {
        Self::Drawing(match error {
            BooleanError::TooComplex => DrawingError::TooComplex,
            BooleanError::OutOfMemory => DrawingError::OutOfMemory,
        })
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
    Ok(drawing.with_node_at(parent, Arc::new(rebuilt))?)
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
            |path| path.transformed(map).bounds(),
        ),
        Kind::Text { text, .. } => text_bounds(text, glyphs, complete).transformed(map),
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
/// [`EditError::Nothing`] where `parent` names no group.
pub fn insert(drawing: &Drawing, parent: &[usize], node: Arc<Node>) -> Result<(Drawing, NodePath)> {
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
/// [`EditError::Nothing`] where `parent` names no group within a layer.
pub fn append(
    drawing: &Drawing,
    parent: &[usize],
    nodes: Vec<Arc<Node>>,
) -> Result<(Drawing, Vec<NodePath>)> {
    if parent.is_empty() {
        return Err(EditError::Nothing);
    }
    let children = children_at(drawing, parent).ok_or(EditError::Nothing)?;
    let first = children.len();
    let count = nodes.len();
    let drawing = with_children(drawing, parent, children.appended(nodes)?)?;
    Ok((drawing, paths_in(parent, first..first + count)?))
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
                if !fallible::push(&mut changes, (index, Arc::new(changed))) {
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

/// Every object at `paths` restyled by `restyle`; a group's objects within
/// it, every one.
///
/// # Errors
///
/// As the drawing refuses a style.
pub fn restyle(
    drawing: &Drawing,
    paths: &[NodePath],
    restyle: &dyn Fn(&Style) -> Style,
) -> Result<Drawing> {
    each(drawing, paths, |_, _, node| restyled(node, restyle))
}

/// `node` restyled by `restyle`, or nothing where it changes no style.
fn restyled(node: &Node, restyle: &dyn Fn(&Style) -> Style) -> Result<Option<Node>> {
    let kind = match node.kind() {
        Kind::Shape { shape, style } => {
            let Some(style) = changed_style(style, restyle) else {
                return Ok(None);
            };
            Kind::Shape {
                shape: shape.clone(),
                style,
            }
        }
        Kind::Text { text, style } => {
            let Some(style) = changed_style(style, restyle) else {
                return Ok(None);
            };
            Kind::Text {
                text: text.clone(),
                style,
            }
        }
        Kind::Group { children, clip } => {
            let mut changed = Vec::new();
            for (index, child) in children.iter().enumerate() {
                if let Some(child) = restyled(child, restyle)? {
                    if !fallible::push(&mut changed, (index, Arc::new(child))) {
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
fn changed_style(style: &Arc<Style>, restyle: &dyn Fn(&Style) -> Style) -> Option<Arc<Style>> {
    let restyled = restyle(style);
    (restyled != **style).then(|| Arc::new(restyled))
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
    let regrouped = children.without(&members)?.inserted(at, Arc::new(group))?;
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
                    children: Children::from_nodes(alloc::vec![Arc::new(node)])?,
                    clip: Some(Arc::new(seen_through)),
                },
            )?;
        }
        if !fallible::push(&mut released, Arc::new(node)) {
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
    Ok((traced.0.transformed(parent_space(drawing, path)), traced.1))
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
    let mut path = Path::new();
    for placed in setting.glyphs {
        let set = placed.glyph.outline().transformed(placed.at);
        if path.subpaths.try_reserve(set.subpaths.len()).is_err() {
            return Err(DrawingError::OutOfMemory.into());
        }
        path.subpaths.extend(set.subpaths);
    }
    Ok(path)
}

/// `node`'s outline in its parent's space and its rule.
fn outline_of(node: &Node, glyphs: &dyn Glyphs) -> Result<(Path, FillRule)> {
    let (local, rule) = match node.kind() {
        Kind::Shape { shape, style } => (shape.to_path()?, style.fill_rule),
        Kind::Text { text, .. } => (glyph_outlines(text, glyphs)?, FillRule::NonZero),
        Kind::Group { children, .. } => {
            let mut union = Path::new();
            for child in children.iter() {
                let (part, rule) = outline_of(child, glyphs)?;
                union = boolean(&union, FillRule::NonZero, &part, rule, Operation::Union)?;
            }
            (union, FillRule::NonZero)
        }
    };
    Ok((local.transformed(node.transform()), rule))
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
    let style = drawing
        .node_at(&lowest)
        .and_then(|node| node.style().cloned())
        .unwrap_or_else(|| Arc::new(Style::default()));
    let back = parent_space(drawing, &lowest)
        .invert()
        .ok_or(DrawingError::Invalid)?;
    let shape = PathShape::from_path(&result.transformed(back))?;
    let node = Node::new(
        ids.issue(),
        Kind::Shape {
            shape: Shape::Path(Arc::new(shape)),
            style,
        },
    )?;
    let drawing = drawing.with_node_at(&lowest, Arc::new(node))?;
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
        copies.push((index + 1, Arc::new(moved)));
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
                *child = Arc::new(renumbered(child, ids)?);
            }
            let clip = match clip {
                Some(clip) => Some(Arc::new(renumbered(clip, ids)?)),
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
/// every text the outlines of its glyphs, each painted as it was.
///
/// # Errors
///
/// As the drawing refuses the paths.
pub fn to_paths(drawing: &Drawing, paths: &[NodePath], glyphs: &dyn Glyphs) -> Result<Drawing> {
    each(drawing, paths, |_, _, node| {
        let (path, style) = match node.kind() {
            Kind::Shape {
                shape: Shape::Path(_),
                ..
            }
            | Kind::Group { .. } => return Ok(None),
            Kind::Shape { shape, style } => (shape.to_path()?, style.clone()),
            Kind::Text { text, style } => (glyph_outlines(text, glyphs)?, style.clone()),
        };
        let traced = node.with_kind(Kind::Shape {
            shape: Shape::Path(Arc::new(PathShape::from_path(&path)?)),
            style,
        })?;
        Ok(Some(traced))
    })
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
        let dashed = dashes(&shape.to_path()?, &style.stroke)?;
        let pen = StrokeStyle {
            width: style.stroke.width,
            join: style.stroke.join,
            miter_limit: style.stroke.miter_limit,
            start_cap: style.stroke.cap,
            end_cap: style.stroke.cap,
        };
        let area = outline(&dashed, &pen, 0.01)?;
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
            shape: Shape::Path(Arc::new(PathShape::from_path(&area)?)),
            style: Arc::new(filled),
        })?;
        Ok(Some(outlined))
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
    .with_name(Some(Arc::from(name)))?;
    insert(drawing, &[], Arc::new(layer))
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
