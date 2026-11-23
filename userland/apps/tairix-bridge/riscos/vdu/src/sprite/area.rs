//! Sprite areas in guest memory: the one an operation's area value names,
//! its control block, and the walk through its sprites. A walk takes at most
//! as many sprites as the block counts, each on a word inside the area, so a
//! chain RISC OS 5 would wander off is refused rather than followed.

use tairix_image::{sprite_area as block, SpriteName, SPRITE_HEADER_LEN};
use tairix_riscos_kernel::os::{Fail, Os};
use tairix_riscos_kernel::swi::sprite_op::{BY_NAME, BY_POINTER, REASON};

use super::header::Sprite;
use super::{NOT_FOUND, NO_SYSTEM_AREA, UNRECOGNISED};

/// How R0's bits 8 and 9 name an operation's area and sprite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Naming {
    /// The system area, a sprite by name.
    System,
    /// R1's area, a sprite by name.
    Named,
    /// R1's area, a sprite by its address.
    Pointer,
}

impl Naming {
    /// R0's naming; none past the three forms.
    pub(crate) const fn of(r0: u32) -> Option<Self> {
        match r0 & !REASON {
            0 => Some(Self::System),
            BY_NAME => Some(Self::Named),
            BY_POINTER => Some(Self::Pointer),
            _ => None,
        }
    }

    /// The area value R0 holds for it.
    pub(crate) const fn value(self) -> u32 {
        match self {
            Self::System => 0,
            Self::Named => BY_NAME,
            Self::Pointer => BY_POINTER,
        }
    }
}

/// A sprite area: its first byte, and whether it is the system area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Area {
    pub(crate) base: u32,
    pub(crate) system: bool,
}

/// An area's control block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    pub(crate) size: u32,
    pub(crate) count: u32,
    pub(crate) first: u32,
    pub(crate) free: u32,
}

impl Area {
    /// The area R0's naming and R1 name.
    ///
    /// # Errors
    ///
    /// "No system sprite area" while dynamic area 3 holds nothing.
    pub(crate) fn named(os: &mut Os<'_>, naming: Naming, r1: u32) -> Result<Self, Fail> {
        match naming {
            Naming::System => Self::system(os),
            Naming::Named | Naming::Pointer => Ok(Self {
                base: r1,
                system: false,
            }),
        }
    }

    /// The system area, where dynamic area 3 holds any.
    pub(crate) fn system(os: &mut Os<'_>) -> Result<Self, Fail> {
        let held = crate::state::read(os, |vdu, _| vdu.sprites.system).flatten();
        match held {
            Some(base) => Ok(Self { base, system: true }),
            None => Err(os.error(NO_SYSTEM_AREA)),
        }
    }

    /// The address of byte `offset` of it.
    pub(crate) const fn address(self, offset: u32) -> u32 {
        self.base.wrapping_add(offset)
    }

    /// Its control block.
    pub(crate) fn block(self, os: &mut Os<'_>) -> Result<Block, Fail> {
        let mut words = [0u8; block::LEN as usize];
        os.read_bytes(self.base, &mut words)?;
        let word = |offset| super::header::word_at(&words, offset);
        Ok(Block {
            size: word(block::SIZE),
            count: word(block::COUNT),
            first: word(block::FIRST),
            free: word(block::FREE),
        })
    }

    /// Writes its control block's word at `offset`.
    pub(crate) fn set(self, os: &mut Os<'_>, offset: u32, value: u32) -> Result<(), Fail> {
        os.write_word(self.address(offset), value)
    }

    /// A walk through its sprites, as many as its block counts.
    pub(crate) fn walk(self, os: &mut Os<'_>) -> Result<Walk, Fail> {
        let block = self.block(os)?;
        Ok(Walk {
            area: self,
            offset: block.first,
            left: block.count,
            size: block.size,
        })
    }

    /// The first sprite of its walk named `name`.
    pub(crate) fn find(self, os: &mut Os<'_>, name: &SpriteName) -> Result<Option<Sprite>, Fail> {
        let mut walk = self.walk(os)?;
        while let Some(sprite) = walk.next(os)? {
            if sprite.name.matches(name) {
                return Ok(Some(sprite));
            }
            // A sprite claiming no length is met again at every step after.
            if sprite.next == 0 {
                break;
            }
        }
        Ok(None)
    }

