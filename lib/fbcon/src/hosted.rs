//! The framebuffer console a kernel hosts.
//!
//! One console per port, behind one render lock, painting the scan-out
//! surface it was given — the firmware's, a kernel block a display engine
//! reaches, or none yet. A port supplies only what its silicon decides
//! ([`ConsolePort`]): whether an extent translates for a kernel write, where
//! the direct map puts a physical block, how written pixels reach a
//! scan-out that does not snoop the caches, and which interrupt mask the
//! lock holds.
//!
//! # The slot
//!
//! The state lives in a plain cell rather than inside the lock, because a
//! port may record its boot surface before its MMU is on, where an atomic
//! read-modify-write is not defined. Discipline: [`FramebufferConsole::publish`]
//! writes the cell once on the single boot CPU before anything reads it;
//! every later access holds the render lock. A fatal path that has stopped
//! every other CPU is the one exception ([`FramebufferConsole::fatal`]).
//!
//! # Hosting
//!
//! A console with no surface still interprets everything written to it into
//! its grid. [`FramebufferConsole::adopt`] gives it one — the block a display
//! driver scans — at that block's geometry, re-cutting the grid so the text
//! the boot wrote is what appears. An adopted console can record the rows it
//! paints ([`FramebufferConsole::take_damage`]) for an engine that must be
//! told what changed, and calls the hook installed with
//! [`FramebufferConsole::on_damage`] — after the render lock is released, so
//! the lock stays a leaf — whenever rows become owed.

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use tairix_abi::TerminalSize;
use tairix_sync::{FnCell, IrqSafeSpinLock, IrqSafeSpinLockGuard};

use crate::port::{ConsolePort, ConsoleSurface};
use crate::{fill_surface, merge_bands, DirtyBand, Geometry, Grid, Surface, TextConsole};

/// Where a hosted console's cell grids come from: the heap, in a kernel.
pub trait GridSource: 'static {
    /// One grid.
    type Grid: Grid;

    /// A blank grid of `cells` cells, or `None` when none can be had.
    fn blank(cells: usize) -> Option<Self::Grid>;
}

/// Why [`FramebufferConsole::adopt`] refused a surface.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AdoptRefused {
    /// No console is attached to host.
    NoConsole,
    /// The mode cannot hold one glyph cell, or its stride is not whole
    /// pixels.
    Geometry,
    /// The surface does not translate for a kernel write.
    Unreachable,
    /// The device does not snoop the caches, and this port cannot clean them
    /// for it.
    Incoherent,
    /// The grids for the new geometry could not be allocated.
    OutOfMemory,
}

/// How a paint takes the render lock: always, on a fatal path that stopped
/// every other CPU (so a holder never runs again), or only where it is free
/// on one that did not.
const PAINT_LOCKED: u8 = 0;
const PAINT_BUSTED: u8 = 1;
const PAINT_IF_FREE: u8 = 2;

/// The lock a paint holds.
enum Hold<'a, I: tairix_sync::InterruptControl> {
    Locked(IrqSafeSpinLockGuard<'a, (), I>),
    /// Held by a CPU that never runs again.
    Busted,
    /// Held by one that may: the paint is skipped.
    Contended,
}

/// The surface a console paints.
struct Scanout {
    /// Where the kernel reaches the first pixel.
    base: NonNull<u32>,
    placement: ConsoleSurface,
    geometry: Geometry,
}

impl Scanout {
    fn pixel_count(&self) -> usize {
        self.geometry.pixel_count()
    }

    fn bytes(&self) -> usize {
        self.pixel_count() * 4
    }

    fn stride_bytes(&self) -> usize {
        self.geometry.stride_px as usize * 4
    }
}

/// The pixels of `scanout`.
///
/// # Safety
///
/// The render lock is held (or a fatal path busted it with every other CPU
/// stopped), the extent translates for a kernel write, and no other reference
/// into it is live for `'a`.
unsafe fn pixels<'a>(scanout: &Scanout) -> &'a mut [u32] {
    // SAFETY: the caller's contract: the extent is writable, holds
    // `pixel_count` pixels, and nothing else references it.
    unsafe { core::slice::from_raw_parts_mut(scanout.base.as_ptr(), scanout.pixel_count()) }
}

