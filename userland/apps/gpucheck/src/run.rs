//! The `Run` entry-point binary of the `gpucheck` tool.
//!
//! It lists the GPUs drivers serve, runs [`tairix_gpucheck::work`] on each
//! and on the software device, and states whether they agree; asked
//! `--present`, shows [`tairix_gpucheck::Scene`] full screen through a
//! swapchain and states whether every frame it presented was shown; asked
//! `--shader`, runs the kernel a file holds the same way, WGSL compiled in a
//! sandbox worker that is this binary in its worker role. Every refusal
//! states its reason, and the step it met, on `stderr`.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
extern crate alloc;

#[cfg(all(freestanding, feature = "program"))]
mod program {
    use alloc::format;
    use alloc::sync::Arc;
    use alloc::vec::Vec;

    use tairix_abi::gpu::GpuDeviceDescriptor;
    use tairix_gpu::Device;
    use tairix_gpucheck::{command, compare, reference, work, Command, Difference, USAGE};
    use tairix_procinfo::{for_each_gpu_device, IpcTransport, WalkStep};
    use tairix_rt::io::{write_stderr_line, Stdout, Write};

    pub(crate) const NAME: &str = "gpucheck";

    /// The region each GPU's bytes cross through: room for the work's
    /// largest transfer and its largest module, which crosses whole.
    pub(crate) const REGION: usize = 1 << 20;

    pub(crate) fn fail(reason: &str) -> i32 {
        write_stderr_line(format!("{NAME}: {reason}"));
        1
    }

    pub(crate) fn say(line: &str) -> bool {
        Stdout.write_all(line.as_bytes()).is_ok() && Stdout.write_all(b"\n").is_ok()
    }

    /// The GPUs drivers serve, or `None` where they could not be listed.
    pub(crate) fn served() -> Option<Vec<GpuDeviceDescriptor>> {
        let mut served = Vec::new();
        for_each_gpu_device(&IpcTransport, |device| {
            served
                .try_reserve(1)
                .map_err(|_| tairix_abi::Errno::OutOfMemory)?;
            served.push(*device);
            Ok(WalkStep::Continue)
        })
        .ok()?;
        Some(served)
    }

    /// Program entry point: what the command line asks, its exit status.
    fn main() -> i32 {
        // This binary re-spawned as its own sandbox worker compiles WGSL and
        // does nothing else.
        if tairix_sandbox::rt::worker_role() {
            let mut service = tairix_sandbox::wgsl::WgslService;
            return tairix_sandbox::rt::serve_stdio(&mut service).exit_code();
        }
        let Some(arguments) = tairix_help::arguments(NAME) else {
            return tairix_help::MISUSE;
        };
        match command(&arguments) {
            Ok(Command::Check) => check(),
            Ok(Command::Present) => crate::present::run(),
            Ok(Command::Shader { file, words }) => crate::kernel::run(file, words),
            Ok(Command::Help) => tairix_help::print_own_short_help(NAME, Some(USAGE)),
            Err(err) => {
                tairix_help::report_misuse(NAME, &err);
                tairix_help::MISUSE
            }
        }
    }

    /// `0` once every served GPU agreed with the software device, `1` where
    /// one differed, none was served, or they could not be listed.
    fn check() -> i32 {
        let Some(served) = served() else {
            return fail("the served GPUs could not be listed");
        };
        if served.is_empty() {
            return fail("no GPU is served");
        }
        let Ok(reference) = reference(Arc::new(tairix_rt::ClockDelay)) else {
            return fail("the software device could not be made");
        };
        let expected = match work(&reference) {
            Ok(expected) => expected,
            Err(refused) => return fail(&format!("the software device refused {refused}")),
        };
        drop(reference);
        let mut agreed = true;
        for gpu in &served {
            let line = match Device::connect(gpu.endpoint, REGION) {
                Err(why) => {
                    agreed = false;
                    format!("gpu {}: could not be reached: {why}", gpu.node)
                }
                Ok(device) => match work(&device)
                    .map_err(Difference::Refused)
                    .and_then(|answered| compare(&answered, &expected))
                {
                    Ok(()) => format!("gpu {}: agrees with the software device", gpu.node),
                    Err(difference) => {
                        agreed = false;
                        format!("gpu {}: {difference}", gpu.node)
                    }
                },
            };
            if !say(&line) {
                return fail("standard output refused the answer");
            }
        }
        i32::from(!agreed)
    }

