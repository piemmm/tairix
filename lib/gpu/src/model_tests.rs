use alloc::vec::Vec;

use tairix_abi::Duration64;
use tairix_inline::ArrayVec;

use super::{BufferKey, ContextKey, Dispatch, JobId, Model, Point, Request, TimelineKey};
use crate::command::Command;
use crate::info::{
    BufferUsage, DeviceInfo, Features, FormatTable, Limits, MemoryHeap, MemoryProperties,
    MemoryType, QueueCapabilities, QueueFamily,
};
use crate::room::Room;
use crate::{Error, Loss};

const HOST: u32 = 0;
const DEVICE_ONLY: u32 = 1;

/// One heap, a host-visible and a device-only memory type, and one transfer
/// family of `queues` queues on an engine holding `depth` jobs.
fn model(queues: u32, depth: usize) -> Model {
    let mut heaps = ArrayVec::new();
    let _ = heaps.try_push(MemoryHeap {
        size: 1 << 20,
        device_local: true,
    });
    let mut memory_types = ArrayVec::new();
    let _ = memory_types.try_push(MemoryType {
        heap: 0,
        properties: MemoryProperties::HOST_VISIBLE.with(MemoryProperties::HOST_COHERENT),
    });
    let _ = memory_types.try_push(MemoryType {
        heap: 0,
        properties: MemoryProperties::DEVICE_LOCAL,
    });
    let mut queue_families = ArrayVec::new();
    let _ = queue_families.try_push(QueueFamily {
        capabilities: QueueCapabilities::TRANSFER,
        queues,
    });
    let info = DeviceInfo {
        heaps,
        memory_types,
        queue_families,
        limits: Limits {
            max_allocation_size: 1 << 20,
            max_job_time: Duration64::from_secs(10),
            ..crate::software::limits(1 << 20)
        },
        capabilities: ArrayVec::new(),
        features: Features::NONE,
        formats: FormatTable::from_fn(|_| crate::FormatFeatures::NONE),
    };
    Model::new(info, &[depth]).expect("a well-formed device")
}

/// `size` bytes of `memory_type` made and filed, as a context allocates.
fn allocate(
    model: &mut Model,
    context: ContextKey,
    size: u64,
    memory_type: u32,
) -> Result<super::AllocationKey, Error> {
    let (charge, overhead) = model.charge_allocation(context, size, memory_type)?;
    let made = crate::memory::Memory::zeroed(size, charge, overhead).and_then(crate::shared);
    let memory = match made {
        Ok(memory) => memory,
        Err(error) => {
            model.unreserve_allocation(context);
            return Err(error);
        }
    };
    model
        .file_allocation(context, memory, memory_type, size)
        .map_err(|(error, _)| error)
}

fn buffer(model: &mut Model, context: ContextKey) -> BufferKey {
    let memory = allocate(model, context, 64, HOST).expect("memory");
    model
        .create_buffer(context, memory, 0, 64, BufferUsage::ALL)
        .expect("a buffer")
}

/// Queue one fill of `buffer` on `queue`, waiting for `waits` and setting
/// `signals`, to run for at most `span` nanoseconds.
fn job(
    model: &mut Model,
    context: ContextKey,
    queue: usize,
    buffer: BufferKey,
    waits: &[(TimelineKey, u64)],
    signals: &[(TimelineKey, u64)],
    span: u64,
) -> (TimelineKey, u64) {
    let commands = [Command::Fill {
        dst: buffer,
        offset: 0,
        size: 64,
        word: 0,
    }];
    model
        .submit(
            context,
            &Request {
                queue,
                buffers: &[&commands],
                waits,
                signals,
                span,
            },
            &mut Vec::new(),
        )
        .expect("admitted")
}

fn reached(
    model: &Model,
    context: ContextKey,
    (timeline, value): (TimelineKey, u64),
) -> Result<bool, Error> {
    model
        .point(context, timeline, value)
        .map(|point| matches!(point, Point::Reached))
}

