use tairix_riscos_arm::fpa::fpsr::ENABLES;
use tairix_riscos_kernel::FRESH_FPSR;

use super::*;
use crate::c::testing::{Client, FREE, RETURN, STATICS};
use crate::kernel::statics::at as kernel_at;

#[test]
fn multiply_keeps_the_low_word() {
    let mut client = Client::new();
    assert_eq!(client.answer(multiply, &[0x1_0001, 0x1_0001]), 0x0002_0001);
    assert_eq!(client.answer(multiply, &[7, 6]), 42);
}

#[test]
fn checks_pass_the_application_space_to_512_mib_aligned_to_the_access() {
    let mut client = Client::new();
    for (entry, address) in [
        (
            read_byte_check as tairix_riscos_kernel::module::Code,
            0x8000,
        ),
        (read_byte_check, 0x1FFF_FFFF),
        (write_byte_check, 0x1FFF_FFFF),
        (read_half_check, 0x8002),
        (write_half_check, 0x8002),
        (read_word_check, 0x9000),
        (write_word_check, 0x8004),
    ] {
        client.running_with(&[address]);
        let (done, _, pc) = client.subroutine(entry);
        assert!(done.is_ok() && pc == RETURN, "{address:#x}");
    }
}

#[test]
fn a_check_that_fails_traps_with_its_access_s_error_noted() {
    for (entry, address, error) in [
        (
            read_byte_check as tairix_riscos_kernel::module::Code,
            0x7FFF,
            READ_FAIL,
        ),
        (read_byte_check, 0x2000_0000, READ_FAIL),
        (read_byte_check, 0, READ_FAIL),
        (write_byte_check, 0xFC00_0000, WRITE_FAIL),
        (read_half_check, 0x8001, READ_FAIL),
        (read_word_check, 0x8002, READ_FAIL),
        (write_half_check, 0x8001, WRITE_FAIL),
        (write_word_check, 0x9001, WRITE_FAIL),
    ] {
        let mut client = Client::new();
        client.running_with(&[address]);
        let _ = client.subroutine(entry);
        assert_eq!(
            client.word(STATICS + kernel_at::LAST_ERROR),
            error,
            "{address:#x}"
        );
    }
}

#[test]
fn a_count_increments_the_word_after_the_call_and_returns_past_it() {
    let mut client = Client::new();
    let counter = FREE + 0x40;
    client.put(counter, &[41, 0, 0, 0]);
    client.running_with(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xE000, counter]);
    let (done, _, pc) = client.subroutine(count_next);
    assert!(done.is_ok());
    assert_eq!((client.word(counter), pc), (42, counter + 4));
    client.running_with(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xE000, counter]);
    let (_, _, pc) = client.subroutine(count_with_position);
    assert_eq!(
        (client.word(counter), pc),
        (43, counter + 8),
        "past the position word too"
    );
}

#[test]
fn divtest_traps_only_a_divisor_of_nought() {
    let mut client = Client::new();
    client.running_with(&[3]);
    let (done, _, pc) = client.subroutine(divtest);
    assert!(done.is_ok() && pc == RETURN);
    client.running_with(&[0]);
    let _ = client.subroutine(divtest);
    assert_eq!(
        client.word(STATICS + kernel_at::LAST_ERROR),
        trap::DIVIDE_BY_ZERO
    );
}

mod recorded;

use recorded::{LOADED, STORED};

const PACKED: u32 = FREE + 0x100;

fn packed_at(client: &mut Client) -> [u32; 4] {
    let mut words = [0; 4];
    client
        .session
        .kernel
        .os()
        .read_words(PACKED, &mut words)
        .expect("read");
    words
}

#[test]
fn stfp_and_ldfp_convert_as_genuine_does() {
    let mut client = Client::new();
    client.session.kernel.os().set_fpsr(FRESH_FPSR);
    for (bits, stored, loaded) in STORED {
        client
            .session
            .kernel
            .os()
            .write_words(PACKED, &[0xDEAD_BEEF; 4])
            .expect("filled");
        let [high, low] = tairix_softfloat::fpa_words(bits);
        client.call(stfp, &[high, low, PACKED]);
        assert_eq!(
            packed_at(&mut client),
            [stored[0], stored[1], stored[2], 0xDEAD_BEEF],
            "_stfp({bits:#X})"
        );
        client.call(ldfp, &[PACKED]);
        let left = client.session.kernel.os().fpa_extended(0).expect("F0");
        assert_eq!(left, loaded, "_ldfp(_stfp({bits:#X}))");
    }
    for (packed, loaded) in LOADED {
        client
            .session
            .kernel
            .os()
            .write_words(PACKED, &packed)
            .expect("written");
        client.call(ldfp, &[PACKED]);
        let left = client.session.kernel.os().fpa_extended(0).expect("F0");
        assert_eq!(left, loaded, "_ldfp({packed:08X?})");
    }
}

#[test]
fn ldfp_works_on_the_guest_s_fpa_its_flags_and_traps_included() {
    let mut client = Client::new();
    let inexact = tairix_riscos_arm::fpa::Exception::Inexact.flag();
    for (packed, flagged) in [
        ([0x0000_0314, 0x1592_6535, 0x8979_3200], true),
        ([0x0000_1100, 0, 0], false),
    ] {
        client.session.kernel.os().set_fpsr(FRESH_FPSR);
        client
            .session
            .kernel
            .os()
            .write_words(PACKED, &packed)
            .expect("written");
        client.call(ldfp, &[PACKED]);
        let fpsr = client.session.kernel.os().fpsr();
        assert_eq!(fpsr & inexact != 0, flagged, "{packed:08X?}");
        assert_eq!(
            fpsr & ENABLES,
            FRESH_FPSR & ENABLES,
            "the traps as they were"
        );
    }
    let beyond_doubles = [0x0040_0100, 0, 0];
    client
        .session
        .kernel
        .os()
        .write_words(PACKED, &beyond_doubles)
        .expect("written");
    client.session.kernel.os().set_fpsr(0);
    client.call(ldfp, &[PACKED]);
    let bits = client.session.kernel.os().fpa_double(0).expect("F0");
    assert_eq!(bits, f64::INFINITY.to_bits(), "an untrapped overflow");
    client.session.kernel.os().set_fpsr(FRESH_FPSR);
    assert!(
        client.outcome(ldfp, &[PACKED]).is_err(),
        "a trapped overflow raised"
    );
}
