//! System Monitor, a TAIRiX screensaver: the machine's own readings at a
//! glance — its processors, memory, tasks, storage and network, and whatever
//! needs attention — set out to be read from across a room.
//!
//! The readings are the Switchboard's, handed over by the host through the
//! machine feed (`plans/NEW-SWITCHBOARD.md` S14), each task's name already
//! withheld as the desktop's settings say. Between readings nothing moves and
//! nothing is drawn: the board costs the machine it reports on one frame per
//! reading, of the parts whose readings changed.
//!
//! It never passes readings off as live once they stop: after
//! [`STALE_PERIODS`] of the monitor's own period without one, the verdict says
//! when they stopped and the panels dim. Each minute the board steps round a
//! small orbit against burn-in, as the time beside the machine's name moves
//! on.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

mod board;
#[cfg(test)]
mod fixtures;
mod verdict;

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::saver_ipc::SAVER_TIME_TEXT_MAX;
use tairix_abi::switchboard_ipc::{MachineHost, MachineReport};
use tairix_abi::Errno;
use tairix_geometry::{Rect, Region};
use tairix_raster::Surface;
use tairix_saver::{erase, DateShown, Frame, Host, NextFrame, Scene, Setup, Told};
use tairix_theme::motion::SceneClock;
use tairix_theme::{Accessibility, DesktopText, Motion, Theme};

use board::{detail_line, Board, Part, Readings, ORBIT};
use verdict::Verdict;

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
pub struct SystemMonitor {
    /// The theme the board is drawn in: the desktop's dark one, on the user's
    /// own text and axes, since a screensaver is black whatever the desktop's
    /// appearance.
    theme: Theme,
    board: Board,
    screen: (u32, u32),
    /// The machine's name until a report names it.
    host: String,
    /// The reading the board was last drawn telling; `None` before the first
    /// frame.
    shown: Option<Told>,
    report: Option<MachineReport>,
    /// Whether a report came since the last frame, which stamps its arrival.
    fresh: bool,
    /// When the latest report came, and the minute it came, as the clock
    /// spelled it.
    arrived: Option<(u64, BoundedText<0, SAVER_TIME_TEXT_MAX>)>,
    /// The parts whose readings changed since the last frame.
    owed: Vec<Part>,
    /// When the board began waiting for its first reading.
    started_ns: u64,
    /// Whether the monitor that reads the machine is running.
    monitored: bool,
    stale: bool,
    orbit: usize,
    frame_ns: u64,
}

