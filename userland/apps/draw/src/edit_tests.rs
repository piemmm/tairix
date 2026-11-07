//! Unit tests for the editor's operations.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_bezier::boolean::Operation;
use tairix_bezier::{Affine, Bounds, Point};
use tairix_colour::Rgba;
use tairix_drawing::shape::RectShape;
use tairix_drawing::text::Glyph;
use tairix_drawing::{
    Children, Drawing, Glyphs, Grid, Ids, Kind, Node, Page, Paint, Shape, Style, Unit,
};

use super::{
    add_layer, align, append, arrange, bounds_of, combine, distribute, duplicate, flip, framed,
    group, insert, remove, remove_layer, restyle, transform, ungroup, Align, Arrange, EditError,
};

struct NoGlyphs;

impl Glyphs for NoGlyphs {
    fn glyph(&self, _family: &str, _weight: u16, _italic: bool, _c: char) -> Option<Arc<Glyph>> {
        None
    }
}

fn rect(ids: &mut Ids, x: f64, width: f64) -> Arc<Node> {
    Arc::new(
        Node::new(
            ids.issue(),
            Kind::Shape {
                shape: Shape::Rect(RectShape {
                    x,
                    y: 0.0,
                    width,
                    height: 10.0,
                    rx: 0.0,
                    ry: 0.0,
                }),
                style: Arc::new(Style::default()),
            },
        )
        .expect("a rect"),
    )
}

/// A drawing of one layer holding rectangles at `xs`, each 10 wide.
fn drawing(ids: &mut Ids, xs: &[f64]) -> Drawing {
    let shapes: Vec<Arc<Node>> = xs.iter().map(|&x| rect(ids, x, 10.0)).collect();
    let layer = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(shapes).expect("held"),
            clip: None,
        },
    )
    .expect("a layer");
    Drawing::new(
        Page {
            width: 200.0,
            height: 100.0,
            unit: Unit::Px,
            background: None,
        },
        Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        Arc::from(&[][..]),
        Grid::default(),
    )
    .expect("a drawing")
}

fn xs(drawing: &Drawing) -> Vec<f64> {
    let layer = drawing.layers().get(0).expect("a layer");
    layer
        .children()
        .expect("a group")
        .iter()
        .map(|node| node.bounds().min.x)
        .collect()
}

#[test]
fn inserting_and_removing_keep_the_rest_where_it_was() {
    let mut ids = Ids::new();
    let drawing = drawing(&mut ids, &[0.0, 20.0]);
    let (drawing, path) = insert(&drawing, &[0], rect(&mut ids, 40.0, 10.0)).expect("put");
    assert_eq!(path, vec![0, 2]);
    assert_eq!(xs(&drawing), vec![0.0, 20.0, 40.0]);
    let drawing = remove(&drawing, &[vec![0, 0], vec![0, 2]]).expect("taken out");
    assert_eq!(xs(&drawing), vec![20.0]);
    assert_eq!(
        remove(&drawing, &[vec![0]]).err(),
        Some(EditError::Nothing),
        "a layer is not an object"
    );
}

#[test]
fn a_move_given_in_the_drawing_is_made_in_each_parent() {
    let mut ids = Ids::new();
    let drawing = drawing(&mut ids, &[0.0]);
    // The layer itself is scaled, so its child moves half as far in its own
    // space to move as far on the page.
    let layer = drawing
        .layers()
        .get(0)
        .expect("layer")
        .with_transform(Affine::scale(2.0, 2.0))
        .expect("scaled");
    let drawing = drawing.with_node_at(&[0], Arc::new(layer)).expect("placed");
    let moved = transform(&drawing, &[vec![0, 0]], Affine::translate(10.0, 0.0)).expect("moved");
    let bounds = bounds_of(&moved, &[vec![0, 0]], &NoGlyphs);
    assert!((bounds.min.x - 10.0).abs() < 1e-9, "{bounds:?}");
}

