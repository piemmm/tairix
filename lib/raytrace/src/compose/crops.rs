//! What a farmed land's fields grow in its sward in place of its wild grass:
//! each crop at each stage the season stands it at, and a meadow's aftermath
//! once it is cut, as a kind of grass and its colours; and what each looks
//! like too far off to make out a leaf, which the ground paints instead.

use tairix_countryside::usage::Crop;

use super::fields::maize;
use super::rgb;
use crate::farmed::{self, Grown, Stage, CODES};
use crate::grass::{GrassKind, Habit, Head, CROP_KINDS, WILD_KINDS};
use crate::ground::Far;
use crate::pigment::Blades;
use crate::tree::Season;

/// What a farmed land's fields grow in its sward in a season: each kind and
/// its colours, and what it looks like from afar; and which of them each
/// growth's byte grows as, by its place among the sward's kinds, nought for
/// one growing none.
#[derive(Copy, Clone, Debug)]
pub(super) struct Sowing {
    pub(super) kinds: [Option<(GrassKind, Blades)>; CROP_KINDS],
    pub(super) far: [Option<Far>; CROP_KINDS],
    pub(super) by_growth: [u8; CODES],
}

impl Sowing {
    /// A sward on land nobody farms.
    pub(super) const NONE: Self = Self {
        kinds: [None; CROP_KINDS],
        far: [None; CROP_KINDS],
        by_growth: [0; CODES],
    };
}

/// A growth as its shoots stand and the colours they are: how many to a
/// square metre; its kind, whose thickness is then those shoots' share of
/// its sward's; its leaves' two greens, the colour their tips dry to, and
/// its heads'; how much of its ground it hides seen from above, and how much
/// of that its heads make; and the share of the ground between it that its
/// own cut straw or leaves lie over.
struct Look {
    shoots: f64,
    kind: GrassKind,
    leaves: [u32; 2],
    tip: u32,
    head: u32,
    cover: f64,
    crown: f64,
    strewn: f64,
}

/// A cereal's young shoots, the form every crop's look is drawn from.
const SHOOTS: GrassKind = GrassKind {
    height: (0.06, 0.18),
    width: 0.005,
    lean: 0.45,
    droop: 0.3,
    thickness: 1.0,
    tufted: 0.0,
    stems: 0.0,
    head: None,
    nod: 0.0,
    habit: Habit::Open,
    share: 1.0,
    rows: farmed::row_spacing(Crop::Wheat),
    stood: None,
};

/// What `grown` looks like in the sward; `None` for a growth showing nothing
/// above its tilth.
fn look(grown: Grown) -> Option<Look> {
    let (crop, stage) = match grown {
        Grown::Sown(crop, stage) => (crop, stage),
        Grown::Hayed => return Some(AFTERMATH),
        _ => return None,
    };
    let rows = farmed::row_spacing(crop);
    let drilled = |look: Look| Look {
        kind: GrassKind { rows, ..look.kind },
        ..look
    };
    let cereal = |[wheat, barley, oats]: [Look; 3]| match crop {
        Crop::Barley => barley,
        Crop::Oats => oats,
        _ => wheat,
    };
    let look = match (crop, stage) {
        (_, Stage::Drilled | Stage::Ploughed) => return None,
        (Crop::Wheat | Crop::Barley | Crop::Oats, Stage::Shooting) => cereal(YOUNG),
        (Crop::Wheat | Crop::Barley | Crop::Oats, Stage::Green | Stage::Flowering) => cereal(GROWN),
        (Crop::Wheat | Crop::Barley | Crop::Oats, Stage::Ripe) => cereal(RIPE),
        (Crop::Wheat | Crop::Barley | Crop::Oats, Stage::Stubble) => cereal(STUBBLE),
        (Crop::Maize, Stage::Ripe) => MAIZE_RIPE,
        (Crop::Maize, Stage::Stubble) => MAIZE_STUBBLE,
        (Crop::Maize, _) => MAIZE,
        (Crop::Rapeseed, Stage::Shooting) => RAPE_YOUNG,
        (Crop::Rapeseed, Stage::Green) => RAPE_WINTER,
        (Crop::Rapeseed, Stage::Flowering) => RAPE_FLOWERING,
        (Crop::Rapeseed, Stage::Ripe) => RAPE_PODS,
        (Crop::Rapeseed, Stage::Stubble) => RAPE_STUBBLE,
        // A cut ley is mown grass, not straw.
        (Crop::Ley, Stage::Stubble) => LEY_CUT,
        (Crop::Ley, _) => LEY,
    };
    Some(drilled(look))
}

