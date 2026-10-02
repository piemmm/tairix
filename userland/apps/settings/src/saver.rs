//! The screensavers' own settables: what each one has to set, as the rows its
//! group on the Screensaver pane draws.
//!
//! A screensaver's options are kept whichever screensaver is chosen, so each
//! row reads and writes its own part of the options and nothing else.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::time::Duration64;
use tairix_wallpaper::{
    CatalogItem, CellSize, CpuUse, Pace, SceneDetail, ScreensaverKind, ScreensaverOptions,
    SettingsKey, SlideOrder, SlideSource, StarDensity, WallpaperCategory, SLIDE_INTERVAL_MAX,
    SLIDE_INTERVAL_MIN,
};

use crate::form::{labelled, pick, set, switch_label, wait_label, with_current, SWITCH};

/// How long the slideshow row offers to show each picture: from the shortest
/// the slideshow shows one to the longest.
const SLIDE_LADDER: [Duration64; 11] = [
    SLIDE_INTERVAL_MIN,
    Duration64::from_secs(10),
    Duration64::from_secs(15),
    Duration64::from_secs(30),
    Duration64::from_secs(60),
    Duration64::from_secs(120),
    Duration64::from_secs(300),
    Duration64::from_secs(600),
    Duration64::from_secs(900),
    Duration64::from_secs(1_800),
    SLIDE_INTERVAL_MAX,
];

/// One screensaver's settable.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SaverOption {
    /// How long the slideshow shows each picture.
    SlideInterval,
    /// The order the slideshow shows its pictures in.
    SlideOrder,
    /// Which of the shipped pictures the slideshow shows.
    SlideSource,
    /// Whether the clock shows the date.
    ClockDate,
    /// Whether the clock names who is signed in, and where.
    ClockIdentity,
    /// Whether the minimal clock shows the date.
    RibbonDate,
    /// How many stars the starfield flies through.
    StarDensity,
    /// Whether the starfield surges into warp.
    StarWarp,
    /// How large the Game of Life's cells are.
    LifeCells,
    /// How fast the Game of Life's generations pass.
    LifeSpeed,
    /// How many of the machine's cores the ray tracer traces on.
    RaytraceCpu,
    /// Whether the ray tracer keeps each finished picture.
    RaytraceSave,
    /// How much each of the ray tracer's scenes sets out.
    RaytraceDetail,
    /// How fast the retro games' flight crosses the grid.
    RetroGamesSpeed,
    /// Whether the System Monitor names the busiest tasks.
    MonitorTasks,
}

