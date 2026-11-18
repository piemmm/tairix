//! A kernel booted for a harness: on a layout drawn at random, with an
//! application slot current.

use tairix_fuzzseed::Prng;
use tairix_riscos_kernel::memory::layout::{Layout, RISC_OS_5, SHARED};
use tairix_riscos_kernel::memory::policy::Ceiling;
use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::os::{Config, Host, Kernel};

/// The application space a harness's kernel has.
pub const APPLICATION_SIZE: u32 = 0x1_0000;

/// A kernel with `host`, on the 26-bit layout one time in three and RISC OS
/// 5's otherwise, its application slot of [`APPLICATION_SIZE`] current.
pub fn boot_with(rng: &mut Prng, host: Box<dyn Host>) -> Kernel {
    let (layout, bits): (&'static Layout, Bits) = if rng.below(3) == 0 {
        (&SHARED, Bits::Bit26)
    } else {
        (&RISC_OS_5, Bits::Bit32)
    };
    let config = Config {
        layout,
        ceiling: Ceiling::derive(64 * 1024 * 1024),
        memo_pages: 8,
        host,
        vdu: None,
        modules: &[],
        zone: tairix_tz::Zone::UTC,
        pressure: &tairix_reclaim::pressure::Unpressured,
        log: &tairix_log::DiscardSink,
    };
    let mut kernel = Kernel::new(config).expect("a kernel boots");
    let memory = kernel.memory();
    let slot = memory.create_slot(bits).expect("a slot");
    memory
        .set_slot_size(slot, APPLICATION_SIZE)
        .expect("its memory");
    memory.switch_slot(slot).expect("made current");
    kernel
}
