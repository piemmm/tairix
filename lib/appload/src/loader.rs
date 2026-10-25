//! The [`AppLoader`] pipeline: validate a bundle's layout, verify its signed
//! manifest and contents, compute its capability ceiling, and resolve its
//! shared-library references under the policy.
//!
//! This is the one place a bundle is judged. The pipeline **fails closed**: the first deviation — a stray top-level entry, a
//! manifest that will not decode, a wrong interface hash, a bad signature, a
//! content-hash mismatch — refuses the whole bundle and nothing is launched.

use alloc::string::String;

use tairix_abi::{
    decode_capability_ids, is_user_saver_bundle, resolve_library as resolve_library_policy,
    validate_bundle_layout, validate_saver_options, AppInfoHeader, CapabilityId, Duration64, Errno,
    LibraryScope, LoadImage, ProgramKind, PublisherBinding, PublisherId, APPINFO_FLAGS_SAVER,
    APPINFO_FLAGS_SAVER_OWN, APPINFO_MAX_CAPABILITIES, SYSCALL_TABLE_HASH_LEN,
};
use tairix_caps::CapabilitySet;
use tairix_crypto::{sha256, Ed25519PublicKey, Ed25519Signature};
use tairix_log::{log, Event, EventId, Field, Level, Sink};
use tairix_path::join;

use crate::bundle::{BundleIdentity, BundleStore, Clock, LoadedApp, ResolvedLibrary, Verifier};
use crate::error::AppError;
use crate::events;

/// Why a manifest's capability request could not be read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RequestRefused {
    /// The body is shorter than the header says it is.
    Truncated,
    /// The header's counts overflow a body length.
    Overflow(Errno),
    /// An identifier names no capability.
    Invalid(Errno),
}

impl RequestRefused {
    /// The error a caller reports it as.
    #[must_use]
    pub const fn errno(self) -> Errno {
        match self {
            Self::Truncated => Errno::BufferTooSmall,
            Self::Overflow(err) | Self::Invalid(err) => err,
        }
    }
}

/// The capabilities the manifest `bytes`, headed by `header`, requests: the
/// one reading of a manifest's request, shared by the load gate and by an
/// authority deciding who may allow a program.
///
/// # Errors
///
/// [`RequestRefused`] naming what made the body unreadable.
pub fn requested_capabilities(
    bytes: &[u8],
    header: &AppInfoHeader,
) -> Result<CapabilitySet, RequestRefused> {
    let body = bytes
        .get(AppInfoHeader::WIRE_LEN..)
        .ok_or(RequestRefused::Truncated)?;
    let expected = header.body_len().map_err(RequestRefused::Overflow)?;
    if body.len() < expected {
        return Err(RequestRefused::Truncated);
    }
    let mut scratch = [CapabilityId::FS_MOUNT; APPINFO_MAX_CAPABILITIES as usize];
    let decoded = decode_capability_ids(body, usize::from(header.capability_count), &mut scratch)
        .map_err(RequestRefused::Invalid)?;
    let mut set = CapabilitySet::empty();
    for cap in &scratch[..decoded] {
        set.insert(*cap);
    }
    Ok(set)
}

/// The developer identity a manifest's publisher key digests to.
///
/// One definition, because it is what per-app stored state is *owned by*: two
/// hosts deriving it differently would key the same developer's data
/// differently, so the load gate, the kernel's attestation, and any consumer
/// matching an attested identity against an on-disk bundle all call this.
///
/// It is a derivation, never a judgement: the caller decides whether the key
/// it hashes was authenticated. [`AppLoader`] calls it only after the
/// manifest's signature and publisher binding verify, which is what makes the
/// identity it attests trustworthy; a consumer reading an unverified manifest
/// gets the identity that manifest *claims*.
#[must_use]
pub fn publisher_id_of(header: &AppInfoHeader) -> PublisherId {
    PublisherId::from_raw(sha256(&header.publisher_id_preimage()))
}

/// Construction-time configuration for an [`AppLoader`].
///
/// All seams are borrowed for the loader's lifetime, mirroring `init`'s
/// host configuration: one loader per service process, alive for the run.
pub struct AppLoaderConfig<'a> {
    /// ABI version the loader accepts in an `AppInfo` manifest. A manifest
    /// targeting a different version is refused.
    pub accepted_abi_version: u32,
    /// The kernel's compiled-in syscall-table hash. A manifest whose
    /// declared hash does not match is refused.
    pub syscall_table_hash: [u8; SYSCALL_TABLE_HASH_LEN],
    /// Seam that reads a bundle off storage.
    pub store: &'a dyn BundleStore,
    /// Seam that verifies a manifest signature.
    pub verifier: &'a dyn Verifier,
    /// Monotonic clock, read only to time the load-from-store and
    /// verification phases for the [`events::APP_LOADED`] audit record. No
    /// load decision depends on it.
    pub clock: &'a dyn Clock,
    /// Structured audit log sink.
    pub sink: &'a dyn Sink,
}

/// How the process a bundle is loaded for is spawned, which a screensaver's
/// admission turns on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LoadMode {
    /// A spawn that hands the child no shared region: an ordinary one, or a
    /// parser sandbox.
    Ordinary,
    /// A drawing sandbox: a sandbox handed the one region it presents through.
    Drawing,
}

/// The application-bundle loader (Stage 6).
pub struct AppLoader<'a> {
    cfg: AppLoaderConfig<'a>,
}

impl<'a> AppLoader<'a> {
    /// Create a loader over the given configuration.
    #[must_use]
    pub fn new(cfg: AppLoaderConfig<'a>) -> Self {
        Self { cfg }
    }