    tairix_rt::entry!(main);
}

/// `gpucheck --present`: the scene shown full screen through a swapchain,
/// paced by each frame's fate, until `Escape` or a close.
#[cfg(all(freestanding, feature = "program"))]
mod present {
    use alloc::format;
    use alloc::rc::Rc;
    use alloc::string::String;
    use alloc::sync::Arc;
    use core::cell::Cell;

    use tairix_abi::driver::display::{DamageList, DamageRect, DisplayFormat};
    use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode};
    use tairix_abi::window_ipc::{WindowEvent, WindowSizeState, WindowSizing};
    use tairix_abi::Errno;
    use tairix_gpu::{Context, ContextDesc, Device, Queue, QueueCapabilities, Submission};
    use tairix_gpucheck::{family, memory_for, software, Scene};
    use tairix_window::app::{self, RtWindowTransport, Wake};
    use tairix_window::swapchain::{PresentMode, Refused, Swapchain, Waits, WindowSpec};
    use tairix_window::{
        EventDrain, EventError, EventMailbox, EventSource, Parked, WindowClient, WindowEvents,
    };

    use crate::program::{fail, say, served, NAME, REGION};

    /// The token the swapchain's fill wake fires under.
    const FILL_TOKEN: u64 = app::FIRST_APP_TOKEN;
    /// The window's title.
    const TITLE: &str = "GPU check";
    /// The window's frames' encoding.
    const FRAMES: DisplayFormat = DisplayFormat::Bgra8888;
    /// The window manager may make anything of the window's size.
    const ANY_SIZE: WindowSizing = WindowSizing::Resizable {
        min_width_px: 0,
        min_height_px: 0,
        max_width_px: 0,
        max_height_px: 0,
    };

    type Client = WindowClient<RtWindowTransport>;
    type Chain = Swapchain<tairix_rt::shm::SharedRegion>;

    /// The park: the event mailbox, and the swapchain's fill wake, which ends
    /// the wait for the loop to take the fill in.
    struct Source {
        mailbox: EventMailbox,
        set: u64,
        /// Set when the wait reported the fill wake, cleared when the loop
        /// takes it.
        filled: Rc<Cell<bool>>,
    }

    impl EventDrain for Source {
        fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
            self.mailbox.try_next(event)
        }
    }

    impl EventSource for Source {
        fn park(&mut self) -> Result<Parked, Errno> {
            match app::park(self.set)? {
                Wake::App(FILL_TOKEN) => {
                    self.filled.set(true);
                    Ok(Parked::Interrupted)
                }
                Wake::Event
                | Wake::App(_)
                | Wake::PressureChanged
                | Wake::PressureUnchanged
                | Wake::DesktopChanged => Ok(Parked::Served),
            }
        }
    }

    /// The device the scene is drawn on and what it is called: the first GPU
    /// a driver serves, else the software device, sized for `screen`.
    fn device(screen: (u32, u32)) -> Result<(Device, String), String> {
        let served = served().ok_or_else(|| String::from("the served GPUs could not be listed"))?;
        if let Some(gpu) = served.first() {
            let device = Device::connect(gpu.endpoint, REGION)
                .map_err(|why| format!("gpu {} could not be reached: {why}", gpu.node))?;
            return Ok((device, format!("gpu {}", gpu.node)));
        }
        let device = software(memory_for(screen), Arc::new(tairix_rt::ClockDelay))
            .map_err(|why| format!("the software device could not be made: {why}"))?;
        Ok((device, String::from("the software device")))
    }

    /// The first queue of `context`'s device that copies.
    fn copying(device: &Device, context: &Context) -> Result<Queue, String> {
        let family = family(device, QueueCapabilities::TRANSFER)
            .map_err(|_| String::from("the device has no queue that copies"))?;
        context
            .queue(family, 0)
            .map_err(|why| format!("the device gave no queue: {why}"))
    }

    /// What became of the frames presented.
    #[derive(Default)]
    struct Tally {
        presented: u64,
        shown: u64,
        unseen: u64,
    }

    impl Tally {
        /// Every frame presented was shown, each answered on its own.
        const fn all_shown(&self) -> bool {
            self.presented > 0 && self.unseen == 0 && self.shown == self.presented
        }
    }

    /// The drawing: the device, its context and queue, and the scene at the
    /// window's extent once the window covers the screen.
    struct Drawing {
        device: Device,
        context: Context,
        queue: Queue,
        /// Whether the window has covered the screen, so frames may go.
        begun: bool,
        scene: Option<Scene>,
        /// Whether the swapchain's picture holds the scene.
        drawn: bool,
    }

    impl Drawing {
        /// Make the swapchain's frames again at `extent`, the scene laid out
        /// for them as they are next drawn.
        fn resize(
            &mut self,
            client: &mut Client,
            chain: &mut Chain,
            extent: (u32, u32),
        ) -> Result<(), String> {
            chain
                .resize(client, (&self.device, &self.context), extent)
                .map_err(|refused| format!("the window could not be resized: {refused}"))?;
            self.begun = true;
            self.scene = None;
            self.drawn = false;
            Ok(())
        }

        /// Draw the scene into the swapchain's picture where it does not hold
        /// it, and present the whole of it, where a frame may go, answering
        /// whether the frame was presented here rather than once filled.
        fn present(&mut self, client: &mut Client, chain: &mut Chain) -> Result<bool, String> {
            if !self.begun {
                return Ok(false);
            }
            let mode = chain.mode();
            let extent = (mode.width_px, mode.height_px);
            let Some(image) = chain.acquire(client) else {
                return Ok(false);
            };
            if self.scene.is_none() {
                let laid = Scene::new(&self.device, &self.context, extent, image.desc().format)
                    .map_err(|why| format!("the scene could not be laid out: {why}"))?;
                self.scene = Some(laid);
            }
            let Some(scene) = &self.scene else {
                return Ok(false);
            };
            if !self.drawn {
                let mut commands = self.context.command_buffer();
                scene
                    .record(&mut commands, image)
                    .and_then(|()| {
                        self.queue
                            .submit(&Submission {
                                command_buffers: &[&commands],
                                waits: &[],
                                signals: &[],
                                deadline: tairix_gpucheck::DEADLINE,
                            })
                            .map(drop)
                            .map_err(|refused| refused.error)
                    })
                    .map_err(|why| format!("the scene could not be drawn: {why}"))?;
                self.drawn = true;
            }
            let whole = DamageList::new(&[DamageRect {
                x: 0,
                y: 0,
                width_px: extent.0,
                height_px: extent.1,
            }])
            .map_err(|why| format!("the frame could not be named: {why}"))?;
            match chain.present(client, (&self.context, &self.queue), whole) {
                Ok(presented) => Ok(presented),
                Err(Refused::Window(Errno::WouldBlock)) => Ok(false),
                Err(refused) => Err(format!("a frame could not be presented: {refused}")),
            }
        }
    }

    /// Whether `key` is `Escape` going down.
    fn escape(key: &KeyInput) -> bool {
        matches!(
            key,
            KeyInput::Pressed {
                key: Some(KeyValue::Named(NamedKeyCode::Escape)),
                ..
            }
        )
    }

    /// What the loop does once an event is taken in.
    enum Next {
        Carry,
        Stop,
    }

    /// Take `event` in.
    fn take(
        event: &WindowEvent,
        (client, chain, drawing): (&mut Client, &mut Chain, &mut Drawing),
        tally: &mut Tally,
    ) -> Result<Next, String> {
        let window = chain.window();
        match *event {
            WindowEvent::FramePresented { shown, .. } => {
                if !chain.event(event) {
                    return Ok(Next::Carry);
                }
                if shown {
                    tally.shown += 1;
                } else {
                    tally.unseen += 1;
                }
            }
            WindowEvent::Resized {
                window_id,
                width_px,
                height_px,
                state,
            } if window_id == window => {
                // The scene goes up only once the window covers the screen,
                // so no frame of it is shown at another size first.
                if state == WindowSizeState::Fullscreen || drawing.begun {
                    drawing.resize(client, chain, (width_px, height_px))?;
                }
            }
            WindowEvent::ContentReleased { window_id } if window_id == window => {
                chain
                    .release()
                    .map_err(|refused| format!("the frames could not be let go: {refused}"))?;
            }
            WindowEvent::RedrawRequested { window_id } if window_id == window => {
                chain
                    .reattach(client, (&drawing.device, &drawing.context))
                    .map_err(|refused| format!("the frames could not be made again: {refused}"))?;
                drawing.drawn = false;
            }
            WindowEvent::Key { window_id, ref key } if window_id == window && escape(key) => {
                return Ok(Next::Stop);
            }
            WindowEvent::CloseRequested { window_id } if window_id == window => {
                return Ok(Next::Stop);
            }
            _ => app::release_carried(event),
        }
        Ok(Next::Carry)
    }

    /// The window open and asked full screen, the swapchain filling it, the
    /// events it is driven by, the drawing and the device's name.
    struct Opened {
        chain: Chain,
        events: WindowEvents<Source>,
        /// Whether the wait reported the fill wake since the loop last looked.
        filled: Rc<Cell<bool>>,
        drawing: Drawing,
        on: String,
    }

    /// Connect, open the window on the device the scene is drawn on, and ask
    /// for full screen.
    fn open(client: &mut Client) -> Result<Opened, String> {
        let info = client
            .connect()
            .map_err(|err| format!("the desktop refused the connect ({err})"))?;
        let screen = (info.screen_width_px(), info.screen_height_px());
        let binding = app::bind_event_mailbox().map_err(|err| format!("{err}"))?;
        let (device, on) = device(screen)?;
        let context = device
            .create_context(&ContextDesc::default())
            .map_err(|why| format!("the device gave no context: {why}"))?;
        let queue = copying(&device, &context)?;
        let spec = WindowSpec {
            event_endpoint: binding.endpoint(),
            title: TITLE,
            sizing: ANY_SIZE,
            size: ((screen.0 / 2).max(1), (screen.1 / 2).max(1)),
            format: FRAMES,
        };
        let waits = Waits {
            name: NAME,
            set: binding.set(),
            token: FILL_TOKEN,
        };
        let chain = Swapchain::open(
            client,
            (&device, &context),
            (&spec, PresentMode::Fifo, waits),
        )
        .map_err(|refused| format!("the window could not be opened: {refused}"))?;
        client
            .set_size_state(chain.window(), WindowSizeState::Fullscreen)
            .map_err(|err| format!("full screen could not be asked for ({err})"))?;
        let server = client
            .session()
            .ok_or_else(|| String::from("the desktop did not identify itself"))?;
        let filled = Rc::new(Cell::new(false));
        let events = WindowEvents::new(Source {
            mailbox: EventMailbox::new(binding.endpoint(), server),
            set: binding.set(),
            filled: Rc::clone(&filled),
        });
        Ok(Opened {
            chain,
            events,
            filled,
            drawing: Drawing {
                device,
                context,
                queue,
                begun: false,
                scene: None,
                drawn: false,
            },
            on,
        })
    }

    /// Present the scene each frame the pacing allows until `Escape` or a
    /// close, then wait for the last frame's fate: what became of them all.
    fn show(client: &mut Client, opened: &mut Opened) -> Result<Tally, String> {
        let Opened {
            chain,
            events,
            filled,
            drawing,
            ..
        } = opened;
        let mut tally = Tally::default();
        let mut stopping = false;
        loop {
            let mut waited = events.wait(client);
            loop {
                match waited {
                    Ok(Some(event)) => {
                        if let Next::Stop = take(&event, (client, chain, drawing), &mut tally)? {
                            stopping = true;
                        }
                    }
                    Ok(None) => break,
                    Err(EventError::Undecodable(_)) => {}
                    Err(EventError::Mailbox(err)) => {
                        return Err(format!("the event channel was lost ({err})"));
                    }
                    Err(EventError::Refused(refused)) => {
                        return Err(format!("the desktop refused the window: {refused}"));
                    }
                }
                waited = events.try_wait(client);
            }
            let presented = chain
                .service(client, filled.replace(false))
                .map_err(|refused| format!("a frame could not be filled: {refused}"))?;
            tally.presented += u64::from(presented);
            if stopping {
                if chain.idle() {
                    return Ok(tally);
                }
            } else {
                tally.presented += u64::from(drawing.present(client, chain)?);
            }
        }
    }

    /// `0` once every frame presented, from the first full-screen one to
    /// `Escape` or a close, was shown; `1` where one was not, or a step was
    /// refused.
    pub(crate) fn run() -> i32 {
        let mut client = WindowClient::new(RtWindowTransport);
        let mut opened = match open(&mut client) {
            Ok(opened) => opened,
            Err(reason) => return fail(&reason),
        };
        let tally = match show(&mut client, &mut opened) {
            Ok(tally) => tally,
            Err(reason) => return fail(&reason),
        };
        // The verdict stands on the frames already answered.
        let _ = client.close(opened.chain.window());
        let on = &opened.on;
        let verdict = if tally.all_shown() {
            format!(
                "{} frames presented on {on}, every one shown",
                tally.presented
            )
        } else {
            format!(
                "{} frames presented on {on}, {} shown",
                tally.presented, tally.shown
            )
        };
        if !say(&verdict) {
            return fail("standard output refused the answer");
        }
        i32::from(!tally.all_shown())
    }
}

