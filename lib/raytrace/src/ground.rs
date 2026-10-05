//! The ground of a land, as its lie and what water and wear left on it have
//! it: bedded, jointed rock where it stands too steep for soil, streaked
//! where water runs down it and lichened where it faces the sky; soil, and
//! fresh silt where water laid it down; scree gathered below the cliffs;
//! grass, lush where it is wet and parched in patches where it is not, and
//! moss in the wettest hollows; sand along the shore; snow on the flatter
//! ground above its line; and a road or a path where one runs.
//!
//! Every pattern fades to its mean once a pixel's footprint spans it, so the
//! land far off is as steady as the land underfoot is detailed.

use core::f64::consts::{FRAC_PI_2, TAU};

use tairix_util::mathf;

use crate::course::{Courses, Nearest};
use crate::land::{decode_lane, Surface, DAMP_BANK, SOAKED, TRACK_GAUGE};
use crate::noise::{cell, cells2, fbm2, hash3, noise2, octaves_within, smoothstep};
use crate::pigment::Spot;
use crate::sample::{mix32, unit};
use crate::shade::Shades;
use crate::snow;
use crate::vector::Vec3;

/// The grains of ground too fine to model, to a metre, coarsest first: a
/// crumb or a fine pebble, coarse sand, fine sand; and the shade a grain and
/// the gap about it average to.
const GRIT: [(f64, u32); 3] = [(55.0, 0x91), (170.0, 0x92), (520.0, 0x93)];
const GRIT_MEAN: f64 = 0.96;

/// The granules of a river's bed to a metre, gravel finer than the stones a
/// bed lays; its cobbles; and its sand's mottles.
const GRANULES: f64 = 90.0;
const COBBLES: f64 = 14.0;
const MOTTLES: f64 = 9.0;

/// How deep snow must lie to hide the ground's colour beneath it, and by
/// what share of itself the broad patches of the ground thin it or deepen
/// it, so a scoured stretch bares in patches rather than along a line.
const GROUND_BURIED: f64 = 0.06;
const SNOW_EDGES: f64 = 0.6;

/// How thick the beds a cut bank shows lie, and its gravel's pebbles to a
/// metre.
const BEDS: f64 = 0.22;
const PEBBLES: f64 = 30.0;

/// A region's ground colours.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Palette {
    pub(crate) grass: Vec3,
    /// What grass parches to.
    pub(crate) dry: Vec3,
    pub(crate) moss: Vec3,
    pub(crate) earth: Vec3,
    /// What rivers lay down.
    pub(crate) silt: Vec3,
    pub(crate) rock: Vec3,
    /// The darker of the rock's beds.
    pub(crate) strata: Vec3,
    pub(crate) lichen: Vec3,
    pub(crate) sand: Vec3,
    pub(crate) snow: Vec3,
}

/// Rock: its colours and how thick its beds lie.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Rock {
    pub(crate) stone: Vec3,
    pub(crate) strata: Vec3,
    pub(crate) lichen: Vec3,
    pub(crate) bedding: f64,
    pub(crate) seed: u32,
}

/// A land's ground.
#[derive(Clone, Debug)]
pub(crate) struct Ground {
    pub(crate) palette: Palette,
    /// The height up to which the shore is sand.
    pub(crate) shore: f64,
    /// The height from which snow lies on all but the steepest ground.
    pub(crate) snow_line: f64,
    /// How upright ground must stand for soil to hold on it, as its normal's
    /// upward part.
    pub(crate) cliff: f64,
    /// How thick the rock's beds lie.
    pub(crate) bedding: f64,
    pub(crate) seed: u32,
    /// The road across the land, painted where it runs.
    pub(crate) road: Option<Road>,
    /// The floor of the woods on the land, where they stand.
    pub(crate) floor: Option<Floor>,
}

/// What lies under a wood's crowns: the leaves or needles they shed, fresh
/// and gone brown, over the dark humus they rot to, and how much of it moss
/// carpets.
#[derive(Clone, Debug)]
pub(crate) struct Floor {
    pub(crate) shades: Shades,
    pub(crate) leaves: [Vec3; 2],
    pub(crate) humus: Vec3,
    pub(crate) moss: f64,
}

/// A road to paint: what it is made of, and where it runs.
#[derive(Clone, Debug)]
pub(crate) struct Road {
    pub(crate) surface: Surface,
    pub(crate) courses: Courses,
}

