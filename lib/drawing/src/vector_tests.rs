//! The walk the vector formats share, and the drawing their tests write.

use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_bezier::{Affine, Point};
use tairix_colour::Rgba;

use super::Real;
use crate::children::Children;
use crate::drawing::{Drawing, Grid, Info, Page, Unit};
use crate::id::Ids;
use crate::node::{Kind, Node};
use crate::shape::{EllipseShape, RectShape, Shape};
use crate::style::{Gradient, GradientKind, Paint, Spread, Stop, Stroke, Style};
use crate::testing::{shared, slice};
use crate::text::{Baseline, Text, TextAlign};
use crate::SharedSlice;

fn node(ids: &mut Ids, kind: Kind) -> Arc<Node> {
    Arc::new(Node::new(ids.issue(), kind).expect("a node"))
}

fn rect(ids: &mut Ids, x: f64, style: Style) -> Arc<Node> {
    node(
        ids,
        Kind::Shape {
            shape: Shape::Rect(RectShape {
                x,
                y: 10.0,
                width: 40.0,
                height: 30.0,
                rx: 0.0,
                ry: 0.0,
            }),
            style: Arc::new(style),
        },
    )
}

fn gradient(kind: GradientKind, spread: Spread) -> Paint {
    Paint::Gradient(Arc::new(Gradient {
        kind,
        stops: vec![
            Stop {
                offset: 0.0,
                colour: Rgba::new(255, 0, 0, 255),
            },
            Stop {
                offset: 0.5,
                colour: Rgba::new(0, 255, 0, 128),
            },
            Stop {
                offset: 1.0,
                colour: Rgba::new(0, 0, 255, 255),
            },
        ],
        spread,
        transform: Affine::IDENTITY,
    }))
}

fn group(ids: &mut Ids, children: Vec<Arc<Node>>, clip: Option<Arc<Node>>) -> Node {
    Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(children).expect("held"),
            clip,
        },
    )
    .expect("a group")
}

/// A reflected radial fill, stroked with a repeated linear gradient.
fn radial() -> Style {
    Style {
        fill: gradient(
            GradientKind::Radial {
                centre: Point::new(120.0, 25.0),
                radius: 5.0,
                focus: Point::new(121.0, 25.0),
            },
            Spread::Reflect,
        ),
        stroke: Stroke {
            paint: gradient(
                GradientKind::Linear {
                    start: Point::new(0.0, 0.0),
                    end: Point::new(10.0, 10.0),
                },
                Spread::Repeat,
            ),
            width: 2.0,
            ..Stroke::default()
        },
        ..Style::default()
    }
}

/// A drawing of most of what a vector format is asked to say.
pub(crate) fn sample(ids: &mut Ids) -> Drawing {
    let stroked = Style {
        fill: Paint::Colour(Rgba::new(10, 20, 30, 255)),
        stroke: Stroke {
            paint: Paint::Colour(Rgba::new(200, 100, 0, 128)),
            width: 3.0,
            dashes: slice(&[4.0, 2.0]),
            dash_offset: 1.0,
            ..Stroke::default()
        },
        ..Style::default()
    };
    let linear = Style {
        fill: gradient(
            GradientKind::Linear {
                start: Point::new(0.0, 0.0),
                end: Point::new(40.0, 0.0),
            },
            Spread::Pad,
        ),
        stroke: Stroke {
            paint: Paint::None,
            ..Stroke::default()
        },
        ..Style::default()
    };
    let window_a = rect(ids, 0.0, Style::default());
    let window_b = node(
        ids,
        Kind::Shape {
            shape: Shape::Ellipse(EllipseShape {
                centre: Point::new(60.0, 25.0),
                radii: Point::new(15.0, 15.0),
                arc: None,
            }),
            style: Arc::new(Style::default()),
        },
    );
    let windows = Arc::new(group(ids, vec![window_a, window_b], None));
    let content = vec![rect(ids, 0.0, stroked.clone())];
    let clipped = group(ids, content, Some(windows));
    let pair = vec![rect(ids, 50.0, stroked), rect(ids, 60.0, linear)];
    let half = group(ids, pair, None).with_opacity(0.5).expect("weakened");
    let text = node(
        ids,
        Kind::Text {
            text: Arc::new(Text {
                content: shared("ab"),
                family: shared("sans-serif"),
                size: 12.0,
                weight: 400,
                italic: false,
                letter_spacing: 0.0,
                align: TextAlign::Start,
                baseline: Baseline::Lines {
                    origin: Point::new(10.0, 80.0),
                    line_height: 1.25,
                },
                runs: SharedSlice::new(),
            }),
            style: Arc::new(Style::default()),
        },
    );
    let shaded = rect(ids, 100.0, radial());
    let layer = group(
        ids,
        vec![Arc::new(clipped), Arc::new(half), shaded, text],
        None,
    );
    Drawing::new(
        Page {
            width: 200.0,
            height: 100.0,
            unit: Unit::Px,
            background: Some(Rgba::new(250, 250, 240, 255)),
        },
        Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        SharedSlice::new(),
        Grid::default(),
    )
    .expect("a drawing")
    .with_info(Info {
        title: Some(shared("Rivers & roads")),
        description: None,
    })
    .expect("titled")
}

