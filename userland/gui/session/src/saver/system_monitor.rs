//! The System Monitor screensaver: the machine's own readings at a glance —
//! its processors, memory, tasks, storage and network, and whatever needs
//! attention — set out to be read from across a room.
//!
//! The readings are the Switchboard's, published to the session while the
//! board is up (`plans/NEW-SWITCHBOARD.md` S14). The session draws them, so
//! nothing but the session draws over the lock. Between readings nothing
//! moves and nothing is drawn: the board costs the machine it reports on one
//! repaint per reading, of the parts whose readings changed.
//!
//! It never passes readings off as live once they stop: after
//! [`STALE_PERIODS`] of the monitor's own period without one, the verdict says
//! when they stopped and the panels dim. Each minute the board steps round a
//! small orbit against burn-in, as the time beside the machine's name moves
//! on.

mod board;
#[cfg(test)]
pub(crate) mod fixtures;
mod verdict;

use alloc::string::String;

use tairix_abi::switchboard_ipc::{MachineHost, MachineReport};
use tairix_abi::time::WallClockReading;
use tairix_browse::format_date;
use tairix_theme::{Accessibility, Motion, Theme};
use tairix_wallpaper::SystemMonitorOptions;
use tairix_wm::{Compositor, Rect, Region, Surface, WindowId};

use board::{detail_line, erase, Board, Part, Readings, ORBIT};
use verdict::Verdict;

use super::clock::SaverIdentity;
use super::telling::{DateSpelling, Telling};

/// How many of the monitor's own periods may pass without a reading before
/// the board says the readings have stopped: one late reading is the
/// machine being busy, three is the monitor not answering.
pub const STALE_PERIODS: u64 = 3;

/// How long the board waits for its first reading before saying the monitor
/// is not answering: long enough for one it has just started to load.
pub const FIRST_READING_NS: u64 = 15_000_000_000;

/// The positions the board steps round, a minute at each, in multiples of
/// [`ORBIT`]: every pixel it lights moves on, and none drifts far.
const ORBIT_STEPS: [(i32, i32); 8] = [
    (0, 0),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
];

/// The System Monitor screensaver.
pub(super) struct SystemMonitor {
    /// The theme the board is drawn in: the desktop's dark one, on the user's
    /// own accessibility axes, since a screensaver is black whatever the
    /// desktop's appearance.
    theme: Theme,
    board: Board,
    screen: (u32, u32),
    /// The machine's name until a report names it.
    host: String,
    name_tasks: bool,
    telling: Telling,
    report: Option<MachineReport>,
    /// When the latest report came, and the minute it came, as the clock
    /// spelled it.
    arrived: Option<(u64, String)>,
    /// When the board began waiting for its first reading.
    started_ns: u64,
    /// Whether the monitor that reads the machine is running.
    monitored: bool,
    stale: bool,
    orbit: usize,
}

impl SystemMonitor {
    /// A board for a `screen`, telling `wall` as of `now_ns`, naming the
    /// machine `identity` names until a report does; `None` for a screen too
    /// small to seat one.
    pub(super) fn new(
        identity: &SaverIdentity,
        active: &Theme,
        screen: (u32, u32),
        (wall, now_ns): (Option<WallClockReading>, u64),
        options: SystemMonitorOptions,
    ) -> Option<Self> {
        let theme = board_theme(active);
        let board = Board::new(screen, shifted(0), &theme)?;
        let spelling: DateSpelling = format_date;
        Some(Self {
            theme,
            board,
            screen,
            host: identity.host.clone(),
            name_tasks: options.tasks,
            telling: Telling::new(Some(spelling), (wall, now_ns)),
            report: None,
            arrived: None,
            started_ns: now_ns,
            monitored: true,
            stale: false,
            orbit: 0,
        })
    }

    /// When the board next owes a frame: the minute turning, or the moment
    /// its readings would go stale.
    pub(super) fn due_ns(&self) -> u64 {
        self.telling
            .tick_ns()
            .min(self.stale_ns().unwrap_or(u64::MAX))
    }

    /// When the board's readings go stale, if they are live now.
    fn stale_ns(&self) -> Option<u64> {
        if self.stale || !self.monitored {
            return None;
        }
        Some(match (&self.report, &self.arrived) {
            (Some(report), Some((at, _))) => {
                at.saturating_add(report.period.as_nanos().saturating_mul(STALE_PERIODS))
            }
            _ => self.started_ns.saturating_add(FIRST_READING_NS),
        })
    }

    /// Draw the board whole onto `surface`, as it stands.
    pub(super) fn paint(&self, surface: &mut Surface) {
        let (width, height) = self.screen;
        erase(surface, Rect::new(0, 0, width, height));
        self.paint_parts(surface, &Part::ALL);
    }

    /// Draw `parts` onto `surface`, each in its own slot, as they stand.
    fn paint_parts(&self, surface: &mut Surface, parts: &[Part]) {
        let verdict = self.verdict();
        let detail = detail_line(
            self.report.as_ref().and_then(|report| report.uptime),
            self.telling.date(),
        );
        let host = self
            .report
            .as_ref()
            .and_then(|report| report.host.as_ref())
            .map_or(self.host.as_str(), MachineHost::as_str);
        let readings = Readings {
            host,
            time: self.telling.time(),
            detail: &detail,
            verdict: &verdict,
            name_tasks: self.name_tasks,
            stale: self.stale || !self.monitored,
        };
        for part in parts {
            board::paint(
                surface,
                &self.board,
                *part,
                &self.theme,
                self.report.as_ref(),
                &readings,
            );
        }
    }