fn take(model: &mut Model, now: u64) -> Dispatch {
    model.dispatch(&|| now).0.expect("a job ready to run")
}

#[test]
fn a_reset_spares_every_context_whose_work_had_not_begun() {
    let mut model = model(1, 3);
    let contexts: Vec<ContextKey> = (0..3)
        .map(|_| model.create_context(None).expect("a context"))
        .collect();
    let (guilty, bystander, collateral) = (contexts[0], contexts[1], contexts[2]);
    let buffers: Vec<BufferKey> = contexts.iter().map(|&c| buffer(&mut model, c)).collect();
    let _ = job(&mut model, guilty, 0, buffers[0], &[], &[], 10);
    let spared = job(&mut model, bystander, 0, buffers[1], &[], &[], 100);
    let _ = job(&mut model, collateral, 0, buffers[2], &[], &[], 100);
    let running: Vec<JobId> = (0..3).map(|_| take(&mut model, 0).job).collect();
    assert!(model.dispatch(&|| 0).0.is_none(), "the engine is full");
    let (overran, begun) = (running[0], running[2]);
    model.reset(0, 10, &|job| job == overran || job == begun);
    assert_eq!(model.status(guilty), Err(Error::Lost(Loss::Overran)));
    assert_eq!(model.status(collateral), Err(Error::Lost(Loss::Reset)));
    assert_eq!(model.status(bystander), Ok(()));
    assert_eq!(reached(&model, bystander, spared), Ok(false));
    let again = take(&mut model, 20);
    assert_eq!(again.job, running[1], "the spared job runs again, first");
    assert!(model.dispatch(&|| 20).0.is_none());
    let _ = model.complete(again.engine, again.job, None);
    assert_eq!(reached(&model, bystander, spared), Ok(true));
}

#[test]
fn a_spared_job_goes_back_ahead_of_what_was_dispatched_after_it() {
    let mut model = model(1, 3);
    let contexts: Vec<ContextKey> = (0..3)
        .map(|_| model.create_context(None).expect("a context"))
        .collect();
    let (first, second, guilty) = (contexts[0], contexts[1], contexts[2]);
    let buffers: Vec<BufferKey> = contexts.iter().map(|&c| buffer(&mut model, c)).collect();
    // The first context's queue is listed again for its next job as soon as
    // its first is running.
    let _ = job(&mut model, first, 0, buffers[0], &[], &[], 100);
    let _ = job(&mut model, first, 0, buffers[0], &[], &[], 100);
    let _ = job(&mut model, second, 0, buffers[1], &[], &[], 100);
    let _ = job(&mut model, guilty, 0, buffers[2], &[], &[], 10);
    let running: Vec<JobId> = (0..3).map(|_| take(&mut model, 0).job).collect();
    let overran = running[2];
    model.reset(0, 10, &|job| job == overran);
    assert_eq!(model.status(guilty), Err(Error::Lost(Loss::Overran)));
    let order: Vec<JobId> = (0..2).map(|_| take(&mut model, 20).job).collect();
    assert_eq!(order, running[..2], "each spared job ahead of the next");
}

#[test]
fn spared_jobs_run_again_in_the_order_they_first_ran() {
    let mut model = model(1, 4);
    let contexts: Vec<ContextKey> = (0..3)
        .map(|_| model.create_context(None).expect("a context"))
        .collect();
    let (first, second, guilty) = (contexts[0], contexts[1], contexts[2]);
    let buffers: Vec<BufferKey> = contexts.iter().map(|&c| buffer(&mut model, c)).collect();
    // The first context's two jobs run before the second's, and a job of
    // the second's waits behind them all.
    let _ = job(&mut model, first, 0, buffers[0], &[], &[], 100);
    let _ = job(&mut model, first, 0, buffers[0], &[], &[], 100);
    let early = take(&mut model, 0).job;
    let middle = take(&mut model, 0).job;
    let _ = job(&mut model, second, 0, buffers[1], &[], &[], 100);
    let late = take(&mut model, 0).job;
    let _ = job(&mut model, guilty, 0, buffers[2], &[], &[], 10);
    let overran = take(&mut model, 0).job;
    let _ = job(&mut model, second, 0, buffers[1], &[], &[], 100);
    model.reset(0, 10, &|job| job == overran);
    assert_eq!(model.status(guilty), Err(Error::Lost(Loss::Overran)));
    let order: Vec<JobId> = (0..3).map(|_| take(&mut model, 20).job).collect();
    assert_eq!(order, [early, middle, late], "as they first ran");
    let after = take(&mut model, 20).job;
    assert_eq!(after.context, second, "then what came after them");
}

