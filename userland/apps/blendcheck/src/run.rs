//! The `Run` entry-point binary of the `blendcheck` tool.
//!
//! It owns the seat for as long as the pattern is up: it takes the display
//! lease, the frames the display driver allocated and one layer buffer, shows
//! the pattern [`tairix_blendcheck`] builds, parks until a key or a pointer
//! button is pressed, and gives the seat back to the text console on every
//! exit after the lease. Every refusal states its reason on `stderr`.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
extern crate alloc;

#[cfg(all(freestanding, feature = "program"))]
mod program {
    use tairix_abi::driver::display::DamageRect;
    use tairix_abi::input::{KeyInput, PointerInput, TimedPointer};
    use tairix_abi::seat::{ReleaseSurface, SEAT_PRIMARY};
    use tairix_abi::touch::TouchFrame;
    use tairix_abi::{Errno, WaitSetOp, WaitSourceKind, WAITSET_TIMEOUT_NONE};
    use tairix_blendcheck::{parse, pattern, Command, USAGE};
    use tairix_display::{establish_display, Base, DisplayClient, Presenter, RtDisplayTransport};
    use tairix_rt::io::{write_stderr_line, Stdout, Write};

    const NAME: &str = "blendcheck";

    /// The token the seat's input wakes the wait with.
    const SEAT_TOKEN: u64 = 1;

    /// What the screen shows, stated before it is taken over.
    const LEGEND: &str = "blendcheck: three panes of dark glass over a grey ramp, top to bottom:\n\
         \x20 1. blended by the desktop, dithered, as the desktop draws translucency\n\
         \x20 2. blended by the desktop without the dither: what banding looks like\n\
         \x20 3. blended by the display engine\n\
         Press a key or a pointer button to return.\n";

    /// State `reason` on `stderr` and answer the failure status.
    fn fail(reason: &str) -> i32 {
        write_stderr_line(alloc::format!("{NAME}: {reason}"));
        1
    }

    /// Program entry point: `0` once the pattern was shown and dismissed,
    /// `1` when it could not be shown or the command line was not understood.
    fn main() -> i32 {
        let Some(arguments) = tairix_help::arguments(NAME) else {
            return tairix_help::MISUSE;
        };
        match parse(&arguments) {
            Ok(Command::Run) => {}
            Ok(Command::Help) => return tairix_help::print_own_short_help(NAME, Some(USAGE)),
            Err(err) => {
                tairix_help::report_misuse(NAME, &err);
                return tairix_help::MISUSE;
            }
        }
        if Stdout.write_all(LEGEND.as_bytes()).is_err() {
            return fail("standard output refused the legend");
        }
        let taken = tairix_rt::display_acquire(SEAT_PRIMARY);
        if taken < 1 {
            return match Errno::from_syscall(taken) {
                Errno::SeatBusy => {
                    fail("a session holds the display; run blendcheck from a text session")
                }
                refused => fail(&alloc::format!("the display could not be taken: {refused}")),
            };
        }
        let code = check();
        // Owner-checked: a lease already lost refuses, and that is ignored.
        let _ = tairix_rt::display_release(SEAT_PRIMARY, ReleaseSurface::Text);
        code
    }

