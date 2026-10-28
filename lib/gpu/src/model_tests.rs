use alloc::vec::Vec;

use tairix_abi::Duration64;
use tairix_inline::ArrayVec;

use super::{BufferKey, ContextKey, Dispatch, JobKey, Model, Point, Request, TimelineKey};
use crate::command::Command;
use crate::info::{
    BufferUsage, DeviceInfo, Limits, MemoryHeap, MemoryProperties, MemoryType, QueueCapabilities,
    QueueFamily,
};
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
            buffer_alignment: 16,
            max_job_time: Duration64::from_secs(10),
        },
    };
    Model::new(info, &[depth]).expect("a well-formed device")
}

fn buffer(model: &mut Model, context: ContextKey) -> BufferKey {
    let memory = model.allocate(context, 64, HOST).expect("memory");
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
    model.dispatch(now).expect("a job ready to run")
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
    let running: Vec<JobKey> = (0..3).map(|_| take(&mut model, 0).job).collect();
    assert!(model.dispatch(0).is_none(), "the engine is full");
    let begun = running[2];
    model.reset(0, 10, &|job| job == begun);
    assert_eq!(model.status(guilty), Err(Error::Lost(Loss::Overran)));
    assert_eq!(model.status(collateral), Err(Error::Lost(Loss::Reset)));
    assert_eq!(model.status(bystander), Ok(()));
    assert_eq!(reached(&model, bystander, spared), Ok(false));
    let again = take(&mut model, 20);
    assert_eq!(again.job, running[1], "the spared job runs again, first");
    assert!(model.dispatch(20).is_none());
    let _ = model.complete(again.job, false);
    assert_eq!(reached(&model, bystander, spared), Ok(true));
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
    let _ = model.complete(first.job, true);
    assert_eq!(model.status(doomed), Err(Error::Lost(Loss::Fault)));
    let woken = model.complete(second.job, false);
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
    let _ = job(&mut model, context, 0, target, &[], &[(timeline, 1)], 10);
    let _ = job(&mut model, context, 1, target, &[], &[(timeline, 2)], 10);
    let first = take(&mut model, 0);
    let second = take(&mut model, 0);
    let woken = model.complete(second.job, false);
    assert_eq!(model.timeline_value(context, timeline), Ok(0));
    assert_eq!(woken.len(), 1, "only the second queue's fence moved");
    let woken = model.complete(first.job, false);
    assert_eq!(model.timeline_value(context, timeline), Ok(2));
    assert_eq!(woken.len(), 2);
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
    while let Some(dispatch) = model.dispatch(0) {
        let _ = model.complete(dispatch.job, false);
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
    assert!(model.dispatch(0).is_none(), "the first job is not ready");
    let _ = model.signal(context, gate, 1).expect("advances");
    let first = take(&mut model, 0);
    let second = take(&mut model, 0);
    assert_ne!(first.job, second.job);
    assert!(model.dispatch(0).is_none());
}

#[test]
fn memory_the_cpu_cannot_reach_is_refused_to_it() {
    let mut model = model(1, 1);
    let context = model.create_context(None).expect("a context");
    let device_only = model.allocate(context, 64, DEVICE_ONLY).expect("memory");
    let host = model.allocate(context, 64, HOST).expect("memory");
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
    let _ = model.complete(running.job, false);
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
    for info in [missing_heap, no_queues, crowded, odd_alignment, no_deadline] {
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
    assert!(model.complete(running.job, false).is_empty());
    assert_eq!(model.status(gone), Err(Error::Destroyed));
    let next = take(&mut model, 0);
    let _ = model.complete(next.job, false);
    assert_eq!(reached(&model, staying, kept), Ok(true));
    assert!(
        model.dispatch(0).is_none(),
        "the released context's queued job went with it"
    );
}
