//! DrawFile's workspace in the RMA: its messages, the matrices and the cap
//! and join block it hands the Draw module, the table ColourTrans selects
//! into, the work sprite text is drawn through, the first error a render
//! met, kept where a later one cannot overwrite it, and what a text area is
//! painted from.

use tairix_riscos_kernel::msgtrans::OwnFile;
use tairix_riscos_kernel::os::{Fail, Os};

use super::render::write_words;
use crate::geometry::Matrix;

/// Its messages' file.
pub(super) const MESSAGES_FILE: OwnFile =
    OwnFile::named(b"Resources:$.Resources.DrawFile.Messages\0");

/// The identity, for a render given no matrix.
pub(super) const IDENTITY: u32 = MESSAGES_FILE.room();
/// The cap and join block a path is stroked with.
pub(super) const CAP_JOIN: u32 = IDENTITY + 24;
/// The matrix an object is plotted through.
pub(super) const MATRIX: u32 = CAP_JOIN + 16;
/// The translation table a sprite is plotted through.
pub(super) const TABLE: u32 = MATRIX + 24;
const TABLE_BYTES: u32 = 256;
/// The work sprite's area.
pub(super) const AREA: u32 = TABLE + TABLE_BYTES;
const AREA_HEADER: u32 = 16;
/// The work sprite: eight pixels square at one bit, a palette and a mask.
const SPRITE_BYTES: u32 = 0x7C;
const AREA_BYTES: u32 = AREA_HEADER + SPRITE_BYTES;
/// The first error a render met.
pub(super) const ERROR: u32 = AREA + AREA_BYTES;
const ERROR_BYTES: u32 = 256;
/// A text area's font name, terminated, for the printer driver.
pub(super) const NAME: u32 = ERROR + ERROR_BYTES;
/// The longest such name, its terminator included.
pub(super) const NAME_BYTES: u32 = 256;
/// A text area's paragraph, as the Font Manager is given it.
pub(super) const TEXT: u32 = NAME + NAME_BYTES;
/// The coordinate block a text area's line is painted with.
pub(super) const COORDINATES: u32 = TEXT + super::text_area::BUFFER_BYTES.next_multiple_of(4);
/// What system-font text writes before each character, for `OS_WriteN`.
pub(super) const MASK_GCOL: u32 = COORDINATES + super::text_area::BLOCK_BYTES;
/// The whole workspace.
pub(super) const BYTES: u32 = MASK_GCOL + 4;

/// The workspace laid out at `at`, the identity written.
pub(super) fn lay_out(os: &mut Os<'_>, at: u32) -> Result<(), Fail> {
    write_words(os, at + IDENTITY, &Matrix::IDENTITY.words())
}

/// The work sprite written in the area at `area`, its one colour `colour`
/// and its mask solid; the sprite's address.
pub(super) fn write_work_sprite(os: &mut Os<'_>, area: u32, colour: u32) -> Result<u32, Fail> {
    let sprite = area + AREA_HEADER;
    write_words(os, area, &[AREA_BYTES, 1, AREA_HEADER, AREA_BYTES])?;
    let header = [
        SPRITE_BYTES,
        u32::from(b't'),
        0,
        0,
        0,
        7,
        0,
        7,
        0x3C,
        0x5C,
        0,
        colour,
        colour,
        0,
        0,
    ];
    write_words(os, sprite, &header)?;
    write_words(os, sprite + 0x3C, &[0; 8])?;
    write_words(os, sprite + 0x5C, &[u32::MAX; 8])?;
    Ok(sprite)
}

/// The error block at `block` copied into the workspace at `workspace`; the
/// copy's address.
pub(super) fn keep_error(os: &mut Os<'_>, workspace: u32, block: u32) -> Result<u32, Fail> {
    let mut bytes = [0; ERROR_BYTES as usize];
    os.read_bytes(block, &mut bytes)?;
    if let Some(last) = bytes.last_mut() {
        *last = 0;
    }
    os.write_bytes(workspace + ERROR, &bytes)?;
    Ok(workspace + ERROR)
}
