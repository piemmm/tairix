//! VDU 25's plot codes, which the BBC Micro's MOS and RISC OS share: the
//! shape in the top five bits, and in the low three whether the point is
//! given absolutely and which colour plots.

/// The bits that name the shape.
pub const SHAPE: u8 = 0xF8;
/// The bit that gives the point absolutely, rather than from the last.
pub const ABSOLUTE: u8 = 4;
/// The bits that choose the colour: none, so a move, or one of the three
/// below.
pub const INK: u8 = 3;
/// The foreground colour.
pub const FOREGROUND: u8 = 1;
/// The colour inverted.
pub const INVERSE: u8 = 2;
/// The background colour.
pub const BACKGROUND: u8 = 3;

/// A solid line with both its ends: the first of the eight line shapes.
pub const LINE: u8 = 0x00;
/// A solid line without its last point.
pub const LINE_WITHOUT_LAST: u8 = 0x08;
/// A solid line without its first point.
pub const LINE_WITHOUT_FIRST: u8 = 0x20;
/// A dotted line without either end: the last of the line shapes.
pub const DOTTED_WITHOUT_ENDS: u8 = 0x38;
/// A point.
pub const POINT: u8 = 0x40;
/// A horizontal line filled left and right while the background lasts.
pub const FILL_TO_NON_BACKGROUND: u8 = 0x48;
/// A triangle.
pub const TRIANGLE: u8 = 0x50;
/// A horizontal line filled right until the background.
pub const FILL_RIGHT_TO_BACKGROUND: u8 = 0x58;
/// A rectangle.
pub const RECTANGLE: u8 = 0x60;
/// A horizontal line filled left and right until the foreground.
pub const FILL_TO_FOREGROUND: u8 = 0x68;
/// A parallelogram.
pub const PARALLELOGRAM: u8 = 0x70;
/// A horizontal line filled right while the foreground lasts.
pub const FILL_RIGHT_TO_NON_FOREGROUND: u8 = 0x78;
/// A flood fill while the background lasts.
pub const FLOOD: u8 = 0x80;
/// A flood fill until the foreground.
pub const FLOOD_TO_FOREGROUND: u8 = 0x88;
/// A circle's outline.
pub const CIRCLE: u8 = 0x90;
/// A filled circle.
pub const CIRCLE_FILL: u8 = 0x98;
/// A circular arc.
pub const ARC: u8 = 0xA0;
/// A segment.
pub const SEGMENT: u8 = 0xA8;
/// A sector.
pub const SECTOR: u8 = 0xB0;
/// A block moved or copied, by its colour bits: none moves the cursor alone.
pub const BLOCK: u8 = 0xB8;
/// An ellipse's outline.
pub const ELLIPSE: u8 = 0xC0;
/// A filled ellipse.
pub const ELLIPSE_FILL: u8 = 0xC8;

/// [`BLOCK`]'s colour bits that move the block, clearing where it was.
pub const BLOCK_MOVE: u8 = 1;
/// [`BLOCK`]'s colour bits that copy the block, as the background's do.
pub const BLOCK_COPY: u8 = 2;
