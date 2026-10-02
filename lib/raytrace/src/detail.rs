//! How much a scene sets out: one generator at two densities.
//!
//! A [`Detail`] picks a table of densities — how many objects a scene may
//! hold, how many trees each kind of wood may stand, how its radiosity
//! records and its caustics are laid — that composing and gathering read,
//! and nothing else.
//! The land, the eye, the hour and the weather are drawn apart from all of
//! it, so a seed shows the same place at either.

/// How much a scene sets out.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Detail {
    /// Every setting, plainer: woods reaching less far and radiosity records
    /// of fewer rays laid more sparingly, within about 384 MiB at the scene's
    /// peak.
    Simple,
    /// All the realism the budget buys, within 2 GiB at the scene's peak.
    Maximum,
}

impl Detail {
    /// Both details, plainest first.
    pub const ALL: [Self; 2] = [Self::Simple, Self::Maximum];

    /// The most a scene at this detail holds at its peak, in bytes: what a
    /// caller weighs before asking a machine to spare it.
    #[must_use]
    pub const fn peak(self) -> u64 {
        match self {
            Self::Simple => 384 << 20,
            Self::Maximum => 2 << 30,
        }
    }

    /// Every density this detail sets.
    pub(crate) const fn densities(self) -> &'static Densities {
        match self {
            Self::Simple => &SIMPLE,
            Self::Maximum => &MAXIMUM,
        }
    }
}

/// The densities a detail sets.
#[derive(Debug)]
pub(crate) struct Densities {
    /// The most objects a scene holds: a forest's trees, understory and
    /// deadwood among them.
    pub(crate) objects: usize,
    pub(crate) woods: Woods,
    pub(crate) records: Records,
    pub(crate) focus: Focus,
}

/// The most trees each wood a setting asks for may stand, which also bounds
/// how far about the eye it is sown.
#[derive(Debug)]
pub(crate) struct Woods {
    /// About a building, a sculpture or an aqueduct.
    pub(crate) backdrop: u32,
    pub(crate) meadow: u32,
    pub(crate) forest: u32,
    pub(crate) winter: u32,
    pub(crate) canyon: u32,
    pub(crate) valley: u32,
    /// A rocky desert's saguaros, and its scrub.
    pub(crate) cacti: u32,
    pub(crate) scrub: u32,
}

/// How a scene's radiosity records are laid.
#[derive(Debug)]
pub(crate) struct Records {
    /// The rows of equal cosine and the columns of azimuth a record's
    /// hemisphere is cut into, one ray a cell.
    pub(crate) rows: usize,
    pub(crate) columns: usize,
    /// How many rows of sites the finest grid records are laid over has
    /// across the picture's height: a record holds for no less than a share
    /// of the picture as fine.
    pub(crate) finest: u32,
    /// The most records a square of the picture as wide as it is high holds,
    /// and, in a small picture, the pixels there must be for each.
    pub(crate) a_square: u64,
    pub(crate) pixels_each: u64,
}

/// How a scene's caustics are laid.
#[derive(Debug)]
pub(crate) struct Focus {
    /// How many pixels apart the survey of the picture looks.
    pub(crate) stride: u32,
    /// A tile's cell, as a share of the footprint the eye sees a point its
    /// beams light over.
    pub(crate) texel: f64,
    /// The most cells a scene's tiles hold.
    pub(crate) cells: usize,
}

impl Records {
    /// The rays a record's hemisphere is gathered through.
    pub(crate) const fn cells(&self) -> usize {
        self.rows * self.columns
    }
}

const SIMPLE: Densities = Densities {
    objects: 1 << 17,
    woods: Woods {
        backdrop: 20_000,
        meadow: 20_000,
        forest: 90_000,
        winter: 40_000,
        canyon: 4000,
        valley: 20_000,
        cacti: 600,
        scrub: 900,
    },
    records: Records {
        rows: 8,
        columns: 32,
        finest: 240,
        a_square: 1600,
        pixels_each: 64,
    },
    focus: Focus {
        stride: 4,
        texel: 1.0,
        cells: 1 << 21,
    },
};

const MAXIMUM: Densities = Densities {
    objects: 1 << 19,
    woods: Woods {
        backdrop: 120_000,
        meadow: 120_000,
        forest: 270_000,
        winter: 160_000,
        canyon: 40_000,
        valley: 120_000,
        cacti: 6000,
        scrub: 9000,
    },
    records: Records {
        rows: 16,
        columns: 64,
        finest: 480,
        a_square: 6400,
        pixels_each: 16,
    },
    focus: Focus {
        stride: 2,
        texel: 0.5,
        cells: 1 << 23,
    },
};