impl SystemMonitor {
    /// The board `setup` describes.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a screen too small to seat one,
    /// [`Errno::OutOfMemory`] when the heap will not hold its bookkeeping.
    pub fn new(setup: &Setup<'_>) -> Result<Self, Errno> {
        let start = setup.start;
        let screen = (start.region.width(), start.region.height());
        let theme = board_theme(&setup.theme());
        let board = Board::new(screen, shifted(0), &theme).ok_or(Errno::OutOfRange)?;
        let mut owed = Vec::new();
        owed.try_reserve_exact(Part::ALL.len())
            .map_err(|_| Errno::OutOfMemory)?;
        Ok(Self {
            theme,
            board,
            screen,
            host: String::from(start.machine.as_str()),
            shown: None,
            report: None,
            fresh: false,
            arrived: None,
            owed,
            started_ns: start.now_ns,
            monitored: true,
            stale: false,
            orbit: 0,
            frame_ns: SceneClock::frame_on(start.refresh),
        })
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

    /// Draw `parts` onto `surface`, each in its own slot, as they stand.
    fn paint_parts(&self, surface: &mut Surface, parts: &[Part]) {
        let verdict = self.verdict();
        let (time, date) = self
            .shown
            .as_ref()
            .map_or(("", ""), |told| (told.time(), told.date()));
        let detail = detail_line(self.report.as_ref().and_then(|report| report.uptime), date);
        let host = self
            .report
            .as_ref()
            .and_then(|report| report.host.as_ref())
            .map_or(self.host.as_str(), MachineHost::as_str);
        let readings = Readings {
            host,
            time,
            detail: &detail,
            verdict: &verdict,
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
            (Some(_), Some((_, at))) if self.stale => Verdict::Stale { at: *at },
            (Some(report), _) => Verdict::of(report),
            (None, _) if self.stale => Verdict::Silent,
            (None, _) => Verdict::Waiting,
        }
    }

    /// Owe the next frame `part`.
    fn owe(&mut self, part: Part) {
        if !self.owed.contains(&part) {
            self.owed.push(part);
        }
    }
}

impl Scene for SystemMonitor {
    /// Draw what changed since the last frame: the whole board for the first
    /// frame, the minute turning — which also steps the orbit — or the
    /// readings going stale, and otherwise the parts a reading changed.
    fn frame(
        &mut self,
        surface: &mut Surface,
        damage: &mut Region,
        frame: &Frame<'_>,
        _host: &mut Host<'_>,
    ) -> NextFrame {
        let now_ns = frame.now_ns;
        let wall = frame.wall;
        if core::mem::take(&mut self.fresh) {
            self.arrived = Some((now_ns, wall.time));
        }
        let mut whole = self.shown.is_none();
        if self.stale_ns().is_some_and(|stale| now_ns >= stale) {
            self.stale = true;
            whole = true;
        }
        let told = Told::of(wall, DateShown::Numeric);
        if self.shown != Some(told) {
            if self.shown.is_some() {
                self.orbit = (self.orbit + 1) % ORBIT_STEPS.len();
                if let Some(board) = Board::new(self.screen, shifted(self.orbit), &self.theme) {
                    self.board = board;
                }
            }
            self.shown = Some(told);
            whole = true;
        }
        if whole {
            let screen = Rect::new(0, 0, self.screen.0, self.screen.1);
            erase(surface, screen);
            damage.add(screen);
            self.paint_parts(surface, &Part::ALL);
        } else {
            for part in &self.owed {
                damage.add(self.board.slot(*part));
            }
            self.paint_parts(surface, &self.owed);
        }
        self.owed.clear();
        let reading = frame.reading_changes_ns(self.frame_ns);
        NextFrame::At(self.stale_ns().map_or(reading, |stale| stale.min(reading)))
    }

    /// Adopt `report`, owing the next frame the parts whose readings it
    /// changed.
    fn machine(&mut self, report: &MachineReport) {
        let revived = self.stale || !self.monitored;
        let changed = Part::ALL.map(|part| {
            revived
                || self
                    .report
                    .as_ref()
                    .is_none_or(|held| differs(part, held, report))
        });
        let was = self.verdict();
        self.report = Some(report.clone());
        self.fresh = true;
        self.monitored = true;
        self.stale = false;
        for (part, changed) in Part::ALL.into_iter().zip(changed) {
            if changed {
                self.owe(part);
            }
        }
        if self.verdict() != was {
            self.owe(Part::Header);
        }
    }

    /// The monitor stopped: whatever the board holds is no longer live.
    fn machine_lost(&mut self) {
        if !self.monitored {
            return;
        }
        self.monitored = false;
        for part in Part::ALL {
            self.owe(part);
        }
    }
}

/// Whether `part`'s readings differ between `held` and `fresh`.
fn differs(part: Part, held: &MachineReport, fresh: &MachineReport) -> bool {
    match part {
        Part::Header => {
            held.host != fresh.host || held.uptime.map(minutes) != fresh.uptime.map(minutes)
        }
        Part::Cpu => held.cpu != fresh.cpu,
        Part::Memory => held.memory != fresh.memory,
        Part::Tasks => {
            held.tasks != fresh.tasks
                || held.scope != fresh.scope
                || held.cpu.load != fresh.cpu.load
        }
        Part::Storage => held.storage != fresh.storage,
        Part::Network => held.network != fresh.network,
    }
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

/// The theme the board is drawn in: the desktop's dark one, on the axes and
/// in the text `active` is drawn in.
fn board_theme(active: &Theme) -> Theme {
    let fonts = active.fonts();
    Theme::dark()
        .with_axes(Accessibility {
            contrast: active.contrast(),
            density: active.density(),
            motion: if active.motion().reduced_motion() {
                Motion::Reduced
            } else {
                Motion::Full
            },
        })
        .with_text(DesktopText::new(fonts.ui_family(), fonts.base_size_px()).ok())
}

#[cfg(test)]
mod tests;
