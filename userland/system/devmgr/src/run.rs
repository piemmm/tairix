//! The `Run` entry-point binary of the device-manager service, installed
//! at `/System/Services/devmgr.app/Run` — the
//! long-running user-space service PID 1 `init` launches to observe the
//! discovered hardware tree and react to it.
//!
//! This is a **pure-Rust** program: TAIRiX is Rust-only,
//! so it links the Rust userland runtime `tairix-rt` — never the C ABI,
//! which exists solely for programs **not** written in Rust. `tairix-rt` provides `_start`, the per-process stack canary, the panic handler, and the syscall wrappers
//! (`hw_tree_read` / `hw_tree_wait`); `tairix_rt::entry!` names this
//! program's `main`.
//!
//! # What this service does (Design D D2b-2c)
//!
//! At startup it fetches the kernel-decoded driver **catalogue** over the
//! capability-gated `ipc_call` endpoint the kernel store service serves
//! (`tairix_abi::driver_store::DRIVER_STORE_ENDPOINT`): one entry per installed bundle, an opaque `bundle_id` plus the
//! bind table the kernel decoded from its signed manifest. The store is
//! read-only and static for the life of the system, so the catalogue is
//! fetched **once**; a fetch failure is fail-soft.
//!
//! It then reads the architecture-neutral hardware tree the kernel
//! discovered at boot through the capability-gated `hw_tree_read` syscall
//! (`CAP_SYSINFO_HW`), matches each node against
//! the catalogue with the shared [`tairix_devmatch`] policy, and asks the kernel to load the matched bundle for each winning
//! node (`StoreRequest::Load`) — the kernel re-runs the signed gate and
//! spawns the driver with only that node's grants. It then
//! **blocks** in `hw_tree_wait` until the tree changes (a node seeded,
//! appended, or removed) and re-matches, loading each
//! newly-appeared node's driver once. It never busy-spins: the wait parks the task in the kernel until the store's generation
//! advances. The device manager owns matching *policy* only; the kernel
//! keeps the load *mechanism* (bytes, signature, spawn) in its TCB.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy,
//! and fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
// Compiled only for the freestanding service binary, which links the
// optional `tairix-rt` runtime through the default `program` feature. The
// kernel links this crate's *library* with `default-features = false`, so
// it never builds this module (nor pulls in `tairix-rt`).
#[cfg(all(freestanding, feature = "program"))]
mod program {
    use tairix_abi::audio::{AudioBaseline, AudioRequest, AUDIO_ENDPOINT, AUDIO_MAX_REQUEST};
    use tairix_abi::driver_store::{
        decode_config_reply, StoreRequest, SystemConfigFile, DRIVER_STORE_ENDPOINT,
        READ_CONFIG_REQUEST_LEN,
    };
    use tairix_abi::hwtree::HwDeviceClass;
    use tairix_abi::net_ipc::{
        decode_bind_driver_reply, NetBondConfigMsg, NetInterfaceConfigMsg, NetstackRequest,
        NetworkSettings, BIND_DRIVER_REPLY_LEN, IF_NAME_LEN, NETSTACK_ENDPOINT,
    };
    use tairix_abi::reply::{decode_status_reply, STATUS_REPLY_LEN};
    use tairix_abi::sound_cue::SoundEvent;
    use tairix_abi::waitset::WaitSetOp;
    use tairix_abi::OpenFlags;
    use tairix_abi::{Errno, HwNode, HwTreeHeader, WAITSET_TIMEOUT_NONE};
    use tairix_devmgr::audiobind::{AudioBaselineSource, AudiodBind};
    use tairix_devmgr::{
        events, DriverStoreCall, HwTreeService, NetstackBind, NetworkConfigSource,
        NetworkInterfaceConfigSource, Presence, WAKE_SOURCES,
    };
    use tairix_log::{log, Event, Field, Level};
    use tairix_netconfig::InterfaceConfigPlan;
    use tairix_rt::LogSink;
    use tairix_soundtheme::cue::RtCuePost;
    use tairix_soundtheme::{Answer, PostedCues, CUE_ANSWER_DEADLINE_NS};
    use tairix_util::fmt::{format_hex_u64, format_u64, format_usize};