    /// Load and authorise the bundle rooted at `bundle`, returning the
    /// capability ceiling and entry point a caller may spawn it with.
    ///
    /// `user_grants` is the launching user's capability set; the granted
    /// ceiling is the manifest request intersected with it. The loader never widens a request.
    ///
    /// `mode` is how the child is spawned. A screensaver loads only into a
    /// [`LoadMode::Drawing`] spawn, and nothing else does. A bundle in a
    /// user's own saver store is
    /// verified against its own signer key rather than the configured
    /// [`Verifier`]'s anchor, so it must be a screensaver loading into a
    /// drawing spawn and declare no privileged feed — it is admitted with
    /// nothing a signature could have granted.
    ///
    /// # Errors
    ///
    /// Returns the first [`AppError`] encountered: [`AppError::Store`] for an
    /// unreadable bundle, [`AppError::Layout`] for a layout deviation,
    /// [`AppError::Manifest`] for a bad manifest, [`AppError::InterfaceHashMismatch`]
    /// for a syscall-hash mismatch, [`AppError::Signature`] for a failed
    /// signature, [`AppError::PublisherCert`] for a publisher the bundle
    /// cannot prove it belongs to, [`AppError::Misplaced`] for a bundle
    /// launched from a store its signed kind does not run from,
    /// [`AppError::DrawingOnly`] for a screensaver outside a drawing spawn or
    /// another kind inside one,
    /// [`AppError::PrivilegedFeed`] for a user's saver declaring a privileged
    /// feed, or [`AppError::ContentHashMismatch`] for tampered contents.
    pub fn load(
        &self,
        bundle: &str,
        user_grants: &CapabilitySet,
        mode: LoadMode,
    ) -> Result<LoadedApp, AppError> {
        let start_ns = self.cfg.clock.now_ns();
        let mut load_ns = 0u64;
        let names = self
            .timed(&mut load_ns, || self.cfg.store.entries(bundle))
            .map_err(|e| self.store_error(bundle, e))?;
        let refs: alloc::vec::Vec<&str> = names.iter().map(String::as_str).collect();
        if let Err(e) = validate_bundle_layout(&refs) {
            self.audit(
                events::APP_LAYOUT_REJECTED,
                Level::Warn,
                bundle,
                "layout invalid",
            );
            return Err(AppError::Layout(e));
        }

        let bytes = self
            .timed(&mut load_ns, || self.cfg.store.read_appinfo(bundle))
            .map_err(|e| self.store_error(bundle, e))?;
        let header = match AppInfoHeader::from_bytes(&bytes) {
            Ok(header) => header,
            Err(e) => {
                self.audit(
                    events::APP_MANIFEST_INVALID,
                    Level::Warn,
                    bundle,
                    "manifest undecodable",
                );
                return Err(AppError::Manifest(e));
            }
        };
        if header.abi_version != self.cfg.accepted_abi_version {
            self.audit(
                events::APP_MANIFEST_INVALID,
                Level::Warn,
                bundle,
                "abi version unsupported",
            );
            return Err(AppError::Manifest(Errno::AbiVersionUnsupported));
        }

        if ct_ne(&header.syscall_table_hash, &self.cfg.syscall_table_hash) {
            self.audit(
                events::APP_INTERFACE_MISMATCH,
                Level::Warn,
                bundle,
                "syscall interface hash mismatch",
            );
            return Err(AppError::InterfaceHashMismatch);
        }

        let own_signer = is_user_saver_bundle(bundle);
        self.verify_manifest_signature(bundle, &bytes, &header, own_signer)?;
        let publisher = self.verify_publisher(bundle, &header)?;
        let kind = self.admit_kind(bundle, &header, mode, own_signer)?;
        // Checked before the bundle is read and hashed, so a manifest that is
        // refused anyway costs no pass over its contents.
        if let Err(e) = validate_saver_options(&bytes[AppInfoHeader::WIRE_LEN..], &header) {
            self.audit(
                events::APP_MANIFEST_INVALID,
                Level::Warn,
                bundle,
                "screensaver options invalid",
            );
            return Err(AppError::Manifest(e));
        }

        // One pass reads and hashes the bundle contents and hands back the
        // entry-point `Run` image read during that same walk, so `Run` is
        // read from disk once rather than twice.
        let contents = self
            .timed(&mut load_ns, || self.cfg.store.contents(bundle))
            .map_err(|e| self.store_error(bundle, e))?;
        let read_bytes = contents.read_bytes;
        if ct_ne(&contents.content_hash, &header.content_hash) {
            self.audit(
                events::APP_CONTENT_MISMATCH,
                Level::Warn,
                bundle,
                "content hash mismatch",
            );
            return Err(AppError::ContentHashMismatch);
        }

        let requested = self.requested_capabilities(&bytes, &header, bundle)?;
        let granted = requested.intersection(user_grants);

        // The `Run` bytes are already authenticated: they were part of the
        // content the signed hash just matched.
        let (run_image, libraries) = self.validate_run_image(bundle, contents.run_image)?;

        let run_path = join(bundle, "Run");
        // The two phases split the whole load: time spent inside the store
        // seam (reading the bundle off disk) is the load cost; everything
        // else since `start_ns` (layout, manifest, interface-hash, signature,
        // content-hash, and entry-point verification) is the verify cost.
        let total_ns = self.cfg.clock.now_ns().saturating_sub(start_ns);
        let verify_ns = total_ns.saturating_sub(load_ns);
        self.audit_loaded(
            (bundle, header.bundle_name(), own_signer),
            load_ns,
            verify_ns,
            read_bytes,
        );
        Ok(LoadedApp::new(
            BundleIdentity {
                id: header.bundle_id().into(),
                name: header.bundle_name().into(),
                version: header.bundle_version().into(),
                publisher,
                kind,
            },
            run_path,
            run_image,
            granted,
            libraries,
        ))
    }

    /// Resolve a shared-library `reference` for the bundle rooted at
    /// `bundle` under the dynamic-loader policy.
    ///
    /// A reference is accepted only if it lies inside the bundle's own
    /// `Libraries/` directory or `/System/Libraries/`.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Library`] if the reference points anywhere else or
    /// contains a `..` component.
    pub fn resolve_library(&self, bundle: &str, reference: &str) -> Result<LibraryScope, AppError> {
        let dir = join(bundle, "Libraries");
        match resolve_library_policy(reference, &dir) {
            Ok(scope) => {
                self.audit(events::LIBRARY_RESOLVED, Level::Info, bundle, reference);
                Ok(scope)
            }
            Err(e) => {
                self.audit(events::LIBRARY_REFUSED, Level::Warn, bundle, reference);
                Err(AppError::Library(e))
            }
        }
    }

