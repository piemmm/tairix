//! The engine's 3D work against the model device made to offer Venus.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::dma::DmaHost;
use tairix_abi::driver::RegionCaching;
use tairix_abi::hwtree::HwResource;
use tairix_rt::sync::Mutex;
use tairix_virtio::{MockRegion, MockWait, SharedMemory};

use super::*;
use crate::venus::desk::{Answer, Desk, Order, Payload, Rouse};
use crate::venus::display::Counter;
use crate::wire::BlobSpec;

/// Bytes of the model's host-visible region.
const REGION: u64 = 64 << 20;

const PAGE: u64 = 4096;

/// The Venus capset the model lists: 160 bytes, as virglrenderer's is.
fn venus_capset() -> Vec<u8> {
    (0..160u8).collect()
}

impl Rig {
    /// A device offering Venus beside two capsets before it, blob resources,
    /// contexts for a capset, and a host-visible region, with a control queue
    /// as deep as QEMU's 3D device lets it be.
    fn venus() -> Self {
        let rig = Self::new();
        {
            let mut model = rig.model();
            model.capsets = vec![
                Capset {
                    id: 1,
                    version: 1,
                    data: vec![1; 308],
                },
                Capset {
                    id: 2,
                    version: 2,
                    data: vec![2; 780],
                },
                Capset {
                    id: wire::CAPSET_VENUS,
                    version: 0,
                    data: venus_capset(),
                },
            ];
            model.map_cache = 1;
        }
        Self {
            features: TRANSPORT_FEATURES
                | wire::FEATURE_VIRGL
                | wire::FEATURE_RESOURCE_BLOB
                | wire::FEATURE_CONTEXT_INIT,
            queue_max: 256,
            capsets: 3,
            host_visible: Some(REGION),
            ..rig
        }
    }
}

/// Take the device's answers, and the one completion they hold, `tag`'s.
fn completed(gpu: &mut Gpu<'_>, tag: u64) -> Result<Done, DriverError> {
    gpu.poll().expect("answers taken");
    let completion = gpu.completion().expect("a completion");
    assert_eq!(completion.tag, tag);
    assert_eq!(gpu.completion(), None, "one answer alone");
    completion.outcome
}

/// A live context, made under tag 1.
fn context(gpu: &mut Gpu<'_>) -> Context {
    let context = gpu.create_context(b"test", 1).expect("asked");
    assert_eq!(completed(gpu, 1), Ok(Done::Done));
    context
}

/// A live blob of `pages` pages context `context` made under tag 2.
fn blob(gpu: &mut Gpu<'_>, context: Context, pages: u64, mappable: bool) -> Blob {
    let spec = BlobSpec {
        blob_id: 0,
        size: pages * PAGE,
        mappable,
        shareable: false,
    };
    let blob = gpu.create_blob(context, spec, 2).expect("asked");
    assert_eq!(completed(gpu, 2), Ok(Done::Done));
    blob
}

#[test]
fn a_device_offering_venus_has_its_capset_read_at_bring_up() {
    let rig = Rig::venus();
    let (gpu, _) = rig.open();
    let capset = venus_capset();
    assert_eq!(gpu.venus(), Some((0, capset.as_slice())));
    assert_eq!(
        rig.records.all(),
        vec![
            Record::ConsoleShown {
                width: 1280,
                height: 800
            },
            Record::Venus { version: 0 }
        ]
    );
    assert_eq!(
        rig.model.borrow().capset_infos_asked,
        3,
        "the list read as far as Venus"
    );
}

