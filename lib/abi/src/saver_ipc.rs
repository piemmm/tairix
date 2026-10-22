//! The screensaver protocol: what the desktop and the sandboxed saver it hosts
//! say to each other over the saver's two pipes (`plans/SCREENSAVERS.md`).
//!
//! Every message is one `lib/sandbox` frame whose first byte names it. Both
//! directions are untrusted by their reader — a saver may be anyone's code, and
//! a saver owes its host no trust either — so every field is bounded, every
//! reserved byte is refused, and a message that is not exactly one well-formed
//! frame is refused whole. A `Start`'s scale is the one field held to a range
//! this crate does not define: its reader makes a `tairix_geometry::Scale` of
//! it, and refuses the `Start` when it cannot.
//!
//! The pixels themselves never cross a pipe. A saver draws into the frame area
//! of the one shared region its host handed it ([`SaverRegion`]) and says which
//! rectangles changed; pictures for a slides or keeping feed pass through the
//! region's feed area, owned by whichever side the protocol last handed it to.

use crate::bounded_text::BoundedText;
use crate::desktop::{
    decode_text, encode_text, Appearance, Contrast, Density, DesktopText, TEXT_WIRE_LEN,
};
use crate::driver::display::{
    DamageList, DamageRect, DisplayFormat, DisplayMode, RefreshRate, MAX_DAMAGE_RECTS,
};
use crate::font_ipc::{
    FontRequest, FONT_MAX_FAMILIES_REPLY, FONT_MAX_GLYPH_REPLY, FONT_MAX_OUTLINE_REPLY,
    FONT_METRICS_REPLY_LEN,
};
use crate::le::{read_u16, read_u32, read_u64};
use crate::saver::{
    ChoiceValue, OptionKey, SaverFeeds, SaverValue, SAVER_CHOICE_VALUE_MAX, SAVER_MAX_OPTIONS,
    SAVER_OPTION_KEY_MAX,
};
use crate::session_ipc::SESSION_LOGIN_NAME_MAX;
use crate::switchboard_ipc::MachineReport;
use crate::sysinfo::{MemoryBand, HOSTNAME_MAX};
use crate::Errno;

/// The protocol a saver and its host speak; a `Start` naming another is
/// refused.
pub const SAVER_PROTOCOL_VERSION: u16 = 1;

/// How long a saver has from its spawn to its `Ready` before its host ends it.
pub const SAVER_START_DEADLINE_NS: u64 = 10_000_000_000;

/// How long a saver has to answer a `Frame` before its host ends it.
pub const SAVER_FRAME_DEADLINE_NS: u64 = 2_000_000_000;

/// The most font calls a saver makes while it starts, or from one frame asked
/// for to the next, before its host ends it. A reply answers as much of a glyph
/// run as it holds, so a face needs a few calls for each size it sets; the
/// bound is what stops a saver turning the relay into its own font workload.
pub const SAVER_FONT_CALLS_MAX: u16 = 64;

/// The soonest one picture a saver keeps follows the last: a picture is the
/// end of a scene's work, so anything faster is a saver filling the user's
/// pictures rather than finishing them.
pub const SAVER_KEEP_SPACING_NS: u64 = 30_000_000_000;

/// The most pictures one run of a saver keeps; past it a keep is answered as
/// not kept, and the saver carries on.
pub const SAVER_KEEPS_MAX: u16 = 256;

/// Longest title a kept picture is filed under, in bytes.
pub const SAVER_KEEP_TITLE_MAX: usize = 48;

/// Longest spelling of the time a [`WallReading`] carries, in bytes.
pub const SAVER_TIME_TEXT_MAX: usize = 32;

/// Longest spelling of the date a [`WallReading`] carries, in bytes.
pub const SAVER_DATE_TEXT_MAX: usize = 64;

/// The bytes of seed a saver is started with: its only entropy, which it
/// keys its hashing and seeds its generators from.
pub const SAVER_SEED_LEN: usize = 32;

/// Largest font-service reply a `FontReply` relays: the glyph batch, which
/// bounds every other reply kind.
pub const SAVER_FONT_REPLY_MAX: usize = FONT_MAX_GLYPH_REPLY;
const _: () = assert!(
    SAVER_FONT_REPLY_MAX >= FONT_MAX_OUTLINE_REPLY
        && SAVER_FONT_REPLY_MAX >= FONT_MAX_FAMILIES_REPLY
        && SAVER_FONT_REPLY_MAX >= FONT_METRICS_REPLY_LEN
);

const OP_START: u8 = 1;
const OP_FRAME: u8 = 2;
const OP_MACHINE: u8 = 3;
const OP_SLIDE: u8 = 4;
const OP_FONT_REPLY: u8 = 5;
const OP_KEPT: u8 = 6;
const OP_PRESSURE: u8 = 7;
const OP_MACHINE_LOST: u8 = 8;

const OP_READY: u8 = 0x81;
const OP_PRESENTED: u8 = 0x82;
const OP_FONT_CALL: u8 = 0x83;
const OP_KEEP: u8 = 0x84;
const OP_WANT_SLIDE: u8 = 0x85;
const OP_UNSTARTED: u8 = 0x86;

