//! The device engine's alternate settings and isochronous streams, driven
//! against the mock controller's isochronous endpoint model.

use super::*;
use crate::transport::IsoSlotDone;
use alloc::vec;
use tairix_abi::usb_urb::{IsoLayout, IsoPacket, IsoPacketStatus, UsbSpeed};
use tairix_abi::HwDeviceClass;

/// A USB Audio function on a high-speed device: a control interface with no
/// endpoint, and a streaming interface whose setting 1 brings an
/// asynchronous OUT data endpoint paced by an IN feedback endpoint, and
/// whose setting 2 brings a larger OUT endpoint alone; then two vendor
/// interfaces, one published and one left to claim.
static AUDIO_CONFIG: [u8; 136] = [
    // Configuration: wTotalLength 136, 4 interfaces.
    9, 0x02, 136, 0, 4, 1, 0, 0x80, 50, //
    // Interface 0, setting 0: Audio Control, no endpoint.
    9, 0x04, 0, 0, 0, 0x01, 0x01, 0x00, 0, //
    // Class-specific AC header naming streaming interface 1.
    9, 0x24, 0x01, 0x00, 0x01, 30, 0, 1, 1, //
    // Interface 1, setting 0: Audio Streaming, zero bandwidth.
    9, 0x04, 1, 0, 0, 0x01, 0x02, 0x00, 0, //
    // Interface 1, setting 1: two endpoints.
    9, 0x04, 1, 1, 2, 0x01, 0x02, 0x00, 0, //
    7, 0x24, 0x01, 1, 1, 1, 0, //
    // EP1 OUT, asynchronous isochronous data, 200 bytes, every 8 microframes,
    // paced by EP1 IN.
    9, 0x05, 0x01, 0x05, 200, 0, 4, 0, 0x81, //
    7, 0x25, 0x01, 0, 0, 0, 0, //
    // EP1 IN, isochronous feedback, 4 bytes, every 8 microframes.
    9, 0x05, 0x81, 0x11, 4, 0, 4, 0, 0, //
    // Interface 1, setting 2: one larger OUT endpoint.
    9, 0x04, 1, 2, 1, 0x01, 0x02, 0x00, 0, //
    7, 0x24, 0x01, 1, 1, 1, 0, //
    9, 0x05, 0x01, 0x09, 0x90, 0x01, 4, 0, 0, //
    7, 0x25, 0x01, 0, 0, 0, 0, //
    // A vendor interface driven over the control endpoint alone: published.
    9, 0x04, 2, 0, 0, 0xFF, 0x00, 0x00, 0, //
    // A vendor interface with two settings that bring nothing: claimable.
    9, 0x04, 3, 0, 0, 0xFF, 0x00, 0x00, 0, //
    9, 0x04, 3, 1, 0, 0xFF, 0x00, 0x00, 0, //
];

/// The OUT data endpoint and the IN feedback endpoint of setting 1.
const DATA: u8 = 0x01;
const FEEDBACK: u8 = 0x81;

/// Enumerate the audio function on root port 1.
fn audio_device(mem: &SharedMem) -> UsbDevice<'static, ModelXhci, MockDma> {
    let mut mock = MockXhci::with_device(mem);
    mock.keyboard_config = &AUDIO_CONFIG;
    mock.mfindex = 1000;
    let mut device = started_device(mock, mem);
    assert_eq!(attach_root_device(&mut device, 1), Ok(0));
    device
}

/// The audio function with streaming interface 1 claimed and setting 1
/// selected.
fn streaming_device(mem: &SharedMem) -> UsbDevice<'static, ModelXhci, MockDma> {
    let mut device = audio_device(mem);
    let mut engine = device.engine_for(0);
    engine
        .claim_interface(1)
        .expect("the streaming interface is free");
    engine.set_interface(1, 1).expect("the bus has room");
    device
}

fn geometry(slots: u16, packets: u16, packet_bytes: u32) -> IsoLayout {
    IsoLayout::new(slots, packets, packet_bytes).expect("a valid layout")
}

/// A region for `layout` whose OUT slot `slot` carries `lengths`, each
/// interval's data its packet index repeated.
fn out_region(layout: IsoLayout, slot: u16, lengths: &[u32]) -> Vec<u8> {
    let mut region = vec![0u8; layout.region_len()];
    for (packet, &length) in (0u16..).zip(lengths) {
        layout
            .set_record(
                &mut region,
                slot,
                packet,
                IsoPacket {
                    length,
                    status: IsoPacketStatus::Moved,
                },
            )
            .expect("in the layout");
        let data = layout
            .data_mut(&mut region, slot, packet)
            .expect("in the layout");
        let length = usize::try_from(length).expect("small").min(data.len());
        data[..length].fill(u8::try_from(packet).expect("small"));
    }
    region
}