#[test]
fn an_engine_s_list_grows_by_doubling_so_a_burst_of_contexts_moves_it_few_times() {
    let mut model = model(2, 1);
    let mut rooms = Vec::new();
    for _ in 0..256 {
        let _ = model.create_context(None).expect("a context");
        rooms.push(
            model
                .engines
                .first()
                .expect("an engine")
                .runnable
                .capacity(),
        );
    }
    rooms.dedup();
    assert!(
        rooms.len() <= 10,
        "512 places in {} sizes: {rooms:?}",
        rooms.len()
    );
    assert!(rooms.last().is_some_and(|&room| room >= 512));
}

#[test]
fn completions_of_another_context_s_lost_work_signal_nothing() {
    let mut model = model(1, 2);
    let doomed = model.create_context(None).expect("a context");
    let mine = buffer(&mut model, doomed);
    let fence = job(&mut model, doomed, 0, mine, &[], &[], 10);
    let first = take(&mut model, 0);
    let second_fence = job(&mut model, doomed, 0, mine, &[], &[], 10);
    let second = take(&mut model, 0);
    let _ = model.complete(first.engine, first.job, Some(Loss::Fault));
    assert_eq!(model.status(doomed), Err(Error::Lost(Loss::Fault)));
    let woken = model.complete(second.engine, second.job, None);
    assert!(woken.is_empty());
    assert_eq!(
        reached(&model, doomed, fence),
        Err(Error::Lost(Loss::Fault))
    );
    assert_eq!(
        reached(&model, doomed, second_fence),
        Err(Error::Lost(Loss::Fault))
    );
}

#[test]
fn a_value_is_reached_only_once_every_signal_up_to_it_has_landed() {
    let mut model = model(2, 2);
    let context = model.create_context(None).expect("a context");
    let target = buffer(&mut model, context);
    let timeline = model.create_timeline(context, 0).expect("a timeline");
    let fences = [
        job(&mut model, context, 0, target, &[], &[(timeline, 1)], 10),
        job(&mut model, context, 1, target, &[], &[(timeline, 2)], 10),
    ];
    // A waiter on each, so a timeline that moves has one to wake.
    let _waiters: Vec<Point> = [(timeline, 2), fences[0], fences[1]]
        .into_iter()
        .map(|(t, value)| model.point(context, t, value).expect("waited"))
        .collect();
    let first = take(&mut model, 0);
    let second = take(&mut model, 0);
    let woken = model.complete(second.engine, second.job, None);
    assert_eq!(model.timeline_value(context, timeline), Ok(0));
    assert_eq!(woken.len(), 1, "only the second queue's fence moved");
    let woken = model.complete(first.engine, first.job, None);
    assert_eq!(model.timeline_value(context, timeline), Ok(2));
    assert_eq!(woken.len(), 2);
}