#[test]
fn a_device_short_of_what_venus_needs_draws_in_2d_alone() {
    let without_contexts = || Rig {
        features: TRANSPORT_FEATURES | wire::FEATURE_VIRGL | wire::FEATURE_RESOURCE_BLOB,
        ..Rig::venus()
    };
    let without_region = || Rig {
        host_visible: None,
        ..Rig::venus()
    };
    let without_venus = || {
        let rig = Rig::venus();
        rig.model().capsets.truncate(2);
        rig
    };
    let oversized = || {
        let rig = Rig::venus();
        rig.model().capsets[2].data = vec![0; wire::CAPSET_MAX + 1];
        rig
    };
    let refusing = || {
        let rig = Rig::venus();
        rig.model().refuse = Some((0x0108, ERR_UNSPEC));
        rig
    };
    let cases: [(&dyn Fn() -> Rig, NoVenus); 5] = [
        (&without_contexts, NoVenus::Features),
        (&without_region, NoVenus::HostVisibleRegion),
        (&without_venus, NoVenus::Capset),
        (&oversized, NoVenus::Capset),
        (&refusing, NoVenus::Refused(DriverError::DeviceFault)),
    ];
    for (rig, why) in cases {
        let rig = rig();
        let (mut gpu, _) = rig.open();
        assert_eq!(gpu.venus(), None, "{why:?}");
        assert_eq!(
            rig.records.all().last(),
            Some(&Record::NoVenus { why }),
            "{why:?}"
        );
        assert_eq!(
            gpu.create_context(b"none", 1),
            Err(DriverError::Unsupported)
        );
        assert_eq!(gpu.completion(), None);
        assert!(gpu.output().is_some(), "the display is served all the same");
    }
}

#[test]
fn a_device_claiming_endless_capsets_is_read_only_so_far() {
    let rig = Rig {
        capsets: 1000,
        ..Rig::venus()
    };
    rig.model().capsets.clear();
    let (gpu, _) = rig.open();
    assert_eq!(gpu.venus(), None);
    assert_eq!(rig.model.borrow().capset_infos_asked, 16);
}

#[test]
fn a_context_is_named_only_once_made_and_gone_once_destroyed() {
    let rig = Rig::venus();
    let (mut gpu, _) = rig.open();
    let context = gpu.create_context(b"first", 7).expect("asked");
    assert_eq!(
        gpu.submit_3d(context, 0, Stream::EMPTY, 8),
        Err(DriverError::Busy),
        "not yet answered"
    );
    assert_eq!(completed(&mut gpu, 7), Ok(Done::Done));
    assert_eq!(
        rig.model.borrow().contexts.get(&1),
        Some(&(wire::CAPSET_VENUS, b"first".to_vec()))
    );
    gpu.destroy_context(context, 9).expect("asked");
    assert_eq!(
        gpu.submit_3d(context, 0, Stream::EMPTY, 10),
        Err(DriverError::Busy),
        "being ended"
    );
    assert_eq!(completed(&mut gpu, 9), Ok(Done::Done));
    assert!(rig.model.borrow().contexts.is_empty());
    assert_eq!(
        gpu.submit_3d(context, 0, Stream::EMPTY, 11),
        Err(DriverError::NotFound),
        "a destroyed context names nothing"
    );
    let again = gpu.create_context(&[b'x'; 80], 12).expect("asked");
    assert_ne!(again, context, "its slot reused under a fresh generation");
    assert_eq!(completed(&mut gpu, 12), Ok(Done::Done));
    assert_eq!(gpu.destroy_context(context, 13), Err(DriverError::NotFound));
    assert_eq!(
        rig.model
            .borrow()
            .contexts
            .get(&1)
            .map(|(_, name)| name.len()),
        Some(wire::CONTEXT_NAME_MAX),
        "a name past its field is cut"
    );
}

#[test]
fn a_context_is_not_destroyed_while_a_submission_to_it_is_out() {
    let rig = Rig::venus();
    let (mut gpu, transport) = rig.open();
    let busy = context(&mut gpu);
    let idle = context(&mut gpu);
    rig.model().hold_submits = true;
    gpu.submit_3d(busy, 3, Stream::EMPTY, 51).expect("asked");
    assert_eq!(gpu.destroy_context(busy, 52), Err(DriverError::Busy));
    gpu.destroy_context(idle, 53)
        .expect("another context's submission holds this one back nothing");
    assert_eq!(completed(&mut gpu, 53), Ok(Done::Done));
    transport
        .borrow_mut()
        .answer_held(wire::CONTROL_QUEUE, 0, answer_done)
        .expect("held");
    assert_eq!(completed(&mut gpu, 51), Ok(Done::Done));
    gpu.destroy_context(busy, 54).expect("nothing out");
    assert_eq!(completed(&mut gpu, 54), Ok(Done::Done));
    assert!(rig.model.borrow().contexts.is_empty());
}

