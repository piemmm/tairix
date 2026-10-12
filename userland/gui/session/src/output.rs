//! The session following its display's output (`plans/DISPLAY.md` D12).
//!
//! The output is the display service's: it moves when a sink arrives or
//! leaves, or the output runs another mode, and the service announces each
//! move on [`NoticeTopic::DisplayOutput`](tairix_abi::NoticeTopic). The
//! session re-reads the output on that edge and on a present the service
//! refused for frames that no longer describe it, and converges once.

use tairix_abi::driver::display::DisplayMode;
use tairix_abi::DriverError;

/// What the output asks of the session's frames.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Follow {
    /// There is no output: give the frames back and compose nothing.
    Release,
    /// The output runs the mode the frames are shaped for: show the whole
    /// screen again, for the sink that may be new.
    Repaint,
    /// The output runs `mode`: establish frames for it and re-lay everything
    /// mode-sized.
    Reconfigure(DisplayMode),
}

impl Follow {
    /// What `output` asks of a session holding frames shaped for `held`, if
    /// any. `forgotten` says the service refused those frames as unknown — a
    /// restarted service holds no configuration — so they are established
    /// again even for the mode they are shaped for.
    #[must_use]
    pub fn of(output: Option<DisplayMode>, held: Option<DisplayMode>, forgotten: bool) -> Self {
        match output {
            None => Self::Release,
            Some(mode) if held == Some(mode) && !forgotten => Self::Repaint,
            Some(mode) => Self::Reconfigure(mode),
        }
    }
}

/// What a refused present means for the session.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The seat's lease is gone: the session is over.
    SeatLost,
    /// The frames do not describe the output any more: re-read it.
    /// `forgotten` when the service holds no configuration for them at all.
    Follow {
        /// The service knows nothing of the frames.
        forgotten: bool,
    },
    /// The display failed.
    Failed,
}

impl Refusal {
    /// What the service's refusal `err` of a present means.
    #[must_use]
    pub const fn of(err: DriverError) -> Self {
        match err {
            DriverError::SeatRevoked | DriverError::PermissionDenied => Self::SeatLost,
            DriverError::Stale => Self::Follow { forgotten: false },
            DriverError::NotFound => Self::Follow { forgotten: true },
            _ => Self::Failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use tairix_abi::driver::display::{DisplayFormat, DisplayMode};
    use tairix_abi::DriverError;

    use super::{Follow, Refusal};

    const SMALL: DisplayMode = DisplayMode {
        width_px: 1024,
        height_px: 768,
        stride_bytes: 4096,
        format: DisplayFormat::Bgra8888,
    };

    const LARGE: DisplayMode = DisplayMode {
        width_px: 1920,
        height_px: 1080,
        stride_bytes: 7680,
        format: DisplayFormat::Bgra8888,
    };

    #[test]
    fn no_output_gives_the_frames_back_whatever_they_are() {
        for held in [None, Some(SMALL)] {
            for forgotten in [false, true] {
                assert_eq!(Follow::of(None, held, forgotten), Follow::Release);
            }
        }
    }

    #[test]
    fn the_mode_the_frames_hold_is_repainted_rather_than_rebuilt() {
        assert_eq!(Follow::of(Some(SMALL), Some(SMALL), false), Follow::Repaint);
    }

    #[test]
    fn another_mode_or_no_frames_establishes_frames_for_the_output() {
        assert_eq!(
            Follow::of(Some(LARGE), Some(SMALL), false),
            Follow::Reconfigure(LARGE)
        );
        assert_eq!(
            Follow::of(Some(SMALL), None, false),
            Follow::Reconfigure(SMALL)
        );
    }

    #[test]
    fn frames_a_restarted_service_forgot_are_established_again() {
        assert_eq!(
            Follow::of(Some(SMALL), Some(SMALL), true),
            Follow::Reconfigure(SMALL)
        );
    }

    #[test]
    fn a_refused_present_is_read_for_what_it_says() {
        assert_eq!(Refusal::of(DriverError::SeatRevoked), Refusal::SeatLost);
        assert_eq!(
            Refusal::of(DriverError::PermissionDenied),
            Refusal::SeatLost
        );
        assert_eq!(
            Refusal::of(DriverError::Stale),
            Refusal::Follow { forgotten: false }
        );
        assert_eq!(
            Refusal::of(DriverError::NotFound),
            Refusal::Follow { forgotten: true }
        );
        assert_eq!(Refusal::of(DriverError::DeviceFault), Refusal::Failed);
    }
}
