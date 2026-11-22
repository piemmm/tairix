//! The save area's layout: written state reads back as it was, and a save
//! area a program has written reads back as nothing or as state the drivers
//! could have written.

use tairix_riscos_filing::session::{session, Setup, SLOT_BYTES};
use tairix_riscos_kernel::os::Os;

use super::*;

fn with_session(test: impl FnOnce(&mut Os<'_>, u32)) {
    let mut session = session(&Setup {
        vdu: Some(crate::DRIVERS),
        modules: &crate::MODULES,
        bytes: SLOT_BYTES,
        keys: &[],
        disc: None,
    });
    let mut os = session.kernel.os();
    let area = os.rma_claim(SAVE_AREA_BYTES).expect("a save area");
    test(&mut os, area);
}

fn mode_28() -> Format {
    Format::numbered(tairix_vdu::mode::numbered(28).expect("mode 28"))
}

#[test]
fn the_layout_fits_the_save_area_risc_os_5_reports() {
    let mut words = Words::default();
    words.put(WRITTEN);
    Saved::fresh(&mode_28(), 0).encode(&mut words);
    assert_eq!(words.at, WORDS, "every word of the layout written");
}

#[test]
fn written_state_reads_back_as_it_was() {
    with_session(|os, area| {
        let mut saved = Saved::fresh(&mode_28(), 0);
        saved.text.x = 3;
        saved.text.y = 2;
        saved.text.editing = Some((1, 1));
        saved.graphics.cursor = (-40, 1000);
        saved.colours.fore.pattern = Some([1, 2, 3, 4, 5, 6, 7, 8]);
        saved.colours.dot = 0x0123_4567_89AB_CDEF;
        saved.queue = Queue {
            code: 25,
            gathered: [1, 2, 3, 0, 0, 0, 0, 0, 0],
            waiting: 0u8.wrapping_sub(2),
        };
        saved.spool = 7;
        saved.write(os, area).expect("written");
        assert_eq!(Saved::read(os, area).expect("read"), Some(saved));
    });
}

#[test]
fn an_area_never_written_reads_back_as_none() {
    with_session(|os, area| {
        os.write_word(area, 0).expect("zeroed");
        assert_eq!(Saved::read(os, area).expect("read"), None);
        os.write_word(area, 0x1234_5678)
            .expect("another program's layout");
        assert_eq!(Saved::read(os, area).expect("read"), None);
    });
}

#[test]
fn no_word_a_program_writes_reaches_the_drivers_unchecked() {
    with_session(|os, area| {
        let saved = Saved::fresh(&mode_28(), 0);
        saved.write(os, area).expect("written");
        let hostile = [u32::MAX, 0x8000_0000, 0x7FFF_FFFF, 2, 0x100, 65];
        for i in 1..WORDS {
            let at = area + 4 * u32::try_from(i).expect("an index");
            let kept = os.read_word(at).expect("a word");
            for &word in &hostile {
                os.write_word(at, word).expect("corrupted");
                if let Some(read) = Saved::read(os, area).expect("read") {
                    let (width, height) = read.text.extent();
                    assert!(read.text.x <= width && read.text.y < height, "word {i}");
                    assert!(read.text.window.left <= read.text.window.right, "word {i}");
                    assert!(read.text.window.top <= read.text.window.bottom, "word {i}");
                    let g = read.graphics.window;
                    assert!(0 <= g.left && g.left <= g.right && 0 <= g.bottom && g.bottom <= g.top);
                    assert!(
                        (1..=MOST_DOTS).contains(&read.colours.dot_length),
                        "word {i}"
                    );
                    assert!(read.text.cursor.appearance <= 3, "word {i}");
                }
            }
            os.write_word(at, kept).expect("restored");
        }
    });
}

#[test]
fn a_destination_its_windows_do_not_fit_takes_its_whole_extent() {
    let screen = mode_28();
    let mut saved = Saved::fresh(&screen, 0);
    saved.text.x = 5;
    let small = Format::sprite(&crate::mode::Drawn {
        flags: 0,
        ncolour: 63,
        eig: (1, 1),
        y_shift: 0,
        log2_bpp: 3,
        log2_bpc: 3,
        line_length: 32,
        width: 32,
        height: 16,
    });
    saved.fit(&small);
    assert_eq!(
        (saved.text.window.right, saved.text.window.bottom),
        (3, 1),
        "four columns, two rows"
    );
    assert_eq!(
        (saved.graphics.window.right, saved.graphics.window.top),
        (31, 15)
    );
    assert_eq!((saved.text.x, saved.text.y), (0, 0));
    let fitted = saved.clone();
    saved.fit(&screen);
    assert_eq!(saved, fitted, "windows that fit are kept");
}
