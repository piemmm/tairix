//! Frames: the screen's vertical syncs on the monotonic clock at the mode's
//! frame rate, counted on the clock's interrupt. Each raises the vertical
//! sync event, counts the flashing colours down to their change and ends an
//! `OS_Byte` 19 wait. Nothing is timed while nothing waits for a frame: a
//! deadline is given only for a flash due or the event enabled.

use tairix_riscos_kernel::byte::var;
use tairix_riscos_kernel::os::{Fail, Os, Regs, Unwind};

use crate::state::{read, with};

/// The vertical sync event.
const VSYNC_EVENT: u32 = 4;
/// The most frames one interrupt works through: a guest that masked
/// interrupts for a long while sees the count jump, not a burst of events.
const CATCH_UP: u64 = 8;

/// The frame count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Frames {
    /// The monotonic time the count began at.
    began: u64,
    /// The frames worked through.
    counted: u64,
}

impl Frames {
    pub(crate) const fn new(now: u64) -> Self {
        Self {
            began: now,
            counted: 0,
        }
    }

    /// The frames whole at monotonic time `now`, at `rate` a second.
    const fn whole(&self, now: u64, rate: u32) -> u64 {
        now.saturating_sub(self.began) * rate as u64 / 100
    }

    /// When frame `n` begins, in monotonic centiseconds.
    const fn time_of(&self, n: u64, rate: u32) -> u64 {
        let rate = if rate == 0 { 1 } else { rate as u64 };
        self.began + (n * 100).div_ceil(rate)
    }
}

/// The frames since the last interrupt worked through: the flash counted
/// down and the event raised for each.
pub(crate) fn frames(os: &mut Os<'_>) -> Result<(), Unwind> {
    let now = os.now();
    let Some((elapsed, flashing)) = read(os, |vdu, _| {
        let rate = vdu.screen.frame_rate;
        let whole = vdu.frames.whole(now, rate);
        let elapsed = whole.saturating_sub(vdu.frames.counted);
        vdu.frames.counted = whole;
        let format = vdu.screen.format;
        (elapsed, vdu.palette.flashing(&format))
    }) else {
        return Ok(());
    };
    for _ in 0..elapsed.min(CATCH_UP) {
        if flashing {
            flash_frame(os);
        }
        os.raise_event(&mut Regs::new([
            VSYNC_EVENT,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]))?;
    }
    Ok(())
}

/// One frame of the flash: the counter down, the flashing colours' state
/// changed where it reaches nought, the counter then the new state's
/// duration. A state lasting for ever holds.
fn flash_frame(os: &mut Os<'_>) {
    let second = read(os, |vdu, _| vdu.palette.second).unwrap_or(false);
    let lasting = os.byte_var(if second {
        var::SECOND_DURATION
    } else {
        var::FIRST_DURATION
    });
    if lasting == 0 {
        return;
    }
    let left = os.byte_var(var::FLASH_COUNTER).saturating_sub(1);
    if left > 0 {
        os.set_byte_var(var::FLASH_COUNTER, left);
        return;
    }
    let next = if second {
        var::FIRST_DURATION
    } else {
        var::SECOND_DURATION
    };
    let duration = os.byte_var(next);
    os.set_byte_var(var::FLASH_COUNTER, duration);
    let _ = read(os, |vdu, _| vdu.palette.second = !second);
}

/// When the drivers' next timed work falls due: the frame the flash changes
/// at, or the next frame while the vertical sync event is enabled.
pub(crate) fn deadline(os: &mut Os<'_>) -> Option<u64> {
    let event = os.event_enabled(VSYNC_EVENT);
    let counter = u64::from(os.byte_var(var::FLASH_COUNTER).max(1));
    read(os, |vdu, os| {
        let rate = vdu.screen.frame_rate;
        let next = vdu.frames.counted + 1;
        if event {
            return Some(vdu.frames.time_of(next, rate));
        }
        let format = vdu.screen.format;
        let lasting = os.byte_var(if vdu.palette.second {
            var::SECOND_DURATION
        } else {
            var::FIRST_DURATION
        });
        (vdu.palette.flashing(&format) && lasting != 0)
            .then(|| vdu.frames.time_of(vdu.frames.counted + counter, rate))
    })
    .flatten()
}

/// `OS_Byte` 19: waits for the next frame.
pub(crate) fn wait_for_frame(os: &mut Os<'_>) -> Result<(), Fail> {
    let now = os.now();
    let at = with(os, |vdu, _| {
        let rate = vdu.screen.frame_rate;
        Ok(vdu.frames.time_of(vdu.frames.whole(now, rate) + 1, rate))
    })?;
    os.wait_until(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_fall_at_the_rate_from_where_the_count_began() {
        let frames = Frames::new(100);
        assert_eq!(frames.whole(100, 60), 0);
        assert_eq!(
            frames.whole(102, 60),
            1,
            "a sixtieth is under two centiseconds"
        );
        assert_eq!(frames.whole(200, 60), 60);
        assert_eq!(frames.time_of(1, 60), 102);
        assert_eq!(frames.time_of(3, 60), 105);
        assert_eq!(frames.time_of(50, 50), 200);
        assert_eq!(frames.whole(50, 60), 0, "before the count began");
    }
}