/// Cereals' young shoots in their rows: wheat's, barley's a yellower green,
/// oats' a bluer.
const WHEAT_YOUNG: Look = Look {
    shoots: 700.0,
    kind: SHOOTS,
    leaves: [0x4A_7E_2C, 0x56_8A_34],
    tip: 0x7A_96_44,
    head: 0,
    cover: 0.35,
    crown: 0.0,
    strewn: 0.0,
};
const YOUNG: [Look; 3] = [
    WHEAT_YOUNG,
    Look {
        leaves: [0x5A_86_2E, 0x66_92_36],
        tip: 0x86_9C_48,
        ..WHEAT_YOUNG
    },
    Look {
        leaves: [0x46_78_3A, 0x52_84_42],
        tip: 0x76_90_4E,
        ..WHEAT_YOUNG
    },
];

/// Cereals grown to their height, their ears out and still green.
const WHEAT_GROWN: Look = Look {
    shoots: 1500.0,
    kind: GrassKind {
        height: (0.3, 0.55),
        width: 0.011,
        lean: 0.35,
        droop: 0.35,
        stems: 0.22,
        head: Some(Head::Ear),
        ..SHOOTS
    },
    leaves: [0x3C_6C_2A, 0x48_78_30],
    tip: 0x6A_8A_3C,
    head: 0x7A_96_48,
    cover: 0.96,
    crown: 0.2,
    strewn: 0.0,
};
const GROWN: [Look; 3] = [
    WHEAT_GROWN,
    Look {
        kind: GrassKind {
            head: Some(Head::Awned),
            nod: 0.25,
            ..WHEAT_GROWN.kind
        },
        leaves: [0x52_80_30, 0x5E_8C_36],
        tip: 0x88_9C_4A,
        head: 0x8C_A0_52,
        ..WHEAT_GROWN
    },
    Look {
        kind: GrassKind {
            height: (0.35, 0.6),
            head: Some(Head::Plume),
            nod: 0.3,
            ..WHEAT_GROWN.kind
        },
        leaves: [0x40_70_3C, 0x4C_7C_44],
        tip: 0x76_8E_4E,
        head: 0x80_98_5A,
        ..WHEAT_GROWN
    },
];

/// Cereals ripe: their stems gold, their dried leaves curled low; wheat's
/// ears upright, barley's hanging by its awns, oats' panicles nodding.
const WHEAT_RIPE: Look = Look {
    shoots: 1200.0,
    kind: GrassKind {
        height: (0.42, 0.6),
        width: 0.009,
        lean: 0.3,
        droop: 0.6,
        stems: 0.7,
        head: Some(Head::Ear),
        nod: 0.12,
        ..SHOOTS
    },
    leaves: [0xB4_94_4E, 0xA4_84_44],
    tip: 0xC8_B0_74,
    head: 0xD2_AE_5E,
    cover: 0.94,
    crown: 0.55,
    strewn: 0.0,
};
const RIPE: [Look; 3] = [
    WHEAT_RIPE,
    Look {
        kind: GrassKind {
            height: (0.38, 0.5),
            head: Some(Head::Awned),
            nod: 0.85,
            ..WHEAT_RIPE.kind
        },
        leaves: [0xC4_AC_6C, 0xB4_9C_5C],
        tip: 0xD8_C8_90,
        head: 0xD8_C0_80,
        crown: 0.5,
        ..WHEAT_RIPE
    },
    Look {
        kind: GrassKind {
            height: (0.48, 0.65),
            stems: 0.6,
            head: Some(Head::Plume),
            nod: 0.45,
            ..WHEAT_RIPE.kind
        },
        leaves: [0xBC_AC_70, 0xAC_9C_62],
        tip: 0xD4_C8_98,
        head: 0xDC_CC_A0,
        crown: 0.45,
        ..WHEAT_RIPE
    },
];

