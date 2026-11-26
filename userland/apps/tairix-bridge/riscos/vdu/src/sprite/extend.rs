//! SpriteExtend 1.86: `SpriteV`'s default in the kernel's place while it is
//! active, taking its own reasons — verification, rows and columns, joining,
//! palettes and alpha, and the scaled and transformed plots — and passing
//! every other to the kernel's, as the module in front of the kernel's
//! claim does. Killed, its reasons are unknown to the kernel.

use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::module::{Builtin, VectorDefault};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::vectors;

use super::area::{Area, Naming};
use super::create::Target;
use super::edit::{self, Change};
use super::paint::{self, PlotFlags};
use super::{alpha, join, jpeg, reason, verify};
use crate::inverse::Kept;
use tairix_riscos_kernel::module::Swis;

/// "SpriteExtend does not plot grey-scaled sprites": reason 53, gone since
/// SpriteExtend 0.99.
pub(crate) const NO_GREY_SCALED: Error = Error {
    number: 0x71F,
    token: "NoGreyScaledSprites",
};

/// SpriteExtend.
pub(crate) static SPRITE_EXTEND: Builtin = Builtin {
    title: "SpriteExtend",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: Some(Swis {
        chunk: jpeg::CHUNK,
        prefix: "JPEG",
        names: jpeg::NAMES,
        handler: jpeg::swi,
    }),
    services: None,
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[],
    vectors: &[VectorDefault {
        vector: vectors::SPRITE,
        code: vector,
    }],
    resources: &[],
    code: None,
};

/// SpriteExtend's state: `JPEG_PDriverIntercept`'s flags, and the 32K table
/// its plots onto a palettised destination last took.
#[derive(Default)]
pub(crate) struct State {
    pub(super) intercept: u32,
    pub(super) inverse: Kept,
}

fn init(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.hold_extension(State::default())
}

#[expect(clippy::unnecessary_wraps, reason = "a module entry's signature")]
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.release_extension::<State>();
    Ok(())
}

/// `SpriteV` while SpriteExtend is active.
fn vector(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let Some(naming) = Naming::of(regs.r[0]) else {
        return super::kernel(os, regs);
    };
    let asked = regs.r[0] & tairix_riscos_kernel::swi::sprite_op::REASON;
    match asked {
        reason::VERIFY => verify::verify(os, naming, regs),
        reason::PUT_GREY_SCALED => Err(os.error(NO_GREY_SCALED)),
        reason::POINTER => {
            let area = Area::named(os, naming, regs.r[1])?;
            let sprite = area.sprite(os, naming, regs.r[2])?;
            paint::set_pointer(os, &sprite, regs)
        }
        reason::PLOT_MASK_SCALED
        | reason::PUT_SCALED
        | reason::PLOT_MASK_TRANSFORMED
        | reason::PUT_TRANSFORMED
        | reason::TILE => {
            let area = Area::named(os, naming, regs.r[1])?;
            let sprite = area.sprite(os, naming, regs.r[2])?;
            let [_, _, _, r3, r4, r5, r6, r7, ..] = regs.r;
            let flags = PlotFlags(r5);
            match asked {
                reason::PLOT_MASK_SCALED => {
                    let at = paint::at(os, r3, r4)?;
                    paint::plot_mask_scaled(os, &sprite, at, r6)
                }
                reason::PUT_SCALED => {
                    let at = paint::at(os, r3, r4)?;
                    paint::put_scaled(os, &sprite, at, flags, r6, r7)
                }
                reason::PLOT_MASK_TRANSFORMED => {
                    paint::plot_mask_transformed(os, &sprite, r3, r4, r6)
                }
                reason::PUT_TRANSFORMED => {
                    paint::put_transformed(os, &sprite, r3, r4, flags, r6, r7)
                }
                _ => {
                    let at = paint::at(os, r3, r4)?;
                    paint::tile(os, &sprite, at, flags, r6, r7)
                }
            }
        }
        reason::INSERT_ROW
        | reason::DELETE_ROW
        | reason::APPEND
        | reason::PALETTE
        | reason::ALPHA
        | reason::INSERT_COLUMN
        | reason::DELETE_COLUMN
        | reason::ROWS
        | reason::COLUMNS => {
            let area = Area::named(os, naming, regs.r[1])?;
            let sprite = area.sprite(os, naming, regs.r[2])?;
            let target = Target {
                area,
                naming,
                sprite,
            };
            let one = |insert: bool| {
                if insert {
                    Change::Insert {
                        at: regs.r[3],
                        count: 1,
                    }
                } else {
                    Change::Delete {
                        at: regs.r[3],
                        count: 1,
                    }
                }
            };
            match asked {
                reason::INSERT_ROW => edit::rows(os, &target, one(true)),
                reason::DELETE_ROW => edit::rows(os, &target, one(false)),
                reason::ROWS => edit::rows(os, &target, Change::of(regs)),
                reason::INSERT_COLUMN => edit::columns(os, &target, one(true)),
                reason::DELETE_COLUMN => edit::columns(os, &target, one(false)),
                reason::COLUMNS => edit::columns(os, &target, Change::of(regs)),
                reason::APPEND => join::append(os, &target, regs),
                reason::PALETTE => edit::palette(os, &target, regs),
                _ => alpha::convert(os, &target, regs),
            }
        }
        _ => super::kernel(os, regs),
    }
}
