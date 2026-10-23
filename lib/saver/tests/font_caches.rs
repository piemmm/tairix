//! The font caches a saver installs: the machine's share, as every desktop
//! program's are, and never past a share of the saver's own ceiling. A test
//! binary of its own, because the caches are the process's.

use tairix_abi::saver_ipc::HostMessage;
use tairix_abi::Errno;
use tairix_geometry::Region;
use tairix_raster::Surface;
use tairix_saver::testing::{self, HeapRegion};
use tairix_saver::{serve, Ended, Frame, Host, NextFrame, Scene};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// A scene that draws nothing.
struct Still;

impl Scene for Still {
    fn frame(
        &mut self,
        _surface: &mut Surface,
        _damage: &mut Region,
        _frame: &Frame<'_>,
        _host: &mut Host<'_>,
    ) -> NextFrame {
        NextFrame::Idle
    }
}

/// Start a saver on a machine of `installed` bytes, held to `memory`.
fn started(installed: u64, memory: u64) {
    let mut start = testing::start(4, 4);
    start.installed_memory = installed;
    start.memory = memory;
    let (channel, _) = testing::script(&[HostMessage::Start(start)]);
    let ended = serve(
        channel,
        |layout| Ok(HeapRegion::new(vec![0; layout.len()])),
        |_| Ok::<_, Errno>(Still),
    );
    assert_eq!(ended, Ended::Dismissed);
}

#[test]
fn a_savers_glyph_cache_is_the_machines_share_within_its_own_ceiling() {
    // Eight gibibytes give the desktop's share of two mebibytes, where the
    // saver's own sixty-four would give sixteen kibibytes.
    started(8 * GIB, 64 * MIB);
    let mib = usize::try_from(MIB).expect("small");
    assert!(
        tairix_font::glyph_cache_holds(1, mib),
        "sized from the machine"
    );
    // A terabyte's share is held to a sixteenth of the saver's ceiling.
    started(1024 * GIB, 64 * MIB);
    assert!(tairix_font::glyph_cache_holds(1, 3 * mib));
    assert!(
        !tairix_font::glyph_cache_holds(1, 5 * mib),
        "held within the saver"
    );
}
