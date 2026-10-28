//! Leases (MS-SMB2 §2.2.13.2.10, §2.2.23–§2.2.25): what a client may cache,
//! and the break that takes caching back.
//!
//! A break arrives unsolicited, outside any request's reply, and may arrive
//! unsigned (MS-SMB2 §5.1): it is acted on because it only ever asks the
//! client to cache less. What to flush before acknowledging it is the
//! caller's coherence protocol, not this codec's.

use tairix_util::flags;

use crate::file::FileId;
use crate::header::structure;
use crate::wire::{Encoder, Reader};
use crate::SmbError;

/// A lease key: the client's name for one lease, shared by every open of a
/// file it caches.
pub type LeaseKey = [u8; 16];

flags! {
    /// What a lease lets the client cache.
    pub struct LeaseState(u32) {
        /// Reads.
        const READ = 0x01;
        /// The open itself, past its close.
        const HANDLE = 0x02;
        /// Writes, flushed later.
        const WRITE = 0x04;
    }
}

/// The lease flag naming a parent's lease key.
const PARENT_KEY_SET: u32 = 0x04;

/// The lease flag saying a break is under way.
const BREAK_IN_PROGRESS: u32 = 0x02;

/// The lease create context's data length (`SMB2_CREATE_REQUEST_LEASE_V2`,
/// `SMB2_CREATE_RESPONSE_LEASE_V2`).
pub(crate) const LEASE_CONTEXT_LEN: usize = 52;

/// A lease, as a create asks for one and as the server grants it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Lease {
    /// The lease's key.
    pub key: LeaseKey,
    /// The caching asked for, or granted.
    pub state: LeaseState,
    /// The lease on the parent directory, which the server breaks when this
    /// file's name changes.
    pub parent: Option<LeaseKey>,
    /// The lease's version: the server's count of its state changes.
    pub epoch: u16,
    /// A break is under way: granted only, never asked for.
    pub breaking: bool,
}

impl Lease {
    pub(crate) fn encode(&self, enc: &mut Encoder) {
        enc.bytes(&self.key);
        enc.u32(self.state.bits());
        let mut flags = 0;
        if self.parent.is_some() {
            flags |= PARENT_KEY_SET;
        }
        if self.breaking {
            flags |= BREAK_IN_PROGRESS;
        }
        enc.u32(flags);
        enc.u64(0);
        enc.bytes(&self.parent.unwrap_or_default());
        enc.u16(self.epoch);
        enc.u16(0);
    }

    pub(crate) fn decode(data: &[u8]) -> Result<Self, SmbError> {
        if data.len() != LEASE_CONTEXT_LEN {
            return Err(SmbError::Malformed);
        }
        let mut r = Reader::new(data);
        let key = r.array()?;
        let state = LeaseState::from_bits(r.u32()?);
        let flags = r.u32()?;
        r.skip(8)?;
        let parent: LeaseKey = r.array()?;
        let epoch = r.u16()?;
        Ok(Self {
            key,
            state,
            parent: (flags & PARENT_KEY_SET != 0).then_some(parent),
            epoch,
            breaking: flags & BREAK_IN_PROGRESS != 0,
        })
    }
}

/// The notification flag asking for an acknowledgment.
const ACK_REQUIRED: u32 = 0x01;

/// A server's notice that a lease is being broken (MS-SMB2 §2.2.23.2).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LeaseBreak {
    /// The lease's version after the break.
    pub epoch: u16,
    /// Whether the break waits for an acknowledgment.
    pub ack_required: bool,
    /// The lease's key.
    pub key: LeaseKey,
    /// The caching the client held.
    pub current: LeaseState,
    /// The caching the client may keep.
    pub new: LeaseState,
}

/// A server's notice that an oplock is being broken (MS-SMB2 §2.2.23.1).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OplockBreak {
    /// The most the client may keep.
    pub level: u8,
    /// The open whose oplock it is.
    pub file_id: FileId,
}

/// An unsolicited break, of a lease or an oplock.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BreakNotice {
    /// A lease break.
    Lease(LeaseBreak),
    /// An oplock break, which a client asking only for leases never holds.
    Oplock(OplockBreak),
}

impl BreakNotice {
    /// The break in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] for anything else.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        if let Ok(mut r) = structure(message, 44) {
            let epoch = r.u16()?;
            let flags = r.u32()?;
            let key = r.array()?;
            let current = LeaseState::from_bits(r.u32()?);
            let new = LeaseState::from_bits(r.u32()?);
            r.skip(12)?;
            return Ok(Self::Lease(LeaseBreak {
                epoch,
                ack_required: flags & ACK_REQUIRED != 0,
                key,
                current,
                new,
            }));
        }
        let mut r = structure(message, 24)?;
        let level = r.u8()?;
        r.skip(5)?;
        Ok(Self::Oplock(OplockBreak {
            level,
            file_id: r.array()?,
        }))
    }

    /// Append the break's body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        match self {
            Self::Lease(lease) => {
                enc.u16(44);
                enc.u16(lease.epoch);
                enc.u32(if lease.ack_required { ACK_REQUIRED } else { 0 });
                enc.bytes(&lease.key);
                enc.u32(lease.current.bits());
                enc.u32(lease.new.bits());
                enc.zeros(12);
            }
            Self::Oplock(oplock) => {
                enc.u16(24);
                enc.u8(oplock.level);
                enc.zeros(5);
                enc.bytes(&oplock.file_id);
            }
        }
    }
}

/// A lease break's acknowledgment, and the server's response to it, which
/// share one layout (MS-SMB2 §2.2.24.2, §2.2.25.2).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LeaseAck {
    /// The lease's key.
    pub key: LeaseKey,
    /// The caching kept: in an acknowledgment, at most the break's new state.
    pub state: LeaseState,
}

impl LeaseAck {
    /// Append the body after its header.
    pub fn encode(&self, enc: &mut Encoder) {
        enc.u16(36);
        enc.u16(0);
        enc.u32(0);
        enc.bytes(&self.key);
        enc.u32(self.state.bits());
        enc.u64(0);
    }

    /// The acknowledgment or response in `message`, header included.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] when it is not one.
    pub fn decode(message: &[u8]) -> Result<Self, SmbError> {
        let mut r = structure(message, 36)?;
        r.skip(6)?;
        let key = r.array()?;
        let state = LeaseState::from_bits(r.u32()?);
        r.skip(8)?;
        Ok(Self { key, state })
    }
}

#[cfg(test)]
#[path = "lease_tests.rs"]
mod tests;