struct State<G> {
    scanout: Option<Scanout>,
    console: Option<TextConsole<G>>,
    /// Rows painted since the host last took them, when it asked to be told.
    damage: Option<DirtyBand>,
    tracks_damage: bool,
}

/// The one framebuffer console a port hosts.
pub struct FramebufferConsole<P: ConsolePort, S: GridSource> {
    slot: UnsafeCell<Option<State<S::Grid>>>,
    /// A console is attached: set, release, once the slot is complete, and
    /// cleared by [`Self::retire`]; every write path checks it, acquire,
    /// before the lock.
    active: AtomicBool,
    /// The slot holds a scan-out: stored, release, wherever the lock changes
    /// it once attached, so a fatal path can ask without taking the lock.
    scanned: AtomicBool,
    lock: IrqSafeSpinLock<(), P::Irq>,
    /// How a paint takes the lock: `PAINT_LOCKED` until a fatal path says.
    paint_lock: AtomicU8,
    /// A fatal reclaim found the lock held, so the report's first paint
    /// takes the screen back.
    reclaim_owed: AtomicBool,
    /// Called once rows become owed to a host that tracks them.
    damage_hook: FnCell<fn()>,
    port: PhantomData<fn() -> (P, S)>,
}

// SAFETY: every access to `slot` after `publish` holds `lock`, and `publish`
// runs on the single boot CPU before any other access exists (see the module
// discipline); a busted paint runs only once every other CPU has stopped. The
// other fields are atomics and the lock itself. The scan-out pointer the slot
// holds is reached only under the same discipline.
unsafe impl<P: ConsolePort, S: GridSource> Sync for FramebufferConsole<P, S> {}

