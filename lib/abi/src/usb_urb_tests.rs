extern crate alloc;

use super::*;

fn interrupt_in() -> UrbRequest {
    UrbRequest {
        endpoint: 1,
        transfer_type: UsbTransferType::Interrupt,
        direction: UsbDirection::In,
        length: 8,
        setup: [0; 8],
    }
}

fn layout() -> IsoLayout {
    IsoLayout::new(4, 8, 192).expect("a valid layout")
}

fn round_trip(request: UsbRequest) {
    let mut buf = [0u8; USB_REQUEST_MAX_LEN];
    let n = request.encode(&mut buf).expect("encodes");
    assert_eq!(n, request.encoded_len());
    assert_eq!(UsbRequest::decode(&buf[..n]), Ok(request));
}

/// A stream number the codec tests share.
const STREAM: core::num::NonZeroU32 = core::num::NonZeroU32::MIN.saturating_add(6);

#[test]
fn every_request_round_trips_at_its_own_length() {
    round_trip(UsbRequest::Transfer(UrbRequest {
        transfer_type: UsbTransferType::Control,
        endpoint: 0,
        setup: [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00],
        ..interrupt_in()
    }));
    round_trip(UsbRequest::Transfer(interrupt_in()));
    round_trip(UsbRequest::SetInterface {
        interface: 1,
        alternate: 2,
    });
    round_trip(UsbRequest::ClaimInterface { interface: 3 });
    round_trip(UsbRequest::IsoStart(IsoStartParams {
        endpoint: 0x81,
        layout: layout(),
    }));
    round_trip(UsbRequest::IsoQueue {
        endpoint: 0x01,
        slot: 3,
    });
    round_trip(UsbRequest::IsoStop { endpoint: 0x02 });
}

