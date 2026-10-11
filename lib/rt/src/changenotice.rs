//! Publishing a counted change notice: a topic whose payload is a service's
//! count of changes so far ([`NoticeTopic::AudioDevices`],
//! [`NoticeTopic::Network`]), telling every subscriber to read the state
//! again.
//!
//! The kernel keeps a topic's value across its publisher's life and takes a
//! re-statement of that value as no change, waking nobody. A restarted
//! publisher counting from zero could announce the very count its last
//! instance left and so leave every subscriber holding that instance's state,
//! which is why a count resumes past the value already on the board.

use tairix_abi::{Errno, Notice, NoticeTopic, NOTICE_PAYLOAD_MAX};

use crate::{notice_publish, notice_read};

/// One service's announcements on a counted topic.
#[derive(Debug)]
pub struct ChangeNotice {
    topic: NoticeTopic,
    announced: Option<u64>,
}

impl ChangeNotice {
    /// Take up the counted `topic`, answering the count the service resumes
    /// from: one past the count on the board, so its first announcement wakes
    /// every subscriber whatever an earlier instance last said.
    #[must_use]
    pub fn resume(topic: NoticeTopic) -> (Self, u64) {
        let mut buf = [0u8; NOTICE_PAYLOAD_MAX];
        let board = usize::try_from(notice_read(topic, &mut buf))
            .ok()
            .and_then(|len| buf.get(..len))
            .and_then(|bytes| Notice::decode(topic, bytes).ok());
        (
            Self {
                topic,
                announced: None,
            },
            resumed_after(board.as_ref()),
        )
    }

    /// Announce `changes`, unless it is the count last announced.
    ///
    /// # Errors
    ///
    /// The kernel's refusal (`Errno::PermissionDenied` without the topic's
    /// authority), or [`Errno::OutOfRange`] for a topic that carries no
    /// count. Neither can change while the service runs, so a refused count is
    /// not offered again.
    pub fn announce(&mut self, changes: u64) -> Result<(), Errno> {
        if self.announced == Some(changes) {
            return Ok(());
        }
        self.announced = Some(changes);
        let notice = Notice::counted(self.topic, changes).ok_or(Errno::OutOfRange)?;
        match notice_publish(&notice) {
            0 => Ok(()),
            refused => Err(Errno::from_syscall(refused)),
        }
    }
}

/// The count a publisher resumes from after `board`, the topic's value as the
/// kernel holds it.
fn resumed_after(board: Option<&Notice>) -> u64 {
    board
        .and_then(Notice::changes)
        .map_or(1, |last| last.wrapping_add(1))
}

#[cfg(test)]
#[path = "changenotice_tests.rs"]
mod tests;
