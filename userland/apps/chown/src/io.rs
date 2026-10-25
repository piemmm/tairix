//! The seam `chown` reads owner and group names through, and the stream it
//! reports on.

use alloc::string::String;

use tairix_abi::Errno;

pub use tairix_util::output::Output;

/// An account the directory lists, as an owner operand names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Account {
    /// Its uid.
    pub uid: u32,
    /// Its login group, which `OWNER:` gives.
    pub login_group: u32,
}

/// The account and group directories an owner operand's names are read
/// against.
pub trait Accounts {
    /// The account named `name`, or [`None`] when the directory lists none.
    ///
    /// # Errors
    ///
    /// The directory could not be read, which is never taken for an absent
    /// name.
    fn user(&self, name: &str) -> Result<Option<Account>, Errno>;

    /// The gid of the group named `name`, or [`None`] when the directory
    /// lists none.
    ///
    /// # Errors
    ///
    /// As [`Accounts::user`].
    fn group(&self, name: &str) -> Result<Option<u32>, Errno>;

    /// The name of the account `uid`, or [`None`] when the directory lists
    /// none.
    ///
    /// # Errors
    ///
    /// As [`Accounts::user`].
    fn user_name(&self, uid: u32) -> Result<Option<String>, Errno>;

    /// The name of the group `gid`, or [`None`] when the directory lists none.
    ///
    /// # Errors
    ///
    /// As [`Accounts::user`].
    fn group_name(&self, gid: u32) -> Result<Option<String>, Errno>;
}