impl SaverOption {
    /// What `kind` has to set, in the order its group lists them.
    #[must_use]
    pub const fn of(kind: ScreensaverKind) -> &'static [Self] {
        match kind {
            ScreensaverKind::Blank | ScreensaverKind::Dim => &[],
            ScreensaverKind::Slideshow => {
                &[Self::SlideInterval, Self::SlideOrder, Self::SlideSource]
            }
            ScreensaverKind::Clock => &[Self::ClockDate, Self::ClockIdentity],
            ScreensaverKind::Ribbon => &[Self::RibbonDate],
            ScreensaverKind::Starfield => &[Self::StarDensity, Self::StarWarp],
            ScreensaverKind::Life => &[Self::LifeCells, Self::LifeSpeed],
            ScreensaverKind::Raytrace => {
                &[Self::RaytraceCpu, Self::RaytraceSave, Self::RaytraceDetail]
            }
            ScreensaverKind::RetroGames => &[Self::RetroGamesSpeed],
            ScreensaverKind::SystemMonitor => &[Self::MonitorTasks],
        }
    }

    /// The registry key this option writes.
    #[must_use]
    pub const fn key(self) -> SettingsKey {
        match self {
            Self::SlideInterval => SettingsKey::SlideInterval,
            Self::SlideOrder => SettingsKey::SlideOrder,
            Self::SlideSource => SettingsKey::SlideCategory,
            Self::ClockDate => SettingsKey::ClockDate,
            Self::ClockIdentity => SettingsKey::ClockIdentity,
            Self::RibbonDate => SettingsKey::RibbonDate,
            Self::StarDensity => SettingsKey::StarDensity,
            Self::StarWarp => SettingsKey::StarWarp,
            Self::LifeCells => SettingsKey::LifeCells,
            Self::LifeSpeed => SettingsKey::LifeSpeed,
            Self::RaytraceCpu => SettingsKey::RaytraceCpu,
            Self::RaytraceSave => SettingsKey::RaytraceSave,
            Self::RaytraceDetail => SettingsKey::RaytraceDetail,
            Self::RetroGamesSpeed => SettingsKey::RetroGamesSpeed,
            Self::MonitorTasks => SettingsKey::MonitorTasks,
        }
    }

    /// The row's leading label, which is also its search term.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SlideInterval => "Change every",
            Self::SlideOrder => "Order",
            Self::SlideSource => "Pictures",
            Self::ClockDate | Self::RibbonDate => "Show the date",
            Self::ClockIdentity => "Show who is signed in",
            Self::StarDensity => "Stars",
            Self::StarWarp => "Warp",
            Self::LifeCells => "Cell size",
            Self::LifeSpeed | Self::RetroGamesSpeed => "Speed",
            Self::RaytraceCpu => "Processor use",
            Self::RaytraceSave => "Save pictures",
            Self::RaytraceDetail => "Detail",
            Self::MonitorTasks => "Name the busiest tasks",
        }
    }

    /// The sentence beneath the label: what choosing this actually does.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::SlideInterval => "How long each picture is shown before the next.",
            Self::SlideOrder => {
                "In order goes through the pictures as they are listed. Shuffled shows every one \
                 once before any is shown again."
            }
            Self::SlideSource => "Which of the shipped pictures are shown.",
            Self::ClockDate | Self::RibbonDate => "Whether the date is shown beneath the time.",
            Self::ClockIdentity => {
                "Whether the account signed in and this machine's name are shown beneath it."
            }
            Self::StarDensity => "How many stars the flight passes.",
            Self::StarWarp => {
                "Whether the flight surges into warp now and then, rather than only cruising."
            }
            Self::LifeCells => "How large each cell is drawn. Smaller cells make a larger board.",
            Self::LifeSpeed => "How fast one generation follows the last.",
            Self::RaytraceCpu => {
                "Idle time traces on one core and leaves the rest of the machine free. \
                 Performance traces on every core, so each picture is finished sooner."
            }
            Self::RaytraceSave => {
                "Whether each finished picture is kept as a PNG in Documents, under Pictures \
                 and then Raytracing."
            }
            Self::RaytraceDetail => {
                "Simple sets each scene out plainly, in a fraction of the memory. Maximum \
                 realism stands its woods further out and gathers its light more finely, in \
                 many times the memory; while that is not free, scenes are set out simply."
            }
            Self::RetroGamesSpeed => "How fast the flight crosses the grid towards the sun.",
            Self::MonitorTasks => {
                "Whether the programs using the most processor time are named. Anyone who can \
                 see the screen can read them, even while it is locked."
            }
        }
    }

    /// The choices this option offers, in the order they are listed, and
    /// which of them `options` holds; `catalog` is the shipped pictures, whose
    /// categories the slideshow may be narrowed to.
    pub(crate) fn choices(
        self,
        options: &ScreensaverOptions,
        catalog: &[CatalogItem],
    ) -> (Vec<String>, usize) {
        match self {
            Self::SlideInterval => labelled(
                &slide_ladder(options.slideshow.interval),
                options.slideshow.interval,
                span_label,
            ),
            Self::SlideOrder => pick(&SlideOrder::ALL, options.slideshow.order, slide_order_label),
            Self::SlideSource => {
                let current = &options.slideshow.source;
                let ladder = source_ladder(current, catalog);
                let at = ladder.iter().position(|held| held == current).unwrap_or(0);
                (ladder.iter().map(source_label).collect(), at)
            }
            Self::ClockDate => pick(&SWITCH, options.clock.date, switch_label),
            Self::ClockIdentity => pick(&SWITCH, options.clock.identity, switch_label),
            Self::RibbonDate => pick(&SWITCH, options.ribbon.date, switch_label),
            Self::StarDensity => pick(
                &StarDensity::ALL,
                options.starfield.stars,
                star_density_label,
            ),
            Self::StarWarp => pick(&SWITCH, options.starfield.warp, switch_label),
            Self::LifeCells => pick(&CellSize::ALL, options.life.cells, cell_size_label),
            Self::LifeSpeed => pick(&Pace::ALL, options.life.speed, pace_label),
            Self::RaytraceCpu => pick(&CpuUse::ALL, options.raytrace.cpu, cpu_use_label),
            Self::RaytraceSave => pick(&SWITCH, options.raytrace.save, switch_label),
            Self::RaytraceDetail => pick(&SceneDetail::ALL, options.raytrace.detail, detail_label),
            Self::RetroGamesSpeed => pick(&Pace::ALL, options.retro_games.speed, pace_label),
            Self::MonitorTasks => pick(&SWITCH, options.system_monitor.tasks, switch_label),
        }
    }

    /// Write the choice at `index` onto `options`, answering whether it named
    /// one this option offers.
    ///
    /// Fails closed: an index outside the list this very surface built
    /// changes nothing.
    pub(crate) fn adopt(
        self,
        index: usize,
        options: &mut ScreensaverOptions,
        catalog: &[CatalogItem],
    ) -> bool {
        match self {
            Self::SlideInterval => set(
                &slide_ladder(options.slideshow.interval),
                index,
                &mut options.slideshow.interval,
            ),
            Self::SlideOrder => set(&SlideOrder::ALL, index, &mut options.slideshow.order),
            Self::SlideSource => {
                let chosen = source_ladder(&options.slideshow.source, catalog)
                    .into_iter()
                    .nth(index);
                match chosen {
                    Some(source) => {
                        options.slideshow.source = source;
                        true
                    }
                    None => false,
                }
            }
            Self::ClockDate => set(&SWITCH, index, &mut options.clock.date),
            Self::ClockIdentity => set(&SWITCH, index, &mut options.clock.identity),
            Self::RibbonDate => set(&SWITCH, index, &mut options.ribbon.date),
            Self::StarDensity => set(&StarDensity::ALL, index, &mut options.starfield.stars),
            Self::StarWarp => set(&SWITCH, index, &mut options.starfield.warp),
            Self::LifeCells => set(&CellSize::ALL, index, &mut options.life.cells),
            Self::LifeSpeed => set(&Pace::ALL, index, &mut options.life.speed),
            Self::RaytraceCpu => set(&CpuUse::ALL, index, &mut options.raytrace.cpu),
            Self::RaytraceSave => set(&SWITCH, index, &mut options.raytrace.save),
            Self::RaytraceDetail => set(&SceneDetail::ALL, index, &mut options.raytrace.detail),
            Self::RetroGamesSpeed => set(&Pace::ALL, index, &mut options.retro_games.speed),
            Self::MonitorTasks => set(&SWITCH, index, &mut options.system_monitor.tasks),
        }
    }
}