/// What the water in a land's ground does to its surface.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Moisture {
    /// The share of the surface water stands on: saturated ground bare of
    /// growth, a bog's pools, mud.
    pub(crate) standing: f64,
    /// How soaked the bare soil is, which darkens it.
    pub(crate) soaked: f64,
}

impl Ground {
    /// The moisture of the ground at `spot`: growth takes up the water of all
    /// but the most saturated ground and covers the soil, and snow lies over
    /// whatever is beneath it.
    pub(crate) fn moisture(&self, spot: &Spot) -> Moisture {
        let [wet, _, _, green, _] = spot.ground;
        let bare = (1.0 - green) * (1.0 - self.snowed(spot, 0.0));
        Moisture {
            standing: smoothstep(0.82, 1.0, wet) * bare,
            soaked: wet * bare,
        }
    }

    /// How much of the ground at `spot` snow covers, `patch` the broad
    /// patches its line wanders by: above its line on all but steep ground,
    /// and wherever the wind left it lying deep enough to hide the ground,
    /// thinning to bare in patches where it scoured it.
    fn snowed(&self, spot: &Spot, patch: f64) -> f64 {
        let lying = smoothstep(
            self.snow_line - 30.0,
            self.snow_line + 30.0,
            spot.height + 45.0 * patch,
        ) * smoothstep(0.5, 0.78, spot.normal.y);
        let [.., channel] = spot.ground;
        let drifted = snow::buries(
            snow::depth_of(channel) * (1.0 + SNOW_EDGES * patch),
            GROUND_BURIED,
        );
        lying.max(drifted)
    }
}

/// Where `p` lies among a set of upright joints striking at `angle`,
/// `spacing` apart and wavering a little under `seed`: which slab between
/// them, and how far it lies from the nearer joint, as a share of the
/// spacing.
fn joint_set(p: Vec3, (angle, spacing): (f64, f64), seed: u32) -> (u32, f64) {
    let (sin, cos) = (mathf::sin(angle), mathf::cos(angle));
    let (across, along) = (p.x * cos + p.z * sin, p.z * cos - p.x * sin);
    let waver = 0.3 * noise2(along / (2.5 * spacing), p.y / (2.5 * spacing), seed);
    let (slab, within) = cell(across / spacing + waver);
    (slab, within.min(1.0 - within))
}

/// `value`, a pattern `detail` of its own periods across a pixel, faded to
/// its mean of nought as the pixel comes to span it.
fn fade(value: f64, detail: f64) -> f64 {
    value * (1.0 - smoothstep(0.25, 1.0, detail))
}

/// How much of a footprint `footprint` across, centred `distance` from a
/// line's middle, the line `half` either side of it covers.
fn coverage(distance: f64, half: f64, footprint: f64) -> f64 {
    let soft = 0.5 * footprint.max(1e-5);
    let within = 1.0 - smoothstep(half - soft, half + soft, distance);
    within * (2.0 * half / (2.0 * half).max(footprint))
}