#[test]
fn memory_made_for_a_context_released_meanwhile_is_refused_and_given_back() {
    use crate::memory::{Budget, Memory};
    let mut model = model(1, 1);
    let context = model.create_context(None).expect("a context");
    let (charge, overhead) = model.charge_allocation(context, 64, HOST).expect("charged");
    let memory =
        crate::shared(Memory::zeroed(64, charge, overhead).expect("made")).expect("shared");
    // Released while its memory is made, without the device's lock.
    drop(model.destroy_context(context));
    let held = model.work_heap_taken();
    let (error, refused) = model
        .file_allocation(context, memory, HOST, 64)
        .expect_err("refused");
    assert_eq!(error, Error::Destroyed);
    assert_eq!(
        alloc::sync::Arc::strong_count(&refused),
        1,
        "handed back, the last of it, to go without the device's lock"
    );
    assert_eq!(held, model.work_heap_taken(), "charged until it goes");
    drop(refused);
    assert_eq!(
        held - model.work_heap_taken(),
        64 + Memory::overhead(64) + super::shared_bytes::<Budget>(),
        "the memory, and the budget it alone still held"
    );
}

#[test]
fn a_timeline_no_one_waits_on_wakes_no_one_as_it_moves() {
    let mut model = model(1, 1);
    let context = model.create_context(None).expect("a context");
    let target = buffer(&mut model, context);
    let _ = job(&mut model, context, 0, target, &[], &[], 10);
    let unwatched = take(&mut model, 0);
    assert!(
        model
            .complete(unwatched.engine, unwatched.job, None)
            .is_empty(),
        "no waiter, so no wake to make"
    );
    let (fence, value) = job(&mut model, context, 0, target, &[], &[], 10);
    let _waiter = model.point(context, fence, value).expect("waited");
    let watched = take(&mut model, 0);
    assert_eq!(model.complete(watched.engine, watched.job, None).len(), 1);
}

#[test]
fn a_context_whose_own_work_overran_is_lost_for_that() {
    let mut model = model(2, 2);
    let context = model.create_context(None).expect("a context");
    let target = buffer(&mut model, context);
    let _ = job(&mut model, context, 0, target, &[], &[], 100);
    let _ = job(&mut model, context, 1, target, &[], &[], 10);
    let _ = take(&mut model, 0);
    let _ = take(&mut model, 0);
    // The job that had begun but not overrun is met first.
    model.reset(0, 10, &|_| true);
    assert_eq!(model.status(context), Err(Error::Lost(Loss::Overran)));
}

#[test]
fn work_that_had_not_begun_overran_nothing() {
    let mut model = model(1, 2);
    let guilty = model.create_context(None).expect("a context");
    let waiting = model.create_context(None).expect("a context");
    let buffers = [buffer(&mut model, guilty), buffer(&mut model, waiting)];
    let _ = job(&mut model, guilty, 0, buffers[0], &[], &[], 10);
    let _ = job(&mut model, waiting, 0, buffers[1], &[], &[], 5);
    let overran = take(&mut model, 0).job;
    let _ = take(&mut model, 0);
    // The waiting job's deadline, counted from its dispatch, has passed too.
    model.reset(0, 10, &|job| job == overran);
    assert_eq!(model.status(guilty), Err(Error::Lost(Loss::Overran)));
    assert_eq!(model.status(waiting), Ok(()), "it never ran");
}

#[test]
fn a_second_reset_puts_what_it_spares_ahead_of_what_the_first_left() {
    let mut model = model(1, 4);
    let contexts: Vec<ContextKey> = (0..4)
        .map(|_| model.create_context(None).expect("a context"))
        .collect();
    let buffers: Vec<BufferKey> = contexts.iter().map(|&c| buffer(&mut model, c)).collect();
    for (at, span) in [10, 100, 1000, 1000].into_iter().enumerate() {
        let _ = job(&mut model, contexts[at], 0, buffers[at], &[], &[], span);
    }
    let running: Vec<JobId> = (0..4).map(|_| take(&mut model, 0).job).collect();
    let first = running[0];
    model.reset(0, 10, &|job| job == first);
    // Two of the three spared run again before the next reset.
    let again: Vec<JobId> = (0..2).map(|_| take(&mut model, 20).job).collect();
    assert_eq!(again, running[1..3]);
    let overran = again[0];
    model.reset(0, 130, &|job| job == overran);
    let order: Vec<JobId> = (0..2).map(|_| take(&mut model, 140).job).collect();
    assert_eq!(
        order,
        [running[2], running[3]],
        "in the order they first ran"
    );
}

