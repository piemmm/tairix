//! The session-worker role: one session, its port the standard streams and
//! its screens' ring the spawn share.

use tairix_reclaim::{PressureBand, PressureGauge};
use tairix_riscos_app::worker::{Memory, Worker};
use tairix_riscos_session::protocol::{Stopped, FRAME_MOST};
use tairix_riscos_session::worker::{serve as serve_session, End};
use tairix_sandbox::rt::{draw_seed, spawn_share, StdioChannel};
use tairix_sandbox::{ClockedLink, WorkerLink};

/// The worker's memory pressure: its own process's.
struct RtMemory;

impl Memory for RtMemory {
    fn gauge(&self) -> &'static (dyn PressureGauge + 'static) {
        tairix_rt::pressure::gauge()
    }

    fn report(&self, band: PressureBand) {
        tairix_rt::pressure::report(band);
    }
}

/// The session served to its end: the exit that end is.
pub fn serve() -> i32 {
    let (Ok(share), Some(seed)) = (spawn_share(), draw_seed()) else {
        return Stopped::Refused.code();
    };
    let Ok(link) = WorkerLink::new(StdioChannel, FRAME_MOST) else {
        return End::NoMemory.code();
    };
    let port = ClockedLink::new(link, tairix_rt::clock_get);
    serve_session(port, share, Worker::new(RtMemory, seed)).code()
}