#[test]
fn objects_are_raised_lowered_and_sent_to_either_end() {
    let mut ids = Ids::new();
    let drawing = drawing(&mut ids, &[0.0, 20.0, 40.0, 60.0]);
    let (raised, at) = arrange(&drawing, &[vec![0, 1]], Arrange::Raise).expect("raised");
    assert_eq!(xs(&raised), vec![0.0, 40.0, 20.0, 60.0]);
    assert_eq!(at, vec![vec![0, 2]]);
    let (top, _) = arrange(&drawing, &[vec![0, 0], vec![0, 1]], Arrange::Top).expect("to the top");
    assert_eq!(xs(&top), vec![40.0, 60.0, 0.0, 20.0]);
    let (bottom, _) = arrange(&drawing, &[vec![0, 3]], Arrange::Bottom).expect("to the bottom");
    assert_eq!(xs(&bottom), vec![60.0, 0.0, 20.0, 40.0]);
    let (lowered, _) = arrange(&drawing, &[vec![0, 0]], Arrange::Lower).expect("as low as it goes");
    assert_eq!(xs(&lowered), xs(&drawing));
}

#[test]
fn grouping_and_ungrouping_return_what_was_there() {
    let mut ids = Ids::new();
    let drawing = drawing(&mut ids, &[0.0, 20.0, 40.0]);
    let (grouped, path) = group(&drawing, &[vec![0, 0], vec![0, 2]], &mut ids).expect("grouped");
    assert_eq!(
        path,
        vec![0, 1],
        "the group stands where the topmost was, less those taken"
    );
    let group_node = grouped.node_at(&path).expect("the group");
    assert_eq!(group_node.children().expect("a group").len(), 2);
    let moved = grouped
        .with_node_at(
            &path,
            Arc::new(
                group_node
                    .with_transform(Affine::translate(5.0, 0.0))
                    .expect("moved"),
            ),
        )
        .expect("placed");
    let (released, at) = ungroup(&moved, &path, &mut ids).expect("ungrouped");
    assert_eq!(at, vec![vec![0, 1], vec![0, 2]]);
    assert_eq!(
        xs(&released),
        vec![20.0, 5.0, 45.0],
        "each keeps the group's place"
    );
}

#[test]
fn copies_stand_above_their_originals() {
    let mut ids = Ids::new();
    let drawing = drawing(&mut ids, &[0.0, 20.0, 40.0]);
    let (copied, at) = duplicate(
        &drawing,
        &[vec![0, 0], vec![0, 2]],
        Point::new(1.0, 0.0),
        &mut ids,
    )
    .expect("copied");
    assert_eq!(xs(&copied), vec![0.0, 1.0, 20.0, 40.0, 41.0]);
    assert_eq!(at, vec![vec![0, 1], vec![0, 4]]);
    let layer = copied.layers().get(0).expect("layer");
    let children = layer.children().expect("group");
    assert_ne!(
        children.get(0).map(|n| n.id()),
        children.get(1).map(|n| n.id()),
        "a copy is a new object"
    );
}

#[test]
fn aligning_distributing_and_flipping_place_objects_as_asked() {
    let mut ids = Ids::new();
    let drawing = drawing(&mut ids, &[0.0, 15.0, 100.0]);
    let all = [vec![0, 0], vec![0, 1], vec![0, 2]];
    let page = Bounds::new(Point::ZERO, Point::new(200.0, 100.0));
    let right = align(&drawing, &all, Align::Right, page, &NoGlyphs).expect("aligned");
    assert_eq!(xs(&right), vec![190.0, 190.0, 190.0]);
    let spread = distribute(&drawing, &all, false, &NoGlyphs).expect("spread");
    assert_eq!(
        xs(&spread),
        vec![0.0, 50.0, 100.0],
        "equal gaps, the outermost unmoved"
    );
    let flipped = flip(&drawing, &[vec![0, 0], vec![0, 1]], false, &NoGlyphs).expect("flipped");
    assert_eq!(xs(&flipped), vec![15.0, 0.0, 100.0]);
}