#[test]
fn numbers_are_spelled_as_both_formats_read_them() {
    assert_eq!(Real(1e-300).to_string(), "0");
    assert_eq!(Real(-0.000_01).to_string(), "0");
    assert_eq!(Real(12.5).to_string(), "12.5");
    assert_eq!(Real(1e9).to_string(), "1000000000");
    assert_eq!(Real(f64::NAN).to_string(), "0");
    assert!(!Real(1e20).to_string().contains('e'));
}

/// Pictures as the vector formats meet them: an opaque grey one, a
/// translucent one shown twice, one read from a baseline JPEG turned by its
/// metadata, and one read from a JPEG said to be progressive.
pub(crate) fn pictured(ids: &mut Ids) -> (Drawing, alloc::vec::Vec<u8>) {
    use crate::testing::{image, jpeg, pixels, source_of};
    let rect = |x0, y0, x1, y1| {
        tairix_bezier::Bounds::from_points([Point::new(x0, y0), Point::new(x1, y1)])
    };
    let grey = crate::image::Image::new(
        pixels(3, 2, |x, y| {
            let level = u8::try_from(40 * (x + y)).expect("a level");
            [level, level, level, 255]
        }),
        rect(0.0, 0.0, 30.0, 20.0),
        None,
    )
    .expect("grey");
    let clear = crate::image::Image::new(
        pixels(2, 2, |x, _| {
            if x == 0 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 0, 0]
            }
        }),
        rect(0.0, 0.0, 20.0, 20.0),
        None,
    )
    .expect("clear");
    let photo = pixels(4, 2, |x, _| {
        [u8::try_from(60 * x).expect("a level"), 100, 150, 255]
    });
    let baseline = jpeg(&photo);
    let mut source = source_of(&baseline);
    let decoded = tairix_image::decode(
        &baseline,
        &tairix_image::DecodeLimits::new(64, 64, 4096, 4096),
    )
    .expect("decoded");
    let pixels_of = || tairix_raster::Surface::from_rgba8(4, 2, decoded.pixels()).expect("pixels");
    let kept = crate::image::Image::new(
        pixels_of(),
        rect(0.0, 0.0, 40.0, 20.0),
        Some(source.clone()),
    )
    .expect("kept");
    if let crate::image::Encoding::Jpeg(frame) = &mut source.encoding {
        frame.progressive = true;
    }
    let progressive =
        crate::image::Image::new(pixels_of(), rect(0.0, 0.0, 40.0, 20.0), Some(source))
            .expect("progressive");
    let layer = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(vec![
                image(ids, grey, Affine::translate(5.0, 5.0)),
                image(ids, clear.clone(), Affine::translate(50.0, 5.0)),
                image(ids, clear, Affine::translate(80.0, 5.0)),
                image(ids, kept, Affine::translate(5.0, 50.0)),
                image(ids, progressive, Affine::translate(60.0, 50.0)),
            ])
            .expect("held"),
            clip: None,
        },
    )
    .expect("a layer");
    let drawing = Drawing::new(
        Page {
            width: 200.0,
            height: 100.0,
            unit: Unit::Px,
            background: None,
        },
        Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        SharedSlice::new(),
        Grid::default(),
    )
    .expect("a drawing");
    (drawing, baseline)
}

#[test]
fn a_clear_pixel_takes_its_shown_neighbours_colour() {
    use tairix_raster::{Color, Pixel};
    let red = Color::rgba(255, 0, 0, 255).premultiply();
    let faint = Color::rgba(0, 0, 255, 51).premultiply();
    let clear = Pixel::TRANSPARENT;
    let row = [red, clear, faint];
    let bled = super::bled([None, Some(&row), None], 1);
    // Weighted by how much each shows: 255 of red to 51 of blue.
    assert_eq!((bled.r, bled.g, bled.b, bled.a), (213, 0, 43, 0));
    let alone = [clear];
    assert_eq!(
        super::bled([None, Some(&alone), None], 0),
        Color::TRANSPARENT,
        "nothing to take is nothing"
    );
}

#[test]
fn a_stored_jpeg_is_turned_onto_its_picture_as_its_metadata_says() {
    let (drawing, _) = pictured(&mut Ids::new());
    let layer = drawing.layers().get(0).expect("a layer");
    let node = layer
        .children()
        .and_then(|children| children.get(3))
        .expect("the photo");
    let Kind::Image { image } = node.kind() else {
        panic!("a picture");
    };
    let raster = super::Raster::of(image, true);
    assert!(matches!(raster, super::Raster::Jpeg(..)));
    assert_eq!(raster.size(), (4, 2));
    let to_place = raster.to_place(image);
    assert_eq!(to_place.map(Point::ZERO), image.place().min);
    assert!(
        matches!(super::Raster::of(image, false), super::Raster::Jpeg(..)),
        "baseline"
    );
    let progressive = layer
        .children()
        .and_then(|children| children.get(4))
        .expect("progressive");
    let Kind::Image { image } = progressive.kind() else {
        panic!("a picture");
    };
    assert!(matches!(
        super::Raster::of(image, false),
        super::Raster::Pixels(_)
    ));
    assert!(matches!(
        super::Raster::of(image, true),
        super::Raster::Jpeg(..)
    ));
}
