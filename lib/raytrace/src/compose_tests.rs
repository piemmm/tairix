//! Host tests of the scene composer: every setting, under many seeds, makes a
//! scene that is lit, sound, seen from the open, framed, and exposed to read
//! on screen.

use alloc::vec::Vec;

use tairix_util::mathf;

use super::*;
use crate::sample::{mix32, unit};
use crate::scene::{Draft, Scene, Sight, Target};
use crate::tone::Encoder;
use crate::trace::{Quality, Tracer};
use crate::vector::Ray;

const ASPECT: f64 = 16.0 / 9.0;

/// Seeds every setting is composed under for the cheap checks, and for the
/// ones that fill its land and trace it.
const SEEDS: u64 = 24;
const TRACED: u64 = 3;

fn composed() -> impl Iterator<Item = (Setting, u64, Parts)> {
    Setting::ALL.into_iter().flat_map(|setting| {
        (0..SEEDS).map(move |seed| {
            (
                setting,
                seed,
                compose(setting, seed, ASPECT).expect("the scene composes"),
            )
        })
    })
}

fn finished(setting: Setting, seed: u64) -> Scene {
    Draft::new(setting, seed, ASPECT)
        .and_then(Draft::finish)
        .expect("the scene finishes")
}

fn unit_range(value: f64) -> bool {
    (0.0..=1.0).contains(&value)
}

fn nonnegative(colour: Vec3) -> bool {
    colour.is_finite() && colour.min(Vec3::ZERO) == Vec3::ZERO
}

fn sound(finish: &Finish) -> bool {
    match *finish {
        Finish::Coated { roughness } | Finish::Metal { roughness } => unit_range(roughness),
        Finish::Brushed { along, across } => unit_range(along) && unit_range(across),
        Finish::Lacquer { roughness, flakes } => unit_range(roughness) && flakes > 0.0,
        Finish::Glass {
            ior,
            absorb,
            glow,
            roughness,
            dispersion,
            foam,
        } => {
            ior > 1.0
                && nonnegative(absorb)
                && nonnegative(glow)
                && unit_range(roughness)
                && dispersion >= 0.0
                && foam.is_none_or(|foam| foam.spread > 0.0)
        }
        Finish::Film {
            thickness, index, ..
        } => 0.0 <= thickness.0 && thickness.0 <= thickness.1 && index > 1.0,
        Finish::Leaf { translucency } => unit_range(translucency),
        Finish::Matte => true,
        Finish::Glow { radiance } => nonnegative(radiance),
    }
}

#[test]
fn every_scene_is_lit_and_made_of_sound_parts() {
    for (setting, seed, parts) in composed() {
        let what = alloc::format!("{setting:?} {seed}");
        assert!(
            !parts.lights.is_empty() || parts.sky.ambient().max_element() > 0.0,
            "{what}: unlit"
        );
        assert!(
            parts.objects.len() >= 2,
            "{what}: {} objects",
            parts.objects.len()
        );
        assert!(parts.exposure > 0.0 && parts.exposure.is_finite(), "{what}");
        assert!(nonnegative(parts.bounce), "{what}");
        for material in &parts.materials {
            assert!(sound(&material.finish), "{what}: {material:?}");
            if let Pigment::Solid(colour) = material.pigment {
                assert!(
                    nonnegative(colour) && colour.max_element() <= 1.0,
                    "{what}: {colour:?}"
                );
            }
        }
        for (index, object) in parts.objects.iter().enumerate() {
            assert!(object.material < parts.materials.len(), "{what}");
            // Light passes glass, water and bubbles, and nothing else.
            let clear = matches!(
                parts.materials[object.material].finish,
                Finish::Glass { .. } | Finish::Film { shell: true, .. }
            );
            assert_eq!(object.filter.is_some(), clear, "{what}: object {index}");
            if let Some(light) = object.light {
                let owner = parts.lights.get(light).and_then(Light::object);
                assert_eq!(owner, u32::try_from(index).ok(), "{what}: lamp {light}");
            }
            match object.shape {
                Shape::Hull { first, count, .. } => {
                    assert!((first + count) as usize <= parts.faces.len(), "{what}");
                }
                Shape::Land { field } => assert!((field as usize) < parts.fields.len(), "{what}"),
                Shape::Lawn(ref lawn) => {
                    assert!((lawn.field as usize) < parts.fields.len(), "{what}");
                }
                _ => {}
            }
        }
        for fill in &parts.fills {
            match fill.target {
                Target::Field(index) => assert!(index < parts.fields.len(), "{what}"),
                Target::Clouds => assert!(parts.sky.clouds.is_some(), "{what}"),
            }
        }
    }
}

