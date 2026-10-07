use super::*;
use crate::compose::{Composition, Setting};
use crate::detail::Detail;

/// A meadow made for hay lies with its round bales: each in the cut field,
/// resting on its ground or on the bale it is stacked on, a round bale on
/// its side with its axis across the baler's way.
#[test]
fn a_cut_fields_bales_rest_on_its_cut_ground() {
    let mut composition =
        Composition::new(Setting::Farmland, 47, (640, 360), Detail::Simple).expect("composes");
    let land = composition.run_until_seen().expect("a land");
    let fields = &composition.stage.fields;
    let bales: alloc::vec::Vec<(Solid, Bound)> = composition
        .buildings()
        .flat_map(crate::prototype::Building::parts)
        .filter_map(|part| match part {
            Part::Solid(solid) => {
                let made = composition
                    .stage
                    .materials
                    .get(usize::from(solid.material()))
                    .expect("a material");
                match &made.pigment {
                    Pigment::Straw(straw) => Some((*solid, straw.bound)),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    // How far a unit reaches from its middle toward the ground facing
    // `normal`: a box by each of its half extents as they lean; a round unit
    // by its radius across its axis and its half length along it.
    let reaching = |solid: &Solid, normal: Vec3| {
        let (frame, half) = (solid.frame(), solid.half());
        if matches!(solid.form(), Form::Block { .. }) {
            half.x * frame.x.dot(normal).abs()
                + half.y * frame.y.dot(normal).abs()
                + half.z * frame.z.dot(normal).abs()
        } else {
            let along = frame.y.dot(normal);
            half.x * mathf::sqrt(1.0 - along * along) + half.y * along.abs()
        }
    };
    for (solid, bound) in &bales {
        let centre = solid.centre();
        let normal = land.grids.normal(fields, centre.x, centre.z);
        if *bound == Bound::Rolled {
            assert!(
                solid.frame().y.dot(normal).abs() < 0.1,
                "a round bale stands on its end at {centre:?}"
            );
        }
        let over = (centre.y - land.grids.height(fields, centre.x, centre.z)) * normal.y;
        let grounded = (over - reaching(solid, normal)).abs() < 0.15;
        let stacked = bales.iter().any(|(under, _)| {
            let apart = under.centre() - centre;
            Point::new(apart.x, apart.z).length() < under.half().x.min(under.half().z)
                && ((centre - under.centre()).dot(normal)
                    - reaching(solid, normal)
                    - reaching(under, normal))
                .abs()
                    < 0.05
        });
        assert!(
            grounded || stacked,
            "a bale at {centre:?}, {over} over its ground, rests on neither it nor a bale"
        );
        // A bale on the ground rests where the field lies cut, within the few
        // centimetres it is sunk into it: a round bale on its side, all else
        // on its foot.
        if !stacked {
            let foot = match bound {
                Bound::Rolled => centre - normal * solid.half().x,
                _ => centre - solid.frame().y * solid.half().y,
            };
            let lies_cut = [
                (0.0, 0.0),
                (0.06, 0.0),
                (-0.06, 0.0),
                (0.0, 0.06),
                (0.0, -0.06),
            ]
            .into_iter()
            .any(|(dx, dz)| {
                cut(Grown::of(
                    land.grids.lie(fields, foot.x + dx, foot.z + dz).grown,
                ))
            });
            assert!(
                lies_cut,
                "a {bound:?} bale {:?} resting at {foot:?} off cut ground",
                solid.half()
            );
        }
    }
    assert!(bales.len() > 3, "{} bales", bales.len());
}

/// Only a field lying cut is baled.
#[test]
fn only_cut_ground_is_baled() {
    assert!(cut(Grown::Hayed));
    assert!(cut(Grown::Sown(Crop::Barley, Growth::Stubble)));
    assert!(cut(Grown::Sown(Crop::Ley, Growth::Stubble)));
    assert!(!cut(Grown::Sown(Crop::Wheat, Growth::Ripe)));
    assert!(
        !cut(Grown::Sown(Crop::Maize, Growth::Stubble)),
        "maize is chopped, not baled"
    );
    assert!(!cut(Grown::Mown) && !cut(Grown::Grazed));
}