impl<P: ConsolePort, S: GridSource> Default for FramebufferConsole<P, S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: ConsolePort, S: GridSource> FramebufferConsole<P, S> {
    /// A console with no surface and none attached.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slot: UnsafeCell::new(None),
            active: AtomicBool::new(false),
            scanned: AtomicBool::new(false),
            lock: IrqSafeSpinLock::new(()),
            paint_lock: AtomicU8::new(PAINT_LOCKED),
            reclaim_owed: AtomicBool::new(false),
            damage_hook: FnCell::empty(),
            port: PhantomData,
        }
    }

    /// Record the boot surface the kernel reaches at `base` and clear it.
    ///
    /// Uses no atomic operation, so a port may call it before its MMU is on.
    /// `false`, leaving nothing recorded, for a mode no glyph fits.
    ///
    /// # Safety
    ///
    /// The single boot CPU calls this before any other method of this console
    /// runs, and `[base, base + stride × height)` is scan-out memory the caller
    /// owns exclusively and can write at `base` now.
    #[must_use]
    pub unsafe fn publish(&self, base: NonNull<u32>, placement: ConsoleSurface) -> bool {
        let Some(scanout) = scanout_at(base, placement) else {
            return false;
        };
        // SAFETY: the caller's contract — exclusive, writable scan-out memory
        // of the mode's extent, which `scanout_at` sized the slice to.
        fill_surface(unsafe { pixels(&scanout) });
        // SAFETY: single boot CPU, before any other access (the contract).
        unsafe {
            *self.slot.get() = Some(State {
                scanout: Some(scanout),
                console: None,
                damage: None,
                tracks_damage: false,
            });
        }
        true
    }

    /// Attach the console: to the published surface at its geometry, or with
    /// no surface at `unhosted` when none was published, until a display
    /// driver hosts it ([`Self::adopt`]).
    ///
    /// `false` when there is neither, the published surface does not
    /// translate, or the grid source refuses; the port then keeps its serial
    /// console. Needs the lock's atomics.
    pub fn attach(&self, unhosted: Option<Geometry>) -> bool {
        let geometry = {
            let _guard = self.lock.lock();
            // SAFETY: render lock held.
            match unsafe { (*self.slot.get()).as_ref() } {
                Some(State {
                    scanout: Some(scanout),
                    console: None,
                    ..
                }) => scanout.geometry,
                None => match unhosted {
                    Some(geometry) => geometry,
                    None => return false,
                },
                Some(_) => return false,
            }
        };
        let Some(mut console) = console_for::<S>(geometry) else {
            return false;
        };
        let _guard = self.lock.lock();
        // SAFETY: render lock held.
        let slot = unsafe { &mut *self.slot.get() };
        let state = slot.get_or_insert(State {
            scanout: None,
            console: None,
            damage: None,
            tracks_damage: false,
        });
        if state.console.is_some() {
            return false;
        }
        if let Some(scanout) = &state.scanout {
            if !P::reachable(scanout.base, scanout.bytes()) {
                // Painting a surface the active regime does not map would
                // fault inside the renderer with the lock held.
                return false;
            }
            // SAFETY: lock held; reachability proved just above.
            if let Some(band) = console.clear(unsafe { pixels(scanout) }) {
                publish_band::<P>(scanout, band);
            }
        }
        self.scanned
            .store(state.scanout.is_some(), Ordering::Release);
        state.console = Some(console);
        self.active.store(true, Ordering::Release);
        true
    }

    /// Whether a console is attached.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Whether the console has a surface to paint. Takes no lock, so a fatal
    /// path may ask.
    #[must_use]
    pub fn paints(&self) -> bool {
        self.is_active() && self.scanned.load(Ordering::Acquire)
    }

    /// Call `hook`, outside the render lock, whenever rows become owed to a
    /// host that tracks them.
    pub fn on_damage(&self, hook: fn()) {
        self.damage_hook.install(hook);
    }

    /// Host the console on the surface the kernel reaches at `base`, re-cutting it
    /// to that surface's geometry and repainting it in the disposition it
    /// holds. With `tracks_damage`, every row painted from now on is owed to
    /// [`Self::take_damage`]. `snooped` says whether the device reading the
    /// surface snoops the caches.
    ///
    /// The surface it painted before, if any, is the caller's again.
    ///
    /// # Errors
    ///
    /// The [`AdoptRefused`] reason; the console keeps the surface it had.
    pub fn adopt(
        &self,
        base: NonNull<u32>,
        placement: ConsoleSurface,
        tracks_damage: bool,
        snooped: bool,
    ) -> Result<(), AdoptRefused> {
        if !self.is_active() {
            return Err(AdoptRefused::NoConsole);
        }
        if !snooped && !P::CLEANS {
            return Err(AdoptRefused::Incoherent);
        }
        let scanout = scanout_at(base, placement).ok_or(AdoptRefused::Geometry)?;
        if !P::reachable(base, scanout.bytes()) {
            return Err(AdoptRefused::Unreachable);
        }
        let (main, alt) = grids::<S>(scanout.geometry).ok_or(AdoptRefused::OutOfMemory)?;
        let owed = self.host(scanout, tracks_damage, main, alt)?;
        self.notify(owed);
        Ok(())
    }

    /// The locked half of [`Self::adopt`]: `true` when rows became owed.
    fn host(
        &self,
        scanout: Scanout,
        tracks_damage: bool,
        main: S::Grid,
        alt: S::Grid,
    ) -> Result<bool, AdoptRefused> {
        let _guard = self.lock.lock();
        // SAFETY: `active` was observed, so the slot is complete, and the
        // render lock is held.
        let state = unsafe { (*self.slot.get()).as_mut() }.ok_or(AdoptRefused::NoConsole)?;
        let console = state.console.take().ok_or(AdoptRefused::NoConsole)?;
        let mut console = console.resized(scanout.geometry, main, alt);
        state.damage = None;
        state.tracks_damage = tracks_damage;
        let disposition = console.surface();
        // SAFETY: lock held; reachability proved above, and the surface is
        // not yet installed, so nothing else references it.
        let surface = unsafe { pixels(&scanout) };
        let band = match disposition {
            Surface::Shown => console.show(surface),
            Surface::Blank => console.blank(surface),
            Surface::Hidden => None,
        };
        if let Some(band) = band {
            publish_band::<P>(&scanout, band);
            state.damage = tracks_damage.then_some(band);
        }
        state.console = Some(console);
        state.scanout = Some(scanout);
        self.scanned.store(true, Ordering::Release);
        Ok(state.damage.is_some())
    }

    /// Whether rows are owed to a host that tracks them, without taking them.
    #[must_use]
    pub fn owes_damage(&self) -> bool {
        self.is_active()
            && self
                .with_state(|state| state.damage.is_some())
                .unwrap_or(false)
    }

    /// The rows painted since the last call, for a host that asked to be
    /// told ([`Self::adopt`]).
    pub fn take_damage(&self) -> Option<DirtyBand> {
        if !self.is_active() {
            return None;
        }
        let _guard = self.lock.lock();
        // SAFETY: render lock held.
        unsafe { (*self.slot.get()).as_mut() }.and_then(|state| state.damage.take())
    }

    /// Owe the host `band` again: rows taken that never reached it.
    pub fn owe_damage(&self, band: DirtyBand) {
        if !self.is_active() {
            return;
        }
        let owed = {
            let _guard = self.lock.lock();
            // SAFETY: render lock held.
            unsafe { (*self.slot.get()).as_mut() }.is_some_and(|state| owe(state, band))
        };
        self.notify(owed);
    }

    /// Stop painting the surface at physical `phys`, keeping the text, which
    /// is repainted once the console is hosted again. `false`, leaving the
    /// console as it was, when that is not the surface it paints.
    pub fn vacate(&self, phys: u64) -> bool {
        if !self.is_active() {
            return false;
        }
        let _guard = self.lock.lock();
        // SAFETY: `active` was observed, so the slot is complete, and the
        // render lock is held.
        let Some(state) = (unsafe { (*self.slot.get()).as_mut() }) else {
            return false;
        };
        if state.scanout.as_ref().map(|scanout| scanout.placement.phys) != Some(phys) {
            return false;
        }
        state.scanout = None;
        self.scanned.store(false, Ordering::Release);
        state.damage = None;
        state.tracks_damage = false;
        true
    }

    /// Give up the surface at physical `phys` for good: nothing will host the
    /// console again, so it is no longer attached and its writes go nowhere.
    /// `false`, leaving the console as it was, when that is not the surface it
    /// paints.
    pub fn retire(&self, phys: u64) -> bool {
        if !self.vacate(phys) {
            return false;
        }
        self.active.store(false, Ordering::Release);
        true
    }

    /// Render kernel-diagnostic `bytes`, verbatim.
    pub fn write_bytes(&self, bytes: &[u8]) {
        let owed = self.paint(|console, pixels| console.write_bytes(pixels, bytes));
        self.notify(owed);
    }

    /// Render program-output `bytes`, with the console line discipline's
    /// `LF` → `CR LF`, in one repaint.
    pub fn write_output_bytes(&self, bytes: &[u8]) {
        let owed = self.paint(|console, pixels| console.write_output_bytes(pixels, bytes));
        self.notify(owed);
    }

    /// Discard everything a finished session left on the console: both grids,
    /// every pixel, and a partly received escape sequence.
    pub fn purge(&self) {
        let owed = self.paint(TextConsole::purge);
        self.notify(owed);
    }

    /// Give the scan-out to whoever the boot seat says owns it. Waits for a
    /// concurrent write, because a skipped hide would leave the console
    /// painting over a session's frame.
    pub fn set_surface(&self, surface: Surface) {
        if !self.is_active() {
            return;
        }
        let owed = {
            let _guard = self.lock.lock();
            // SAFETY: render lock held.
            unsafe { (*self.slot.get()).as_mut() }
                .is_some_and(|state| apply_surface::<P, S::Grid>(state, surface))
        };
        self.notify(owed);
    }

    /// Call the damage hook for rows that became owed, with the lock
    /// released.
    fn notify(&self, owed: bool) {
        if owed {
            if let Some(hook) = self.damage_hook.load() {
                hook();
            }
        }
    }

    /// Take the scan-out back for a fatal report. The lock may be held by the
    /// reporting CPU itself, so a contended reclaim is not waited for: it is
    /// owed to the report's first paint instead.
    pub fn reclaim_surface(&self) {
        if !self.is_active() {
            return;
        }
        let Some(_guard) = self.lock.try_lock() else {
            self.reclaim_owed.store(true, Ordering::Release);
            return;
        };
        // SAFETY: render lock held.
        if let Some(state) = unsafe { (*self.slot.get()).as_mut() } {
            apply_surface::<P, S::Grid>(state, Surface::Shown);
        }
    }

    /// Render the rest of a fatal report without waiting on the lock, which
    /// the reporting CPU's own abandoned frame may hold.
    ///
    /// With `every_cpu_stopped`, every later write renders whoever holds it:
    /// its holder never runs again, and a torn screen beats a report that
    /// hangs on its own lock. Otherwise a holder may still be painting, so a
    /// write renders only where the lock is free, and the serial line carries
    /// what the screen cannot.
    pub fn fatal(&self, every_cpu_stopped: bool) {
        let mode = if every_cpu_stopped {
            PAINT_BUSTED
        } else {
            PAINT_IF_FREE
        };
        self.paint_lock.store(mode, Ordering::Release);
    }

    /// The console's character-cell grid.
    #[must_use]
    pub fn text_grid(&self) -> Option<TerminalSize> {
        let geometry = self.with_state(|state| state.console.as_ref().map(|c| *c.geometry()))??;
        let rows = u16::try_from(geometry.rows()).ok()?;
        let cols = u16::try_from(geometry.columns()).ok()?;
        TerminalSize::new(rows, cols).ok()
    }

    /// Where the console's scan-out lies, if it has one.
    #[must_use]
    pub fn placement(&self) -> Option<ConsoleSurface> {
        self.with_state(|state| state.scanout.as_ref().map(|scanout| scanout.placement))?
    }

    /// The physical extent `(base, bytes)` of the scan-out, if any.
    #[must_use]
    pub fn extent(&self) -> Option<(u64, u64)> {
        self.with_state(|state| {
            state
                .scanout
                .as_ref()
                .map(|s| (s.placement.phys, s.bytes() as u64))
        })?
    }

    fn with_state<R>(&self, read: impl FnOnce(&State<S::Grid>) -> R) -> Option<R> {
        let _guard = self.lock.lock();
        // SAFETY: render lock held; read-only.
        unsafe { (*self.slot.get()).as_ref() }.map(read)
    }

    /// The lock a paint holds, as [`Self::fatal`] last said.
    fn hold(&self) -> Hold<'_, P::Irq> {
        match self.paint_lock.load(Ordering::Acquire) {
            PAINT_LOCKED => Hold::Locked(self.lock.lock()),
            PAINT_BUSTED => self.lock.try_lock().map_or(Hold::Busted, Hold::Locked),
            _ => self.lock.try_lock().map_or(Hold::Contended, Hold::Locked),
        }
    }

    /// Run `op` on the console over the pixels it may paint, publish what it
    /// dirtied, and record it for a tracking host. `true` when rows became
    /// owed.
    fn paint(
        &self,
        op: impl FnOnce(&mut TextConsole<S::Grid>, &mut [u32]) -> Option<DirtyBand>,
    ) -> bool {
        if !self.is_active() {
            return false;
        }
        let _held = match self.hold() {
            Hold::Contended => return false,
            held => held,
        };
        // SAFETY: `active` was observed (acquire), so the slot is complete;
        // the lock is held, or a fatal path stopped every CPU that could hold
        // it.
        let Some(state) = (unsafe { (*self.slot.get()).as_mut() }) else {
            return false;
        };
        let reclaimed = self.reclaim_owed.swap(false, Ordering::AcqRel)
            && apply_surface::<P, S::Grid>(state, Surface::Shown);
        let Some(console) = state.console.as_mut() else {
            return reclaimed;
        };
        let scanout = state
            .scanout
            .as_ref()
            .filter(|_| console.surface().paints());
        // SAFETY: as above; reachability was proved when the scan-out was
        // installed, and neither the boot identity map nor the direct map is
        // ever withdrawn. A console with no scan-out to paint gets no slice,
        // so no reference into a session's surface is formed.
        let surface: &mut [u32] = scanout.map_or(&mut [], |scanout| unsafe { pixels(scanout) });
        let band = op(console, surface);
        let owed = match (band, scanout) {
            (Some(band), Some(scanout)) => {
                publish_band::<P>(scanout, band);
                owe(state, band)
            }
            _ => false,
        };
        reclaimed || owed
    }
}

