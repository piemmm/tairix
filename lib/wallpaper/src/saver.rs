//! What each screensaver scene draws, and the picture that previews each one.
//!
//! A kind with something to set keeps its options here, and every kind's
//! options are kept whichever is chosen, so choosing another screensaver and
//! coming back restores what was set. Each option is a closed value set or a
//! bounded number, read and rendered on the registry's own discipline
//! ([`crate::settings`]).
//!
//! Every kind also ships one preview picture under [`SCREENSAVER_PREVIEW_STORE`],
//! named by its document spelling, so a chooser can show what it would pick.

use alloc::format;
use alloc::string::{String, ToString};

use tairix_abi::desktop::ScreensaverKind;
use tairix_abi::time::Duration64;

use crate::catalog::is_wallpaper_category_name;
use crate::input::parse_decimal;

/// Where the OS ships one preview picture per screensaver kind.
pub const SCREENSAVER_PREVIEW_STORE: &str = "/System/Graphics/Screensavers";

/// The file-name suffix of a screensaver's preview picture.
const PREVIEW_SUFFIX: &str = ".png";

/// Largest screensaver preview any consumer reads, in bytes.
///
/// A fixed validation bound on untrusted input: a preview is a small picture
/// drawn in a chooser, so this bounds the work one can demand before a byte is
/// decoded, and the image build refuses a shipped preview over it.
pub const MAX_SCREENSAVER_PREVIEW_BYTES: usize = 1024 * 1024;

/// A screensaver's preview picture's file name: its document spelling, as a
/// PNG.
#[must_use]
pub fn preview_file(kind: ScreensaverKind) -> String {
    format!("{}{PREVIEW_SUFFIX}", kind.as_str())
}

/// A screensaver's preview picture's absolute path.
#[must_use]
pub fn preview_path(kind: ScreensaverKind) -> String {
    format!("{SCREENSAVER_PREVIEW_STORE}/{}", preview_file(kind))
}

/// The screensaver a shipped preview file name previews, or `None` for a name
/// no kind asks for.
///
/// The image build applies this to every file it plants, so a preview nothing
/// would ever show fails the build rather than shipping.
#[must_use]
pub fn preview_kind(file: &str) -> Option<ScreensaverKind> {
    let stem = file.strip_suffix(PREVIEW_SUFFIX)?;
    ScreensaverKind::from_value(stem)
}

/// A wallpaper category's name as a setting holds it: a legal category
/// directory name, within the ABI's file-name bound.
///
/// A name, not a promise: a store an update changed may no longer hold the
/// category a setting names, and its consumer decides what that means.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WallpaperCategory(String);

impl WallpaperCategory {
    /// The category named `name`, or `None` for a name no category directory
    /// could carry.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        (name.len() <= tairix_abi::FS_NAME_MAX && is_wallpaper_category_name(name))
            .then(|| Self(name.to_string()))
    }

    /// The category's name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The order a slideshow shows its pictures in.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum SlideOrder {
    /// The store's own order, round and round.
    #[default]
    Sequential,
    /// Every picture once in a shuffled order before any repeats.
    Shuffled,
}

impl SlideOrder {
    /// Every order, in the order a chooser offers them.
    pub const ALL: [Self; 2] = [Self::Sequential, Self::Shuffled];

    /// The canonical value spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Shuffled => "shuffled",
        }
    }

    /// Decode a value spelling; `None` for anything outside the closed set.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|order| order.as_str() == value)
    }
}

/// The shortest a slideshow shows one picture.
pub const SLIDE_INTERVAL_MIN: Duration64 = Duration64::from_secs(5);
/// The longest a slideshow shows one picture: an hour.
pub const SLIDE_INTERVAL_MAX: Duration64 = Duration64::from_secs(3_600);
/// How long a slideshow shows one picture until it is told otherwise.
pub const SLIDE_INTERVAL_DEFAULT: Duration64 = Duration64::from_secs(30);

/// Which of the shipped pictures a slideshow shows.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SlideSource {
    /// Every category's.
    #[default]
    Every,
    /// One category's.
    Category(WallpaperCategory),
}

impl SlideSource {
    /// The one category named, or `None` for every category.
    #[must_use]
    pub const fn category(&self) -> Option<&WallpaperCategory> {
        match self {
            Self::Every => None,
            Self::Category(category) => Some(category),
        }
    }

    /// Decode a value spelling: empty for every category, else a category's
    /// name; `None` for a name no category could carry.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        if value.is_empty() {
            return Some(Self::Every);
        }
        WallpaperCategory::new(value).map(Self::Category)
    }

    /// The canonical value spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.category().map_or("", WallpaperCategory::as_str)
    }
}

