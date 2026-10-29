//! The shares the service has mounted: whose each is, which login session it
//! ends with, and which connection serves it.
//!
//! A mount is found by its offer, the name the kernel gave it and the caller
//! names it by. A login session's anchor is counted so it is watched once
//! however many mounts end with it, and every mount of it is withdrawn when it
//! ends.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::netattach::{OfferId, OFFER_KEY_LEN};
use tairix_abi::netshare_ipc::{MountEntry, MountState, SHARE_NAME_BYTES};
use tairix_abi::ProcId;
use tairix_collections::HashMap;
use tairix_crypto::Sha256Stream;
use tairix_hash::BuildFastHash;
use tairix_inline::ArrayString;

use crate::host::ConnId;

/// Separates a volume's key from every other use of the same digest.
const KEY_LABEL: &[u8] = b"tairix smbfs volume key\0";

/// The key a server's share is offered under: the same for the same share of
/// the same server, so a share keeps one volume identity across mounts, and
/// different for a server answering with another GUID. A share's name is
/// folded as SMB matches it, ASCII case aside.
#[must_use]
pub fn volume_key(server_guid: &[u8; 16], share: &str) -> [u8; OFFER_KEY_LEN] {
    let mut digest = Sha256Stream::new();
    digest.update(KEY_LABEL);
    digest.update(server_guid);
    for byte in share.bytes() {
        digest.update(&[byte.to_ascii_lowercase()]);
    }
    let mut key = [0; OFFER_KEY_LEN];
    key.copy_from_slice(&digest.finalize()[..OFFER_KEY_LEN]);
    key
}

/// No room could be had for a record.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NoRoom;

/// One mounted share.
#[derive(Debug)]
pub struct Mount {
    /// Its offer.
    pub offer: OfferId,
    /// The principal it is for.
    pub uid: u32,
    /// The login session it ends with.
    pub session: ProcId,
    /// The connection that serves it.
    pub conn: ConnId,
    /// The volume key it was offered under.
    pub key: [u8; OFFER_KEY_LEN],
    /// The host as the caller named it.
    pub host: String,
    /// The share.
    pub share: ArrayString<SHARE_NAME_BYTES>,
    /// Whether it is served read-only.
    pub read_only: bool,
}

impl Mount {
    /// The entry a listing of its owner's mounts shows, its connection
    /// standing as `state`.
    #[must_use]
    pub fn entry(&self, state: MountState) -> MountEntry<'_> {
        MountEntry {
            offer: self.offer,
            host: &self.host,
            share: &self.share,
            read_only: self.read_only,
            state,
        }
    }
}

/// Every mount, and the login sessions they end with.
pub struct Mounts {
    by_offer: HashMap<u64, Mount, BuildFastHash>,
    anchors: HashMap<ProcId, u32, BuildFastHash>,
}

impl Default for Mounts {
    fn default() -> Self {
        Self::new()
    }
}

