//! The 64-byte SMB2 header (MS-SMB2 §2.2.1) and the command set.

use tairix_util::flags;

use crate::status::NtStatus;
use crate::wire::{Encoder, Reader};
use crate::SmbError;

/// The four bytes an SMB2 message begins with.
pub const PROTOCOL_ID: [u8; 4] = [0xfe, b'S', b'M', b'B'];

/// The header's length, and the offset every message body starts at.
pub const HEADER_LEN: usize = 64;

/// Where the header's signature lies, zeroed while one is computed.
pub const SIGNATURE_RANGE: core::ops::Range<usize> = 48..64;

/// Where the header's flags lie.
pub const FLAGS_RANGE: core::ops::Range<usize> = 16..20;

/// Where the header's `NextCommand` lies: the offset of a compound's next
/// message.
pub const NEXT_COMMAND_RANGE: core::ops::Range<usize> = 20..24;

/// An SMB2 command.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Command {
    /// `NEGOTIATE`.
    Negotiate,
    /// `SESSION_SETUP`.
    SessionSetup,
    /// `LOGOFF`.
    Logoff,
    /// `TREE_CONNECT`.
    TreeConnect,
    /// `TREE_DISCONNECT`.
    TreeDisconnect,
    /// `CREATE`.
    Create,
    /// `CLOSE`.
    Close,
    /// `FLUSH`.
    Flush,
    /// `READ`.
    Read,
    /// `WRITE`.
    Write,
    /// `LOCK`.
    Lock,
    /// `IOCTL`.
    Ioctl,
    /// `CANCEL`.
    Cancel,
    /// `ECHO`.
    Echo,
    /// `QUERY_DIRECTORY`.
    QueryDirectory,
    /// `CHANGE_NOTIFY`.
    ChangeNotify,
    /// `QUERY_INFO`.
    QueryInfo,
    /// `SET_INFO`.
    SetInfo,
    /// `OPLOCK_BREAK`, which also carries lease breaks.
    OplockBreak,
}

impl Command {
    const ALL: [Self; 19] = [
        Self::Negotiate,
        Self::SessionSetup,
        Self::Logoff,
        Self::TreeConnect,
        Self::TreeDisconnect,
        Self::Create,
        Self::Close,
        Self::Flush,
        Self::Read,
        Self::Write,
        Self::Lock,
        Self::Ioctl,
        Self::Cancel,
        Self::Echo,
        Self::QueryDirectory,
        Self::ChangeNotify,
        Self::QueryInfo,
        Self::SetInfo,
        Self::OplockBreak,
    ];

    /// The command's code on the wire.
    #[must_use]
    pub const fn code(self) -> u16 {
        self as u16
    }

    /// The command with wire code `code`.
    #[must_use]
    pub fn from_code(code: u16) -> Option<Self> {
        Self::ALL.get(usize::from(code)).copied()
    }
}

flags! {
    /// The header's flags (MS-SMB2 §2.2.1.2).
    pub struct Flags(u32) {
        /// The message is a response.
        const SERVER_TO_REDIR = 0x0000_0001;
        /// The header names an asynchronous operation rather than a tree.
        const ASYNC_COMMAND = 0x0000_0002;
        /// The message is a related operation within a compound.
        const RELATED_OPERATIONS = 0x0000_0004;
        /// The message is signed.
        const SIGNED = 0x0000_0008;
        /// The path is a DFS path.
        const DFS_OPERATIONS = 0x1000_0000;
        /// The request replays one whose response was lost.
        const REPLAY_OPERATION = 0x2000_0000;
    }
}

/// What a header is addressed by: a tree for a synchronous message, or the
/// id an asynchronous operation was given.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Address {
    /// A synchronous message on a tree (zero before one is connected).
    Sync {
        /// The tree.
        tree_id: u32,
    },
    /// A message about an operation the server made asynchronous.
    Async {
        /// The id the server's interim response gave it.
        async_id: u64,
    },
}

/// An SMB2 header.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Header {
    /// How many credits the message consumes.
    pub credit_charge: u16,
    /// A response's status; zero in a request.
    pub status: NtStatus,
    /// The command.
    pub command: Command,
    /// Credits requested by a request or granted by a response.
    pub credits: u16,
    /// The flags, but [`Flags::ASYNC_COMMAND`], which `address` decides.
    pub flags: Flags,
    /// The offset of the next message in a compound, or zero for the last.
    pub next_command: u32,
    /// The message id.
    pub message_id: u64,
    /// The tree or asynchronous operation.
    pub address: Address,
    /// The session, zero before one exists.
    pub session_id: u64,
    /// The signature, zero when unsigned.
    pub signature: [u8; 16],
}

impl Header {
    /// Append the header's 64 bytes.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.bytes(&PROTOCOL_ID);
        enc.u16(64);
        enc.u16(self.credit_charge);
        enc.u32(self.status.code());
        enc.u16(self.command.code());
        enc.u16(self.credits);
        let flags = match self.address {
            Address::Sync { .. } => self.flags.without(Flags::ASYNC_COMMAND),
            Address::Async { .. } => self.flags.with(Flags::ASYNC_COMMAND),
        };
        enc.u32(flags.bits());
        enc.u32(self.next_command);
        enc.u64(self.message_id);
        match self.address {
            Address::Sync { tree_id } => {
                enc.u32(0);
                enc.u32(tree_id);
            }
            Address::Async { async_id } => enc.u64(async_id),
        }
        enc.u64(self.session_id);
        enc.bytes(&self.signature);
    }

    /// The header at the start of `message`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] for a short message, another protocol, or a
    /// structure size other than 64; [`SmbError::Protocol`] for a command
    /// SMB2 does not define.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = Reader::new(message);
        if r.array::<4>()? != PROTOCOL_ID || r.u16()? != 64 {
            return Err(SmbError::Malformed);
        }
        let credit_charge = r.u16()?;
        let status = NtStatus::from_code(r.u32()?);
        let command = Command::from_code(r.u16()?).ok_or(SmbError::Protocol)?;
        let credits = r.u16()?;
        let flags = Flags::from_bits(r.u32()?);
        let next_command = r.u32()?;
        let message_id = r.u64()?;
        let address = if flags.contains(Flags::ASYNC_COMMAND) {
            Address::Async { async_id: r.u64()? }
        } else {
            r.skip(4)?;
            Address::Sync { tree_id: r.u32()? }
        };
        Ok(Self {
            credit_charge,
            status,
            command,
            credits,
            flags: flags.without(Flags::ASYNC_COMMAND),
            next_command,
            message_id,
            address,
            session_id: r.u64()?,
            signature: r.array()?,
        })
    }

    /// Whether the header is a response's.
    #[must_use]
    pub const fn is_response(&self) -> bool {
        self.flags.contains(Flags::SERVER_TO_REDIR)
    }
}

/// Check a message's fixed `StructureSize` against `expected`, reading it
/// from the start of the body.
///
/// # Errors
///
/// [`SmbError::Malformed`] when it differs or the body is short.
pub fn structure(message: &[u8], expected: u16) -> Result<Reader<'_>, SmbError> {
    let mut r = Reader::at(message, HEADER_LEN)?;
    if r.u16()? == expected {
        Ok(r)
    } else {
        Err(SmbError::Malformed)
    }
}
