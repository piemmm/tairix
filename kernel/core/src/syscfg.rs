//! Boot-time system-configuration loader.
//!
//! The machine's boot-time policy lives in one text document, `system.conf`,
//! whose single definition is the [`tairix_sysconfig`] engine, in two layers:
//! the administrator's on the root volume, and the image's default on the
//! read-only `/System` volume, which stands while the administrator's does
//! not exist. This module is the kernel's reader of the cache-policy portion
//! of that document: as the root is mounted it reads the administrator's
//! layer off the just-mounted volume — the image's, read off the `/System`
//! volume, where there is none — parses it, and applies its caching switches
//! to the live cache-admission control
//! ([`CACHE_CONTROL`](crate::cache_control::CACHE_CONTROL)).
//!
//! It mirrors the `/System/Security` database readers ([`crate::users`],
//! [`crate::groups`]): the same bounded, fail-closed `read_bootstrap_file`
//! under the kernel's capability-less uid-0 bootstrap identity, and the same
//! "audit the outcome, never abort the boot" posture.
//!
//! # Fail-safe, not fail-closed
//!
//! Unlike a security database, an absent or malformed configuration store is
//! **not** an error that stops the system: every SMARTRAM cache is a
//! reclaimable accelerator that is never the source of truth, so the safe
//! fallback is the all-caches-enabled default (today's behaviour). A missing
//! store is the normal fresh-install case; a present-but-malformed store is
//! audited as a rejection and the defaults are applied. The unlock never fails
//! over the cache policy.

use tairix_abi::driver::filesystem::{FilesystemRead, FilesystemSecurity};
use tairix_abi::driver_store::SystemConfigFile;
use tairix_log::{FieldValue, Level, Sink};
use tairix_sysconfig::{CacheClass, SystemConfig, CONFIG_PATH, MAX_CONFIG_LEN};

use crate::audit::{emit, AuditEvent};
use crate::cache_control::CacheControl;
use crate::fs::{read_bootstrap_file, BootstrapFile, BoundedReadError, VfsError};

/// `system.conf` as the boot reads it off the root volume.
pub const SYSTEM_CONFIG: BootstrapFile = BootstrapFile {
    path: CONFIG_PATH,
    max_len: MAX_CONFIG_LEN,
};

/// `system.conf`'s image default as the boot reads it off the read-only
/// `/System` volume.
pub const IMAGE_SYSTEM_CONFIG: BootstrapFile = BootstrapFile {
    path: SystemConfigFile::System.on_volume_path(),
    max_len: MAX_CONFIG_LEN,
};

/// What reading the image's default answered: the document, `None` where the
/// image carries none, or the refusal.
pub type ImageConfig = Result<Option<alloc::vec::Vec<u8>>, BoundedReadError>;