impl Mounts {
    /// No mounts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            by_offer: HashMap::with_hasher(BuildFastHash::new()),
            anchors: HashMap::with_hasher(BuildFastHash::new()),
        }
    }

    /// Whether `uid` holds a mount offered under `key`.
    #[must_use]
    pub fn holds(&self, uid: u32, key: &[u8; OFFER_KEY_LEN]) -> bool {
        self.by_offer
            .values()
            .any(|mount| mount.uid == uid && mount.key == *key)
    }

    /// Record `mount`, answering whether it is the first of its session — the
    /// one whose anchor must be watched.
    ///
    /// # Errors
    ///
    /// [`NoRoom`] when no room can be had for it, and for an offer already
    /// recorded, which the kernel never names twice.
    pub fn add(&mut self, mount: Mount) -> Result<bool, NoRoom> {
        let offer = mount.offer.get();
        if self.by_offer.contains_key(&offer)
            || self.by_offer.try_reserve(1).is_err()
            || self.anchors.try_reserve(1).is_err()
        {
            return Err(NoRoom);
        }
        let session = mount.session;
        self.by_offer.try_insert(offer, mount).map_err(|_| NoRoom)?;
        if let Some(count) = self.anchors.get_mut(&session) {
            *count += 1;
            return Ok(false);
        }
        if self.anchors.try_insert(session, 1).is_err() {
            self.by_offer.remove(&offer);
            return Err(NoRoom);
        }
        Ok(true)
    }

    /// The mount `offer` names, when `uid` made it.
    #[must_use]
    pub fn owned(&self, uid: u32, offer: OfferId) -> Option<&Mount> {
        self.by_offer
            .get(&offer.get())
            .filter(|mount| mount.uid == uid)
    }

    /// Take the mount `offer` names out, with whether it was the last of its
    /// session — whose anchor need no longer be watched.
    pub fn remove(&mut self, offer: OfferId) -> Option<(Mount, bool)> {
        let mount = self.by_offer.remove(&offer.get())?;
        let last = match self.anchors.get_mut(&mount.session) {
            Some(count) if *count > 1 => {
                *count -= 1;
                false
            }
            Some(_) => {
                self.anchors.remove(&mount.session);
                true
            }
            None => false,
        };
        Some((mount, last))
    }

    /// The offer of a mount `pick` chooses, if one does.
    pub fn first(&self, pick: impl Fn(&Mount) -> bool) -> Option<OfferId> {
        self.by_offer
            .values()
            .find(|mount| pick(mount))
            .map(|mount| mount.offer)
    }

    /// The entries of `uid`'s mounts, in offer order, each connection
    /// standing as `state` says.
    ///
    /// # Errors
    ///
    /// [`NoRoom`] when no room can be had for them.
    pub fn entries(
        &self,
        uid: u32,
        state: impl Fn(&Mount) -> MountState,
    ) -> Result<Vec<MountEntry<'_>>, NoRoom> {
        let mut entries = Vec::new();
        for mount in self.by_offer.values().filter(|mount| mount.uid == uid) {
            entries.try_reserve(1).map_err(|_| NoRoom)?;
            entries.push(mount.entry(state(mount)));
        }
        entries.sort_unstable_by_key(|entry| entry.offer);
        Ok(entries)
    }

    /// The mount `offer` names.
    #[must_use]
    pub fn get(&self, offer: OfferId) -> Option<&Mount> {
        self.by_offer.get(&offer.get())
    }

    /// Whether any mount ends with the session `anchor` names.
    #[must_use]
    pub fn watches(&self, anchor: ProcId) -> bool {
        self.anchors.contains_key(&anchor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tairix_collections::SlotMap;

    fn mount(offer: u64, uid: u32, session: u8, share: &str) -> Mount {
        Mount {
            offer: OfferId::new(offer).expect("not zero"),
            uid,
            session: ProcId::from_raw([session; 16]),
            conn: SlotMap::<ConnId, ()>::new()
                .try_insert(())
                .expect("an empty table takes one"),
            key: volume_key(b"tairix test peer", share),
            host: String::from("nas.example"),
            share: ArrayString::try_from(share).expect("fits"),
            read_only: false,
        }
    }

    #[test]
    fn a_share_s_key_follows_its_server_and_its_name_folded() {
        let key = volume_key(b"tairix test peer", "Media");
        assert_eq!(key, volume_key(b"tairix test peer", "MEDIA"));
        assert_ne!(key, volume_key(b"tairix test peer", "Music"));
        assert_ne!(key, volume_key(b"another server!!", "Media"));
    }

    #[test]
    fn a_session_is_watched_from_its_first_mount_to_its_last() {
        let mut mounts = Mounts::new();
        assert_eq!(mounts.add(mount(1, 1000, 1, "a")), Ok(true));
        assert_eq!(mounts.add(mount(2, 1000, 1, "b")), Ok(false));
        assert_eq!(mounts.add(mount(3, 1001, 2, "a")), Ok(true));
        assert_eq!(
            mounts.add(mount(3, 1001, 2, "c")),
            Err(NoRoom),
            "an offer twice"
        );
        let anchor = ProcId::from_raw([1; 16]);
        assert!(mounts.watches(anchor));
        let first = OfferId::new(1).expect("not zero");
        assert!(mounts.owned(1001, first).is_none(), "another's mount");
        assert!(mounts.owned(1000, first).is_some());
        assert_eq!(mounts.remove(first).map(|(_, last)| last), Some(false));
        assert!(mounts.watches(anchor));
        let second = OfferId::new(2).expect("not zero");
        assert_eq!(mounts.remove(second).map(|(_, last)| last), Some(true));
        assert!(!mounts.watches(anchor));
        assert!(mounts.remove(second).is_none());
    }

    #[test]
    fn a_principal_lists_only_its_own_mounts_in_offer_order() {
        let mut mounts = Mounts::new();
        for (offer, uid, share) in [(9, 1000, "z"), (4, 1001, "y"), (2, 1000, "x")] {
            assert!(mounts.add(mount(offer, uid, 1, share)).is_ok());
        }
        let entries = mounts
            .entries(1000, |mount| {
                if mount.offer.get() == 9 {
                    MountState::Lost
                } else {
                    MountState::Serving
                }
            })
            .expect("room");
        let shares: Vec<(&str, MountState)> = entries
            .iter()
            .map(|entry| (entry.share, entry.state))
            .collect();
        assert_eq!(
            shares,
            [("x", MountState::Serving), ("z", MountState::Lost)],
            "each as its connection stands"
        );
        assert!(mounts.holds(1000, &volume_key(b"tairix test peer", "X")));
        assert!(!mounts.holds(1001, &volume_key(b"tairix test peer", "x")));
        let theirs = mounts.first(|mount| mount.uid == 1001);
        assert_eq!(theirs, OfferId::new(4));
        assert_eq!(mounts.first(|mount| mount.uid == 1002), None);
    }
}