impl Rock {
    /// The rock at `p`, its face turned toward `normal`, a pixel `width`
    /// across.
    pub(crate) fn colour(&self, p: Vec3, normal: Vec3, width: f64) -> Vec3 {
        let Self {
            stone: rock,
            strata,
            lichen,
            bedding,
            seed,
        } = *self;
        let warp =
            0.35 * bedding * noise2(p.x / (9.0 * bedding), p.z / (9.0 * bedding), seed ^ 0x51);
        let (bed, within) = cell((p.y + warp) / bedding);
        let key = mix32(bed ^ seed);
        let beds = width / bedding;
        // Beds finer than a pixel average to the rock's mean, rather than
        // striping it with whichever bed each pixel happens to fall on.
        let mean = rock.lerp(strata, 0.5) * 0.99;
        let own = rock.lerp(strata, unit(key)) * (0.86 + 0.26 * unit(mix32(key)));
        let tone = mean.lerp(own, 1.0 - smoothstep(0.3, 1.5, beds));
        let parting = fade(
            1.0 - smoothstep(0.0, 0.07, within.min(1.0 - within)),
            8.0 * beds,
        );
        // Bedded rock breaks along two sets of upright joints near square to
        // each other, and parts along its beds across them, so it stands in
        // blocks.
        let spacing = bedding / 1.3;
        let strike = TAU * unit(mix32(seed ^ 0x7b));
        let skew = 0.15 * (unit(mix32(seed ^ 0x7d)) - 0.5);
        let (first, near_first) = joint_set(p, (strike, spacing), seed ^ 0x7a);
        let (second, near_second) = joint_set(
            p,
            (
                strike + FRAC_PI_2 + skew,
                spacing * (0.8 + 0.5 * unit(mix32(seed ^ 0x7e))),
            ),
            seed ^ 0x7c,
        );
        // A joint opens here and closes there rather than running whole.
        let open = smoothstep(
            -0.2,
            0.35,
            noise2(
                p.x / (2.0 * spacing) + p.y,
                p.z / (2.0 * spacing),
                seed ^ 0x7f,
            ),
        );
        let crack = fade(
            1.0 - smoothstep(0.012, 0.05, near_first.min(near_second)),
            3.0 * beds,
        ) * open;
        let block = 1.0
            + fade(
                0.22 * (unit(hash3(first, second, bed, seed)) - 0.5),
                1.3 * beds,
            );
        // Water running down a steep face leaves it streaked.
        let across = if normal.x.abs() > normal.z.abs() {
            p.z
        } else {
            p.x
        };
        let streaked = smoothstep(
            0.15,
            0.7,
            noise2(across * 1.8 / bedding, p.y * 0.1 / bedding, seed ^ 0x9),
        ) * smoothstep(0.75, 0.25, normal.y);
        let grain = fade(
            noise2(p.x * 9.0 + p.y * 3.0, p.z * 9.0 - p.y * 2.0, seed ^ 0x4d),
            width * 9.0,
        );
        let mut colour = tone
            * block
            * (1.0 - 0.4 * parting)
            * (1.0 - 0.4 * crack)
            * (1.0 - 0.3 * streaked)
            * (0.93 + 0.1 * grain);
        // Lichen spreads over rock that faces the sky.
        let spread = noise2(p.x * 0.8 + p.y * 0.3, p.z * 0.8, seed ^ 0x1c)
            + 0.35
                * fade(
                    noise2(p.x * 6.0, p.z * 6.0 + p.y * 4.0, seed ^ 0x2e),
                    width * 6.0,
                );
        let lichened = smoothstep(0.35, 0.7, spread) * smoothstep(0.1, 0.55, normal.y);
        colour = colour.lerp(lichen, 0.65 * lichened);
        colour
    }
}