#[test]
fn an_engine_serves_the_queues_ready_for_it_in_turn() {
    let mut model = model(1, 1);
    let one = model.create_context(None).expect("a context");
    let two = model.create_context(None).expect("a context");
    let (a, b) = (buffer(&mut model, one), buffer(&mut model, two));
    let a_jobs = [
        job(&mut model, one, 0, a, &[], &[], 10),
        job(&mut model, one, 0, a, &[], &[], 10),
    ];
    let b_jobs = [
        job(&mut model, two, 0, b, &[], &[], 10),
        job(&mut model, two, 0, b, &[], &[], 10),
    ];
    let mut order = Vec::new();
    while let Some(dispatch) = model.dispatch(&|| 0).0 {
        let _ = model.complete(dispatch.engine, dispatch.job, None);
        let ran_one = a_jobs
            .iter()
            .filter(|&&f| reached(&model, one, f) == Ok(true))
            .count();
        let ran_two = b_jobs
            .iter()
            .filter(|&&f| reached(&model, two, f) == Ok(true))
            .count();
        order.push((ran_one, ran_two));
    }
    assert_eq!(order, [(1, 0), (1, 1), (2, 1), (2, 2)]);
}

#[test]
fn a_queue_s_next_job_waits_its_turn_however_much_room_its_engine_has() {
    let mut model = model(1, 4);
    let context = model.create_context(None).expect("a context");
    let target = buffer(&mut model, context);
    let gate = model.create_timeline(context, 0).expect("a timeline");
    let _ = job(&mut model, context, 0, target, &[(gate, 1)], &[], 10);
    let _ = job(&mut model, context, 0, target, &[], &[], 10);
    assert!(
        model.dispatch(&|| 0).0.is_none(),
        "the first job is not ready"
    );
    let _ = model.signal(context, gate, 1).expect("advances");
    let first = take(&mut model, 0);
    let second = take(&mut model, 0);
    assert_ne!(first.job, second.job);
    assert!(model.dispatch(&|| 0).0.is_none());
}

#[test]
fn memory_the_cpu_cannot_reach_is_refused_to_it() {
    let mut model = model(1, 1);
    let context = model.create_context(None).expect("a context");
    let device_only = allocate(&mut model, context, 64, DEVICE_ONLY).expect("memory");
    let host = allocate(&mut model, context, 64, HOST).expect("memory");
    assert!(matches!(
        model.host_memory(context, device_only),
        Err(Error::NotHostVisible)
    ));
    assert!(model.host_memory(context, host).is_ok());
}

#[test]
fn a_released_timeline_stays_while_queued_work_holds_it() {
    let mut model = model(1, 1);
    let context = model.create_context(None).expect("a context");
    let target = buffer(&mut model, context);
    let timeline = model.create_timeline(context, 0).expect("a timeline");
    let _ = job(&mut model, context, 0, target, &[], &[(timeline, 3)], 10);
    model.destroy_timeline(context, timeline);
    assert_eq!(model.timeline_value(context, timeline), Ok(0));
    let running = take(&mut model, 0);
    let _ = model.complete(running.engine, running.job, None);
    assert_eq!(model.timeline_value(context, timeline), Err(Error::Stale));
}