    /// Validate the entry-point `Run` binary (already read once during the
    /// content-hash walk) and resolve the shared libraries it declares it
    /// needs, returning the validated bytes so the caller spawns exactly what
    /// was checked (no re-read).
    ///
    /// `run_bytes` were read and authenticated as part of the signed content
    /// hash, so this method performs no further disk I/O.
    ///
    /// `LoadImage::parse` enforces the hardening invariants (PIE, W^X,
    /// and the syscall-hash CFI tag) on the binary; a malformed image or a
    /// CFI-tag mismatch is refused (`AppError::RunImage`).
    ///
    /// # Errors
    ///
    /// [`AppError::RunImage`] if the bytes are not a valid `rxe` image, or
    /// [`AppError::Library`] if a needed library violates the policy.
    fn validate_run_image(
        &self,
        bundle: &str,
        run_bytes: alloc::vec::Vec<u8>,
    ) -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<ResolvedLibrary>), AppError> {
        let image = LoadImage::parse(&run_bytes, &self.cfg.syscall_table_hash).map_err(|e| {
            self.audit(
                events::APP_RUN_IMAGE_INVALID,
                Level::Warn,
                bundle,
                "run image rejected",
            );
            AppError::RunImage(e)
        })?;
        let libraries = self.resolve_needed_libraries(bundle, &image)?;
        Ok((run_bytes, libraries))
    }

    /// Resolve every shared library the entry-point `image` declares it needs
    /// against the dynamic-loader policy, in declaration order.
    ///
    /// This is where the C-ABI runtime (the curated `tairix_sys_*` /
    /// `/System/Libraries/` *System runtime / C ABI* library) and any bundle
    /// `Libraries/` reference is bound; an out-of-tree reference fails closed.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Library`] on the first reference that points
    /// outside the bundle's `Libraries/` or `/System/Libraries/`.
    fn resolve_needed_libraries(
        &self,
        bundle: &str,
        image: &LoadImage,
    ) -> Result<alloc::vec::Vec<ResolvedLibrary>, AppError> {
        let mut resolved = alloc::vec::Vec::new();
        for reference in image.needed_libraries() {
            let scope = self.resolve_library(bundle, reference)?;
            resolved.push(ResolvedLibrary {
                reference: reference.into(),
                scope,
            });
        }
        Ok(resolved)
    }

    /// Decode the manifest's capability-request body into a [`CapabilitySet`],
    /// auditing what made it unreadable.
    fn requested_capabilities(
        &self,
        bytes: &[u8],
        header: &AppInfoHeader,
        bundle: &str,
    ) -> Result<CapabilitySet, AppError> {
        requested_capabilities(bytes, header).map_err(|refused| {
            let why = match refused {
                RequestRefused::Truncated => "manifest body truncated",
                RequestRefused::Overflow(_) => "manifest body length overflow",
                RequestRefused::Invalid(_) => "capability body invalid",
            };
            self.audit(events::APP_MANIFEST_INVALID, Level::Warn, bundle, why);
            AppError::Manifest(refused.errno())
        })
    }

    /// The signed kind of `bundle`'s manifest `header`, admitted to run from
    /// where the bundle is and in `mode`: a screensaver only in a drawing
    /// spawn, and a user's own, self-signed one only when it declares no
    /// privileged feed.
    fn admit_kind(
        &self,
        bundle: &str,
        header: &AppInfoHeader,
        mode: LoadMode,
        own_signer: bool,
    ) -> Result<ProgramKind, AppError> {
        let Some(kind) = header
            .program_kind()
            .filter(|kind| kind.launchable_from(bundle))
        else {
            self.audit(
                events::APP_MISPLACED,
                Level::Warn,
                bundle,
                "signed kind does not run from this store",
            );
            return Err(AppError::Misplaced);
        };
        if !runs_in(kind, mode) {
            self.audit(
                events::APP_DRAWING_ONLY,
                Level::Warn,
                bundle,
                drawing_refusal(kind),
            );
            return Err(AppError::DrawingOnly);
        }
        if own_signer && header.flags & APPINFO_FLAGS_SAVER & !APPINFO_FLAGS_SAVER_OWN != 0 {
            self.audit(
                events::APP_FEED_REFUSED,
                Level::Warn,
                bundle,
                "own screensaver declares a privileged feed",
            );
            return Err(AppError::PrivilegedFeed);
        }
        Ok(kind)
    }

    /// Verify the manifest's Ed25519 signature over the whole manifest
    /// except the signature field: the header prefix concatenated with the
    /// capability/MIME body, so a swapped capability id in the body breaks
    /// it rather than hiding behind a header-only signature.
    ///
    /// `own_signer` verifies against the key the manifest names instead of
    /// the configured [`Verifier`]: proof the bundle is whole, which the
    /// caller must not take for proof of anything else.
    fn verify_manifest_signature(
        &self,
        bundle: &str,
        bytes: &[u8],
        header: &AppInfoHeader,
        own_signer: bool,
    ) -> Result<(), AppError> {
        let mut signed = alloc::vec::Vec::with_capacity(
            AppInfoHeader::signed_range().end + (bytes.len() - AppInfoHeader::WIRE_LEN),
        );
        signed.extend_from_slice(&bytes[AppInfoHeader::signed_range()]);
        signed.extend_from_slice(&bytes[AppInfoHeader::WIRE_LEN..]);
        let verified = if own_signer {
            Ed25519PublicKey::from_bytes(&header.signer_pubkey).is_ok_and(|key| {
                key.verify(&signed, &Ed25519Signature(header.signature))
                    .is_ok()
            })
        } else {
            self.cfg
                .verifier
                .verify(&signed, &header.signature, &header.signer_pubkey)
                .is_ok()
        };
        if !verified {
            self.audit(
                events::APP_SIGNATURE_INVALID,
                Level::Warn,
                bundle,
                "signature did not verify",
            );
            return Err(AppError::Signature);
        }
        Ok(())
    }

    /// Resolve the bundle's stable developer identity, verifying the
    /// publisher's delegation of the build signing key first.
    ///
    /// The fourth authenticity check, beside the manifest signature, the
    /// content hash, and the syscall-interface hash. A self-published
    /// manifest needs no further signature — the trust root that admitted
    /// the signer admitted the publisher, because they are one key. A
    /// delegated manifest must carry the publisher's signature over
    /// [`AppInfoHeader::publisher_cert_message`], which names both the
    /// bundle identifier and the signing key, so a certificate cannot be
    /// lifted onto another bundle or another key.
    ///
    /// The publisher key is deliberately **not** trust-rooted: it is a claim
    /// the certificate makes unforgeable, not an authority. What makes it
    /// meaningful is the per-app store pinning the identity it first saw, so
    /// a later build must present a chain back to the same publisher.
    fn verify_publisher(
        &self,
        bundle: &str,
        header: &AppInfoHeader,
    ) -> Result<PublisherId, AppError> {
        let binding = header.publisher_binding().map_err(|e| {
            self.audit(
                events::APP_PUBLISHER_INVALID,
                Level::Warn,
                bundle,
                "publisher binding malformed",
            );
            AppError::Manifest(e)
        })?;
        if binding == PublisherBinding::Delegated {
            let refuse = || {
                self.audit(
                    events::APP_PUBLISHER_INVALID,
                    Level::Warn,
                    bundle,
                    "publisher certificate did not verify",
                );
                AppError::PublisherCert
            };
            let key =
                Ed25519PublicKey::from_bytes(&header.publisher_pubkey).map_err(|_| refuse())?;
            key.verify(
                &header.publisher_cert_message(),
                &Ed25519Signature(header.publisher_cert),
            )
            .map_err(|_| refuse())?;
        }
        Ok(publisher_id_of(header))
    }

    fn store_error(&self, bundle: &str, err: Errno) -> AppError {
        log(
            self.cfg.sink,
            &Event {
                level: Level::Warn,
                id: events::APP_STORE_ERROR,
                message: event_message(events::APP_STORE_ERROR),
                fields: &[
                    Field {
                        key: "bundle",
                        value: tairix_log::FieldValue::Str(bundle),
                    },
                    Field {
                        key: "errno",
                        value: tairix_log::FieldValue::SignedInt(i64::from(err.as_i32())),
                    },
                ],
            },
        );
        AppError::Store(err)
    }

    /// Run `f`, adding the monotonic time it took to `acc`. Used to
    /// accumulate the time spent inside the store seam (reading the bundle
    /// off disk) so the [`events::APP_LOADED`] record can report it. A clock
    /// that fails to advance contributes a zero-length span, never a
    /// wrap-around, thanks to the saturating subtraction.
    fn timed<T>(&self, acc: &mut u64, f: impl FnOnce() -> T) -> T {
        let start = self.cfg.clock.now_ns();
        let out = f();
        *acc = acc.saturating_add(self.cfg.clock.now_ns().saturating_sub(start));
        out
    }

    /// Emit the [`events::APP_LOADED`] record for an accepted bundle,
    /// carrying how long the load-from-store and verification phases took so
    /// a slow first launch (a large bundle read off disk, or an expensive
    /// signature/content-hash check) is diagnosable from the audit log.
    ///
    /// `read_bytes` is what the load span moved, so the record states a
    /// throughput rather than a bare duration — a slow store and a large
    /// bundle look identical without it. The signer says whose key admitted
    /// it: the system's, or a user's own saver's own.
    fn audit_loaded(
        &self,
        (bundle, name, own_signer): (&str, &str, bool),
        load_ns: u64,
        verify_ns: u64,
        read_bytes: usize,
    ) {
        log(
            self.cfg.sink,
            &Event {
                level: Level::Info,
                id: events::APP_LOADED,
                message: event_message(events::APP_LOADED),
                fields: &[
                    Field {
                        key: "bundle",
                        value: tairix_log::FieldValue::Str(bundle),
                    },
                    Field {
                        key: "detail",
                        value: tairix_log::FieldValue::Str(name),
                    },
                    Field {
                        key: "signer",
                        value: tairix_log::FieldValue::Str(if own_signer {
                            "own"
                        } else {
                            "system"
                        }),
                    },
                    Field {
                        key: "load",
                        value: tairix_log::FieldValue::Duration(Duration64::from_nanos(load_ns)),
                    },
                    Field {
                        key: "verify",
                        value: tairix_log::FieldValue::Duration(Duration64::from_nanos(verify_ns)),
                    },
                    Field {
                        key: "read_bytes",
                        value: tairix_log::FieldValue::UnsignedInt(read_bytes as u64),
                    },
                ],
            },
        );
    }

    fn audit(&self, id: EventId, level: Level, bundle: &str, detail: &str) {
        audit_to(self.cfg.sink, id, level, bundle, detail);
    }
}

/// Which way a bundle of `kind` broke the drawing sandbox's one rule.
const fn drawing_refusal(kind: ProgramKind) -> &'static str {
    if matches!(kind, ProgramKind::Screensaver) {
        "a screensaver outside the drawing sandbox"
    } else {
        "another kind inside the drawing sandbox"
    }
}

/// Whether a program of `kind` may run in a spawn of `mode`: a screensaver
/// in a drawing spawn alone, and a drawing spawn runs nothing else.
fn runs_in(kind: ProgramKind, mode: LoadMode) -> bool {
    (kind == ProgramKind::Screensaver) == (mode == LoadMode::Drawing)
}

/// Hold a bundle a launch cache serves without the gate to the one rule that
/// turns on the spawn rather than on the bundle: a screensaver loads only into
/// a [`LoadMode::Drawing`] spawn, and nothing else does. Audited as the gate's
/// own refusal is.
///
/// # Errors
///
/// [`AppError::DrawingOnly`] for a screensaver outside a drawing spawn or
/// another kind inside one.
pub fn admit_cached(app: &LoadedApp, mode: LoadMode, sink: &dyn Sink) -> Result<(), AppError> {
    if runs_in(app.kind(), mode) {
        return Ok(());
    }
    let run = app.run_path();
    audit_to(
        sink,
        events::APP_DRAWING_ONLY,
        Level::Warn,
        run.strip_suffix("/Run").unwrap_or(run),
        drawing_refusal(app.kind()),
    );
    Err(AppError::DrawingOnly)
}

fn audit_to(sink: &dyn Sink, id: EventId, level: Level, bundle: &str, detail: &str) {
    log(
        sink,
        &Event {
            level,
            id,
            message: event_message(id),
            fields: &[
                Field {
                    key: "bundle",
                    value: tairix_log::FieldValue::Str(bundle),
                },
                Field {
                    key: "detail",
                    value: tairix_log::FieldValue::Str(detail),
                },
            ],
        },
    );
}

