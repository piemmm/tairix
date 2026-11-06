//! Cinder's icon-bar declaration: a primary click raises or reopens his pen,
//! and his one row sends him out or brings him home.
//!
//! Here rather than in the program body because two readers need it: the
//! running program, which matches the chosen row back to the action, and the
//! desktop QEMU vertical, which reconstructs the menu to know where to click.

use tairix_abi::window_ipc::{
    AppBar, AppBarClick, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuRow, AppMenuSymbol,
};
use tairix_abi::Errno;
use tairix_window::QUIT_ROW;

use crate::pen::{BRING_HOME_LABEL, LET_OUT_LABEL};

/// The id of the row that lets him out or brings him home: the first past the
/// convention's *Quit* row, so the two can never collide.
pub const ROW_OUT: u16 = QUIT_ROW + 1;

/// His declaration for `endpoint`: the row offers whichever of letting him out
/// or bringing him home he is not, by `loose`, in the pen button's own words.
///
/// # Errors
///
/// Whatever the bounded menu builder refuses ([`tairix_window::declaration`]).
pub fn declaration(endpoint: u64, loose: bool) -> Result<AppBar, Errno> {
    let (label, symbol) = if loose {
        (BRING_HOME_LABEL, AppMenuSymbol::BringHome)
    } else {
        (LET_OUT_LABEL, AppMenuSymbol::LetOut)
    };
    let row = AppMenuRow::Item(AppMenuItem::new(
        AppMenuItemId::new(ROW_OUT)?,
        AppMenuLabel::new(label)?,
        symbol,
    ));
    tairix_window::declaration(endpoint, AppBarClick::RaiseOrOpen, &[row])
}

#[cfg(test)]
#[path = "appbar_tests.rs"]
mod tests;