#[test]
fn a_description_no_context_could_be_served_by_is_refused() {
    let valid = || {
        let model = model(1, 1);
        model.info().clone()
    };
    let mut missing_heap = valid();
    if let Some(kind) = missing_heap.memory_types.as_mut_slice().first_mut() {
        kind.heap = 4;
    }
    let mut no_queues = valid();
    if let Some(family) = no_queues.queue_families.as_mut_slice().first_mut() {
        family.queues = 0;
    }
    let mut crowded = valid();
    if let Some(family) = crowded.queue_families.as_mut_slice().first_mut() {
        family.queues = crate::MAX_QUEUES_PER_FAMILY + 1;
    }
    let mut odd_alignment = valid();
    odd_alignment.limits.buffer_alignment = 24;
    let mut no_deadline = valid();
    no_deadline.limits.max_job_time = Duration64::ZERO;
    // Render limits past what a pass or a pipeline can record.
    let mut targets = valid();
    targets.limits.max_color_attachments =
        u32::try_from(crate::MAX_PASS_ATTACHMENTS).expect("small") + 1;
    let mut buffers = valid();
    buffers.limits.max_vertex_input_bindings =
        u32::try_from(crate::MAX_DRAW_VERTEX_BUFFERS).expect("small") + 1;
    let mut attributes = valid();
    attributes.limits.max_vertex_input_attributes = crate::MAX_VERTEX_ATTRIBUTES + 1;
    let mut bounds = valid();
    bounds.limits.viewport_bounds_range = [1, -1];
    for info in [
        missing_heap,
        no_queues,
        crowded,
        odd_alignment,
        no_deadline,
        targets,
        buffers,
        attributes,
        bounds,
    ] {
        assert!(matches!(Model::new(info, &[1]), Err(Error::Malformed)));
    }
    assert!(matches!(Model::new(valid(), &[]), Err(Error::Malformed)));
    assert!(matches!(Model::new(valid(), &[0]), Err(Error::Malformed)));
}

#[test]
fn a_released_context_s_running_work_is_let_finish_and_discarded() {
    let mut model = model(1, 1);
    let gone = model.create_context(None).expect("a context");
    let staying = model.create_context(None).expect("a context");
    let (a, b) = (buffer(&mut model, gone), buffer(&mut model, staying));
    let _ = job(&mut model, gone, 0, a, &[], &[], 10);
    let _ = job(&mut model, gone, 0, a, &[], &[], 10);
    let kept = job(&mut model, staying, 0, b, &[], &[], 10);
    let running = take(&mut model, 0);
    model.destroy_context(gone);
    assert!(model.complete(running.engine, running.job, None).is_empty());
    assert_eq!(model.status(gone), Err(Error::Destroyed));
    let next = take(&mut model, 0);
    let _ = model.complete(next.engine, next.job, None);
    assert_eq!(reached(&model, staying, kept), Ok(true));
    assert!(
        model.dispatch(&|| 0).0.is_none(),
        "the released context's queued job went with it"
    );
}

#[test]
fn work_a_queue_s_family_does_not_run_is_refused() {
    let mut model = model(1, 1);
    let mut families = ArrayVec::new();
    let _ = families.try_push(QueueFamily {
        capabilities: QueueCapabilities::COMPUTE,
        queues: 1,
    });
    let info = DeviceInfo {
        queue_families: families,
        ..model.info().clone()
    };
    model = Model::new(info, &[1]).expect("a well-formed device");
    let context = model.create_context(None).expect("a context");
    let target = buffer(&mut model, context);
    let fill = [Command::Fill {
        dst: target,
        offset: 0,
        size: 4,
        word: 0,
    }];
    let request = Request {
        queue: 0,
        buffers: &[&fill],
        waits: &[],
        signals: &[],
        span: 1_000,
    };
    assert_eq!(
        model.submit(context, &request, &mut Vec::new()).err(),
        Some(crate::SubmitError {
            command: Some(0),
            error: Error::Unsupported
        })
    );
}