/// Constant-time inequality over two 32-byte digests, so a hostile bundle
/// cannot probe the kernel's hashes byte-by-byte via timing.
fn ct_ne(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        != 0
}

fn event_message(id: EventId) -> &'static str {
    match id {
        events::APP_LOADED => "application bundle loaded",
        events::APP_LAYOUT_REJECTED => "application bundle layout rejected",
        events::APP_MANIFEST_INVALID => "application manifest invalid",
        events::APP_INTERFACE_MISMATCH => "application syscall interface hash mismatch",
        events::APP_SIGNATURE_INVALID => "application signature invalid",
        events::APP_CONTENT_MISMATCH => "application content hash mismatch",
        events::APP_STORE_ERROR => "application bundle store error",
        events::LIBRARY_RESOLVED => "shared library resolved",
        events::LIBRARY_REFUSED => "shared library refused",
        events::APP_RUN_IMAGE_INVALID => "application run image invalid",
        events::APP_PUBLISHER_INVALID => "application publisher unattributable",
        events::APP_MISPLACED => "application launched from the wrong store",
        events::APP_DRAWING_ONLY => {
            "screensaver refused outside the drawing sandbox, or another kind inside it"
        }
        events::APP_FEED_REFUSED => "own screensaver refused a system feed",
        _ => "load gate event",
    }
}

#[cfg(test)]
mod tests {
    use super::{event_message, AppLoader, AppLoaderConfig, LoadMode};
    use crate::bundle::{BundleContents, BundleStore, Verifier};
    use crate::error::AppError;
    use crate::events;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::{
        AppInfoHeader, BundleLayoutError, CapabilityId, Errno, LibraryError, LibraryScope,
        LoadHeader, NeededLibrary, RxeError, RxePermission, Segment, ABI_VERSION_CURRENT,
        APPINFO_MAGIC, BUNDLE_ID_MAX, BUNDLE_NAME_MAX, BUNDLE_VERSION_MAX, LOAD_FLAG_PIE,
        LOAD_MAGIC, MIME_TYPE_MAX, SYSCALL_TABLE_HASH_LEN,
    };
    use tairix_abi::{ProgramKind, PublisherId};
    use tairix_caps::CapabilitySet;
    use tairix_crypto::sha256;
    use tairix_log::{Event, EventId, Sink};

    const KERNEL_HASH: [u8; SYSCALL_TABLE_HASH_LEN] = [0x11; SYSCALL_TABLE_HASH_LEN];
    const CONTENT_HASH: [u8; 32] = [0x22; 32];
    /// The build signing key the fixture manifests declare. The `Verifier`
    /// seam is mocked, so it needs no real key material.
    const SIGNER: [u8; 32] = [0xEF; 32];
    /// The curated System runtime / C ABI shared library a hosted C program
    /// dynamically links; it lives under
    /// `/System/Libraries/`.
    const RUNTIME_LIB: &str = "/System/Libraries/libros-sys.so";

    fn inline_buf<const N: usize>(text: &str) -> [u8; N] {
        let mut buf = [0u8; N];
        buf[..text.len()].copy_from_slice(text.as_bytes());
        buf
    }

    /// Build an `AppInfo` manifest (header + capability list + one MIME
    /// entry) requesting `caps`, declaring `syscall_hash` and `content_hash`,
    /// self-published by [`SIGNER`].
    fn build_manifest(
        caps: &[CapabilityId],
        syscall_hash: [u8; SYSCALL_TABLE_HASH_LEN],
        content_hash: [u8; 32],
    ) -> Vec<u8> {
        build_manifest_published_by(caps, syscall_hash, content_hash, SIGNER, [0; 64])
    }

    /// The same manifest, but claiming `publisher` with `cert`, so a test can
    /// present the load gate with any publisher story — including the ones it
    /// must refuse.
    fn build_manifest_published_by(
        caps: &[CapabilityId],
        syscall_hash: [u8; SYSCALL_TABLE_HASH_LEN],
        content_hash: [u8; 32],
        publisher: [u8; 32],
        cert: [u8; 64],
    ) -> Vec<u8> {
        let header = AppInfoHeader {
            magic: APPINFO_MAGIC,
            abi_version: ABI_VERSION_CURRENT,
            flags: 0,
            capability_count: u16::try_from(caps.len()).unwrap(),
            mime_count: 1,
            browse_count: 0,
            kind: ProgramKind::Application.to_wire(),
            option_count: 0,
            id_len: u8::try_from("com.example.app".len()).unwrap(),
            name_len: u8::try_from("Example".len()).unwrap(),
            version_len: u8::try_from("1.0".len()).unwrap(),
            purpose_len: 0,
            author_len: 0,
            library_icon_len: 0,
            library: 0,
            title_len: 0,
            id: inline_buf::<BUNDLE_ID_MAX>("com.example.app"),
            name: inline_buf::<BUNDLE_NAME_MAX>("Example"),
            version: inline_buf::<BUNDLE_VERSION_MAX>("1.0"),
            library_icon: [0; tairix_abi::LIBRARY_ICON_MAX],
            purpose: [0; tairix_abi::BUNDLE_PURPOSE_MAX],
            author: [0; tairix_abi::BUNDLE_AUTHOR_MAX],
            title: [0; tairix_abi::BUNDLE_TITLE_MAX],
            syscall_table_hash: syscall_hash,
            content_hash,
            signer_pubkey: SIGNER,
            publisher_pubkey: publisher,
            publisher_cert: cert,
            signature: [0x12; 64],
        };
        let mut out = header.to_le_bytes().to_vec();
        for cap in caps {
            out.extend_from_slice(&cap.as_u16().to_le_bytes());
        }
        let mime = b"text/plain";
        out.push(u8::try_from(mime.len()).unwrap());
        let mut buf = [0u8; MIME_TYPE_MAX];
        buf[..mime.len()].copy_from_slice(mime);
        out.extend_from_slice(&buf);
        out
    }

    /// Build a minimal valid `rxe` entry-point image declaring `needed`
    /// shared libraries and carrying `cfi` as its CFI tag. This is the C
    /// program's `Run` binary as the dynamic loader sees it.
    fn build_run_image(needed: &[&str], cfi: [u8; SYSCALL_TABLE_HASH_LEN]) -> Vec<u8> {
        let header = LoadHeader {
            magic: LOAD_MAGIC,
            abi_version: ABI_VERSION_CURRENT,
            flags: LOAD_FLAG_PIE,
            segment_count: 1,
            needed_count: u16::try_from(needed.len()).unwrap(),
            entry: 0x1000,
            cfi_tag: cfi,
        };
        let segment = Segment {
            vaddr: 0x1000,
            file_offset: 0,
            file_size: 0x1000,
            mem_size: 0x1000,
            permission: RxePermission::ReadExecute,
        };
        let mut out = header.to_le_bytes().to_vec();
        out.extend_from_slice(&segment.to_le_bytes());
        for reference in needed {
            out.extend_from_slice(
                &NeededLibrary::from_reference(reference)
                    .unwrap()
                    .to_le_bytes(),
            );
        }
        out
    }

    fn full_layout() -> Vec<String> {
        [
            "AppInfo",
            "Run",
            "Code",
            "Libraries",
            "Resources",
            "DefaultSettings",
            "Help",
        ]
        .iter()
        .map(ToString::to_string)
        .collect()
    }

    fn cap_set(list: &[CapabilityId]) -> CapabilitySet {
        let mut set = CapabilitySet::empty();
        for cap in list {
            set.insert(*cap);
        }
        set
    }