/// A cut cereal's stubble: short straws standing in their rows, cut straw
/// lying between.
const WHEAT_STUBBLE: Look = Look {
    shoots: 900.0,
    kind: GrassKind {
        height: (0.07, 0.11),
        width: 0.012,
        lean: 0.05,
        droop: 0.0,
        stems: 1.0,
        ..SHOOTS
    },
    leaves: [0xCC_B4_78, 0xBC_A4_68],
    tip: 0xD8_C8_90,
    head: 0,
    cover: 0.4,
    crown: 0.0,
    strewn: 0.45,
};
const STUBBLE: [Look; 3] = [
    WHEAT_STUBBLE,
    Look {
        leaves: [0xD4_C0_86, 0xC4_B0_76],
        tip: 0xE0_D4_A0,
        ..WHEAT_STUBBLE
    },
    Look {
        leaves: [0xD0_C2_8C, 0xC0_B2_7C],
        tip: 0xDC_D0_A4,
        ..WHEAT_STUBBLE
    },
];

/// Maize in summer: tall, broad-leaved, a tassel on each stem, as green as
/// its plants nearer the eye.
const MAIZE: Look = Look {
    shoots: 110.0,
    kind: GrassKind {
        height: (0.9, 1.5),
        width: 0.075,
        lean: 0.55,
        droop: 0.45,
        stems: 0.1,
        head: Some(Head::Plume),
        ..SHOOTS
    },
    leaves: maize::GREEN_COLOURS.greens,
    tip: 0x6A_8C_3C,
    head: maize::GREEN_COLOURS.tassel,
    cover: 0.97,
    crown: 0.08,
    strewn: 0.0,
};

/// Maize ripe in autumn, its leaves dried and hanging.
const MAIZE_RIPE: Look = Look {
    kind: GrassKind {
        droop: 0.6,
        ..MAIZE.kind
    },
    leaves: maize::RIPE_COLOURS.greens,
    tip: 0xC0_AC_78,
    head: maize::RIPE_COLOURS.tassel,
    cover: 0.9,
    ..MAIZE
};

/// Maize cut: its stalks' stubs in their wide rows, its shredded leaves
/// lying between.
const MAIZE_STUBBLE: Look = Look {
    shoots: 12.0,
    kind: GrassKind {
        height: (0.15, 0.25),
        width: 0.055,
        lean: 0.05,
        droop: 0.0,
        stems: 1.0,
        head: None,
        ..MAIZE.kind
    },
    leaves: [0xB4_A0_70, 0xA4_90_62],
    tip: 0xC4_B4_88,
    head: 0,
    cover: 0.06,
    crown: 0.0,
    strewn: 0.35,
};

/// Rape sown in autumn: low rosettes of broad, blue-green leaves.
const RAPE_YOUNG: Look = Look {
    shoots: 220.0,
    kind: GrassKind {
        height: (0.06, 0.16),
        width: 0.03,
        lean: 0.75,
        droop: 0.45,
        ..SHOOTS
    },
    leaves: [0x3E_68_3C, 0x4A_74_46],
    tip: 0x5A_7C_4A,
    head: 0,
    cover: 0.45,
    crown: 0.0,
    strewn: 0.0,
};

/// Rape through the winter: its rosettes grown broader and darker.
const RAPE_WINTER: Look = Look {
    shoots: 260.0,
    kind: GrassKind {
        height: (0.1, 0.24),
        width: 0.035,
        lean: 0.7,
        droop: 0.4,
        ..RAPE_YOUNG.kind
    },
    leaves: [0x38_5E_38, 0x44_6A_42],
    tip: 0x54_74_48,
    cover: 0.6,
    ..RAPE_YOUNG
};

/// Rape in flower: its racemes a sheet of yellow over its leaves.
const RAPE_FLOWERING: Look = Look {
    shoots: 500.0,
    kind: GrassKind {
        height: (0.45, 0.8),
        width: 0.03,
        lean: 0.45,
        droop: 0.3,
        stems: 0.45,
        head: Some(Head::Plume),
        ..SHOOTS
    },
    leaves: [0x46_6E_36, 0x52_7A_3E],
    tip: 0x7A_90_44,
    head: 0xF2_D2_1E,
    cover: 0.97,
    crown: 0.8,
    strewn: 0.0,
};

/// Rape in pod, its leaves fallen and its stems browning.
const RAPE_PODS: Look = Look {
    shoots: 420.0,
    kind: GrassKind {
        width: 0.01,
        lean: 0.4,
        stems: 0.7,
        nod: 0.2,
        ..RAPE_FLOWERING.kind
    },
    leaves: [0x8A_80_4A, 0x7A_70_40],
    tip: 0x9A_8A_54,
    head: 0x8E_86_4C,
    cover: 0.9,
    crown: 0.6,
    strewn: 0.0,
};