#[test]
fn a_union_of_two_rectangles_is_one_path_where_the_lowest_stood() {
    let mut ids = Ids::new();
    let drawing = drawing(&mut ids, &[0.0, 5.0, 100.0]);
    let (joined, at) = combine(
        &drawing,
        &[vec![0, 1], vec![0, 0]],
        Operation::Union,
        &NoGlyphs,
        &mut ids,
    )
    .expect("joined");
    assert_eq!(at, vec![0, 0]);
    assert_eq!(xs(&joined), vec![0.0, 100.0]);
    let union = joined.node_at(&at).expect("the union");
    assert!(matches!(
        union.kind(),
        Kind::Shape {
            shape: Shape::Path(_),
            ..
        }
    ));
    assert!((union.bounds().width() - 15.0).abs() < 1e-6);
    assert_eq!(
        combine(
            &drawing,
            &[vec![0, 0]],
            Operation::Union,
            &NoGlyphs,
            &mut ids
        )
        .err(),
        Some(EditError::Nothing)
    );
}

#[test]
fn a_restyled_group_restyles_everything_in_it_and_layers_come_and_go() {
    let mut ids = Ids::new();
    let drawing = drawing(&mut ids, &[0.0, 20.0]);
    let red = |style: &Style| Style {
        fill: Paint::Colour(Rgba::new(255, 0, 0, 255)),
        ..style.clone()
    };
    let painted = restyle(&drawing, &[vec![0]], &red).expect("restyled");
    let layer = painted.layers().get(0).expect("layer");
    assert!(layer.children().expect("group").iter().all(|node| node
        .style()
        .is_some_and(|style| style.fill == Paint::Colour(Rgba::new(255, 0, 0, 255)))));
    let (two, path) = add_layer(&painted, "Ink", &mut ids).expect("a layer");
    assert_eq!(path, vec![1]);
    assert_eq!(
        two.layers().get(1).and_then(|layer| layer.name()),
        Some("Ink")
    );
    let one = remove_layer(&two, 1).expect("removed");
    assert_eq!(
        remove_layer(&one, 0).err(),
        Some(EditError::Nothing),
        "the last layer stays"
    );
}

#[test]
fn an_edit_that_changes_nothing_answers_the_very_drawing() {
    let mut ids = Ids::new();
    let drawing = drawing(&mut ids, &[0.0, 20.0, 40.0]);
    let same = restyle(&drawing, &[vec![0, 0], vec![0, 1]], &Style::clone).expect("restyled");
    assert!(
        same.is(&drawing),
        "no style changed, so no node is made anew"
    );
    let (top, at) = arrange(&drawing, &[vec![0, 2]], Arrange::Top).expect("arranged");
    assert!(top.is(&drawing), "already on top");
    assert_eq!(at, vec![vec![0, 2]]);
    let (bottom, _) =
        arrange(&drawing, &[vec![0, 0], vec![0, 1]], Arrange::Bottom).expect("arranged");
    assert!(bottom.is(&drawing), "already at the bottom");
    let all = [vec![0, 0], vec![0, 1], vec![0, 2]];
    let spread = distribute(&drawing, &all, false, &NoGlyphs).expect("spread");
    assert!(spread.is(&drawing), "already evenly spread");
    let page = Bounds::new(Point::ZERO, Point::new(200.0, 100.0));
    let top = align(&drawing, &all, Align::Top, page, &NoGlyphs).expect("aligned");
    assert!(top.is(&drawing), "already along the top");
}

#[test]
fn many_objects_go_in_and_out_together() {
    let mut ids = Ids::new();
    let lefts: Vec<f64> = (0..300).map(|i| f64::from(i) * 20.0).collect();
    let drawing = drawing(&mut ids, &lefts);
    let more: Vec<Arc<Node>> = (0..200)
        .map(|i| rect(&mut ids, 9000.0 + f64::from(i), 1.0))
        .collect();
    let (grown, at) = append(&drawing, &[0], more).expect("appended");
    assert_eq!(at.len(), 200);
    assert_eq!(at.first(), Some(&vec![0, 300]));
    assert_eq!(at.last(), Some(&vec![0, 499]));
    let every_other: Vec<Vec<usize>> = (0..500).step_by(2).map(|i| vec![0, i]).collect();
    let thinned = remove(&grown, &every_other).expect("removed");
    let kept = xs(&thinned);
    assert_eq!(kept.len(), 250);
    assert_eq!(
        vec![kept[0], kept[149], kept[150]],
        vec![20.0, 5980.0, 9001.0]
    );
    assert_eq!(
        append(&drawing, &[], Vec::new()).err(),
        Some(EditError::Nothing),
        "objects go into a group"
    );
}