/// The intervals the slideshow row offers a desktop currently at `current`.
fn slide_ladder(current: Duration64) -> Vec<Duration64> {
    with_current(SLIDE_LADDER.to_vec(), current, |span| *span)
}

/// A span of whole seconds as a reader says it: in seconds below a minute, in
/// minutes and hours as [`wait_label`] says them above.
fn span_label(span: Duration64) -> String {
    let seconds = span.saturating_total_nanos() / 1_000_000_000;
    let minutes = |count: u64| wait_label(u16::try_from(count).unwrap_or(u16::MAX));
    match (seconds / 60, seconds % 60) {
        (0, 1) => String::from("1 second"),
        (0, seconds) => alloc::format!("{seconds} seconds"),
        (whole, 0) => minutes(whole),
        (whole, rest) => alloc::format!("{} {rest} seconds", minutes(whole)),
    }
}

const fn slide_order_label(order: SlideOrder) -> &'static str {
    match order {
        SlideOrder::Sequential => "In order",
        SlideOrder::Shuffled => "Shuffled",
    }
}

/// The pictures the slideshow row offers a desktop currently showing
/// `current`: every category's first, then each category the catalog files a
/// picture under, in its order, and `current` itself when an update has taken
/// its category out of the catalog — still what the document says, so still
/// offered.
fn source_ladder(current: &SlideSource, catalog: &[CatalogItem]) -> Vec<SlideSource> {
    let mut ladder = Vec::new();
    ladder.push(SlideSource::Every);
    for item in catalog {
        let listed = ladder
            .iter()
            .any(|held| held.as_str() == item.category.as_str());
        if let Some(category) = WallpaperCategory::new(&item.category).filter(|_| !listed) {
            ladder.push(SlideSource::Category(category));
        }
    }
    if !ladder.contains(current) {
        ladder.push(current.clone());
    }
    ladder
}

fn source_label(source: &SlideSource) -> String {
    match source {
        SlideSource::Every => String::from("Every category"),
        SlideSource::Category(name) => name.as_str().to_string(),
    }
}

const fn star_density_label(density: StarDensity) -> &'static str {
    match density {
        StarDensity::Sparse => "Sparse",
        StarDensity::Normal => "Normal",
        StarDensity::Dense => "Dense",
    }
}

const fn cell_size_label(size: CellSize) -> &'static str {
    match size {
        CellSize::Small => "Small",
        CellSize::Medium => "Medium",
        CellSize::Large => "Large",
    }
}

const fn pace_label(pace: Pace) -> &'static str {
    match pace {
        Pace::Slow => "Slow",
        Pace::Normal => "Normal",
        Pace::Fast => "Fast",
    }
}

const fn cpu_use_label(cpu: CpuUse) -> &'static str {
    match cpu {
        CpuUse::Idle => "Idle time",
        CpuUse::Performance => "Performance",
    }
}

const fn detail_label(detail: SceneDetail) -> &'static str {
    match detail {
        SceneDetail::Simple => "Simple",
        SceneDetail::Maximum => "Maximum realism",
    }
}