/// How the region's pixels are laid out: premultiplied RGBA, as an app's
/// window frame is.
const FORMAT: DisplayFormat = DisplayFormat::Rgba8888;

/// The shared region a saver presents through: a frame area of one screen,
/// then, for a saver given slides or keeping, a feed area of one more.
///
/// Both sides derive the layout from the screen and the granted feeds through
/// this one function, so the host never has to describe it and a saver cannot
/// claim another.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SaverRegion {
    width: u32,
    height: u32,
    feed: bool,
}

impl SaverRegion {
    /// The region for a `width` × `height` screen, with a feed area when
    /// `feeds` passes pictures.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for an empty screen, [`Errno::LengthOutOfRange`]
    /// for one whose region a `usize` cannot measure.
    pub fn new(width: u32, height: u32, feeds: SaverFeeds) -> Result<Self, Errno> {
        if width == 0 || height == 0 {
            return Err(Errno::OutOfRange);
        }
        if width.checked_mul(FORMAT.bytes_per_pixel()).is_none() {
            return Err(Errno::LengthOutOfRange);
        }
        let region = Self {
            width,
            height,
            feed: feeds.uses_feed_area(),
        };
        region.checked_len().ok_or(Errno::LengthOutOfRange)?;
        Ok(region)
    }

    fn checked_frame_len(&self) -> Option<usize> {
        usize::try_from(self.width)
            .ok()?
            .checked_mul(FORMAT.bytes_per_pixel() as usize)?
            .checked_mul(usize::try_from(self.height).ok()?)
    }

    fn checked_len(&self) -> Option<usize> {
        let frame = self.checked_frame_len()?;
        if self.feed {
            frame.checked_mul(2)
        } else {
            Some(frame)
        }
    }

    /// The screen's width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The screen's height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The bytes one row of either area takes.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.width as usize * FORMAT.bytes_per_pixel() as usize
    }

    /// The frame area: the region's first `frame_len` bytes.
    #[must_use]
    pub fn frame_len(&self) -> usize {
        self.checked_frame_len().unwrap_or(0)
    }

    /// The feed area, when the region has one: the screen-sized picture after
    /// the frame.
    #[must_use]
    pub fn feed_range(&self) -> Option<core::ops::Range<usize>> {
        let frame = self.frame_len();
        self.feed.then(|| frame..frame * 2)
    }

    /// How either area is laid out, as a window frame of the screen's size:
    /// what the host's decoder and the saver's encoder both read it by.
    #[must_use]
    pub const fn display_mode(&self) -> DisplayMode {
        DisplayMode {
            width_px: self.width,
            height_px: self.height,
            stride_bytes: self.width * FORMAT.bytes_per_pixel(),
            format: FORMAT,
        }
    }

    /// The region's whole length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.checked_len().unwrap_or(0)
    }

    /// Whether the region holds no bytes, which [`Self::new`] never builds.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One option's value as a saver is started with it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OptionValue {
    /// The option's key.
    pub key: OptionKey,
    /// Its value.
    pub value: SaverValue,
}

const VALUE_SWITCH: u8 = 1;
const VALUE_CHOICE: u8 = 2;
const VALUE_RANGE: u8 = 3;
const OPTION_VALUE_LEN: usize = 2 + SAVER_OPTION_KEY_MAX + 1 + 1 + SAVER_CHOICE_VALUE_MAX;

impl OptionValue {
    fn encode(&self, out: &mut [u8; OPTION_VALUE_LEN]) {
        out[0] = self.key.text().len_byte();
        out[2..2 + SAVER_OPTION_KEY_MAX].copy_from_slice(self.key.text().raw_bytes());
        let at = 2 + SAVER_OPTION_KEY_MAX;
        match self.value {
            SaverValue::Switch(on) => {
                out[at] = VALUE_SWITCH;
                out[at + 2] = u8::from(on);
            }
            SaverValue::Choice(word) => {
                out[at] = VALUE_CHOICE;
                out[at + 1] = word.text().len_byte();
                out[at + 2..].copy_from_slice(word.text().raw_bytes());
            }
            SaverValue::Range(number) => {
                out[at] = VALUE_RANGE;
                out[at + 2..at + 6].copy_from_slice(&number.to_le_bytes());
            }
        }
    }

    fn decode(bytes: &[u8; OPTION_VALUE_LEN]) -> Result<Self, Errno> {
        if bytes[1] != 0 {
            return Err(Errno::BadMagic);
        }
        let key = OptionKey::from_wire(bytes[0], &array(bytes, 2))?;
        let at = 2 + SAVER_OPTION_KEY_MAX;
        let rest = &bytes[at + 2..];
        let value = match bytes[at] {
            VALUE_SWITCH => {
                if bytes[at + 1] != 0 || !zero(&rest[1..]) {
                    return Err(Errno::BadMagic);
                }
                match rest[0] {
                    0 => SaverValue::Switch(false),
                    1 => SaverValue::Switch(true),
                    _ => return Err(Errno::OutOfRange),
                }
            }
            VALUE_CHOICE => SaverValue::Choice(ChoiceValue::from_wire(
                bytes[at + 1],
                &array(bytes, at + 2),
            )?),
            VALUE_RANGE => {
                if bytes[at + 1] != 0 || !zero(&rest[4..]) {
                    return Err(Errno::BadMagic);
                }
                SaverValue::Range(i32::from_le_bytes(array(bytes, at + 2)))
            }
            _ => return Err(Errno::BadMagic),
        };
        Ok(Self { key, value })
    }
}

