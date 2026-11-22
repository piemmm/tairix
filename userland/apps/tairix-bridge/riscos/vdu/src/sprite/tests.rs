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
