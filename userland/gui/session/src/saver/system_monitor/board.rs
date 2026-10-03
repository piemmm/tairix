//! The board: where each part of the System Monitor stands on the screen, and
//! what each part draws.
//!
//! The board is laid out against a reference of [`REFERENCE`] logical pixels
//! and scaled to fill the screen, so its readings are as legible from across
//! a room on a 4K panel as on a 1080p one. Its parts are slots that tile it,
//! so a part whose readings changed repaints its own slot and nothing else.
//!
//! Every part is built from the shared controls — the titled block, the metric
//! tile, the chart, the composition bar, the status pill — so the board reads
//! as the Switchboard does. The per-processor heat grid is the one instrument
//! drawn here: a reading per core at a size a few hundred cores still fit.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Reverse;

use tairix_abi::switchboard_ipc::{
    MachineComposition, MachineCores, MachineCpu, MachineDevice, MachineInterface, MachineMemory,
    MachineNetwork, MachineScope, MachineStorage, MachineTasks, Permille,
};
use tairix_abi::sysinfo::{LoadAverage, VolumeHealth};
use tairix_colour::Rgba;
use tairix_controls::{
    blend_area, block, paint_run, run_width, Chart, CompositionBar, CompositionSegment, MeterValue,
    MetricInstrument, MetricLayout, MetricTile, PressureKind, PressureState, ProgressValue,
    StatusPill,
};
use tairix_font::BitmapFont;
use tairix_geometry::to_i32;
use tairix_procinfo::display::{
    byte_parts, format_bytes, format_duration, format_rate, percent, whole_percent,
};
use tairix_procinfo::{format_load, memory_composition, volume_health_name, MemoryPart};
use tairix_theme::{SignalRole, TextRole, Theme};
use tairix_wm::{Color, Rect, Scale, Surface};

use super::verdict::Verdict;

/// The board's reference size in logical pixels: a screen this many times the
/// scale on either side fills it, and a wider or taller screen grows the board
/// along that side.
pub const REFERENCE: (u32, u32) = (960, 540);

/// The black clear of the board's edge, in logical pixels.
const MARGIN: u32 = 18;

/// How far the board orbits its rest position against burn-in, in logical
/// pixels either way; less than the margin, so nothing leaves the screen.
pub const ORBIT: u32 = 6;

/// The largest side a per-core cell is drawn at, in logical pixels, so a
/// machine with a few cores does not draw them as tiles.
const CELL_MAX: u32 = 20;

/// How much of a headline panel the band beneath its headline takes, in
/// percent: the processors' cells and memory's composition alike, so the two
/// traces above them stand level.
const BAND_SHARE: u32 = 35;

/// The narrowest a column of a list is laid in, in logical pixels: room for a
/// row's line of detail to read before it is cut.
const COLUMN_MIN: u32 = 240;

/// How much of a stale board's panels is laid under black, out of 255: the
/// readings stay legible, plainly not live.
const STALE_DIM: u8 = 150;

/// The figure a reading with no measurement behind it shows: never a zero.
const UNREAD: &str = "\u{2013}";

/// One part of the board.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Part {
    /// The machine's name, the time, and the verdict.
    Header,
    /// The processors.
    Cpu,
    /// Memory.
    Memory,
    /// The tasks.
    Tasks,
    /// The storage devices.
    Storage,
    /// The network interfaces.
    Network,
}

impl Part {
    /// Every part, in the order the board lays them out.
    pub const ALL: [Self; 6] = [
        Self::Header,
        Self::Cpu,
        Self::Memory,
        Self::Tasks,
        Self::Storage,
        Self::Network,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Header => 0,
            Self::Cpu => 1,
            Self::Memory => 2,
            Self::Tasks => 3,
            Self::Storage => 4,
            Self::Network => 5,
        }
    }
}

/// Where the board's parts stand on a screen, and the scale they are drawn at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Board {
    scale: Scale,
    slots: [Rect; 6],
}