/// The slideshow's options.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlideshowOptions {
    /// How long each picture is shown, within
    /// [`SLIDE_INTERVAL_MIN`]`..=`[`SLIDE_INTERVAL_MAX`].
    pub interval: Duration64,
    /// The order the pictures come in.
    pub order: SlideOrder,
    /// Which pictures are shown.
    pub source: SlideSource,
}

impl Default for SlideshowOptions {
    fn default() -> Self {
        Self {
            interval: SLIDE_INTERVAL_DEFAULT,
            order: SlideOrder::default(),
            source: SlideSource::default(),
        }
    }
}

impl SlideshowOptions {
    /// Decode an interval spelled in whole seconds, within the bounds.
    pub(crate) fn interval_from_value(value: &str) -> Option<Duration64> {
        let span = Duration64::from_secs(i64::from(parse_decimal(value)?));
        (SLIDE_INTERVAL_MIN..=SLIDE_INTERVAL_MAX)
            .contains(&span)
            .then_some(span)
    }

    /// An interval's spelling in whole seconds.
    pub(crate) fn render_interval(span: Duration64) -> String {
        format!("{}", span.saturating_total_nanos() / 1_000_000_000)
    }
}

/// The clock's options.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ClockOptions {
    /// Whether the date is shown beneath the time.
    pub date: bool,
    /// Whether the account and the machine are named beneath it.
    pub identity: bool,
}

impl Default for ClockOptions {
    fn default() -> Self {
        Self {
            date: true,
            identity: true,
        }
    }
}

/// The minimal clock's options.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RibbonOptions {
    /// Whether the date is shown beneath the time.
    pub date: bool,
}

impl Default for RibbonOptions {
    fn default() -> Self {
        Self { date: true }
    }
}

/// How many stars the starfield flies through.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum StarDensity {
    /// Half as many.
    Sparse,
    /// The field's own density.
    #[default]
    Normal,
    /// Twice as many.
    Dense,
}

impl StarDensity {
    /// Every density, sparsest first.
    pub const ALL: [Self; 3] = [Self::Sparse, Self::Normal, Self::Dense];

    /// The canonical value spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sparse => "sparse",
            Self::Normal => "normal",
            Self::Dense => "dense",
        }
    }

    /// Decode a value spelling; `None` for anything outside the closed set.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|density| density.as_str() == value)
    }

    /// How many stars this density flies through for every hundred the
    /// field's own density would.
    #[must_use]
    pub const fn percent(self) -> u64 {
        match self {
            Self::Sparse => 50,
            Self::Normal => 100,
            Self::Dense => 200,
        }
    }
}

/// The starfield's options.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StarfieldOptions {
    /// How many stars there are.
    pub stars: StarDensity,
    /// Whether the flight surges into warp and settles back, rather than only
    /// cruising.
    pub warp: bool,
}

impl Default for StarfieldOptions {
    fn default() -> Self {
        Self {
            stars: StarDensity::default(),
            warp: true,
        }
    }
}

/// How large the Game of Life's cells are drawn.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum CellSize {
    /// A fine board of many cells.
    Small,
    /// The board's own size.
    #[default]
    Medium,
    /// A coarse board of few.
    Large,
}

impl CellSize {
    /// Every size, smallest first.
    pub const ALL: [Self; 3] = [Self::Small, Self::Medium, Self::Large];

    /// The canonical value spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    /// Decode a value spelling; `None` for anything outside the closed set.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|size| size.as_str() == value)
    }

    /// A cell's side in logical pixels.
    #[must_use]
    pub const fn logical_side(self) -> u32 {
        match self {
            Self::Small => 4,
            Self::Medium => 8,
            Self::Large => 14,
        }
    }
}

/// How fast the Game of Life's generations pass.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum LifeSpeed {
    /// Five generations a second.
    Slow,
    /// Ten generations a second.
    #[default]
    Normal,
    /// Fifteen generations a second.
    Fast,
}

impl LifeSpeed {
    /// Every speed, slowest first.
    pub const ALL: [Self; 3] = [Self::Slow, Self::Normal, Self::Fast];

    /// The canonical value spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Slow => "slow",
            Self::Normal => "normal",
            Self::Fast => "fast",
        }
    }

    /// Decode a value spelling; `None` for anything outside the closed set.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|speed| speed.as_str() == value)
    }

    /// How many generations pass each second.
    #[must_use]
    pub const fn per_second(self) -> u32 {
        match self {
            Self::Slow => 5,
            Self::Normal => 10,
            Self::Fast => 15,
        }
    }
}