/// What a saver is started with: everything it draws from that is not a
/// frame's own.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Start {
    /// The screen.
    pub region: SaverRegion,
    /// The desktop's UI scale, in percent (`tairix_geometry::Scale`).
    pub scale_percent: u32,
    /// The display's refresh rate.
    pub refresh: RefreshRate,
    /// The monotonic time the saver was started at, which its scene sets off
    /// from: what it draws is timed by the host's frames, not by its own
    /// reading of the clock.
    pub now_ns: u64,
    /// Whether the user asked for reduced motion.
    pub reduced_motion: bool,
    /// How much separation the desktop draws around a control.
    pub contrast: Contrast,
    /// How much room the desktop gives a control.
    pub density: Density,
    /// The desktop's appearance.
    pub appearance: Appearance,
    /// The face and size the user chose for the desktop's text, or `None` for
    /// the theme's own.
    pub text: Option<DesktopText>,
    /// The host's CSPRNG output, the saver's only entropy: it has no RNG of
    /// its own.
    pub seed: [u8; SAVER_SEED_LEN],
    /// The memory pressure the machine is under, which the saver's caches
    /// answer to.
    pub pressure: MemoryBand,
    /// The machine's online cores.
    pub cores: u16,
    /// The threads the saver may hold, its first among them.
    pub threads: u32,
    /// The bytes of address space the saver may map.
    pub memory: u64,
    /// The account's login name; empty when unknown.
    pub account: BoundedText<0, SESSION_LOGIN_NAME_MAX>,
    /// The machine's name; empty when unknown.
    pub machine: BoundedText<0, HOSTNAME_MAX>,
    /// The privileged feeds the saver is given.
    pub feeds: SaverFeeds,
    /// The slides the slides feed offers, numbered from zero; zero without the
    /// feed.
    pub slides: u16,
    options: [Option<OptionValue>; SAVER_MAX_OPTIONS as usize],
}

const START_VERSION: usize = 1;
const START_WIDTH: usize = 4;
const START_HEIGHT: usize = 8;
const START_SCALE: usize = 12;
const START_REFRESH: usize = 16;
const START_MEMORY: usize = 20;
const START_THREADS: usize = 28;
const START_FEEDS: usize = 32;
const START_CORES: usize = 36;
const START_SLIDES: usize = 38;
const START_FLAGS: usize = 40;
const START_APPEARANCE: usize = 41;
const START_ACCOUNT_LEN: usize = 42;
const START_MACHINE_LEN: usize = 43;
const START_OPTION_COUNT: usize = 44;
const START_PRESSURE: usize = 45;
const START_CONTRAST: usize = 46;
const START_DENSITY: usize = 47;
const START_NOW: usize = 48;
const START_SEED: usize = 56;
const START_TEXT: usize = START_SEED + SAVER_SEED_LEN;
const START_ACCOUNT: usize = START_TEXT + TEXT_WIRE_LEN;
const START_MACHINE: usize = START_ACCOUNT + SESSION_LOGIN_NAME_MAX;
const START_OPTIONS: usize = START_MACHINE + HOSTNAME_MAX;

/// Encoded length of a `Start`.
pub const START_WIRE_LEN: usize = START_OPTIONS + SAVER_MAX_OPTIONS as usize * OPTION_VALUE_LEN;

impl Start {
    /// A start with no option values yet.
    #[must_use]
    pub fn new(region: SaverRegion, scale_percent: u32, refresh: RefreshRate) -> Self {
        Self {
            region,
            scale_percent,
            refresh,
            now_ns: 0,
            reduced_motion: false,
            contrast: Contrast::default(),
            density: Density::default(),
            appearance: Appearance::default(),
            text: None,
            seed: [0; SAVER_SEED_LEN],
            pressure: MemoryBand::NORMAL,
            cores: 1,
            threads: 1,
            memory: 0,
            account: BoundedText::EMPTY,
            machine: BoundedText::EMPTY,
            feeds: SaverFeeds::NONE,
            slides: 0,
            options: [None; SAVER_MAX_OPTIONS as usize],
        }
    }

    /// Add `value`, refusing a ninth or a key already given.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] past [`SAVER_MAX_OPTIONS`] values,
    /// [`Errno::OutOfRange`] for a repeated key.
    pub fn push_option(&mut self, value: OptionValue) -> Result<(), Errno> {
        if self.options().any(|held| held.key == value.key) {
            return Err(Errno::OutOfRange);
        }
        let slot = self
            .options
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(Errno::LengthOutOfRange)?;
        *slot = Some(value);
        Ok(())
    }