/// The scan-out the mode at `base` describes, or `None` for a mode no glyph
/// fits.
fn scanout_at(base: NonNull<u32>, placement: ConsoleSurface) -> Option<Scanout> {
    let mode = placement.mode;
    let geometry = Geometry::for_display(mode.width_px, mode.height_px, mode.stride_bytes)?;
    Some(Scanout {
        base,
        placement,
        geometry,
    })
}

/// A console over fresh grids for `geometry`.
fn console_for<S: GridSource>(geometry: Geometry) -> Option<TextConsole<S::Grid>> {
    let (main, alt) = grids::<S>(geometry)?;
    Some(TextConsole::new(geometry, main, alt))
}

/// Blank `main`/`alt` grids for `geometry`, or `None` when the source refuses.
fn grids<S: GridSource>(geometry: Geometry) -> Option<(S::Grid, S::Grid)> {
    let cells = geometry.cell_count();
    Some((S::blank(cells)?, S::blank(cells)?))
}

/// Apply a disposition to the console and its scan-out. `true` when rows
/// became owed. The render lock is held.
fn apply_surface<P: ConsolePort, G: Grid>(state: &mut State<G>, surface: Surface) -> bool {
    let Some(console) = state.console.as_mut() else {
        return false;
    };
    let scanout = state.scanout.as_ref().filter(|_| surface.paints());
    let pixels: &mut [u32] = match scanout {
        // Re-proved on every disposition change: this is the path a fatal
        // report takes to get the screen back, where a fault would cost the
        // machine the one diagnosis it was about to emit.
        Some(scanout) if P::reachable(scanout.base, scanout.bytes()) => {
            // SAFETY: the render lock is held and the extent translates.
            unsafe { pixels(scanout) }
        }
        Some(_) => return false,
        None => &mut [],
    };
    let band = match surface {
        Surface::Shown => console.show(pixels),
        Surface::Blank => console.blank(pixels),
        Surface::Hidden => {
            console.hide();
            None
        }
    };
    match (band, scanout) {
        (Some(band), Some(scanout)) => {
            publish_band::<P>(scanout, band);
            owe(state, band)
        }
        _ => false,
    }
}

/// Record `band` for a host tracking damage. `true` when nothing was owed
/// before.
fn owe<G>(state: &mut State<G>, band: DirtyBand) -> bool {
    if !state.tracks_damage {
        return false;
    }
    let first = state.damage.is_none();
    state.damage = merge_bands(state.damage, Some(band));
    first
}

/// Hand the pixel rows `band` covers to the scan-out.
fn publish_band<P: ConsolePort>(scanout: &Scanout, (start, end): DirtyBand) {
    let row = scanout.geometry.stride_px as usize;
    let rows = (end.min(scanout.geometry.height_px) - start.min(end)) as usize;
    // SAFETY: `start` is a row of the surface, so the offset stays inside
    // the extent `base` addresses.
    let first = unsafe {
        scanout
            .base
            .add(start.min(scanout.geometry.height_px) as usize * row)
    };
    P::publish(first, rows * scanout.stride_bytes());
}

#[cfg(test)]
#[path = "hosted_tests.rs"]
mod tests;
