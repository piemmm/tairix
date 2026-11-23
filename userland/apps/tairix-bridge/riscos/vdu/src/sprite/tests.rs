//! The sprite calls' errors: each token one the kernel's messages give text
//! for, so no error shows its token in its place.

use tairix_riscos_filing::session::{session, Setup, SLOT_BYTES};
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::os::Fail;

use super::format::{BAD_MODE_WORD, BAD_SPRITE_MODE};
use super::*;

#[test]
fn every_sprite_error_has_a_message_of_its_own() {
    let mut session = session(&Setup {
        vdu: Some(crate::DRIVERS),
        modules: &crate::MODULES,
        bytes: SLOT_BYTES,
        keys: &[],
        disc: None,
    });
    let mut os = session.kernel.os();
    let errors: [Error; 20] = [
        TABLE_REQUIRED,
        NO_SYSTEM_AREA,
        NO_ROOM,
        NO_SPRITES,
        NO_MEMORY,
        NOT_FOUND,
        BAD_FILE,
        NO_ROOM_TO_COPY,
        OUTSIDE,
        NO_ROOM_TO_GROW,
        UNKNOWN_REASON,
        CANNOT_JOIN,
        RESERVED_FLAGS,
        NO_DEEP_PALETTE,
        NO_DEEP_NEW_PALETTE,
        UNRECOGNISED,
        BAD_SPRITE_MODE,
        BAD_MODE_WORD,
        format::UNKNOWN_MODE,
        extend::NO_GREY_SCALED,
    ];
    for error in errors {
        let Fail::Error(block) = os.error(error) else {
            panic!("{} raised", error.token);
        };
        assert_eq!(os.read_word(block).expect("a block"), error.number);
        let text = os.read_zstring(block + 4, 256).expect("its text");
        assert!(
            !text.is_empty() && text != error.token.as_bytes(),
            "{}",
            error.token
        );
    }
}

#[test]
fn a_sprite_holding_256_palette_entries_or_more_has_a_full_palette() {
    let held = |entries: u32| {
        let image =
            tairix_image::SPRITE_HEADER_LEN + entries * tairix_image::SPRITE_PALETTE_ENTRY_LEN;
        header::Sprite {
            at: 0,
            next: image,
            name: tairix_image::SpriteName::spelt_by(b"s"),
            words_less_one: 0,
            rows_less_one: 0,
            first_bit: 0,
            last_bit: 31,
            image,
            mask: image,
            mode: 28,
        }
        .full_palette()
    };
    assert_eq!(
        [0, 64, 255, 256, 300].map(held),
        [false, false, false, true, true]
    );
}

#[test]
fn a_numbered_mode_s_sprite_is_drawn_at_its_mode_s_eigen_factors() {
    assert_eq!(format::drawn_eig(12), (1, 2));
    assert_eq!(format::drawn_eig(13), (2, 2));
    assert_eq!(format::drawn_eig(28), (1, 1));
    let hundred_dpi = 1 | 100 << 1 | 100 << 14 | 6 << 27;
    assert_eq!(
        format::drawn_eig(hundred_dpi),
        (1, 1),
        "the nearest factor not finer"
    );
    assert_eq!(
        format::variable(hundred_dpi, tairix_vdu::mode::var::X_EIG),
        None
    );
}