impl Board {
    /// The board on a `screen` of physical pixels, `shift` logical pixels from
    /// its rest position, drawn in `theme`; `None` for a screen too small to
    /// seat one.
    #[must_use]
    pub fn new(screen: (u32, u32), shift: (i32, i32), theme: &Theme) -> Option<Self> {
        let scale = board_scale(screen)?;
        let margin = scale.scale_length(MARGIN);
        let (width, height) = (
            screen.0.checked_sub(margin.saturating_mul(2))?,
            screen.1.checked_sub(margin.saturating_mul(2))?,
        );
        let left = to_i32(margin).saturating_add(shift_px(scale, shift.0));
        let top = to_i32(margin).saturating_add(shift_px(scale, shift.1));
        let header = header_height(theme, scale).min(height);
        let body = Rect::new(
            left,
            top.saturating_add(to_i32(header)),
            width,
            height - header,
        );
        let mut slots = [Rect::EMPTY; 6];
        slots[Part::Header.index()] = Rect::new(left, top, width, header);
        if width.saturating_mul(4) >= height.saturating_mul(5) {
            let rows = split(body.top(), body.height, &[14, 11]);
            let ((upper_top, upper_h), (lower_top, lower_h)) = (rows[0], rows[1]);
            for (part, (x, w)) in [Part::Cpu, Part::Memory, Part::Tasks]
                .into_iter()
                .zip(split(body.left(), body.width, &[1, 1, 1]))
            {
                slots[part.index()] = Rect::new(x, upper_top, w, upper_h);
            }
            for (part, (x, w)) in [Part::Storage, Part::Network].into_iter().zip(split(
                body.left(),
                body.width,
                &[3, 2],
            )) {
                slots[part.index()] = Rect::new(x, lower_top, w, lower_h);
            }
        } else {
            let parts = [
                Part::Cpu,
                Part::Memory,
                Part::Tasks,
                Part::Storage,
                Part::Network,
            ];
            for (part, (y, h)) in
                parts
                    .into_iter()
                    .zip(split(body.top(), body.height, &[3, 3, 3, 3, 2]))
            {
                slots[part.index()] = Rect::new(body.left(), y, body.width, h);
            }
        }
        Some(Self { scale, slots })
    }

    /// The scale the board is drawn at.
    #[must_use]
    pub const fn scale(&self) -> Scale {
        self.scale
    }

    /// The slot `part` is drawn in.
    #[must_use]
    pub const fn slot(&self, part: Part) -> Rect {
        self.slots[part.index()]
    }
}

/// The scale at which the reference board fills the shorter way of `screen`,
/// no larger than a scale may be; `None` for a screen it would fill only
/// below the smallest, where no reading could be read.
fn board_scale(screen: (u32, u32)) -> Option<Scale> {
    let across = u64::from(screen.0) * 100 / u64::from(REFERENCE.0);
    let down = u64::from(screen.1) * 100 / u64::from(REFERENCE.1);
    let percent = u32::try_from(across.min(down)).unwrap_or(Scale::MAX_PERCENT);
    Scale::from_percent(percent.min(Scale::MAX_PERCENT))
}

/// A logical shift in physical pixels, keeping its sign.
fn shift_px(scale: Scale, logical: i32) -> i32 {
    let magnitude = to_i32(scale.scale_length(logical.unsigned_abs()));
    if logical < 0 {
        -magnitude
    } else {
        magnitude
    }
}

/// `length` pixels from `start` cut into runs weighted by `weights`, the last
/// taking what rounding leaves so the runs tile the length exactly.
fn split(start: i32, length: u32, weights: &[u32]) -> Vec<(i32, u32)> {
    let total: u32 = weights.iter().sum::<u32>().max(1);
    let mut at = start;
    let mut left = length;
    let mut runs = Vec::with_capacity(weights.len());
    for (index, weight) in weights.iter().enumerate() {
        let run = if index + 1 == weights.len() {
            left
        } else {
            u32::try_from(u64::from(length) * u64::from(*weight) / u64::from(total)).unwrap_or(0)
        };
        runs.push((at, run));
        at = at.saturating_add(to_i32(run));
        left = left.saturating_sub(run);
    }
    runs
}

/// What the board draws, beyond the report: the parts the session supplies.
pub struct Readings<'a> {
    /// The machine's name.
    pub host: &'a str,
    /// The time, as the icon bar spells it.
    pub time: &'a str,
    /// The line beside the verdict: the uptime and the date.
    pub detail: &'a str,
    /// What the board says first.
    pub verdict: &'a Verdict,
    /// Whether the busiest tasks are named.
    pub name_tasks: bool,
    /// Whether the readings have stopped coming.
    pub stale: bool,
}

/// The line beside the verdict: how long the machine has been up, and the
/// date, each where it is known.
#[must_use]
pub fn detail_line(uptime: Option<tairix_abi::Duration64>, date: &str) -> String {
    match (uptime, date.is_empty()) {
        (Some(uptime), true) => format!("up {}", format_duration(uptime)),
        (Some(uptime), false) => format!("up {} \u{b7} {date}", format_duration(uptime)),
        (None, _) => String::from(date),
    }
}