#[test]
fn an_audio_function_publishes_its_control_interface_and_leaves_streaming_to_claim() {
    let mem = shared_mem();
    let device = audio_device(&mem);
    let identity = device.device_identity(0).expect("the control interface");
    assert_eq!(identity.interface_number, 0);
    assert_eq!(identity.interface_class, 0x01_01_00);
    assert_eq!(
        device.describe_device(0, 0, 9).expect("described").class(),
        Some(HwDeviceClass::Audio)
    );
    let vendor = device
        .device_identity(1)
        .expect("the control-only vendor interface");
    assert_eq!(vendor.interface_number, 2);
    assert!(
        !device.device_live(2),
        "neither interface with settings to choose has a node"
    );
}

#[test]
fn a_sibling_interface_is_claimed_once_and_only_if_it_exists() {
    let mem = shared_mem();
    let mut device = audio_device(&mem);
    let mut engine = device.engine_for(0);
    assert_eq!(engine.claim_interface(1), Ok(()));
    assert_eq!(
        engine.claim_interface(1),
        Ok(()),
        "claiming again is no change"
    );
    assert_eq!(engine.claim_interface(0), Ok(()), "its own interface");
    assert_eq!(engine.claim_interface(7), Err(DriverError::NotFound));
    assert_eq!(
        engine.claim_interface(2),
        Err(DriverError::AlreadyExists),
        "another node serves it"
    );
    assert_eq!(engine.claim_interface(3), Ok(()));
    let scope = engine.scope().expect("served");
    assert!(scope.governs(0) && scope.governs(1) && scope.governs(3) && !scope.governs(2));
    assert_eq!(
        device.engine_for(1).claim_interface(1),
        Err(DriverError::AlreadyExists),
        "node 0 claimed it"
    );
}

#[test]
fn selecting_a_setting_reserves_its_endpoints_before_the_device_is_told() {
    let mem = shared_mem();
    let mut device = audio_device(&mem);
    let mut engine = device.engine_for(0);
    assert_eq!(
        engine.set_interface(1, 1),
        Err(DriverError::NotFound),
        "not claimed"
    );
    engine.claim_interface(1).expect("free");
    assert_eq!(engine.set_interface(1, 1), Ok(()));
    let endpoints = engine.scope().expect("served").endpoints;
    assert_eq!(endpoints, 1 << 2 | 1 << 3);
    let mock = device.host_mut().model_mut();
    assert_eq!(mock.set_interfaces, [(1, 1)]);
    let mut iso = mock.iso.clone();
    iso.sort_by_key(|iso| iso.dci);
    let shape: Vec<_> = iso
        .iter()
        .map(|iso| (iso.dci, iso.ep_type, iso.max_packet, iso.esit, iso.interval))
        .collect();
    assert_eq!(shape, [(2, 1, 200, 200, 3), (3, 5, 4, 4, 3)]);
    assert!(iso.iter().all(|iso| iso.error_count == 0 && iso.mult == 0));
    assert!(iso.iter().all(|iso| iso.max_burst == 0));
}

#[test]
fn a_setting_the_bus_cannot_schedule_is_refused_and_leaves_nothing_behind() {
    let mem = shared_mem();
    let mut device = audio_device(&mem);
    let chunks = device.dma_ref().chunks.len();
    device.host_mut().model_mut().iso_configure_refusal = Some(CompletionCode::BandwidthError);
    let mut engine = device.engine_for(0);
    engine.claim_interface(1).expect("free");
    assert_eq!(engine.set_interface(1, 1), Err(DriverError::NoBandwidth));
    assert_eq!(engine.scope().expect("served").endpoints, 0);
    let mock = device.host_mut().model_mut();
    assert!(mock.set_interfaces.is_empty(), "the device was never told");
    assert!(mock.iso.is_empty());
    assert_eq!(device.dma_ref().chunks.len(), chunks, "no ring stranded");
}