    /// Show the pattern on the owned seat until it is dismissed.
    fn check() -> i32 {
        let Ok(set) = u64::try_from(tairix_rt::waitset_create()) else {
            return fail("no wait set could be made to wait for a key on");
        };
        if tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::SeatInput,
            SEAT_PRIMARY,
            SEAT_TOKEN,
        ) < 0
        {
            return fail("the seat's input could not be waited on");
        }
        let output = match DisplayClient::new(RtDisplayTransport, SEAT_PRIMARY).query() {
            Ok(Some(output)) => output,
            Ok(None) => return fail("no output is connected"),
            Err(_) => return fail("the display service refused the output query"),
        };
        let mut display = match establish_display(
            DisplayClient::new(RtDisplayTransport, SEAT_PRIMARY),
            output.mode,
        ) {
            Ok(display) => display,
            Err(refused) => return fail(refused.reason()),
        };
        let Some(engine) = display.layers() else {
            return fail("the display composites no hardware layers, so every blend on it is the desktop's own");
        };
        let caps = engine.caps();
        let stated = if caps.band_free_blend { "yes" } else { "no" };
        if Stdout
            .write_all(
                alloc::format!("{NAME}: the engine states band-free blending: {stated}\n")
                    .as_bytes(),
            )
            .is_err()
        {
            return fail("standard output refused what the engine states");
        }
        let Some(pattern) = pattern(&output.mode) else {
            return fail("the output's mode has no room or no memory for the pattern");
        };
        if !pattern.fits(&caps) {
            return fail("the engine's layers are smaller than the pattern's pane");
        }
        let pane = pattern.engine_pane();
        let Ok(buffer) = engine.alloc(pane.width_px, pane.height_px) else {
            return fail("the display gave no buffer for the engine's pane");
        };
        let laid = engine
            .pixels(buffer)
            .is_some_and(|(bytes, stride)| pattern.lay_glass(bytes, stride));
        let shown = laid
            && engine
                .present_layers(
                    Some(Base {
                        frame: &pattern.frame,
                        damage: &[DamageRect::full(&output.mode)],
                    }),
                    &[pattern.engine_layer(buffer)],
                )
                .is_ok();
        let code = if !laid {
            fail("the engine's buffer could not hold the glass")
        } else if !shown {
            fail("the display refused the pattern")
        } else if wait_for_dismissal(set) {
            0
        } else {
            fail("the seat was lost before a key was pressed")
        };
        let _ = engine.free(buffer);
        code
    }

    /// Park until a key or a pointer button goes down, draining every channel
    /// the seat's input wakes for so the wait never stays ready; `false` once
    /// the seat is lost.
    fn wait_for_dismissal(set: u64) -> bool {
        loop {
            let mut token = 0u64;
            if tairix_rt::waitset_wait(set, WAITSET_TIMEOUT_NONE, &mut token) < 0 {
                return false;
            }
            let keyboard = drain::<{ KeyInput::WIRE_LEN }>(tairix_rt::keyboard_read, |wire| {
                matches!(KeyInput::from_bytes(wire), Ok(KeyInput::Pressed { .. }))
            });
            let pointer = drain::<{ TimedPointer::WIRE_LEN }>(tairix_rt::pointer_read, |wire| {
                matches!(
                    TimedPointer::from_bytes(wire),
                    Ok(TimedPointer {
                        input: PointerInput::Pressed(_),
                        ..
                    })
                )
            });
            let touch = drain::<{ TouchFrame::WIRE_LEN }>(tairix_rt::touch_read, |_| false);
            match [keyboard, pointer, touch] {
                [Drained::Lost, ..] | [_, Drained::Lost, _] | [.., Drained::Lost] => return false,
                [Drained::Dismissed, ..] | [_, Drained::Dismissed, _] => return true,
                _ => {}
            }
        }
    }

    /// What draining one seat channel found.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Drained {
        /// Nothing that ends the check.
        Empty,
        /// A record that ends it.
        Dismissed,
        /// The channel is no longer the seat's to read.
        Lost,
    }

    /// Read records of `LEN` bytes from one seat channel until it is empty.
    fn drain<const LEN: usize>(
        read: fn(u64, &mut [u8]) -> i64,
        dismisses: impl Fn(&[u8]) -> bool,
    ) -> Drained {
        let mut wire = [0u8; LEN];
        let mut found = Drained::Empty;
        loop {
            let got = read(SEAT_PRIMARY, &mut wire);
            if got == 0 {
                return found;
            }
            if usize::try_from(got).ok() != Some(LEN) {
                return Drained::Lost;
            }
            if dismisses(&wire) {
                found = Drained::Dismissed;
            }
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