    /// The option values, in the order they were given.
    pub fn options(&self) -> impl Iterator<Item = &OptionValue> + '_ {
        self.options.iter().map_while(Option::as_ref)
    }

    /// The value given for `key`.
    #[must_use]
    pub fn option(&self, key: &str) -> Option<SaverValue> {
        self.options()
            .find(|held| held.key.as_str() == key)
            .map(|held| held.value)
    }

    fn encode(&self, out: &mut [u8; START_WIRE_LEN]) {
        out[0] = OP_START;
        out[START_VERSION..START_VERSION + 2]
            .copy_from_slice(&SAVER_PROTOCOL_VERSION.to_le_bytes());
        out[START_WIDTH..START_WIDTH + 4].copy_from_slice(&self.region.width.to_le_bytes());
        out[START_HEIGHT..START_HEIGHT + 4].copy_from_slice(&self.region.height.to_le_bytes());
        out[START_SCALE..START_SCALE + 4].copy_from_slice(&self.scale_percent.to_le_bytes());
        out[START_REFRESH..START_REFRESH + 4].copy_from_slice(&self.refresh.mhz().to_le_bytes());
        out[START_NOW..START_SEED].copy_from_slice(&self.now_ns.to_le_bytes());
        out[START_SEED..START_TEXT].copy_from_slice(&self.seed);
        encode_text(self.text, &mut out[START_TEXT..START_ACCOUNT]);
        out[START_PRESSURE] = self.pressure.depth();
        out[START_MEMORY..START_MEMORY + 8].copy_from_slice(&self.memory.to_le_bytes());
        out[START_THREADS..START_THREADS + 4].copy_from_slice(&self.threads.to_le_bytes());
        out[START_FEEDS..START_FEEDS + 4].copy_from_slice(&self.feeds.bits().to_le_bytes());
        out[START_CORES..START_CORES + 2].copy_from_slice(&self.cores.to_le_bytes());
        out[START_SLIDES..START_SLIDES + 2].copy_from_slice(&self.slides.to_le_bytes());
        out[START_FLAGS] = u8::from(self.reduced_motion);
        out[START_APPEARANCE] = self.appearance.code();
        out[START_CONTRAST] = self.contrast.code();
        out[START_DENSITY] = self.density.code();
        out[START_ACCOUNT_LEN] = self.account.len_byte();
        out[START_MACHINE_LEN] = self.machine.len_byte();
        out[START_ACCOUNT..START_MACHINE].copy_from_slice(self.account.raw_bytes());
        out[START_MACHINE..START_OPTIONS].copy_from_slice(self.machine.raw_bytes());
        let mut count = 0u8;
        for (slot, value) in self.options().enumerate() {
            let at = START_OPTIONS + slot * OPTION_VALUE_LEN;
            let mut entry = [0u8; OPTION_VALUE_LEN];
            value.encode(&mut entry);
            out[at..at + OPTION_VALUE_LEN].copy_from_slice(&entry);
            count += 1;
        }
        out[START_OPTION_COUNT] = count;
    }

    fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        let bytes: &[u8; START_WIRE_LEN] = exact(bytes)?;
        if read_u16(bytes, START_VERSION) != SAVER_PROTOCOL_VERSION {
            return Err(Errno::AbiVersionUnsupported);
        }
        if bytes[3] != 0 {
            return Err(Errno::BadMagic);
        }
        let feeds = SaverFeeds::from_bits(read_u32(bytes, START_FEEDS))?;
        let region = SaverRegion::new(
            read_u32(bytes, START_WIDTH),
            read_u32(bytes, START_HEIGHT),
            feeds,
        )?;
        let refresh =
            RefreshRate::from_mhz(read_u32(bytes, START_REFRESH)).ok_or(Errno::OutOfRange)?;
        let reduced_motion = match bytes[START_FLAGS] {
            0 => false,
            1 => true,
            _ => return Err(Errno::OutOfRange),
        };
        let mut start = Self {
            now_ns: read_u64(bytes, START_NOW),
            reduced_motion,
            appearance: Appearance::from_code(bytes[START_APPEARANCE])?,
            contrast: Contrast::from_code(bytes[START_CONTRAST])?,
            density: Density::from_code(bytes[START_DENSITY])?,
            text: decode_text(&bytes[START_TEXT..START_ACCOUNT])?,
            seed: array(bytes, START_SEED),
            pressure: MemoryBand::new(bytes[START_PRESSURE])?,
            cores: read_u16(bytes, START_CORES),
            threads: read_u32(bytes, START_THREADS),
            memory: read_u64(bytes, START_MEMORY),
            account: BoundedText::from_wire(
                bytes[START_ACCOUNT_LEN],
                &array(bytes, START_ACCOUNT),
            )?,
            machine: BoundedText::from_wire(
                bytes[START_MACHINE_LEN],
                &array(bytes, START_MACHINE),
            )?,
            feeds,
            slides: read_u16(bytes, START_SLIDES),
            ..Self::new(region, read_u32(bytes, START_SCALE), refresh)
        };
        if start.cores == 0 || start.threads == 0 || (!feeds.slides() && start.slides != 0) {
            return Err(Errno::OutOfRange);
        }
        let count = usize::from(bytes[START_OPTION_COUNT]);
        if count > usize::from(SAVER_MAX_OPTIONS) {
            return Err(Errno::LengthOutOfRange);
        }
        for slot in 0..usize::from(SAVER_MAX_OPTIONS) {
            let at = START_OPTIONS + slot * OPTION_VALUE_LEN;
            let entry: [u8; OPTION_VALUE_LEN] = array(bytes, at);
            if slot < count {
                start.push_option(OptionValue::decode(&entry)?)?;
            } else if !zero(&entry) {
                return Err(Errno::BadMagic);
            }
        }
        Ok(start)
    }
}