#[test]
fn bounds_say_whether_text_waits_on_glyphs() {
    let mut ids = Ids::new();
    let drawing = drawing(&mut ids, &[0.0]);
    assert!(
        framed(&drawing, &[vec![0, 0]], &NoGlyphs).complete,
        "a shape waits on nothing"
    );
    let text = Node::new(
        ids.issue(),
        Kind::Text {
            text: Arc::new(tairix_drawing::Text {
                content: Arc::from("Hi"),
                family: Arc::from("sans-serif"),
                size: 10.0,
                weight: 400,
                italic: false,
                letter_spacing: 0.0,
                line_height: 1.25,
                align: tairix_drawing::TextAlign::Start,
                origin: Point::new(50.0, 50.0),
            }),
            style: Arc::new(Style::default()),
        },
    )
    .expect("text");
    let (with_text, path) = insert(&drawing, &[0], Arc::new(text)).expect("put");
    let waiting = framed(&with_text, &[vec![0, 0], path], &NoGlyphs);
    assert!(!waiting.complete, "its glyphs have not arrived");
    assert!(
        !waiting.bounds.is_empty(),
        "framed by its estimate meanwhile"
    );
}

#[test]
fn text_whose_glyphs_are_missing_is_not_traced_without_them() {
    let mut ids = Ids::new();
    let drawing = drawing(&mut ids, &[]);
    let text = Node::new(
        ids.issue(),
        Kind::Text {
            text: Arc::new(tairix_drawing::Text {
                content: Arc::from("Hi"),
                family: Arc::from("sans-serif"),
                size: 10.0,
                weight: 400,
                italic: false,
                letter_spacing: 0.0,
                line_height: 1.25,
                align: tairix_drawing::TextAlign::Start,
                origin: Point::new(50.0, 50.0),
            }),
            style: Arc::new(Style::default()),
        },
    )
    .expect("text");
    let (with_text, path) = insert(&drawing, &[0], Arc::new(text)).expect("put");
    assert_eq!(
        super::to_paths(&with_text, &[path], &NoGlyphs).err(),
        Some(EditError::Unlettered)
    );
}

/// The identity of `node` and of everything within it, clips included.
fn gather(node: &Node, into: &mut Vec<tairix_drawing::ObjectId>) {
    into.push(node.id());
    if let Kind::Group { children, clip } = node.kind() {
        for child in children.iter() {
            gather(child, into);
        }
        if let Some(clip) = clip {
            gather(clip, into);
        }
    }
}

#[test]
fn ungrouping_a_clipped_group_gives_every_new_object_an_identity_of_its_own() {
    let mut ids = Ids::new();
    let drawing = drawing(&mut ids, &[0.0, 20.0]);
    let (grouped, path) = group(&drawing, &[vec![0, 0], vec![0, 1]], &mut ids).expect("grouped");
    let group_node = grouped.node_at(&path).expect("the group");
    let Kind::Group { children, .. } = group_node.kind() else {
        panic!("a group");
    };
    let clipped = group_node
        .with_kind(Kind::Group {
            children: children.clone(),
            clip: Some(rect(&mut ids, 0.0, 15.0)),
        })
        .expect("clipped");
    let clipped = grouped
        .with_node_at(&path, Arc::new(clipped))
        .expect("placed");
    let (released, _) = ungroup(&clipped, &path, &mut ids).expect("ungrouped");
    let mut seen = Vec::new();
    for layer in released.layers().iter() {
        gather(layer, &mut seen);
    }
    let all = seen.len();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), all, "no identity is shared");
}
