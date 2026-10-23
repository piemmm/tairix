//! A saver's `Run`: its standard streams and its spawn share.

use alloc::boxed::Box;

use tairix_abi::Errno;
use tairix_raster::PixelMemory;
use tairix_rt::shm::{SpawnPart, SpawnShare};
use tairix_sandbox::rt::StdioChannel;

use crate::{serve, Ended, RegionBytes, Scene, Setup};

/// One part of the spawn share: the frame area a scene draws straight into,
/// or the feed area after it.
pub struct Part(SpawnPart);

impl PixelMemory for Part {
    fn bytes_mut(&mut self) -> &mut [u8] {
        self.0.bytes_mut()
    }
}

impl AsMut<[u8]> for Part {
    fn as_mut(&mut self) -> &mut [u8] {
        self.0.bytes_mut()
    }
}

impl RegionBytes for SpawnShare {
    type Rest = Part;

    fn into_parts(self, frame_len: usize) -> Result<(Box<dyn PixelMemory>, Part), Errno> {
        let (frame, rest) = self.split(frame_len)?;
        Ok((Box::new(Part(frame)), Part(rest)))
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