impl Ground {
    /// The ground's colour at `spot`.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        let palette = &self.palette;
        let (p, width) = (spot.p, spot.width.max(1e-5));
        let [wet, laid, lane, green, _] = spot.ground;
        let laid = 2.0 * laid - 1.0;
        let upright = spot.normal.y;
        let seed = self.seed;
        let patch = fbm2(
            p.x / 45.0,
            p.z / 45.0,
            seed,
            (octaves_within(width / 45.0).min(4), 0.5, 2.1),
        );
        let mottle = fade(noise2(p.x * 2.3, p.z * 2.3, seed ^ 3), width * 2.3);
        // Worn ground shows its stony subsoil, built-up ground fresh silt; the
        // rock is worked out only where it, its scree or bare stone shows.
        // A river's bed shows what its floods leave there however steep, and
        // a face of earth laid down stands as earth, not rock.
        let subsoil = 0.5 * smoothstep(-0.2, -0.8, laid);
        let talus = self.talus(laid, (upright, wet), patch);
        // Gravel lies no steeper than it rests at; the walls of a channel
        // steeper than that are its bank's earth, or rock.
        let soaked = smoothstep(SOAKED - 0.16, SOAKED, wet);
        let bed = (1.0 - green) * soaked * smoothstep(0.7, 0.82, upright);
        // The steep walls a low water bares in its channel are its banks'
        // earth unless the channel marks bare rock there.
        let walled = soaked * smoothstep(-0.3, -0.1, laid);
        let bare = smoothstep(self.cliff + 0.08, self.cliff - 0.06, upright + 0.05 * patch)
            * (1.0 - bed.max(walled))
            * (1.0 - smoothstep(0.05, 0.18, laid));
        let rock = (subsoil > 0.0 || talus > 0.0 || bare > 0.0).then(|| {
            Rock {
                stone: palette.rock,
                strata: palette.strata,
                lichen: palette.lichen,
                bedding: self.bedding,
                seed,
            }
            .colour(p, spot.normal, width)
        });
        let mut soil = palette
            .earth
            .lerp(palette.silt, smoothstep(0.05, 0.6, laid));
        if let Some(rock) = rock {
            soil = soil.lerp(rock * 0.85, subsoil);
        }
        // A face of earth a river cut shows the beds of loam and gravel its
        // floods laid down.
        let cut = smoothstep(0.78, 0.55, upright) * smoothstep(0.05, 0.18, laid);
        if cut > 0.0 {
            soil = soil.lerp(self.cut_bank(p, width), cut);
        }
        // The bed a river's floods scour, under its water or bared by it: the
        // gravel and sand of the land's own rock, sand where the slack water
        // dropped it, and the bare rock of a ledge.
        if bed > 0.0 {
            let sandy = smoothstep(0.1, 0.45, laid);
            let mut floor = if sandy >= 1.0 {
                self.sand_bed(p, width)
            } else if sandy > 0.0 {
                self.gravel_bed(p, width)
                    .lerp(self.sand_bed(p, width), sandy)
            } else {
                self.gravel_bed(p, width)
            };
            // A river's bare rock is stained dark with the film of algae and
            // silt its water leaves.
            if let Some(rock) = rock {
                let stained = rock.lerp(palette.moss * 0.6, 0.35) * 0.7;
                floor = floor.lerp(stained, smoothstep(-0.25, -0.7, laid));
            }
            soil = soil.lerp(floor, bed);
        }
        let grit = self.grit(p, width);
        let soil = soil * (0.9 + 0.12 * mottle) * grit;
        let mut colour = match rock.filter(|_| talus > 0.0) {
            Some(rock) => soil.lerp(self.scree(p, width, rock), talus),
            None => soil,
        };
        // Grass is painted only off a river's bed: what grows sparsely on the
        // bed stands on its gravel, a blade at a time.
        let grows = green * smoothstep(self.cliff - 0.02, self.cliff + 0.16, upright) * (1.0 - bed);
        let grassed = smoothstep(0.2, 0.75, grows + 0.22 * patch + 0.1 * mottle);
        // Beneath a sward's own blades, its grass is the thatch at their roots.
        let thatch = palette.earth.lerp(palette.dry, 0.45) * (0.7 + 0.2 * mottle) * grit;
        let grass = self
            .grass(wet, patch, mottle)
            .lerp(thatch, smoothstep(0.0, 0.6, spot.thatch));
        colour = colour.lerp(grass, grassed);
        if let Some(floor) = &self.floor {
            let (under, hidden) = floor.shades.at(p.x, p.z);
            let littered = smoothstep(0.15, 0.7, under.max(hidden) + 0.2 * patch);
            if littered > 0.0 {
                colour = colour.lerp(self.litter(floor, p, width, (wet, mottle, grit)), littered);
            }
        }
        if let Some(rock) = rock {
            colour = colour.lerp(rock, bare);
        }
        let beach = smoothstep(
            self.shore + 1.2,
            self.shore + 0.2,
            spot.height + 0.6 * patch,
        );
        colour = colour.lerp(palette.sand * (0.94 + 0.08 * mottle) * grit, beach);
        colour = self.lanes(colour, spot, (lane, grit), (wet, patch, mottle));
        let snowed = self.snowed(
            &Spot {
                normal: Vec3::new(spot.normal.x, upright + 0.08 * mottle, spot.normal.z),
                ..*spot
            },
            patch,
        );
        colour.lerp(palette.snow * (0.95 + 0.05 * mottle), snowed)
    }

    /// A wood's floor: its fallen leaves lying in drifts, each its own shade
    /// between fresh and brown, humus showing in the hollows and where they
    /// lie thin, and carpets of moss, thickest where the ground is damp; on
    /// the ground's own `grit`.
    fn litter(
        &self,
        floor: &Floor,
        p: Vec3,
        width: f64,
        (wet, mottle, grit): (f64, f64, f64),
    ) -> Vec3 {
        let seed = self.seed;
        let drift = smoothstep(
            -0.35,
            0.45,
            fade(noise2(p.x * 0.7, p.z * 0.7, seed ^ 0x71), width * 0.7),
        );
        let fallen = cells2(p.x * 9.0, p.z * 9.0, seed ^ 0x72, 0.9);
        let mean = floor.leaves[0].lerp(floor.leaves[1], 0.5);
        let leaf = mean.lerp(
            floor.leaves[0].lerp(floor.leaves[1], unit(fallen.id)),
            1.0 - smoothstep(0.25, 1.0, width * 9.0),
        );
        let thin = smoothstep(
            0.1,
            0.8,
            fade(noise2(p.x * 2.1, p.z * 2.1, seed ^ 0x73), width * 2.1),
        );
        let hollows = smoothstep(
            0.1,
            0.7,
            fade(noise2(p.x * 0.18, p.z * 0.18, seed ^ 0x75), width * 0.18),
        );
        let ground = leaf.lerp(
            floor.humus,
            (0.55 * (1.0 - drift) * thin).max(0.45 * hollows),
        ) * (0.9 + 0.14 * mottle)
            * grit;
        let carpet = fade(noise2(p.x * 0.15, p.z * 0.15, seed ^ 0x74), width * 0.15)
            + 0.35 * fade(noise2(p.x * 0.6, p.z * 0.6, seed ^ 0x76), width * 0.6);
        let mossy = smoothstep(
            0.55 - floor.moss,
            0.85 - floor.moss,
            0.5 + 0.5 * carpet + 0.35 * wet,
        );
        let moss = self.palette.moss
            * (0.85 + 0.3 * fade(noise2(p.x * 3.3, p.z * 3.3, seed ^ 0x77), width * 3.3))
            * self.grit(p * 1.6, width * 1.6);
        ground.lerp(moss, 0.85 * mossy)
    }

    /// Grains of ground too fine to model, as fine as a footprint `width`
    /// across resolves them: each its own shade and a little darker in the
    /// gaps between them, settling to an even shade as a pixel comes to span
    /// them.
    fn grit(&self, p: Vec3, width: f64) -> f64 {
        let mut shade = 1.0;
        for (scale, salt) in GRIT {
            let detail = width * scale;
            // Coarsest first: once one is too fine to resolve, so is the rest.
            if detail >= 1.0 {
                break;
            }
            let grains = cells2(p.x * scale, p.z * scale, self.seed ^ salt, 0.9);
            let own = 0.85 + 0.3 * unit(grains.id);
            let gap = 1.0 - smoothstep(0.03, 0.14, grains.wall());
            shade *= 1.0 + fade(own * (1.0 - 0.3 * gap) - GRIT_MEAN, detail);
        }
        shade
    }

    /// A river's scoured bed: gravel of the land's own rock, each granule its
    /// own shade between its stone and its darker beds, sand and silt in the
    /// gaps, settling to their mean far off.
    fn gravel_bed(&self, p: Vec3, width: f64) -> Vec3 {
        let palette = &self.palette;
        let granules = cells2(p.x * GRANULES, p.z * GRANULES, self.seed ^ 0x95, 0.9);
        let stone = palette.rock.lerp(palette.strata, unit(granules.id))
            * (0.8 + 0.4 * unit(mix32(granules.id ^ 7)));
        let sand = palette.sand.lerp(palette.silt, 0.5);
        let between = 1.0 - smoothstep(0.04, 0.16, granules.wall());
        let mean = sand.lerp(palette.rock.lerp(palette.strata, 0.5), 0.6);
        // Cobbles too small to lay, each its own shade, shown as far off as
        // a pixel resolves them.
        let cobble = cells2(p.x * COBBLES, p.z * COBBLES, self.seed ^ 0x97, 0.9);
        let shade = 1.0 + fade(0.3 * (unit(cobble.id) - 0.5), width * COBBLES);
        mean.lerp(
            stone.lerp(sand * 0.8, between),
            1.0 - smoothstep(0.25, 1.0, width * GRANULES),
        ) * shade
    }

    /// The face of a bank a river has cut: beds of dark loam and of gravel
    /// laid by its floods, wavering as they run, the gravel's pebbles each
    /// their own shade, settling to their mean far off.
    fn cut_bank(&self, p: Vec3, width: f64) -> Vec3 {
        let palette = &self.palette;
        let loam = palette.earth.lerp(palette.strata, 0.3) * 0.8;
        let wavering = 0.12 * noise2(p.x * 0.5, p.z * 0.5, self.seed ^ 0x98);
        let (bed, within) = cell((p.y + wavering) / BEDS);
        // Not every bed is gravel, nor every gravel bed as coarse.
        let lens = unit(mix32(bed ^ self.seed));
        let gravelly = fade(
            smoothstep(0.65, 0.78, within) * (1.0 - smoothstep(0.86, 0.95, within)),
            width / BEDS,
        ) * smoothstep(0.35, 0.65, lens);
        let pebbles = cells2(
            p.x * PEBBLES + p.z * PEBBLES,
            p.y * PEBBLES,
            self.seed ^ 0x99,
            0.9,
        );
        let pebble = palette.rock.lerp(palette.strata, unit(pebbles.id))
            * (0.75 + 0.4 * unit(mix32(pebbles.id ^ 5)));
        let between = 1.0 - smoothstep(0.05, 0.2, pebbles.wall());
        let gravel = palette.rock.lerp(loam, 0.5).lerp(
            pebble.lerp(loam, between),
            1.0 - smoothstep(0.25, 1.0, width * PEBBLES),
        );
        loam.lerp(gravel, gravelly)
    }

    /// A river's sand: fine and pale, mottled where its grains lie thicker
    /// or thinner, settling to its mean far off.
    fn sand_bed(&self, p: Vec3, width: f64) -> Vec3 {
        let palette = &self.palette;
        let sand = palette.sand.lerp(palette.silt, 0.35);
        let mottled = fade(
            noise2(p.x * MOTTLES, p.z * MOTTLES, self.seed ^ 0x96),
            width * MOTTLES,
        );
        sand * (0.92 + 0.08 * mottled)
    }

    /// How much of the ground is scree: broken rock gathered where the land
    /// was built up at the foot of what stands too steep for soil — on
    /// drained ground, the damp ground by water holding finer stuff.
    fn talus(&self, laid: f64, (upright, wet): (f64, f64), patch: f64) -> f64 {
        let below_cliffs = smoothstep(self.cliff - 0.04, self.cliff + 0.06, upright)
            * smoothstep(self.cliff + 0.3, self.cliff + 0.12, upright);
        below_cliffs
            * smoothstep(0.05, 0.4, laid + 0.15 * patch)
            * (1.0 - smoothstep(DAMP_BANK - 0.13, DAMP_BANK, wet))
    }

    /// Scree: stones of `rock` lying loose in the soil between them.
    fn scree(&self, p: Vec3, width: f64, rock: Vec3) -> Vec3 {
        let stones = cells2(p.x * 3.5, p.z * 3.5, self.seed ^ 0x33, 0.9);
        let stone = rock * (0.75 + 0.45 * unit(stones.id));
        let between = 1.0 - smoothstep(0.02, 0.1, stones.wall());
        let mean = rock * 0.85;
        mean.lerp(
            stone.lerp(self.palette.earth * 0.6, between),
            1.0 - smoothstep(0.1, 0.4, width * 3.5),
        )
    }

    /// Grass: lush and deep where it is wet, parched in patches where it is
    /// not, and moss in the wettest ground.
    fn grass(&self, wet: f64, patch: f64, mottle: f64) -> Vec3 {
        let palette = &self.palette;
        let lush = smoothstep(0.25, 0.85, wet);
        let parched = smoothstep(-0.05, 0.55, patch) * (1.0 - lush);
        let blade =
            palette.grass.lerp(palette.dry, parched) * (0.88 + 0.14 * mottle) * (1.0 - 0.12 * lush);
        blade.lerp(palette.moss, 0.7 * smoothstep(0.6, 0.95, wet + 0.1 * patch))
    }

    /// `colour` with any path trodden into it and any road laid over it, the
    /// path's earth showing the ground's `grit`.
    fn lanes(
        &self,
        colour: Vec3,
        spot: &Spot,
        (lane, grit): (f64, f64),
        (wet, patch, mottle): (f64, f64, f64),
    ) -> Vec3 {
        let (road, footpath) = decode_lane(lane);
        let palette = &self.palette;
        let trodden = palette.earth.lerp(palette.silt, 0.4) * (0.82 + 0.25 * mottle) * grit;
        let mut colour = colour.lerp(trodden, smoothstep(0.2, 0.8, footpath + 0.15 * mottle));
        let Some(painted) = &self.road else {
            return colour.lerp(palette.silt * 0.8, road);
        };
        let p = spot.p;
        if let Some(near) = painted.courses.nearest(p.x, p.z) {
            let half = 0.5 * near.width;
            let share = 1.0
                - smoothstep(
                    half - 0.1 - 0.5 * spot.width,
                    half + 0.1 + 0.5 * spot.width,
                    near.distance,
                );
            if share > 0.0 {
                let surface = match painted.surface {
                    Surface::Tarmac => self.tarmac(p, &near, spot.width),
                    Surface::Gravel => self.gravel(p, &near, spot.width),
                    Surface::Track => self.track(p, &near, (wet, patch, mottle), spot.width),
                };
                colour = colour.lerp(surface, share);
            }
        }
        colour
    }

    /// Tarmac: its stone showing through where wheels wear it, patched and
    /// cracked, a dashed line down its middle and a solid one along each
    /// edge.
    fn tarmac(&self, p: Vec3, near: &Nearest, width: f64) -> Vec3 {
        let seed = self.seed;
        let half = 0.5 * near.width;
        let across = near.distance * near.side;
        let stone = fade(noise2(p.x * 45.0, p.z * 45.0, seed ^ 0x61), width * 45.0);
        let asphalt = Vec3::new(0.052, 0.051, 0.05) * (1.0 + 0.25 * stone);
        let worn = [-0.72, -0.28, 0.28, 0.72]
            .iter()
            .map(|lane| 1.0 - smoothstep(0.15, 0.55, (across - lane * half).abs()))
            .fold(0.0, f64::max);
        let patched = smoothstep(
            0.55,
            0.6,
            noise2(near.along / 7.0, across / 3.0, seed ^ 0x62),
        );
        let cracks = cells2(p.x * 0.9, p.z * 0.9, seed ^ 0x63, 0.9);
        let cracked = fade(1.0 - smoothstep(0.004, 0.02, cracks.wall()), width * 0.9)
            * smoothstep(0.1, 0.5, noise2(p.x * 0.05, p.z * 0.05, seed ^ 0x64));
        let mut colour =
            asphalt * (1.0 + 0.35 * worn) * (1.0 - 0.3 * patched) * (1.0 - 0.5 * cracked);
        let paint = Vec3::splat(0.62)
            * (0.85 + 0.15 * fade(noise2(p.x * 12.0, p.z * 12.0, seed ^ 0x65), width * 12.0));
        let (_, dash) = cell(near.along / 9.0);
        let dashed = 1.0 - smoothstep(0.33 - 0.5 * width / 9.0, 0.33 + 0.5 * width / 9.0, dash);
        let centre = coverage(across.abs(), 0.05, width) * dashed;
        let edges = coverage((across.abs() - (half - 0.3)).abs(), 0.05, width);
        colour = colour.lerp(paint, centre.max(edges) * (1.0 - 0.35 * worn));
        colour
    }

    /// Gravel: loose stones, packed darker where wheels run.
    fn gravel(&self, p: Vec3, near: &Nearest, width: f64) -> Vec3 {
        let half = 0.5 * near.width;
        let across = near.distance * near.side;
        let stones = cells2(p.x * 22.0, p.z * 22.0, self.seed ^ 0x71, 0.95);
        let pebble = fade(unit(stones.id) - 0.5, width * 22.0);
        let loose = Vec3::new(0.24, 0.22, 0.19) * (1.0 + 0.5 * pebble);
        let packed = [-0.45, 0.45]
            .iter()
            .map(|wheel| 1.0 - smoothstep(0.2, 0.6, (across - wheel * half).abs()))
            .fold(0.0, f64::max);
        loose.lerp(self.palette.earth * 0.8, 0.45 * packed)
    }

    /// A track: two ruts of bare earth with grass between them and along
    /// its verges.
    fn track(
        &self,
        p: Vec3,
        near: &Nearest,
        (wet, patch, mottle): (f64, f64, f64),
        width: f64,
    ) -> Vec3 {
        let across = near.distance * near.side;
        let rut = [-TRACK_GAUGE, TRACK_GAUGE]
            .iter()
            .map(|wheel| coverage((across - wheel).abs(), 0.2 + 0.05 * mottle, width))
            .fold(0.0, f64::max);
        let earth = self.palette.earth
            * (0.75 + 0.2 * fade(noise2(p.x * 5.0, p.z * 5.0, self.seed ^ 0x81), width * 5.0));
        self.grass(wet, patch, mottle).lerp(earth, rut)
    }
}

#[cfg(test)]
#[path = "ground_tests.rs"]
mod tests;