/// Lay `part` of the board onto `surface` whole, over black, from `report`'s
/// readings; with no report, its panel states that it holds none.
pub fn paint(
    surface: &mut Surface,
    board: &Board,
    part: Part,
    theme: &Theme,
    report: Option<&tairix_abi::switchboard_ipc::MachineReport>,
    readings: &Readings<'_>,
) {
    let slot = board.slot(part);
    erase(surface, slot);
    let scale = board.scale();
    let look = Look { theme, scale };
    if part == Part::Header {
        header(surface, slot, &look, readings);
        return;
    }
    let (title, subtitle) = heading(part, report);
    let Some(content) = panel(surface, slot, &look, title, &subtitle) else {
        return;
    };
    match (part, report) {
        (_, None) => look.note(surface, content, "No readings yet"),
        (Part::Cpu, Some(report)) => cpu(surface, content, &look, &report.cpu),
        (Part::Memory, Some(report)) => memory(surface, content, &look, &report.memory),
        (Part::Tasks, Some(report)) => tasks(
            surface,
            content,
            &look,
            (&report.tasks, report.cpu.load),
            readings.name_tasks,
        ),
        (Part::Storage, Some(report)) => storage(surface, content, &look, report.storage.as_ref()),
        (Part::Network, Some(report)) => network(surface, content, &look, report.network.as_ref()),
        (Part::Header, Some(_)) => {}
    }
    if readings.stale {
        blend_area(surface, slot, Rgba::new(0, 0, 0, STALE_DIM));
    }
}

/// The board's ground: black, whatever the desktop's appearance, so nothing
/// is lit that does not need to be.
pub fn erase(surface: &mut Surface, rect: Rect) {
    let Some((x, y)) = rect.surface_origin() else {
        return;
    };
    surface.fill_rect(x, y, rect.width, rect.height, Color::rgb(0, 0, 0));
}

/// How tall the header stands: the name line, then the verdict's pill.
#[must_use]
pub fn header_height(theme: &Theme, scale: Scale) -> u32 {
    let gap = scale.scale_length(theme.metrics().control_gap).max(1);
    font(theme, scale, TextRole::Heading)
        .line_height()
        .saturating_add(gap)
        .saturating_add(StatusPill::measured_height(scale, theme))
        .saturating_add(gap)
}

/// A part's title, and what its title line says beside it.
fn heading(
    part: Part,
    report: Option<&tairix_abi::switchboard_ipc::MachineReport>,
) -> (&'static str, String) {
    match part {
        Part::Header => ("", String::new()),
        Part::Cpu => (
            "PROCESSORS",
            report
                .map(|report| count(report.cpu.cores.len(), "core", "cores"))
                .unwrap_or_default(),
        ),
        Part::Memory => (
            "MEMORY",
            report
                .and_then(|report| report.memory.committed)
                .map(|committed| format!("{} installed", format_bytes(committed.total_bytes())))
                .unwrap_or_default(),
        ),
        Part::Tasks => (
            "TASKS",
            report
                .map(|report| match report.scope {
                    MachineScope::Machine => String::from("whole machine"),
                    MachineScope::Own => String::from("your own"),
                })
                .unwrap_or_default(),
        ),
        Part::Storage => (
            "STORAGE",
            report
                .and_then(|report| report.storage.as_ref())
                .map(|storage| count(usize::from(storage.total()), "device", "devices"))
                .unwrap_or_default(),
        ),
        Part::Network => (
            "NETWORK",
            report
                .and_then(|report| report.network.as_ref())
                .map(|network| count(usize::from(network.total()), "interface", "interfaces"))
                .unwrap_or_default(),
        ),
    }
}

/// `count` of a thing, spelled in the number it is.
fn count(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// The drawing context: the theme the board is drawn in, at the board's scale.
struct Look<'a> {
    theme: &'a Theme,
    scale: Scale,
}

impl Look<'_> {
    fn font(&self, role: TextRole) -> BitmapFont {
        font(self.theme, self.scale, role)
    }

    fn gap(&self) -> u32 {
        self.scale
            .scale_length(self.theme.metrics().control_gap)
            .max(1)
    }

    fn ink(&self) -> Color {
        Color::from(self.theme.palette().on_surface)
    }

    fn muted(&self) -> Color {
        Color::from(self.theme.palette().on_surface_muted)
    }

    /// One quiet line at the top of `rect`, for a part with nothing to show.
    fn note(&self, surface: &mut Surface, rect: Rect, text: &str) {
        line(
            surface,
            self.font(TextRole::Body),
            text,
            rect,
            false,
            self.muted(),
        );
    }
}

fn font(theme: &Theme, scale: Scale, role: TextRole) -> BitmapFont {
    BitmapFont::for_role(theme.fonts(), role, scale)
}

/// Draw `text` at the top of `rect`, cut with the shared mark where it does
/// not fit and set against the trailing edge when `trailing`, answering the
/// width drawn.
fn line(
    surface: &mut Surface,
    font: BitmapFont,
    text: &str,
    rect: Rect,
    trailing: bool,
    color: Color,
) -> u32 {
    let run = font.elide_to_width(text, rect.width);
    let width = run_width(font, run);
    let x = if trailing {
        rect.right().saturating_sub(to_i32(width))
    } else {
        rect.left()
    };
    paint_run(surface, font, run, (x, rect.top()), color, None);
    width
}

