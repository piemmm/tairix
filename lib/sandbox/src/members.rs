//! A session's two descriptors kept on its owner's wait-set: `Stream` on the
//! read end while the session wants to read, `StreamRoom` on the write end
//! while it wants to write.
//!
//! A direction is registered only while wanted, which is what keeps a
//! level-triggered readiness source from waking an owner with nothing to do.
//! The kernel keys a member by its descriptor's number and resolves that
//! number afresh at every wait, so a member left behind follows whatever is
//! next given the number, and refuses the member its new owner adds for it.
//! [`Members`] therefore takes a member out as soon as it is not wanted, and
//! everything it holds when it is dropped.

use tairix_abi::{Errno, WaitSourceKind};

use crate::session::SessionDescriptors;

/// The wait-set members are kept on.
pub trait MemberSet {
    /// Add a member observing `kind` on descriptor `fd`, reported as `token`.
    ///
    /// # Errors
    ///
    /// The wait-set's typed refusal; nothing was added.
    fn add(&mut self, kind: WaitSourceKind, fd: u32, token: u64) -> Result<(), Errno>;

    /// Take out the member observing `kind` on `fd`. A refusal can only mean
    /// there is none, which is what was asked.
    fn remove(&mut self, kind: WaitSourceKind, fd: u32, token: u64);
}

/// One session's members of `W`, registered exactly while wanted.
pub struct Members<W: MemberSet> {
    set: W,
    read_token: u64,
    write_token: u64,
    read: Option<u32>,
    write: Option<u32>,
}

impl<W: MemberSet> Members<W> {
    /// Members of `set`, reporting the read end's readiness as `read_token`
    /// and the write end's room as `write_token`; none registered yet.
    #[must_use]
    pub const fn over(set: W, read_token: u64, write_token: u64) -> Self {
        Self {
            set,
            read_token,
            write_token,
            read: None,
            write: None,
        }
    }

    /// Register exactly the wanted directions of `descriptors`, none at all
    /// when there are none. Each direction is brought in line whatever came
    /// of the other.
    ///
    /// # Errors
    ///
    /// The wait-set's refusal to add a wanted member: that direction is left
    /// unregistered, and the session cannot be heard in it.
    pub fn sync(
        &mut self,
        descriptors: Option<SessionDescriptors>,
        wants_read: bool,
        wants_write: bool,
    ) -> Result<(), Errno> {
        let read = descriptors.filter(|_| wants_read).map(|d| d.read_fd);
        let write = descriptors.filter(|_| wants_write).map(|d| d.write_fd);
        let (read_token, write_token) = (self.read_token, self.write_token);
        let read = keep(
            &mut self.set,
            (WaitSourceKind::Stream, read_token),
            &mut self.read,
            read,
        );
        let write = keep(
            &mut self.set,
            (WaitSourceKind::StreamRoom, write_token),
            &mut self.write,
            write,
        );
        read.and(write)
    }

    /// Take out every member: the session's worker is gone, and the numbers
    /// of its descriptors are free to be given to another pipe.
    pub fn release(&mut self) {
        if let Some(fd) = self.read.take() {
            self.set.remove(WaitSourceKind::Stream, fd, self.read_token);
        }
        if let Some(fd) = self.write.take() {
            self.set
                .remove(WaitSourceKind::StreamRoom, fd, self.write_token);
        }
    }
}

impl<W: MemberSet> Drop for Members<W> {
    fn drop(&mut self) {
        self.release();
    }
}

/// Move one direction's member from `armed` to `want`.
fn keep<W: MemberSet>(
    set: &mut W,
    (kind, token): (WaitSourceKind, u64),
    armed: &mut Option<u32>,
    want: Option<u32>,
) -> Result<(), Errno> {
    if *armed == want {
        return Ok(());
    }
    if let Some(fd) = armed.take() {
        set.remove(kind, fd, token);
    }
    if let Some(fd) = want {
        set.add(kind, fd, token)?;
        *armed = Some(fd);
    }
    Ok(())
}

#[cfg(test)]
#[path = "members_tests.rs"]
mod tests;