/// The wall clock as the icon bar reads it, spelled for a saver that tells the
/// time: a saver reads no wall clock and no zone of its own.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WallReading {
    /// The time as the bar spells it; empty while the clock is not set.
    pub time: BoundedText<0, SAVER_TIME_TEXT_MAX>,
    /// The date as the bar spells it; empty while the clock is not set.
    pub numeric_date: BoundedText<0, SAVER_DATE_TEXT_MAX>,
    /// The date written out; empty while the clock is not set.
    pub long_date: BoundedText<0, SAVER_DATE_TEXT_MAX>,
    /// The monotonic time the reading next changes.
    pub next_change_ns: u64,
}

impl WallReading {
    /// A clock that is not set, told again at `next_change_ns`.
    #[must_use]
    pub const fn unset(next_change_ns: u64) -> Self {
        Self {
            time: BoundedText::EMPTY,
            numeric_date: BoundedText::EMPTY,
            long_date: BoundedText::EMPTY,
            next_change_ns,
        }
    }
}

const WALL_LEN: usize = 4 + 8 + SAVER_TIME_TEXT_MAX + 2 * SAVER_DATE_TEXT_MAX;

impl WallReading {
    fn encode(&self, out: &mut [u8]) {
        out[0] = self.time.len_byte();
        out[1] = self.numeric_date.len_byte();
        out[2] = self.long_date.len_byte();
        out[4..12].copy_from_slice(&self.next_change_ns.to_le_bytes());
        let date = 12 + SAVER_TIME_TEXT_MAX;
        out[12..date].copy_from_slice(self.time.raw_bytes());
        out[date..date + SAVER_DATE_TEXT_MAX].copy_from_slice(self.numeric_date.raw_bytes());
        out[date + SAVER_DATE_TEXT_MAX..WALL_LEN].copy_from_slice(self.long_date.raw_bytes());
    }

    fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes[3] != 0 {
            return Err(Errno::BadMagic);
        }
        let date = 12 + SAVER_TIME_TEXT_MAX;
        Ok(Self {
            time: BoundedText::from_wire(bytes[0], &array(bytes, 12))?,
            numeric_date: BoundedText::from_wire(bytes[1], &array(bytes, date))?,
            long_date: BoundedText::from_wire(bytes[2], &array(bytes, date + SAVER_DATE_TEXT_MAX))?,
            next_change_ns: read_u64(bytes, 4),
        })
    }
}

/// Why a wanted slide was not shown.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SlideOutcome {
    /// The slide is in the feed area, fitted to the screen, until the saver
    /// wants another.
    Shown,
    /// The picture could not be read or decoded; the feed area is unchanged.
    Unavailable,
}

/// When a saver wants its next frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NextFrame {
    /// At the display's next frame.
    Soon,
    /// At this monotonic time, or the display frame after it.
    At(u64),
    /// Only when a feed brings something new: the host asks for a frame at
    /// the display's next one after every feed message, whatever was last
    /// asked for, so what a feed brought is drawn.
    Idle,
}

/// What the host says to a saver.
// Decoded into and handled at once; lib/abi has no allocator to box the large
// readings.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostMessage<'a> {
    /// Begin: the first message, sent once.
    Start(Start),
    /// Draw a frame and present it.
    Frame {
        /// The frame's serial, which its `Presented` names.
        serial: u32,
        /// The monotonic time the frame is drawn for.
        now_ns: u64,
        /// The wall clock.
        wall: WallReading,
    },
    /// A machine reading for a saver given the machine feed, its task names
    /// already withheld as the desktop's settings say.
    Machine(MachineReport),
    /// The machine feed's source stopped: the monitor that reads the machine
    /// is not running, so the last reading is no longer live.
    MachineLost,
    /// The answer to a `WantSlide`.
    Slide {
        /// The slide it was for.
        index: u16,
        /// Whether it is in the feed area.
        outcome: SlideOutcome,
    },
    /// The font service's reply to the saver's `FontCall`: its bytes, at most
    /// [`SAVER_FONT_REPLY_MAX`], or the error the call failed with.
    FontReply(Result<&'a [u8], Errno>),
    /// The answer to a `Keep`: the feed area is the saver's again.
    Kept {
        /// Whether the picture was kept.
        kept: bool,
    },
    /// The machine's memory pressure moved.
    Pressure(MemoryBand),
}