    struct MockStore {
        entries: Vec<String>,
        appinfo: Vec<u8>,
        content: [u8; 32],
        run: Vec<u8>,
        entries_fail: bool,
        /// How many times [`BundleStore::contents`] was called, so a test can
        /// prove the load path reads (and so hashes) the bundle contents —
        /// which carry the `Run` image — exactly once.
        contents_calls: core::cell::Cell<usize>,
    }
    impl MockStore {
        fn good(caps: &[CapabilityId]) -> Self {
            Self {
                entries: full_layout(),
                appinfo: build_manifest(caps, KERNEL_HASH, CONTENT_HASH),
                content: CONTENT_HASH,
                run: build_run_image(&[RUNTIME_LIB], KERNEL_HASH),
                entries_fail: false,
                contents_calls: core::cell::Cell::new(0),
            }
        }
    }
    impl BundleStore for MockStore {
        fn entries(&self, _bundle: &str) -> Result<Vec<String>, Errno> {
            if self.entries_fail {
                return Err(Errno::NotFound);
            }
            Ok(self.entries.clone())
        }
        fn read_appinfo(&self, _bundle: &str) -> Result<Vec<u8>, Errno> {
            Ok(self.appinfo.clone())
        }
        fn contents(&self, _bundle: &str) -> Result<BundleContents, Errno> {
            self.contents_calls.set(self.contents_calls.get() + 1);
            Ok(BundleContents {
                content_hash: self.content,
                read_bytes: self.run.len(),
                run_image: self.run.clone(),
            })
        }
    }

    struct AcceptVerifier;
    impl Verifier for AcceptVerifier {
        fn verify(&self, _: &[u8], _: &[u8; 64], _: &[u8; 32]) -> Result<(), Errno> {
            Ok(())
        }
    }
    struct RejectVerifier;
    impl Verifier for RejectVerifier {
        fn verify(&self, _: &[u8], _: &[u8; 64], _: &[u8; 32]) -> Result<(), Errno> {
            Err(Errno::SignatureInvalid)
        }
    }

    struct RecordingSink {
        events: RefCell<Vec<EventId>>,
        /// The `(load, verify)` durations carried by the last `APP_LOADED`
        /// record, so a test can assert both phases were timed and emitted.
        loaded_phases: RefCell<Option<(tairix_abi::Duration64, tairix_abi::Duration64)>>,
        /// The `read_bytes` the last `APP_LOADED` record carried — the load
        /// span's denominator.
        loaded_bytes: RefCell<Option<u64>>,
        /// Whose key the last `APP_LOADED` record says admitted the bundle.
        loaded_signer: RefCell<Option<&'static str>>,
        /// The `errno` the last `APP_STORE_ERROR` record carried.
        store_errno: RefCell<Option<i64>>,
    }
    impl RecordingSink {
        fn new() -> Self {
            Self {
                events: RefCell::new(Vec::new()),
                loaded_phases: RefCell::new(None),
                loaded_bytes: RefCell::new(None),
                loaded_signer: RefCell::new(None),
                store_errno: RefCell::new(None),
            }
        }
        fn count(&self, id: EventId) -> usize {
            self.events.borrow().iter().filter(|e| **e == id).count()
        }
    }
    impl Sink for RecordingSink {
        fn write_event(&self, event: &Event<'_>) {
            self.events.borrow_mut().push(event.id);
            if event.id == events::APP_LOADED {
                let phase = |key| {
                    event.fields.iter().find_map(|f| match (f.key, f.value) {
                        (k, tairix_log::FieldValue::Duration(d)) if k == key => Some(d),
                        _ => None,
                    })
                };
                if let (Some(load), Some(verify)) = (phase("load"), phase("verify")) {
                    *self.loaded_phases.borrow_mut() = Some((load, verify));
                }
                *self.loaded_bytes.borrow_mut() =
                    event.fields.iter().find_map(|f| match (f.key, f.value) {
                        ("read_bytes", tairix_log::FieldValue::UnsignedInt(n)) => Some(n),
                        _ => None,
                    });
                *self.loaded_signer.borrow_mut() =
                    event.fields.iter().find_map(|f| match (f.key, f.value) {
                        ("signer", tairix_log::FieldValue::Str("own")) => Some("own"),
                        ("signer", tairix_log::FieldValue::Str("system")) => Some("system"),
                        _ => None,
                    });
            }
            if event.id == events::APP_STORE_ERROR {
                *self.store_errno.borrow_mut() =
                    event.fields.iter().find_map(|f| match (f.key, f.value) {
                        ("errno", tairix_log::FieldValue::SignedInt(n)) => Some(n),
                        _ => None,
                    });
            }
        }
    }

    /// A monotonic clock that advances a fixed step on every read, so the
    /// loader observes non-zero elapsed time for both phases deterministically
    /// (no host wall clock in a `no_std` unit test).
    struct StepClock {
        now: core::cell::Cell<u64>,
    }
    impl StepClock {
        fn new() -> Self {
            Self {
                now: core::cell::Cell::new(0),
            }
        }
    }
    impl crate::bundle::Clock for StepClock {
        fn now_ns(&self) -> u64 {
            let t = self.now.get();
            self.now.set(t + 1_000);
            t
        }
    }

