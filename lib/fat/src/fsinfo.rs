//! FAT32's `FSInfo` sector: hints of the free clusters' count and of where
//! to look for the next, each a hint a reader may not trust, and the
//! signatures that tell the sector is one.

use crate::{le32, put32};

/// The bytes of the structure, at the head of its sector.
pub const LEN: usize = 512;
/// `FSI_LeadSig`.
pub const LEAD: u32 = 0x4161_5252;
/// `FSI_StrucSig`.
pub const STRUCTURE: u32 = 0x6141_7272;
/// `FSI_TrailSig`.
pub const TRAIL: u32 = 0xAA55_0000;
/// A hint's value for "unknown".
pub const UNKNOWN: u32 = 0xFFFF_FFFF;

/// The fields, by the specification's names.
pub mod at {
    /// `FSI_LeadSig`.
    pub const LEAD: usize = 0;
    /// `FSI_StrucSig`.
    pub const STRUCTURE: usize = 484;
    /// `FSI_Free_Count`.
    pub const FREE: usize = 488;
    /// `FSI_Nxt_Free`.
    pub const NEXT: usize = 492;
    /// `FSI_TrailSig`.
    pub const TRAIL: usize = 508;
}

/// What an `FSInfo` sector hints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsInfo {
    /// The free clusters, if known.
    pub free: Option<u32>,
    /// A cluster to start looking for a free one from, if known.
    pub next: Option<u32>,
}

impl FsInfo {
    /// Nothing known: what a new volume carries.
    pub const UNKNOWN: Self = Self {
        free: None,
        next: None,
    };

    /// What `sector` hints, if its signatures say it is an `FSInfo` sector.
    #[must_use]
    pub fn decode(sector: &[u8; LEN]) -> Option<Self> {
        let signed = le32(sector, at::LEAD)? == LEAD
            && le32(sector, at::STRUCTURE)? == STRUCTURE
            && le32(sector, at::TRAIL)? == TRAIL;
        let hint = |at| le32(sector, at).filter(|&value| value != UNKNOWN);
        signed.then(|| Self {
            free: hint(at::FREE),
            next: hint(at::NEXT),
        })
    }

    /// The sector carrying these hints.
    #[must_use]
    pub fn encode(&self) -> [u8; LEN] {
        let mut sector = [0; LEN];
        put32(&mut sector, at::LEAD, LEAD);
        put32(&mut sector, at::STRUCTURE, STRUCTURE);
        put32(&mut sector, at::FREE, self.free.unwrap_or(UNKNOWN));
        put32(&mut sector, at::NEXT, self.next.unwrap_or(UNKNOWN));
        put32(&mut sector, at::TRAIL, TRAIL);
        sector
    }
}

#[cfg(test)]
#[path = "fsinfo_tests.rs"]
mod tests;