#[test]
fn a_timeline_s_pending_signals_are_charged_for_their_room_and_give_it_back_as_they_land() {
    let mut model = model(1, 1);
    let context = model.create_context(None).expect("a context");
    let target = buffer(&mut model, context);
    let gate = model.create_timeline(context, 0).expect("a gate");
    let timeline = model.create_timeline(context, 0).expect("a timeline");
    let room = |model: &Model| {
        model
            .contexts
            .get(context)
            .expect("live")
            .timelines
            .get(timeline)
            .expect("held")
            .pending
            .room_bytes()
    };
    let before = model.memory_taken(context).expect("live").objects;
    for value in 1..=512 {
        let _ = job(
            &mut model,
            context,
            0,
            target,
            &[(gate, 1)],
            &[(timeline, value)],
            10,
        );
    }
    let burst = room(&model);
    assert!(burst >= 512 * size_of::<super::Pending>() as u64);
    assert!(model.memory_taken(context).expect("live").objects >= before + burst);
    let _ = model.signal(context, gate, 1).expect("opened");
    while let Some(dispatch) = model.dispatch(&|| 0).0 {
        let _ = model.complete(dispatch.engine, dispatch.job, None);
    }
    assert_eq!(model.timeline_value(context, timeline), Ok(512));
    assert!(
        room(&model) * 32 <= burst,
        "{} of a burst's {burst} kept once every signal landed",
        room(&model)
    );
}

#[test]
fn work_a_lost_context_drops_lets_go_of_the_timelines_it_held() {
    for overrun in [false, true] {
        let mut model = model(1, 1);
        let context = model.create_context(None).expect("a context");
        let target = buffer(&mut model, context);
        let timeline = model.create_timeline(context, 0).expect("a timeline");
        let _ = job(&mut model, context, 0, target, &[], &[(timeline, 1)], 10);
        let _ = job(&mut model, context, 0, target, &[], &[(timeline, 2)], 10);
        let first = take(&mut model, 0);
        if overrun {
            model.reset(first.engine, 10, &|_| true);
        } else {
            let _ = model.complete(first.engine, first.job, Some(Loss::Fault));
        }
        let held = |model: &Model| {
            model
                .contexts
                .get(context)
                .expect("kept")
                .timelines
                .get(timeline)
                .map(|t| t.holds)
        };
        assert_eq!(held(&model), Some(0), "overrun {overrun}");
        model.destroy_timeline(context, timeline);
        assert_eq!(held(&model), None, "let go at once, overrun {overrun}");
    }
}

#[test]
fn an_engine_has_room_to_list_every_queue_of_every_context_it_serves() {
    let mut model = model(3, 1);
    let contexts: Vec<ContextKey> = (0..7)
        .map(|_| model.create_context(None).expect("a context"))
        .collect();
    let engine = model.engines.first().expect("an engine");
    assert_eq!(engine.queues, 21);
    assert!(engine.runnable.capacity() >= 21);
    for &context in &contexts[..3] {
        model.destroy_context(context);
    }
    assert_eq!(model.engines.first().expect("an engine").queues, 12);
}

#[test]
fn the_room_a_burst_of_contexts_made_stays_charged_to_the_heap_and_small() {
    let mut model = model(2, 1);
    let base = model.work_heap_taken();
    let burst = |model: &mut Model| {
        let keys: Vec<ContextKey> = (0..256)
            .map(|_| model.create_context(None).expect("a context"))
            .collect();
        for key in keys {
            model.destroy_context(key);
        }
        model.work_heap_taken() - base
    };
    let kept = burst(&mut model);
    assert!(kept >= 256 * 8, "every slot charged: {kept}");
    assert!(
        kept <= 256 * 32 + 256,
        "each slot small, the lists given back: {kept}"
    );
    let runnable = model
        .engines
        .first()
        .expect("an engine")
        .runnable
        .capacity();
    assert!(
        runnable <= 4,
        "the engine's list fits the queues it serves: {runnable}"
    );
    assert_eq!(burst(&mut model), kept, "a second burst reuses the room");
}