/// What a saver says to its host.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SaverMessage {
    /// The region is mapped and the first frame may be asked for.
    Ready,
    /// A frame is in the frame area.
    Presented {
        /// The `Frame` it answers.
        serial: u32,
        /// What changed; `None` when nothing did.
        damage: Option<DamageList>,
        /// When the next frame is wanted.
        next: NextFrame,
    },
    /// One request of the font service, in its own wire form.
    FontCall([u8; FontRequest::WIRE_LEN]),
    /// The feed area holds a finished picture of `width` × `height` pixels,
    /// to keep under `title`.
    Keep {
        /// What the picture is called.
        title: BoundedText<1, SAVER_KEEP_TITLE_MAX>,
        /// Its width, at most the screen's.
        width: u32,
        /// Its height, at most the screen's.
        height: u32,
    },
    /// Put slide `index` into the feed area.
    WantSlide(u16),
    /// The saver cannot start, for this reason, and ends; only before `Ready`.
    Unstarted(Errno),
}

const FRAME_WIRE_LEN: usize = 1 + 3 + 4 + 8 + WALL_LEN;
const SLIDE_WIRE_LEN: usize = 4;
const KEPT_WIRE_LEN: usize = 2;
const FONT_REPLY_PREFIX: usize = 8;
const PRESENTED_WIRE_LEN: usize = 1 + 1 + 1 + 1 + 4 + 8 + MAX_DAMAGE_RECTS * 16;
const KEEP_WIRE_LEN: usize = 4 + 4 + 4 + SAVER_KEEP_TITLE_MAX;
const UNSTARTED_WIRE_LEN: usize = 8;

/// The longest message a host sends: a full font reply.
pub const SAVER_HOST_MESSAGE_MAX: usize = FONT_REPLY_PREFIX + SAVER_FONT_REPLY_MAX;

/// The longest message a saver sends.
pub const SAVER_MESSAGE_MAX: usize = 1 + FontRequest::WIRE_LEN;
const _: () =
    assert!(SAVER_MESSAGE_MAX >= PRESENTED_WIRE_LEN && SAVER_MESSAGE_MAX >= KEEP_WIRE_LEN);
const _: () = assert!(SAVER_HOST_MESSAGE_MAX >= START_WIRE_LEN);

