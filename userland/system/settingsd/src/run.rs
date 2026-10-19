//! The `Run` binary of the machine-settings service, installed at
//! `/System/Services/settingsd.app/Run` and started by init; an inert stub on
//! the host.
//!
//! It serves `settings-v1` one call at a time on the reserved rendezvous,
//! parked in the kernel's receive until a call arrives.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::string::String;
    use alloc::vec::Vec;

    use tairix_abi::driver_store::SystemConfigFile;
    use tairix_abi::reply::encode_status_reply;
    use tairix_abi::settings_ipc::{SETTINGS_ENDPOINT, SETTINGS_REPLY_MAX, SETTINGS_REQUEST_MAX};
    use tairix_abi::{CapabilityId, Errno};
    use tairix_caps::CapabilitySet;
    use tairix_log::{Event, EventId, Level};
    use tairix_rt::{LogSink, ParentDir};
    use tairix_settingsd::events::{ORIGIN_UNREADABLE, SERVICE_UNAVAILABLE, SETTINGSD_READY};
    use tairix_settingsd::{document_ceiling, DocumentStore, SettingsService};

    /// Calls the rendezvous holds. An administrator's tools ask one at a time;
    /// a fail-closed bound, past which the kernel refuses the post.
    const CAPACITY: usize = 8;

    fn record(id: EventId, level: Level, message: &str) {
        tairix_log::log(
            &LogSink,
            &Event {
                level,
                id,
                message,
                fields: &[],
            },
        );
    }

    /// The two layers of each document, through the VFS under the service's
    /// own identity.
    struct VfsStore;

    impl DocumentStore for VfsStore {
        fn read(&mut self, which: SystemConfigFile) -> Result<Option<String>, Errno> {
            which.read_effective(|path| {
                tairix_rt::read_document(path.as_bytes(), document_ceiling(which))
            })
        }

        fn replace(&mut self, which: SystemConfigFile, text: &str) -> Result<(), Errno> {
            // The image lays out the service's directories; it may not make one.
            tairix_rt::replace_file(
                which.path().as_bytes(),
                text.as_bytes(),
                ParentDir::Existing,
            )
        }
    }

    /// Bind the rendezvous, admitting only a holder of `CAP_SETTINGS_WRITE`.
    fn bind() -> bool {
        let mut send = CapabilitySet::empty();
        send.insert(CapabilityId::SETTINGS_WRITE);
        tairix_rt::call_create(
            SETTINGS_ENDPOINT,
            &send,
            &CapabilitySet::empty(),
            SETTINGS_REQUEST_MAX,
            SETTINGS_REPLY_MAX,
            CAPACITY,
        ) == 0
    }

    /// A zeroed buffer of `len` bytes, or `None` when the heap cannot hold it.
    fn buffer(len: usize) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(len).ok()?;
        bytes.resize(len, 0);
        Some(bytes)
    }

    /// Serve calls until the rendezvous fails, returning the exit code.
    fn serve(service: &mut SettingsService<VfsStore, LogSink>) -> i32 {
        let (Some(mut request), Some(mut reply)) =
            (buffer(SETTINGS_REQUEST_MAX), buffer(SETTINGS_REPLY_MAX))
        else {
            record(
                SERVICE_UNAVAILABLE,
                Level::Warn,
                "settingsd: no memory for a request",
            );
            return 1;
        };
        loop {
            let mut ticket = 0;
            let Ok(len) = tairix_rt::call_recv(SETTINGS_ENDPOINT, &mut request, &mut ticket) else {
                // No receive fails and then succeeds: retrying would spin.
                record(
                    SERVICE_UNAVAILABLE,
                    Level::Warn,
                    "settingsd: the rendezvous stopped receiving",
                );
                return 1;
            };
            let Ok(origin) = tairix_rt::peer_origin(SETTINGS_ENDPOINT, ticket) else {
                record(
                    ORIGIN_UNREADABLE,
                    Level::Warn,
                    "settingsd: the kernel would not attest a caller",
                );
                let refused = encode_status_reply(Err(Errno::PermissionDenied));
                let _ = tairix_rt::call_reply(SETTINGS_ENDPOINT, ticket, &refused);
                continue;
            };
            let reply_len = service.serve(&origin, &request[..len], &mut reply);
            let _ = tairix_rt::call_reply(SETTINGS_ENDPOINT, ticket, &reply[..reply_len]);
        }
    }

    fn main() -> i32 {
        if !bind() {
            record(
                SERVICE_UNAVAILABLE,
                Level::Warn,
                "settingsd: cannot bind the settings rendezvous",
            );
            return 1;
        }
        record(SETTINGSD_READY, Level::Info, "settingsd: serving settings");
        if tairix_rt::servicenotice::announce_ready().is_err() {
            record(
                SERVICE_UNAVAILABLE,
                Level::Warn,
                "settingsd: the service manager did not record this readiness",
            );
        }
        serve(&mut SettingsService::new(VfsStore, LogSink))
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