#[test]
fn a_context_the_device_refuses_is_let_go() {
    let rig = Rig::venus();
    let (mut gpu, _) = rig.open();
    rig.model().refuse = Some((CTX_CREATE, ERR_UNSPEC));
    let context = gpu.create_context(b"refused", 3).expect("asked");
    assert_eq!(completed(&mut gpu, 3), Err(DriverError::DeviceFault));
    assert_eq!(
        gpu.submit_3d(context, 0, Stream::EMPTY, 4),
        Err(DriverError::NotFound)
    );
}

#[test]
fn a_blob_is_made_mapped_unmapped_and_destroyed_against_the_device() {
    let rig = Rig::venus();
    let (mut gpu, _) = rig.open();
    let context = context(&mut gpu);
    let spec = BlobSpec {
        blob_id: 0,
        size: 3 * PAGE,
        mappable: true,
        shareable: false,
    };
    let blob = gpu.create_blob(context, spec, 2).expect("asked");
    assert_eq!(
        gpu.map_blob(blob, 0, 3),
        Err(DriverError::Busy),
        "not yet made"
    );
    assert_eq!(completed(&mut gpu, 2), Ok(Done::Done));
    let id = blob.resource_id().expect("an id");
    assert!(id >= 0x1_0000, "above every presenter's resource");
    assert_eq!(
        rig.model.borrow().blobs.get(&id),
        Some(&HostBlob {
            context: 1,
            blob_id: 0,
            size: 3 * PAGE,
            flags: 1,
            mapped: None,
        })
    );
    assert_eq!(
        gpu.map_blob(blob, PAGE + 1, 3),
        Err(DriverError::OutOfRange),
        "off a page"
    );
    assert_eq!(
        gpu.map_blob(blob, REGION - PAGE, 3),
        Err(DriverError::OutOfRange),
        "past the region"
    );
    gpu.map_blob(blob, 8 * PAGE, 3).expect("asked");
    assert_eq!(
        completed(&mut gpu, 3),
        Ok(Done::Mapped(RegionCaching::WriteBack))
    );
    assert_eq!(rig.model.borrow().blobs[&id].mapped, Some(8 * PAGE));
    assert_eq!(
        gpu.destroy_blob(blob, 4),
        Err(DriverError::Busy),
        "a mapped blob is unmapped first"
    );
    assert_eq!(gpu.map_blob(blob, 0, 4), Err(DriverError::Busy));
    gpu.unmap_blob(blob, 5).expect("asked");
    assert_eq!(completed(&mut gpu, 5), Ok(Done::Done));
    assert_eq!(rig.model.borrow().blobs[&id].mapped, None);
    gpu.destroy_blob(blob, 6).expect("asked");
    assert_eq!(completed(&mut gpu, 6), Ok(Done::Done));
    assert!(rig.model.borrow().blobs.is_empty());
    assert_eq!(gpu.map_blob(blob, 0, 7), Err(DriverError::NotFound));
}

#[test]
fn a_blob_takes_the_caching_its_device_states() {
    for (stated, caching) in [
        (2, RegionCaching::Uncached),
        (3, RegionCaching::WriteCombine),
    ] {
        let rig = Rig::venus();
        let (mut gpu, _) = rig.open();
        let context = context(&mut gpu);
        let blob = blob(&mut gpu, context, 1, true);
        rig.model().map_cache = stated;
        gpu.map_blob(blob, 0, 3).expect("asked");
        assert_eq!(completed(&mut gpu, 3), Ok(Done::Mapped(caching)));
    }
}

