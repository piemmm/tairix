//! TAIRiX `settingsd` — the machine-settings service (`settings-v1`).
//!
//! The one writer of the administrator's layer of the machine's two settings
//! documents ([`SystemConfigFile`]): the machine configuration and the
//! per-interface network configuration. An administrator's program, run
//! elevated, asks it to read a document or to change one; the kernel admits
//! only a caller holding `CAP_SETTINGS_WRITE`.
//!
//! A change is made to the document as it stands — the administrator's layer
//! where one exists, else the image's — through the document's own engine, all
//! or none, and replaces the administrator's layer whole and at once. Requests
//! are served one at a time, so two programs changing one document at once
//! lose neither change.
//!
//! This crate is the host-testable engine over an injected [`DocumentStore`];
//! the `Run` binary wires the VFS.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod events;

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver_store::SystemConfigFile;
use tairix_abi::origin::Origin;
use tairix_abi::reply::{encode_status_reply, STATUS_REPLY_LEN};
use tairix_abi::settings_ipc::{encode_reply, Settings, SettingsRequest, DOCUMENT_MAX};
use tairix_abi::{CapabilityId, Errno};
use tairix_log::field::FieldValue;
use tairix_log::{Event, EventId, Field, Level, Sink};
use tairix_netconfig::{ConfigError as NetworkError, NetworkConfig};
use tairix_sysconfig::{Key, SystemConfig};

/// Both layers of each document, as the service reaches them.
pub trait DocumentStore {
    /// The document from the first of its layers that exists, or `None` when
    /// neither does.
    ///
    /// # Errors
    ///
    /// A layer that exists but cannot be read, or holds more than
    /// [`document_ceiling`] bytes or text that is not UTF-8.
    fn read(&mut self, which: SystemConfigFile) -> Result<Option<String>, Errno>;

    /// Replace the administrator's layer of `which` with `text`, whole: a
    /// crash leaves the old document or the new.
    ///
    /// # Errors
    ///
    /// The volume's refusal, which leaves the layer as it was.
    fn replace(&mut self, which: SystemConfigFile, text: &str) -> Result<(), Errno>;
}

/// The longest `which` its own engine reads.
#[must_use]
pub const fn document_ceiling(which: SystemConfigFile) -> usize {
    match which {
        SystemConfigFile::System => tairix_sysconfig::MAX_CONFIG_LEN,
        SystemConfigFile::Network => tairix_netconfig::MAX_CONFIG_LEN,
    }
}

/// The service: the documents, and the sink its decisions are recorded in.
pub struct SettingsService<S, L> {
    store: S,
    log: L,
}

impl<S: DocumentStore, L: Sink> SettingsService<S, L> {
    /// A service over `store`, recording to `log`.
    pub const fn new(store: S, log: L) -> Self {
        Self { store, log }
    }

    /// Answer `request` from `caller` into `reply`, returning the reply's
    /// length: the document as it stands after the request, or the refusal.
    /// `reply` holds
    /// [`SETTINGS_REPLY_MAX`](tairix_abi::settings_ipc::SETTINGS_REPLY_MAX)
    /// bytes.
    pub fn serve(&mut self, caller: &Origin, request: &[u8], reply: &mut [u8]) -> usize {
        // The kernel admits only a holder; checked again so a rendezvous
        // bound without the gate fails closed rather than open.
        let decoded = if caller
            .capabilities()
            .holds_cap(CapabilityId::SETTINGS_WRITE)
        {
            SettingsRequest::decode(request)
        } else {
            Err(Errno::PermissionDenied)
        };
        let answer = match decoded {
            Ok(SettingsRequest::Read { document }) => {
                let answer = self.read(document);
                self.record(
                    caller,
                    document,
                    &answer,
                    (events::READ_ANSWERED, events::READ_REFUSED),
                    0,
                );
                answer
            }
            Ok(SettingsRequest::Set { document, settings }) => {
                let answer = self.set(document, settings);
                self.record(
                    caller,
                    document,
                    &answer,
                    (events::CHANGE_APPLIED, events::CHANGE_REFUSED),
                    settings.iter().len(),
                );
                answer
            }
            Err(err) => {
                refused(&self.log, caller, err);
                Err(err)
            }
        };
        let answer = answer.as_deref().map_err(|err| *err);
        encode_reply(answer, reply).unwrap_or_else(|err| status(reply, Err(err)))
    }

    /// `which` as it stands; empty where neither layer exists.
    fn read(&mut self, which: SystemConfigFile) -> Result<String, Errno> {
        let document = self.store.read(which)?.unwrap_or_default();
        if document.len() > DOCUMENT_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(document)
    }

    /// Set each of `settings` in `which`, all or none, answering the document
    /// written.
    fn set(&mut self, which: SystemConfigFile, settings: Settings<'_>) -> Result<String, Errno> {
        refuse_repeats(settings)?;
        let current = self.store.read(which)?;
        let document = match which {
            SystemConfigFile::System => changed_machine(current.as_deref(), settings)?,
            SystemConfigFile::Network => changed_network(current.as_deref(), settings)?,
        };
        self.store.replace(which, &document)?;
        Ok(document)
    }