#[test]
fn a_frame_of_any_other_length_is_refused() {
    let mut buf = [0u8; USB_REQUEST_MAX_LEN + 1];
    let n = UsbRequest::IsoQueue {
        endpoint: 1,
        slot: 0,
    }
    .encode(&mut buf)
    .expect("encodes");
    assert_eq!(
        UsbRequest::decode(&buf[..n - 1]),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(UsbRequest::decode(&buf[..=n]), Err(Errno::LengthOutOfRange));
    assert_eq!(UsbRequest::decode(&[]), Err(Errno::LengthOutOfRange));
}

#[test]
fn encode_refuses_a_short_buffer() {
    let mut buf = [0u8; URB_REQUEST_LEN - 1];
    assert_eq!(
        UsbRequest::Transfer(interrupt_in()).encode(&mut buf),
        Err(Errno::BufferTooSmall)
    );
}

#[test]
fn malformed_fields_fail_closed() {
    let mut buf = [0u8; USB_REQUEST_MAX_LEN];
    let encode = |request: UsbRequest, buf: &mut [u8]| request.encode(buf).expect("encodes");

    let n = encode(UsbRequest::Transfer(interrupt_in()), &mut buf);
    buf[1] = MAX_ENDPOINT + 1;
    assert_eq!(UsbRequest::decode(&buf[..n]), Err(Errno::OutOfRange));
    let n = encode(UsbRequest::Transfer(interrupt_in()), &mut buf);
    // Isochronous is a stream, never a URB.
    buf[2] = 3;
    assert_eq!(UsbRequest::decode(&buf[..n]), Err(Errno::OutOfRange));
    let n = encode(UsbRequest::Transfer(interrupt_in()), &mut buf);
    buf[3] = 2;
    assert_eq!(UsbRequest::decode(&buf[..n]), Err(Errno::OutOfRange));

    let start = UsbRequest::IsoStart(IsoStartParams {
        endpoint: 0x81,
        layout: layout(),
    });
    let n = encode(start, &mut buf);
    buf[1] = 0x80;
    assert_eq!(
        UsbRequest::decode(&buf[..n]),
        Err(Errno::OutOfRange),
        "endpoint zero streams nothing"
    );
    let n = encode(start, &mut buf);
    buf[1] = 0x91;
    assert_eq!(UsbRequest::decode(&buf[..n]), Err(Errno::OutOfRange));
    let n = encode(start, &mut buf);
    buf[6] = 1;
    assert_eq!(UsbRequest::decode(&buf[..n]), Err(Errno::OutOfRange));

    let n = encode(
        UsbRequest::IsoQueue {
            endpoint: 1,
            slot: 0,
        },
        &mut buf,
    );
    buf[2..4].copy_from_slice(&ISO_MAX_SLOTS.to_le_bytes());
    assert_eq!(UsbRequest::decode(&buf[..n]), Err(Errno::OutOfRange));

    buf[0] = 0;
    assert_eq!(UsbRequest::decode(&buf[..2]), Err(Errno::OutOfRange));
    buf[0] = 7;
    assert_eq!(UsbRequest::decode(&buf[..2]), Err(Errno::OutOfRange));
}

#[test]
fn a_layout_is_held_to_every_bound() {
    assert!(IsoLayout::new(ISO_MIN_SLOTS, 1, 1).is_ok());
    assert!(IsoLayout::new(ISO_MIN_SLOTS, 63, 1024).is_ok());
    assert!(IsoLayout::new(ISO_MAX_SLOTS, 3, 1024).is_ok());
    for (slots, packets, bytes) in [
        (ISO_MIN_SLOTS - 1, 8, 192),
        (ISO_MAX_SLOTS + 1, 8, 192),
        (4, 0, 192),
        (4, ISO_MAX_PACKETS + 1, 192),
        (4, 8, 0),
        (4, 8, ISO_MAX_PACKET_BYTES + 1),
        // Within each bound alone, more intervals than a ring holds.
        (ISO_MAX_SLOTS, 4, 192),
        (ISO_MIN_SLOTS, ISO_MAX_PACKETS, 192),
        // Within every bound but the region's.
        (ISO_MIN_SLOTS, 63, ISO_MAX_PACKET_BYTES),
    ] {
        assert_eq!(
            IsoLayout::new(slots, packets, bytes),
            Err(Errno::OutOfRange),
            "{slots} × {packets} × {bytes}"
        );
    }
}

#[test]
fn slots_tile_the_region_without_overlap() {
    let layout = layout();
    assert_eq!(layout.header_len() % 64, 0);
    assert_eq!(layout.slot_stride() % 64, 0);
    assert!(layout.header_len() >= 8 + 8 * usize::from(layout.packets));
    for slot in 0..layout.slots {
        let base = usize::from(slot) * layout.slot_stride();
        let last_record = layout.record_offset(slot, layout.packets - 1) + 8;
        assert!(last_record <= layout.data_offset(slot, 0));
        let last_data = layout.data_offset(slot, layout.packets - 1) + 192;
        assert!(last_data <= base + layout.slot_stride());
    }
    assert_eq!(
        layout.region_len(),
        usize::from(layout.slots) * layout.slot_stride()
    );
    assert_eq!(layout.notify_capacity(), usize::from(layout.slots) + 1);
}

#[test]
fn records_and_data_round_trip_and_refuse_out_of_place_access() {
    let layout = layout();
    let mut region = alloc::vec![0u8; layout.region_len()];
    let record = IsoPacket {
        length: 188,
        status: IsoPacketStatus::Missed,
    };
    layout
        .set_record(&mut region, 2, 7, record)
        .expect("in place");
    assert_eq!(layout.record(&region, 2, 7), Ok(record));
    layout
        .data_mut(&mut region, 3, 0)
        .expect("in place")
        .fill(0xA5);
    assert!(layout
        .data(&region, 3, 0)
        .expect("in place")
        .iter()
        .all(|&b| b == 0xA5));
    assert_eq!(layout.record(&region, 4, 0), Err(Errno::OutOfRange));
    assert_eq!(layout.record(&region, 0, 8), Err(Errno::OutOfRange));
    assert_eq!(
        layout.record(&region[..16], 1, 0),
        Err(Errno::BufferTooSmall)
    );
    let at = layout.record_offset(1, 1) + 4;
    region[at] = 3;
    assert_eq!(layout.record(&region, 1, 1), Err(Errno::OutOfRange));
}

#[test]
fn a_grant_round_trips_and_a_refusal_surfaces_its_errno() {
    let grant = IsoGrant {
        region_grant: 0x1234,
        grantor: crate::ProcId::from_raw([0x5A; crate::PROC_ID_LEN]),
        notify: iso_notify_endpoint_for(77, 0x81),
        interval_microframes: 8,
        speed: UsbSpeed::Full,
        stream: STREAM,
    };
    assert_eq!(IsoGrant::decode(&grant.encode()), Ok(grant));
    let refused = crate::reply::encode_status_reply(Err(Errno::NoBandwidth));
    assert_eq!(IsoGrant::decode(&refused), Err(Errno::NoBandwidth));
    let mut short = grant.encode();
    assert_eq!(
        IsoGrant::decode(&short[..ISO_GRANT_REPLY_LEN - 1]),
        Err(Errno::BadMagic)
    );
    short[45] = 1;
    assert_eq!(IsoGrant::decode(&short), Err(Errno::BadMagic));
    let mut tail = grant.encode();
    tail[55] = 1;
    assert_eq!(IsoGrant::decode(&tail), Err(Errno::BadMagic));
    let mut unnumbered = grant.encode();
    unnumbered[48..52].fill(0);
    assert_eq!(
        IsoGrant::decode(&unnumbered),
        Err(Errno::BadMagic),
        "no stream number"
    );
    let mut speed = grant.encode();
    speed[44] = 9;
    assert_eq!(IsoGrant::decode(&speed), Err(Errno::BadMagic));
    let mut zero = grant.encode();
    zero[40..44].fill(0);
    assert_eq!(IsoGrant::decode(&zero), Err(Errno::BadMagic), "no interval");
    let kernel = IsoGrant {
        grantor: crate::ProcId::KERNEL,
        ..grant
    };
    assert_eq!(IsoGrant::decode(&kernel.encode()), Err(Errno::BadMagic));
    let unnamed = IsoGrant {
        region_grant: 0,
        ..grant
    };
    assert_eq!(IsoGrant::decode(&unnamed.encode()), Err(Errno::BadMagic));
}

#[test]
fn notify_ports_are_distinct_per_pid_and_endpoint_and_never_reserved() {
    let a = iso_notify_endpoint_for(5, 0x81);
    assert_ne!(a, iso_notify_endpoint_for(6, 0x81));
    assert_ne!(a, iso_notify_endpoint_for(5, 0x01));
    assert!(!crate::ipc::is_reserved_endpoint(a));
    assert!(!crate::ipc::is_reserved_endpoint(iso_notify_endpoint_for(
        crate::PID_MAX,
        0x8F
    )));
}

#[test]
fn notifications_round_trip_and_fail_closed() {
    for notify in [
        IsoNotify::SlotDone {
            endpoint: 0x01,
            stream: STREAM,
            slot: 31,
            skipped: 3,
            microframe: u64::MAX - 1,
            completed_at: 123_456_789,
        },
        IsoNotify::Halted {
            endpoint: 0x82,
            stream: STREAM,
            reason: Errno::NotFound,
        },
    ] {
        let bytes = notify.encode();
        assert_eq!(IsoNotify::decode(&bytes), Ok(notify));
        assert_eq!(IsoNotify::decode(&bytes).map(|n| n.stream()), Ok(STREAM));
        assert_eq!(IsoNotify::decode(&bytes[..39]), Err(Errno::BadMagic));
        let mut unnumbered = bytes;
        unnumbered[12..16].fill(0);
        assert_eq!(IsoNotify::decode(&unnumbered), Err(Errno::BadMagic));
    }
    let mut bytes = IsoNotify::Halted {
        endpoint: 0x82,
        stream: STREAM,
        reason: Errno::NotFound,
    }
    .encode();
    bytes[8..12].copy_from_slice(&999i32.to_le_bytes());
    assert_eq!(IsoNotify::decode(&bytes), Err(Errno::BadMagic));
    let mut bytes = IsoNotify::SlotDone {
        endpoint: 0x01,
        stream: STREAM,
        slot: 0,
        skipped: 0,
        microframe: 0,
        completed_at: 0,
    }
    .encode();
    bytes[5] = 0;
    assert_eq!(IsoNotify::decode(&bytes), Err(Errno::BadMagic));
    bytes[5] = 1;
    bytes[4] = 3;
    assert_eq!(IsoNotify::decode(&bytes), Err(Errno::BadMagic));
    bytes[4] = 1;
    bytes[6..8].copy_from_slice(&ISO_MAX_SLOTS.to_le_bytes());
    assert_eq!(IsoNotify::decode(&bytes), Err(Errno::BadMagic));
}

#[test]
fn completion_round_trips_and_surfaces_its_errno() {
    let mut buf = [0u8; URB_COMPLETION_LEN];
    let n = encode_completion(&mut buf, 8).expect("encodes");
    assert_eq!(n, URB_COMPLETION_LEN);
    assert_eq!(decode_completion(&buf[..n]), Ok(8));
    let n = encode_error_completion(&mut buf, Errno::WouldBlock).expect("encodes");
    assert_eq!(decode_completion(&buf[..n]), Err(Errno::WouldBlock));
}

#[test]
fn truncated_or_corrupt_completions_fail_closed() {
    let mut buf = [0u8; COMPLETION_STATUS_LEN];
    put_i32(&mut buf, 0, 0);
    assert_eq!(decode_completion(&buf), Err(Errno::BadMagic));
    let mut buf = [0u8; URB_COMPLETION_LEN];
    put_i32(&mut buf, 0, -9_999);
    assert_eq!(decode_completion(&buf), Err(Errno::BadMagic));
    // A hostile `i32::MIN` cannot be negated in `i32`.
    put_i32(&mut buf, 0, i32::MIN);
    assert_eq!(decode_completion(&buf), Err(Errno::BadMagic));
}