#[test]
fn a_device_refusing_a_setting_keeps_the_one_it_had() {
    let mem = shared_mem();
    let mut device = streaming_device(&mem);
    let chunks = device.dma_ref().chunks.len();
    device.host_mut().model_mut().stall_set_interface = true;
    assert_eq!(
        device.engine_for(0).set_interface(1, 2),
        Err(DriverError::EndpointStalled)
    );
    let mut iso = device.host_mut().model_mut().iso.clone();
    iso.sort_by_key(|iso| iso.dci);
    let held: Vec<_> = iso.iter().map(|iso| (iso.dci, iso.max_packet)).collect();
    assert_eq!(held, [(2, 200), (3, 4)], "setting 1 is back");
    assert_eq!(device.dma_ref().chunks.len(), chunks);
    assert_eq!(
        device.engine_for(0).scope().expect("served").endpoints,
        1 << 2 | 1 << 3
    );
}

#[test]
fn switching_settings_drops_the_old_endpoints_and_their_rings() {
    let mem = shared_mem();
    let mut device = audio_device(&mem);
    let baseline = device.dma_ref().chunks.len();
    let mut engine = device.engine_for(0);
    engine.claim_interface(1).expect("free");
    engine.set_interface(1, 1).expect("room");
    engine.set_interface(1, 2).expect("room");
    let held: Vec<_> = device
        .host_mut()
        .model_mut()
        .iso
        .iter()
        .map(|iso| (iso.dci, iso.max_packet))
        .collect();
    assert_eq!(held, [(2, 400)]);
    assert_eq!(device.dma_ref().chunks.len(), baseline + 1);
    device.engine_for(0).set_interface(1, 0).expect("room");
    assert!(device.host_mut().model_mut().iso.is_empty());
    assert_eq!(device.dma_ref().chunks.len(), baseline);
    assert_eq!(
        device.host_mut().model_mut().set_interfaces,
        [(1, 1), (1, 2), (1, 0)]
    );
}

#[test]
fn a_stream_places_each_interval_at_its_frame_and_raises_one_interrupt_a_slot() {
    let mem = shared_mem();
    let mut device = streaming_device(&mem);
    device.host_mut().model_mut().iso_hold = true;
    let layout = geometry(4, 4, 200);
    let shape = device
        .engine_for(0)
        .iso_start(DATA, layout)
        .expect("the stream fits");
    assert_eq!(shape.interval_microframes, 8);
    assert_eq!(shape.speed, UsbSpeed::High);
    let region = out_region(layout, 0, &[192, 192, 196, 0]);
    device
        .engine_for(0)
        .iso_queue(DATA, 0, &region)
        .expect("queued");
    let mock = device.host_mut().model_mut();
    // The mock's scheduling threshold is zero: 1000 + 8 rounds to frame 126.
    let tds: Vec<_> = mock
        .iso_tds
        .iter()
        .map(|td| (td.frame_id, td.length, td.ioc, td.bei, td.sia, td.trbs))
        .collect();
    assert_eq!(
        tds,
        [
            (126, 192, true, true, false, 1),
            (127, 192, true, true, false, 1),
            (128, 196, true, true, false, 1),
            (129, 0, true, false, false, 1),
        ]
    );
    assert!(mock.iso_tds.iter().all(|td| td.tbc == 0 && td.tlbpc == 0));
    let buffers: Vec<_> = mock
        .iso_tds
        .iter()
        .map(|td| (td.buffer, td.length))
        .collect();
    for (packet, (buffer, length)) in buffers.into_iter().enumerate() {
        let bytes = mock.read_mem(buffer, usize::try_from(length).expect("small"));
        assert!(bytes.iter().all(|&byte| usize::from(byte) == packet));
    }
    for _ in 0..4 {
        mock.complete_iso(CompletionCode::Success, 0);
    }
    device.pump_reports().expect("the ring drains");
    let mut region = vec![0u8; layout.region_len()];
    let done = device
        .engine_for(0)
        .iso_take(DATA, &mut region)
        .expect("the stream runs");
    assert_eq!(
        done,
        Some(IsoSlotDone {
            slot: 0,
            skipped: 0,
            microframe: 1008
        })
    );
    let records: Vec<_> = (0..4)
        .map(|packet| layout.record(&region, 0, packet).expect("written"))
        .collect();
    assert!(records
        .iter()
        .all(|record| record.status == IsoPacketStatus::Moved));
    assert_eq!(records[2].length, 196);
    assert_eq!(device.engine_for(0).iso_take(DATA, &mut region), Ok(None));
}