    /// Its sprite numbered `number` from one.
    pub(crate) fn nth(self, os: &mut Os<'_>, number: u32) -> Result<Option<Sprite>, Fail> {
        if number == 0 {
            return Ok(None);
        }
        let mut walk = self.walk(os)?;
        let mut at = 0;
        while let Some(sprite) = walk.next(os)? {
            at += 1;
            if at == number {
                return Ok(Some(sprite));
            }
            // A sprite claiming no length is every sprite after it.
            if sprite.next == 0 {
                return Ok((number - at <= walk.counted()).then_some(sprite));
            }
        }
        Ok(None)
    }

    /// The sprite R0's naming and R2 name in it: a name looked up, or a
    /// sprite's address taken as given.
    ///
    /// # Errors
    ///
    /// "Sprite not found", the area's own refusal, or a fault reading the
    /// name.
    pub(crate) fn sprite(self, os: &mut Os<'_>, naming: Naming, r2: u32) -> Result<Sprite, Fail> {
        if naming == Naming::Pointer {
            if r2 == 0 {
                return Err(os.error(NOT_FOUND));
            }
            return Sprite::read(os, r2);
        }
        let name = read_name(os, r2)?;
        self.find(os, &name)?.ok_or_else(|| os.error(NOT_FOUND))
    }

    /// Checks `sprite` lies whole within the area's sprites, as a change that
    /// moves the sprites after it relies on: a sprite given by an address
    /// outside them would have RISC OS 5 move whatever memory followed it.
    ///
    /// # Errors
    ///
    /// "Unrecognised sprite data".
    pub(crate) fn holds(self, os: &mut Os<'_>, sprite: &Sprite) -> Result<Block, Fail> {
        let block = self.block(os)?;
        let start = sprite.at.wrapping_sub(self.base);
        let held = block.free <= block.size
            && block.first >= block::LEN
            && start >= block.first
            && start.is_multiple_of(4)
            && sprite.next.is_multiple_of(4)
            && sprite.next >= SPRITE_HEADER_LEN
            && u64::from(start) + u64::from(sprite.next) <= u64::from(block.free)
            && sprite.at >= self.base;
        if held {
            Ok(block)
        } else {
            Err(os.error(UNRECOGNISED))
        }
    }
}

impl Area {
    /// Opens `bytes` of room at `offset` among its sprites, moving everything
    /// after up and its free space with it.
    ///
    /// # Errors
    ///
    /// `no_room` where its size does not hold the room; "Unrecognised sprite
    /// data" for a block whose free space lies outside the area or before
    /// `offset`; a fault, nothing yet changed, where memory would refuse the
    /// move.
    pub(crate) fn open(
        self,
        os: &mut Os<'_>,
        offset: u32,
        bytes: u32,
        no_room: tairix_riscos_kernel::error::Error,
    ) -> Result<(), Fail> {
        let block = self.block(os)?;
        if block.free > block.size || offset > block.free {
            return Err(os.error(UNRECOGNISED));
        }
        let free = u64::from(block.free) + u64::from(bytes);
        if free > u64::from(block.size) {
            return Err(os.error(no_room));
        }
        let end = u64::from(self.base) + free;
        if !os.writable(self.address(offset), end) {
            return Err(os.error(UNRECOGNISED));
        }
        if bytes == 0 {
            return Ok(());
        }
        os.copy_bytes(
            self.address(offset),
            self.address(offset + bytes),
            block.free - offset,
        )?;
        self.set(os, block::FREE, block.free + bytes)
    }

