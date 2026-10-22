//! A saver's `Run`: its standard streams and its spawn share.

use tairix_abi::Errno;
use tairix_rt::shm::SpawnShare;
use tairix_sandbox::rt::StdioChannel;

use crate::{serve, Ended, RegionBytes, Scene, Setup};

impl RegionBytes for SpawnShare {
    fn bytes_mut(&mut self) -> &mut [u8] {
        SpawnShare::bytes_mut(self)
    }
}

/// Run the scene `make` builds until the host dismisses the saver, answering
/// the process's exit code: zero for a dismissal, one for anything else.
///
/// `make` refuses with the reason a scene cannot be built: the memory it
/// cannot have, or an option the host did not send.
pub fn run<S: Scene>(make: impl FnOnce(&Setup<'_>) -> Result<S, Errno>) -> i32 {
    let ended = serve(StdioChannel, |region| SpawnShare::map(region.len()), make);
    match ended {
        Ended::Dismissed => 0,
        Ended::Broken(_) | Ended::Unstarted(_) => 1,
    }
}