/// The header: the machine's name and the time, then the verdict beside the
/// uptime and the date.
fn header(surface: &mut Surface, slot: Rect, look: &Look<'_>, readings: &Readings<'_>) {
    let title = look.font(TextRole::Heading);
    let gap = look.gap();
    let time = line(surface, title, readings.time, slot, true, look.ink());
    let named = slot.width.saturating_sub(time.saturating_add(gap * 2));
    line(
        surface,
        title,
        readings.host,
        Rect::new(slot.left(), slot.top(), named, slot.height),
        false,
        look.ink(),
    );
    let pill_top = slot.top().saturating_add(to_i32(title.line_height() + gap));
    let mut pill = StatusPill::new(readings.verdict.text());
    if let Some(tone) = readings.verdict.tone() {
        pill = pill.with_tone(tone);
    }
    let pill_w = pill
        .measured_width(look.scale, look.theme)
        .min(slot.width * 2 / 3);
    let pill_h = StatusPill::measured_height(look.scale, look.theme);
    pill.render(
        surface,
        Rect::new(slot.left(), pill_top, pill_w, pill_h),
        look.scale,
        look.theme,
    );
    let caption = look.font(TextRole::Body);
    let beside = Rect::new(
        slot.left().saturating_add(to_i32(pill_w + gap * 2)),
        pill_top.saturating_add(to_i32(pill_h.saturating_sub(caption.line_height()) / 2)),
        slot.width.saturating_sub(pill_w + gap * 2),
        caption.line_height(),
    );
    line(
        surface,
        caption,
        readings.detail,
        beside,
        true,
        look.muted(),
    );
}

/// A part's plate and title, with `subtitle` set quietly against the title
/// line's far end, answering where its content goes.
fn panel(
    surface: &mut Surface,
    slot: Rect,
    look: &Look<'_>,
    title: &str,
    subtitle: &str,
) -> Option<Rect> {
    let inner = block::plate(surface, slot, look.scale, look.theme)?;
    let title_font = look.font(TextRole::SectionHeader);
    let named = title_font.text_width(title).saturating_add(look.gap() * 2);
    let beside = Rect::new(
        inner.left().saturating_add(to_i32(named)),
        inner.top(),
        inner.width.saturating_sub(named),
        title_font.line_height(),
    );
    line(
        surface,
        look.font(TextRole::Caption),
        subtitle,
        beside,
        true,
        look.muted(),
    );
    block::title(surface, inner, look.scale, look.theme, title);
    block::titled_content(slot, look.scale, look.theme)
}

/// The pressure a tile wears: `kind`'s while the monitor holds it pressured.
const fn pressure(pressured: bool, kind: PressureKind) -> PressureState {
    if pressured {
        PressureState::Under(kind)
    } else {
        PressureState::None
    }
}

/// A trace of `points` in `kind`'s own colour.
fn trace(kind: PressureKind, points: &[u16]) -> MetricInstrument {
    MetricInstrument::Trend(Chart::new(kind.signal_role()).with_samples(points.iter().copied()))
}

/// A part's headline: its figure in the display face, with the unit it is
/// read in where there is a figure, and the line of detail where there is one.
fn hero(figure: Option<String>, unit: &str, detail: String, kind: PressureKind) -> MetricTile {
    let measured = figure.is_some();
    let mut tile = MetricTile::new("", figure.unwrap_or_else(|| String::from(UNREAD)), kind)
        .with_value_role(TextRole::Display)
        .unplated();
    if measured {
        tile = tile.with_unit(unit);
    }
    if !detail.is_empty() {
        tile = tile.with_detail(detail);
    }
    tile
}

/// The processors: the busy share over its trace, then a cell per core.
fn cpu(surface: &mut Surface, content: Rect, look: &Look<'_>, cpu: &MachineCpu) {
    let busy = cpu.busy.map(|busy| whole_percent(busy.as_u16()));
    let tile = hero(busy, "% busy", load_line(cpu.load), PressureKind::Cpu)
        .with_instrument(trace(PressureKind::Cpu, cpu.history.points()))
        .with_pressure(pressure(cpu.pressured, PressureKind::Cpu));
    let grid_h = if cpu.cores.is_empty() {
        0
    } else {
        band_height(content)
    };
    let gap = look.gap();
    let tile_h = content.height.saturating_sub(grid_h);
    tile.render(
        surface,
        Rect::new(
            content.left(),
            content.top(),
            content.width,
            tile_h.saturating_sub(gap),
        ),
        look.scale,
        look.theme,
        None,
    );
    heat_grid(
        surface,
        Rect::new(
            content.left(),
            content.top().saturating_add(to_i32(tile_h)),
            content.width,
            grid_h,
        ),
        look,
        &cpu.cores,
    );
}