/// Every hull, however it was cut, lies within the extent it is bounded by,
/// so the hierarchy never culls a ray that would have met it.
#[test]
fn every_hull_lies_within_its_extent() {
    let cube = [
        Face {
            normal: Vec3::new(1.0, 0.0, 0.0),
            offset: 1.0,
        },
        Face {
            normal: Vec3::new(-1.0, 0.0, 0.0),
            offset: 2.0,
        },
        Face {
            normal: Vec3::UP,
            offset: 3.0,
        },
        Face {
            normal: -Vec3::UP,
            offset: 0.5,
        },
        Face {
            normal: Vec3::new(0.0, 0.0, 1.0),
            offset: 1.0,
        },
        Face {
            normal: Vec3::new(0.0, 0.0, -1.0),
            offset: 1.0,
        },
    ];
    let extent = hull_extent(&cube).expect("a box");
    assert!((extent.min - Vec3::new(-2.0, -0.5, -1.0)).length() < 1e-6);
    assert!((extent.max - Vec3::new(1.0, 3.0, 1.0)).length() < 1e-6);
    for (setting, seed, parts) in composed().filter(|(_, seed, _)| *seed < 6) {
        let geometry = Geometry {
            faces: &parts.faces,
            fields: &parts.fields,
        };
        for object in &parts.objects {
            let Shape::Hull { pose, extent, .. } = object.shape else {
                continue;
            };
            let reach = (extent.max - extent.min).length() * 2.0 + 1.0;
            for probe in 0..64u32 {
                let draw = |salt: u32| unit(mix32(mix32(probe) ^ salt));
                let (rise, around) = (2.0 * draw(1) - 1.0, core::f64::consts::TAU * draw(2));
                let level = mathf::sqrt(1.0 - rise * rise);
                let dir = Vec3::new(level * mathf::cos(around), rise, level * mathf::sin(around));
                let target = pose.at + pose.frame.to_world(extent.centre());
                let ray = Ray::new(target - dir * reach, dir);
                if let Some(hit) = object.shape.intersect(&ray, 1e-9, f64::INFINITY, geometry) {
                    let local = pose.point_to_local(ray.at(hit.t));
                    let inside = local.min(extent.max + Vec3::splat(1e-6)) == local
                        && local.max(extent.min - Vec3::splat(1e-6)) == local;
                    assert!(inside, "{setting:?} {seed}: {local:?} outside {extent:?}");
                }
            }
        }
    }
}

#[test]
fn a_seed_composes_the_same_scene_every_time_and_another_seed_another() {
    let describe =
        |parts: &Parts| alloc::format!("{:?}{:?}{:?}", parts.objects, parts.lights, parts.camera);
    for setting in Setting::ALL {
        let first = compose(setting, 7, ASPECT).expect("a scene");
        let again = compose(setting, 7, ASPECT).expect("a scene");
        let other = compose(setting, 8, ASPECT).expect("a scene");
        assert_eq!(describe(&first), describe(&again), "{setting:?}");
        assert_ne!(describe(&first), describe(&other), "{setting:?}");
    }
}

