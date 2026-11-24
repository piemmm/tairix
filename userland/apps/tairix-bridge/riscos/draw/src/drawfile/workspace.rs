//! DrawFile's workspace in the RMA: its messages, the matrices and the cap
//! and join block it hands the Draw module, the table ColourTrans selects
//! into, the work sprite text is drawn through, the first error a render
//! met, kept where a later one cannot overwrite it, and what a text area is
//! painted from.

use tairix_drawing::riscos::format;
use tairix_image::{sprite_area, SPRITE_HEADER_LEN, SPRITE_PALETTE_ENTRY_LEN};
use tairix_riscos_kernel::msgtrans::OwnFile;
use tairix_riscos_kernel::os::{Fail, Os};

use crate::geometry::Matrix;

/// Its messages' file.
pub(super) const MESSAGES_FILE: OwnFile =
    OwnFile::named(b"Resources:$.Resources.DrawFile.Messages\0");

/// A matrix's bytes.
#[expect(clippy::cast_possible_truncation, reason = "six words")]
const MATRIX_BYTES: u32 = format::MATRIX as u32;
/// The identity, for a render given no matrix.
pub(super) const IDENTITY: u32 = MESSAGES_FILE.room();
/// The cap and join block a path is stroked with.
pub(super) const CAP_JOIN: u32 = IDENTITY + MATRIX_BYTES;
/// The matrix an object is plotted through.
pub(super) const MATRIX: u32 = CAP_JOIN + 16;
/// The translation table a sprite is plotted through.
pub(super) const TABLE: u32 = MATRIX + MATRIX_BYTES;
const TABLE_BYTES: u32 = 256;
/// The work sprite's area.
pub(super) const AREA: u32 = TABLE + TABLE_BYTES;
/// The work sprite: a character of the system font, eight pixels square at
/// one bit in mode 0, a word a row, its image after a palette of two colours,
/// then its mask.
pub(super) const SIDE_SHIFT: u32 = 3;
pub(super) const SIDE: u32 = 1 << SIDE_SHIFT;
pub(super) const MODE: u32 = 0;
const SPRITE_IMAGE: u32 = SPRITE_HEADER_LEN + 2 * SPRITE_PALETTE_ENTRY_LEN;
const SPRITE_MASK: u32 = SPRITE_IMAGE + 4 * SIDE;
const SPRITE_BYTES: u32 = SPRITE_MASK + 4 * SIDE;
const AREA_BYTES: u32 = sprite_area::LEN + SPRITE_BYTES;
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
    os.write_words(at + IDENTITY, &Matrix::IDENTITY.words())
}

/// The work sprite written in the area at `area`, its one colour `colour`
/// and its mask solid; the sprite's address.
pub(super) fn write_work_sprite(os: &mut Os<'_>, area: u32, colour: u32) -> Result<u32, Fail> {
    let sprite = area + sprite_area::LEN;
    os.write_words(area, &[AREA_BYTES, 1, sprite_area::LEN, AREA_BYTES])?;
    let header = [
        SPRITE_BYTES,
        u32::from(b't'),
        0,
        0,
        0,
        SIDE - 1,
        0,
        SIDE - 1,
        SPRITE_IMAGE,
        SPRITE_MASK,
        MODE,
        colour,
        colour,
        0,
        0,
    ];
    os.write_words(sprite, &header)?;
    os.write_words(sprite + SPRITE_IMAGE, &[0; SIDE as usize])?;
    os.write_words(sprite + SPRITE_MASK, &[u32::MAX; SIDE as usize])?;
    Ok(sprite)
}

/// `name` written at `at` and terminated, cut to the `room` bytes there.
pub(super) fn write_name(os: &mut Os<'_>, at: u32, room: usize, name: &[u8]) -> Result<(), Fail> {
    let kept = &name[..name.len().min(room.saturating_sub(1))];
    os.write_terminated(at, kept, 0, room)
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