/// How tall the band beneath a headline stands in `content`.
const fn band_height(content: Rect) -> u32 {
    content.height.saturating_mul(BAND_SHARE) / 100
}

/// The load averages and the census beside them.
fn load_line(load: Option<LoadAverage>) -> String {
    load.map_or_else(String::new, |load| {
        format!(
            "load {} \u{b7} {} \u{b7} {} \u{b7} {} running",
            format_load(load.load1),
            format_load(load.load5),
            format_load(load.load15),
            load.runnable
        )
    })
}

/// One cell per core, as large as `rect` seats them all, each lit by its share
/// from the groove a track draws to the processors' own colour; an unmeasured
/// core is the groove alone.
fn heat_grid(surface: &mut Surface, rect: Rect, look: &Look<'_>, cores: &MachineCores) {
    let gap = look.scale.scale_length(1).max(1);
    let most = look.scale.scale_length(CELL_MAX).max(1);
    let Some((side, columns)) = cell_fit(cores.len(), (rect.width, rect.height), gap, most) else {
        return;
    };
    let Some((left, top)) = rect.surface_origin() else {
        return;
    };
    let palette = look.theme.palette();
    let groove = palette.scroll_track;
    let lit = palette.signal(SignalRole::Cpu);
    let pitch = side.saturating_add(gap);
    for (index, reading) in cores.readings().enumerate() {
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        let x = left.saturating_add((index % columns).saturating_mul(pitch));
        let y = top.saturating_add((index / columns).saturating_mul(pitch));
        let cell = reading.map_or(Color::from(groove), |share| heat(lit, groove, share));
        surface.fill_rect(x, y, side, side, cell);
    }
}

/// The largest square side up to `most` at which `count` cells, `gap` apart,
/// fit `room`, and how many go to a row — spread evenly over the rows they
/// need, so the last is never left a stub; `None` when not even single pixels
/// fit, or there is nothing to fit.
fn cell_fit(count: usize, room: (u32, u32), gap: u32, most: u32) -> Option<(u32, u32)> {
    let count = u32::try_from(count).ok().filter(|count| *count > 0)?;
    (1..=most).rev().find_map(|side| {
        let pitch = side.saturating_add(gap);
        let across = (room.0.saturating_add(gap) / pitch).min(count);
        if across == 0 {
            return None;
        }
        let rows = count.div_ceil(across);
        (rows.saturating_mul(pitch) <= room.1.saturating_add(gap))
            .then(|| (side, count.div_ceil(rows)))
    })
}

/// `share` of the way from `groove` to `lit`.
fn heat(lit: Rgba, groove: Rgba, share: Permille) -> Color {
    Color::from(groove.mix(lit, share.as_u16()))
}

/// Memory: the committed share over its trace, then where it went.
fn memory(surface: &mut Surface, content: Rect, look: &Look<'_>, memory: &MachineMemory) {
    let used = memory.committed.map(|committed| committed.used());
    let mut detail = memory
        .committed
        .map(|committed| {
            let bytes = u64::try_from(
                u128::from(committed.total_bytes()) * u128::from(committed.used().as_u16()) / 1_000,
            )
            .unwrap_or(u64::MAX);
            let (figure, of) = byte_parts(bytes, committed.total_bytes());
            format!("{figure} {of}")
        })
        .unwrap_or_default();
    if let Some(band) = memory.band {
        if !detail.is_empty() {
            detail.push_str(" \u{b7} ");
        }
        detail.push_str(band.name());
        detail.push_str(" band");
    }
    let figure = used.map(|used| whole_percent(used.as_u16()));
    let tile = hero(figure, "% committed", detail, PressureKind::Memory)
        .with_instrument(trace(PressureKind::Memory, memory.history.points()))
        .with_pressure(pressure(memory.pressured, PressureKind::Memory));
    let band_h = band_height(content);
    let bar = memory
        .composition
        .as_ref()
        .and_then(|parts| composition(parts, (content.width, band_h), look));
    let bar_h = if bar.is_some() { band_h } else { 0 };
    let gap = look.gap();
    let tile_h = content.height.saturating_sub(bar_h);
    tile.render(
        surface,
        Rect::new(
            content.left(),
            content.top(),
            content.width,
            tile_h.saturating_sub(gap),
        ),
        look.scale,
        look.theme,
        None,
    );
    if let Some(bar) = bar {
        bar.render(
            surface,
            Rect::new(
                content.left(),
                content.top().saturating_add(to_i32(tile_h)),
                content.width,
                bar_h,
            ),
            look.scale,
            look.theme,
        );
    }
}