/// `gpucheck --shader`: the kernel a file holds, run on every served GPU and
/// on the software device.
#[cfg(all(freestanding, feature = "program"))]
mod kernel {
    use alloc::format;
    use alloc::string::String;
    use alloc::sync::Arc;

    use tairix_gpu::Device;
    use tairix_gpucheck::{
        compare_kernel, lost, marked_at, reference, run_kernel, Kernel, Outcome, Source,
        MAX_FILE_BYTES,
    };
    use tairix_rt::io::write_stderr_line;
    use tairix_sandbox::host::ParserSandbox;
    use tairix_sandbox::rt::RtLauncher;
    use tairix_sandbox::wgsl::{compile, CompileFailure, Fault, WORKER_MEMORY};
    use tairix_wgsl::{Diagnostic, Severity};

    use crate::program::{fail, say, served, REGION};

    /// What the marker a neighbour's memory holds always carries, so no word
    /// of a kernel's starting buffer could be it.
    const MARKED: u32 = 1 << 31;

    /// `0` once every served GPU agreed with the software device on the
    /// kernel `file` holds, run over `words`; `1` where one differed, the
    /// kernel was refused, or the run could not be made.
    pub(crate) fn run(file: &str, words: u32) -> i32 {
        let kernel = match read(file) {
            Ok(kernel) => kernel,
            Err(status) => return status,
        };
        let mut drawn = [0u8; 4];
        if tairix_rt::random_fill(&mut drawn).is_err() {
            return fail("no random marker could be drawn");
        }
        let marker = u32::from_le_bytes(drawn) | MARKED;
        let Some(served) = served() else {
            return fail("the served GPUs could not be listed");
        };
        if served.is_empty() {
            return fail("no GPU is served");
        }
        let Ok(device) = reference(Arc::new(tairix_rt::ClockDelay)) else {
            return fail("the software device could not be made");
        };
        let expected = match run_kernel(&device, &kernel, (words, marker)) {
            Ok(Outcome::Refused(why)) => {
                return fail(&format!("the software device refused the kernel: {why}"));
            }
            Ok(Outcome::Lost { serves: false, .. }) => {
                return fail("the software device served nothing after losing the kernel");
            }
            Ok(Outcome::Unanswered) => {
                return fail("the software device never answered the kernel");
            }
            Ok(expected) => expected,
            Err(refused) => return fail(&format!("the software device refused {refused}")),
        };
        drop(device);
        if let Outcome::Ran(left) = &expected {
            if let Some(at) = marked_at(left, marker) {
                return fail(&format!(
                    "word {at} on the software device holds its neighbour's marker"
                ));
            }
        }
        let mut agreed = true;
        for gpu in &served {
            let line = match Device::connect(gpu.endpoint, REGION) {
                Err(why) => {
                    agreed = false;
                    format!("gpu {}: could not be reached: {why}", gpu.node)
                }
                Ok(device) => match run_kernel(&device, &kernel, (words, marker)) {
                    Err(refused) => {
                        agreed = false;
                        format!("gpu {}: refused {refused}", gpu.node)
                    }
                    Ok(answered) => match compare_kernel(&answered, &expected, marker) {
                        Ok(()) => agreement(gpu.node, &answered),
                        Err(difference) => {
                            agreed = false;
                            format!("gpu {}: {difference}", gpu.node)
                        }
                    },
                },
            };
            if !say(&line) {
                return fail("standard output refused the answer");
            }
        }
        i32::from(!agreed)
    }