/// The Game of Life's options.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct LifeOptions {
    /// How large a cell is drawn.
    pub cells: CellSize,
    /// How fast the generations pass.
    pub speed: LifeSpeed,
}

/// Every screensaver's options, kept whichever screensaver is chosen.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScreensaverOptions {
    /// The slideshow's.
    pub slideshow: SlideshowOptions,
    /// The clock's.
    pub clock: ClockOptions,
    /// The minimal clock's.
    pub ribbon: RibbonOptions,
    /// The starfield's.
    pub starfield: StarfieldOptions,
    /// The Game of Life's.
    pub life: LifeOptions,
}

#[cfg(test)]
mod tests {
    use tairix_abi::desktop::ScreensaverKind;
    use tairix_abi::time::Duration64;

    use super::{
        preview_file, preview_kind, preview_path, CellSize, LifeSpeed, SlideOrder, SlideSource,
        SlideshowOptions, StarDensity, WallpaperCategory, SCREENSAVER_PREVIEW_STORE,
    };

    #[test]
    fn every_kind_names_one_preview_and_every_preview_one_kind() {
        for kind in ScreensaverKind::ALL {
            assert_eq!(preview_kind(&preview_file(kind)), Some(kind));
            assert_eq!(
                preview_path(kind),
                alloc::format!("{SCREENSAVER_PREVIEW_STORE}/{}.png", kind.as_str())
            );
        }
        for stray in [
            "starfield.jpg",
            "Starfield.png",
            "fireworks.png",
            ".png",
            "life",
        ] {
            assert_eq!(preview_kind(stray), None, "{stray}");
        }
    }

    #[test]
    fn an_interval_is_whole_seconds_within_its_bounds() {
        assert_eq!(
            SlideshowOptions::interval_from_value("5"),
            Some(Duration64::from_secs(5))
        );
        assert_eq!(
            SlideshowOptions::interval_from_value("3600"),
            Some(Duration64::from_secs(3_600))
        );
        for bad in ["4", "3601", "30s", "-30", "", "1e3"] {
            assert_eq!(SlideshowOptions::interval_from_value(bad), None, "{bad:?}");
        }
        let span = Duration64::from_secs(90);
        assert_eq!(
            SlideshowOptions::interval_from_value(&SlideshowOptions::render_interval(span)),
            Some(span)
        );
    }

    #[test]
    fn a_source_is_empty_for_every_category_or_a_plain_name() {
        assert_eq!(SlideSource::from_value(""), Some(SlideSource::Every));
        let nature = WallpaperCategory::new("Nature").expect("a category");
        assert_eq!(
            SlideSource::from_value("Nature"),
            Some(SlideSource::Category(nature.clone()))
        );
        assert_eq!(SlideSource::Category(nature).as_str(), "Nature");
        assert_eq!(SlideSource::Every.as_str(), "");
        for bad in ["a/b", "..", "."] {
            assert_eq!(SlideSource::from_value(bad), None, "{bad:?}");
        }
        let long: alloc::string::String =
            core::iter::repeat_n('a', tairix_abi::FS_NAME_MAX + 1).collect();
        assert_eq!(WallpaperCategory::new(&long), None);
    }

    #[test]
    fn every_closed_option_has_one_spelling() {
        for order in SlideOrder::ALL {
            assert_eq!(SlideOrder::from_value(order.as_str()), Some(order));
        }
        for density in StarDensity::ALL {
            assert_eq!(StarDensity::from_value(density.as_str()), Some(density));
        }
        for size in CellSize::ALL {
            assert_eq!(CellSize::from_value(size.as_str()), Some(size));
        }
        for speed in LifeSpeed::ALL {
            assert_eq!(LifeSpeed::from_value(speed.as_str()), Some(speed));
        }
        assert_eq!(SlideOrder::from_value("random"), None);
        assert_eq!(StarDensity::from_value("Normal"), None);
    }

    #[test]
    fn each_step_of_a_ladder_differs_from_the_next() {
        let percents = StarDensity::ALL.map(StarDensity::percent);
        assert!(percents.windows(2).all(|pair| pair[0] < pair[1]));
        let sides = CellSize::ALL.map(CellSize::logical_side);
        assert!(sides.windows(2).all(|pair| pair[0] < pair[1]));
        let rates = LifeSpeed::ALL.map(LifeSpeed::per_second);
        assert!(rates.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
