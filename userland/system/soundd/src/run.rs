//! The `Run` binary of the cue authority, installed at
//! `/System/Services/soundd.app/Run` and started by init; an inert stub on the
//! host.
//!
//! It serves `cue-v1` from one wait-set — the reserved rendezvous, the decode
//! worker's pipes, each sounding cue's stream mailbox, the memory-pressure
//! band and the seat's display lease — parking until one of them, or the
//! authority's next deadline, wakes it. Once bound it publishes its own
//! instance, which is where a session hands its chosen files. The same binary
//! is its own decode worker, spawned in the sandbox's session role.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::vec::Vec;

    use tairix_abi::audio::{AudioGain, AudioNotify, OpenParams};
    use tairix_abi::driver::audio::Frames;
    use tairix_abi::fs::{FileKind, FileStat, OpenFlags};
    use tairix_abi::notice::{Notice, NoticeTopic, NOTICE_PAYLOAD_MAX};
    use tairix_abi::sound_cue::{CUE_REPLY_LEN, CUE_REQUEST_MAX, SOUND_CUE_ENDPOINT};
    use tairix_abi::{Errno, ProcId, WaitSetOp, WaitSourceKind};
    use tairix_audio::live::{LiveStream, RtAudio};
    use tairix_audio::stream::OpenFailure;
    use tairix_caps::CapabilitySet;
    use tairix_hash::BuildSipHash13;
    use tairix_log::{Event, EventId, Level};
    use tairix_procinfo::IpcTransport;
    use tairix_rt::{File, LogSink, ServedCall};
    use tairix_sandbox::audiodecode::{session_bounds, AudioDecodeService};
    use tairix_sandbox::rt::{
        draw_seed, serve_session_stdio, session_worker_role, RtSessionLauncher, SessionMembers,
    };
    use tairix_sandbox::supervise::SupervisedSession;
    use tairix_soundd::bounds::MAX_CONCURRENT_CUES;
    use tairix_soundd::events::{SERVICE_UNAVAILABLE, SOUNDD_READY};
    use tairix_soundd::service::Reply;
    use tairix_soundd::{cue_cache, CueAuthority, Decoder, FileIdentity, SoundStore, Speakers};

    /// Calls the rendezvous holds: every cue sounding may be waiting on its
    /// answer, with as many again arriving. A fail-closed bound — past it the
    /// kernel refuses the post.
    const CAPACITY: usize = 4 * MAX_CONCURRENT_CUES;

    const CUE_TOKEN: u64 = 1;
    const DECODER_READ: u64 = 2;
    const DECODER_WRITE: u64 = 3;
    const PRESSURE_TOKEN: u64 = 4;
    const LEASE_TOKEN: u64 = 5;
    // A stream's mailbox is its own token: every one carries the audio
    // service's high tag, so none collides with the five above.

    static LOG_SINK: LogSink = LogSink;

    type Authority = CueAuthority<RtStore, RtSpeakers, RtSessionLauncher, LogSink>;

    fn record(id: EventId, level: Level, message: &str) {
        let _ = tairix_log::log(
            &LOG_SINK,
            &Event {
                level,
                id,
                message,
                fields: &[],
            },
        );
    }

    /// The volume, read under the service's own identity, and the files a
    /// session handed over, read under its.
    struct RtStore;

    fn identity(stat: &FileStat) -> FileIdentity {
        FileIdentity {
            id: stat.id,
            content_gen: stat.content_gen,
            len: stat.size,
            modified: stat.times.modified,
        }
    }

    /// The whole of `file`, provided it is still the regular file `expected`
    /// names and no longer than `max`.
    fn read_whole(file: &File, expected: FileIdentity, max: usize) -> Result<Vec<u8>, Errno> {
        let stat = file.stat().map_err(Errno::from_syscall)?;
        if stat.kind != FileKind::Regular || identity(&stat) != expected {
            return Err(Errno::Stale);
        }
        let len = usize::try_from(stat.size)
            .ok()
            .filter(|&len| len <= max)
            .ok_or(Errno::LengthOutOfRange)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| Errno::OutOfMemory)?;
        bytes.resize(len, 0);
        let read = file.read_at(0, &mut bytes).map_err(Errno::from_syscall)?;
        bytes.truncate(read);
        Ok(bytes)
    }

    impl SoundStore for RtStore {
        type Handed = File;

        fn identify(&mut self, path: &str) -> Result<Option<FileIdentity>, Errno> {
            let file = match File::open(path.as_bytes(), OpenFlags::READ) {
                Ok(file) => file,
                Err(raw) => {
                    return match Errno::from_syscall(raw) {
                        Errno::NotFound => Ok(None),
                        err => Err(err),
                    };
                }
            };
            let stat = file.stat().map_err(Errno::from_syscall)?;
            Ok((stat.kind == FileKind::Regular).then(|| identity(&stat)))
        }

        fn read(
            &mut self,
            path: &str,
            expected: FileIdentity,
            max: usize,
        ) -> Result<Vec<u8>, Errno> {
            let file = File::open(path.as_bytes(), OpenFlags::READ).map_err(Errno::from_syscall)?;
            read_whole(&file, expected, max)
        }

        fn redeem(&mut self, handle: u64, grantor: ProcId) -> Result<File, Errno> {
            File::from_delegation_by(handle, grantor).map_err(Errno::from_syscall)
        }

        fn identify_handed(&mut self, file: &File) -> Result<FileIdentity, Errno> {
            let stat = file.stat().map_err(Errno::from_syscall)?;
            if stat.kind != FileKind::Regular {
                return Err(Errno::OutOfRange);
            }
            Ok(identity(&stat))
        }

        fn read_handed(
            &mut self,
            file: &File,
            expected: FileIdentity,
            max: usize,
        ) -> Result<Vec<u8>, Errno> {
            read_whole(file, expected, max)
        }
    }

    /// The audio service, one live stream per sounding cue over one
    /// transport.
    struct RtSpeakers {
        transport: RtAudio,
    }

    impl Speakers for RtSpeakers {
        type Stream = LiveStream;

        fn open(&mut self, params: &OpenParams) -> Result<LiveStream, Errno> {
            LiveStream::open(&mut self.transport, params).map_err(|failure| match failure {
                OpenFailure::Refused(err) | OpenFailure::Notify(err) | OpenFailure::Ring(err) => {
                    err
                }
            })
        }

        fn mailbox(&self, stream: &LiveStream) -> u64 {
            stream.notify_port()
        }

        fn write(
            &mut self,
            stream: &mut LiveStream,
            at: Frames,
            samples: &[u8],
        ) -> Result<u32, Errno> {
            stream
                .write_at(at, samples)
                .map(|written| written.sample_frames)
        }

        fn set_gain(&mut self, stream: &mut LiveStream, gain: AudioGain) -> Result<(), Errno> {
            stream.client_mut().set_gain(&mut self.transport, gain)
        }

        fn start(&mut self, stream: &mut LiveStream) -> Result<(), Errno> {
            stream.client_mut().start(&mut self.transport, Frames::ZERO)
        }

        fn drain(&mut self, stream: &mut LiveStream) -> Result<(), Errno> {
            stream.client_mut().drain(&mut self.transport)
        }

        fn next_notify(&mut self, stream: &mut LiveStream) -> Result<Option<AudioNotify>, Errno> {
            stream.take_notify(&mut self.transport)
        }

        fn close(&mut self, stream: LiveStream) {
            let _ = stream.close(&mut self.transport);
        }
    }

    /// Add or remove one member of the serve wait-set.
    fn watch(set: u64, op: WaitSetOp, kind: WaitSourceKind, id: u64, token: u64) -> bool {
        tairix_rt::waitset_ctl(set, op, kind, id, token) == 0
    }

    /// The authority's sources on the wait-set: the decoder's pipes in the
    /// directions it wants, and every sounding cue's mailbox.
    struct Members {
        decoder: SessionMembers,
        mailboxes: Vec<u64>,
    }

    impl Members {
        /// Bring `set` in line with `authority`, answering whether it could
        /// be: a source nobody watches would leave a cue waiting for ever.
        fn sync(&mut self, set: u64, authority: &Authority) -> bool {
            let (descriptors, read, write) = authority.decoder_members();
            if self.decoder.sync(descriptors, read, write).is_err() {
                return false;
            }
            let mut synced = true;
            self.mailboxes.retain(|&mailbox| {
                let live = authority.mailboxes().any(|held| held == mailbox);
                if !live {
                    synced &= watch(set, WaitSetOp::Del, WaitSourceKind::Port, mailbox, mailbox);
                }
                live
            });
            for mailbox in authority.mailboxes() {
                if self.mailboxes.contains(&mailbox) {
                    continue;
                }
                if self.mailboxes.try_reserve(1).is_err()
                    || !watch(set, WaitSetOp::Add, WaitSourceKind::Port, mailbox, mailbox)
                {
                    return false;
                }
                self.mailboxes.push(mailbox);
            }
            synced
        }
    }

    /// The login session the seat's display lease names, or `None` while no
    /// session's process holds it — and, failing closed, when the lease cannot
    /// be read.
    fn seat() -> Option<ProcId> {
        let mut buf = [0u8; NOTICE_PAYLOAD_MAX];
        let read =
            usize::try_from(tairix_rt::notice_read(NoticeTopic::DisplayLease, &mut buf)).ok()?;
        match Notice::decode(NoticeTopic::DisplayLease, buf.get(..read)?) {
            Ok(Notice::DisplayLease(lease)) => lease.session(),
            _ => None,
        }
    }

    /// Say which process this is, so a session can hand it the files its user
    /// chose. One that cannot is still the machine's cue authority; only a
    /// user's own files have nowhere to go, which it states.
    fn announce() {
        let published = tairix_rt::self_origin().is_ok_and(|origin| {
            tairix_rt::notice_publish(&Notice::CueAuthority {
                instance: origin.proc_id(),
            }) == 0
        });
        if !published {
            record(
                SERVICE_UNAVAILABLE,
                Level::Warn,
                "soundd: cannot say which process it is; no session can hand it a file",
            );
        }
    }

    /// Bind the rendezvous and a wait-set watching it, the memory band and the
    /// seat's lease.
    fn bind() -> Option<u64> {
        let empty = CapabilitySet::empty();
        // Any program may ask for a sound; who may have which is decided
        // per request from its attested origin.
        if tairix_rt::call_create(
            SOUND_CUE_ENDPOINT,
            &empty,
            &empty,
            CUE_REQUEST_MAX,
            CUE_REPLY_LEN,
            CAPACITY,
        ) != 0
        {
            record(
                SERVICE_UNAVAILABLE,
                Level::Warn,
                "soundd: cannot bind the cue rendezvous",
            );
            return None;
        }
        let set = u64::try_from(tairix_rt::waitset_create()).ok()?;
        if !watch(
            set,
            WaitSetOp::Add,
            WaitSourceKind::Endpoint,
            SOUND_CUE_ENDPOINT,
            CUE_TOKEN,
        ) || !tairix_procinfo::pressure::watch(set, PRESSURE_TOKEN)
            // Readable only by the holder of the rendezvous just bound.
            || !watch(
                set,
                WaitSetOp::Add,
                WaitSourceKind::SystemNotice,
                u64::from(NoticeTopic::DisplayLease.as_u32()),
                LEASE_TOKEN,
            )
        {
            record(
                SERVICE_UNAVAILABLE,
                Level::Warn,
                "soundd: cannot watch the cue rendezvous",
            );
            return None;
        }
        Some(set)
    }

    /// Answer every call the authority has finished with.
    fn send_replies(authority: &mut Authority) {
        for (ticket, reply) in authority.take_replies() {
            let _ = tairix_rt::call_reply(SOUND_CUE_ENDPOINT, ticket, &reply);
        }
    }

    /// Take the call waiting on the rendezvous and answer it, now or later,
    /// while `seat` holds the seat.
    fn serve_cue(authority: &mut Authority, now: u64, seat: Option<ProcId>) {
        let mut request = [0u8; CUE_REQUEST_MAX];
        let Ok(Some(ServedCall { ticket, len })) =
            tairix_rt::call_recv_ready(SOUND_CUE_ENDPOINT, &mut request)
        else {
            return;
        };
        let Ok(caller) = tairix_rt::peer_origin(SOUND_CUE_ENDPOINT, ticket) else {
            let refused = Reply::refusal(&request[..len], Errno::PermissionDenied);
            let _ = tairix_rt::call_reply(SOUND_CUE_ENDPOINT, ticket, refused.as_bytes());
            return;
        };
        if let Some(reply) = authority.serve(now, &caller, ticket, &request[..len], seat) {
            let _ = tairix_rt::call_reply(SOUND_CUE_ENDPOINT, ticket, reply.as_bytes());
        }
    }

    fn serve(authority: &mut Authority, set: u64) -> i32 {
        let mut members = Members {
            decoder: SessionMembers::new(set, DECODER_READ, DECODER_WRITE),
            mailboxes: Vec::new(),
        };
        loop {
            if !members.sync(set, authority) {
                record(
                    SERVICE_UNAVAILABLE,
                    Level::Warn,
                    "soundd: a source could not be watched",
                );
                return 1;
            }
            let now = tairix_rt::clock_get();
            let timeout = authority
                .deadline()
                .map_or(u64::MAX, |at| at.saturating_sub(now));
            let mut token = 0;
            let waited = tairix_rt::waitset_wait(set, timeout, &mut token);
            let now = tairix_rt::clock_get();
            if waited != 0 && Errno::from_syscall(waited) != Errno::TimedOut {
                // A dead wait-set would turn the loop into a busy poll: exit
                // and let init restart the service.
                record(
                    SERVICE_UNAVAILABLE,
                    Level::Warn,
                    "soundd: the serve wait-set failed",
                );
                return 1;
            }
            match (waited == 0).then_some(token) {
                Some(PRESSURE_TOKEN) => {
                    if tairix_procinfo::pressure::refresh() {
                        authority.trim();
                    }
                }
                Some(mailbox)
                    if !matches!(
                        mailbox,
                        CUE_TOKEN | DECODER_READ | DECODER_WRITE | LEASE_TOKEN
                    ) =>
                {
                    authority.on_stream(now, mailbox);
                }
                woken => {
                    // The seat as it is now governs what this turn decides, so
                    // a decode that lands beside a lease move sounds for the
                    // seat's new holder.
                    let holder = seat();
                    authority.seat_moved(now, holder);
                    match woken {
                        Some(CUE_TOKEN) => serve_cue(authority, now, holder),
                        Some(DECODER_READ) => authority.on_decoder(now, true, false),
                        Some(DECODER_WRITE) => authority.on_decoder(now, false, true),
                        _ => authority.on_timer(now),
                    }
                }
            }
            send_replies(authority);
        }
    }

    fn main() -> i32 {
        if session_worker_role() {
            serve_session_stdio(&mut AudioDecodeService::new());
            return 0;
        }
        let Ok(bounds) = session_bounds() else {
            record(
                SERVICE_UNAVAILABLE,
                Level::Warn,
                "soundd: the decode session has no bounds",
            );
            return 1;
        };
        let decoder = Decoder::new(
            SupervisedSession::new(RtSessionLauncher::own_binary(), bounds, LogSink),
            draw_seed,
        );
        // A machine that cannot key the cache's index, or say how much RAM
        // it has, keeps nothing cached rather than guessing.
        let key = tairix_rt::hash_seed();
        let total_ram = match key {
            Some(_) => tairix_procinfo::memory_total_bytes(&IpcTransport).unwrap_or(0),
            None => 0,
        };
        let cache = cue_cache(
            total_ram,
            tairix_rt::pressure::gauge(),
            &LOG_SINK,
            BuildSipHash13::with_seed(key.unwrap_or(tairix_hash::HashSeed::UNKEYED)),
        );
        let mut authority = CueAuthority::new(
            RtStore,
            RtSpeakers {
                transport: RtAudio::new(),
            },
            decoder,
            cache,
            LogSink,
        );
        let Some(set) = bind() else {
            return 1;
        };
        announce();
        record(SOUNDD_READY, Level::Info, "soundd: serving cues");
        if tairix_rt::servicenotice::announce_ready().is_err() {
            record(
                SERVICE_UNAVAILABLE,
                Level::Warn,
                "soundd: the service manager did not record this readiness",
            );
        }
        serve(&mut authority, set)
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