#[test]
fn a_mapping_that_states_no_caching_leaves_its_blob_unusable() {
    let rig = Rig::venus();
    let (mut gpu, _) = rig.open();
    let context = context(&mut gpu);
    let blob = blob(&mut gpu, context, 1, true);
    rig.model().map_cache = 0;
    gpu.map_blob(blob, 0, 3).expect("asked");
    assert_eq!(completed(&mut gpu, 3), Err(DriverError::DeviceFault));
    assert_eq!(gpu.unmap_blob(blob, 4), Err(DriverError::NotFound));
    assert_eq!(gpu.destroy_blob(blob, 5), Err(DriverError::NotFound));
}

#[test]
fn a_map_answer_cut_short_of_its_caching_leaves_its_blob_unusable() {
    let rig = Rig::venus();
    let (mut gpu, _) = rig.open();
    let context = context(&mut gpu);
    let blob = blob(&mut gpu, context, 1, true);
    let header = u32::try_from(wire::HEADER_LEN).expect("small");
    rig.model().short_answers = Some((MAP_BLOB, header));
    gpu.map_blob(blob, 0, 3).expect("asked");
    assert_eq!(completed(&mut gpu, 3), Err(DriverError::DeviceFault));
    assert_eq!(
        rig.model
            .borrow()
            .blobs
            .values()
            .next()
            .and_then(|blob| blob.mapped),
        Some(0),
        "the device mapped it all the same"
    );
    assert_eq!(gpu.unmap_blob(blob, 4), Err(DriverError::NotFound));
}

#[test]
fn a_capset_cut_short_of_its_size_is_no_venus() {
    let rig = Rig::venus();
    let header = u32::try_from(wire::HEADER_LEN).expect("small");
    rig.model().short_answers = Some((0x0109, header + 159));
    let (gpu, _) = rig.open();
    assert_eq!(gpu.venus(), None);
    assert_eq!(
        rig.records.all().last(),
        Some(&Record::NoVenus {
            why: NoVenus::Refused(DriverError::DeviceFault)
        })
    );
}

#[test]
fn an_unmappable_blob_is_never_mapped() {
    let rig = Rig::venus();
    let (mut gpu, _) = rig.open();
    let context = context(&mut gpu);
    let blob = blob(&mut gpu, context, 1, false);
    assert_eq!(gpu.map_blob(blob, 0, 3), Err(DriverError::PermissionDenied));
    assert_eq!(
        gpu.create_blob(
            context,
            BlobSpec {
                blob_id: 0,
                size: 0,
                mappable: true,
                shareable: false
            },
            4
        ),
        Err(DriverError::LengthOutOfRange)
    );
}

#[test]
fn a_submission_carries_its_stream_and_its_ring() {
    let rig = Rig::venus();
    let (mut gpu, _) = rig.open();
    let context = context(&mut gpu);
    let mut slab = rig.host.alloc_dma_zeroed(8).expect("dma");
    slab.as_bytes_mut()
        .copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    let stream = Stream {
        device_addr: slab.device_addr(),
        len: 8,
    };
    gpu.submit_3d(context, 5, stream, 20).expect("asked");
    assert_eq!(completed(&mut gpu, 20), Ok(Done::Done));
    gpu.submit_3d(context, 0, Stream::EMPTY, 21).expect("asked");
    assert_eq!(completed(&mut gpu, 21), Ok(Done::Done));
    assert_eq!(
        rig.model.borrow().submitted,
        vec![
            Submitted {
                context: 1,
                ring: Some(5),
                stream: vec![1, 2, 3, 4, 5, 6, 7, 8],
            },
            Submitted {
                context: 1,
                ring: Some(0),
                stream: Vec::new(),
            }
        ]
    );
    for (ring, stream, refused) in [
        (wire::RINGS, Stream::EMPTY, DriverError::OutOfRange),
        (
            0,
            Stream { len: 6, ..stream },
            DriverError::LengthOutOfRange,
        ),
        (
            0,
            Stream {
                len: SUBMIT_MAX + 4,
                ..stream
            },
            DriverError::LengthOutOfRange,
        ),
    ] {
        assert_eq!(gpu.submit_3d(context, ring, stream, 22), Err(refused));
    }
}