/// `composition` as a bar whose key fits `room`: the largest parts named and
/// the rest folded into one, so every part the bar draws is one its key names;
/// `None` when not even the largest beside the rest fits.
fn composition(
    composition: &MachineComposition,
    room: (u32, u32),
    look: &Look<'_>,
) -> Option<CompositionBar> {
    let parts = memory_composition(
        composition.class_bytes(),
        composition.free_bytes(),
        composition.total_bytes(),
    )?;
    let (free, used) = parts.split_last()?;
    let mut largest: Vec<usize> = (0..used.len()).collect();
    largest.sort_by_key(|&index| Reverse(used[index].bytes));
    (1..=used.len())
        .rev()
        // Folding a single part would only rename it.
        .filter(|&named| used.len() - named != 1)
        .filter_map(|named| folded(used, &largest[..named], free))
        .find(|bar| bar.measured_height(room.0, look.scale, look.theme) <= room.1)
}

/// The bar naming `used`'s parts at `named`, in their own order, then the rest
/// as one, then `free`.
fn folded(used: &[MemoryPart], named: &[usize], free: &MemoryPart) -> Option<CompositionBar> {
    let mut segments = Vec::with_capacity(named.len() + 2);
    let (mut rest_bytes, mut rest_share) = (0u64, 0u16);
    for (index, part) in used.iter().enumerate() {
        if named.contains(&index) {
            segments.push(CompositionSegment::new(
                part.label(),
                share_text(part.bytes, part.share),
                part.share,
            ));
        } else {
            rest_bytes = rest_bytes.saturating_add(part.bytes);
            rest_share = rest_share.saturating_add(part.share);
        }
    }
    if named.len() < used.len() {
        segments.push(CompositionSegment::new(
            "Other",
            share_text(rest_bytes, rest_share),
            rest_share,
        ));
    }
    segments.push(CompositionSegment::remainder(
        free.label(),
        share_text(free.bytes, free.share),
        free.share,
    ));
    CompositionBar::new(PressureKind::Memory, segments).ok()
}

/// A part's share of the whole as the key spells it, in the headline's own
/// unit; a part holding anything never reads as none of it.
fn share_text(bytes: u64, share: u16) -> String {
    if bytes > 0 && share < 10 {
        String::from("<1%")
    } else {
        percent(share)
    }
}

/// The tasks: how many, what needs recovery, and — when the board may name
/// them — the busiest, each against the busiest of all.
fn tasks(
    surface: &mut Surface,
    content: Rect,
    look: &Look<'_>,
    (tasks, load): (&MachineTasks, Option<LoadAverage>),
    name_tasks: bool,
) {
    let figure = tasks.count().map(|count| format!("{count}"));
    let tile = hero(figure, "tasks", census_line(tasks, load), PressureKind::Cpu);
    let tile_h = tile
        .measured_height(look.scale, look.theme)
        .min(content.height);
    tile.render(
        surface,
        Rect::new(content.left(), content.top(), content.width, tile_h),
        look.scale,
        look.theme,
        None,
    );
    if !name_tasks {
        return;
    }
    let busiest = tasks
        .busiest()
        .map(|task| task.cpu.as_u16())
        .max()
        .unwrap_or(0)
        .max(1);
    let rows: Vec<MetricTile> = tasks
        .busiest()
        .map(|task| {
            MetricTile::new(
                task.name.as_str(),
                format!(
                    "{} \u{b7} {}",
                    percent(task.cpu.as_u16()),
                    format_bytes(task.memory_bytes)
                ),
                PressureKind::Cpu,
            )
            .with_layout(MetricLayout::Inline)
            .with_instrument(MetricInstrument::Track(MeterValue::Measured(
                ProgressValue::new(share_of(task.cpu.as_u16(), busiest)),
            )))
            .unplated()
        })
        .collect();
    let below = Rect::new(
        content.left(),
        content.top().saturating_add(to_i32(tile_h + look.gap())),
        content.width,
        content.height.saturating_sub(tile_h + look.gap()),
    );
    let _ = grid(surface, below, look, &rows, (pitch(&rows, look), 1));
}

/// What needs recovery, or the thread and user census where nothing does.
fn census_line(tasks: &MachineTasks, load: Option<LoadAverage>) -> String {
    let stopped = tasks.stopped();
    let unanswering = tasks.recovery().saturating_sub(stopped);
    let mut parts: Vec<String> = Vec::new();
    if stopped > 0 {
        parts.push(format!("{stopped} stopped"));
    }
    if unanswering > 0 {
        parts.push(format!("{unanswering} not responding"));
    }
    if parts.is_empty() {
        if let Some(load) = load {
            parts.push(count(
                usize::try_from(load.total_tasks).unwrap_or(usize::MAX),
                "thread",
                "threads",
            ));
            parts.push(format!("{} signed in", load.users));
        }
    }
    parts.join(" \u{b7} ")
}