    /// Record a decision about `which` for `caller`: `ids` are the allowed
    /// and the refused ids.
    fn record(
        &self,
        caller: &Origin,
        which: SystemConfigFile,
        answer: &Result<String, Errno>,
        (allowed, refused): (EventId, EventId),
        settings: usize,
    ) {
        let (id, level, message, errno) = match answer {
            Ok(_) => (allowed, Level::Info, "settingsd: answered", 0),
            Err(err) => (refused, Level::Warn, "settingsd: refused", err.as_i32()),
        };
        tairix_log::log(
            &self.log,
            &Event {
                level,
                id,
                message,
                fields: &[
                    Field {
                        key: "uid",
                        value: FieldValue::UnsignedInt(u64::from(caller.uid())),
                    },
                    Field {
                        key: "document",
                        value: FieldValue::Str(which.path()),
                    },
                    Field {
                        key: "settings",
                        value: FieldValue::UnsignedInt(u64::try_from(settings).unwrap_or(u64::MAX)),
                    },
                    Field {
                        key: "errno",
                        value: FieldValue::SignedInt(i64::from(errno)),
                    },
                ],
            },
        );
    }
}

/// Refuse a change that names one setting twice: there is no order in which
/// both are honoured.
fn refuse_repeats(settings: Settings<'_>) -> Result<(), Errno> {
    let mut names = Vec::new();
    names
        .try_reserve_exact(settings.iter().len())
        .map_err(|_| Errno::OutOfMemory)?;
    names.extend(settings.iter().map(|(name, _)| name));
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Errno::AlreadyExists);
    }
    Ok(())
}

/// The machine configuration `current` (the defaults where there is none)
/// with `settings` set, rendered.
fn changed_machine(current: Option<&str>, settings: Settings<'_>) -> Result<String, Errno> {
    let mut config = match current {
        Some(text) => SystemConfig::parse(text).map_err(|_| Errno::BadMagic)?,
        None => SystemConfig::default(),
    };
    for (name, value) in settings {
        let key = Key::from_name(name).ok_or(Errno::NotFound)?;
        config.set(key, value).map_err(|_| Errno::OutOfRange)?;
    }
    let document = config.render();
    if document.len() > tairix_sysconfig::MAX_CONFIG_LEN {
        return Err(Errno::LengthOutOfRange);
    }
    Ok(document)
}

/// The network configuration `current` (none where there is none) with
/// `settings` set, rendered; the empty value clears a setting.
fn changed_network(current: Option<&str>, settings: Settings<'_>) -> Result<String, Errno> {
    let config = match current {
        Some(text) => NetworkConfig::parse(text).map_err(|_| Errno::BadMagic)?,
        None => NetworkConfig::default(),
    };
    let mut draft = config.edit();
    for (name, value) in settings {
        let (iface, key) = tairix_netconfig::resolve_name(name).map_err(|_| Errno::NotFound)?;
        draft.apply(iface, key, value).map_err(network_errno)?;
    }
    Ok(draft.commit().map_err(network_errno)?.render())
}

/// The refusal a network edit is answered with.
const fn network_errno(err: NetworkError) -> Errno {
    match err {
        NetworkError::TooManyInterfaces => Errno::LimitExceeded,
        NetworkError::TooLong => Errno::LengthOutOfRange,
        NetworkError::InvalidInterfaceName | NetworkError::UnknownKey => Errno::NotFound,
        NetworkError::TooManyMembers
        | NetworkError::TooManyDnsServers
        | NetworkError::MissingValue
        | NetworkError::InvalidValue
        | NetworkError::DuplicateKey
        | NetworkError::InconsistentInterface => Errno::OutOfRange,
    }
}

/// Record a request refused before it named a document.
fn refused<L: Sink>(log: &L, caller: &Origin, err: Errno) {
    tairix_log::log(
        log,
        &Event {
            level: Level::Warn,
            id: events::REQUEST_REFUSED,
            message: "settingsd: refused a request",
            fields: &[
                Field {
                    key: "uid",
                    value: FieldValue::UnsignedInt(u64::from(caller.uid())),
                },
                Field {
                    key: "errno",
                    value: FieldValue::SignedInt(i64::from(err.as_i32())),
                },
            ],
        },
    );
}

/// Write the status frame for `outcome` into `reply`, returning its length.
fn status(reply: &mut [u8], outcome: Result<(), Errno>) -> usize {
    match reply.get_mut(..STATUS_REPLY_LEN) {
        Some(frame) => {
            frame.copy_from_slice(&encode_status_reply(outcome));
            STATUS_REPLY_LEN
        }
        None => 0,
    }
}

#[cfg(test)]
mod tests;