    /// Closes `bytes` at `offset` among its sprites, moving everything after
    /// down and its free space with it.
    ///
    /// # Errors
    ///
    /// "Unrecognised sprite data" for bytes or a free space lying outside the
    /// area; a fault, nothing yet changed, where memory would refuse the move.
    pub(crate) fn close(self, os: &mut Os<'_>, offset: u32, bytes: u32) -> Result<(), Fail> {
        let block = self.block(os)?;
        let end = u64::from(offset) + u64::from(bytes);
        if block.free > block.size || end > u64::from(block.free) {
            return Err(os.error(UNRECOGNISED));
        }
        if !os.writable(
            self.address(offset),
            u64::from(self.base) + u64::from(block.free),
        ) {
            return Err(os.error(UNRECOGNISED));
        }
        if bytes == 0 {
            return Ok(());
        }
        os.copy_bytes(
            self.address(offset + bytes),
            self.address(offset),
            block.free - offset - bytes,
        )?;
        self.set(os, block::FREE, block.free - bytes)
    }

    /// Appends `bytes` of room at its free space for a new sprite, counted
    /// among its sprites: the room's offset.
    ///
    /// # Errors
    ///
    /// `no_room` where its size does not hold the room; "Unrecognised sprite
    /// data" or a fault, nothing changed, as [`Area::open`] gives them.
    pub(crate) fn append(
        self,
        os: &mut Os<'_>,
        bytes: u32,
        no_room: tairix_riscos_kernel::error::Error,
    ) -> Result<u32, Fail> {
        let block = self.block(os)?;
        self.open(os, block.free, bytes, no_room)?;
        self.set(os, block::COUNT, block.count.wrapping_add(1))?;
        Ok(block.free)
    }

    /// Removes `sprite`, whole within its sprites, closing up after it.
    ///
    /// # Errors
    ///
    /// As [`Area::holds`] and [`Area::close`] give them.
    pub(crate) fn remove(
        self,
        os: &mut Os<'_>,
        sprite: &super::header::Sprite,
    ) -> Result<(), Fail> {
        let block = self.holds(os, sprite)?;
        self.close(os, sprite.at.wrapping_sub(self.base), sprite.next)?;
        self.set(os, block::COUNT, block.count.wrapping_sub(1))
    }

    /// The offset of `sprite`'s byte `offset` from the area's start.
    pub(crate) const fn offset_of(self, sprite: &super::header::Sprite, offset: u32) -> u32 {
        sprite.at.wrapping_sub(self.base).wrapping_add(offset)
    }
}

/// A walk through an area's sprites.
pub(crate) struct Walk {
    area: Area,
    offset: u32,
    left: u32,
    size: u32,
}

/// The sprites a walk takes between letting the host have its word.
const WALK_SLICE: u32 = 256;

impl Walk {
    /// The next sprite, none past the count.
    ///
    /// # Errors
    ///
    /// "Unrecognised sprite data" where the chain leaves the area or a word,
    /// a fault reading a control block, or the host stopping a long walk.
    pub(crate) fn next(&mut self, os: &mut Os<'_>) -> Result<Option<Sprite>, Fail> {
        if self.left == 0 {
            return Ok(None);
        }
        let inside = self.offset.is_multiple_of(4)
            && u64::from(self.offset) + u64::from(SPRITE_HEADER_LEN) <= u64::from(self.size);
        if !inside {
            return Err(os.error(UNRECOGNISED));
        }
        let sprite = Sprite::read(os, self.area.address(self.offset))?;
        self.left -= 1;
        self.offset = self.offset.wrapping_add(sprite.next);
        if self.left.is_multiple_of(WALK_SLICE) {
            os.yield_point()?;
        }
        Ok(Some(sprite))
    }

    /// The sprites the walk has yet to take.
    pub(crate) const fn counted(&self) -> u32 {
        self.left
    }
}

/// The name a caller gives at `address`: up to twelve bytes, ending at the
/// first control character or space, each byte read only once it is needed.
pub(crate) fn read_name(os: &mut Os<'_>, address: u32) -> Result<SpriteName, Fail> {
    let mut bytes = [0u8; SpriteName::MAX_LEN];
    let mut length = 0;
    for (slot, at) in bytes.iter_mut().zip(0u32..) {
        let byte = os.read_byte(address.wrapping_add(at))?;
        if byte <= b' ' {
            break;
        }
        *slot = byte;
        length += 1;
    }
    Ok(SpriteName::spelt_by(&bytes[..length]))
}