#[test]
fn answers_in_any_order_each_reach_their_own_sender() {
    let rig = Rig::venus();
    let (mut gpu, transport) = rig.open();
    let context = context(&mut gpu);
    rig.model().hold_submits = true;
    for tag in [31, 32, 33] {
        gpu.submit_3d(context, 1, Stream::EMPTY, tag)
            .expect("asked");
    }
    assert_eq!(transport.borrow().held(wire::CONTROL_QUEUE), 3);
    gpu.poll().expect("answers taken");
    assert_eq!(gpu.completion(), None, "none answered yet");
    // The third answered first, then the first, then the second.
    for (at, tag) in [(2, 33), (0, 31), (0, 32)] {
        transport
            .borrow_mut()
            .answer_held(wire::CONTROL_QUEUE, at, answer_done)
            .expect("held");
        assert_eq!(completed(&mut gpu, tag), Ok(Done::Done));
    }
}

#[test]
fn answers_taken_together_complete_in_the_order_sent() {
    let rig = Rig::venus();
    let (mut gpu, transport) = rig.open();
    let context = context(&mut gpu);
    rig.model().hold_submits = true;
    for tag in [41, 42] {
        gpu.submit_3d(context, 2, Stream::EMPTY, tag)
            .expect("asked");
    }
    for at in [1, 0] {
        transport
            .borrow_mut()
            .answer_held(wire::CONTROL_QUEUE, at, answer_done)
            .expect("held");
    }
    gpu.poll().expect("answers taken");
    let tags: Vec<u64> = core::iter::from_fn(|| gpu.completion())
        .map(|completion| completion.tag)
        .collect();
    assert_eq!(tags, [41, 42]);
}

#[test]
fn a_long_fence_holds_no_display_request_back() {
    let rig = Rig::venus();
    let (mut gpu, transport) = rig.open();
    let context = context(&mut gpu);
    rig.model().hold_submits = true;
    gpu.submit_3d(context, 1, Stream::EMPTY, 50).expect("asked");
    let (frame, _) = present_frame(&mut gpu, &rig);
    assert_eq!(
        gpu.present(frame.id, &[rect(0, 0, 8, 8)]),
        Ok(Presented::Shown)
    );
    let pointer = gpu.alloc_buffer(16, 16).expect("a pointer image");
    gpu.set_cursor(pointer.id, 1, 1)
        .expect("the pointer's transfer is not held back either");
    raise_display_event(&transport);
    gpu.service_events().expect("serviced");
    assert_eq!(gpu.completion(), None, "the long one is still the device's");
    transport
        .borrow_mut()
        .answer_held(wire::CONTROL_QUEUE, 0, answer_done)
        .expect("held");
    gpu.service_events().expect("serviced");
    assert_eq!(
        gpu.completion(),
        Some(Completion {
            tag: 50,
            outcome: Ok(Done::Done)
        })
    );
}

#[test]
fn a_wait_for_one_command_ends_on_its_own_answer() {
    let rig = Rig::venus();
    let (mut gpu, transport) = rig.open();
    let context = context(&mut gpu);
    rig.model().hold_submits = true;
    gpu.submit_3d(context, 1, Stream::EMPTY, 70).expect("asked");
    rig.host.script_waits([MockWait::Silent]);
    assert_eq!(gpu.wait_for(70, 1_000), Err(DriverError::DeviceOffline));
    assert_eq!(gpu.completion(), None, "still the device's");
    transport
        .borrow_mut()
        .answer_held(wire::CONTROL_QUEUE, 0, answer_done)
        .expect("held");
    assert_eq!(gpu.wait_for(70, 1_000), Ok(()));
    assert_eq!(
        gpu.completion(),
        Some(Completion {
            tag: 70,
            outcome: Ok(Done::Done)
        })
    );
    assert_eq!(
        gpu.wait_for(71, 1_000),
        Ok(()),
        "a tag no command out carries costs no wait"
    );
}