impl<'a> HostMessage<'a> {
    /// Encode into the front of `out`, answering the bytes written.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] when `out` cannot hold it, and
    /// [`Errno::MessageTooLarge`] for a font reply past
    /// [`SAVER_FONT_REPLY_MAX`].
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let len = match self {
            Self::Start(_) => START_WIRE_LEN,
            Self::Frame { .. } => FRAME_WIRE_LEN,
            Self::Machine(_) => 1 + MachineReport::WIRE_LEN,
            Self::Slide { .. } => SLIDE_WIRE_LEN,
            Self::FontReply(Ok(reply)) if reply.len() > SAVER_FONT_REPLY_MAX => {
                return Err(Errno::MessageTooLarge);
            }
            Self::FontReply(Ok(reply)) => FONT_REPLY_PREFIX + reply.len(),
            Self::FontReply(Err(_)) => FONT_REPLY_PREFIX,
            Self::Kept { .. } | Self::Pressure(_) | Self::MachineLost => KEPT_WIRE_LEN,
        };
        let out = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        out.fill(0);
        match self {
            Self::Start(start) => {
                let wire: &mut [u8; START_WIRE_LEN] =
                    out.try_into().map_err(|_| Errno::LengthOutOfRange)?;
                start.encode(wire);
            }
            Self::Frame {
                serial,
                now_ns,
                wall,
            } => {
                out[0] = OP_FRAME;
                out[4..8].copy_from_slice(&serial.to_le_bytes());
                out[8..16].copy_from_slice(&now_ns.to_le_bytes());
                wall.encode(&mut out[16..]);
            }
            Self::Machine(report) => {
                out[0] = OP_MACHINE;
                out[1..].copy_from_slice(&report.to_le_bytes());
            }
            Self::Slide { index, outcome } => {
                out[0] = OP_SLIDE;
                out[1] = match outcome {
                    SlideOutcome::Shown => 1,
                    SlideOutcome::Unavailable => 2,
                };
                out[2..4].copy_from_slice(&index.to_le_bytes());
            }
            Self::FontReply(reply) => {
                out[0] = OP_FONT_REPLY;
                match reply {
                    Ok(bytes) => {
                        out[4..8].copy_from_slice(&len_u32(bytes.len()).to_le_bytes());
                        out[FONT_REPLY_PREFIX..].copy_from_slice(bytes);
                    }
                    Err(errno) => {
                        out[1] = 1;
                        out[4..8].copy_from_slice(&errno.as_i32().to_le_bytes());
                    }
                }
            }
            Self::Kept { kept } => {
                out[0] = OP_KEPT;
                out[1] = if *kept { 1 } else { 2 };
            }
            Self::Pressure(band) => {
                out[0] = OP_PRESSURE;
                out[1] = band.depth();
            }
            Self::MachineLost => out[0] = OP_MACHINE_LOST,
        }
        Ok(len)
    }

    /// Decode one frame's payload, a font reply borrowed from it.
    ///
    /// # Errors
    ///
    /// [`Errno::BadMagic`] for an unknown message or a stray byte,
    /// [`Errno::LengthOutOfRange`] for a payload not exactly its message's
    /// length, and the field refusals of the message.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Errno> {
        let (&op, _) = bytes.split_first().ok_or(Errno::LengthOutOfRange)?;
        match op {
            OP_START => Ok(Self::Start(Start::decode(bytes)?)),
            OP_FRAME => {
                let bytes: &[u8; FRAME_WIRE_LEN] = exact(bytes)?;
                if !zero(&bytes[1..4]) {
                    return Err(Errno::BadMagic);
                }
                Ok(Self::Frame {
                    serial: read_u32(bytes, 4),
                    now_ns: read_u64(bytes, 8),
                    wall: WallReading::decode(&bytes[16..])?,
                })
            }
            OP_MACHINE => {
                let report = bytes.get(1..).ok_or(Errno::LengthOutOfRange)?;
                if report.len() != MachineReport::WIRE_LEN {
                    return Err(Errno::LengthOutOfRange);
                }
                Ok(Self::Machine(MachineReport::from_bytes(report)?))
            }
            OP_SLIDE => {
                let bytes: &[u8; SLIDE_WIRE_LEN] = exact(bytes)?;
                let outcome = match bytes[1] {
                    1 => SlideOutcome::Shown,
                    2 => SlideOutcome::Unavailable,
                    _ => return Err(Errno::BadMagic),
                };
                Ok(Self::Slide {
                    index: read_u16(bytes, 2),
                    outcome,
                })
            }
            OP_FONT_REPLY => {
                if bytes.len() < FONT_REPLY_PREFIX || bytes.len() > SAVER_HOST_MESSAGE_MAX {
                    return Err(Errno::LengthOutOfRange);
                }
                if !zero(&bytes[2..4]) {
                    return Err(Errno::BadMagic);
                }
                let word = read_u32(bytes, 4);
                match bytes[1] {
                    0 if usize::try_from(word).ok() == Some(bytes.len() - FONT_REPLY_PREFIX) => {
                        Ok(Self::FontReply(Ok(&bytes[FONT_REPLY_PREFIX..])))
                    }
                    1 if bytes.len() == FONT_REPLY_PREFIX => {
                        let errno = Errno::from_i32(i32::from_le_bytes(array(bytes, 4)))
                            .ok_or(Errno::OutOfRange)?;
                        Ok(Self::FontReply(Err(errno)))
                    }
                    0 | 1 => Err(Errno::LengthOutOfRange),
                    _ => Err(Errno::BadMagic),
                }
            }
            OP_KEPT => {
                let bytes: &[u8; KEPT_WIRE_LEN] = exact(bytes)?;
                match bytes[1] {
                    1 => Ok(Self::Kept { kept: true }),
                    2 => Ok(Self::Kept { kept: false }),
                    _ => Err(Errno::BadMagic),
                }
            }
            OP_PRESSURE => {
                let bytes: &[u8; KEPT_WIRE_LEN] = exact(bytes)?;
                Ok(Self::Pressure(MemoryBand::new(bytes[1])?))
            }
            OP_MACHINE_LOST => {
                let bytes: &[u8; KEPT_WIRE_LEN] = exact(bytes)?;
                if bytes[1] != 0 {
                    return Err(Errno::BadMagic);
                }
                Ok(Self::MachineLost)
            }
            _ => Err(Errno::BadMagic),
        }
    }
}

const NEXT_SOON: u8 = 1;
const NEXT_AT: u8 = 2;
const NEXT_IDLE: u8 = 3;