    /// What the board says first.
    fn verdict(&self) -> Verdict {
        if !self.monitored {
            return Verdict::Unmonitored;
        }
        match (&self.report, &self.arrived) {
            (Some(_), Some((_, at))) if self.stale => Verdict::Stale { at: at.clone() },
            (Some(report), _) => Verdict::of(report),
            (None, _) if self.stale => Verdict::Silent,
            (None, _) => Verdict::Waiting,
        }
    }

    /// Adopt `report`, which came at `now_ns`, repainting the parts whose
    /// readings it changed.
    pub(super) fn adopt(
        &mut self,
        report: MachineReport,
        now_ns: u64,
        wm: WindowId,
        compositor: &mut Compositor,
    ) {
        let revived = self.stale || !self.monitored || self.report.is_none();
        let changed = self
            .report
            .as_ref()
            .map_or(Part::ALL.to_vec(), |held| changed_parts(held, &report));
        let was = self.verdict();
        self.report = Some(report);
        self.arrived = Some((now_ns, String::from(self.telling.time())));
        self.monitored = true;
        self.stale = false;
        let mut parts = if revived { Part::ALL.to_vec() } else { changed };
        if self.verdict() != was && !parts.contains(&Part::Header) {
            parts.push(Part::Header);
        }
        self.repaint(&parts, wm, compositor);
    }

    /// The monitor stopped: whatever the board holds is no longer live.
    pub(super) fn unmonitored(&mut self, wm: WindowId, compositor: &mut Compositor) {
        if !self.monitored {
            return;
        }
        self.monitored = false;
        self.repaint(&Part::ALL, wm, compositor);
    }

    /// Step the board to `now_ns`: the minute turning — which also steps the
    /// orbit — or its readings going stale. `wall` is asked for the time only
    /// at a turn.
    pub(super) fn advance(
        &mut self,
        now_ns: u64,
        wm: WindowId,
        compositor: &mut Compositor,
        wall: &mut dyn FnMut() -> Option<WallClockReading>,
    ) {
        if now_ns < self.due_ns() {
            return;
        }
        let mut whole = false;
        if self.stale_ns().is_some_and(|stale| now_ns >= stale) {
            self.stale = true;
            whole = true;
        }
        if now_ns >= self.telling.tick_ns() {
            self.telling.read(wall(), now_ns);
            self.orbit = (self.orbit + 1) % ORBIT_STEPS.len();
            if let Some(board) = Board::new(self.screen, shifted(self.orbit), &self.theme) {
                self.board = board;
            }
            whole = true;
        }
        if whole {
            self.repaint_whole(wm, compositor);
        }
    }

    /// Repaint `parts`, each in its own slot, over what the screen holds.
    fn repaint(&self, parts: &[Part], wm: WindowId, compositor: &mut Compositor) {
        let mut damage = Region::new();
        for part in parts {
            damage.add(self.board.slot(*part));
        }
        let whole = Rect::new(0, 0, self.screen.0, self.screen.1);
        let _ = compositor.repaint_window(wm, self.screen, &damage, |surface, rects| {
            // A buffer the compositor no longer kept comes back to be drawn
            // whole, whatever this repaint asked for.
            if rects.contains(&whole) {
                self.paint(surface);
            } else {
                self.paint_parts(surface, parts);
            }
        });
    }

    /// Repaint the whole screen: the board has moved, or every part's
    /// standing has.
    fn repaint_whole(&self, wm: WindowId, compositor: &mut Compositor) {
        let mut damage = Region::new();
        damage.add(Rect::new(0, 0, self.screen.0, self.screen.1));
        let _ = compositor.repaint_window(wm, self.screen, &damage, |surface, _| {
            self.paint(surface);
        });
    }
}

/// The parts whose readings differ between `held` and `fresh`.
fn changed_parts(held: &MachineReport, fresh: &MachineReport) -> alloc::vec::Vec<Part> {
    let mut parts = alloc::vec::Vec::new();
    if held.host != fresh.host || held.uptime.map(minutes) != fresh.uptime.map(minutes) {
        parts.push(Part::Header);
    }
    if held.cpu != fresh.cpu {
        parts.push(Part::Cpu);
    }
    if held.memory != fresh.memory {
        parts.push(Part::Memory);
    }
    if held.tasks != fresh.tasks || held.scope != fresh.scope || held.cpu.load != fresh.cpu.load {
        parts.push(Part::Tasks);
    }
    if held.storage != fresh.storage {
        parts.push(Part::Storage);
    }
    if held.network != fresh.network {
        parts.push(Part::Network);
    }
    parts
}

/// A span to the minute, the finest an uptime is spelled at.
fn minutes(span: tairix_abi::Duration64) -> i64 {
    span.secs() / 60
}

/// The board's offset at `step` of its orbit, in logical pixels.
fn shifted(step: usize) -> (i32, i32) {
    let (x, y) = ORBIT_STEPS[step % ORBIT_STEPS.len()];
    let reach = i32::try_from(ORBIT).unwrap_or(0);
    (x * reach, y * reach)
}

/// The theme the board is drawn in: the desktop's dark one, on the axes
/// `active` is drawn on.
fn board_theme(active: &Theme) -> Theme {
    Theme::dark().with_axes(Accessibility {
        contrast: active.contrast(),
        density: active.density(),
        motion: if active.motion().reduced_motion() {
            Motion::Reduced
        } else {
            Motion::Full
        },
    })
}

#[cfg(test)]
#[path = "system_monitor_tests.rs"]
mod tests;