#[test]
fn three_d_work_cannot_take_the_display_paths_room() {
    let rig = Rig::venus();
    let (mut gpu, transport) = rig.open();
    let context = context(&mut gpu);
    rig.model().hold_submits = true;
    let mut sent = 0;
    while gpu.submit_3d(context, 1, Stream::EMPTY, 100 + sent).is_ok() {
        sent += 1;
    }
    assert!(sent > 0);
    assert_eq!(
        gpu.submit_3d(context, 1, Stream::EMPTY, 0),
        Err(DriverError::Busy)
    );
    let (frame, _) = present_frame(&mut gpu, &rig);
    assert_eq!(
        gpu.present(frame.id, &[rect(0, 0, 8, 8)]),
        Ok(Presented::Shown),
        "the display path keeps its own room"
    );
    while transport.borrow().held(wire::CONTROL_QUEUE) > 0 {
        transport
            .borrow_mut()
            .answer_held(wire::CONTROL_QUEUE, 0, answer_done)
            .expect("held");
    }
    gpu.poll().expect("answers taken");
    assert_eq!(
        gpu.submit_3d(context, 1, Stream::EMPTY, 0),
        Err(DriverError::Busy),
        "answers not yet read hold their room"
    );
    let read = core::iter::from_fn(|| gpu.completion()).count();
    assert_eq!(read, usize::try_from(sent).expect("small"));
    gpu.submit_3d(context, 1, Stream::EMPTY, 0)
        .expect("room once read");
}

#[test]
fn an_answer_without_its_fence_leaves_what_it_named_unknown() {
    let rig = Rig::venus();
    let (mut gpu, _) = rig.open();
    rig.model().drops_fences = true;
    let context = gpu.create_context(b"lost", 60).expect("asked");
    assert_eq!(completed(&mut gpu, 60), Err(DriverError::DeviceFault));
    rig.model().drops_fences = false;
    assert_eq!(
        gpu.submit_3d(context, 0, Stream::EMPTY, 61),
        Err(DriverError::NotFound)
    );
    assert_eq!(gpu.destroy_context(context, 62), Err(DriverError::NotFound));
    let next = gpu.create_context(b"next", 63).expect("asked");
    assert_eq!(completed(&mut gpu, 63), Ok(Done::Done));
    assert_eq!(
        rig.model
            .borrow()
            .contexts
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [1, 2],
        "the unknown one's id is never given again"
    );
    assert_ne!(next, context);
}

/// A rouse the test needs nothing of: it serves the desk itself.
struct Quiet;

impl Rouse for Quiet {
    fn rouse(&self) -> bool {
        true
    }
}

/// What the display end answers `order` with, carried out against the model.
fn through_desk(gpu: &mut Gpu<'_>, desk: &Desk<u32>, order: Order) -> Answer {
    assert!(desk.order(order, 0).is_ok());
    gpu.serve_desk();
    gpu.poll().expect("answers taken");
    gpu.serve_desk();
    desk.next_answer().expect("answered").1
}