impl SaverMessage {
    /// Encode into the front of `out`, answering the bytes written.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] when `out` cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let len = match self {
            Self::Ready => 1,
            Self::Presented { .. } => PRESENTED_WIRE_LEN,
            Self::FontCall(_) => 1 + FontRequest::WIRE_LEN,
            Self::Keep { .. } => KEEP_WIRE_LEN,
            Self::WantSlide(_) => 4,
            Self::Unstarted(_) => UNSTARTED_WIRE_LEN,
        };
        let out = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        out.fill(0);
        match self {
            Self::Ready => out[0] = OP_READY,
            Self::Presented {
                serial,
                damage,
                next,
            } => {
                out[0] = OP_PRESENTED;
                let rects = damage.as_ref().map_or(&[][..], DamageList::rects);
                out[1] = u8::try_from(rects.len()).unwrap_or(0);
                let (tag, at) = match next {
                    NextFrame::Soon => (NEXT_SOON, 0),
                    NextFrame::At(at) => (NEXT_AT, *at),
                    NextFrame::Idle => (NEXT_IDLE, 0),
                };
                out[2] = tag;
                out[4..8].copy_from_slice(&serial.to_le_bytes());
                out[8..16].copy_from_slice(&at.to_le_bytes());
                for (index, rect) in rects.iter().enumerate() {
                    let base = 16 + index * 16;
                    out[base..base + 4].copy_from_slice(&rect.x.to_le_bytes());
                    out[base + 4..base + 8].copy_from_slice(&rect.y.to_le_bytes());
                    out[base + 8..base + 12].copy_from_slice(&rect.width_px.to_le_bytes());
                    out[base + 12..base + 16].copy_from_slice(&rect.height_px.to_le_bytes());
                }
            }
            Self::FontCall(request) => {
                out[0] = OP_FONT_CALL;
                out[1..].copy_from_slice(request);
            }
            Self::Keep {
                title,
                width,
                height,
            } => {
                out[0] = OP_KEEP;
                out[1] = title.len_byte();
                out[4..8].copy_from_slice(&width.to_le_bytes());
                out[8..12].copy_from_slice(&height.to_le_bytes());
                out[12..].copy_from_slice(title.raw_bytes());
            }
            Self::WantSlide(index) => {
                out[0] = OP_WANT_SLIDE;
                out[2..4].copy_from_slice(&index.to_le_bytes());
            }
            Self::Unstarted(errno) => {
                out[0] = OP_UNSTARTED;
                out[4..8].copy_from_slice(&errno.as_i32().to_le_bytes());
            }
        }
        Ok(len)
    }

    /// Decode one frame's payload.
    ///
    /// The font request is carried in its own wire form and judged by the
    /// font service's own decoder here, so a host relays only a request that
    /// service would read.
    ///
    /// # Errors
    ///
    /// [`Errno::BadMagic`] for an unknown message or a stray byte,
    /// [`Errno::LengthOutOfRange`] for a payload not exactly its message's
    /// length or damage the frame format cannot hold, and the field refusals
    /// of the message.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        let (&op, _) = bytes.split_first().ok_or(Errno::LengthOutOfRange)?;
        match op {
            OP_READY => {
                exact::<1>(bytes)?;
                Ok(Self::Ready)
            }
            OP_PRESENTED => {
                let bytes: &[u8; PRESENTED_WIRE_LEN] = exact(bytes)?;
                let count = usize::from(bytes[1]);
                if count > MAX_DAMAGE_RECTS {
                    return Err(Errno::LengthOutOfRange);
                }
                if bytes[3] != 0 || !zero(&bytes[16 + count * 16..]) {
                    return Err(Errno::BadMagic);
                }
                let at = read_u64(bytes, 8);
                let next = match bytes[2] {
                    NEXT_SOON if at == 0 => NextFrame::Soon,
                    NEXT_AT => NextFrame::At(at),
                    NEXT_IDLE if at == 0 => NextFrame::Idle,
                    _ => return Err(Errno::BadMagic),
                };
                let mut rects = [DamageRect {
                    x: 0,
                    y: 0,
                    width_px: 0,
                    height_px: 0,
                }; MAX_DAMAGE_RECTS];
                for (index, rect) in rects.iter_mut().enumerate().take(count) {
                    let base = 16 + index * 16;
                    *rect = DamageRect {
                        x: read_u32(bytes, base),
                        y: read_u32(bytes, base + 4),
                        width_px: read_u32(bytes, base + 8),
                        height_px: read_u32(bytes, base + 12),
                    };
                }
                let damage = if count == 0 {
                    None
                } else {
                    Some(DamageList::new(&rects[..count])?)
                };
                Ok(Self::Presented {
                    serial: read_u32(bytes, 4),
                    damage,
                    next,
                })
            }
            OP_FONT_CALL => {
                let request: [u8; FontRequest::WIRE_LEN] = bytes
                    .get(1..)
                    .and_then(|request| request.try_into().ok())
                    .ok_or(Errno::LengthOutOfRange)?;
                FontRequest::from_bytes(&request)?;
                Ok(Self::FontCall(request))
            }
            OP_KEEP => {
                let bytes: &[u8; KEEP_WIRE_LEN] = exact(bytes)?;
                if !zero(&bytes[2..4]) {
                    return Err(Errno::BadMagic);
                }
                Ok(Self::Keep {
                    title: BoundedText::from_wire(bytes[1], &array(bytes, 12))?,
                    width: read_u32(bytes, 4),
                    height: read_u32(bytes, 8),
                })
            }
            OP_WANT_SLIDE => {
                let bytes: &[u8; 4] = exact(bytes)?;
                if bytes[1] != 0 {
                    return Err(Errno::BadMagic);
                }
                Ok(Self::WantSlide(read_u16(bytes, 2)))
            }
            OP_UNSTARTED => {
                let bytes: &[u8; UNSTARTED_WIRE_LEN] = exact(bytes)?;
                if !zero(&bytes[1..4]) {
                    return Err(Errno::BadMagic);
                }
                let errno = Errno::from_i32(i32::from_le_bytes(array(bytes, 4)))
                    .ok_or(Errno::OutOfRange)?;
                Ok(Self::Unstarted(errno))
            }
            _ => Err(Errno::BadMagic),
        }
    }
}

fn exact<const N: usize>(bytes: &[u8]) -> Result<&[u8; N], Errno> {
    bytes.try_into().map_err(|_| Errno::LengthOutOfRange)
}

fn array<const N: usize>(bytes: &[u8], at: usize) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes[at..at + N]);
    out
}

fn zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| b == 0)
}

fn len_u32(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "saver_ipc_tests.rs"]
mod tests;
