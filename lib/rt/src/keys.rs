//! Keystrokes from standard input, delivered to a wait-set.
//!
//! A console-backed standard input cannot join a wait-set — the stream source
//! admits only a pipe or pty backing — so a program that must wait on its
//! keyboard and on something else at once reads the keyboard on a thread of
//! its own. [`KeyRelay`] is that thread and the private mailbox it forwards
//! to, which the program's wait-set watches. Neither side spins.
//!
//! The reader takes input only while the process holds its terminal's
//! foreground. Refused for want of it, it parks on the terminal's foreground
//! edge and reads again when the hands next change, so a program sent to the
//! background keeps running and takes its keyboard back when it returns. Only
//! end of input — or a refusal that no change of hands can lift — ends it.
//!
//! The mailbox is process-local plumbing, not an interface: each message's
//! attested sender is checked against this process, so nothing else can type
//! into it. Set the input discipline the program wants *before* starting the
//! relay: a keystroke read under the cooked one would already be echoed and
//! held to the end of its line.

use tairix_abi::ipc::{SendRefusal, IPC_MEMORY_RETRY_NS};
use tairix_abi::waitset::WaitSourceKind;
use tairix_abi::{Errno, Origin, ORIGIN_WIRE_LEN, STDIN};

use crate::io::{Read, Stdin};
use crate::thread::Thread;
use crate::{PrivatePort, WaitSet};

/// Largest read the reader makes, and so the mailbox's message size with its
/// tag: a terminal delivers keystrokes a few at a time, and a paste arrives in
/// several messages rather than being cut.
const KEY_CHUNK: usize = 512;

/// Messages the mailbox holds before the reader waits for room.
const KEY_CAPACITY: usize = 64;

/// The message tag: keystrokes follow.
const TAG_TYPED: u8 = 1;
/// The message tag: standard input has ended.
const TAG_ENDED: u8 = 2;

/// The token both of the reader's wait-sets report under.
const TOKEN: u64 = 1;

/// What one message from the reader carries.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Keys<'a> {
    /// Keystrokes, as the terminal delivered them.
    Typed(&'a [u8]),
    /// Standard input has ended; nothing more will arrive.
    Ended,
}

/// The reader thread's mailbox: add [`Self::port`] to a wait-set as a
/// [`WaitSourceKind::Port`] member and [`Self::take`] when it reports.
///
/// Dropping the relay unbinds the mailbox, which ends the reader at its next
/// post.
pub struct KeyRelay {
    port: PrivatePort,
    pid: u64,
    buf: [u8; KEY_CHUNK + 1],
}

impl KeyRelay {
    /// Bind the mailbox and start the reader, which is detached: the
    /// process's exit ends it.
    ///
    /// # Errors
    ///
    /// The refusal of the mailbox, the process's own origin, the reader's
    /// wait-sets, or the thread.
    pub fn start() -> Result<Self, Errno> {
        let pid = crate::self_origin().map_err(Errno::from_syscall)?.pid();
        let port = PrivatePort::bind(KEY_CHUNK + 1, KEY_CAPACITY)?;
        let room = watching(WaitSourceKind::PortRoom, port.id())?;
        let hands = watching(WaitSourceKind::Foreground, u64::from(STDIN))?;
        let pause = WaitSet::create()?;
        let mailbox = port.id();
        Thread::spawn(move || read_keys(mailbox, &room, &hands, &pause))?.detach();
        Ok(Self {
            port,
            pid,
            buf: [0; KEY_CHUNK + 1],
        })
    }

    /// The mailbox the reader posts to.
    #[must_use]
    pub const fn port(&self) -> u64 {
        self.port.id()
    }

    /// Take the oldest message this process's reader sent, discarding any
    /// other; `None` once the mailbox is empty.
    pub fn take(&mut self) -> Option<Keys<'_>> {
        let mut sender = [0u8; ORIGIN_WIRE_LEN];
        let len = loop {
            let len = crate::ipc_recv(self.port.id(), &mut self.buf, &mut sender).ok()?;
            if read_message(self.buf.get(..len)?, &sender, self.pid).is_some() {
                break len;
            }
        };
        read_message(self.buf.get(..len)?, &sender, self.pid)
    }
}

/// What `message` carries, when `sender` is this process's own reader.
fn read_message<'a>(
    message: &'a [u8],
    sender: &[u8; ORIGIN_WIRE_LEN],
    pid: u64,
) -> Option<Keys<'a>> {
    if !Origin::from_bytes(sender).is_ok_and(|origin| origin.pid() == pid) {
        return None;
    }
    match message {
        [TAG_ENDED] => Some(Keys::Ended),
        [TAG_TYPED, typed @ ..] if !typed.is_empty() => Some(Keys::Typed(typed)),
        _ => None,
    }
}