    /// The line for `node`, which answered as the software device did.
    fn agreement(node: u32, answered: &Outcome) -> String {
        match answered {
            Outcome::Lost { loss, .. } => format!(
                "gpu {node}: agrees with the software device: each lost the kernel as {}, and served on",
                lost(*loss)
            ),
            Outcome::Ran(_) | Outcome::Refused(_) | Outcome::Unanswered => {
                format!("gpu {node}: agrees with the software device")
            }
        }
    }

    /// The kernel `file` holds, admitted; or the status its refusal ends the
    /// command with, the refusal stated.
    fn read(file: &str) -> Result<Kernel, i32> {
        let bytes =
            tairix_rt::read_path_to_end(file.as_bytes(), MAX_FILE_BYTES).map_err(|raw| {
                let why = tairix_abi::Errno::from_syscall(raw);
                fail(&format!("{file}: {why}"))
            })?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(fail(&format!("{file}: longer than any kernel may be")));
        }
        match Source::of(&bytes) {
            None => Err(fail(&format!(
                "{file}: neither a SPIR-V module nor WGSL source"
            ))),
            Some(Source::Spirv(module)) => {
                Kernel::decode(module).map_err(|refused| fail(&format!("{file}: {refused}")))
            }
            Some(Source::Wgsl(source)) => compiled(file, source),
        }
    }

    /// The WGSL kernel `source` compiled in a sandbox worker and admitted
    /// here, every warning and note stated; or the status its refusal ends
    /// the command with.
    fn compiled(file: &str, source: &str) -> Result<Kernel, i32> {
        let mut sandbox =
            ParserSandbox::new(RtLauncher::own_binary(WORKER_MEMORY), tairix_rt::LogSink);
        let compiled = compile(&mut sandbox, source, &[]).map_err(|failure| match failure {
            CompileFailure::Source(error) => {
                state(file, &error);
                1
            }
            CompileFailure::TooLarge => fail(&format!("{file}: longer than WGSL source may be")),
            CompileFailure::Sandbox(why) => {
                fail(&format!("{file}: the compiler's sandbox failed: {why}"))
            }
            CompileFailure::Fault(Fault::OutOfMemory) | CompileFailure::NoMemory => {
                fail(&format!("{file}: the compile ran out of memory"))
            }
            CompileFailure::Fault(Fault::Internal | Fault::MalformedRequest)
            | CompileFailure::ReplyMalformed => fail(&format!("{file}: the compiler failed")),
        })?;
        for diagnostic in &compiled.diagnostics {
            state(file, diagnostic);
        }
        Kernel::new(compiled.module).map_err(|refused| fail(&format!("{file}: {refused}")))
    }

    /// `diagnostic` stated on `stderr` at its place in `file`.
    fn state(file: &str, diagnostic: &Diagnostic) {
        let severity = match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "note",
        };
        write_stderr_line(format!(
            "{file}:{}:{}: {severity}: {}",
            diagnostic.line, diagnostic.column, diagnostic.message
        ));
    }
}

// On the host the program's real entry is not compiled, so this inert `main`
// keeps the crate building under the host tooling.
#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}