/// The camera stands in the open: above every plane and every stretch of
/// land beneath it, and outside every solid, which each way it looks meets
/// from without.
#[test]
fn the_camera_stands_in_the_open() {
    for setting in Setting::ALL {
        for seed in 0..TRACED {
            let scene = finished(setting, seed);
            let eye = scene.camera.eye();
            for object in &scene.objects {
                if let Shape::Plane { normal, offset } = object.shape {
                    assert!(
                        normal.dot(eye) > offset + 0.05,
                        "{setting:?} {seed}: beneath a plane"
                    );
                }
            }
            for field in &scene.fields {
                let over = field.bounds().is_none_or(|bounds| {
                    (bounds.min.x..bounds.max.x).contains(&eye.x)
                        && (bounds.min.z..bounds.max.z).contains(&eye.z)
                });
                if over {
                    assert!(
                        eye.y > field.height_at(eye.x, eye.z),
                        "{setting:?} {seed}: underground"
                    );
                }
            }
            for step in 0..96u32 {
                let turn = f64::from(step) * 2.399_963;
                let rise = 1.0 - 2.0 * (f64::from(step) + 0.5) / 96.0;
                let across = mathf::sqrt(1.0 - rise * rise);
                let dir = Vec3::new(across * mathf::cos(turn), rise, across * mathf::sin(turn));
                let Some((index, hit)) =
                    scene.closest(&Ray::new(eye, dir), f64::INFINITY, Sight::Eye)
                else {
                    continue;
                };
                // Leaves and blades are thin, and met from either side.
                let thin = matches!(scene.objects[index].shape, Shape::Crown(_) | Shape::Lawn(_));
                assert!(
                    thin || hit.normal.dot(dir) < 0.0,
                    "{setting:?} {seed}: the eye is inside object {index}"
                );
            }
        }
    }
}

/// The eye is above every level quad beneath it: water is looked down on,
/// never up at from beneath.
#[test]
fn the_camera_is_above_the_water() {
    for (setting, seed, parts) in composed() {
        let eye = parts.camera.eye();
        for object in &parts.objects {
            let Shape::Quad {
                corner,
                edge_u,
                edge_v,
            } = object.shape
            else {
                continue;
            };
            let normal = edge_u.cross(edge_v);
            let offset = eye - corner;
            let (a, b) = (
                offset.dot(edge_u) / edge_u.dot(edge_u),
                offset.dot(edge_v) / edge_v.dot(edge_v),
            );
            let level = normal.y.abs() > 0.999 * normal.length();
            if level && (0.0..=1.0).contains(&a) && (0.0..=1.0).contains(&b) {
                assert!(eye.y > corner.y + 0.05, "{setting:?} {seed}: under water");
            }
        }
    }
}

/// A druse's crystals are rooted in its rock, however flat the rock came
/// out, rather than standing in the air above it.
#[test]
fn a_druses_crystals_grow_out_of_its_rock() {
    let mut druses = 0;
    for seed in 0..64 {
        let parts = compose(Setting::Crystals, seed, ASPECT).expect("a scene");
        let geometry = Geometry {
            faces: &parts.faces,
            fields: &parts.fields,
        };
        let rock = parts.objects.iter().find_map(|object| match object.shape {
            Shape::Hull {
                pose, first, count, ..
            } if matches!(
                parts.materials[object.material].pigment,
                Pigment::Speckle { .. }
            ) =>
            {
                Some((
                    pose,
                    first as usize..(first + count) as usize,
                    object.shape.bounds(geometry)?,
                ))
            }
            _ => None,
        });
        let Some((rock, faces, bounds)) = rock else {
            continue;
        };
        druses += 1;
        let breadth = 0.5 * (bounds.max.x - bounds.min.x);
        for object in &parts.objects {
            let Shape::Hull { pose, .. } = object.shape else {
                continue;
            };
            let glass = matches!(
                parts.materials[object.material].finish,
                Finish::Glass { .. }
            );
            let (dx, dz) = (pose.at.x - rock.at.x, pose.at.z - rock.at.z);
            // Loose gems lie clear of the rock's footprint, well beyond this.
            if !glass || mathf::sqrt(dx * dx + dz * dz) > 0.6 * breadth {
                continue;
            }
            let root = rock.point_to_local(pose.at);
            assert!(
                parts.faces[faces.clone()]
                    .iter()
                    .all(|face| face.normal.dot(root) < face.offset),
                "{seed}: a crystal stands clear of its rock at {:?}",
                pose.at
            );
        }
    }
    assert!(druses > 8, "{druses} druses in 64 scenes");
}