#[test]
fn an_in_stream_delivers_what_each_interval_received() {
    let mem = shared_mem();
    let mut device = streaming_device(&mem);
    device.host_mut().model_mut().iso_hold = true;
    let layout = geometry(2, 2, 4);
    assert_eq!(
        device.engine_for(0).iso_start(FEEDBACK, geometry(2, 2, 3)),
        Err(DriverError::OutOfRange),
        "an IN interval must hold the endpoint's whole budget"
    );
    device
        .engine_for(0)
        .iso_start(FEEDBACK, layout)
        .expect("fits");
    let region = vec![0u8; layout.region_len()];
    device
        .engine_for(0)
        .iso_queue(FEEDBACK, 1, &region)
        .expect("queued");
    let mock = device.host_mut().model_mut();
    let tds: Vec<_> = mock
        .iso_tds
        .iter()
        .map(|td| (td.buffer, td.length))
        .collect();
    assert_eq!(
        tds.iter().map(|&(_, length)| length).collect::<Vec<_>>(),
        [4, 4]
    );
    mock.write_mem(tds[0].0, &[0x66, 0x66, 0x0B]);
    mock.complete_iso(CompletionCode::ShortPacket, 1);
    mock.complete_iso(CompletionCode::UsbTransactionError, 0);
    device.pump_reports().expect("drains");
    let mut region = vec![0u8; layout.region_len()];
    let done = device
        .engine_for(0)
        .iso_take(FEEDBACK, &mut region)
        .expect("runs")
        .expect("finished");
    assert_eq!(done.slot, 1);
    assert_eq!(
        layout.record(&region, 1, 0),
        Ok(IsoPacket {
            length: 3,
            status: IsoPacketStatus::Moved
        })
    );
    assert_eq!(
        layout.data(&region, 1, 0).expect("laid out")[..3],
        [0x66, 0x66, 0x0B]
    );
    assert_eq!(
        layout.record(&region, 1, 1).map(|record| record.status),
        Ok(IsoPacketStatus::Failed)
    );
}

#[test]
fn a_late_slot_restarts_and_reports_the_gap() {
    let mem = shared_mem();
    let mut device = streaming_device(&mem);
    let layout = geometry(2, 4, 200);
    device.engine_for(0).iso_start(DATA, layout).expect("fits");
    let region = out_region(layout, 0, &[8; 4]);
    device
        .engine_for(0)
        .iso_queue(DATA, 0, &region)
        .expect("queued");
    device.pump_reports().expect("drains");
    let mut taken = vec![0u8; layout.region_len()];
    let first = device
        .engine_for(0)
        .iso_take(DATA, &mut taken)
        .expect("runs")
        .expect("finished");
    assert_eq!((first.microframe, first.skipped), (1008, 0));
    // The stream was due again at 1040; the class driver comes back at 2000.
    device.host_mut().model_mut().mfindex = 2000;
    let region = out_region(layout, 1, &[8; 4]);
    device
        .engine_for(0)
        .iso_queue(DATA, 1, &region)
        .expect("queued");
    device.pump_reports().expect("drains");
    let second = device
        .engine_for(0)
        .iso_take(DATA, &mut taken)
        .expect("runs")
        .expect("finished");
    assert_eq!((second.microframe, second.skipped), (2008, 121));
    let frames: Vec<_> = device
        .host_mut()
        .model_mut()
        .iso_tds
        .iter()
        .map(|td| td.frame_id)
        .collect();
    assert_eq!(frames, [126, 127, 128, 129, 251, 252, 253, 254]);
}

#[test]
fn a_slot_is_queued_once_and_only_with_records_its_intervals_can_carry() {
    let mem = shared_mem();
    let mut device = streaming_device(&mem);
    device.host_mut().model_mut().iso_hold = true;
    let layout = geometry(2, 2, 200);
    device.engine_for(0).iso_start(DATA, layout).expect("fits");
    let too_long = out_region(layout, 0, &[201, 8]);
    assert_eq!(
        device.engine_for(0).iso_queue(DATA, 0, &too_long),
        Err(DriverError::OutOfRange)
    );
    let region = out_region(layout, 0, &[8, 8]);
    assert_eq!(device.engine_for(0).iso_queue(DATA, 0, &region), Ok(()));
    assert_eq!(
        device.engine_for(0).iso_queue(DATA, 0, &region),
        Err(DriverError::Busy)
    );
    assert_eq!(
        device.engine_for(0).iso_queue(DATA, 2, &region),
        Err(DriverError::OutOfRange)
    );
    assert_eq!(
        device.engine_for(0).iso_queue(FEEDBACK, 0, &region),
        Err(DriverError::NotFound),
        "no stream runs there"
    );
}