/// Rape cut: its tall, woody stubble.
const RAPE_STUBBLE: Look = Look {
    shoots: 300.0,
    kind: GrassKind {
        height: (0.12, 0.2),
        width: 0.02,
        lean: 0.1,
        droop: 0.0,
        stems: 1.0,
        head: None,
        nod: 0.0,
        ..RAPE_FLOWERING.kind
    },
    leaves: [0xA8_96_68, 0x98_86_5A],
    tip: 0xB8_A8_7C,
    head: 0,
    cover: 0.35,
    crown: 0.0,
    strewn: 0.3,
};

/// A ley grown: an even sward of sown grass.
const LEY: Look = Look {
    shoots: 1600.0,
    kind: GrassKind {
        height: (0.12, 0.35),
        width: 0.006,
        lean: 0.4,
        droop: 0.3,
        tufted: 0.1,
        stems: 0.05,
        head: Some(Head::Spike),
        ..SHOOTS
    },
    leaves: [0x3A_74_26, 0x46_80_2E],
    tip: 0x6A_94_3E,
    head: 0x8A_A0_50,
    cover: 0.98,
    crown: 0.03,
    strewn: 0.0,
};

/// A ley cut for silage: short, fresh, mown grass.
const LEY_CUT: Look = Look {
    kind: GrassKind {
        height: (0.03, 0.07),
        width: 0.005,
        lean: 0.3,
        droop: 0.1,
        tufted: 0.0,
        stems: 0.0,
        head: None,
        ..LEY.kind
    },
    leaves: [0x58_84_30, 0x64_8E_38],
    tip: 0x9A_A0_58,
    head: 0,
    cover: 0.9,
    crown: 0.0,
    ..LEY
};

/// A hay meadow's aftermath once it is cut: short, yellowing grass, sown in
/// no rows.
const AFTERMATH: Look = Look {
    shoots: 1400.0,
    kind: GrassKind {
        height: (0.03, 0.08),
        width: 0.004,
        lean: 0.3,
        droop: 0.15,
        tufted: 0.2,
        rows: 0.0,
        ..SHOOTS
    },
    leaves: [0x6E_8A_3A, 0x7A_94_42],
    tip: 0xB0_A8_68,
    head: 0,
    cover: 0.85,
    crown: 0.0,
    strewn: 0.0,
};

impl Look {
    /// This look's kind, its thickness set to stand its shoots in a sward
    /// whose shoots stand `shoots` a square metre, and its colours.
    fn grown(&self, shoots: f64) -> (GrassKind, Blades) {
        let kind = GrassKind {
            thickness: self.shoots / shoots.max(1.0),
            ..self.kind
        };
        let blades = Blades {
            leaves: self.leaves.map(rgb),
            tip: rgb(self.tip),
            head: rgb(self.head),
        };
        (kind, blades)
    }

    /// What it looks like too far off to make out a leaf, coloured as
    /// `blades` are.
    fn far(&self, blades: &Blades) -> Far {
        let leaf = (blades.leaves[0] + blades.leaves[1]) * 0.4 + blades.tip * 0.2;
        Far {
            colour: leaf.lerp(blades.head, self.crown),
            cover: self.cover,
            strewn: (blades.tip, self.strewn),
        }
    }
}

/// What a farmed land's fields grow in its sward in `season`, a sward whose
/// shoots stand `shoots` a square metre where it grows best, a crop standing
/// as plants about the eye giving way to it over `stood`. Each growth the
/// season holds takes a kind of its own, as many as the sward holds.
pub(super) fn sowing(season: Season, shoots: f64, stood: (f64, f64)) -> Sowing {
    let mut sowing = Sowing::NONE;
    let mut slots = sowing
        .kinds
        .iter_mut()
        .zip(sowing.far.iter_mut())
        .zip(WILD_KINDS..);
    for grown in farmed::growths(season) {
        let Some(look) = look(grown) else {
            continue;
        };
        let Some(((slot, far), index)) = slots.next() else {
            break;
        };
        let (mut kind, blades) = look.grown(shoots);
        if maize::stands(grown) {
            kind.stood = Some(stood);
        }
        let code = usize::from(grown.code());
        if let (Some(by_growth), Ok(index)) = (sowing.by_growth.get_mut(code), u8::try_from(index))
        {
            *slot = Some((kind, blades));
            *far = Some(look.far(&blades));
            *by_growth = index;
        }
    }
    sowing
}

#[cfg(test)]
#[path = "crops_tests.rs"]
mod tests;
