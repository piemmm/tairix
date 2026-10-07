use core::f64::consts::PI;

use super::*;
use crate::compose::woodland::ACROSS;
use crate::compose::{Composition, Setting};
use crate::detail::Detail;
use crate::vector::wrapped;

/// A mesh too large to index in thirty-two bits is refused before any of
/// its triangles are counted out, never wrapped.
#[test]
fn a_mesh_past_its_indices_is_refused() {
    assert!(strip_faces(usize::try_from(u32::MAX).expect("wide"), &[9]).is_none());
    let mut faces = Vec::new();
    assert!(fan(&mut faces, (u32::MAX - 4, 9), true).is_none());
    assert!(faces.is_empty());
    assert!(fan(&mut faces, (u32::MAX - 9, 9), true).is_some());
}

#[test]
fn runs_are_found_where_they_hold_and_narrowed_to_where_they_change() {
    let found = runs((0.0, 10.0), |along| ((3.2..7.9).contains(&along), 1.0)).expect("room");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        (found[0].0 - 3.2).abs() < 1e-3 && (found[0].1 - 7.9).abs() < 1e-3,
        "{found:?}"
    );
    let ends = runs((0.0, 10.0), |along| (!(4.0..5.0).contains(&along), 0.7)).expect("room");
    assert_eq!(ends.len(), 2, "{ends:?}");
    assert!(
        ends[0].0.abs() < 1e-12 && (ends[0].1 - 4.0).abs() < 1e-3,
        "{ends:?}"
    );
    assert!(
        (ends[1].0 - 5.0).abs() < 1e-3 && (ends[1].1 - 10.0).abs() < 1e-12,
        "{ends:?}"
    );
    assert!(runs((0.0, 10.0), |_| (false, 0.5))
        .expect("room")
        .is_empty());
}

/// A run laid along `x`, the line's left `z`: its strips' faces look out
/// from its middle and the fans closing its ends look back and on along it.
#[test]
fn a_mesh_is_wound_to_face_out_and_closed_at_both_ends() {
    let strips: &[usize] = &[9];
    let section: Vec<(f64, f64)> = (0..9)
        .map(|k| {
            let angle = (-60.0 + 37.5 * f64::from(k)) * PI / 180.0;
            (mathf::cos(angle), 1.0 + mathf::sin(angle))
        })
        .collect();
    let mut vertices: Vec<Vec3> = Vec::new();
    for station in 0..4 {
        vertices.extend(
            section
                .iter()
                .map(|&(across, up)| Vec3::new(f64::from(station), up, across)),
        );
    }
    let mut faces = strip_faces(4, strips).expect("room");
    let sides = faces.len();
    for (stations, forward) in [(0..9, false), (27..36, true)] {
        let cap = outline(&vertices[stations], strips).expect("room");
        let base = u32::try_from(vertices.len()).expect("small");
        vertices.extend_from_slice(&cap);
        fan(&mut faces, (base, cap.len()), forward).expect("room");
    }
    assert_eq!(faces.len(), sides + 2 * 7);
    for (index, &[a, b, c]) in faces.iter().enumerate() {
        let corner = |at: u32| vertices[at as usize];
        let normal = (corner(b) - corner(a)).cross(corner(c) - corner(a));
        let middle = (corner(a) + corner(b) + corner(c)) * (1.0 / 3.0);
        let out = if index < sides {
            Vec3::new(0.0, middle.y - 1.0, middle.z)
        } else if index < sides + 7 {
            Vec3::new(-1.0, 0.0, 0.0)
        } else {
            Vec3::new(1.0, 0.0, 0.0)
        };
        assert!(normal.dot(out) > 0.0, "face {index} looks in: {normal:?}");
    }
}

/// A far farmland's hedges and walls stand on as one mesh from where they
/// stop being built to where they shrink to about a pixel, across the view:
/// none nearer than they are built, a wall's mesh taking up where its stones
/// stop, and none further than it still spans a pixel.
#[test]
fn far_hedges_and_walls_stand_on_where_they_are_not_built() {
    let size = (1280, 720);
    let mut seen = (false, false);
    for seed in 16..22 {
        let mut composition =
            Composition::new(Setting::Farmland, seed, size, Detail::Simple).expect("composes");
        let (pixel, reach) = (composition.stage.pixel, Detail::Simple.densities().bounds);
        let stones = drystone::TYPICAL / (reach.stones * pixel);
        composition.run_until_seen().expect("a land");
        let camera = &composition.seen.as_ref().expect("seen").1;
        let eye = camera.eye();
        let ahead = camera.ray((0.0, 0.0), (0.0, 0.0)).dir;
        let heading = mathf::atan2(ahead.x, ahead.z);
        let kind = |material: Option<u16>| {
            material
                .and_then(|material| composition.stage.materials.get(usize::from(material)))
                .map(|made| match &made.pigment {
                    Pigment::Clumped(_) => Some(Bound::Hedge),
                    Pigment::Masonry(masonry) if masonry.massed.is_some() => Some(Bound::Wall),
                    _ => None,
                })
        };
        let mut nearest = (f64::INFINITY, f64::INFINITY);
        for building in composition.buildings() {
            for part in building.parts() {
                let Part::Facet(facet) = part else {
                    continue;
                };
                let Some(Some(bound)) = kind(facet.material) else {
                    continue;
                };
                for &corner in &facet.corners {
                    let at =
                        building.vertex(corner).expect("a vertex") + Vec3::new(eye.x, 0.0, eye.z);
                    let apart = mathf::hypot(at.x - eye.x, at.z - eye.z);
                    let off = wrapped(mathf::atan2(at.x - eye.x, at.z - eye.z) - heading).abs();
                    assert!(
                        off < ACROSS + 0.4,
                        "{seed}: a far boundary {off} off the view"
                    );
                    let (built, tallest) = match bound {
                        Bound::Hedge => (reach.hedges - 1.5, 4.6),
                        _ => (stones - 2.1, 1.75),
                    };
                    assert!(
                        apart > built,
                        "{seed}: a far {bound:?} {apart} m off, built to {built}"
                    );
                    assert!(
                        apart < tallest / pixel * 1.03 + 5.0,
                        "{seed}: a far {bound:?} {apart} m off"
                    );
                    if bound == Bound::Hedge {
                        nearest.0 = nearest.0.min(apart);
                    } else {
                        nearest.1 = nearest.1.min(apart);
                    }
                }
            }
        }
        seen.0 |= nearest.0 < reach.hedges + 30.0;
        seen.1 |= nearest.1 < stones + 3.0;
    }
    assert!(
        seen.0 && seen.1,
        "no hedge or no wall stood on from where it is built: {seen:?}"
    );
}