/// Read `system.conf`'s image default off the mounted `/System` volume `fs`.
pub fn read_image_system_config<F>(fs: &mut F) -> ImageConfig
where
    F: FilesystemRead + FilesystemSecurity + ?Sized,
{
    match read_bootstrap_file(fs, IMAGE_SYSTEM_CONFIG) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(BoundedReadError::Vfs(VfsError::NotFound)) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Read `system.conf` off the mounted root `fs` — the image's default
/// (`image`) where the root carries none — parse it, and apply the caching
/// switches to `control` and the pointer-button chatter window to the seat's
/// live control.
///
/// Production passes the process-global
/// [`CACHE_CONTROL`](crate::cache_control::CACHE_CONTROL) — the very control
/// every cache consults — so the policy takes effect on each cache's next
/// operation. Called once from the root unlock, right after the users and
/// groups databases load. The document carries no credential bytes (it is the
/// public configuration document), so the read buffer needs no zeroisation.
///
/// Never fails: with neither layer it applies the [`SystemConfig::default`]
/// (all caches enabled) and audits [`AuditEvent::SystemConfigApplied`] with
/// `source: default`; a layer that cannot be read or parsed applies the same
/// defaults and audits [`AuditEvent::SystemConfigRejected`] — a root layer
/// that cannot be read is never replaced by the image's. A well-formed
/// document is applied and audited [`AuditEvent::SystemConfigApplied`] with
/// `source: store` or `source: image`.
pub fn load_and_apply_system_config<F>(
    fs: &mut F,
    image: ImageConfig,
    control: &CacheControl,
    audit: &dyn Sink,
) where
    F: FilesystemRead + FilesystemSecurity + ?Sized,
{
    let (config, source) = match read_bootstrap_file(fs, SYSTEM_CONFIG) {
        Ok(buf) => parsed(&buf, "store", audit),
        // The administrator's layer is absent: the image's stands, if it has one.
        Err(BoundedReadError::Vfs(VfsError::NotFound)) => match image {
            Ok(Some(buf)) => parsed(&buf, "image", audit),
            Ok(None) => (SystemConfig::default(), "default"),
            Err(err) => {
                audit_rejected(audit, read_error_cause(err));
                (SystemConfig::default(), "default")
            }
        },
        Err(err) => {
            audit_rejected(audit, read_error_cause(err));
            (SystemConfig::default(), "default")
        }
    };

    control.apply(&config);
    // The pointer-button chatter window is seat policy, applied to the same
    // process-global control every seat reads on each button edge.
    crate::seat::CLICK_DEBOUNCE.set_ms(config.input_mouse_debounce_ms);
    audit_applied(audit, &config, source);
}

/// The configuration `buf` holds, read from `source`, or the defaults with
/// the refusal audited.
fn parsed(buf: &[u8], source: &'static str, audit: &dyn Sink) -> (SystemConfig, &'static str) {
    match core::str::from_utf8(buf).map(SystemConfig::parse) {
        Ok(Ok(config)) => (config, source),
        Ok(Err(_)) => {
            audit_rejected(audit, "malformed");
            (SystemConfig::default(), "default")
        }
        Err(_) => {
            audit_rejected(audit, "not_utf8");
            (SystemConfig::default(), "default")
        }
    }
}

/// The stable `cause` string for a bootstrap-read refusal other than a
/// missing file (which is the normal default case, never a rejection).
const fn read_error_cause(err: BoundedReadError) -> &'static str {
    match err {
        BoundedReadError::Vfs(_) => "read_error",
        BoundedReadError::NotAFile => "not_a_file",
        BoundedReadError::TooLarge => "too_large",
        BoundedReadError::ShortRead => "short_read",
        BoundedReadError::OutOfMemory => "out_of_memory",
    }
}

/// Audit the applied cache policy: the store `source` and the effective mode
/// of each cache class (the master `cache.all` ceiling already folded in).
fn audit_applied(audit: &dyn Sink, config: &SystemConfig, source: &'static str) {
    emit(
        audit,
        Level::Info,
        AuditEvent::SystemConfigApplied,
        &[
            tairix_log::Field {
                key: "source",
                value: FieldValue::Str(source),
            },
            class_field("cache.filesystem", config, CacheClass::Filesystem),
            class_field("cache.block", config, CacheClass::Block),
            class_field("cache.transform", config, CacheClass::Transform),
            class_field("cache.semantic", config, CacheClass::Semantic),
        ],
    );
}

/// One audit field naming a cache class's effective mode (`auto` / `off`).
fn class_field<'a>(
    key: &'a str,
    config: &SystemConfig,
    class: CacheClass,
) -> tairix_log::Field<'a> {
    tairix_log::Field {
        key,
        value: FieldValue::Str(config.effective_cache(class).as_str()),
    }
}

/// Audit a store that was present but could not be read or parsed; the
/// defaults are applied instead (fail-safe).
fn audit_rejected(audit: &dyn Sink, cause: &'static str) {
    emit(
        audit,
        Level::Warn,
        AuditEvent::SystemConfigRejected,
        &[
            tairix_log::Field {
                key: "path",
                value: FieldValue::Str(CONFIG_PATH),
            },
            tairix_log::Field {
                key: "cause",
                value: FieldValue::Str(cause),
            },
        ],
    );
}

#[cfg(test)]
#[path = "syscfg_tests.rs"]
mod tests;