/// `value` as a permille of `largest`, which is not zero.
fn share_of(value: u16, largest: u16) -> u16 {
    u16::try_from(u32::from(value) * 1_000 / u32::from(largest)).unwrap_or(1_000)
}

/// The pitch `rows` are laid at: the tallest of them, and a gap.
fn pitch(rows: &[MetricTile], look: &Look<'_>) -> u32 {
    rows.iter()
        .map(|row| row.measured_height(look.scale, look.theme))
        .max()
        .unwrap_or(0)
        .saturating_add(look.gap())
}

/// How many columns a list `width` wide is laid in.
fn columns(width: u32, look: &Look<'_>) -> u32 {
    let gap = look.gap();
    let narrowest = look.scale.scale_length(COLUMN_MIN);
    (width.saturating_add(gap) / narrowest.saturating_add(gap).max(1)).max(1)
}

/// How wide each of `columns` is across `width`, `gap` apart.
fn column_width(width: u32, columns: u32, gap: u32) -> u32 {
    width.saturating_sub(gap.saturating_mul(columns.saturating_sub(1))) / columns.max(1)
}

/// How many rows `height` seats, `columns` to a line laid at `pitch`.
fn seats(height: u32, (pitch, columns): (u32, u32), gap: u32) -> usize {
    let lines = height.saturating_add(gap) / pitch.max(1);
    usize::try_from(lines.saturating_mul(columns)).unwrap_or(usize::MAX)
}

/// Lay `rows` across and down `rect`, `columns` to a line laid at `pitch`, as
/// many as it seats whole, answering how many that was.
fn grid(
    surface: &mut Surface,
    rect: Rect,
    look: &Look<'_>,
    rows: &[MetricTile],
    (pitch, columns): (u32, u32),
) -> usize {
    let gap = look.gap();
    let columns = columns.max(1);
    let shown = seats(rect.height, (pitch, columns), gap).min(rows.len());
    let width = column_width(rect.width, columns, gap);
    for (index, row) in (0u32..).zip(&rows[..shown]) {
        let x = (index % columns).saturating_mul(width.saturating_add(gap));
        let y = (index / columns).saturating_mul(pitch);
        row.render(
            surface,
            Rect::new(
                rect.left().saturating_add(to_i32(x)),
                rect.top().saturating_add(to_i32(y)),
                width,
                row.measured_height(look.scale, look.theme),
            ),
            look.scale,
            look.theme,
            None,
        );
    }
    shown
}

/// The rows of a list of `total`, built for the width of the columns they
/// fall in, across and down `content`, and — where some are left undrawn, by
/// the report or by the room — how many, at its foot.
///
/// The foot is reserved only when something will be left out, so a list that
/// fits whole loses no row to a line that would say nothing.
fn list(
    surface: &mut Surface,
    content: Rect,
    look: &Look<'_>,
    total: usize,
    rows: impl FnOnce(u32) -> Vec<MetricTile>,
) {
    let gap = look.gap();
    let columns = columns(content.width, look);
    let rows = rows(column_width(content.width, columns, gap));
    let layout = (pitch(&rows, look), columns);
    if rows.len() == total && seats(content.height, layout, gap) >= rows.len() {
        let _ = grid(surface, content, look, &rows, layout);
        return;
    }
    let foot = look
        .font(TextRole::Caption)
        .line_height()
        .saturating_add(gap);
    let room = Rect::new(
        content.left(),
        content.top(),
        content.width,
        content.height.saturating_sub(foot),
    );
    let drawn = grid(surface, room, look, &rows, layout);
    more(surface, content, look, total, drawn);
}

/// The storage devices, the least healthy first, and how many more there are.
fn storage(
    surface: &mut Surface,
    content: Rect,
    look: &Look<'_>,
    storage: Option<&MachineStorage>,
) {
    let Some(storage) = storage else {
        look.note(surface, content, "The mount table could not be read");
        return;
    };
    if storage.total() == 0 {
        look.note(surface, content, "No storage devices");
        return;
    }
    list(
        surface,
        content,
        look,
        usize::from(storage.total()),
        |width| {
            let fit = (width, look.font(TextRole::Body));
            storage
                .devices()
                .map(|device| device_row(device, fit))
                .collect()
        },
    );
}

