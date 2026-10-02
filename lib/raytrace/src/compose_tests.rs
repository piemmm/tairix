//! Host tests of the scene composer: every setting, under several seeds,
//! makes a scene that is lit, sound, seen from the open, framed, and exposed
//! to read on screen.
//!
//! A scene on a land is only whole once its land is built, which is the most
//! a scene costs, so every scene the tests look at is built once, the lot
//! spread over the host's threads, and every test reads the same corpus.

extern crate std;

use alloc::vec::Vec;
use std::sync::OnceLock;

use tairix_parallel::Threaded;
use tairix_util::mathf;

use super::*;
use crate::sample::{mix32, unit};
use crate::scene::{Draft, Scene, Sight};
use crate::tone::Encoder;
use crate::trace::{Quality, Tracer};
use crate::vector::Ray;

/// The picture a scene's tests are composed for.
const SIZE: (u32, u32) = (64, 36);

/// Seeds every setting is built under.
const SEEDS: u64 = 3;

/// Seeds a setting that stands on no land is also built under, being cheap.
const STILL_SEEDS: u64 = 12;

/// Whether `setting` may stand on a land, which is the most a scene costs to
/// build.
fn landed(setting: Setting) -> bool {
    !matches!(
        setting,
        Setting::Classic | Setting::Studio | Setting::Crystals | Setting::Nocturne
    )
}

/// The seeds `setting` is built under.
fn seeds(setting: Setting) -> u64 {
    if landed(setting) {
        SEEDS
    } else {
        STILL_SEEDS
    }
}

/// A scene built, with the setting and seed it was built from.
struct Built {
    setting: Setting,
    seed: u64,
    scene: Scene,
}

/// Prepare a draft of `setting` under `seed` across `runner` to the end.
fn build(setting: Setting, seed: u64, runner: Threaded) -> Scene {
    let mut draft = Draft::new(setting, seed, SIZE).expect("the scene composes");
    while !draft
        .prepare(&runner, &mut || false)
        .expect("the scene prepares")
    {}
    draft.finish().expect("the scene finishes")
}

/// Every setting's scenes, built once for every test.
fn corpus() -> &'static [Built] {
    static CORPUS: OnceLock<Vec<Built>> = OnceLock::new();
    CORPUS.get_or_init(|| {
        let wanted: Vec<(Setting, u64)> = Setting::ALL
            .into_iter()
            .flat_map(|setting| (0..seeds(setting)).map(move |seed| (setting, seed)))
            .collect();
        // Several scenes at once, each spread over a few threads: enough to
        // keep a host busy without holding every scene's grids at once.
        let next = core::sync::atomic::AtomicUsize::new(0);
        let built = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..6 {
                scope.spawn(|| {
                    let runner = Threaded::new(4);
                    loop {
                        let at = next.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                        let Some(&(setting, seed)) = wanted.get(at) else {
                            break;
                        };
                        let scene = build(setting, seed, runner);
                        let mut built = built.lock().expect("no builder panicked");
                        built.push(Built {
                            setting,
                            seed,
                            scene,
                        });
                    }
                });
            }
        });
        let mut built = built.into_inner().expect("no builder panicked");
        built.sort_by_key(|built| {
            (
                Setting::ALL.iter().position(|s| *s == built.setting),
                built.seed,
            )
        });
        built
    })
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
        Finish::Matte | Finish::Ground => true,
        Finish::Glow { radiance } => nonnegative(radiance),
    }
}

/// Every setting is named, no two alike, in letters a file name can carry.
#[test]
fn every_setting_has_a_name_of_its_own() {
    let names: Vec<&str> = Setting::ALL.iter().map(|setting| setting.name()).collect();
    for (at, name) in names.iter().enumerate() {
        assert!(
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic()),
            "{name}"
        );
        assert!(!names[..at].contains(name), "{name} twice");
    }
}