    /// Buffer the catalogue and each `Load` reply are received into, sized to
    /// the endpoint's `DRIVER_STORE_MAX_REPLY` so a full catalogue is never
    /// truncated. Disjoint from the tree buffer so
    /// a load can run while a tree snapshot is being decoded.
    const REPLY_BUF_LEN: usize = 64 * 1024;

    /// The stable name of a node's device class for the log line. An
    /// unknown (un-modelled) discriminant is reported as `?`, never
    /// guessed.
    fn class_name(class: Option<HwDeviceClass>) -> &'static str {
        class.map_or("?", HwDeviceClass::name)
    }

    /// The production [`HwTreeService`] backing: the reactive observe loop
    /// ([`tairix_devmgr::run`]) reads, waits, and reports through this seam,
    /// which binds the `hw_tree_read` / `hw_tree_wait` `abi-v1` syscalls and
    /// emits each tree/node report through the kernel's diagnostic log via
    /// [`LogSink`] (the serial UART on a debug build) — never `stderr`. The loop's control flow is host-tested in
    /// `tairix_devmgr::service`; this is the freestanding I/O it binds.
    struct RtTreeService {
        /// The wait set holding [`WAKE_SOURCES`].
        set: u64,
        /// Device sounds asked for and not yet answered.
        cues: PostedCues,
        cue_post: RtCuePost,
    }

    /// The wait-set token a cue's answer wakes the service with, after every
    /// one [`WAKE_SOURCES`] takes.
    const TOKEN_CUE: u64 = WAKE_SOURCES.len() as u64 + 1;

    impl RtTreeService {
        /// A seam whose wait set holds every wake source, built before the
        /// first read of the tree, so no move after that read is missed.
        fn new() -> Result<Self, Errno> {
            let set = tairix_rt::waitset_create();
            if set < 0 {
                return Err(Errno::from_syscall(set));
            }
            let set = set.unsigned_abs();
            for (token, (kind, id)) in (1u64..).zip(WAKE_SOURCES) {
                let added = tairix_rt::waitset_ctl(set, WaitSetOp::Add, kind, id, token);
                if added != 0 {
                    return Err(Errno::from_syscall(added));
                }
            }
            Ok(Self {
                set,
                cues: PostedCues::new(),
                cue_post: RtCuePost::new(set, TOKEN_CUE),
            })
        }
    }

    impl Presence for RtTreeService {
        fn now_ns(&mut self) -> u64 {
            tairix_rt::clock_get()
        }

        fn settled(&mut self) {
            if tairix_rt::servicenotice::announce_ready().is_err() {
                log(
                    &LogSink,
                    &Event {
                        level: Level::Warn,
                        id: events::READINESS_REFUSED,
                        message:
                            "the service manager did not record the device manager's readiness",
                        fields: &[],
                    },
                );
            }
        }

        fn cue(&mut self, event: SoundEvent) {
            // A sound refused or unanswered is the authority's to record;
            // the device has arrived or left either way.
            let _ = self.cues.post(
                &mut self.cue_post,
                event,
                Answer::Started,
                CUE_ANSWER_DEADLINE_NS,
            );
        }
    }

    impl HwTreeService for RtTreeService {
        fn read_tree(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
            tairix_rt::hw_tree_read(buf).map_err(Errno::from_syscall)
        }

        fn wait_for_change(&mut self, timeout_ns: u64) -> Result<(), Errno> {
            // Park until the tree or the mount table moves, or `timeout_ns`
            // elapses; the caller picks the deadline (unbounded once nothing
            // is outstanding). A negative return is a `-errno` the loop
            // fails closed on, `TimedOut` included — the loop reads that as
            // "re-react". A cue's answer is collected and the park resumed,
            // since it moves nothing the loop reacts to.
            let deadline = (timeout_ns != WAITSET_TIMEOUT_NONE)
                .then(|| tairix_rt::clock_get().saturating_add(timeout_ns));
            loop {
                let left = deadline.map_or(WAITSET_TIMEOUT_NONE, |at| {
                    at.saturating_sub(tairix_rt::clock_get())
                });
                let mut token = 0u64;
                let waited = tairix_rt::waitset_wait(self.set, left, &mut token);
                if waited < 0 {
                    return Err(Errno::from_syscall(waited));
                }
                if token != TOKEN_CUE {
                    return Ok(());
                }
                self.cues.collect(&mut self.cue_post, |_, _| {});
            }
        }

        fn on_header(&mut self, header: &HwTreeHeader) {
            // Verbose boot/hotplug diagnostics: emitted at `Debug` so they are
            // filtered out by default and surface only when the level is
            // lowered. Routed through the kernel diagnostic
            // log, never `stderr`.
            let mut gen = [0u8; 16];
            let mut count = [0u8; 20];
            log(
                &LogSink,
                &Event {
                    level: Level::Debug,
                    id: events::TREE_OBSERVED,
                    message: "hardware tree snapshot observed",
                    fields: &[
                        Field {
                            key: "generation",
                            value: tairix_log::FieldValue::Str(format_hex_u64(
                                header.generation(),
                                &mut gen,
                            )),
                        },
                        Field {
                            key: "nodes",
                            value: tairix_log::FieldValue::Str(format_u64(
                                header.node_count(),
                                &mut count,
                            )),
                        },
                    ],
                },
            );
        }

        fn on_node(&mut self, node: &HwNode) {
            let mut id = [0u8; 12];
            let mut parent = [0u8; 12];
            let mut keys = [0u8; 12];
            // The root has no parent (the all-ones sentinel); name it `root`
            // rather than render a meaningless huge number.
            let parent_str = if node.is_root() {
                "root"
            } else {
                format_usize(node.parent() as usize, &mut parent)
            };
            log(
                &LogSink,
                &Event {
                    level: Level::Debug,
                    id: events::NODE_OBSERVED,
                    message: "hardware tree node observed",
                    fields: &[
                        Field {
                            key: "id",
                            value: tairix_log::FieldValue::Str(format_usize(
                                node.id() as usize,
                                &mut id,
                            )),
                        },
                        Field {
                            key: "parent",
                            value: tairix_log::FieldValue::Str(parent_str),
                        },
                        Field {
                            key: "class",
                            value: tairix_log::FieldValue::Str(class_name(node.class())),
                        },
                        Field {
                            key: "keys",
                            value: tairix_log::FieldValue::Str(format_usize(
                                node.match_keys().len(),
                                &mut keys,
                            )),
                        },
                    ],
                },
            );
        }
    }

    /// The production [`DriverStoreCall`] backing: it binds the `ipc_call`
    /// `abi-v1` syscall to the read-only `/System` driver-store endpoint
    /// ([`DRIVER_STORE_ENDPOINT`]) the kernel store service serves. The protocol logic (request framing, reply
    /// decoding) is host-tested in `tairix_devmgr::store`; this is the
    /// freestanding I/O it binds. The kernel re-checks the
    /// caller's `CAP_DRV_LOAD` on every call; this client adds no authority.
    struct RtStoreCall;

    impl DriverStoreCall for RtStoreCall {
        fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
            // `ipc_call` returns the raw `-errno` on failure; recover the
            // typed `Errno` and surface it fail-closed.
            tairix_rt::ipc_call(DRIVER_STORE_ENDPOINT, request, reply).map_err(Errno::from_syscall)
        }
    }

    /// The production [`AudiodBind`] backing: hands a discovered sound
    /// device's channel to the audio service with one `ipc_call` to the
    /// reserved [`AUDIO_ENDPOINT`] carrying an
    /// [`AudioRequest::BindDriver`]. The audio service checks the call
    /// against this process's kernel-attested `CAP_DRV_LOAD`; this client
    /// adds no authority, and the protocol logic (which channels to bind,
    /// once each, fail-soft on refusal) is host-tested in
    /// `tairix_devmgr::audiobind`.
    struct RtAudiodBind;

    impl RtAudiodBind {
        /// One `audio-v1` call answered with a status.
        fn call(request: &AudioRequest) -> Result<(), Errno> {
            let mut frame = [0u8; AUDIO_MAX_REQUEST];
            let len = request.encode(&mut frame)?;
            let mut reply = [0u8; STATUS_REPLY_LEN];
            let len = tairix_rt::ipc_call(AUDIO_ENDPOINT, &frame[..len], &mut reply)
                .map_err(Errno::from_syscall)?;
            decode_status_reply(&reply[..len])
        }
    }

    impl AudiodBind for RtAudiodBind {
        fn bind_driver(&mut self, endpoint_id: u64, location: u64) -> Result<(), Errno> {
            Self::call(&AudioRequest::BindDriver {
                endpoint_id,
                location,
            })
        }

        fn unbind_driver(&mut self, endpoint_id: u64) -> Result<(), Errno> {
            Self::call(&AudioRequest::UnbindDriver { endpoint_id })
        }

        fn deliver_baseline(&mut self, baseline: AudioBaseline) -> Result<(), Errno> {
            Self::call(&AudioRequest::Baseline(baseline))
        }
    }

    /// The production [`AudioBaselineSource`] backing: the `audio.*` keys of
    /// `system.conf`, read through the layered store and mapped through the
    /// one shared `lib/sysconfig` engine. [`None`] on any failure, which
    /// leaves the audio service on the baseline nobody configured.
    struct RtAudioBaseline;

    impl AudioBaselineSource for RtAudioBaseline {
        fn load(&mut self) -> Option<AudioBaseline> {
            #[allow(
                clippy::large_stack_arrays,
                reason = "one byte past the engine's ceiling, so a document that fills \
                          the buffer is proven over-long rather than parsed short"
            )]
            let mut buf = [0u8; tairix_sysconfig::MAX_CONFIG_LEN + 1];
            let (len, path) = read_layered_config(SystemConfigFile::System, &mut buf)?;
            let Some(config) = core::str::from_utf8(&buf[..len])
                .ok()
                .and_then(|text| tairix_sysconfig::SystemConfig::parse(text).ok())
            else {
                malformed_document(path);
                return None;
            };
            Some(config.audio_baseline())
        }
    }

    /// The production [`NetstackBind`] backing: hands a discovered NIC
    /// device channel to the network stack with one `ipc_call` to the
    /// reserved [`NETSTACK_ENDPOINT`] carrying a
    /// [`NetstackRequest::BindDriver`]. The kernel gates the call on the
    /// device manager's `CAP_NET_ADMIN`; this client adds no authority, and
    /// the protocol logic (which channels to bind, once each, fail-soft on
    /// refusal) is host-tested in `tairix_devmgr::netbind`.
    struct RtNetstackBind;

    impl NetstackBind for RtNetstackBind {
        fn bind_driver(
            &mut self,
            endpoint_id: u64,
            iface: &[u8; IF_NAME_LEN],
            node_location: u64,
        ) -> Result<[u8; 6], Errno> {
            let request = NetstackRequest::BindDriver {
                endpoint_id,
                iface: *iface,
                node_location,
            }
            .to_le_bytes();
            let mut reply = [0u8; BIND_DRIVER_REPLY_LEN];
            let len = tairix_rt::ipc_call(NETSTACK_ENDPOINT, &request, &mut reply)
                .map_err(Errno::from_syscall)?;
            decode_bind_driver_reply(&reply[..len])
        }

        fn apply_settings(&mut self, settings: NetworkSettings) -> Result<(), Errno> {
            let request = NetstackRequest::ApplyNetworkSettings(settings).to_le_bytes();
            let mut reply = [0u8; STATUS_REPLY_LEN];
            let len = tairix_rt::ipc_call(NETSTACK_ENDPOINT, &request, &mut reply)
                .map_err(Errno::from_syscall)?;
            decode_status_reply(&reply[..len])
        }

        fn apply_interface_config(&mut self, config: &NetInterfaceConfigMsg) -> Result<(), Errno> {
            let request = config.to_le_bytes();
            let mut reply = [0u8; STATUS_REPLY_LEN];
            let len = tairix_rt::ipc_call(NETSTACK_ENDPOINT, &request, &mut reply)
                .map_err(Errno::from_syscall)?;
            decode_status_reply(&reply[..len])
        }

        fn apply_bond_config(&mut self, config: &NetBondConfigMsg) -> Result<(), Errno> {
            let request = config.to_le_bytes();
            let mut reply = [0u8; STATUS_REPLY_LEN];
            let len = tairix_rt::ipc_call(NETSTACK_ENDPOINT, &request, &mut reply)
                .map_err(Errno::from_syscall)?;
            decode_status_reply(&reply[..len])
        }
    }

    /// The `ipc_call` reply buffer a config read frames into: the reply header
    /// plus the larger of the two config engines' document ceilings, so
    /// either whitelisted file fits in one bounded read. A document longer
    /// than its engine's ceiling could never parse anyway, so the buffer is
    /// sized to that ceiling, never a scalable capacity.
    const STORE_CONFIG_REPLY_LEN: usize =
        8 + if tairix_sysconfig::MAX_CONFIG_LEN > tairix_netconfig::MAX_CONFIG_LEN {
            tairix_sysconfig::MAX_CONFIG_LEN
        } else {
            tairix_netconfig::MAX_CONFIG_LEN
        };

    /// Read the image's default of one settings document over the
    /// always-mounted read-only `/System` **store** endpoint, copying its
    /// bytes into `out` and returning their length.
    ///
    /// The device manager configures interfaces on the same read-only
    /// `/System` volume the drivers autoload from, and must do so *before*
    /// the root is mounted — while the general VFS (`fs_open`) is not yet
    /// mounted. The store service already owns that volume, so the default is
    /// read through it (`StoreRequest::ReadConfig`) over the same
    /// `CAP_DRV_LOAD`-gated endpoint the catalogue uses; the kernel re-checks
    /// the capability, so this adds no authority. [`None`] on any failure (an
    /// absent file is a benign in-band `NotFound`, an oversized or corrupt
    /// frame, a transport error), so a caller keeps its safe defaults and
    /// retries at its next reaction, never guessing (fail closed).
    fn read_store_config(which: SystemConfigFile, out: &mut [u8]) -> Option<usize> {
        let mut request = [0u8; READ_CONFIG_REQUEST_LEN];
        let n = StoreRequest::ReadConfig { which }
            .encode(&mut request)
            .ok()?;
        // Deliberately on the stack: this runs before the root unlock, so the
        // heap's producer is not up yet. The spawn stack sizing covers it.
        #[allow(clippy::large_stack_arrays)]
        let mut reply = [0u8; STORE_CONFIG_REPLY_LEN];
        let len = tairix_rt::ipc_call(DRIVER_STORE_ENDPOINT, &request[..n], &mut reply).ok()?;
        let bytes = decode_config_reply(&reply[..len]).ok()?;
        if bytes.len() > out.len() {
            return None;
        }
        out[..bytes.len()].copy_from_slice(bytes);
        Some(bytes.len())
    }

    /// Read the administrator's layer of a settings document at `path`
    /// through the **secured VFS**, under this service's own attested
    /// identity: `Ok(None)` when the layer does not exist, or cannot be
    /// reached because the root is not mounted yet.
    ///
    /// `out` must be one byte longer than the document's parse ceiling: a
    /// read that fills it proves the file is over-long, and is refused
    /// rather than handed on as a truncated prefix that might parse as a
    /// valid — but partial — document.
    fn read_vfs_config(path: &str, out: &mut [u8]) -> Result<Option<usize>, Errno> {
        let ret = tairix_rt::fs_open(path.as_bytes(), OpenFlags::READ, 0);
        if ret < 0 {
            return match Errno::from_syscall(ret) {
                Errno::NotFound | Errno::NotImplemented => Ok(None),
                err => Err(err),
            };
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        // `ret >= 0` checked above; descriptors are small kernel indices.
        let fd = ret as u32;
        let outcome = tairix_rt::fs_read(fd, 0, out);
        let _ = tairix_rt::fs_close(fd);
        let len = outcome.map_err(Errno::from_syscall)?;
        if len < out.len() {
            Ok(Some(len))
        } else {
            Err(Errno::LengthOutOfRange)
        }
    }

    /// Read one settings document from the first of its layers that exists
    /// ([`SystemConfigFile::read_effective`]), answering its length and the
    /// path of the layer read.
    ///
    /// The administrator's layer is reached through the VFS and the image's
    /// over the store endpoint, which is what lets the image's stand before
    /// the root is mounted. An administrator's layer that exists but cannot
    /// be read delivers nothing, and is reported.
    fn read_layered_config(
        which: SystemConfigFile,
        out: &mut [u8],
    ) -> Option<(usize, &'static str)> {
        let found = which.read_effective(|path| {
            let len = if path == which.path() {
                read_vfs_config(path, out)?
            } else {
                read_store_config(which, out)
            };
            Ok(len.map(|len| (len, path)))
        });
        found.unwrap_or_else(|err| {
            unreadable_document(which.path(), err);
            None
        })
    }

    /// Report an administrator's layer that exists but could not be read.
    fn unreadable_document(path: &'static str, err: Errno) {
        log(
            &LogSink,
            &Event {
                level: Level::Warn,
                id: events::CONFIG_DOCUMENT_UNREADABLE,
                message: "configuration document could not be read; nothing delivered from it",
                fields: &[
                    Field {
                        key: "path",
                        value: tairix_log::FieldValue::Str(path),
                    },
                    Field {
                        key: "errno",
                        value: tairix_log::FieldValue::SignedInt(i64::from(err.as_i32())),
                    },
                ],
            },
        );
    }

    /// The production [`NetworkConfigSource`] backing: reads the stack-wide
    /// `net.*` policy from `system.conf` through the layered store
    /// ([`read_layered_config`]) and maps it through the one shared
    /// `lib/sysconfig` engine.
    ///
    /// It returns [`None`] on any failure — neither layer reachable, an
    /// absent, unreadable, or oversized document, or one the engine cannot
    /// parse — so delivery keeps the network stack on its safe defaults and
    /// retries at the next reaction, never guessing at a policy (fail
    /// closed).
    /// The production policy read, carrying the machine's RAM total once
    /// it has been answered.
    ///
    /// Installed RAM is a static hardware fact, so it is read once and
    /// kept rather than queried at every reaction. A `None` is "not
    /// answered yet" — the sysinfo broker may not be running this early —
    /// and is retried, because the capacity derived from a zero total is
    /// the smallest machine's rather than this machine's.
    #[derive(Default)]
    struct RtNetworkConfig {
        machine_ram_bytes: Option<u64>,
    }

    impl RtNetworkConfig {
        /// The machine's usable physical RAM, zero until the ungated
        /// System Information API answers.
        fn machine_ram_bytes(&mut self) -> u64 {
            if let Some(bytes) = self.machine_ram_bytes {
                return bytes;
            }
            match tairix_procinfo::memory_total_bytes(&tairix_procinfo::IpcTransport) {
                Ok(bytes) if bytes > 0 => {
                    self.machine_ram_bytes = Some(bytes);
                    bytes
                }
                _ => 0,
            }
        }
    }

    impl NetworkConfigSource for RtNetworkConfig {
        fn load(&mut self) -> Option<NetworkSettings> {
            // One byte past the engine's ceiling, so a document that fills
            // the buffer is proven over-long and refused rather than parsed
            // as a truncated prefix.
            #[allow(clippy::large_stack_arrays)]
            let mut buf = [0u8; tairix_sysconfig::MAX_CONFIG_LEN + 1];
            let (len, path) = read_layered_config(SystemConfigFile::System, &mut buf)?;
            let Some(config) = core::str::from_utf8(&buf[..len])
                .ok()
                .and_then(|text| tairix_sysconfig::SystemConfig::parse(text).ok())
            else {
                // The document read but did not parse: refuse it whole and
                // say so, or a hand edit would look like it had no effect.
                malformed_document(path);
                return None;
            };
            Some(config.network_settings(self.machine_ram_bytes()))
        }
    }

    /// Report a readable-but-unparseable configuration document, by the path
    /// of the layer read. Nothing is delivered from it; the caller keeps the
    /// safe defaults.
    fn malformed_document(path: &'static str) {
        log(
            &LogSink,
            &Event {
                level: Level::Warn,
                id: events::CONFIG_DOCUMENT_MALFORMED,
                message: "configuration document could not be parsed; nothing delivered from it",
                fields: &[Field {
                    key: "path",
                    value: tairix_log::FieldValue::Str(path),
                }],
            },
        );
    }

    /// The production [`NetworkInterfaceConfigSource`] backing: reads the
    /// per-interface configuration through the layered store
    /// ([`read_layered_config`]) and maps it through the one shared
    /// `lib/netconfig` projection
    /// ([`InterfaceConfigPlan::of`](tairix_netconfig::InterfaceConfigPlan::of)).
    ///
    /// Before the root unlock only the image's layer is reachable, over the
    /// store endpoint, so interfaces on the read-only volume the drivers
    /// autoload from are configured as they come up. It returns [`None`] on
    /// any failure — neither layer reachable, an absent, unreadable, or
    /// oversized document, or one the engine cannot parse or validate — so
    /// delivery leaves the interfaces unconfigured and retries at the next
    /// reaction, never guessing at a partial configuration (fail closed).
    struct RtNetworkInterfaceConfig;

    impl NetworkInterfaceConfigSource for RtNetworkInterfaceConfig {
        fn load(&mut self) -> Option<InterfaceConfigPlan> {
            #[allow(clippy::large_stack_arrays)]
            let mut buf = [0u8; tairix_netconfig::MAX_CONFIG_LEN + 1];
            let (len, path) = read_layered_config(SystemConfigFile::Network, &mut buf)?;
            let text = core::str::from_utf8(&buf[..len]).ok()?;
            // `parse` validates the whole document (including the bond and
            // static-addressing invariants), so a semantically inconsistent
            // store is refused whole here rather than delivered as a partial
            // guess (fail closed).
            let Ok(config) = tairix_netconfig::NetworkConfig::parse(text) else {
                malformed_document(path);
                return None;
            };
            Some(InterfaceConfigPlan::of(&config))
        }
    }

    /// Program entry point. Runs the reactive match-and-load loop for the
    /// life of the service (`budget = None`): fetch the catalogue once, then
    /// read the discovered tree, load a driver for every matched node, and
    /// block on every generation advance to re-match. The
    /// loop returns only on a fail-closed tree-seam error; PID 1 `init`
    /// supervises and relaunches the service.
    fn main() -> i32 {
        // The catalogue/load reply buffer. The tree snapshot is read into a
        // separate, service-owned buffer that grows to fit the discovered
        // tree (`tairix_devmgr::run`) — a real board's
        // firmware tree dwarfs QEMU `virt`'s, so a fixed stack buffer here
        // would be a scaling cliff. The stack sizing
        // (`spawn_layout::USER_STACK_PAGES`, ~1.1 MiB) covers this 64 KiB
        // reply buffer comfortably.
        #[allow(clippy::large_stack_arrays)]
        let mut reply_buf = [0u8; REPLY_BUF_LEN];
        let mut net_config_source = RtNetworkConfig::default();
        let mut tree = match RtTreeService::new() {
            Ok(tree) => tree,
            Err(err) => {
                log(
                    &LogSink,
                    &Event {
                        level: Level::Error,
                        id: events::TREE_SEAM_FAILED,
                        message: "the device manager's wait set could not be built; exiting for supervision",
                        fields: &[Field {
                            key: "error",
                            value: tairix_log::FieldValue::Error(err),
                        }],
                    },
                );
                return 1;
            }
        };
        match tairix_devmgr::run(
            &mut tree,
            &mut RtStoreCall,
            &mut RtNetstackBind,
            &mut RtAudiodBind,
            &mut RtAudioBaseline,
            &mut net_config_source,
            &mut RtNetworkInterfaceConfig,
            &LogSink,
            &mut reply_buf,
            None,
        ) {
            Ok(()) => 0,
            Err(err) => {
                // Fail loud: state the reason for the abnormal exit through
                // the kernel diagnostic log (this service has no terminal
                // consumer), carrying the errno so the refusing seam is
                // diagnosable, then exit non-zero for `init`'s supervision.
                let mut code = [0u8; 12];
                let errno = usize::try_from(err.as_i32()).unwrap_or(0);
                log(
                    &LogSink,
                    &Event {
                        level: Level::Error,
                        id: events::TREE_SEAM_FAILED,
                        message: "hardware-tree seam failed; devmgr exiting for supervision",
                        fields: &[Field {
                            key: "errno",
                            value: tairix_log::FieldValue::Str(format_usize(errno, &mut code)),
                        }],
                    },
                );
                1
            }
        }
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// Whenever the real freestanding `tairix-rt` `_start` path is not compiled
// — on the host (`cargo build --workspace`, clippy, fmt), or for a
// `program`-less build of this crate — this inert `main` keeps the crate
// building under the host tooling. It performs no I/O.
#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