    fn cfg<'a>(
        store: &'a dyn BundleStore,
        verifier: &'a dyn Verifier,
        clock: &'a dyn crate::bundle::Clock,
        sink: &'a RecordingSink,
    ) -> AppLoaderConfig<'a> {
        AppLoaderConfig {
            accepted_abi_version: ABI_VERSION_CURRENT,
            syscall_table_hash: KERNEL_HASH,
            store,
            verifier,
            clock,
            sink,
        }
    }

    /// A self-published bundle needs no certificate — its publisher key *is*
    /// the signing key the trust root admitted — and its developer identity is
    /// the labelled digest of that key.
    #[test]
    fn a_self_published_bundle_is_attributed_to_the_digest_of_its_publisher_key() {
        let store = MockStore::good(&[]);
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));

        let app = loader
            .load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary,
            )
            .expect("loads");

        let header = AppInfoHeader::from_bytes(&store.appinfo).expect("decodes");
        assert_eq!(app.publisher(), super::publisher_id_of(&header));
        assert!(!app.publisher().is_none());
        assert_eq!(sink.count(events::APP_PUBLISHER_INVALID), 0);
    }

    /// The hoisted derivation is the labelled digest of the manifest's
    /// publisher key, and a different key is a different developer.
    ///
    /// Pinned here rather than at the attestation site, so the gate's own
    /// `publisher()` test comparing against this helper cannot be satisfied by
    /// both sides being wrong together.
    #[test]
    fn the_publisher_derivation_is_the_labelled_digest_of_the_manifest_key() {
        let header = AppInfoHeader::from_bytes(&MockStore::good(&[]).appinfo).expect("decodes");
        assert_eq!(
            super::publisher_id_of(&header),
            PublisherId::from_raw(sha256(&header.publisher_id_preimage()))
        );
        assert!(!super::publisher_id_of(&header).is_none());

        let mut rotated = header;
        rotated.publisher_pubkey[0] ^= 0xFF;
        assert_ne!(
            super::publisher_id_of(&rotated),
            super::publisher_id_of(&header)
        );
    }

    /// A manifest may claim any publisher it likes; claiming one it cannot
    /// produce a certificate for is refused, so a hostile bundle cannot
    /// impersonate a developer to reach that developer's per-app store.
    #[test]
    fn a_forged_publisher_delegation_is_refused() {
        let mut store = MockStore::good(&[]);
        store.appinfo =
            build_manifest_published_by(&[], KERNEL_HASH, CONTENT_HASH, [0x5A; 32], [0x77; 64]);
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));

        assert_eq!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::PublisherCert)
        );
        assert_eq!(sink.count(events::APP_PUBLISHER_INVALID), 1);
        assert_eq!(sink.count(events::APP_LOADED), 0);
    }

    /// Neither half-form is admitted: a claim with no certificate to justify
    /// it, and a certificate with nothing to delegate, are both refused
    /// before any capability is granted.
    #[test]
    fn a_malformed_publisher_binding_is_refused() {
        for (publisher, cert) in [([0x5A; 32], [0u8; 64]), (SIGNER, [0x77; 64])] {
            let mut store = MockStore::good(&[]);
            store.appinfo =
                build_manifest_published_by(&[], KERNEL_HASH, CONTENT_HASH, publisher, cert);
            let verifier = AcceptVerifier;
            let sink = RecordingSink::new();
            let clock = StepClock::new();
            let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));

            assert_eq!(
                loader.load(
                    "/System/Applications/Example.app",
                    &CapabilitySet::empty(),
                    LoadMode::Ordinary
                ),
                Err(AppError::Manifest(Errno::SignatureInvalid))
            );
            assert_eq!(sink.count(events::APP_PUBLISHER_INVALID), 1);
            assert_eq!(sink.count(events::APP_LOADED), 0);
        }
    }

    #[test]
    fn loads_valid_bundle_and_intersects_capabilities() {
        let store = MockStore::good(&[CapabilityId::FS_MOUNT, CapabilityId::NET_RAW]);
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));

        // The user holds only NET_RAW, so FS_MOUNT is dropped on intersect.
        let user = cap_set(&[CapabilityId::NET_RAW]);
        let app = loader
            .load(
                "/System/Applications/Example.app",
                &user,
                LoadMode::Ordinary,
            )
            .expect("loads");

        assert_eq!(app.id(), "com.example.app");
        assert_eq!(app.name(), "Example");
        assert_eq!(app.version(), "1.0");
        assert_eq!(app.run_path(), "/System/Applications/Example.app/Run");
        // The returned image is byte-for-byte the store's validated `Run`,
        // so a spawner maps exactly what the pipeline checked.
        assert_eq!(
            app.run_image(),
            store
                .contents("/System/Applications/Example.app")
                .expect("contents")
                .run_image
                .as_slice()
        );
        assert!(app.granted().contains(CapabilityId::NET_RAW));
        assert!(!app.granted().contains(CapabilityId::FS_MOUNT));
        assert_eq!(app.granted().len(), 1);
        assert_eq!(sink.count(events::APP_LOADED), 1);
        // The load path reads (and hashes) the bundle contents exactly once;
        // the `Run` image comes back from that same pass rather than a
        // second disk read. The extra `+ 1` is this test's own probe above.
        assert_eq!(store.contents_calls.get(), 1 + 1);
    }

    /// Records the exact byte stream the loader asked to be verified, so a
    /// test can pin what the signature covers.
    struct CapturingVerifier(core::cell::RefCell<alloc::vec::Vec<u8>>);
    impl Verifier for CapturingVerifier {
        fn verify(&self, signed: &[u8], _: &[u8; 64], _: &[u8; 32]) -> Result<(), Errno> {
            *self.0.borrow_mut() = signed.to_vec();
            Ok(())
        }
    }

    #[test]
    fn signature_covers_the_capability_body() {
        // Regression: the signed message must be the header prefix
        // concatenated with the capability/MIME body — a verifier fed the
        // header alone would let a tampered store swap capability ids
        // behind a valid signature.
        let store = MockStore::good(&[CapabilityId::FS_MOUNT, CapabilityId::NET_RAW]);
        let verifier = CapturingVerifier(core::cell::RefCell::new(alloc::vec::Vec::new()));
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        loader
            .load(
                "/System/Applications/Example.app",
                &cap_set(&[CapabilityId::NET_RAW]),
                LoadMode::Ordinary,
            )
            .expect("loads");

        let signed = verifier.0.borrow();
        let manifest = store
            .read_appinfo("/System/Applications/Example.app")
            .expect("manifest");
        let body = &manifest[AppInfoHeader::WIRE_LEN..];
        assert!(!body.is_empty(), "fixture must carry a capability body");
        assert_eq!(signed.len(), AppInfoHeader::signed_range().end + body.len());
        assert_eq!(
            &signed[..AppInfoHeader::signed_range().end],
            &manifest[AppInfoHeader::signed_range()]
        );
        assert_eq!(&signed[AppInfoHeader::signed_range().end..], body);
    }

    /// Regression: a bundle's kind was only the directory it sat in, so a
    /// service or component copied anywhere ran as whatever that directory
    /// held.
    #[test]
    fn a_bundle_runs_only_from_where_its_signed_kind_may() {
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let load = |kind: ProgramKind, bundle: &str| {
            let mut store = MockStore::good(&[]);
            let mut header = AppInfoHeader::from_bytes(&store.appinfo).unwrap();
            header.kind = kind.to_wire();
            store.appinfo[..AppInfoHeader::WIRE_LEN].copy_from_slice(&header.to_le_bytes());
            AppLoader::new(cfg(&store, &AcceptVerifier, &clock, &sink))
                .load(bundle, &CapabilitySet::empty(), LoadMode::Ordinary)
                .map(|_| ())
        };
        assert_eq!(
            load(ProgramKind::Service, "/System/Services/sysinfod.app"),
            Ok(())
        );
        assert_eq!(
            load(ProgramKind::Service, "/Users/ada/Commands/sysinfod.app"),
            Err(AppError::Misplaced)
        );
        assert_eq!(
            load(ProgramKind::Command, "/System/Applications/ps.app"),
            Err(AppError::Misplaced)
        );
        assert_eq!(
            load(ProgramKind::Command, "/Users/ada/bin/tool.app"),
            Ok(())
        );
        assert!(sink.events.borrow().contains(&events::APP_MISPLACED));
    }

    #[test]
    fn minimal_layout_loads() {
        let mut store = MockStore::good(&[]);
        store.entries = vec!["AppInfo".to_string(), "Run".to_string()];
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        let app = loader
            .load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary,
            )
            .expect("loads");
        assert!(app.granted().is_empty());
    }

    #[test]
    fn rejects_unknown_layout_entry() {
        let mut store = MockStore::good(&[]);
        store.entries.push("Plugins".to_string());
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        assert_eq!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::Layout(BundleLayoutError::UnknownEntry))
        );
        assert_eq!(sink.count(events::APP_LAYOUT_REJECTED), 1);
    }

    #[test]
    fn rejects_missing_run() {
        let mut store = MockStore::good(&[]);
        store.entries = vec!["AppInfo".to_string(), "Resources".to_string()];
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        assert_eq!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::Layout(BundleLayoutError::MissingRun))
        );
    }

    #[test]
    fn rejects_undecodable_manifest() {
        let mut store = MockStore::good(&[]);
        store.appinfo[0] ^= 0xFF; // corrupt magic
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        assert_eq!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::Manifest(Errno::BadMagic))
        );
        assert_eq!(sink.count(events::APP_MANIFEST_INVALID), 1);
    }

    #[test]
    fn rejects_unsupported_abi_version() {
        let store = MockStore::good(&[]);
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let mut config = cfg(&store, &verifier, &clock, &sink);
        config.accepted_abi_version = ABI_VERSION_CURRENT + 1;
        let loader = AppLoader::new(config);
        assert_eq!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::Manifest(Errno::AbiVersionUnsupported))
        );
    }

    #[test]
    fn rejects_interface_hash_mismatch() {
        let store = MockStore {
            entries: full_layout(),
            appinfo: build_manifest(&[], [0x99; SYSCALL_TABLE_HASH_LEN], CONTENT_HASH),
            content: CONTENT_HASH,
            run: build_run_image(&[RUNTIME_LIB], KERNEL_HASH),
            entries_fail: false,
            contents_calls: core::cell::Cell::new(0),
        };
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        assert_eq!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::InterfaceHashMismatch)
        );
        assert_eq!(sink.count(events::APP_INTERFACE_MISMATCH), 1);
    }

    #[test]
    fn rejects_bad_signature() {
        let store = MockStore::good(&[]);
        let verifier = RejectVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        assert_eq!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::Signature)
        );
        assert_eq!(sink.count(events::APP_SIGNATURE_INVALID), 1);
    }

    #[test]
    fn rejects_content_hash_mismatch() {
        let mut store = MockStore::good(&[]);
        store.content = [0x77; 32]; // does not match the manifest's CONTENT_HASH
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        assert_eq!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::ContentHashMismatch)
        );
        assert_eq!(sink.count(events::APP_CONTENT_MISMATCH), 1);
    }

    #[test]
    fn store_failure_is_reported() {
        let mut store = MockStore::good(&[]);
        store.entries_fail = true;
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        assert_eq!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::Store(Errno::NotFound))
        );
        assert_eq!(sink.count(events::APP_STORE_ERROR), 1);
        assert_eq!(
            *sink.store_errno.borrow(),
            Some(i64::from(Errno::NotFound.as_i32())),
            "the record names why the store refused"
        );
    }

    #[test]
    fn rejects_truncated_capability_body() {
        let mut store = MockStore::good(&[CapabilityId::FS_MOUNT]);
        // Drop the capability/MIME body, leaving only the header. The
        // declared capability_count (1) can no longer be satisfied.
        store.appinfo.truncate(AppInfoHeader::WIRE_LEN);
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        assert_eq!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::Manifest(Errno::BufferTooSmall))
        );
    }

    /// The one reading of a manifest's request, as an authority deciding who
    /// may allow a program reads it: exactly the identifiers the body lists,
    /// and a short body refused rather than read as fewer.
    #[test]
    fn a_manifest_requests_exactly_the_capabilities_its_body_lists() {
        let caps = [CapabilityId::TIME_SET, CapabilityId::SHM];
        let manifest = build_manifest(&caps, [0; SYSCALL_TABLE_HASH_LEN], [0; 32]);
        let header = AppInfoHeader::from_bytes(&manifest).expect("a header");
        let requested = super::requested_capabilities(&manifest, &header).expect("a request");
        assert!(caps.iter().all(|cap| requested.contains(*cap)));
        assert!(!requested.contains(CapabilityId::FS_MOUNT));
        assert_eq!(
            super::requested_capabilities(&manifest[..AppInfoHeader::WIRE_LEN], &header),
            Err(super::RequestRefused::Truncated)
        );
    }

    #[test]
    fn resolves_libraries_within_policy() {
        let store = MockStore::good(&[]);
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));

        assert_eq!(
            loader.resolve_library(
                "/System/Applications/Example.app",
                "/System/Applications/Example.app/Libraries/libui.so"
            ),
            Ok(LibraryScope::Bundle)
        );
        assert_eq!(
            loader.resolve_library(
                "/System/Applications/Example.app",
                "/System/Libraries/libtls.so"
            ),
            Ok(LibraryScope::System)
        );
        assert_eq!(sink.count(events::LIBRARY_RESOLVED), 2);
    }

    #[test]
    fn refuses_libraries_outside_policy() {
        let store = MockStore::good(&[]);
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));

        assert_eq!(
            loader.resolve_library("/System/Applications/Example.app", "/System/Kernel/secret"),
            Err(AppError::Library(LibraryError::OutsidePolicy))
        );
        assert_eq!(
            loader.resolve_library(
                "/System/Applications/Example.app",
                "/System/Libraries/../Kernel/x"
            ),
            Err(AppError::Library(LibraryError::Traversal))
        );
        assert_eq!(sink.count(events::LIBRARY_REFUSED), 2);
    }

    #[test]
    fn resolves_c_runtime_from_system_libraries() {
        // A C-compiled bundle whose Run binary dynamically links the curated
        // System runtime / C ABI library plus one private bundle library.
        let mut store = MockStore::good(&[]);
        store.run = build_run_image(
            &[
                RUNTIME_LIB,
                "/System/Applications/Example.app/Libraries/private.so",
            ],
            KERNEL_HASH,
        );
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));

        let app = loader
            .load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary,
            )
            .expect("loads");
        let libs = app.libraries();
        assert_eq!(libs.len(), 2);
        assert_eq!(libs[0].reference, RUNTIME_LIB);
        assert_eq!(libs[0].scope, LibraryScope::System);
        assert_eq!(
            libs[1].reference,
            "/System/Applications/Example.app/Libraries/private.so"
        );
        assert_eq!(libs[1].scope, LibraryScope::Bundle);
        assert_eq!(sink.count(events::LIBRARY_RESOLVED), 2);
        assert_eq!(sink.count(events::APP_LOADED), 1);
    }

    #[test]
    fn intersects_capabilities_for_c_bundle() {
        // The launching user holds only NET_RAW; FS_MOUNT is dropped, and the
        // C runtime still resolves from /System/Libraries.
        let store = MockStore::good(&[CapabilityId::FS_MOUNT, CapabilityId::NET_RAW]);
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));

        let user = cap_set(&[CapabilityId::NET_RAW]);
        let app = loader
            .load(
                "/System/Applications/Example.app",
                &user,
                LoadMode::Ordinary,
            )
            .expect("loads");
        assert!(app.granted().contains(CapabilityId::NET_RAW));
        assert!(!app.granted().contains(CapabilityId::FS_MOUNT));
        assert_eq!(app.granted().len(), 1);
        assert_eq!(app.libraries().len(), 1);
        assert_eq!(app.libraries()[0].scope, LibraryScope::System);
    }

    #[test]
    fn refuses_run_image_out_of_tree_library() {
        let mut store = MockStore::good(&[]);
        store.run = build_run_image(&["/System/Kernel/secret.so"], KERNEL_HASH);
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        assert_eq!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::Library(LibraryError::OutsidePolicy))
        );
        assert_eq!(sink.count(events::LIBRARY_REFUSED), 1);
    }

    #[test]
    fn refuses_run_image_with_cfi_mismatch() {
        let mut store = MockStore::good(&[]);
        store.run = build_run_image(&[RUNTIME_LIB], [0x99; SYSCALL_TABLE_HASH_LEN]);
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        assert_eq!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::RunImage(RxeError::InterfaceHashMismatch))
        );
        assert_eq!(sink.count(events::APP_RUN_IMAGE_INVALID), 1);
    }

    #[test]
    fn refuses_malformed_run_image() {
        let mut store = MockStore::good(&[]);
        store.run = alloc::vec![0u8, 1, 2, 3];
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));
        assert!(matches!(
            loader.load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary
            ),
            Err(AppError::RunImage(_))
        ));
        assert_eq!(sink.count(events::APP_RUN_IMAGE_INVALID), 1);
    }

    #[test]
    fn app_loaded_records_load_and_verify_durations() {
        // The clock advances 1000 ns per read; the pipeline makes three timed
        // store reads (entries, AppInfo, and the single contents pass that
        // both hashes the bundle and returns the `Run` image), each opened and
        // closed once, so the load phase is 3 × 1000 ns and the verify phase
        // is the remaining elapsed span. Both must be present and non-zero on
        // the successful-load record, so a slow first launch is diagnosable.
        // (`Run` is no longer read a second time, so this is one span fewer
        // than the old read-twice pipeline.)
        let store = MockStore::good(&[]);
        let verifier = AcceptVerifier;
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let loader = AppLoader::new(cfg(&store, &verifier, &clock, &sink));

        loader
            .load(
                "/System/Applications/Example.app",
                &CapabilitySet::empty(),
                LoadMode::Ordinary,
            )
            .expect("loads");

        assert_eq!(sink.count(events::APP_LOADED), 1);
        let (load, verify) = sink
            .loaded_phases
            .borrow()
            .expect("APP_LOADED carries load and verify durations");
        assert_eq!(load, tairix_abi::Duration64::from_nanos(3_000));
        assert!(
            verify > tairix_abi::Duration64::ZERO,
            "verification time must be recorded"
        );
        // The span's denominator travels with it: a bare duration cannot
        // tell a slow store from a large bundle.
        assert_eq!(
            *sink.loaded_bytes.borrow(),
            Some(store.run.len() as u64),
            "APP_LOADED carries the bytes the load span moved"
        );
    }

    /// Regression: a refused misplaced bundle was logged under the generic
    /// message, telling a reader nothing.
    #[test]
    fn event_messages_are_total() {
        assert_eq!(
            event_message(events::APP_LOADED),
            "application bundle loaded"
        );
        assert_eq!(event_message(EventId(1)), "load gate event");
        for id in events::ALL {
            assert_ne!(event_message(id), "load gate event", "{id:?}");
        }
    }

    const SYSTEM_SAVER: &str = "/System/Screensavers/Clock.app";
    const OWN_SAVER: &str = "/Users/ada/Library/Screensavers/Waves.app";
    const OWN_SEED: [u8; 32] = [0x5A; 32];

    fn keep_option() -> tairix_abi::saver::SaverOption {
        tairix_abi::saver::SaverOption::new(
            tairix_abi::saver::OptionKey::new("keep").unwrap(),
            (
                tairix_abi::bounded_text::BoundedText::new("Keep").unwrap(),
                tairix_abi::bounded_text::BoundedText::EMPTY,
            ),
            tairix_abi::saver::SaverOptionKind::Switch { default: true },
        )
        .unwrap()
    }

    /// A screensaver manifest declaring `flags` and `options`, signed by
    /// `seed`'s key over exactly the message the gate verifies.
    fn saver_manifest(
        flags: u32,
        options: &[[u8; tairix_abi::saver::SAVER_OPTION_ENTRY_LEN]],
        seed: [u8; 32],
    ) -> Vec<u8> {
        let key = tairix_crypto::Ed25519SecretKey::from_seed(&seed);
        let signer = *key.public_key().as_bytes();
        let mut header =
            AppInfoHeader::from_bytes(&build_manifest(&[], KERNEL_HASH, CONTENT_HASH)).unwrap();
        header.kind = ProgramKind::Screensaver.to_wire();
        header.mime_count = 0;
        header.flags = flags;
        header.option_count = u8::try_from(options.len()).unwrap();
        header.signer_pubkey = signer;
        header.publisher_pubkey = signer;
        let mut body = Vec::new();
        for entry in options {
            body.extend_from_slice(entry);
        }
        let wire = header.to_le_bytes();
        let mut message = wire[AppInfoHeader::signed_range()].to_vec();
        message.extend_from_slice(&body);
        header.signature = *key.sign(&message).as_bytes();
        let mut out = header.to_le_bytes().to_vec();
        out.extend_from_slice(&body);
        out
    }

    fn saver_store(appinfo: Vec<u8>) -> MockStore {
        MockStore {
            appinfo,
            ..MockStore::good(&[])
        }
    }

    fn load_with(
        store: &MockStore,
        verifier: &dyn Verifier,
        bundle: &str,
        mode: LoadMode,
    ) -> (Result<crate::bundle::LoadedApp, AppError>, RecordingSink) {
        let sink = RecordingSink::new();
        let clock = StepClock::new();
        let result = AppLoader::new(cfg(store, verifier, &clock, &sink)).load(
            bundle,
            &CapabilitySet::empty(),
            mode,
        );
        (result, sink)
    }

    #[test]
    fn a_screensaver_loads_only_into_a_drawing_spawn() {
        let store = saver_store(saver_manifest(0, &[keep_option().to_entry()], OWN_SEED));
        let (refused, sink) = load_with(&store, &AcceptVerifier, SYSTEM_SAVER, LoadMode::Ordinary);
        assert_eq!(refused, Err(AppError::DrawingOnly));
        assert_eq!(sink.count(events::APP_DRAWING_ONLY), 1);
        let (loaded, _) = load_with(&store, &AcceptVerifier, SYSTEM_SAVER, LoadMode::Drawing);
        let app = loaded.expect("a system saver loads into a drawing spawn");
        assert_eq!(app.kind(), ProgramKind::Screensaver);
        assert!(app.granted().is_empty());
    }

    #[test]
    fn a_cached_screensaver_is_held_to_its_spawn_again() {
        let store = saver_store(saver_manifest(0, &[], OWN_SEED));
        let (loaded, _) = load_with(&store, &AcceptVerifier, SYSTEM_SAVER, LoadMode::Drawing);
        let app = loaded.expect("loads");
        let sink = RecordingSink::new();
        assert_eq!(super::admit_cached(&app, LoadMode::Drawing, &sink), Ok(()));
        assert_eq!(
            super::admit_cached(&app, LoadMode::Ordinary, &sink),
            Err(AppError::DrawingOnly)
        );
        assert_eq!(sink.count(events::APP_DRAWING_ONLY), 1);
        let (command, _) = load_with(
            &MockStore::good(&[]),
            &AcceptVerifier,
            "/System/Applications/Example.app",
            LoadMode::Ordinary,
        );
        let command = command.expect("loads");
        assert_eq!(
            super::admit_cached(&command, LoadMode::Ordinary, &sink),
            Ok(())
        );
        assert_eq!(
            super::admit_cached(&command, LoadMode::Drawing, &sink),
            Err(AppError::DrawingOnly),
            "a drawing spawn runs savers alone"
        );
        assert_eq!(sink.count(events::APP_DRAWING_ONLY), 2);
    }

    #[test]
    fn a_system_saver_answers_to_the_anchor() {
        let store = saver_store(saver_manifest(0, &[], OWN_SEED));
        let (refused, _) = load_with(&store, &RejectVerifier, SYSTEM_SAVER, LoadMode::Drawing);
        assert_eq!(refused, Err(AppError::Signature));
    }

    #[test]
    fn a_users_own_saver_is_verified_against_its_own_key() {
        let store = saver_store(saver_manifest(0, &[keep_option().to_entry()], OWN_SEED));
        let (loaded, sink) = load_with(&store, &RejectVerifier, OWN_SAVER, LoadMode::Drawing);
        assert!(loaded.is_ok(), "the anchor is not asked: {loaded:?}");
        assert_eq!(
            *sink.loaded_signer.borrow(),
            Some("own"),
            "its admission names its own key"
        );
        let mut forged = store.appinfo.clone();
        let last = forged.len() - 1;
        forged[last] ^= 1;
        let (refused, sink) = load_with(
            &saver_store(forged),
            &AcceptVerifier,
            OWN_SAVER,
            LoadMode::Drawing,
        );
        assert_eq!(refused, Err(AppError::Signature));
        assert_eq!(sink.count(events::APP_SIGNATURE_INVALID), 1);
        let (ordinary, _) = load_with(&store, &AcceptVerifier, OWN_SAVER, LoadMode::Ordinary);
        assert_eq!(ordinary, Err(AppError::DrawingOnly));
    }

    #[test]
    fn a_users_own_saver_may_not_ask_for_a_privileged_feed() {
        for feed in [
            tairix_abi::APPINFO_FLAG_FEED_MACHINE,
            tairix_abi::APPINFO_FLAG_FEED_KEEP,
            tairix_abi::APPINFO_FLAG_FEED_SLIDES,
        ] {
            let store = saver_store(saver_manifest(feed, &[], OWN_SEED));
            let (refused, sink) = load_with(&store, &AcceptVerifier, OWN_SAVER, LoadMode::Drawing);
            assert_eq!(refused, Err(AppError::PrivilegedFeed));
            assert_eq!(sink.count(events::APP_FEED_REFUSED), 1);
            let (system, _) = load_with(&store, &AcceptVerifier, SYSTEM_SAVER, LoadMode::Drawing);
            assert!(system.is_ok(), "a system saver is given its feed");
        }
        let resources =
            tairix_abi::APPINFO_FLAG_SAVER_PARALLEL | tairix_abi::APPINFO_FLAG_SAVER_LARGE_MEMORY;
        let store = saver_store(saver_manifest(resources, &[], OWN_SEED));
        let (loaded, _) = load_with(&store, &AcceptVerifier, OWN_SAVER, LoadMode::Drawing);
        assert!(loaded.is_ok(), "resource needs are not feeds");
    }

    /// A program of another kind dropped in a saver store is not one, however
    /// it is signed.
    #[test]
    fn a_users_saver_store_holds_only_screensavers() {
        let mut header = AppInfoHeader::from_bytes(&saver_manifest(0, &[], OWN_SEED)).unwrap();
        header.kind = ProgramKind::Command.to_wire();
        let key = tairix_crypto::Ed25519SecretKey::from_seed(&OWN_SEED);
        let wire = header.to_le_bytes();
        header.signature = *key.sign(&wire[AppInfoHeader::signed_range()]).as_bytes();
        let command = saver_store(header.to_le_bytes().to_vec());
        let (misplaced, sink) = load_with(&command, &AcceptVerifier, OWN_SAVER, LoadMode::Drawing);
        assert_eq!(misplaced, Err(AppError::Misplaced));
        assert_eq!(sink.count(events::APP_MISPLACED), 1);
    }

    #[test]
    fn a_saver_whose_options_do_not_decode_is_refused() {
        let mut entry = keep_option().to_entry();
        entry[0] = 0;
        let store = saver_store(saver_manifest(0, &[entry], OWN_SEED));
        let (refused, sink) = load_with(&store, &AcceptVerifier, SYSTEM_SAVER, LoadMode::Drawing);
        assert_eq!(refused, Err(AppError::Manifest(Errno::BadMagic)));
        assert_eq!(sink.count(events::APP_MANIFEST_INVALID), 1);
        let twice = [keep_option().to_entry(), keep_option().to_entry()];
        let store = saver_store(saver_manifest(0, &twice, OWN_SEED));
        let (refused, _) = load_with(&store, &AcceptVerifier, SYSTEM_SAVER, LoadMode::Drawing);
        assert_eq!(refused, Err(AppError::Manifest(Errno::OutOfRange)));
    }
}