#[test]
fn every_scene_is_lit_and_made_of_sound_parts() {
    for Built {
        setting,
        seed,
        scene,
    } in corpus()
    {
        let what = alloc::format!("{setting:?} {seed}");
        assert!(
            !scene.lights.is_empty()
                || scene
                    .sky
                    .radiance(Vec3::ZERO, Vec3::UP, false, 0.5)
                    .max_element()
                    > 0.0,
            "{what}: unlit"
        );
        assert!(
            scene.objects.len() >= 2,
            "{what}: {} objects",
            scene.objects.len()
        );
        assert!(scene.exposure > 0.0 && scene.exposure.is_finite(), "{what}");
        for material in &scene.materials {
            assert!(sound(&material.finish), "{what}: {material:?}");
            if let Pigment::Solid(colour) = material.pigment {
                assert!(
                    nonnegative(colour) && colour.max_element() <= 1.0,
                    "{what}: {colour:?}"
                );
            }
        }
        for (index, object) in scene.objects.iter().enumerate() {
            assert!(object.material < scene.materials.len(), "{what}");
            // Light passes glass, water and bubbles, and nothing else.
            let clear = matches!(
                scene.materials[object.material].finish,
                Finish::Glass { .. } | Finish::Film { shell: true, .. }
            );
            assert_eq!(object.filter.is_some(), clear, "{what}: object {index}");
            if let Some(light) = object.light {
                let owner = scene.lights.get(light).and_then(Light::object);
                assert_eq!(owner, u32::try_from(index).ok(), "{what}: lamp {light}");
            }
            match object.shape {
                Shape::Hull { first, count, .. } => {
                    assert!((first + count) as usize <= scene.faces.len(), "{what}");
                }
                Shape::Land { field } => assert!((field as usize) < scene.fields.len(), "{what}"),
                Shape::Lawn { lawn } => {
                    let lawn = &scene.lawns[lawn as usize];
                    assert!((lawn.field as usize) < scene.fields.len(), "{what}");
                }
                Shape::Instance { prototype, .. } => {
                    assert!((prototype as usize) < scene.prototypes.len(), "{what}");
                }
                _ => {}
            }
        }
    }
}

