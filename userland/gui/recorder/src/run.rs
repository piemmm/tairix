//! The `Run` entry point of `recorder.app`, the screen-recording component
//! only the desktop session starts (`plans/CAPTURE.md`).
//!
//! Its standard input and output are its pipes to the session. It parks in
//! one wait-set on them, on its sound streams' mailboxes and on the encoding
//! thread's wake, and serves the `tairix_recorder` core from each wake; it
//! never polls. A slot is a region the session granted this process and the
//! output a file it delegated, each named by the session's `Start`, and the
//! sound is the audio service's capture streams. Every abnormal end is stated
//! on standard error as well as in the log.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy and
//! fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![cfg_attr(freestanding, feature(allocator_api))]
#![deny(missing_docs)]

#[cfg(freestanding)]
extern crate alloc;

#[cfg(freestanding)]
mod program {
    use alloc::sync::Arc;

    use tairix_abi::audio::{
        AudioNotify, CaptureSource, ClockReport, OpenParams, StreamRole, StreamState, Tenancy,
    };
    use tairix_abi::driver::audio::{ChannelMap, Frames, SampleFormat, StreamDirection};
    use tairix_abi::recorder_ipc::RECORDER_SLOTS;
    use tairix_abi::{
        Errno, LimitKind, ProcId, WaitSetOp, WaitSourceKind, RLIMIT_INFINITY, STDIN, STDOUT,
    };
    use tairix_audio::live::{LiveStream, RtAudio};
    use tairix_audio::stream::OpenFailure;
    use tairix_fscopy::Target;
    use tairix_log::{Event, Field, FieldValue, Level};
    use tairix_parallel::{JobRunner, Pool};
    use tairix_recorder::sound::{PERIOD, RATE};
    use tairix_recorder::{
        carry_out, client_of, Answer, Desk, Encoding, Exit, Host, Job, News, Output, Recorder,
        SlotMemory, SoundSource, EXIT_ENDED, FEW_ENCODING_CORES, NO_ENCODING_THREAD,
        RECORDER_ENDED,
    };
    use tairix_rt::shm::MappedGrant;
    use tairix_rt::sync::WorkerWake;
    use tairix_rt::work::{NoWorker, Worker, WorkerGuard};
    use tairix_rt::{File, LogSink, WaitSet};
    use tairix_sandbox::rt::SessionMembers;
    use tairix_sandbox::{SessionDescriptors, SessionTransport};
    use tairix_util::defer::JobQueue;

    /// The wait-set token of the session's pipe in.
    const SESSION_READ: u64 = 1;
    /// The wait-set token of the room in the session's pipe out.
    const SESSION_WRITE: u64 = 2;
    /// The wait-set token of the encoding thread's wake.
    const ENCODED: u64 = 3;
    /// The wait-set token of the sound streams' mailboxes.
    const HEARD: u64 = 4;

    /// The jobs the encoding thread holds at most: a frame a slot, a batch of
    /// packets, and the finish.
    const ENCODE_JOBS: usize = RECORDER_SLOTS + 2;

    /// The name standard error speaks under.
    const NAME: &str = "recorder";

    type EncodeWorker = Worker<Encoding<Out, Slot, Pool>, Job, Answer, JobQueue<Job, Answer>>;

    /// The session's pipes: standard input from it, standard output to it.
    struct Pipes;

    impl SessionTransport for Pipes {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
            tairix_rt::fs_read(STDIN, 0, buf).map_err(Errno::from_syscall)
        }

        fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
            tairix_rt::fs_write(STDOUT, 0, buf).map_err(Errno::from_syscall)
        }

        fn descriptors(&self) -> Option<SessionDescriptors> {
            Some(SessionDescriptors {
                read_fd: STDIN,
                write_fd: STDOUT,
            })
        }

        // The far end is the parent, never this process's to reap.
        fn dispose(self) -> Option<i32> {
            None
        }
    }

    /// The output the session delegated.
    struct Out(File);

    impl Target for Out {
        fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
            Target::write_all_at(&mut self.0, offset, bytes)
        }

        fn set_len(&mut self, len: u64) -> Result<(), Errno> {
            Target::set_len(&mut self.0, len)
        }
    }

    impl Output for Out {
        fn regular_len(&self) -> Result<Option<u64>, Errno> {
            self.0.regular_len()
        }

        fn sync(&mut self) -> Result<(), Errno> {
            self.0.sync().map_err(Errno::from_syscall)
        }
    }

    /// A slot: a region the session made and granted this process.
    struct Slot(MappedGrant);

    impl SlotMemory for Slot {
        fn bytes(&self) -> &[u8] {
            self.0.bytes()
        }
    }

    /// A capture stream of the audio service's, its mailbox watched by the
    /// service thread's wait-set.
    struct RtSound {
        audio: RtAudio,
        stream: Option<LiveStream>,
        set: u64,
    }

    impl RtSound {
        /// Open `source` in `layout`, F32 at the recording's rate.
        fn open(set: u64, source: CaptureSource, layout: ChannelMap) -> Result<Self, Errno> {
            let params = OpenParams {
                device_id: source.device_id()?,
                direction: StreamDirection::Capture,
                format: SampleFormat::F32,
                rate: RATE,
                channel_map: layout,
                role: StreamRole::Media,
                latency_target_frames: u32::try_from(PERIOD).unwrap_or(u32::MAX),
                tenancy: Tenancy::Caller,
            };
            let mut audio = RtAudio::new();
            let stream = LiveStream::open(&mut audio, &params).map_err(OpenFailure::errno)?;
            let grant = stream.client().grant();
            let port = stream.notify_port();
            let sound = Self {
                audio,
                stream: Some(stream),
                set,
            };
            // Frames in any other form than asked would be mixed as noise.
            if grant.format != SampleFormat::F32
                || grant.rate != RATE
                || grant.channel_map != layout
            {
                return Err(Errno::NotSupported);
            }
            match tairix_rt::waitset_ctl(set, WaitSetOp::Add, WaitSourceKind::Port, port, HEARD) {
                0 => Ok(sound),
                refused => Err(Errno::from_syscall(refused)),
            }
        }
    }

    impl SoundSource for RtSound {
        fn start(&mut self) -> Result<(), Errno> {
            let stream = self.stream.as_mut().ok_or(Errno::NotConnected)?;
            stream.client_mut().start(&mut self.audio, Frames::ZERO)
        }

        fn read(&mut self, out: &mut [u8]) -> Result<usize, Errno> {
            let stream = self.stream.as_mut().ok_or(Errno::NotConnected)?;
            let frames = stream.ring()?.read(out)?;
            usize::try_from(frames).map_err(|_| Errno::OutOfRange)
        }

        fn take_news(&mut self) -> Result<News, Errno> {
            let stream = self.stream.as_mut().ok_or(Errno::NotConnected)?;
            let id = stream.client().grant().stream_id;
            let mut news = News::default();
            while let Some(notify) = stream.take_notify(&mut self.audio)? {
                match notify {
                    AudioNotify::Xrun {
                        stream_id,
                        lost_frames,
                        ..
                    } if stream_id == id => {
                        news.lost_frames = news.lost_frames.saturating_add(lost_frames);
                    }
                    AudioNotify::StateChanged {
                        stream_id,
                        state: StreamState::DeviceLost | StreamState::Faulted,
                        ..
                    } if stream_id == id => news.gone = true,
                    _ => {}
                }
            }
            Ok(news)
        }

        fn clock(&mut self) -> Result<ClockReport, Errno> {
            let stream = self.stream.as_ref().ok_or(Errno::NotConnected)?;
            stream.client().clock(&mut self.audio)
        }
    }

    impl Drop for RtSound {
        fn drop(&mut self) {
            if let Some(stream) = self.stream.take() {
                let port = stream.notify_port();
                let _ = tairix_rt::waitset_ctl(
                    self.set,
                    WaitSetOp::Del,
                    WaitSourceKind::Port,
                    port,
                    HEARD,
                );
                let _ = stream.close(&mut self.audio);
            }
        }
    }

    /// The encoding thread's desk.
    struct RtDesk {
        worker: Arc<EncodeWorker>,
        _guard: WorkerGuard<Encoding<Out, Slot, Pool>, Job, Answer, JobQueue<Job, Answer>>,
    }

    impl Desk for RtDesk {
        fn submit(&mut self, job: Job) -> Result<(), Errno> {
            self.worker
                .submit(job)
                .map(drop)
                .map_err(|_| Errno::LimitExceeded)
        }

        fn collect(&mut self) -> Option<Answer> {
            self.worker.collect()
        }
    }

    /// The runtime the recorder reaches the machine through.
    struct RtHost {
        set: u64,
        /// The session its spawner named, whose grants alone its slots are.
        session: ProcId,
        /// The encoding thread once it has work: what its wake is drained by.
        encoder: Option<Arc<EncodeWorker>>,
    }

    impl Host for RtHost {
        type Output = Out;
        type Slot = Slot;
        type Sound = RtSound;
        type Desk = RtDesk;
        type Runner = Pool;

        fn now_ns(&self) -> u64 {
            tairix_rt::clock_get()
        }

        fn memory_room(&self) -> u64 {
            tairix_rt::rlimit_get(LimitKind::AddressSpaceBytes)
                .map_or(RLIMIT_INFINITY, |limit| limit.soft)
        }

        fn open_output(&mut self, handle: u64) -> Result<Out, Errno> {
            File::from_delegation(handle)
                .map(Out)
                .map_err(Errno::from_syscall)
        }

        fn map_slot(&mut self, handle: u64, len: usize) -> Result<Slot, Errno> {
            MappedGrant::map(self.session, handle, len).map(Slot)
        }

        fn open_sound(
            &mut self,
            source: CaptureSource,
            layout: ChannelMap,
        ) -> Result<RtSound, Errno> {
            RtSound::open(self.set, source, layout)
        }

        fn runner(&mut self) -> Pool {
            let online = tairix_procinfo::online_cpus(&tairix_procinfo::IpcTransport);
            let pool = Pool::for_cpus(online);
            let cores =
                |count: usize| FieldValue::UnsignedInt(u64::try_from(count).unwrap_or(u64::MAX));
            if pool.worker_count() < online.saturating_sub(1) {
                let _ = tairix_log::log(
                    &LogSink,
                    &Event {
                        level: Level::Warn,
                        id: FEW_ENCODING_CORES,
                        message: "frames are coded on fewer cores than the machine has",
                        fields: &[
                            Field {
                                key: "cores",
                                value: cores(JobRunner::width(&pool)),
                            },
                            Field {
                                key: "online",
                                value: cores(online),
                            },
                        ],
                    },
                );
            }
            pool
        }

        fn start_encoding(&mut self, encoding: Encoding<Out, Slot, Pool>) -> Result<RtDesk, Errno> {
            let worker =
                EncodeWorker::queued(carry_out, encoding, WorkerWake::create(), ENCODE_JOBS)
                    .map_err(|_| Errno::OutOfMemory)?;
            let worker = Arc::try_new(worker).map_err(|_| Errno::OutOfMemory)?;
            let guard = WorkerGuard::new(&worker);
            start(self.set, &worker);
            self.encoder = Some(Arc::clone(&worker));
            Ok(RtDesk {
                worker,
                _guard: guard,
            })
        }
    }

    /// Start the encoding thread, its wake watched by wait-set `set`. One the
    /// kernel refuses leaves its work to the service thread: slower, never
    /// wrong.
    fn start(set: u64, worker: &Arc<EncodeWorker>) {
        let watched = worker.wake().read_end().is_some_and(|read| {
            tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Add,
                WaitSourceKind::Stream,
                u64::from(read),
                ENCODED,
            ) == 0
        });
        let refused = if watched {
            match Worker::start(worker) {
                Ok(_) => return,
                Err(NoWorker::Wake) => Errno::NotConnected,
                Err(NoWorker::Thread(errno)) => errno,
            }
        } else {
            worker.stop();
            Errno::NotConnected
        };
        let _ = tairix_log::log(
            &LogSink,
            &Event {
                level: Level::Warn,
                id: NO_ENCODING_THREAD,
                message: "no encoding thread; its work runs on the service thread",
                fields: &[Field {
                    key: "error",
                    value: FieldValue::Error(refused),
                }],
            },
        );
    }

    /// State `exit`'s reason, where it has one, and answer its status.
    fn finish(exit: Exit) -> i32 {
        if let Some(reason) = exit.reason {
            tairix_rt::io::write_stderr_line(format_args!("{NAME}: {reason}"));
        }
        exit.code
    }

    /// End for want of `what`, stated on standard error and in the log.
    fn fail(what: &str, errno: Errno) -> i32 {
        tairix_rt::io::write_stderr_line(format_args!("{NAME}: {what}: {errno}"));
        let _ = tairix_log::log(
            &LogSink,
            &Event {
                level: Level::Error,
                id: RECORDER_ENDED,
                message: what,
                fields: &[Field {
                    key: "error",
                    value: FieldValue::Error(errno),
                }],
            },
        );
        EXIT_ENDED
    }

    /// Program entry point.
    fn main() -> i32 {
        let Some(session) = tairix_rt::args().as_deref().and_then(client_of) else {
            return fail("no session was named to record for", Errno::NotFound);
        };
        let set = match WaitSet::create() {
            Ok(set) => set,
            Err(errno) => return fail("no wait-set could be had", errno),
        };
        let mut host = RtHost {
            set: set.handle(),
            session,
            encoder: None,
        };
        let Ok(mut recorder) = Recorder::new(Pipes, LogSink) else {
            return fail("no room for the session's pipes", Errno::OutOfMemory);
        };
        let mut members = SessionMembers::new(set.handle(), SESSION_READ, SESSION_WRITE);
        loop {
            if let Some(exit) = recorder.serve(&mut host) {
                return finish(exit);
            }
            let (descriptors, read, write) = (
                recorder.descriptors(),
                recorder.wants_read(),
                recorder.wants_write(),
            );
            if let Err(errno) = members.sync(descriptors, read, write) {
                return fail("its pipes could not be watched", errno);
            }
            match set.wait_until(u64::MAX) {
                Ok(Some(SESSION_READ)) => recorder.on_readable(),
                Ok(Some(SESSION_WRITE)) => recorder.on_writable(),
                Ok(Some(ENCODED)) => {
                    if let Some(encoder) = host.encoder.as_ref() {
                        encoder.wake().drain();
                    }
                }
                Ok(_) => {}
                Err(errno) => return fail("its wait failed", errno),
            }
        }
    }

    tairix_rt::entry!(main);
}

// The host's stand-in for the freestanding entry point: it performs no I/O.
#[cfg(not(freestanding))]
fn main() {}