#[test]
fn a_layout_must_fit_the_ring_and_the_controllers_window() {
    let mem = shared_mem();
    let mut device = streaming_device(&mem);
    // Built field by field: the ABI's own bounds refuse it, and the engine
    // must too for a layout that never passed through them.
    let oversized = IsoLayout {
        slots: 32,
        packets: 16,
        packet_bytes: 200,
    };
    assert_eq!(
        device.engine_for(0).iso_start(DATA, oversized),
        Err(DriverError::OutOfRange),
        "more intervals than one ring holds"
    );
    assert_eq!(
        device.engine_for(0).iso_start(0x02, geometry(2, 2, 200)),
        Err(DriverError::NotFound),
        "no selected setting brings it"
    );
}

#[test]
fn a_running_stream_holds_its_setting_until_stopped() {
    let mem = shared_mem();
    let mut device = streaming_device(&mem);
    device.host_mut().model_mut().iso_hold = true;
    let chunks = device.dma_ref().chunks.len();
    let layout = geometry(2, 2, 200);
    device.engine_for(0).iso_start(DATA, layout).expect("fits");
    assert_eq!(device.dma_ref().chunks.len(), chunks + 1, "the data chunk");
    let region = out_region(layout, 0, &[8, 8]);
    device
        .engine_for(0)
        .iso_queue(DATA, 0, &region)
        .expect("queued");
    assert_eq!(
        device.engine_for(0).set_interface(1, 0),
        Err(DriverError::Busy)
    );
    assert_eq!(device.engine_for(0).iso_stop(DATA), Ok(()));
    assert_eq!(
        device.dma_ref().chunks.len(),
        chunks,
        "its buffers went back"
    );
    let mut taken = vec![0u8; layout.region_len()];
    assert_eq!(
        device.engine_for(0).iso_take(DATA, &mut taken),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        device.engine_for(0).iso_stop(DATA),
        Err(DriverError::NotFound)
    );
    // The repositioned ring serves the next stream from its start.
    device.engine_for(0).iso_start(DATA, layout).expect("fits");
    device
        .engine_for(0)
        .iso_queue(DATA, 1, &out_region(layout, 1, &[8, 8]))
        .expect("queued");
    assert_eq!(device.host_mut().model_mut().iso_tds.len(), 4);
    assert_eq!(
        device.engine_for(0).set_interface(1, 0),
        Err(DriverError::Busy)
    );
}

#[test]
fn starting_a_stream_again_replaces_the_one_left_running() {
    let mem = shared_mem();
    let mut device = streaming_device(&mem);
    device.host_mut().model_mut().iso_hold = true;
    let layout = geometry(2, 2, 200);
    device.engine_for(0).iso_start(DATA, layout).expect("fits");
    let chunks = device.dma_ref().chunks.len();
    device
        .engine_for(0)
        .iso_queue(DATA, 0, &out_region(layout, 0, &[8, 8]))
        .expect("queued");
    device
        .engine_for(0)
        .iso_start(DATA, layout)
        .expect("replaced");
    assert_eq!(
        device.dma_ref().chunks.len(),
        chunks,
        "one data chunk, not two"
    );
    let mut taken = vec![0u8; layout.region_len()];
    assert_eq!(device.engine_for(0).iso_take(DATA, &mut taken), Ok(None));
}

#[test]
fn a_departing_device_takes_its_settings_and_streams_with_it() {
    let mem = shared_mem();
    let mut device = streaming_device(&mem);
    device.host_mut().model_mut().iso_hold = true;
    let layout = geometry(2, 2, 200);
    device.engine_for(0).iso_start(DATA, layout).expect("fits");
    device
        .engine_for(0)
        .iso_queue(DATA, 0, &out_region(layout, 0, &[8, 8]))
        .expect("queued");
    root_port_change(&mut device, 0, regs::PORTSC_PP | regs::PORTSC_CSC);
    assert_eq!(
        device.next_root_change(&TestDelay::default()),
        Ok(HubEvent::Detached(0))
    );
    assert_eq!(
        device.dma_ref().chunks.len(),
        1,
        "only the controller's own chunk is left"
    );
    assert!(device.dma_ref().withheld.is_empty());
}