/// A scene that stands on a land stands on one that water has worn and
/// whose grids are whole: every land grid holds real heights, and the ground
/// the eye sees is shaded by what the land is like there. Every landscape
/// stands on one.
#[test]
fn a_landscape_stands_on_a_built_land() {
    let landscapes = [
        Setting::Meadow,
        Setting::Forest,
        Setting::Alpine,
        Setting::Coast,
        Setting::Desert,
        Setting::Winter,
        Setting::Canyon,
        Setting::Valley,
        Setting::Sculpture,
    ];
    for Built {
        setting,
        seed,
        scene,
    } in corpus()
    {
        let on_land = scene
            .objects
            .iter()
            .any(|object| matches!(object.shape, Shape::Land { .. }));
        assert!(
            on_land || !landscapes.contains(setting),
            "{setting:?} {seed}: no land"
        );
        if !on_land {
            continue;
        }
        let grounds = scene
            .objects
            .iter()
            .filter(|object| matches!(scene.materials[object.material].finish, Finish::Ground))
            .count();
        assert!(grounds >= 1, "{setting:?} {seed}: no ground");
        for field in &scene.fields {
            let finite = field
                .heights()
                .iter()
                .filter(|height| height.is_finite())
                .count();
            assert!(
                finite > 0 || field.side() <= 2,
                "{setting:?} {seed}: an empty grid"
            );
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
    for Built {
        setting,
        seed,
        scene,
    } in corpus()
    {
        let geometry = Geometry {
            faces: &scene.faces,
            fields: &scene.fields,
            prototypes: &scene.prototypes,
            lawns: &scene.lawns,
        };
        for object in &scene.objects {
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

/// What a scene is made of and how it is seen, as text to compare.
fn describe(scene: &Scene) -> alloc::string::String {
    alloc::format!(
        "{:?}{:?}{:?}{:?}",
        scene.objects,
        scene.lights,
        scene.camera,
        scene.exposure
    )
}

#[test]
fn a_seed_composes_the_same_scene_every_time_and_another_seed_another() {
    let runner = Threaded::new(8);
    for setting in [Setting::Classic, Setting::Ruins, Setting::Meadow] {
        let first = corpus()
            .iter()
            .find(|built| built.setting == setting && built.seed == 1)
            .expect("in the corpus");
        let again = build(setting, 1, runner);
        let other = corpus()
            .iter()
            .find(|built| built.setting == setting && built.seed == 2)
            .expect("in the corpus");
        assert_eq!(describe(&first.scene), describe(&again), "{setting:?}");
        assert_ne!(
            describe(&first.scene),
            describe(&other.scene),
            "{setting:?}"
        );
    }
}

/// The camera stands in the open: above every plane and every stretch of
/// land beneath it, and outside every solid, which each way it looks meets
/// from without.
#[test]
fn the_camera_stands_in_the_open() {
    for Built {
        setting,
        seed,
        scene,
    } in corpus()
    {
        let eye = scene.camera.eye();
        for object in &scene.objects {
            if let Shape::Plane { normal, offset } = object.shape {
                assert!(
                    normal.dot(eye) > offset + 0.05,
                    "{setting:?} {seed}: beneath a plane"
                );
            }
        }
        // Every way the eye looks, straight up first, what it meets it meets
        // from outside: beneath a land, it would meet the land's underside.
        for step in 0..=96u32 {
            let dir = if step == 96 {
                Vec3::UP
            } else {
                let turn = f64::from(step) * 2.399_963;
                let rise = 1.0 - 2.0 * (f64::from(step) + 0.5) / 96.0;
                let across = mathf::sqrt(1.0 - rise * rise);
                Vec3::new(across * mathf::cos(turn), rise, across * mathf::sin(turn))
            };
            let Some((index, hit)) = scene.closest(&Ray::new(eye, dir), f64::INFINITY, Sight::Eye)
            else {
                continue;
            };
            // Leaves and blades are thin, and met from either side.
            let thin = matches!(
                scene.objects[index].shape,
                Shape::Instance { .. } | Shape::Lawn { .. }
            );
            assert!(
                thin || hit.normal.dot(dir) < 0.0,
                "{setting:?} {seed}: the eye is inside object {index}"
            );
        }
    }
}

/// On a land the eye stands clear of what lies beneath it — the finest land
/// laid there, or the water over it — never in it or skimming it.
#[test]
fn the_eye_stands_clear_of_the_ground_beneath_it() {
    for Built {
        setting,
        seed,
        scene,
    } in corpus()
    {
        if !landed(*setting) {
            continue;
        }
        let eye = scene.camera.eye();
        let Some((_, hit)) =
            scene.closest(&Ray::new(eye, -Vec3::UP), f64::INFINITY, Sight::Recorded)
        else {
            continue;
        };
        assert!(
            hit.t > 0.5 && hit.normal.dot(-Vec3::UP) < 0.0,
            "{setting:?} {seed}: the eye stands {} above what is below it",
            hit.t
        );
    }
}

/// The eye is above every level quad beneath it: water is looked down on,
/// never up at from beneath.
#[test]
fn the_camera_is_above_the_water() {
    for Built {
        setting,
        seed,
        scene,
    } in corpus()
    {
        let eye = scene.camera.eye();
        for object in &scene.objects {
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
        let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
        let mut stage = Stage::new().expect("a stage");
        still::crystals(&mut stage, &mut dice).expect("a still life");
        let geometry = stage.geometry();
        let rock = stage.objects.iter().find_map(|object| match object.shape {
            Shape::Hull {
                pose, first, count, ..
            } if matches!(
                stage.materials[object.material].pigment,
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
        for object in &stage.objects {
            let Shape::Hull { pose, .. } = object.shape else {
                continue;
            };
            let glass = matches!(
                stage.materials[object.material].finish,
                Finish::Glass { .. }
            );
            let (dx, dz) = (pose.at.x - rock.at.x, pose.at.z - rock.at.z);
            // Loose gems lie clear of the rock's footprint, well beyond this.
            if !glass || mathf::sqrt(dx * dx + dz * dz) > 0.6 * breadth {
                continue;
            }
            let root = rock.point_to_local(pose.at);
            assert!(
                stage.faces[faces.clone()]
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
    for seed in 0..24 {
        for compose in [
            architecture::colonnade,
            architecture::rotunda,
            architecture::ruins,
        ] {
            let mut dice = Dice(NonCryptoRng::seed_from_u64(seed));
            let mut stage = Stage::new().expect("a stage");
            compose(&mut stage, &mut dice).expect("a building");
            scrolls += buried_scrolls(&stage, seed);
        }
    }
    assert!(scrolls > 0, "no Ionic capital in any building");
}

/// How many Ionic scrolls `stage` holds, each checked to be in sight.
fn buried_scrolls(stage: &Stage, seed: u64) -> u32 {
    let geometry = stage.geometry();
    // The building stands in its clearing before its land is built, and
    // nothing on the land stands near a capital.
    let first = |ray: &Ray, far: f64| {
        stage
            .objects
            .iter()
            .enumerate()
            .filter(|(_, object)| {
                !matches!(
                    object.shape,
                    Shape::Land { .. } | Shape::Lawn { .. } | Shape::Plane { .. }
                )
            })
            .filter_map(|(index, object)| {
                let hit = object.shape.intersect(ray, 1e-9, far, geometry)?;
                Some((index, hit.t))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    };
    let mut scrolls = 0;
    for (index, object) in stage.objects.iter().enumerate() {
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
        let visible = [1.0, -1.0].into_iter().any(|side| {
            let from = coil + pose.frame.y * (side * away);
            first(&Ray::new(from, pose.frame.y * -side), away) == Some(index)
        });
        assert!(visible, "{seed}: scroll {index} is buried");
    }
    scrolls
}

/// The pieces a still life or a building is about are in the picture.
#[test]
fn the_pieces_stand_in_the_frame() {
    use Setting::{
        Arcade, Bubbles, Classic, Colonnade, Crystals, Lagoon, Nocturne, Rotunda, Ruins, Studio,
    };
    let framed_settings = [
        Classic, Studio, Crystals, Nocturne, Bubbles, Colonnade, Arcade, Rotunda, Ruins, Lagoon,
    ];
    for Built {
        setting,
        seed,
        scene,
    } in corpus()
        .iter()
        .filter(|built| framed_settings.contains(&built.setting))
    {
        let geometry = Geometry {
            faces: &scene.faces,
            fields: &scene.fields,
            prototypes: &scene.prototypes,
            lawns: &scene.lawns,
        };
        let framed = scene
            .objects
            .iter()
            .filter(|object| {
                !matches!(
                    object.shape,
                    Shape::Instance { .. } | Shape::Land { .. } | Shape::Lawn { .. }
                )
            })
            .filter_map(|object| object.shape.bounds(geometry))
            .filter(|bounds| {
                scene
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

/// A coarse render of each setting is neither black, nor blown out, nor
/// flat: the exposure and lighting each setting chooses read on screen.
#[test]
fn each_setting_renders_a_picture_worth_looking_at() {
    let size = (24u32, 14u32);
    let encoder = Encoder::new().expect("an encoder");
    for Built {
        setting,
        seed,
        scene,
    } in corpus().iter().filter(|built| built.seed < SEEDS)
    {
        let tracer = Tracer::new(scene, &encoder, size, 5);
        let levels: Vec<f64> = (0..size.0 * size.1)
            .map(|at| {
                let (pixel, _) = tracer.pixel((at % size.0, at / size.0), Quality::Fair);
                Vec3::new(f64::from(pixel.r), f64::from(pixel.g), f64::from(pixel.b)).luminance()
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

/// A lawn's canopy grid holds a vertex at the middle of each block of its
/// cells and one beyond each end, and no more: its tier is never rounded up
/// to a grid as much as twice its size, every padded vertex filled.
#[test]
fn a_canopy_grid_is_as_large_as_its_lawn_and_no_larger() {
    use crate::grass::{Cover, Seen, Weeds};
    // A side's cells, a cell's breadth, and the cells to a block of the grid.
    for (cells, cell, block) in [(100.0, 0.5, 1u32), (1344.0, 0.25, 1), (2400.0, 3.0, 2)] {
        let mut stage = Stage::new().expect("a stage");
        let span = cells * cell;
        let lawn = Lawn {
            field: 0,
            from: (-40.0, 10.0),
            to: (span - 40.0, 10.0 + 0.6 * span),
            hole: None,
            floor: 0.0,
            ceiling: 1.0,
            cell,
            cover: Cover::Weeds(Weeds {
                share: 0.5,
                leaves: (5, 9),
            }),
            shade: None,
            sward: 1,
            seed: 2,
            seen: Seen::from((0.0, 0.0), (1.0, 2.0)),
            tops: None,
        };
        let tops = stage.tops(&lawn).expect("a canopy grid");
        assert_eq!(tops.block, block);
        let blocks = usize::try_from(mathf::round_i32(mathf::ceil(cells / f64::from(block))))
            .expect("a count of blocks");
        let side = stage.fields[tops.field as usize].side();
        assert_eq!(side, blocks + 2, "{cells} cells of {cell}");
    }
}

/// Every kind of prototype grows a bounded step a unit, as many at once as
/// the runner runs and never more — a rock, a log, a stump, a fern or a palm
/// made on a core of its own rather than all on the caller's — and each
/// comes out as it does grown alone.
#[test]
fn prototypes_grow_a_core_apiece_a_unit_as_each_grows_alone() {
    use crate::rock::Habit;
    use crate::tree::Stock;
    let stock = Stock { bark: 0, leaves: 0 };
    let mut recipes: Vec<Recipe> = (0..5)
        .map(|seed| Recipe::Rock {
            habit: Habit {
                squash: 0.6,
                fractures: 3,
            },
            stock: 0,
            seed,
        })
        .collect();
    recipes.extend([
        Recipe::Log {
            length: 9.0,
            radius: 0.3,
            bark: 0,
            thrown: true,
            seed: 7,
        },
        Recipe::Stump {
            height: 0.6,
            radius: 0.4,
            top: Top::Sawn,
            bark: 0,
            wood: 0,
            seed: 8,
        },
        Recipe::Fern {
            height: 0.9,
            stock,
            fronds: 12,
            seed: 9,
        },
        Recipe::Palm {
            height: 11.0,
            stock,
            fronds: 14,
            seed: 10,
        },
    ]);
    let grown = |runner: &dyn tairix_parallel::JobRunner| {
        let mut grow = Grow::new(recipes.len()).expect("room to grow");
        let mut was = 0;
        while !grow.step(&recipes, runner).expect("room to grow") {
            let now = grow.grown.iter().flatten().count();
            assert!(now - was <= runner.width(), "{was} then {now}");
            assert!(grow.active.len() <= runner.width());
            was = now;
        }
        grow.grown
            .into_iter()
            .map(|prototype| {
                let prototype = prototype.expect("grown");
                (prototype.parts().len(), describe_box(prototype.bounds()))
            })
            .collect::<Vec<_>>()
    };
    let alone = grown(&tairix_parallel::SERIAL);
    for runner in [
        &tairix_parallel::Reversed::new(3) as &dyn tairix_parallel::JobRunner,
        &Threaded::new(4),
    ] {
        assert_eq!(grown(runner), alone);
    }
}

/// A box's corners, as bits.
fn describe_box(bounds: Aabb) -> [u64; 6] {
    [
        bounds.min.x.to_bits(),
        bounds.min.y.to_bits(),
        bounds.min.z.to_bits(),
        bounds.max.x.to_bits(),
        bounds.max.y.to_bits(),
        bounds.max.z.to_bits(),
    ]
}