/// An Ionic capital's scrolls stand out of the faces of its abacus, where
/// they are seen, rather than buried inside it.
#[test]
fn an_ionic_capitals_scrolls_are_in_sight() {
    let mut scrolls = 0;
    let buildings = composed().filter(|(setting, ..)| {
        matches!(
            setting,
            Setting::Colonnade | Setting::Rotunda | Setting::Ruins
        )
    });
    for (setting, seed, parts) in buildings {
        let geometry = Geometry {
            faces: &parts.faces,
            fields: &parts.fields,
        };
        // The grids are not filled yet; nothing on them stands near a capital.
        let first = |ray: &Ray, far: f64| {
            parts
                .objects
                .iter()
                .enumerate()
                .filter(|(_, object)| !matches!(object.shape, Shape::Land { .. } | Shape::Lawn(_)))
                .filter_map(|(index, object)| {
                    let hit = object.shape.intersect(ray, 1e-9, far, geometry)?;
                    Some((index, hit.t))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(index, _)| index)
        };
        for (index, object) in parts.objects.iter().enumerate() {
            let Shape::Torus {
                pose,
                major,
                minor,
                arc,
            } = object.shape
            else {
                continue;
            };
            if arc > -1.0 || pose.frame.y.y.abs() > 1e-9 {
                continue;
            }
            scrolls += 1;
            // Straight at its coil from a little way off either face.
            let coil = pose.at + pose.frame.x * major;
            let away = 4.0 * (major + minor);
            let seen = [1.0, -1.0].into_iter().any(|side| {
                let from = coil + pose.frame.y * (side * away);
                first(&Ray::new(from, pose.frame.y * -side), away) == Some(index)
            });
            assert!(seen, "{setting:?} {seed}: scroll {index} is buried");
        }
    }
    assert!(scrolls > 0, "no Ionic capital in any building");
}

/// The pieces a still life or a building is about are in the picture.
#[test]
fn the_pieces_stand_in_the_frame() {
    use Setting::{
        Arcade, Bubbles, Classic, Colonnade, Crystals, Lagoon, Nocturne, Rotunda, Ruins, Studio,
    };
    for setting in [
        Classic, Studio, Crystals, Nocturne, Bubbles, Colonnade, Arcade, Rotunda, Ruins, Lagoon,
    ] {
        for seed in 0..SEEDS {
            let parts = compose(setting, seed, ASPECT).expect("a scene");
            let geometry = Geometry {
                faces: &parts.faces,
                fields: &parts.fields,
            };
            let framed = parts
                .objects
                .iter()
                .filter_map(|object| object.shape.bounds(geometry))
                .filter(|bounds| {
                    parts
                        .camera
                        .project(bounds.centre())
                        .is_some_and(|(x, y)| x.abs() <= 1.0 && y.abs() <= 1.0)
                })
                .count();
            assert!(
                framed >= 3,
                "{setting:?} {seed}: {framed} pieces in the frame"
            );
        }
    }
}

/// A coarse render of each setting is neither black, nor blown out, nor
/// flat: the exposure and lighting each setting chooses read on screen.
#[test]
fn each_setting_renders_a_picture_worth_looking_at() {
    let size = (24u32, 14u32);
    let encoder = Encoder::new().expect("an encoder");
    for setting in Setting::ALL {
        for seed in 0..TRACED {
            let scene = finished(setting, seed);
            let tracer = Tracer::new(&scene, &encoder, size, 5);
            let levels: Vec<f64> = (0..size.0 * size.1)
                .map(|at| {
                    let (pixel, _) = tracer.pixel((at % size.0, at / size.0), Quality::Fair);
                    (0.2126 * f64::from(pixel.r)
                        + 0.7152 * f64::from(pixel.g)
                        + 0.0722 * f64::from(pixel.b))
                        / 255.0
                })
                .collect();
            let count = f64::from(size.0 * size.1);
            let mean = levels.iter().sum::<f64>() / count;
            let spread =
                mathf::sqrt(levels.iter().map(|l| (l - mean) * (l - mean)).sum::<f64>() / count);
            let blown = levels.iter().filter(|l| **l > 0.99).count();
            assert!(
                (0.06..0.85).contains(&mean),
                "{setting:?} {seed}: mean {mean}"
            );
            assert!(spread > 0.02, "{setting:?} {seed}: flat, spread {spread}");
            assert!(
                blown * 6 < levels.len(),
                "{setting:?} {seed}: {blown} blown out"
            );
        }
    }
}
