//! The browse grants a request is admitted against: the store at
//! [`DISCOVERY_POLICY_PATH`](tairix_abi::discovery_policy::DISCOVERY_POLICY_PATH),
//! read once at start and held as the attested identities it names.

use tairix_abi::discovery_policy::read_grants;
use tairix_abi::{AppIdentity, Errno};
use tairix_collections::HashSet;
use tairix_hash::BuildFastHash;
use tairix_net::dnssd::ServiceType;

/// Which application may browse for which service type.
///
/// The store is the administrator's, never shaped by a client, so it is
/// hashed unkeyed.
pub struct Grants {
    entries: HashSet<(AppIdentity, ServiceType), BuildFastHash>,
}

impl Default for Grants {
    fn default() -> Self {
        Self {
            entries: HashSet::with_hasher(BuildFastHash::new()),
        }
    }
}

impl Grants {
    /// The grants `store` records, taken whole or not at all: a line the
    /// codec refuses, a service name outside RFC 6335, or an identity the
    /// kernel could never attest refuses the store, so the service grants
    /// nothing rather than whatever of it parsed.
    ///
    /// # Errors
    ///
    /// The codec's refusal, [`Errno::OutOfRange`] for a grant naming no
    /// attestable identity or no valid service type, and
    /// [`Errno::OutOfMemory`] when the table cannot grow.
    pub fn load(store: &[u8]) -> Result<Self, Errno> {
        let mut grants = Self::default();
        read_grants(store, &mut |grant| {
            let app = AppIdentity::new(grant.bundle_id, grant.publisher)?;
            let service = ServiceType::new(grant.service.name, grant.service.transport)
                .map_err(|_| Errno::OutOfRange)?;
            grants
                .entries
                .try_insert((app, service))
                .map_err(|_| Errno::OutOfMemory)?;
            Ok(())
        })?;
        Ok(grants)
    }

    /// Whether `app` may browse for `service`.
    #[must_use]
    pub fn allows(&self, app: &AppIdentity, service: &ServiceType) -> bool {
        self.entries.contains(&(*app, *service))
    }
}

#[cfg(test)]
mod tests {
    use super::Grants;
    use tairix_abi::{AppIdentity, PublisherId};
    use tairix_net::dnssd::{ServiceType, Transport};

    const PUBLISHER: &str = "0707070707070707070707070707070707070707070707070707070707070707";

    fn app(bundle: &str) -> AppIdentity {
        AppIdentity::new(bundle, PublisherId::from_raw([7; 32])).unwrap()
    }

    fn ipp(name: &[u8]) -> ServiceType {
        ServiceType::new(name, Transport::Tcp).unwrap()
    }

    #[test]
    fn a_grant_admits_its_application_for_its_type_alone() {
        let store = alloc::format!("browse os.example.printing {PUBLISHER} _ipp._tcp\n");
        let grants = Grants::load(store.as_bytes()).unwrap();
        let printing = app("os.example.printing");
        assert!(
            grants.allows(&printing, &ipp(b"IPP")),
            "names compare case-blind"
        );
        assert!(!grants.allows(&printing, &ipp(b"http")));
        assert!(!grants.allows(&app("os.example.other"), &ipp(b"ipp")));
        assert!(!grants.allows(
            &printing,
            &ServiceType::new(b"ipp", Transport::Udp).unwrap()
        ));
    }
}