#[test]
fn a_pipeline_refused_at_its_limit_still_looks_at_the_working_memory() {
    let mut model = model(1, 1);
    let rich = model.create_context(Some(u64::MAX)).expect("a context");
    let own = {
        let probe = model.create_context(Some(u64::MAX)).expect("a probe");
        let used = model.memory_taken(probe).expect("live").total();
        drop(model.destroy_context(probe));
        used
    };
    // Room for its own state and nothing past it.
    let poor = model.create_context(Some(own)).expect("a context");
    let need = crate::software::dispatch::Need {
        words: 1 << 12,
        masks: 1,
    };
    let (plan, released) = model.plan_workspaces(rich, need, 0);
    drop(released);
    let (hold, released) = model.install_workspaces(plan.expect("planned").make().expect("made"));
    drop(released);
    let taken = model.work_heap_taken();
    // Its pipeline's last holder gone where nothing looks at the pool.
    drop(hold);
    let (planned, released) = model.plan_pipeline((poor, crate::pipeline::computes), need, 0);
    assert_eq!(planned.err(), Some(Error::OverLimit));
    drop(released);
    assert!(
        model.work_heap_taken() < taken,
        "the set no pipeline holds let go at the look"
    );
}

#[test]
fn a_context_is_made_from_working_memory_no_pipeline_holds_before_the_heap_refuses() {
    let mut model = model(1, 1);
    let context = model.create_context(Some(u64::MAX)).expect("a context");
    let need = crate::software::dispatch::Need {
        words: 1 << 17,
        masks: 1,
    };
    let (plan, released) = model.plan_workspaces(context, need, 0);
    drop(released);
    let (hold, released) = model.install_workspaces(plan.expect("planned").make().expect("made"));
    drop(released);
    let hold = hold.expect("held");
    // All the heap but what the filler's own entry takes and a little, less
    // than a context's state.
    let free = (1 << 20) - model.work_heap_taken();
    let _filler = allocate(&mut model, context, free - 512, HOST).expect("the heap but a little");
    assert!((1 << 20) - model.work_heap_taken() < 512);
    // The hold goes with nothing looking at the pool after it.
    drop(hold);
    assert!(
        model.create_context(None).is_ok(),
        "the working memory no pipeline holds went back first"
    );
}

#[test]
fn allocations_charged_together_each_have_their_tables_room_before_any_memory_is_made() {
    let mut model = model(1, 1);
    let taken = |model: &Model, context| model.memory_taken(context).expect("live").total();
    // An unlimited context's cost of three allocations, and of a fourth's
    // charges, its table having room for it.
    let probe = model.create_context(None).expect("a context");
    for _ in 0..3 {
        allocate(&mut model, probe, 64, HOST).expect("memory");
    }
    let three = taken(&model, probe);
    let charged = model.charge_allocation(probe, 64, HOST).expect("charged");
    let one = taken(&model, probe) - three;
    drop(charged);
    model.unreserve_allocation(probe);
    // Room for two allocations' charges, not for those and the table's
    // growth the second needs once the first's entry is kept.
    let tight = model
        .create_context(Some(three + 2 * one))
        .expect("a context");
    for _ in 0..3 {
        allocate(&mut model, tight, 64, HOST).expect("memory");
    }
    assert_eq!(taken(&model, tight), three);
    let _first = model.charge_allocation(tight, 64, HOST).expect("charged");
    assert_eq!(
        model.charge_allocation(tight, 64, HOST).err(),
        Some(Error::OverLimit),
        "the second refused before its memory is made"
    );
}

#[test]
fn a_refused_submission_leaves_its_resolved_commands_with_the_caller() {
    let mut model = model(1, 1);
    let probe = model.create_context(None).expect("a context");
    let _ = buffer(&mut model, probe);
    let used = model.memory_taken(probe).expect("live").total();
    // No room for any job past its buffer.
    let tight = model.create_context(Some(used)).expect("a context");
    let target = buffer(&mut model, tight);
    let fill = || Command::Fill {
        dst: target,
        offset: 0,
        size: 64,
        word: 0,
    };
    let fills = [fill(), fill()];
    let request = Request {
        queue: 0,
        buffers: &[&fills],
        waits: &[],
        signals: &[],
        span: 1_000,
    };
    let mut resolved = Vec::new();
    assert_eq!(
        model.submit(tight, &request, &mut resolved).err(),
        Some(crate::SubmitError::from(Error::OverLimit))
    );
    assert_eq!(
        resolved.len(),
        2,
        "every command resolved, to be let go without the device's lock"
    );
}