#[test]
fn a_part_goes_back_only_where_the_device_maps_nothing_into_it() {
    let mapper = MockRegion::new(1 << 20);
    let rig = Rig::venus();
    let (mut gpu, _) = rig.open();
    let desk = Arc::new(Desk::new(Box::new(Quiet)));
    let region = HwResource::memory_region(0x1_0000_0000, REGION, 1);
    let shared = Arc::new(Mutex::new(SharedMemory::new(region).expect("a region")));
    let counter: Arc<dyn Counter> = Arc::clone(&desk) as Arc<dyn Counter>;
    gpu.attach_desk(counter, Arc::clone(&shared), &mapper, None)
        .expect("attached");
    let context = context(&mut gpu);
    let part = || shared.lock().reserve(PAGE).expect("a part");

    let first = blob(&mut gpu, context, 1, true);
    let Answer::Mapped(lent) = through_desk(&mut gpu, &desk, Order::MapBlob(first, part())) else {
        panic!("mapped");
    };
    let Answer::Unmapped(lent) = through_desk(&mut gpu, &desk, Order::UnmapBlob(first, lent))
    else {
        panic!("unmapped");
    };
    assert!(shared.lock().release(lent).is_ok());

    // A map the device refused left nothing mapped.
    rig.model().refuse = Some((MAP_BLOB, ERR_INVALID_PARAMETER));
    let Answer::Refused(_, Some(lent)) =
        through_desk(&mut gpu, &desk, Order::MapBlob(first, part()))
    else {
        panic!("a refused map's part goes back");
    };
    assert!(shared.lock().release(lent).is_ok());

    // An unmap the device refused left the blob mapped.
    let Answer::Mapped(lent) = through_desk(&mut gpu, &desk, Order::MapBlob(first, part())) else {
        panic!("mapped");
    };
    let held = lent.offset();
    rig.model().refuse = Some((UNMAP_BLOB, ERR_INVALID_PARAMETER));
    let kept = through_desk(&mut gpu, &desk, Order::UnmapBlob(first, lent));
    assert!(
        matches!(kept, Answer::Refused(_, None)),
        "a refused unmap keeps its part"
    );

    // A map answered with what the driver cannot read may have mapped.
    let doubtful = blob(&mut gpu, context, 1, true);
    rig.model().short_answers = Some((MAP_BLOB, u32::try_from(wire::HEADER_LEN).expect("small")));
    let unknown = through_desk(&mut gpu, &desk, Order::MapBlob(doubtful, part()));
    assert!(
        matches!(unknown, Answer::Refused(DriverError::DeviceFault, None)),
        "an unread map keeps its part"
    );

    // Neither part kept is handed out again.
    let next = part();
    assert!(
        next.offset() != held && next.offset() != held + PAGE,
        "{}",
        next.offset()
    );
}

#[test]
fn a_3d_answer_a_display_wait_takes_is_handed_to_the_desk_before_the_loop_parks() {
    let mapper = MockRegion::new(1 << 20);
    let rig = Rig::venus();
    let (mut gpu, transport) = rig.open();
    let desk = Arc::new(Desk::new(Box::new(Quiet)));
    let region = HwResource::memory_region(0x1_0000_0000, REGION, 1);
    let shared = Arc::new(Mutex::new(SharedMemory::new(region).expect("a region")));
    let counter: Arc<dyn Counter> = Arc::clone(&desk) as Arc<dyn Counter>;
    gpu.attach_desk(counter, shared, &mapper, None)
        .expect("attached");
    let context = context(&mut gpu);
    // The device answers on its interrupt from here on, as QEMU's 3D one does.
    transport.borrow_mut().set_synchronous_notify(false);
    rig.host.attach(&transport);
    let submit = Order::Submit {
        context,
        ring: 0,
        payload: Payload::new(),
    };
    assert!(desk.order(submit, 0).is_ok());
    gpu.serve_desk();
    assert!(!desk.has_answers());

    // The console's rows are moved and waited for, the submission's answer
    // coming on the same interrupt.
    let flushed = rig.model.borrow().flushes.len();
    rig.console.0.borrow_mut().damage = Some(rect(0, 16, 1280, 32));
    gpu.service_events().expect("serviced");
    assert_eq!(rig.model.borrow().flushes.len(), flushed + 1);
    assert!(
        desk.has_answers(),
        "nothing else rouses the loop for an answer its wait took"
    );
    assert!(matches!(desk.next_answer(), Some((0, Answer::Done))));
}