/// One storage device: how full, against how it is faring and how fast.
fn device_row(device: &MachineDevice, fit: (u32, BitmapFont)) -> MetricTile {
    let used = device.capacity.map(|capacity| {
        let share = u128::from(capacity.used_bytes()) * 1_000 / u128::from(capacity.total_bytes());
        u16::try_from(share).unwrap_or(1_000)
    });
    let health = device.availability.health();
    detailed(
        MetricTile::new(
            device.name.as_str(),
            used.map_or_else(|| String::from(UNREAD), percent),
            PressureKind::Disk,
        )
        .with_layout(MetricLayout::Inline)
        .with_instrument(MetricInstrument::Track(
            used.map_or(MeterValue::Unmeasured, |share| {
                MeterValue::Measured(ProgressValue::new(share))
            }),
        ))
        .with_pressure(pressure(
            health != VolumeHealth::Healthy,
            PressureKind::Disk,
        ))
        .unplated(),
        &device_facts(device),
        fit,
    )
}

/// What a device's line of detail says: how it is faring, how fast, and how
/// full — in that order, so a line too short for all of them loses what
/// matters least.
fn device_facts(device: &MachineDevice) -> Vec<String> {
    let mut facts = Vec::new();
    let health = device.availability.health();
    if health != VolumeHealth::Healthy {
        facts.push(String::from(volume_health_name(health)));
    }
    if let Some(read) = device.read_rate {
        facts.push(format!("{} read", format_rate(read)));
    }
    if let Some(write) = device.write_rate {
        facts.push(format!("{} write", format_rate(write)));
    }
    if let Some(capacity) = device.capacity {
        let (figure, of) = byte_parts(capacity.used_bytes(), capacity.total_bytes());
        facts.push(format!("{figure} {of}"));
    }
    facts
}

/// `tile` with as many of `facts` as fit `width` whole in `font` as its line
/// of detail — the first always — so a line too short is cut between facts,
/// never through one that would then say nothing.
fn detailed(tile: MetricTile, facts: &[String], (width, font): (u32, BitmapFont)) -> MetricTile {
    let Some((first, rest)) = facts.split_first() else {
        return tile;
    };
    let mut line = first.clone();
    for fact in rest {
        let longer = format!("{line} \u{b7} {fact}");
        if font.text_width(&longer) > width {
            break;
        }
        line = longer;
    }
    tile.with_detail(line)
}

/// The network interfaces, and how many more there are.
fn network(
    surface: &mut Surface,
    content: Rect,
    look: &Look<'_>,
    network: Option<&MachineNetwork>,
) {
    let Some(network) = network else {
        look.note(surface, content, "The network stack did not answer");
        return;
    };
    if network.total() == 0 {
        look.note(surface, content, "No network interfaces");
        return;
    }
    list(
        surface,
        content,
        look,
        usize::from(network.total()),
        |width| {
            let fit = (width, look.font(TextRole::Body));
            network
                .interfaces()
                .map(|interface| interface_row(interface, fit))
                .collect()
        },
    );
}

/// One interface: what it carries in all, against its link and each way.
fn interface_row(interface: &MachineInterface, fit: (u32, BitmapFont)) -> MetricTile {
    let total = interface
        .receive_rate
        .zip(interface.send_rate)
        .map(|(received, sent)| received.saturating_add(sent));
    detailed(
        MetricTile::new(
            interface.name.as_str(),
            total.map_or_else(|| String::from(UNREAD), format_rate),
            PressureKind::Network,
        )
        .with_layout(MetricLayout::Inline)
        .unplated(),
        &interface_facts(interface),
        fit,
    )
}

/// What an interface's line of detail says: a link that is down, what it
/// carries each way, and a link that is up — the alarm first, the expected
/// last.
fn interface_facts(interface: &MachineInterface) -> Vec<String> {
    let mut facts = Vec::new();
    if interface.link_up == Some(false) {
        facts.push(String::from("link down"));
    }
    if let Some(received) = interface.receive_rate {
        facts.push(format!("{} in", format_rate(received)));
    }
    if let Some(sent) = interface.send_rate {
        facts.push(format!("{} out", format_rate(sent)));
    }
    if interface.link_up == Some(true) {
        facts.push(String::from("link up"));
    }
    facts
}

/// How many of `total` were not drawn, said at the foot of `rect`.
fn more(surface: &mut Surface, rect: Rect, look: &Look<'_>, total: usize, drawn: usize) {
    let hidden = total.saturating_sub(drawn);
    if hidden == 0 {
        return;
    }
    let caption = look.font(TextRole::Caption);
    let foot = Rect::new(
        rect.left(),
        rect.bottom().saturating_sub(to_i32(caption.line_height())),
        rect.width,
        caption.line_height(),
    );
    line(
        surface,
        caption,
        &format!("+{hidden} more"),
        foot,
        true,
        look.muted(),
    );
}

#[cfg(test)]
#[path = "board_tests.rs"]
mod tests;