/// The reader: block in a read of standard input, forward what arrived, and
/// on a refusal for want of the foreground wait for the hands to change.
fn read_keys(port: u64, room: &WaitSet, hands: &WaitSet, pause: &WaitSet) {
    let relay = |message: &[u8]| post(room.handle(), pause.handle(), port, message);
    let mut buf = [0u8; KEY_CHUNK + 1];
    buf[0] = TAG_TYPED;
    loop {
        match Stdin.read(&mut buf[1..]) {
            Ok(0) => break,
            Ok(read) => {
                if !relay(&buf[..=read]) {
                    return;
                }
            }
            Err(err) if err.errno() == Some(Errno::NotForeground) => {
                let mut token = 0;
                if crate::waitset_wait(hands.handle(), u64::MAX, &mut token) != 0 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = relay(&[TAG_ENDED]);
}

/// A wait-set holding one member.
fn watching(kind: WaitSourceKind, id: u64) -> Result<WaitSet, Errno> {
    let set = WaitSet::create()?;
    set.add(kind, id, TOKEN)?;
    Ok(set)
}

/// Post `message`, waiting rather than dropping a keystroke: on `room` while
/// the mailbox is full or the process's budget spent, both of which its room
/// member reports, and on the empty `pause` for [`IPC_MEMORY_RETRY_NS`] while
/// the kernel has no memory for it, which nothing reports; `false` once the
/// send is refused for good.
fn post(room: u64, pause: u64, port: u64, message: &[u8]) -> bool {
    loop {
        let Err(refused) = crate::ipc_send(port, message) else {
            return true;
        };
        let (wait_on, timeout) = match SendRefusal::of(refused) {
            SendRefusal::Full | SendRefusal::Budget => (room, u64::MAX),
            SendRefusal::Memory => (pause, IPC_MEMORY_RETRY_NS),
            // The mailbox unbound is how the relay is told to end.
            SendRefusal::Gone => return false,
            SendRefusal::Final(refused) => {
                crate::io::write_stderr_line(format_args!(
                    "keys: a keystroke could not be relayed: {refused}"
                ));
                return false;
            }
        };
        let mut token = 0;
        let waited = crate::waitset_wait(wait_on, timeout, &mut token);
        if waited != 0 && Errno::from_syscall(waited) != Errno::TimedOut {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use tairix_abi::{
        CapabilitySummary, Errno, Origin, ProcId, TrustDomain, ORIGIN_CONSOLE_NONE, ORIGIN_WIRE_LEN,
    };
    use tairix_abi_trap::seam::{arm_script, calls, refusal};

    use super::{post, read_message, Keys, IPC_MEMORY_RETRY_NS, TAG_ENDED, TAG_TYPED};
    use crate::{NUM_IPC_SEND, NUM_WAITSET_WAIT};

    fn from(pid: u64) -> [u8; ORIGIN_WIRE_LEN] {
        Origin::new(
            TrustDomain::User,
            1000,
            1000,
            pid,
            ProcId::from_raw([7; 16]),
            CapabilitySummary::EMPTY,
            ORIGIN_CONSOLE_NONE,
        )
        .to_le_bytes()
    }

    #[test]
    fn the_readers_own_messages_carry_keystrokes_or_the_end() {
        assert_eq!(
            read_message(&[TAG_TYPED, b'q', 0x1b], &from(7), 7),
            Some(Keys::Typed(&[b'q', 0x1b]))
        );
        assert_eq!(read_message(&[TAG_ENDED], &from(7), 7), Some(Keys::Ended));
    }

    /// The mailbox's id is no secret from a process that can watch this one,
    /// so what it is sent is believed only from this process.
    #[test]
    fn nothing_another_process_sends_is_typed() {
        assert_eq!(read_message(&[TAG_TYPED, b'q'], &from(8), 7), None);
        assert_eq!(read_message(&[TAG_ENDED], &from(8), 7), None);
    }

    /// A keystroke the mailbox or the budget refused waits on the room member,
    /// one the kernel had no memory for pauses on the empty set, and each is
    /// sent once its wait ends; a refusal no wait lifts ends the relay.
    #[test]
    fn a_refused_keystroke_waits_for_what_refused_it_and_is_then_sent() {
        const ROOM: u64 = 3;
        const PAUSE: u64 = 4;
        const MAILBOX: u64 = 9;
        for (refused, set, timeout) in [
            (Errno::WouldBlock, ROOM, u64::MAX),
            (Errno::LimitExceeded, ROOM, u64::MAX),
            (Errno::OutOfMemory, PAUSE, IPC_MEMORY_RETRY_NS),
        ] {
            arm_script(&[refusal(refused), refusal(Errno::TimedOut)], 0);
            assert!(post(ROOM, PAUSE, MAILBOX, b"q"), "{refused:?}");
            let traps: Vec<(u64, u64, u64)> = calls()
                .iter()
                .map(|(number, args)| (*number, args[0], args[1]))
                .collect();
            assert_eq!(traps.len(), 3, "{refused:?}");
            assert_eq!((traps[0].0, traps[0].1), (NUM_IPC_SEND, MAILBOX));
            assert_eq!(traps[1], (NUM_WAITSET_WAIT, set, timeout), "{refused:?}");
            assert_eq!((traps[2].0, traps[2].1), (NUM_IPC_SEND, MAILBOX));
        }
        arm_script(&[refusal(Errno::NotFound)], 0);
        assert!(
            !post(ROOM, PAUSE, MAILBOX, b"q"),
            "the mailbox gone ends the relay"
        );
        assert_eq!(calls().len(), 1, "without a wait");
        arm_script(&[refusal(Errno::WouldBlock), refusal(Errno::NotFound)], 0);
        assert!(
            !post(ROOM, PAUSE, MAILBOX, b"q"),
            "a wait that fails ends it"
        );
    }

    #[test]
    fn a_message_of_no_known_shape_carries_nothing() {
        for message in [&[][..], &[TAG_TYPED], &[TAG_ENDED, 0], &[0, b'q'], &[3]] {
            assert_eq!(read_message(message, &from(7), 7), None, "{message:?}");
        }
    }
}
